//! End to end through an unmodified rpi 0.12.0 host with the fake Cursor SDK.
//!
//! Needs the pinned host binary, Node 24+, the worker built and staged into
//! `../rpi-cursor-sdk/runtime` (`node tests/stage-runtime.mjs`) with its
//! node_modules. `RPI_CURSOR_LITE_STAGE=<dir>` tests a `scripts/stage.sh` output
//! instead of this build's library. Cursor's real service is never contacted.
#![cfg(unix)]
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const RPI: &str = "/home/user/Projects/.always-on/rpi-0.12.0/bin/rpi";
const SECRET: &str = "offline-fixture-c1-secret";

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A plugin directory: this build's library beside the shared runtime, or a staged one.
fn scratch() -> (Scratch, PathBuf) {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!(
        "rpi-cursor-lite-host-{:x}",
        u128::from_ne_bytes(random)
    ));
    for dir in ["agent", "home", "work", "plugin"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    let plugin = match std::env::var_os("RPI_CURSOR_LITE_STAGE") {
        Some(stage) => PathBuf::from(stage),
        None => {
            let library = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("librpi_cursor_lite.so");
            std::fs::copy(library, root.join("plugin/librpi_cursor_lite.so")).unwrap();
            let runtime = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rpi-cursor-sdk/runtime");
            std::os::unix::fs::symlink(runtime, root.join("plugin/runtime")).unwrap();
            root.join("plugin")
        }
    };
    (Scratch(root), plugin)
}

fn rpi(root: &Path, plugin: &Path, scenario: &str, args: &[&str]) -> Command {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rpi-cursor-sdk");
    let mut command = Command::new(RPI);
    command
        .current_dir(root.join("work"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("TMPDIR", root)
        .env("RPI_CODING_AGENT_DIR", root.join("agent"))
        .env("CURSOR_API_KEY", SECRET)
        .env(
            "NODE_OPTIONS",
            format!(
                "--import {}",
                sdk.join("tests/fake-sdk/register.mjs").display()
            ),
        )
        .env(
            "RPI_CURSOR_FAKE_SCENARIO",
            Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{scenario}.json")),
        )
        .args([
            "--local-only",
            "--offline",
            "--no-session",
            "--extensions-dir",
        ])
        .arg(plugin)
        .args(args);
    if let Some(node) = std::env::var_os("RPI_CURSOR_NODE") {
        command.env("RPI_CURSOR_NODE", node);
    }
    command
}

fn run(command: &mut Command) -> String {
    let Output {
        status,
        stdout,
        stderr,
    } = command.output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert!(status.success(), "rpi failed: {text}");
    text
}

/// The worker's argv names the plugin directory; none may outlive its host.
fn assert_no_worker_left(plugin: &Path) {
    let needle = plugin.display().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive: Vec<String> = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|entry| std::fs::read(entry.ok()?.path().join("cmdline")).ok())
            .map(|cmdline| String::from_utf8_lossy(&cmdline).replace('\0', " "))
            .filter(|cmdline| cmdline.contains(&needle) && cmdline.contains("main.mjs"))
            .collect();
        if alive.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Cursor worker outlived rpi: {alive:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "requires the pinned rpi 0.12.0 binary, Node 24+ and the staged worker"]
fn unmodified_rpi_lists_streams_aborts_and_reaps_the_cursor_worker() {
    if !Path::new(RPI).is_file() {
        eprintln!("SKIP: {RPI} is absent");
        return;
    }
    let (root, plugin) = scratch();
    let mut outputs = Vec::new();

    let listed = run(&mut rpi(
        &root.0,
        &plugin,
        "reply",
        &["--list-models", "composer-2"],
    ));
    assert!(
        listed
            .lines()
            .any(|line| line.starts_with("cursor") && line.contains("composer-2")),
        "{listed}"
    );
    outputs.push(listed);

    let reply = run(&mut rpi(
        &root.0,
        &plugin,
        "reply",
        &["--model", "cursor/composer-2", "-p", "Say hello"],
    ));
    assert!(
        reply.contains("Hello, from the fake Cursor SDK."),
        "{reply}"
    );
    assert_no_worker_left(&plugin);
    outputs.push(reply);

    // Cursor's own tool work reaches rpi as transcript text, never as a tool call.
    let replay = run(&mut rpi(
        &root.0,
        &plugin,
        "replay",
        &[
            "--model",
            "cursor/composer-2",
            "--mode",
            "json",
            "-p",
            "Run it",
        ],
    ));
    assert!(replay.contains("echo simulated"), "{replay}");
    assert!(!replay.contains("\"toolCall\""), "{replay}");
    assert!(!replay.contains("tool_execution"), "{replay}");
    assert_no_worker_left(&plugin);
    outputs.push(replay);

    let mut child = rpi(
        &root.0,
        &plugin,
        "hang",
        &["--model", "cursor/composer-2", "--mode", "rpc"],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"type":"prompt","message":"Hang until cancelled."}}"#
    )
    .unwrap();
    let mut seen = String::new();
    let mut settled = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        seen.push_str(&line);
        seen.push('\n');
        let event: Value = serde_json::from_str(&line).unwrap_or_default();
        // Abort once the fake's first delta has streamed through the worker.
        if event["type"] == "message_update" && line.contains("Working") && settled.is_none() {
            writeln!(stdin, r#"{{"type":"abort"}}"#).unwrap();
            settled = Some(None);
        }
        if event["type"] == "message_end" {
            settled = Some(Some(event["message"]["stopReason"].clone()));
        }
        if event["type"] == "agent_settled" {
            assert_eq!(event["aborted"], true, "{seen}");
            break;
        }
    }
    assert_eq!(settled, Some(Some("aborted".into())), "{seen}");
    drop(stdin);
    assert!(child.wait().unwrap().success());
    assert_no_worker_left(&plugin);
    outputs.push(seen);

    for output in outputs {
        assert!(!output.contains(SECRET), "CURSOR_API_KEY leaked: {output}");
    }
    // The worker kept its state under the scratch agent dir, not the user's home.
    assert!(!root.0.join("home/.pi").exists());
}

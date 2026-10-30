//! End to end through an unmodified rpi 0.12.0 host with the fake Agent SDK.
//!
//! Needs the pinned host binary, Node 24+ and `npm ci` in `worker/`.
//! `RPI_CLAUDE_AGENT_STAGE=<dir>` tests a `scripts/stage.sh` output instead of
//! this build's library. Anthropic's service is never contacted.
#![cfg(unix)]
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

const RPI: &str = "/home/user/Projects/.always-on/rpi-0.12.0/bin/rpi";
const SECRET: &str = "sk-ant-oat01-offline-host-fixture-secret";

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn package() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// A plugin directory: this build's library beside the worker, or a staged one.
fn scratch() -> (Scratch, PathBuf) {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).unwrap();
    let root = std::env::temp_dir().join(format!(
        "rpi-claude-agent-host-{:x}",
        u128::from_ne_bytes(random)
    ));
    for dir in ["agent", "home", "work", "plugin"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("work/note.txt"), "file body from rpi\n").unwrap();
    let plugin = match std::env::var_os("RPI_CLAUDE_AGENT_STAGE") {
        Some(stage) => PathBuf::from(stage),
        None => {
            let library = std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .join("librpi_claude_agent.so");
            std::fs::copy(library, root.join("plugin/librpi_claude_agent.so")).unwrap();
            std::os::unix::fs::symlink(package().join("worker"), root.join("plugin/runtime"))
                .unwrap();
            root.join("plugin")
        }
    };
    (Scratch(root), plugin)
}

fn rpi(root: &Path, plugin: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(RPI);
    command
        .current_dir(root.join("work"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root.join("home"))
        .env("TMPDIR", root)
        .env("RPI_CODING_AGENT_DIR", root.join("agent"))
        .env("CLAUDE_CODE_OAUTH_TOKEN", SECRET)
        .env("RPI_CLAUDE_AGENT_SDK", package().join("tests/fake-sdk.mjs"))
        .env("RPI_CLAUDE_FAKE_LOG", root.join("fake.jsonl"))
        .env("RPI_CLAUDE_FAKE_EXPECT_TOKEN", SECRET)
        .args([
            "--local-only",
            "--offline",
            "--no-session",
            "--extensions-dir",
        ])
        .arg(plugin)
        .args(args);
    if let Some(node) = std::env::var_os("RPI_CLAUDE_AGENT_NODE") {
        command.env("RPI_CLAUDE_AGENT_NODE", node);
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
            "Claude worker outlived rpi: {alive:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
#[ignore = "requires the pinned rpi 0.12.0 binary, Node 24+ and npm ci in worker/"]
fn unmodified_rpi_lists_streams_runs_rpi_tools_and_aborts() {
    if !Path::new(RPI).is_file() {
        eprintln!("SKIP: {RPI} is absent");
        return;
    }
    let (root, plugin) = scratch();
    let mut outputs = Vec::new();

    let listed = run(&mut rpi(&root.0, &plugin, &["--list-models", "claude"]));
    assert!(
        listed
            .lines()
            .any(|line| line.starts_with("claude-code") && line.contains("claude-opus-5-5")),
        "{listed}"
    );
    outputs.push(listed);

    let reply = run(&mut rpi(
        &root.0,
        &plugin,
        &["--model", "claude-code/claude-opus-5-5", "-p", "Say hello"],
    ));
    assert!(reply.contains("Echo: Say hello"), "{reply}");
    assert_no_worker_left(&plugin);
    outputs.push(reply);

    // Claude's tool call runs through rpi's own read tool and the query resumes.
    let tool = run(&mut rpi(
        &root.0,
        &plugin,
        &[
            "--model",
            "claude-code/claude-opus-5-5",
            "--mode",
            "json",
            "-p",
            "please use a tool",
        ],
    ));
    assert!(tool.contains("tool_execution"), "{tool}");
    assert!(tool.contains("Tool said: file body from rpi"), "{tool}");
    assert_no_worker_left(&plugin);
    outputs.push(tool);
    let log = std::fs::read_to_string(root.0.join("fake.jsonl")).unwrap();
    assert!(log.contains("\"tokenOk\":true"), "{log}");
    // One query for the tool round trip: the same query continued.
    let queries = log
        .lines()
        .filter(|l| l.contains("\"event\":\"query\""))
        .count();
    assert_eq!(queries, 2, "{log}");

    let mut child = rpi(
        &root.0,
        &plugin,
        &["--model", "claude-code/claude-opus-5-5", "--mode", "rpc"],
    )
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"type":"prompt","message":"hang until cancelled"}}"#
    )
    .unwrap();
    let mut seen = String::new();
    let mut settled = None;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        seen.push_str(&line);
        seen.push('\n');
        let event: Value = serde_json::from_str(&line).unwrap_or_default();
        if event["type"] == "message_update" && line.contains("Workin") && settled.is_none() {
            writeln!(stdin, r#"{{"type":"abort"}}"#).unwrap();
            settled = Some(None);
        }
        if event["type"] == "message_end" && event["message"]["role"] == "assistant" {
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
        assert!(!output.contains(SECRET), "token leaked: {output}");
    }
    assert!(!log.contains(SECRET));
}

/// The real Agent SDK and its bundled Claude Code, against `tests/fake-api.mjs`
/// inside a network namespace that has only loopback, so nothing can reach
/// Anthropic. Proves the SDK options and the MCP tool bridge, not just the fake.
#[test]
#[ignore = "requires the pinned rpi, Node 24+, npm ci in worker/ and unprivileged user namespaces"]
fn real_agent_sdk_runs_an_rpi_tool_offline() {
    if !Path::new(RPI).is_file() {
        eprintln!("SKIP: {RPI} is absent");
        return;
    }
    let (root, plugin) = scratch();
    let script = format!(
        r#"set -e
ip link set lo up
node "$1" 18765 "$2/api.jsonl" &
api=$!
for _ in $(seq 50); do (exec 3<>/dev/tcp/127.0.0.1/18765) 2>/dev/null && break; sleep 0.1; done
cd "$2/work"
env -i PATH="$PATH" HOME="$2/home" TMPDIR="$2" RPI_CODING_AGENT_DIR="$2/agent" \
  CLAUDE_CODE_OAUTH_TOKEN={SECRET} ANTHROPIC_BASE_URL=http://127.0.0.1:18765 \
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 \
  "$3" --local-only --offline --no-session --extensions-dir "$4" \
  --model claude-code/claude-opus-5-5 --mode json -p "read note.txt"
kill $api"#
    );
    let output = Command::new("unshare")
        .args(["-rn", "bash", "-c", &script, "offline"])
        .arg(package().join("tests/fake-api.mjs"))
        .arg(&root.0)
        .arg(RPI)
        .arg(&plugin)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if text.contains("unshare: ") {
        eprintln!("SKIP: no unprivileged network namespace: {text}");
        return;
    }
    assert!(output.status.success(), "{text}");
    assert!(text.contains("\"name\":\"read\""), "{text}");
    assert!(text.contains("tool_execution_end"), "{text}");
    assert!(text.contains("Offline tool result SEEN"), "{text}");
    assert!(!text.contains(SECRET), "token leaked: {text}");
    let api = std::fs::read_to_string(root.0.join("api.jsonl")).unwrap();
    // Built-in Claude Code tools stay off; rpi's tools arrive through the MCP bridge.
    assert!(api.contains("mcp__rpi__read"), "{api}");
    for builtin in [
        "\"Bash\"",
        "\"Read\"",
        "\"Edit\"",
        "\"Write\"",
        "\"WebFetch\"",
    ] {
        assert!(!api.contains(builtin), "{builtin} offered: {api}");
    }
    assert!(api.contains("\"bearer\":true") && !api.contains("\"apiKeyHeader\":true"));
    assert_no_worker_left(&plugin);
}

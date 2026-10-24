//! Mandatory real-host acceptance. Build rpi and the package cdylib first;
//! missing artifacts are failures, never passing skips. No live providers.
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};
struct Temp(PathBuf);
impl Temp {
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn isolated_home() -> Temp {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "m-virtual-loaded-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path).unwrap();
    Temp(path)
}
fn debug_dir() -> PathBuf {
    std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned()
}
fn fixture_library(name: &str) -> PathBuf {
    let debug = debug_dir();
    let deps = debug.join("deps");
    let rlib = |name: &str| {
        std::fs::read_dir(&deps)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(&format!("lib{name}-"))
                    && path.extension().is_some_and(|ext| ext == "rlib")
            })
            .max_by_key(|path| path.metadata().unwrap().modified().unwrap())
            .expect("required fixture dependency rlib")
    };
    let library = debug.join(format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    let output = Command::new(std::env::var_os("RPI_TEST_RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition=2021", "--crate-type=cdylib"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/{name}.rs")))
        .arg("-L")
        .arg(format!("dependency={}", deps.display()))
        .arg("--extern")
        .arg(format!(
            "rpi_plugin_sdk={}",
            rlib("rpi_plugin_sdk").display()
        ))
        .arg("--extern")
        .arg(format!("serde_json={}", rlib("serde_json").display()))
        .arg("-o")
        .arg(&library)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    library
}
fn target_library() -> &'static Path {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY.get_or_init(|| fixture_library("virtual_targets"))
}
fn audit_library() -> &'static Path {
    static LIBRARY: OnceLock<PathBuf> = OnceLock::new();
    LIBRARY.get_or_init(|| fixture_library("virtual_loader"))
}
fn config(id: &str, backends: &[&str]) -> Value {
    json!({"id":id,"label":id,"models":[{"id":"m","backends":backends.iter().map(|provider|
        json!({"providerId":provider,"modelId":"m","template":{"api":"openai-completions","baseUrl":"https://fixture.invalid","contextWindow":32000,"maxTokens":4096,"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}})).collect::<Vec<_>>()}]})
}
fn run(virtuals: Value, pool: bool) -> std::process::Output {
    run_mode(virtuals, pool, false, false)
}
fn run_mode(virtuals: Value, pool: bool, cancel: bool, fabric: bool) -> std::process::Output {
    let temp = isolated_home();
    let agent = temp.path().join("agent");
    std::fs::create_dir(&agent).unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"retry":{"enabled":false}}"#,
    )
    .unwrap();
    std::fs::write(agent.join("auth.json"), json!({"virtual-a":{"type":"api_key","key":"source-secret"},
        "virtual-b":{"type":"api_key","key":"source-secret"},"fixture-red":{"type":"api_key","key":"fixture-red-secret"},
        "fixture-blue":{"type":"api_key","key":"fixture-blue-secret"}}).to_string()).unwrap();
    let mut state = json!({"version":1,"scheduler":{"errorsBeforeSwitch":1},"providers":{},"virtuals":virtuals});
    if pool {
        state["providers"] = json!({"fixture-blue":{"policy":"round-robin","affinity":true,"includeUpstream":false,"accounts":[{"id":"pooled","label":"Pooled","authKind":"api_key","credential":{"type":"api_key","key":"pooled-target-secret"}}]}});
    }
    std::fs::write(agent.join("multiprovider-auth.json"), state.to_string()).unwrap();
    let debug = debug_dir();
    let host = std::env::var_os("RPI_TEST_HOST")
        .map(PathBuf::from)
        .unwrap_or_else(|| debug.join("rpi"));
    assert!(
        host.is_file(),
        "build the real rpi host or set RPI_TEST_HOST"
    );
    let library = std::env::var_os("RPI_TEST_MULTIPROVIDER_LIBRARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            debug.join(format!(
                "{}rpi_multiprovider{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ))
        });
    assert!(library.is_file(), "build rpi-multiprovider cdylib first");
    let ready = std::os::unix::net::UnixListener::bind(temp.path().join("ready.sock")).unwrap();
    ready.set_nonblocking(true).unwrap();
    let mut command = Command::new("timeout");
    command
        .arg("25")
        .arg(host)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", temp.path())
        .env("TMPDIR", temp.path())
        .env("RPI_CODING_AGENT_DIR", &agent)
        .env("RPI_OFFLINE", "1")
        .env("VIRTUAL_AUDIT", temp.path().join("audit.jsonl"))
        .env("VIRTUAL_TRACE", temp.path().join("attempts.jsonl"))
        .env("VIRTUAL_SERVICE", temp.path().join("services.jsonl"))
        .env("VIRTUAL_FABRIC", if fabric { "1" } else { "0" })
        .env("VIRTUAL_PACKAGE_DLL", library)
        .env("VIRTUAL_READY", temp.path().join("ready.sock"))
        .current_dir(temp.path())
        .args(["--extension"])
        .arg(target_library())
        .args(["--extension"])
        .arg(audit_library())
        .args([
            "--provider",
            "virtual-a",
            "--model",
            "m",
            "--system-prompt",
            if cancel {
                "cancel"
            } else if fabric {
                "fabric"
            } else if pool {
                "pool"
            } else {
                "virtual"
            },
            "--no-tools",
        ]);
    if cancel || fabric {
        command.args(["--mode", "rpc"]);
    } else {
        command.args(["--print", "hello"]);
    }
    let mut child = command
        .stdin(if cancel || fabric {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output_reader = None;
    if cancel || fabric {
        use std::io::{BufRead, Write};
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        output_reader = Some(std::thread::spawn(move || {
            let mut output = Vec::new();
            for line in std::io::BufReader::new(stdout).lines() {
                let line = line.unwrap();
                output.extend_from_slice(line.as_bytes());
                output.push(b'\n');
                let _ = sender.send(line);
            }
            output
        }));
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"type":"prompt","message":"hello"})).unwrap();
        stdin.flush().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            match ready.accept() {
                Ok(_) => break,
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::yield_now()
                }
                Err(error) => {
                    let _ = child.kill();
                    let output = child.wait_with_output().unwrap();
                    panic!(
                        "target not ready: {error}: service log={:?}: {}{}",
                        std::fs::read_to_string(temp.path().join("services.jsonl")),
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
        }
        if cancel {
            writeln!(child.stdin.as_mut().unwrap(), "{}", json!({"type":"abort"})).unwrap();
            child.stdin.as_mut().unwrap().flush().unwrap();
        }
        loop {
            let line = receiver
                .recv_timeout(std::time::Duration::from_secs(15))
                .expect("cancelled RPC turn must terminate");
            let event: Value = serde_json::from_str(&line).unwrap();
            if event["type"] == "agent_end" {
                break;
            }
        }
    }
    let mut result = child.wait_with_output().unwrap();
    if let Some(reader) = output_reader {
        result.stdout = reader.join().unwrap();
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    for secret in [
        "source-secret",
        "fixture-red-secret",
        "fixture-blue-secret",
        "pooled-target-secret",
    ] {
        assert!(!text.contains(secret), "credential escaped");
    }
    assert_ne!(
        result.status.code(),
        Some(124),
        "real host timed out: {text}"
    );
    let audit: Vec<Value> = std::fs::read_to_string(temp.path().join("audit.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    // Every successfully minted handle must be explicitly released, including
    // failed attempts and cycle errors. Host epoch cleanup is not sufficient.
    for (resolve, handle, release) in [
        ("resolveTarget", "target", "releaseTarget"),
        ("auth.resolveAmbient", "receipt", "releaseAuth"),
        ("resolveTargetAuth", "receipt", "releaseAuth"),
        ("resolveAuth", "receipt", "releaseAuth"),
    ] {
        for event in audit
            .iter()
            .filter(|event| event["action"] == resolve && event["status"] == 0)
        {
            let releases: Vec<_> = audit
                .iter()
                .filter(|released| {
                    released["action"] == release && released[handle] == event[handle]
                })
                .collect();
            assert_eq!(
                releases.len(),
                1,
                "each handle needs exactly one release attempt: {event}: {audit:?}"
            );
            if cancel {
                // Host 8014e36 revokes the entire continuation before package
                // cancellation cleanup. Require the exact rejection, not an
                // arbitrary failed release. Successful post-cancel release is
                // a host limitation recorded in the lane evidence.
                assert_eq!(releases[0]["status"], 1);
                assert_eq!(releases[0]["error"], "inactive provider continuation");
            } else {
                assert_eq!(releases[0]["status"], 0, "release must succeed: {audit:?}");
            }
        }
    }
    if cancel {
        assert!(
            audit
                .iter()
                .any(|event| event["action"] == "auth.resolveAmbient" && event["status"] == 0),
            "cancellation must reach delegated execution: {audit:?}"
        );
        assert!(
            text.contains("aborted"),
            "cancellation must surface: {text}"
        );
    }
    if fabric {
        let services: Vec<Value> = std::fs::read_to_string(temp.path().join("services.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            services.iter().any(|call| call["method"] == "capabilities"),
            "compatible service must be negotiated: {services:?}: {text}"
        );
        assert!(
            !services.iter().any(|call| call["method"] == "compact"),
            "shutdown must cancel delayed compaction"
        );
        let session = &services[0]["identity"]["sessionId"];
        assert!(
            services
                .iter()
                .all(|call| &call["identity"]["sessionId"] == session),
            "service calls must remain in the selected execution scope"
        );
    }
    if result.status.success() && !pool && !cancel && !fabric {
        let attempts: Vec<Value> = std::fs::read_to_string(temp.path().join("attempts.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            attempts.len(),
            2,
            "real backends must fail over: {attempts:?}"
        );
        let mut providers: Vec<_> = attempts
            .iter()
            .map(|attempt| attempt["provider"].as_str().unwrap())
            .collect();
        providers.sort();
        assert_eq!(providers, vec!["fixture-blue", "fixture-red"]);
    }
    result
}
#[test]
fn real_dll_capable_fabric_shutdown_cancels_delayed_compaction() {
    let output = run_mode(
        json!({"virtual-a":config("virtual-a", &["fixture-red","fixture-blue"])}),
        false,
        false,
        true,
    );
    assert!(output.status.success(), "RPC session must exit cleanly");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("network error"),
        "capable Fabric must surface the failed attempt instead of inline failover: {stdout}"
    );
    assert!(
        !stdout.contains("executed:fixture-blue"),
        "no inline retry while Fabric owns failover"
    );
}
#[test]
fn real_dll_cross_provider_dispatch_and_backend_failover() {
    let output = run(
        json!({"virtual-a":config("virtual-a", &["fixture-red","fixture-blue"])}),
        false,
    );
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "executed:fixture-blue"
    );
}
#[test]
fn real_dll_cross_provider_dispatch_reaches_target_pool() {
    let output = run(
        json!({"virtual-a":config("virtual-a", &["fixture-blue"])}),
        true,
    );
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "executed:fixture-blue"
    );
}
#[test]
fn real_dll_cancellation_attempts_release_after_host_revocation() {
    let output = run_mode(
        json!({"virtual-a":config("virtual-a", &["fixture-blue"])}),
        false,
        true,
        false,
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn real_dll_virtual_cycle_rejection_surfaces() {
    let output = run(
        json!({"virtual-a":config("virtual-a", &["virtual-b"]),"virtual-b":config("virtual-b", &["virtual-a"])}),
        false,
    );
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("provider delegation cycle")
            || text.contains("provider wrapper instance reentry"),
        "{text}"
    );
}

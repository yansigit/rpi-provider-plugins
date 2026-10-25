//! Worker bootstrap failures must surface the child's own diagnostics.
//!
//! A package whose `runtime/node_modules` is missing dies in about 50 ms with
//! `ERR_MODULE_NOT_FOUND`. The host used to discard stderr and report the
//! generic ten-second handshake timeout, which hid the cause; these tests pin
//! the fast, self-explaining failure and the bootstrap-secret redaction.
use rpi_cursor_sdk::worker::{HostHandler, Worker, WorkerConfig};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "rpi-worker-bootstrap-{name}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn host() -> HostHandler {
    Arc::new(|call| Box::pin(async move { Ok(call.input) }))
}

/// The Node binary under test; the bootstrap tests must not silently pass by
/// asserting nothing.
fn node_binary() -> PathBuf {
    std::env::var_os("RPI_CURSOR_NODE")
        .map(PathBuf::from)
        .unwrap_or_else(|| "node".into())
}

/// The bootstrap entry under test.
fn entry(directory: &TempDir, source: &str) -> PathBuf {
    let node = node_binary();
    let usable = std::process::Command::new(&node)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    assert!(
        usable,
        "the worker bootstrap tests need Node 24+ on PATH or RPI_CURSOR_NODE pointing at one; \
         {} is not usable, so nothing would be asserted",
        node.display()
    );
    let path = directory.path().join("bootstrap.mjs");
    std::fs::write(&path, source).unwrap();
    path
}

fn spawn_error(node_entry: PathBuf, cwd: &Path) -> String {
    spawn_error_within(node_entry, cwd, Duration::from_secs(5))
}

fn spawn_error_within(node_entry: PathBuf, cwd: &Path, limit: Duration) -> String {
    let started = Instant::now();
    let error = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(Worker::spawn(
            WorkerConfig {
                node: node_binary(),
                entry: node_entry,
                cwd: cwd.to_path_buf(),
            },
            host(),
        ))
        .err()
        .expect("a worker that never connects must not spawn");
    let elapsed = started.elapsed();
    assert!(
        elapsed < limit,
        "bootstrap failure must be reported inside {limit:?}: {elapsed:?}"
    );
    error
}

fn early_exit(node_entry: PathBuf, cwd: &Path) -> String {
    spawn_error(node_entry, cwd)
}

#[test]
fn worker_that_dies_during_bootstrap_reports_its_stderr_fast() {
    let directory = TempDir::new("crash");
    let node_entry = entry(
        &directory,
        concat!(
            "import { readFileSync } from \"node:fs\";\n",
            "readFileSync(0, \"utf8\");\n",
            "process.stderr.write(\"Error [ERR_MODULE_NOT_FOUND]: Cannot find package '@cursor/sdk'\\n\");\n",
            "process.exit(1);\n",
        ),
    );
    let error = early_exit(node_entry, directory.path());
    assert!(
        error.starts_with("Cursor worker exited before handshake ("),
        "unexpected error: {error}"
    );
    assert!(
        error.contains("ERR_MODULE_NOT_FOUND"),
        "worker stderr must reach the caller: {error}"
    );
    assert!(
        !error.contains("handshake timed out"),
        "a dead worker is not a timeout: {error}"
    );
}

#[test]
fn bootstrap_secrets_are_redacted_from_worker_stderr() {
    let directory = TempDir::new("echo");
    let node_entry = entry(
        &directory,
        concat!(
            "import { readFileSync } from \"node:fs\";\n",
            "process.stderr.write(readFileSync(0, \"utf8\"));\n",
            "process.exit(1);\n",
        ),
    );
    let error = early_exit(node_entry, directory.path());
    assert!(
        error.starts_with("Cursor worker exited before handshake ("),
        "unexpected error: {error}"
    );
    for key in ["generation", "token"] {
        let marker = format!("\"{key}\":\"[redacted]\"");
        assert!(
            error.contains(&marker),
            "bootstrap {key} must be redacted: {error}"
        );
    }
    // No 32-byte secret survives the excerpt, even when the child echoes stdin.
    for value in error.split("\":\"").skip(1) {
        let value = value.split('"').next().unwrap_or("");
        assert!(
            value.len() < 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "unredacted secret in worker stderr: {error}"
        );
    }
}

#[test]
fn a_worker_that_closes_its_bootstrap_pipe_still_reports_its_stderr() {
    let directory = TempDir::new("closed");
    let node_entry = entry(
        &directory,
        concat!(
            "import { closeSync } from \"node:fs\";\n",
            "closeSync(0);\n",
            "process.stderr.write(\"Error: bootstrap input closed before the handshake\\n\");\n",
            "setTimeout(() => process.exit(3), 1500);\n",
        ),
    );
    let error = spawn_error(node_entry, directory.path());
    assert!(
        error.contains("bootstrap input closed before the handshake"),
        "worker stderr must reach the caller whichever bootstrap arm loses: {error}"
    );
    assert!(
        !error.contains("worker bootstrap failed"),
        "a closed bootstrap pipe is not a bare failure: {error}"
    );
}

#[test]
fn a_live_worker_that_never_connects_keeps_the_timeout_sentence_and_its_stderr() {
    let directory = TempDir::new("timeout");
    let node_entry = entry(
        &directory,
        concat!(
            "import { readFileSync } from \"node:fs\";\n",
            "readFileSync(0, \"utf8\");\n",
            "process.stderr.write(\"Error: cannot find module '@cursor/sdk' while bootstrapping\\n\");\n",
            "setTimeout(() => process.exit(1), 30000);\n",
        ),
    );
    let error = spawn_error_within(node_entry, directory.path(), Duration::from_secs(13));
    assert!(
        error.starts_with("Cursor worker handshake timed out; Node 24+ is required"),
        "the timeout sentence is part of the contract: {error}"
    );
    assert!(
        error.contains("(worker stderr: "),
        "a timeout must carry what the worker printed: {error}"
    );
    assert!(
        error.contains("cannot find module '@cursor/sdk'"),
        "worker stderr must reach the caller: {error}"
    );
}

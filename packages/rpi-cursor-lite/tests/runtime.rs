//! The worker's minimal host profile through the shared `Session`, with the fake
//! Cursor SDK. Needs Node 24+, `worker/` built and staged into
//! `../rpi-cursor-sdk/runtime` (`node tests/stage-runtime.mjs`) and its node_modules.
use rpi_cursor_lite::{
    native_host::{handler, Router},
    runtime::{drive, Session},
    worker::{HostHandler, WorkerConfig},
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

fn package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).unwrap();
        let path =
            std::env::temp_dir().join(format!("rpi-cursor-lite-{:x}", u128::from_ne_bytes(random)));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The worker inherits this process's environment, so this file has one test.
async fn open(directory: &Directory, scenario: &str, profile: &str) -> Result<Session, String> {
    let sdk = package().join("../rpi-cursor-sdk");
    std::env::set_var(
        "NODE_OPTIONS",
        format!(
            "--import {}",
            sdk.join("tests/fake-sdk/register.mjs").display()
        ),
    );
    std::env::set_var(
        "RPI_CURSOR_FAKE_SCENARIO",
        package().join(format!("tests/fixtures/{scenario}.json")),
    );
    std::env::set_var("RPI_CURSOR_FAKE_LOG", directory.0.join("fake.jsonl"));
    std::env::set_var("HOME", &directory.0);
    Session::open(
        WorkerConfig {
            node: std::env::var_os("RPI_CURSOR_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "node".into()),
            entry: sdk.join("runtime/main.mjs"),
            cwd: directory.0.clone(),
        },
        json!({"agentDir":directory.0.join("agent"),"cwd":directory.0,"sessionDir":directory.0.join("sessions"),"profile":profile}),
        Arc::new(Router),
    )
    .await
}

/// Counts every callback that reaches the host; the minimal profile makes none.
fn counting(calls: &Arc<AtomicUsize>) -> HostHandler {
    let calls = calls.clone();
    let inner = handler();
    Arc::new(move |call| {
        calls.fetch_add(1, Ordering::SeqCst);
        inner(call)
    })
}

async fn stream(
    session: &Session,
    host: HostHandler,
    cancel_after: Option<Duration>,
) -> (Vec<Value>, Result<Value, String>) {
    let model = session.manifest["provider"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "composer-2")
        .unwrap()
        .clone();
    let signal = CancellationToken::new();
    if let Some(delay) = cancel_after {
        let signal = signal.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            signal.cancel();
        });
    }
    let call = session
        .call(
            "provider.stream",
            json!({"entry":"streamSimple","authMode":"replace","model":model,
                "context":{"messages":[{"role":"user","content":"Say hello","timestamp":1}],"systemPrompt":"test prompt"},
                "options":{"apiKey":"offline-fixture"}}),
            host,
            &signal,
            &|| false,
        )
        .await
        .unwrap();
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        drive(call, &signal, &|| false, |event| {
            events.push(event);
            Ok(())
        }),
    )
    .await
    .expect("stream must finish");
    (events, result)
}

fn text(message: &Value) -> String {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24+, the built worker and its node_modules"]
async fn minimal_profile_streams_without_host_callbacks_and_never_returns_tool_calls() {
    let calls = Arc::new(AtomicUsize::new(0));

    let directory = Directory::new();
    let session = open(&directory, "reply", "minimal").await.unwrap();
    // The declared catalog is the worker's own offline fallback catalog.
    let models = &session.manifest["provider"]["models"];
    if std::env::var_os("RPI_CURSOR_LITE_BLESS").is_some() {
        let pretty = serde_json::to_string_pretty(models).unwrap();
        std::fs::write(package().join("models.json"), pretty + "\n").unwrap();
    }
    assert_eq!(
        models,
        &serde_json::from_str::<Value>(rpi_cursor_lite::MODELS).unwrap(),
        "regenerate models.json with RPI_CURSOR_LITE_BLESS=1"
    );
    let (events, result) = stream(&session, counting(&calls), None).await;
    let message = result.unwrap();
    assert_eq!(text(&message), "Hello, from the fake Cursor SDK.");
    assert_eq!(message["stopReason"], "stop");
    assert!(events.iter().any(|event| event["type"] == "text_delta"));
    session.close().await.unwrap();
    // Cursor's own tools stay off: the agent is created with no built-in tools.
    let log = std::fs::read_to_string(directory.0.join("fake.jsonl")).unwrap();
    let create: Value = log
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["kind"] == "sdk_call" && record["api"] == "Agent.create")
        .unwrap_or_else(|| panic!("no Agent.create in {log}"));
    assert_eq!(create["options"]["tools"], json!([]), "{create}");

    let directory = Directory::new();
    let session = open(&directory, "replay", "minimal").await.unwrap();
    let (events, result) = stream(&session, counting(&calls), None).await;
    let message = result.unwrap();
    for item in events.iter().chain([&message]) {
        assert!(
            !item.to_string().contains("\"toolCall\""),
            "replayed Cursor work must never reach the host as a tool call: {item}"
        );
    }
    assert!(message.to_string().contains("echo simulated"), "{message}");
    session.close().await.unwrap();

    let directory = Directory::new();
    let session = open(&directory, "hang", "minimal").await.unwrap();
    let (_, result) = stream(&session, counting(&calls), Some(Duration::from_millis(500))).await;
    match result {
        Ok(message) => assert_eq!(message["stopReason"], "aborted", "{message}"),
        Err(error) => assert!(
            error.contains("cancel") || error.contains("abort"),
            "{error}"
        ),
    }
    session.close().await.unwrap();

    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let directory = Directory::new();
    let error = open(&directory, "reply", "full").await.err().unwrap();
    assert!(
        error.contains("Invalid native Cursor host profile"),
        "{error}"
    );
}

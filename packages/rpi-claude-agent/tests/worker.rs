//! The Node worker through the shared `Session`, with the fake Agent SDK
//! (`tests/fake-sdk.mjs`). Needs Node 24+ and `npm ci` in `worker/`.
//! Anthropic's service is never contacted.
use rpi_claude_agent::{
    native_host::{handler, Router},
    runtime::{drive, Session},
    worker::WorkerConfig,
};
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const SECRET: &str = "sk-ant-oat01-offline-fixture-secret";

fn package() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn directory() -> Directory {
    let mut random = [0; 16];
    getrandom::getrandom(&mut random).unwrap();
    let path = std::env::temp_dir().join(format!(
        "rpi-claude-agent-{:x}",
        u128::from_ne_bytes(random)
    ));
    std::fs::create_dir(&path).unwrap();
    Directory(path)
}

fn read_tool() -> Value {
    json!({"name":"read","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}})
}

fn user(text: &str) -> Value {
    json!({"role":"user","content":text,"timestamp":1})
}

fn request(session: &str, messages: &[Value], key: Option<&str>) -> Value {
    json!({
        "model": {"id":"claude-opus-5-5"},
        "context": {"systemPrompt":"You are rpi.","messages":messages,"tools":[read_tool()]},
        "options": {"apiKey":key,"sessionId":session},
    })
}

/// One `provider.stream`: every emitted chunk, then the final message.
async fn stream(session: &Session, input: Value, cancel_on: Option<&str>) -> (Vec<Value>, Value) {
    let signal = CancellationToken::new();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let is_cancelled = move || flag.load(Ordering::SeqCst);
    let call = session
        .call("provider.stream", input, handler(), &signal, &is_cancelled)
        .await
        .unwrap();
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        drive(call, &signal, &is_cancelled, |event| {
            if cancel_on.is_some_and(|text| event.to_string().contains(text)) {
                cancelled.store(true, Ordering::SeqCst);
            }
            events.push(event);
            Ok(())
        }),
    )
    .await
    .expect("stream settles")
    .unwrap();
    (events, result)
}

fn log(directory: &Directory) -> Vec<Value> {
    std::fs::read_to_string(directory.0.join("fake.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn text_of(message: &Value) -> String {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect()
}

/// The worker inherits this process's environment, so this file has one test.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24+ and npm ci in worker/"]
async fn the_worker_streams_bridges_tools_resumes_and_cancels() {
    let directory = directory();
    std::env::set_var("RPI_CLAUDE_AGENT_SDK", package().join("tests/fake-sdk.mjs"));
    std::env::set_var("RPI_CLAUDE_FAKE_LOG", directory.0.join("fake.jsonl"));
    std::env::set_var("RPI_CLAUDE_FAKE_EXPECT_TOKEN", SECRET);
    std::env::set_var("ANTHROPIC_API_KEY", "must-not-reach-claude-code");
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    let entry = package().join("worker/main.mjs");
    let session = Session::open(
        WorkerConfig {
            node: std::env::var_os("RPI_CLAUDE_AGENT_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "node".into()),
            entry: entry.clone(),
            cwd: directory.0.clone(),
        },
        json!({"agentDir":directory.0.join("agent"),"cwd":directory.0}),
        Arc::new(Router),
    )
    .await
    .unwrap();
    let mut seen = Vec::new();

    // Text streams as deltas and the final message carries usage.
    let mut history = vec![user("hello")];
    let (events, reply) = stream(&session, request("s1", &history, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    assert_eq!(text_of(&reply), "Echo: hello");
    assert_eq!(reply["provider"], "claude-code");
    assert_eq!(reply["usage"]["input"], 10);
    assert_eq!(reply["usage"]["output"], 6);
    assert_eq!(reply["usage"]["cacheRead"], 2);
    assert!(events.iter().any(|event| event["type"] == "text_delta"));
    assert_eq!(events.last().unwrap()["type"], "text_end");
    seen.push(format!("{events:?}{reply}"));

    // A tool call ends the stream as an rpi toolCall with the MCP prefix stripped.
    history.push(reply);
    history.push(user("please use a tool"));
    let (events, call) = stream(&session, request("s1", &history, Some(SECRET)), None).await;
    assert_eq!(call["stopReason"], "tooluse", "{call}");
    let tool_call = call["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["type"] == "toolCall")
        .unwrap()
        .clone();
    assert_eq!(
        tool_call,
        json!({"type":"toolCall","id":"toolu_1","name":"read","arguments":{"path":"note.txt"}})
    );
    assert!(events
        .iter()
        .any(|event| event["type"] == "toolcall_end" && event["name"] == "read"));
    seen.push(format!("{events:?}{call}"));

    // The toolResult resolves the pending handler and the same query continues.
    history.push(call);
    history.push(json!({"role":"toolResult","toolCallId":"toolu_1","toolName":"read","content":[{"type":"text","text":"file body"}],"isError":false,"timestamp":2}));
    let (events, reply) = stream(&session, request("s1", &history, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    assert_eq!(text_of(&reply), "Tool said: file body");
    seen.push(format!("{events:?}{reply}"));
    let entries = log(&directory);
    assert_eq!(
        entries.iter().filter(|e| e["event"] == "query").count(),
        1,
        "{entries:?}"
    );
    let permission = entries.iter().find(|e| e["event"] == "permission").unwrap();
    assert_eq!(permission["allowed"], "allow");
    assert_eq!(permission["denied"], "deny");
    let query = &entries[0];
    assert_eq!(query["options"]["tools"], json!([]));
    assert_eq!(query["options"]["settingSources"], json!([]));
    assert_eq!(query["options"]["includePartialMessages"], true);
    assert_eq!(query["options"]["model"], "claude-opus-5-5");
    assert_eq!(query["options"]["systemPrompt"], "You are rpi.");
    assert_eq!(query["options"]["mcpTools"], json!(["read"]));
    assert_eq!(query["tokenOk"], true, "{query}");
    assert_eq!(query["apiKeyInEnv"], false);
    assert_eq!(
        query["configDir"],
        directory.0.join("agent/claude-code").display().to_string()
    );

    // A history the live query did not produce starts a fresh query from a transcript.
    let edited = vec![
        user("a different start"),
        history[1].clone(),
        user("and now?"),
    ];
    let (_, reply) = stream(&session, request("s1", &edited, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let entries = log(&directory);
    assert_eq!(entries.iter().filter(|e| e["event"] == "query").count(), 2);
    assert!(entries.iter().any(|e| e["event"] == "close" && e["n"] == 1));
    let prompt = entries
        .iter()
        .rfind(|e| e["event"] == "user" && e["n"] == 2)
        .unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(prompt.contains("being resumed"), "{prompt}");
    assert!(prompt.contains("[user]\na different start"), "{prompt}");

    // Usage limits carry the shared rate-limit marker.
    let (_, limited) = stream(
        &session,
        request("s2", &[user("limit")], Some(SECRET)),
        None,
    )
    .await;
    assert_eq!(limited["stopReason"], "error");
    assert_eq!(
        limited["errorMessage"],
        "rate_limited; resets_at=1767225600 You've hit your limit"
    );
    seen.push(limited.to_string());

    // Cancel interrupts the query and settles as aborted.
    let (events, aborted) = stream(
        &session,
        request("s3", &[user("hang")], Some(SECRET)),
        Some("Workin"),
    )
    .await;
    assert_eq!(aborted["stopReason"], "aborted", "{aborted}");
    let entries = log(&directory);
    assert!(
        entries.iter().any(|e| e["event"] == "interrupted"),
        "{entries:?}"
    );
    seen.push(format!("{events:?}{aborted}"));

    // No token: a clear error, and no query.
    let (_, missing) = stream(&session, request("s4", &[user("hello")], None), None).await;
    assert_eq!(missing["stopReason"], "error");
    assert!(missing["errorMessage"]
        .as_str()
        .unwrap()
        .contains("claude setup-token"));

    session.close().await.unwrap();
    for output in seen {
        assert!(!output.contains(SECRET), "token leaked: {output}");
    }
    assert!(!std::fs::read_to_string(directory.0.join("fake.jsonl"))
        .unwrap()
        .contains(SECRET));

    // No node process outlives the session.
    let needle = entry.display().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let alive = std::fs::read_dir("/proc")
            .unwrap()
            .filter_map(|entry| std::fs::read(entry.ok()?.path().join("cmdline")).ok())
            .any(|cmdline| String::from_utf8_lossy(&cmdline).contains(&needle));
        if !alive {
            break;
        }
        assert!(Instant::now() < deadline, "the worker outlived its session");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

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
const OTHER: &str = "sk-ant-oat01-offline-other-account";

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

const CUSTOM_PROMPT: &str = "You are rpi.\n\n<cwd>\n/work/project\n</cwd>";
const RUNTIME_NOTE: &str = "<runtime_info>You are running in rpi through the Claude Code harness. Mention this only when asked.</runtime_info>";

/// rpi's default scaffold as current rpi composes it: scaffold, appended text,
/// project context, skills, then the cwd section.
fn default_prompt() -> String {
    [
        "You are an expert coding assistant operating inside rpi, a coding agent harness. You help users by reading files, executing commands, editing code, and writing new files.",
        "Available tools:\n- read  — Read file contents",
        "In addition to the tools above, you may have access to other custom tools depending on the project.",
        "Guidelines:\n- Reply in the same language as the user's latest message (the conversation's language when\n  that message is too short to tell), unless the user asks for another.\n- Be concise in your responses",
        "APPENDED",
        "<project_context>\nproject notes\n</project_context>",
        "The following skills provide specialized instructions.\n<available_skills>\n</available_skills>",
        "<cwd>\n/work/default\n</cwd>",
    ]
    .join("\n\n")
}

fn request(session: &str, messages: &[Value], key: Option<&str>) -> Value {
    request_with(session, messages, key, CUSTOM_PROMPT)
}

fn request_with(session: &str, messages: &[Value], key: Option<&str>, system: &str) -> Value {
    json!({
        "model": {"id":"claude-opus-5-5"},
        "context": {"systemPrompt":system,"messages":messages,"tools":[read_tool()]},
        "options": {"apiKey":key,"sessionId":session},
    })
}

/// One `provider.stream`: every emitted chunk, then the final message.
async fn stream(session: &Session, input: Value, cancel_on: Option<&str>) -> (Vec<Value>, Value) {
    stream_with(session, input, cancel_on, Arc::new(AtomicBool::new(false))).await
}

/// [`stream`], also cancelled when the caller sets `cancelled`.
async fn stream_with(
    session: &Session,
    input: Value,
    cancel_on: Option<&str>,
    cancelled: Arc<AtomicBool>,
) -> (Vec<Value>, Value) {
    let signal = CancellationToken::new();
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

/// The last query the fake SDK started, and the text of its last user message.
fn last_query(directory: &Directory) -> (Value, Value) {
    let entries = log(directory);
    let query = entries
        .iter()
        .rfind(|e| e["event"] == "query")
        .unwrap()
        .clone();
    let user = entries
        .iter()
        .rfind(|e| e["event"] == "user" && e["n"] == query["n"])
        .cloned()
        .unwrap_or_default();
    (query, user)
}

/// The saved record whose Claude session is `claude`.
fn record_of(directory: &Directory, claude: &str) -> Option<(PathBuf, Value)> {
    std::fs::read_dir(directory.0.join("agent/claude-code/rpi-sessions"))
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let record: Value = serde_json::from_str(&std::fs::read_to_string(&path).ok()?).ok()?;
            (record["claude"] == claude).then_some((path, record))
        })
        .next()
}

fn record_count(directory: &Directory) -> usize {
    std::fs::read_dir(directory.0.join("agent/claude-code/rpi-sessions"))
        .map(|dir| dir.count())
        .unwrap_or(0)
}

fn identity(purpose: &str) -> Value {
    json!({"sessionId":"host","operationId":"op","purpose":purpose,"turnId":null,"requestId":null})
}

fn other_model(text: &str) -> Value {
    json!({"role":"assistant","content":[{"type":"thinking","thinking":"private plan"},{"type":"text","text":text}],"api":"openai-responses","provider":"openai","model":"gpt","usage":{},"stopReason":"stop","timestamp":3})
}

/// Polls until `done` holds, or fails after a few seconds.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn open_worker(
    directory: &Directory,
) -> impl std::future::Future<Output = Result<Session, String>> {
    Session::open(
        WorkerConfig {
            node: std::env::var_os("RPI_CLAUDE_AGENT_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "node".into()),
            entry: package().join("worker/main.mjs"),
            cwd: directory.0.clone(),
        },
        json!({"agentDir":directory.0.join("agent"),"cwd":directory.0}),
        Arc::new(Router),
    )
}

fn text_of(message: &Value) -> String {
    message["content"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|block| block["text"].as_str())
        .collect()
}

/// The worker inherits this process's environment; only this test changes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24+ and npm ci in worker/"]
async fn the_worker_streams_bridges_tools_resumes_and_cancels() {
    let directory = directory();
    std::env::set_var("RPI_CLAUDE_AGENT_SDK", package().join("tests/fake-sdk.mjs"));
    std::env::set_var("RPI_CLAUDE_FAKE_LOG", directory.0.join("fake.jsonl"));
    std::env::set_var("RPI_CLAUDE_FAKE_EXPECT_TOKEN", SECRET);
    std::env::set_var("ANTHROPIC_API_KEY", "must-not-reach-claude-code");
    for name in [
        "CLAUDE_CODE_SIMPLE",
        "CLAUDE_CODE_EFFORT_LEVEL",
        "MCP_TOOL_TIMEOUT",
    ] {
        std::env::set_var(name, "from-the-shell");
    }
    std::env::remove_var("CLAUDE_CONFIG_DIR");
    // Short enough to watch, long enough that the other steps never idle out.
    std::env::set_var("RPI_CLAUDE_AGENT_IDLE_MS", "1500");
    std::env::set_var("RPI_CLAUDE_AGENT_TOOL_WAIT_MS", "4000");
    let entry = package().join("worker/main.mjs");
    let session = open_worker(&directory).await.unwrap();
    let mut seen = Vec::new();

    // Text streams as deltas and the final message carries usage. The fake
    // reports the response's usage three times; it counts once.
    let mut history = vec![user("hello")];
    let (events, reply) = stream(&session, request("s1", &history, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    assert_eq!(text_of(&reply), "Echo: hello");
    assert_eq!(reply["provider"], "claude-code");
    assert_eq!(reply["usage"]["input"], 10);
    assert_eq!(reply["usage"]["output"], 5);
    assert_eq!(reply["usage"]["cacheRead"], 2);
    assert_eq!(reply["usage"]["totalTokens"], 17);
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
    let query = &entries[0];
    let options = &query["options"];
    assert_eq!(options["tools"], json!([]));
    assert_eq!(options["settingSources"], json!([]));
    assert_eq!(options["skills"], json!([]));
    assert_eq!(options["settings"], json!({"disableAllHooks":true}));
    assert_eq!(options["strictMcpConfig"], true);
    assert_eq!(options["includePartialMessages"], true);
    assert_eq!(options["verbatimPrompts"], true);
    assert_eq!(
        options["thinking"],
        json!({"type":"adaptive","display":"summarized"})
    );
    // Only rpi's tools run, without a permission callback.
    assert_eq!(options["permissionMode"], "dontAsk");
    assert_eq!(options["allowedTools"], json!(["mcp__rpi__*"]));
    assert!(options.get("canUseTool").is_none(), "{options}");
    assert_eq!(options["model"], "claude-opus-5-5");
    // Claude Code's own prompt; a custom base stays, the cwd becomes the option.
    assert_eq!(
        options["systemPrompt"],
        json!({"type":"preset","preset":"claude_code","append":format!("{RUNTIME_NOTE}\n\nYou are rpi."),"snapshot":false})
    );
    assert_eq!(options["cwd"], "/work/project");
    assert_eq!(options["mcpTools"], json!(["read"]));
    // A chat keeps a Claude session of its own.
    assert_eq!(options["persistSession"], true);
    assert!(options.get("resume").is_none(), "{options}");
    let s1_claude = options["sessionId"].as_str().unwrap().to_owned();
    assert_eq!(s1_claude.len(), 36, "{options}");
    assert_eq!(
        query["env"],
        json!({
            "CLAUDE_CODE_DISABLE_CLAUDE_MDS": "1",
            "CLAUDE_CODE_DISABLE_AUTO_MEMORY": "1",
            "DISABLE_AUTO_COMPACT": "1",
            "CLAUDE_CODE_ENABLE_AWAY_SUMMARY": "0",
            "ENABLE_TOOL_SEARCH": "false",
            "CLAUDE_CODE_DISABLE_ADVISOR_TOOL": "1",
            "MAX_MCP_OUTPUT_TOKENS": "200000",
        }),
        "the three shell variables are cleared"
    );
    assert_eq!(query["tokenOk"], true, "{query}");
    assert_eq!(query["apiKeyInEnv"], false);
    assert_eq!(
        query["configDir"],
        directory.0.join("agent/claude-code").display().to_string()
    );

    // An edit forks Claude's session at the last checkpoint both histories
    // share (after "Echo: hello") and sends only what follows it.
    let mut edited = vec![
        history[0].clone(),
        history[1].clone(),
        user("edited question"),
    ];
    let (_, reply) = stream(&session, request("s1", &edited, Some(SECRET)), None).await;
    assert_eq!(text_of(&reply), "Echo: edited question", "{reply}");
    let entries = log(&directory);
    assert!(entries.iter().any(|e| e["event"] == "close" && e["n"] == 1));
    let fork = entries.iter().rfind(|e| e["event"] == "fork").unwrap();
    assert_eq!(fork["from"], s1_claude.as_str());
    assert_eq!(fork["kept"], 2, "the first user message and reply");
    // The record no longer needs the original, so it is deleted.
    assert!(
        entries
            .iter()
            .any(|e| e["event"] == "delete" && e["id"] == s1_claude.as_str()),
        "{entries:?}"
    );
    let (query, sent) = last_query(&directory);
    assert_eq!(query["options"]["resume"], fork["to"]);
    assert_eq!(sent["text"], "edited question");
    assert_eq!(sent["seen"], 2);
    let forked = fork["to"].clone();

    // Another model answered in between: Claude resumes and gets what it missed
    // as one checkpoint message, without thinking, then the new user message.
    edited.push(reply);
    edited.push(user("ask another model"));
    edited.push(other_model("Other model answer"));
    edited.push(user("back to claude"));
    let (_, reply) = stream(&session, request("s1", &edited, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let (query, sent) = last_query(&directory);
    assert_eq!(query["options"]["resume"], forked);
    let text = sent["text"].as_str().unwrap();
    assert!(
        text.starts_with("<conversation-checkpoint>")
            && text.contains("[user]\nask another model")
            && text.contains("[assistant]\nOther model answer\n</conversation-checkpoint>")
            && text.ends_with("back to claude"),
        "{text}"
    );
    assert!(
        !text.contains("private plan") && !text.contains("edited question"),
        "{text}"
    );
    assert_eq!(sent["seen"], 4);

    // A history no checkpoint matches starts a fresh Claude session from the whole history.
    let fresh = vec![
        user("a different start"),
        history[1].clone(),
        user("and now?"),
    ];
    let (_, reply) = stream(&session, request("s1", &fresh, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let (query, sent) = last_query(&directory);
    assert!(query["options"].get("resume").is_none(), "{query}");
    let text = sent["text"].as_str().unwrap();
    assert!(text.contains("[user]\na different start"), "{text}");
    assert!(text.ends_with("and now?"), "{text}");
    assert_eq!(sent["seen"], 0);

    // The same input answered by another model on a branch: the live query must
    // not continue as if its own reply were there.
    let mut first = request("branch", &[user("branch start")], Some(SECRET));
    first["options"]["identity"] = identity("prompt");
    let (_, _) = stream(&session, first, None).await;
    let (started, _) = last_query(&directory);
    assert_eq!(
        started["options"]["persistSession"], true,
        "a prompt is a chat"
    );
    let branched = [
        user("branch start"),
        other_model("Other branch reply"),
        user("after branch"),
    ];
    let mut second = request("branch", &branched, Some(SECRET));
    second["options"]["identity"] = identity("prompt");
    let (_, reply) = stream(&session, second, None).await;
    assert_eq!(text_of(&reply), "Echo: after branch", "{reply}");
    let (query, sent) = last_query(&directory);
    assert_ne!(query["n"], started["n"]);
    assert!(query["options"].get("resume").is_none(), "{query}");
    assert!(
        sent["text"]
            .as_str()
            .unwrap()
            .contains("[assistant]\nOther branch reply"),
        "{sent}"
    );

    // Compaction, summaries and titles are not chats: nothing saved, and the query ends with the reply.
    let records = record_count(&directory);
    let mut side = request("compaction-call", &[user("summarize this")], Some(SECRET));
    side["options"]["identity"] = identity("compaction");
    let (_, reply) = stream(&session, side, None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let (query, _) = last_query(&directory);
    assert_eq!(query["options"]["persistSession"], false);
    assert!(
        query["options"].get("sessionId").is_none() && query["options"].get("resume").is_none(),
        "{query}"
    );
    assert!(log(&directory)
        .iter()
        .any(|e| e["event"] == "close" && e["n"] == query["n"]));
    assert_eq!(record_count(&directory), records);

    // Released hosts send no identity: a request without tools is their compaction call.
    let mut side = request("compaction-call-released", &[user("summarize this")], Some(SECRET));
    side["context"]["tools"] = json!([]);
    let (_, reply) = stream(&session, side, None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let (query, _) = last_query(&directory);
    assert_eq!(query["options"]["persistSession"], false, "{query}");
    assert_eq!(record_count(&directory), records);

    // rpi's default scaffold and cwd give way to Claude Code's; the rest is appended.
    // Two API responses in one turn: output adds up, input is the last response's.
    let (_, twice) = stream(
        &session,
        request_with("s5", &[user("twice")], Some(SECRET), &default_prompt()),
        None,
    )
    .await;
    assert_eq!(text_of(&twice), "Second answer", "{twice}");
    assert_eq!(
        twice["usage"],
        json!({"input":20,"output":12,"cacheRead":12,"cacheWrite":3,"totalTokens":47,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}})
    );
    let entries = log(&directory);
    let options = &entries.iter().rfind(|e| e["event"] == "query").unwrap()["options"];
    assert_eq!(
        options["systemPrompt"]["append"],
        format!("{RUNTIME_NOTE}\n\nGuidelines:\n- Reply in the same language as the user's latest message (the conversation's language when\n  that message is too short to tell), unless the user asks for another.\n- Be concise in your responses\n\nAPPENDED\n\n<project_context>\nproject notes\n</project_context>\n\nThe following skills provide specialized instructions.\n<available_skills>\n</available_skills>")
    );
    assert_eq!(options["cwd"], "/work/default");

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

    // Cancel interrupts the query and settles as aborted. While the turn runs,
    // its record is marked in flight, so a crash mid-turn never resumes it.
    let cancel = Arc::new(AtomicBool::new(false));
    let watch = async {
        eventually("an in-flight record", || {
            log(&directory)
                .iter()
                .rfind(|e| e["event"] == "user" && e["text"] == "hang")
                .and_then(|e| record_of(&directory, e["session"].as_str()?))
                .is_some_and(|(_, record)| record["head"]["open"] == true)
        })
        .await;
        cancel.store(true, Ordering::SeqCst);
    };
    let ((events, aborted), ()) = tokio::join!(
        stream_with(
            &session,
            request("s3", &[user("hang")], Some(SECRET)),
            None,
            cancel.clone()
        ),
        watch
    );
    assert_eq!(aborted["stopReason"], "aborted", "{aborted}");
    let entries = log(&directory);
    assert!(
        entries.iter().any(|e| e["event"] == "interrupted"),
        "{entries:?}"
    );
    let (hung, _) = last_query(&directory);
    seen.push(format!("{events:?}{aborted}"));

    // The next request continues the same Claude session, which keeps its own
    // partial reply; rpi's copy of it is not sent again.
    // rpi's copy of the partial reply, with a tool call and its result, is skipped too.
    let mut partial = aborted.clone();
    partial["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"toolCall","id":"toolu_partial","name":"read","arguments":{}}));
    let after_abort = vec![
        user("hang"),
        partial,
        json!({"role":"toolResult","toolCallId":"toolu_partial","toolName":"read","content":[{"type":"text","text":"aborted"}],"isError":true,"timestamp":2}),
        user("after abort"),
    ];
    let (_, reply) = stream(&session, request("s3", &after_abort, Some(SECRET)), None).await;
    assert_eq!(text_of(&reply), "Echo: after abort", "{reply}");
    let (query, sent) = last_query(&directory);
    assert_eq!(query["options"]["resume"], hung["options"]["sessionId"]);
    assert_eq!(sent["text"], "after abort");
    assert_eq!(sent["seen"], 2);

    // A query waiting for a tool result outlives the idle stop; a backstop stops it.
    let mut waiting = vec![user("hello wait")];
    let (_, reply) = stream(&session, request("wait", &waiting, Some(SECRET)), None).await;
    waiting.push(reply);
    waiting.push(user("please use a tool"));
    let (_, call) = stream(&session, request("wait", &waiting, Some(SECRET)), None).await;
    assert_eq!(call["stopReason"], "tooluse", "{call}");
    let (pending, _) = last_query(&directory);
    let closed = |n: &Value| {
        log(&directory)
            .iter()
            .any(|e| e["event"] == "close" && e["n"] == *n)
    };
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        !closed(&pending["n"]),
        "the idle stop must wait for the tool result"
    );
    eventually("the tool-wait backstop", || closed(&pending["n"])).await;

    // The tool call's result arrives after that: Claude's session forks at the
    // reply before the call, and the call and its result arrive as text.
    waiting.push(call);
    waiting.push(json!({"role":"toolResult","toolCallId":"toolu_1","toolName":"read","content":[{"type":"text","text":"late body"}],"isError":false,"timestamp":2}));
    let (_, reply) = stream(&session, request("wait", &waiting, Some(SECRET)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let entries = log(&directory);
    let fork = entries.iter().rfind(|e| e["event"] == "fork").unwrap();
    assert_eq!(fork["from"], pending["options"]["sessionId"]);
    assert_eq!(fork["kept"], 2);
    let (query, sent) = last_query(&directory);
    assert_eq!(query["options"]["resume"], fork["to"]);
    let text = sent["text"].as_str().unwrap();
    assert!(
        text.contains("[tool call read toolu_1]")
            && text.contains("[tool result read toolu_1]\nlate body"),
        "{text}"
    );
    assert_eq!(sent["seen"], 2);

    // An unreadable record starts fresh rather than failing or using a stale copy.
    let corrupt = vec![user("hello corrupt")];
    let (_, reply) = stream(&session, request("corrupt", &corrupt, Some(SECRET)), None).await;
    let (first_corrupt, _) = last_query(&directory);
    let (path, _) = record_of(
        &directory,
        first_corrupt["options"]["sessionId"].as_str().unwrap(),
    )
    .unwrap();
    std::fs::write(path, "not json").unwrap();
    let mut changed = request(
        "corrupt",
        &[corrupt[0].clone(), reply, user("after corrupt")],
        Some(SECRET),
    );
    changed["model"]["id"] = json!("claude-sonnet-5-5");
    let (_, reply) = stream(&session, changed, None).await;
    assert_eq!(text_of(&reply), "Echo: after corrupt", "{reply}");
    let (query, _) = last_query(&directory);
    assert!(query["options"].get("resume").is_none(), "{query}");
    assert_ne!(
        query["options"]["sessionId"],
        first_corrupt["options"]["sessionId"]
    );

    // An idle query stops; the next request resumes its session with only the new message.
    let mut idle = vec![user("first idle")];
    let (_, reply) = stream(&session, request("idle", &idle, Some(SECRET)), None).await;
    let (first, _) = last_query(&directory);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert!(
        log(&directory)
            .iter()
            .any(|e| e["event"] == "close" && e["n"] == first["n"]),
        "the idle query stopped"
    );
    idle.push(reply);
    idle.push(user("after idle"));
    let (_, reply) = stream(&session, request("idle", &idle, Some(SECRET)), None).await;
    let (query, sent) = last_query(&directory);
    assert_ne!(query["n"], first["n"]);
    let claude = first["options"]["sessionId"].clone();
    assert_eq!(query["options"]["resume"], claude);
    assert_eq!(sent["text"], "after idle");
    assert_eq!(sent["seen"], 2);

    // A different model restarts Claude Code on the same Claude session.
    idle.push(reply);
    idle.push(user("on sonnet"));
    let mut sonnet = request("idle", &idle, Some(SECRET));
    sonnet["model"]["id"] = json!("claude-sonnet-5-5");
    let (_, reply) = stream(&session, sonnet, None).await;
    let (query, sent) = last_query(&directory);
    assert_eq!(query["options"]["model"], "claude-sonnet-5-5");
    assert_eq!(query["options"]["resume"], claude);
    assert_eq!(sent["text"], "on sonnet");

    // A different token starts a fresh Claude session.
    idle.push(reply);
    idle.push(user("new account"));
    let (_, reply) = stream(&session, request("idle", &idle, Some(OTHER)), None).await;
    assert_eq!(reply["stopReason"], "stop", "{reply}");
    let (query, sent) = last_query(&directory);
    assert!(query["options"].get("resume").is_none(), "{query}");
    assert_ne!(query["options"]["sessionId"], claude);
    let other_claude = query["options"]["sessionId"].clone();
    assert!(sent["text"]
        .as_str()
        .unwrap()
        .contains("[user]\nfirst idle"));
    assert_eq!(sent["seen"], 0);
    idle.push(reply);

    // No token: a clear error, and no query.
    let (_, missing) = stream(&session, request("s4", &[user("hello")], None), None).await;
    assert_eq!(missing["stopReason"], "error");
    assert!(missing["errorMessage"]
        .as_str()
        .unwrap()
        .contains("claude setup-token"));

    session.close().await.unwrap();

    // A new worker (an rpi restart) resumes from the saved record.
    idle.push(user("after restart"));
    let restarted = open_worker(&directory).await.unwrap();
    let (_, reply) = stream(&restarted, request("idle", &idle, Some(OTHER)), None).await;
    assert_eq!(text_of(&reply), "Echo: after restart", "{reply}");
    let (query, sent) = last_query(&directory);
    assert_eq!(query["options"]["resume"], other_claude);
    assert_eq!(sent["text"], "after restart");
    restarted.close().await.unwrap();

    // The records never hold a token.
    let records: Vec<String> =
        std::fs::read_dir(directory.0.join("agent/claude-code/rpi-sessions"))
            .unwrap()
            .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
            .collect();
    assert!(records.len() >= 4, "{records:?}");
    for record in records {
        assert!(record.contains("checkpoints"), "{record}");
        assert!(
            !record.contains(SECRET) && !record.contains(OTHER),
            "{record}"
        );
    }

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

/// The prompt split on its own, for the shapes the end-to-end test does not send.
#[test]
#[ignore = "requires Node 24+ and npm ci in worker/"]
fn system_prompt_split_keeps_everything_but_the_scaffold_and_cwd() {
    let script = r#"
const { splitSystemPrompt } = await import(process.argv[1]);
const note = "<runtime_info>You are running in rpi through the Claude Code harness. Mention this only when asked.</runtime_info>";
const scaffold = "You are an expert coding assistant operating inside rpi, a coding agent harness. You help.\n\nAvailable tools:\n- read  — Read files\n\nIn addition to the tools above, you may have access to other custom tools depending on the project.\n\nGuidelines:\n- Use docs before guessing\n- Be concise\n  continued";
const kept = "Guidelines:\n- Use docs before guessing\n- Be concise\n  continued";
const cases = [
  // Released rpi: the scaffold ends with its cwd line, then appended text and context.
  [scaffold + "\n\nCurrent working directory: /old/style\n\nEXTRA\n\n<project_context>\nx\n</project_context>\n", note + "\n\n" + kept + "\n\nEXTRA\n\n<project_context>\nx\n</project_context>", "/old/style"],
  // Released rpi, the scaffold alone.
  [scaffold + "\n\nCurrent working directory: /old", note + "\n\n" + kept, "/old"],
  // Current rpi: the scaffold alone with a <cwd> section.
  [scaffold + "\n\n<cwd>\n/w\n</cwd>", note + "\n\n" + kept, "/w"],
  // No Guidelines boundary: nothing is dropped.
  ["You are an expert coding assistant operating inside rpi, a coding agent harness. Trimmed.", note + "\n\nYou are an expert coding assistant operating inside rpi, a coding agent harness. Trimmed.", undefined],
  // A custom prompt that mentions <cwd> mid-text keeps it.
  ["Custom.\n\n<cwd>\n/a\n</cwd>\n\nmore", note + "\n\nCustom.\n\n<cwd>\n/a\n</cwd>\n\nmore", undefined],
  ["", note, undefined],
];
for (const [prompt, append, cwd] of cases) {
  const got = splitSystemPrompt(prompt);
  if (got.append !== append || got.cwd !== cwd) throw new Error(JSON.stringify({ prompt, got }));
}
console.log("split ok");
"#;
    let output = std::process::Command::new(
        std::env::var_os("RPI_CLAUDE_AGENT_NODE").unwrap_or_else(|| "node".into()),
    )
    .args(["--input-type=module", "-e", script])
    .arg(package().join("worker/main.mjs"))
    .env("RPI_CLAUDE_AGENT_SDK", package().join("tests/fake-sdk.mjs"))
    .output()
    .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && text.contains("split ok"),
        "{text}"
    );
}

//! Behavioural tests for the Claude Code Messages request and its SSE loop.

use rpi_anthropic_auth::billing::VersionSettings;
use rpi_anthropic_auth::stream::EventSink;
use rpi_anthropic_auth::transport::{self, Body, Prepared, Request, Response, Source};
use rpi_anthropic_auth::version::LearnedFloor;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

const OAUTH_TOKEN: &str = "sk-ant-oat01-abcdefghijklmnop";

fn settings() -> VersionSettings {
    VersionSettings {
        pinned: "2.1.280".to_string(),
        has_override: false,
    }
}

fn model(id: &str) -> Value {
    json!({
        "id": id,
        "name": id,
        "provider": "anthropic",
        "api": "anthropic-messages",
        "reasoning": true,
        "maxTokens": 9000,
        "cost": {"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75},
    })
}

fn context() -> Value {
    json!({
        "systemPrompt": "You are Pi.",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": [],
    })
}

fn prepare_with(model: &Value, options: Value) -> Result<Prepared, String> {
    transport::prepare(
        model,
        &context(),
        &options,
        &HashMap::new(),
        &settings(),
        &LearnedFloor::default(),
    )
}

fn oauth_options() -> Value {
    json!({"apiKey": OAUTH_TOKEN})
}

fn header<'a>(prepared: &'a Prepared, name: &str) -> Option<&'a str> {
    prepared
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn body(prepared: &Prepared) -> Value {
    serde_json::from_str(&prepared.body).expect("the request body is JSON")
}

#[test]
fn oauth_requests_carry_the_claude_code_fingerprint() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    assert!(prepared.oauth);
    assert_eq!(
        header(&prepared, "authorization"),
        Some(format!("Bearer {OAUTH_TOKEN}").as_str())
    );
    assert_eq!(header(&prepared, "x-api-key"), None);
    assert_eq!(
        header(&prepared, "anthropic-beta"),
        Some(transport::REQUIRED_BETAS.join(",").as_str())
    );
    assert_eq!(header(&prepared, "user-agent"), Some("claude-cli/2.1.280"));
    assert_eq!(header(&prepared, "x-app"), Some("cli"));
    assert_eq!(
        header(&prepared, "anthropic-dangerous-direct-browser-access"),
        Some("true")
    );
    assert_eq!(header(&prepared, "anthropic-version"), Some("2023-06-01"));
}

#[test]
fn api_key_requests_are_not_dressed_as_claude_code() {
    let prepared = prepare_with(
        &model("claude-sonnet-4-6"),
        json!({"apiKey": "sk-ant-api03-key"}),
    )
    .expect("prepares");
    assert!(!prepared.oauth);
    assert_eq!(header(&prepared, "x-api-key"), Some("sk-ant-api03-key"));
    assert_eq!(header(&prepared, "authorization"), None);
    assert_eq!(header(&prepared, "user-agent"), None);
    assert_eq!(
        header(&prepared, "anthropic-beta"),
        Some(transport::API_KEY_BETA)
    );
}

#[test]
fn a_missing_credential_names_the_login_the_user_needs() {
    let error = match prepare_with(&model("claude-sonnet-4-6"), json!({})) {
        Ok(_) => panic!("a request without a credential must not prepare"),
        Err(error) => error,
    };
    assert_eq!(
        error,
        "No Anthropic auth available. Run /login and choose Claude Pro/Max."
    );
}

#[test]
fn headers_merge_case_insensitively_and_null_deletes() {
    let mut model = model("claude-sonnet-4-6");
    model["headers"] = json!({"User-Agent": "custom/1.0", "Accept": null, "x-api-key": "leak"});
    let prepared = prepare_with(&model, oauth_options()).expect("prepares");
    assert_eq!(header(&prepared, "user-agent"), Some("custom/1.0"));
    assert_eq!(header(&prepared, "accept"), None);
    // An OAuth request never lets another credential override its own.
    assert_eq!(header(&prepared, "x-api-key"), None);
}

#[test]
fn max_tokens_defaults_to_a_third_of_the_window() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    assert_eq!(body(&prepared)["max_tokens"], json!(3000));
    let explicit = prepare_with(
        &model("claude-sonnet-4-6"),
        json!({"apiKey": OAUTH_TOKEN, "maxTokens": 512}),
    )
    .expect("prepares");
    assert_eq!(body(&explicit)["max_tokens"], json!(512));
}

#[test]
fn adaptive_models_send_an_effort_level() {
    let prepared = prepare_with(
        &model("claude-sonnet-4-6"),
        json!({"apiKey": OAUTH_TOKEN, "reasoning": "Medium"}),
    )
    .expect("prepares");
    let body = body(&prepared);
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "summarized"})
    );
    assert_eq!(body["output_config"], json!({"effort": "medium"}));
}

#[test]
fn the_model_maps_a_reasoning_level_before_the_default_table() {
    let mut model = model("claude-sonnet-4-6");
    model["thinkingLevelMap"] = json!({"high": "xhigh"});
    let prepared = prepare_with(&model, json!({"apiKey": OAUTH_TOKEN, "reasoning": "high"}))
        .expect("prepares");
    assert_eq!(body(&prepared)["output_config"], json!({"effort": "xhigh"}));
}

#[test]
fn legacy_models_send_a_budget_that_fits_inside_max_tokens() {
    let prepared = prepare_with(
        &model("claude-sonnet-4-5"),
        json!({"apiKey": OAUTH_TOKEN, "reasoning": "high"}),
    )
    .expect("prepares");
    let body = body(&prepared);
    assert_eq!(body["max_tokens"], json!(3000));
    assert_eq!(
        body["thinking"],
        json!({"type": "enabled", "budget_tokens": 2999})
    );
    assert_eq!(body.get("output_config"), None);
}

#[test]
fn a_budget_below_the_floor_disables_thinking() {
    let prepared = prepare_with(
        &model("claude-sonnet-4-5"),
        json!({"apiKey": OAUTH_TOKEN, "reasoning": "high", "thinkingBudgets": {"high": 500}}),
    )
    .expect("prepares");
    assert_eq!(body(&prepared).get("thinking"), None);
}

#[test]
fn reasoning_is_off_when_the_caller_asks_for_none() {
    let prepared = prepare_with(
        &model("claude-sonnet-4-6"),
        json!({"apiKey": OAUTH_TOKEN, "reasoning": "Off"}),
    )
    .expect("prepares");
    assert_eq!(body(&prepared).get("thinking"), None);
}

#[test]
fn the_billing_header_opens_the_system_prompt() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let body = body(&prepared);
    let system = body["system"].as_array().expect("system blocks");
    let first = system[0]["text"].as_str().expect("header text");
    assert!(
        first.starts_with("x-anthropic-billing-header: cc_version=2.1.280."),
        "{first}"
    );
    assert_eq!(body["stream"], json!(true));
    assert_eq!(body["model"], json!("claude-sonnet-4-6"));
}

/// A source that replays canned responses and records the requests it saw.
struct Fixture {
    responses: Mutex<Vec<(u16, String)>>,
    opened: Mutex<Vec<Request>>,
    now: i64,
}

impl Fixture {
    fn new(responses: Vec<(u16, String)>) -> Self {
        Self {
            responses: Mutex::new(responses),
            opened: Mutex::new(Vec::new()),
            now: 1_000,
        }
    }

    fn opened(&self) -> Vec<(String, String)> {
        self.opened
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|request| (request.url.clone(), request.body.clone()))
            .collect()
    }
}

impl Source for Fixture {
    type Body = Chunks;

    fn now_ms(&self) -> i64 {
        self.now
    }

    async fn open(&self, request: Request) -> Result<Response<Chunks>, String> {
        self.opened
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(request);
        let mut responses = self
            .responses
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(!responses.is_empty(), "an unexpected request was opened");
        let (status, body) = responses.remove(0);
        Ok(Response {
            status,
            headers: Vec::new(),
            body: Chunks::new(&body),
        })
    }
}

/// Yields one line per chunk so the SSE parser's buffering is exercised.
struct Chunks {
    pieces: Vec<Vec<u8>>,
}

impl Chunks {
    fn new(text: &str) -> Self {
        let mut pieces = Vec::new();
        let bytes = text.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            let end = (index + 7).min(bytes.len());
            pieces.push(bytes[index..end].to_vec());
            index = end;
        }
        Self { pieces }
    }
}

impl Body for Chunks {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.pieces.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.pieces.remove(0)))
    }
}

struct Sink {
    events: Vec<Value>,
}

impl Sink {
    fn new() -> Self {
        Self { events: Vec::new() }
    }

    fn types(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| event["type"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

impl EventSink for Sink {
    async fn emit(&mut self, event: Value) -> Result<(), String> {
        self.events.push(event);
        Ok(())
    }
}

fn sse(events: &[(&str, Value)]) -> String {
    let mut text = String::new();
    for (name, payload) in events {
        text.push_str(&format!("event: {name}\ndata: {payload}\n\n"));
    }
    text
}

fn text_stream() -> String {
    sse(&[
        (
            "message_start",
            json!({"type":"message_start","message":{"usage":{"input_tokens":100,"cache_read_input_tokens":20}}}),
        ),
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":50}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ])
}

async fn stream(prepared: &Prepared, source: &Fixture, caller: CancellationToken) -> (Value, Sink) {
    let mut sink = Sink::new();
    let message = transport::run(source, prepared, caller, &mut sink).await;
    (message, sink)
}

#[tokio::test]
async fn a_text_turn_streams_and_reports_cost() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let source = Fixture::new(vec![(200, text_stream())]);
    let (message, sink) = stream(&prepared, &source, CancellationToken::new()).await;
    assert_eq!(
        sink.types(),
        vec!["start", "text_start", "text_delta", "text_end", "done"]
    );
    assert_eq!(message["content"][0]["text"], json!("Hi"));
    assert_eq!(message["stopReason"], json!("stop"));
    assert_eq!(message["usage"]["input"], json!(100.0));
    assert_eq!(message["usage"]["output"], json!(50.0));
    assert_eq!(message["usage"]["cacheRead"], json!(20.0));
    assert_eq!(message["usage"]["totalTokens"], json!(170.0));
    let cost = message["usage"]["cost"]["total"].as_f64().expect("cost");
    let expected = (100.0 * 3.0 + 50.0 * 15.0 + 20.0 * 0.3) / 1_000_000.0;
    assert!((cost - expected).abs() < 1e-12, "{cost} vs {expected}");
    assert_eq!(message["durationMs"], json!(0));
}

#[tokio::test]
async fn tool_arguments_arrive_as_fragments_and_close_as_one_object() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let body = sse(&[
        (
            "content_block_start",
            json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool_1","name":"mcp__pi__todo_write"}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"a\":"}}),
        ),
        (
            "content_block_delta",
            json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"1}"}}),
        ),
        (
            "content_block_stop",
            json!({"type":"content_block_stop","index":0}),
        ),
        (
            "message_delta",
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        ),
        ("message_stop", json!({"type":"message_stop"})),
    ]);
    let source = Fixture::new(vec![(200, body)]);
    let (message, sink) = stream(&prepared, &source, CancellationToken::new()).await;
    assert_eq!(message["stopReason"], json!("toolUse"));
    assert_eq!(message["content"][0]["arguments"], json!({"a": 1}));
    // OAuth tool names come back in Pi's spelling.
    assert_eq!(message["content"][0]["name"], json!("todo_write"));
    assert!(sink.types().contains(&"toolcall_end".to_string()));
}

#[tokio::test]
async fn an_http_error_is_reported_through_the_message() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let source = Fixture::new(vec![(500, "boom".to_string())]);
    let (message, sink) = stream(&prepared, &source, CancellationToken::new()).await;
    assert_eq!(message["stopReason"], json!("error"));
    assert_eq!(
        message["errorMessage"],
        json!("Anthropic request failed: HTTP 500 boom")
    );
    assert_eq!(
        sink.events.last().expect("an event")["type"],
        json!("error")
    );
}

#[tokio::test]
async fn a_truncated_stream_is_an_error() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let body = sse(&[(
        "content_block_start",
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    )]);
    let source = Fixture::new(vec![(200, body)]);
    let (message, _) = stream(&prepared, &source, CancellationToken::new()).await;
    assert_eq!(message["stopReason"], json!("error"));
    assert_eq!(
        message["errorMessage"],
        json!("Anthropic stream ended before message_stop")
    );
}

#[tokio::test]
async fn cancellation_reports_an_aborted_turn() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let source = Fixture::new(vec![(200, text_stream())]);
    let caller = CancellationToken::new();
    caller.cancel();
    let (message, sink) = stream(&prepared, &source, caller).await;
    assert_eq!(message["stopReason"], json!("aborted"));
    assert_eq!(message["errorMessage"], json!("Request aborted"));
    assert_eq!(
        sink.events.last().expect("an event")["type"],
        json!("error")
    );
}

#[tokio::test]
async fn a_version_rejection_retries_once_at_the_named_floor() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let rejection = json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": "Claude Code 2.1.280 does not support this model; version 2.1.300 or newer is required.",
            "details": {"error_code": "claude_code_version_too_old"},
        }
    })
    .to_string();
    let source = Fixture::new(vec![(400, rejection), (200, text_stream())]);
    let (message, _) = stream(&prepared, &source, CancellationToken::new()).await;
    assert_eq!(message["stopReason"], json!("stop"));
    let opened = source.opened();
    assert_eq!(opened.len(), 2);
    assert!(
        opened[0].1.contains("cc_version=2.1.280"),
        "{}",
        opened[0].1
    );
    assert!(
        opened[1].1.contains("cc_version=2.1.300"),
        "{}",
        opened[1].1
    );
}

#[tokio::test]
async fn an_unrecoverable_rejection_keeps_the_hint() {
    let prepared = prepare_with(&model("claude-sonnet-4-6"), oauth_options()).expect("prepares");
    let rejection = json!({
        "type": "error",
        "error": {
            "type": "invalid_request_error",
            "message": "Claude Code 2.1.280 does not support this model; version 2.1.300 or newer is required.",
            "details": {"error_code": "claude_code_version_too_old"},
        }
    })
    .to_string();
    let repaired = rejection.clone();
    let source = Fixture::new(vec![(400, rejection), (400, repaired)]);
    let (message, _) = stream(&prepared, &source, CancellationToken::new()).await;
    assert_eq!(message["stopReason"], json!("error"));
    let error = message["errorMessage"].as_str().expect("an error message");
    assert!(
        error.contains("2221.300") || error.contains("2.1.300"),
        "{error}"
    );
    assert!(
        error.contains("Automatic recovery did not succeed"),
        "{error}"
    );
}

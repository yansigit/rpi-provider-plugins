//! Assistant-message state: the mapping from Anthropic's stream vocabulary onto
//! Pi's events, and the usage arithmetic the host bills from.

use rpi_anthropic_auth::stream::{map_stop_reason, EventSink, State};
use serde_json::{json, Value};

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

fn model() -> Value {
    json!({
        "id": "claude-sonnet-4-6",
        "provider": "anthropic",
        "api": "anthropic-messages",
        "cost": {"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75},
    })
}

#[test]
fn every_anthropic_stop_reason_maps_onto_a_pi_reason() {
    assert_eq!(map_stop_reason("end_turn"), "stop");
    assert_eq!(map_stop_reason("pause_turn"), "stop");
    assert_eq!(map_stop_reason("stop_sequence"), "stop");
    assert_eq!(map_stop_reason("max_tokens"), "length");
    assert_eq!(map_stop_reason("tool_use"), "toolUse");
    assert_eq!(map_stop_reason("refusal"), "error");
    assert_eq!(map_stop_reason(""), "error");
}

#[test]
fn the_message_starts_as_an_empty_assistant_turn() {
    let state = State::new(&model(), 1_700);
    assert_eq!(state.output["role"], json!("assistant"));
    assert_eq!(state.output["api"], json!("anthropic-messages"));
    assert_eq!(state.output["provider"], json!("anthropic"));
    assert_eq!(state.output["model"], json!("claude-sonnet-4-6"));
    assert_eq!(state.output["content"], json!([]));
    assert_eq!(state.output["stopReason"], json!("stop"));
    assert_eq!(state.output["timestamp"], json!(1700));
    assert_eq!(state.output["usage"]["totalTokens"], json!(0));
}

#[tokio::test]
async fn thinking_and_its_signature_land_on_one_block() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    state.start(&mut sink).await.expect("start");
    state
        .block_start(0, &json!({"type": "thinking"}), &mut sink)
        .await
        .expect("block start");
    state
        .block_delta(
            0,
            &json!({"type": "thinking_delta", "thinking": "hmm"}),
            &mut sink,
        )
        .await
        .expect("delta");
    state
        .block_delta(
            0,
            &json!({"type": "signature_delta", "signature": "sig"}),
            &mut sink,
        )
        .await
        .expect("signature");
    state.block_stop(0, &mut sink).await.expect("block stop");
    assert_eq!(
        state.output["content"],
        json!([{"type": "thinking", "thinking": "hmm", "thinkingSignature": "sig"}])
    );
    // The signature is block state, not a streamed event.
    assert_eq!(
        sink.types(),
        vec!["start", "thinking_start", "thinking_delta", "thinking_end"]
    );
}

#[tokio::test]
async fn redacted_thinking_keeps_the_payload_as_the_signature() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    state
        .block_start(
            0,
            &json!({"type": "redacted_thinking", "data": "cipher"}),
            &mut sink,
        )
        .await
        .expect("block start");
    assert_eq!(
        state.output["content"],
        json!([{
            "type": "thinking",
            "thinking": "[Reasoning redacted]",
            "thinkingSignature": "cipher",
            "redacted": true,
        }])
    );
}

#[tokio::test]
async fn usage_arrives_in_two_halves_and_is_never_overwritten() {
    let mut state = State::new(&model(), 0);
    state.apply_usage(Some(
        &json!({"input_tokens": 100, "cache_read_input_tokens": 20}),
    ));
    assert_eq!(state.output["usage"]["input"], json!(100.0));
    assert_eq!(state.output["usage"]["cacheRead"], json!(20.0));
    state.message_delta(
        &json!({"stop_reason": "max_tokens"}),
        Some(&json!({"output_tokens": 50})),
    );
    assert_eq!(state.output["stopReason"], json!("length"));
    assert_eq!(state.output["usage"]["input"], json!(100.0));
    assert_eq!(state.output["usage"]["output"], json!(50.0));
    assert_eq!(state.output["usage"]["totalTokens"], json!(170.0));
    let cost = state.output["usage"]["cost"]["total"]
        .as_f64()
        .expect("a total");
    let expected = (100.0 * 3.0 + 50.0 * 15.0 + 20.0 * 0.3) / 1_000_000.0;
    assert!((cost - expected).abs() < 1e-12, "{cost} vs {expected}");
}

#[tokio::test]
async fn reasoning_tokens_are_reported_next_to_output() {
    let mut state = State::new(&model(), 0);
    state.apply_usage(Some(
        &json!({"output_tokens": 50, "output_tokens_details": {"thinking_tokens": 7}}),
    ));
    assert_eq!(state.output["usage"]["output"], json!(50.0));
    assert_eq!(state.output["usage"]["reasoning"], json!(7.0));
    assert_eq!(state.output["usage"]["totalTokens"], json!(50.0));
}

#[tokio::test]
async fn an_unknown_stop_reason_is_an_error_the_user_can_read() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    state.message_delta(
        &json!({"stop_reason": "refusal", "stop_details": {"explanation": "policy"}}),
        None,
    );
    assert_eq!(state.output["stopReason"], json!("error"));
    assert_eq!(
        state.output["errorMessage"],
        json!("Provider stop reason: refusal — policy")
    );
    state.finish(42, &mut sink).await.expect("finish");
    assert_eq!(sink.events[0]["type"], json!("error"));
    assert_eq!(sink.events[0]["reason"], json!("error"));
    assert_eq!(sink.events[0]["error"]["durationMs"], json!(42));
}

#[tokio::test]
async fn an_unmapped_stop_reason_names_itself() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    state.message_delta(&json!({"stop_reason": "mystery"}), None);
    state.finish(0, &mut sink).await.expect("finish");
    assert_eq!(state.output["stopReason"], json!("error"));
    assert_eq!(
        state.output["errorMessage"],
        json!("Provider stop reason: mystery")
    );
    assert_eq!(sink.events[0]["type"], json!("error"));
}

#[tokio::test]
async fn a_tool_call_without_a_name_is_a_contract_break() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    let error = state
        .block_start(0, &json!({"type": "tool_use", "id": "tool_1"}), &mut sink)
        .await
        .expect_err("a nameless tool call");
    assert_eq!(error, "Anthropic tool_use block has no name");
}

#[tokio::test]
async fn tool_names_pass_through_unchanged_for_api_keys() {
    let mut state = State::new(&model(), 0);
    state.name_tools(false, &[]);
    let mut sink = Sink::new();
    state
        .block_start(
            0,
            &json!({"type": "tool_use", "id": "tool_1", "name": "todo_write"}),
            &mut sink,
        )
        .await
        .expect("block start");
    assert_eq!(state.output["content"][0]["name"], json!("todo_write"));
    assert_eq!(state.output["content"][0]["arguments"], json!({}));
}

#[tokio::test]
async fn unsupported_blocks_and_deltas_are_named_in_the_error() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    let block = state
        .block_start(0, &json!({"type": "image"}), &mut sink)
        .await
        .expect_err("an image block");
    assert_eq!(block, "Unsupported Anthropic content block type: \"image\"");
    state
        .block_start(0, &json!({"type": "text"}), &mut sink)
        .await
        .expect("a text block");
    let delta = state
        .block_delta(0, &json!({"type": "citation_delta"}), &mut sink)
        .await
        .expect_err("a citation delta");
    assert_eq!(
        delta,
        "Unsupported Anthropic content delta type: \"citation_delta\""
    );
}

#[tokio::test]
async fn a_delta_before_its_block_is_a_contract_break() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    let error = state
        .block_delta(4, &json!({"type": "text_delta", "text": "x"}), &mut sink)
        .await
        .expect_err("an early delta");
    assert_eq!(
        error,
        "Anthropic stream referenced content index 4 before it began"
    );
}

#[tokio::test]
async fn a_failure_marks_the_turn_aborted_only_when_it_was_cancelled() {
    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    state
        .fail("Request aborted", true, 12, &mut sink)
        .await
        .expect("fail");
    assert_eq!(state.output["stopReason"], json!("aborted"));
    assert_eq!(state.output["errorMessage"], json!("Request aborted"));
    assert_eq!(state.output["durationMs"], json!(12));
    assert_eq!(sink.events[0]["reason"], json!("aborted"));
    assert_eq!(sink.events[0]["error"]["stopReason"], json!("aborted"));

    let mut state = State::new(&model(), 0);
    let mut sink = Sink::new();
    state
        .fail("Anthropic request failed: HTTP 500", false, 0, &mut sink)
        .await
        .expect("fail");
    assert_eq!(state.output["stopReason"], json!("error"));
}

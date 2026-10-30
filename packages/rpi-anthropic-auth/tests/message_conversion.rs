//! Behavioural tests for Pi → Anthropic message and tool conversion.

use rpi_anthropic_auth::convert::{
    cache_control_for, convert_pi_messages_to_anthropic, convert_pi_tools_to_anthropic,
    from_claude_code_tool_name, resolve_cache_retention, sanitize_surrogates,
    to_claude_code_tool_name, CacheRetention, ModelRef,
};
use serde_json::{json, Value};

fn model() -> ModelRef {
    ModelRef {
        provider: "anthropic".to_string(),
        api: "anthropic-messages".to_string(),
        id: "claude-sonnet-4-5".to_string(),
    }
}

/// A Pi assistant message that claims the model under test.
fn assistant(content: Vec<Value>) -> Value {
    json!({
        "role": "assistant",
        "content": content,
        "provider": "anthropic",
        "api": "anthropic-messages",
        "model": "claude-sonnet-4-5",
        "usage": {"input": 1, "output": 1},
        "stopReason": "stop",
    })
}

fn user_text(text: &str) -> Value {
    json!({"role": "user", "content": text})
}

fn tool_call(id: &str, name: &str, arguments: Value) -> Value {
    json!({"type": "toolCall", "id": id, "name": name, "arguments": arguments})
}

fn tool_result(id: &str, text: &str) -> Value {
    json!({
        "role": "toolResult",
        "toolCallId": id,
        "content": [{"type": "text", "text": text}],
        "isError": false,
    })
}

fn convert(messages: &[Value], is_oauth: bool, retention: CacheRetention) -> Vec<Value> {
    convert_pi_messages_to_anthropic(messages, is_oauth, &model(), retention)
        .expect("Pi messages convert")
}

#[test]
fn known_tool_names_map_to_the_claude_code_spelling() {
    assert_eq!(to_claude_code_tool_name("read"), "Read");
    assert_eq!(to_claude_code_tool_name("BASH"), "Bash");
    assert_eq!(to_claude_code_tool_name("Grep"), "Grep");
    assert_eq!(
        to_claude_code_tool_name("AskUserQuestion"),
        "AskUserQuestion"
    );
}

#[test]
fn custom_tool_names_are_namespaced_and_sanitized() {
    assert_eq!(
        to_claude_code_tool_name("todo_write"),
        "mcp__pi__todo_write"
    );
    assert_eq!(to_claude_code_tool_name("my tool!"), "mcp__pi__my_tool_");
}

#[test]
fn names_maps_back_using_the_request_tool_list() {
    let tools = vec![json!({"name": "my tool"}), json!({"name": "bash"})];
    assert_eq!(
        from_claude_code_tool_name("mcp__pi__my_tool", &tools),
        "my tool"
    );
    assert_eq!(from_claude_code_tool_name("Bash", &tools), "bash");
    assert_eq!(from_claude_code_tool_name("Bash", &[]), "Bash");
    assert_eq!(
        from_claude_code_tool_name("mcp__pi__unknown", &tools),
        "unknown"
    );
}

#[test]
fn tool_calls_are_rewritten_into_the_claude_code_namespace_for_oauth() {
    let messages = vec![
        assistant(vec![tool_call("call_1", "read", json!({"path": "a.rs"}))]),
        tool_result("call_1", "contents"),
    ];

    let oauth = convert(&messages, true, CacheRetention::None);
    assert_eq!(oauth[0]["content"][0]["name"], "Read");
    assert_eq!(oauth[0]["content"][0]["input"], json!({"path": "a.rs"}));

    let api_key = convert(&messages, false, CacheRetention::None);
    assert_eq!(api_key[0]["content"][0]["name"], "read");
}

#[test]
fn tool_call_arguments_default_to_an_empty_object() {
    let messages = vec![
        json!({
            "role": "assistant",
            "content": [{"type": "toolCall", "id": "call_1", "name": "bash", "arguments": null}],
        }),
        tool_result("call_1", "ok"),
    ];
    assert_eq!(
        convert(&messages, false, CacheRetention::None)[0]["content"][0]["input"],
        json!({})
    );
}

#[test]
fn tool_ids_are_sanitized_to_anthropics_character_set() {
    let messages = vec![
        assistant(vec![tool_call("call:1/2", "bash", json!({}))]),
        tool_result("call:1/2", "ok"),
    ];
    let params = convert(&messages, false, CacheRetention::None);

    assert_eq!(params[0]["content"][0]["id"], "call_1_2");
    assert_eq!(params[1]["content"][0]["tool_use_id"], "call_1_2");
}

#[test]
fn colliding_tool_ids_are_disambiguated() {
    let messages = vec![
        assistant(vec![
            tool_call("call.1", "bash", json!({})),
            tool_call("call_1", "bash", json!({})),
        ]),
        tool_result("call.1", "a"),
        tool_result("call_1", "b"),
    ];
    let params = convert(&messages, false, CacheRetention::None);

    assert_eq!(params[0]["content"][0]["id"], "call_1");
    assert_eq!(params[0]["content"][1]["id"], "call_1_1");
    assert_eq!(params[1]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(params[1]["content"][1]["tool_use_id"], "call_1_1");
}

#[test]
fn orphaned_tool_calls_are_patched_before_the_next_user_turn() {
    let messages = vec![
        assistant(vec![tool_call("call_1", "bash", json!({}))]),
        user_text("never mind"),
    ];
    let params = convert(&messages, false, CacheRetention::None);

    assert_eq!(params.len(), 3);
    assert_eq!(params[0]["role"], "assistant");
    assert_eq!(params[1]["role"], "user");
    assert_eq!(
        params[1]["content"],
        json!([{
            "type": "tool_result",
            "tool_use_id": "call_1",
            "content": "No result provided",
            "is_error": true,
        }])
    );
    assert_eq!(params[2], json!({"role": "user", "content": "never mind"}));
}

#[test]
fn a_history_ending_on_a_tool_call_still_gets_a_result() {
    let messages = vec![assistant(vec![tool_call("call_1", "bash", json!({}))])];
    let params = convert(&messages, false, CacheRetention::None);

    assert_eq!(params.len(), 2);
    assert_eq!(params[1]["content"][0]["is_error"], true);
}

#[test]
fn a_parallel_tool_run_pairs_orphans_inside_the_same_message() {
    let messages = vec![
        assistant(vec![
            tool_call("call_1", "bash", json!({})),
            tool_call("call_2", "read", json!({})),
        ]),
        tool_result("call_1", "done"),
    ];
    let params = convert(&messages, false, CacheRetention::None);

    let content = params[1]["content"].as_array().expect("tool results");
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["tool_use_id"], "call_1");
    assert_eq!(content[0]["content"], "done");
    assert_eq!(content[1]["tool_use_id"], "call_2");
    assert_eq!(content[1]["content"], "No result provided");
}

#[test]
fn tool_results_carry_pis_error_flag() {
    let messages = vec![
        assistant(vec![tool_call("call_1", "bash", json!({}))]),
        json!({
            "role": "toolResult",
            "toolCallId": "call_1",
            "content": [{"type": "text", "text": "boom"}],
            "isError": true,
        }),
    ];
    let params = convert(&messages, false, CacheRetention::None);

    assert_eq!(params[1]["content"][0]["is_error"], true);
    assert_eq!(params[1]["content"][0]["content"], "boom");
}

#[test]
fn multiple_text_tool_result_blocks_join_with_a_newline() {
    let messages = vec![
        assistant(vec![tool_call("call_1", "bash", json!({}))]),
        json!({
            "role": "toolResult",
            "toolCallId": "call_1",
            "content": [{"type": "text", "text": "line 1"}, {"type": "text", "text": "line 2"}],
        }),
    ];
    let params = convert(&messages, false, CacheRetention::None);
    assert_eq!(params[1]["content"][0]["content"], "line 1\nline 2");
}

#[test]
fn tool_result_images_keep_blocks_and_gain_a_placeholder() {
    let messages = vec![
        assistant(vec![tool_call("call_1", "screenshot", json!({}))]),
        json!({
            "role": "toolResult",
            "toolCallId": "call_1",
            "content": [{"type": "image", "mimeType": "image/png", "data": "AAAA"}],
        }),
    ];
    let params = convert(&messages, false, CacheRetention::None);

    assert_eq!(
        params[1]["content"][0]["content"],
        json!([
            {"type": "text", "text": "(see attached image)"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
        ])
    );
}

#[test]
fn blank_user_messages_are_dropped() {
    assert!(convert(&[user_text("   \n ")], false, CacheRetention::None).is_empty());

    let messages = vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "  "}, {"type": "text", "text": "hi"}],
    })];
    let params = convert(&messages, false, CacheRetention::None);
    assert_eq!(
        params,
        vec![json!({"role": "user", "content": [{"type": "text", "text": "hi"}]})]
    );
}

#[test]
fn blank_assistant_text_and_empty_turns_are_dropped() {
    assert!(convert(&[assistant(vec![])], false, CacheRetention::None).is_empty());
    assert!(convert(
        &[assistant(vec![json!({"type": "text", "text": "  "})])],
        false,
        CacheRetention::None
    )
    .is_empty());
}

#[test]
fn user_images_become_base64_sources() {
    let messages = vec![json!({
        "role": "user",
        "content": [{"type": "image", "mimeType": "image/jpeg", "data": "BBBB"}],
    })];
    assert_eq!(
        convert(&messages, false, CacheRetention::None)[0]["content"][0],
        json!({"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "BBBB"}})
    );
}

#[test]
fn aborted_and_errored_assistant_turns_are_skipped() {
    for stop_reason in ["aborted", "error"] {
        let messages = vec![
            json!({
                "role": "assistant",
                "content": [tool_call("call_1", "bash", json!({}))],
                "stopReason": stop_reason,
            }),
            user_text("retry"),
        ];
        assert_eq!(
            convert(&messages, false, CacheRetention::None),
            vec![json!({"role": "user", "content": "retry"})],
            "stopReason {stop_reason} should not poison the next request"
        );
    }
}

#[test]
fn thinking_replays_with_its_signature_only_for_the_same_model() {
    let thinking = json!({
        "type": "thinking",
        "thinking": "let me check",
        "thinkingSignature": "  sig-1  ",
        "redacted": false,
    });

    let same = convert(
        &[assistant(vec![thinking.clone()])],
        false,
        CacheRetention::None,
    );
    assert_eq!(
        same[0]["content"][0],
        json!({"type": "thinking", "thinking": "let me check", "signature": "sig-1"})
    );

    let mut other = assistant(vec![thinking]);
    other["model"] = json!("claude-opus-4-1");
    let cross = convert(&[other], false, CacheRetention::None);
    assert_eq!(
        cross[0]["content"][0],
        json!({"type": "text", "text": "let me check"})
    );
}

#[test]
fn unsigned_thinking_degrades_to_text() {
    let messages = vec![assistant(vec![json!({
        "type": "thinking",
        "thinking": "unsigned",
        "thinkingSignature": "   ",
    })])];
    assert_eq!(
        convert(&messages, false, CacheRetention::None)[0]["content"][0],
        json!({"type": "text", "text": "unsigned"})
    );
}

#[test]
fn redacted_thinking_replays_as_a_redacted_block() {
    let redacted = json!({
        "type": "thinking",
        "thinking": "",
        "thinkingSignature": "ciphertext",
        "redacted": true,
    });

    let same = convert(
        &[assistant(vec![redacted.clone()])],
        false,
        CacheRetention::None,
    );
    assert_eq!(
        same[0]["content"][0],
        json!({"type": "redacted_thinking", "data": "ciphertext"})
    );

    let mut other = assistant(vec![redacted]);
    other["model"] = json!("claude-opus-4-1");
    assert!(convert(&[other], false, CacheRetention::None).is_empty());
}

#[test]
fn short_retention_promotes_text_and_marks_the_last_block() {
    let messages = vec![
        assistant(vec![json!({"type": "text", "text": "yo"})]),
        user_text("hi"),
    ];
    let params = convert(&messages, false, CacheRetention::Short);

    assert!(params[0]["content"][0].get("cache_control").is_none());
    assert_eq!(
        params[1]["content"],
        json!([{"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}])
    );
}

#[test]
fn a_trailing_assistant_turn_is_not_a_breakpoint_host() {
    let messages = vec![
        user_text("hi"),
        assistant(vec![json!({"type": "text", "text": "yo"})]),
    ];
    let params = convert(&messages, false, CacheRetention::Short);

    assert_eq!(params[0]["content"], "hi");
    assert!(params[1]["content"][0].get("cache_control").is_none());
}

#[test]
fn long_retention_sets_the_one_hour_ttl() {
    let messages = vec![json!({
        "role": "user",
        "content": [{"type": "text", "text": "hi"}, {"type": "text", "text": "there"}],
    })];
    let params = convert(&messages, false, CacheRetention::Long);

    assert!(params[0]["content"][0].get("cache_control").is_none());
    assert_eq!(
        params[0]["content"][1]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

#[test]
fn disabled_retention_leaves_content_alone() {
    let params = convert(&[user_text("hi")], false, CacheRetention::None);
    assert_eq!(params[0]["content"], "hi");
    assert_eq!(cache_control_for(CacheRetention::None), None);
}

#[test]
fn retention_resolution_prefers_the_explicit_request() {
    assert!(matches!(
        resolve_cache_retention(None, None),
        CacheRetention::Short
    ));
    assert!(matches!(
        resolve_cache_retention(None, Some("long")),
        CacheRetention::Long
    ));
    assert!(matches!(
        resolve_cache_retention(None, Some("short")),
        CacheRetention::Short
    ));
    assert!(matches!(
        resolve_cache_retention(Some(CacheRetention::None), Some("long")),
        CacheRetention::None
    ));
    assert_eq!(
        cache_control_for(CacheRetention::Long),
        Some(json!({"type": "ephemeral", "ttl": "1h"}))
    );
    assert_eq!(
        cache_control_for(CacheRetention::Short),
        Some(json!({"type": "ephemeral"}))
    );
}

#[test]
fn tool_definitions_take_the_claude_code_namespace_for_oauth() {
    let tools = vec![json!({
        "name": "read_file",
        "description": "Read a file",
        "parameters": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]},
    })];

    let oauth = convert_pi_tools_to_anthropic(&tools, true).expect("tools convert");
    assert_eq!(
        oauth[0],
        json!({
            "name": "mcp__pi__read_file",
            "description": "Read a file",
            "input_schema": {
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        })
    );
    assert_eq!(
        convert_pi_tools_to_anthropic(&tools, false).expect("tools convert")[0]["name"],
        "read_file"
    );
}

#[test]
fn tool_definitions_without_parameters_get_empty_schemas() {
    let converted =
        convert_pi_tools_to_anthropic(&[json!({"name": "read"})], false).expect("tools convert");
    assert_eq!(
        converted[0]["input_schema"],
        json!({"type": "object", "properties": {}, "required": []})
    );
}

#[test]
fn surrogate_escapes_are_settled_at_the_json_boundary() {
    // Unpaired surrogates cannot be represented in a Rust string, so the JSON
    // parser rejects them before they can reach the converter.
    let lone = serde_json::from_str::<Value>("\"a\\ud800b\"").expect_err("lone surrogate");
    assert!(lone.to_string().contains("surrogate") || lone.to_string().contains("escape"));

    let paired: Value = serde_json::from_str("\"\\uD83D\\uDE00\"").expect("paired surrogate");
    let text = paired.as_str().expect("decoded text");
    assert_eq!(text, "\u{1f600}");
    assert_eq!(sanitize_surrogates(text), text);
    assert_eq!(
        convert(
            &[json!({"role": "user", "content": text})],
            false,
            CacheRetention::None
        )[0]["content"],
        text
    );
}

#[test]
fn unsupported_blocks_are_rejected_instead_of_silently_dropped() {
    let err = convert_pi_messages_to_anthropic(
        &[assistant(vec![json!({"type": "audio"})])],
        false,
        &model(),
        CacheRetention::None,
    )
    .expect_err("audio block is not supported");
    assert_eq!(
        err,
        "Invalid Pi message: unsupported assistant content block \"audio\""
    );

    let err = convert_pi_messages_to_anthropic(
        &[json!({"role": "system", "content": "x"})],
        false,
        &model(),
        CacheRetention::None,
    )
    .expect_err("system messages are not Pi transcript roles");
    assert_eq!(err, "Invalid Pi message: unsupported role \"system\"");
}

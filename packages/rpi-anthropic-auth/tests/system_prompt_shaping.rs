//! Behavioural tests for the section-aware system prompt sanitizer.

use rpi_anthropic_auth::prompt::{
    is_anthropic_oauth_access_token, named_section, parse_system_prompt_chunks,
    render_system_prompt_chunks, reset_shaping_warnings, shape_anthropic_oauth_payload,
    shape_anthropic_oauth_system_prompt, shape_system_update_text, MINIMAL_ANTHROPIC_OAUTH_PROMPT,
    MINIMAL_ANTHROPIC_OAUTH_PROMPT_PREFIX, PI_DEFAULT_PROMPT_PREFIX,
};
use serde_json::{json, Value};

/// The replacement phrase the sanitizer applies inside Pi's own sections.
const ENVIRONMENT_PHRASE: &str =
    "Here is some useful information about the environment you are running in:";
const ENVIRONMENT_REPLACEMENT: &str = "Environment context you are running in:";

fn section(name: &str, body: &str) -> String {
    named_section(name, body).raw
}

fn chunk_body(prompt: &str, name: &str) -> Option<String> {
    parse_system_prompt_chunks(prompt)
        .into_iter()
        .find(|chunk| chunk.name.as_deref() == Some(name))
        .map(|chunk| chunk.body)
}

fn default_preamble() -> String {
    format!(
        "{PI_DEFAULT_PROMPT_PREFIX}\n\nYou also answer questions about the codebase you are in."
    )
}

fn full_prompt() -> String {
    [
        default_preamble(),
        section("tools", "read_file: Read a file.\n\nbash: Run a command."),
        section("rules", "Be careful with destructive commands."),
        section(
            "docs",
            "Pi documentation (read only when the user asks about pi itself):\n\n- docs/tools.md",
        ),
        section("project_context", "# Repo\n\nUse tabs, never spaces."),
    ]
    .join("\n\n")
}

#[test]
fn minimal_prompt_starts_with_its_documented_prefix() {
    assert!(MINIMAL_ANTHROPIC_OAUTH_PROMPT.starts_with(MINIMAL_ANTHROPIC_OAUTH_PROMPT_PREFIX));
}

#[test]
fn parse_and_render_round_trip_preserves_bytes() {
    let prompt = full_prompt();
    assert_eq!(
        render_system_prompt_chunks(&parse_system_prompt_chunks(&prompt)),
        prompt
    );
}

#[test]
fn runs_of_blank_lines_stay_with_the_preceding_chunk() {
    let prompt = "preamble\n\n\n<tools>\nbody\n</tools>\n\n\ntrailing";
    let chunks = parse_system_prompt_chunks(prompt);
    assert_eq!(
        chunks
            .iter()
            .map(|chunk| (chunk.name.as_deref(), chunk.raw.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (None, "preamble\n"),
            (Some("tools"), "<tools>\nbody\n</tools>"),
            (None, "\ntrailing"),
        ]
    );
    assert_eq!(render_system_prompt_chunks(&chunks), prompt);
}

#[test]
fn tagged_attributes_inside_a_body_do_not_start_a_section() {
    let body = "<project_instructions path=\"AGENTS.md\">\nFollow it.\n</project_instructions>";
    let prompt = format!("preamble\n\n{}", section("project_context", body));
    let chunks = parse_system_prompt_chunks(&prompt);
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[1].name.as_deref(), Some("project_context"));
    assert_eq!(chunks[1].body, body);
}

#[test]
fn preamble_becomes_the_minimal_prompt_and_other_sections_survive() {
    let prompt = full_prompt();
    let shaped = shape_anthropic_oauth_system_prompt(&prompt);

    assert!(!shaped.contains(PI_DEFAULT_PROMPT_PREFIX));
    assert!(!shaped.contains("You also answer questions about the codebase"));

    let chunks = parse_system_prompt_chunks(&shaped);
    assert_eq!(chunks[0].name, None);
    assert_eq!(chunks[0].body, MINIMAL_ANTHROPIC_OAUTH_PROMPT);
    assert!(shaped.starts_with(MINIMAL_ANTHROPIC_OAUTH_PROMPT));

    assert!(shaped.contains(&section(
        "tools",
        "read_file: Read a file.\n\nbash: Run a command."
    )));
    assert!(shaped.contains(&section("rules", "Be careful with destructive commands.")));
    assert_eq!(
        chunk_body(&shaped, "project_context").as_deref(),
        Some("# Repo\n\nUse tabs, never spaces.")
    );
}

#[test]
fn docs_section_is_dropped_on_its_anchor() {
    let shaped = shape_anthropic_oauth_system_prompt(&full_prompt());
    assert!(!shaped.contains("<docs>"));
    assert!(!shaped.contains("docs/tools.md"));
    assert!(parse_system_prompt_chunks(&shaped)
        .iter()
        .all(|chunk| chunk.name.as_deref() != Some("docs")));
}

#[test]
fn an_extension_docs_section_without_the_anchor_is_kept() {
    let prompt = [
        default_preamble(),
        section("docs", "Project docs live in ./docs; read them on request."),
    ]
    .join("\n\n");
    let shaped = shape_anthropic_oauth_system_prompt(&prompt);
    assert_eq!(
        chunk_body(&shaped, "docs").as_deref(),
        Some("Project docs live in ./docs; read them on request.")
    );
}

#[test]
fn custom_tool_filler_paragraph_is_stripped_and_snippets_kept() {
    let tools = "read_file: Read a file.\n\nIn addition to the tools above, you may have access to custom tools.\n\nbash: Run a command.";
    let prompt = [default_preamble(), section("tools", tools)].join("\n\n");
    let shaped = shape_anthropic_oauth_system_prompt(&prompt);

    assert!(!shaped.contains("In addition to the tools above"));
    assert_eq!(
        chunk_body(&shaped, "tools").as_deref(),
        Some("read_file: Read a file.\n\nbash: Run a command.")
    );
}

#[test]
fn project_context_is_never_rewritten() {
    let body = format!(
        "{PI_DEFAULT_PROMPT_PREFIX}\n\n{ENVIRONMENT_PHRASE}\n\nIn addition to the tools above, this line stays."
    );
    let prompt = [
        default_preamble(),
        section("rules", &format!("{ENVIRONMENT_PHRASE}\n\nKeep it short.")),
        section("project_context", &body),
    ]
    .join("\n\n");
    let shaped = shape_anthropic_oauth_system_prompt(&prompt);

    assert_eq!(
        chunk_body(&shaped, "project_context").as_deref(),
        Some(body.as_str())
    );
    assert!(chunk_body(&shaped, "rules")
        .expect("rules section")
        .starts_with(ENVIRONMENT_REPLACEMENT));
}

#[test]
fn a_prompt_without_pi_sections_passes_through_untouched() {
    reset_shaping_warnings();
    let prompt = "You are a different agent.\n\n<custom>\nUntouched.\n</custom>\n";
    assert_eq!(shape_anthropic_oauth_system_prompt(prompt), prompt);
}

#[test]
fn already_shaped_prompts_keep_round_tripping() {
    let shaped = shape_anthropic_oauth_system_prompt(&full_prompt());
    assert_eq!(shape_anthropic_oauth_system_prompt(&shaped), shaped);
}

#[test]
fn system_blocks_without_the_pi_preamble_are_untouched() {
    let blocks = vec![
        json!({"type": "text", "text": "billing header"}),
        json!({"type": "text", "text": full_prompt()}),
    ];
    let shaped = rpi_anthropic_auth::prompt::shape_system_blocks(&blocks);

    assert_eq!(shaped[0], blocks[0]);
    let text = shaped[1]["text"].as_str().expect("shaped text");
    assert!(!text.contains(PI_DEFAULT_PROMPT_PREFIX));
    assert_eq!(shaped[1]["type"], "text");
}

#[test]
fn updated_tools_section_is_shaped_inside_its_frame() {
    let update = format!(
        "Updated system prompt section \"tools\":\n\n{}",
        section(
            "tools",
            "read_file: Read a file.\n\nIn addition to the tools above, filler."
        )
    );
    let shaped = shape_system_update_text(&update).expect("update survives");

    assert!(shaped.starts_with("Updated system prompt section \"tools\":\n\n<tools>\n"));
    assert!(!shaped.contains("In addition to the tools above"));
    assert!(shaped.contains("read_file: Read a file."));
}

#[test]
fn updated_preamble_is_replaced() {
    let update =
        format!("Updated system prompt section \"preamble\":\n\n{PI_DEFAULT_PROMPT_PREFIX}");
    assert_eq!(
        shape_system_update_text(&update).as_deref(),
        Some(
            format!(
                "Updated system prompt section \"preamble\":\n\n{MINIMAL_ANTHROPIC_OAUTH_PROMPT}"
            )
            .as_str()
        )
    );
}

#[test]
fn removed_docs_notice_is_dropped_with_its_section() {
    let update = "Updated system prompt section \"docs\":\n\n<docs>\nPi documentation (read only when the user asks about pi itself):\n\n- docs/x.md\n</docs>";
    assert_eq!(shape_system_update_text(update), None);

    let removal = "Removed system prompt section \"docs\".";
    assert_eq!(shape_system_update_text(removal), None);
}

#[test]
fn removed_other_section_notice_is_kept() {
    let removal = "Removed system prompt section \"rules\".";
    assert_eq!(shape_system_update_text(removal).as_deref(), Some(removal));
}

#[test]
fn unframed_system_text_is_returned_unchanged() {
    let text = "Context checkpoint: stay on task.";
    assert_eq!(shape_system_update_text(text).as_deref(), Some(text));
}

#[test]
fn framing_parts_keep_their_separation() {
    let text = [
        "Leading context.".to_string(),
        "Removed system prompt section \"skills\".".to_string(),
    ]
    .join("\n\n");
    assert_eq!(
        shape_system_update_text(&text).as_deref(),
        Some(text.as_str())
    );
}

fn oauth_payload(system: Value, messages: Value) -> Value {
    json!({
        "model": "claude-sonnet-4-5",
        "stream": true,
        "max_tokens": 1024,
        "system": system,
        "messages": messages,
    })
}

#[test]
fn access_token_family_gates_shaping() {
    assert!(is_anthropic_oauth_access_token("sk-ant-oat01-abc"));
    assert!(!is_anthropic_oauth_access_token("sk-ant-api03-abc"));
    assert!(!is_anthropic_oauth_access_token(""));
}

#[test]
fn api_key_requests_are_not_shaped() {
    let payload = oauth_payload(
        json!([{"type": "text", "text": full_prompt()}]),
        json!([{"role": "user", "content": "hi"}]),
    );
    let shaped = shape_anthropic_oauth_payload(payload.clone(), "sk-ant-api03-abc");
    assert_eq!(shaped, payload);
}

#[test]
fn oauth_requests_shape_system_blocks_and_drop_empty_updates() {
    let payload = oauth_payload(
        json!([
            {"type": "text", "text": "x-anthropic-billing-header: cc_version=2.0.0"},
            {"type": "text", "text": full_prompt()},
        ]),
        json!([
            {"role": "user", "content": "hi"},
            {
                "role": "system",
                "content": [{
                    "type": "text",
                    "text": "Updated system prompt section \"docs\":\n\n<docs>\nPi documentation (read only when the user asks about pi itself):\n\n- docs/x.md\n</docs>",
                }],
            },
            {"role": "user", "content": "again"},
        ]),
    );

    let shaped = shape_anthropic_oauth_payload(payload, "sk-ant-oat01-abc");
    let system = shaped["system"].as_array().expect("system blocks");

    assert_eq!(
        system[0]["text"],
        "x-anthropic-billing-header: cc_version=2.0.0"
    );
    assert!(!system[1]["text"]
        .as_str()
        .expect("prompt text")
        .contains(PI_DEFAULT_PROMPT_PREFIX));

    let messages = shaped["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert!(messages
        .iter()
        .all(|message| message["role"].as_str() != Some("system")));
}

#[test]
fn content_less_effort_updates_survive() {
    let payload = oauth_payload(
        Value::Null,
        json!([
            {"role": "user", "content": "hi"},
            {"role": "system", "content": [], "output_config": {"effort": "low"}},
        ]),
    );

    let shaped = shape_anthropic_oauth_payload(payload, "sk-ant-oat01-abc");
    let messages = shaped["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["content"], json!([]));
}

#[test]
fn non_text_blocks_keep_an_empty_update_alive() {
    let payload = oauth_payload(
        Value::Null,
        json!([
            {"role": "user", "content": "hi"},
            {
                "role": "system",
                "content": [
                    {"type": "text", "text": "Removed system prompt section \"docs\"."},
                    {"type": "tool_addition", "tool": {"name": "read_file"}},
                ],
            },
        ]),
    );

    let shaped = shape_anthropic_oauth_payload(payload, "sk-ant-oat01-abc");
    let messages = shaped["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[1]["content"],
        json!([{"type": "tool_addition", "tool": {"name": "read_file"}}])
    );
}

#[test]
fn payloads_that_are_not_anthropic_messages_are_untouched() {
    let payload = json!({"prompt": "not anthropic"});
    assert_eq!(
        shape_anthropic_oauth_payload(payload.clone(), "sk-ant-oat01-abc"),
        payload
    );
}

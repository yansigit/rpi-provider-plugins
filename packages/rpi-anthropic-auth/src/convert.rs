//! Pi transcript projection into Anthropic Messages payloads.
//!
//! The port reads Pi's messages and tools as JSON values and returns Anthropic
//! payload fragments as JSON values, so it can sit under the host's request
//! seam without this crate depending on the host SDK.
//!
//! Blocks Pi never emits are rejected instead of silently dropped, so upstream
//! drift shows up as an error rather than a quietly truncated transcript.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// Tool names Claude Code itself exposes.
///
/// Anthropic's OAuth classifier rejects tool definitions with names outside
/// this set with a 400 "Third-party apps now draw from your extra usage" error.
pub const CLAUDE_CODE_TOOL_NAMES: [&str; 10] = [
    "Read",
    "Write",
    "Edit",
    "Bash",
    "Grep",
    "Glob",
    "AskUserQuestion",
    "TodoWrite",
    "WebFetch",
    "WebSearch",
];

/// Claude Code exposes MCP tools as `mcp__<server>__<tool>`, so unknown Pi
/// tools are namespaced under `mcp__pi__`.
pub const CLAUDE_CODE_MCP_TOOL_PREFIX: &str = "mcp__pi__";

/// Environment variable that selects the prompt-cache retention when the
/// caller expresses no preference.
pub const PI_CACHE_RETENTION_ENV: &str = "PI_CACHE_RETENTION";

/// How long Anthropic should keep the request's prompt cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheRetention {
    /// The default five-minute ephemeral cache.
    Short,
    /// A one-hour TTL.
    Long,
    /// No cache breakpoint at all.
    None,
}

/// The identity a transcript is being converted for.
///
/// Thinking signatures are only replayed to the model that produced them.
pub struct ModelRef {
    pub provider: String,
    pub api: String,
    pub id: String,
}

/// Replaces unpaired UTF-16 surrogates with U+FFFD.
///
/// Pi sanitizes unpaired UTF-16 surrogates because JavaScript strings can hold
/// them; Rust strings hold scalar values only, and `serde_json` rejects a lone
/// surrogate escape while parsing (`unexpected end of hex escape`, `lone
/// leading surrogate in hex escape`), so nothing unpaired can reach this far.
/// The function stays as an explicit pass-through to keep call sites shaped
/// like the original port.
pub fn sanitize_surrogates(text: &str) -> &str {
    text
}

/// Resolves the cache retention to use.
///
/// An explicit request wins; otherwise `PI_CACHE_RETENTION=long` selects the
/// one-hour TTL and every other value keeps the default.
pub fn resolve_cache_retention(
    requested: Option<CacheRetention>,
    env_value: Option<&str>,
) -> CacheRetention {
    if let Some(retention) = requested {
        return retention;
    }
    if env_value == Some("long") {
        CacheRetention::Long
    } else {
        CacheRetention::Short
    }
}

/// [`resolve_cache_retention`] reading `PI_CACHE_RETENTION` from the process
/// environment.
pub fn resolve_cache_retention_from_env(requested: Option<CacheRetention>) -> CacheRetention {
    let env_value = std::env::var(PI_CACHE_RETENTION_ENV).ok();
    resolve_cache_retention(requested, env_value.as_deref())
}

/// The `cache_control` breakpoint for a retention, or `None` when caching is off.
pub fn cache_control_for(retention: CacheRetention) -> Option<Value> {
    match retention {
        CacheRetention::None => None,
        CacheRetention::Long => Some(json!({"type": "ephemeral", "ttl": "1h"})),
        CacheRetention::Short => Some(json!({"type": "ephemeral"})),
    }
}

/// Maps a Pi tool name to the name Claude Code would use.
pub fn to_claude_code_tool_name(name: &str) -> String {
    match claude_code_tool_name(name) {
        Some(known) => known.to_string(),
        None => format!(
            "{CLAUDE_CODE_MCP_TOOL_PREFIX}{}",
            sanitize_custom_tool_name(name)
        ),
    }
}

/// Maps a tool name from an Anthropic response back to Pi's own name.
///
/// `tools` is the tool list the request was built from; when it holds a tool
/// whose sanitized name matches, that spelling wins.
pub fn from_claude_code_tool_name(name: &str, tools: &[Value]) -> String {
    let tool_names = || {
        tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
    };

    if let Some(stripped) = name.strip_prefix(CLAUDE_CODE_MCP_TOOL_PREFIX) {
        return tool_names()
            .find(|candidate| sanitize_custom_tool_name(candidate) == stripped)
            .map(str::to_string)
            .unwrap_or_else(|| stripped.to_string());
    }

    tool_names()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

fn claude_code_tool_name(name: &str) -> Option<&'static str> {
    CLAUDE_CODE_TOOL_NAMES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(name))
}

/// Rewrites a tool name to the character set Anthropic accepts in tool names.
///
/// A non-ASCII character maps to one `_` per char; both directions use this
/// function, so such names still round-trip.
fn sanitize_custom_tool_name(name: &str) -> String {
    name.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// Converts Pi's messages into Anthropic's.
///
/// Anthropic requires every `tool_use` block to be immediately followed by a
/// user message holding a matching `tool_result`, so orphaned tool calls — from
/// aborted turns or partial histories — are patched with a synthetic
/// "No result provided" result rather than left to fail the next request.
///
/// `is_oauth` selects the Claude Code tool namespace; `cache_retention` decides
/// where, and whether, a prompt-cache breakpoint is attached.
pub fn convert_pi_messages_to_anthropic(
    messages: &[Value],
    is_oauth: bool,
    model: &ModelRef,
    cache_retention: CacheRetention,
) -> Result<Vec<Value>, String> {
    let mut params: Vec<Value> = Vec::new();
    let mut tool_ids = ToolIds::default();
    let mut pending_tool_use_ids: Vec<String> = Vec::new();
    let mut index = 0usize;

    while index < messages.len() {
        let message = &messages[index];
        match message["role"].as_str() {
            Some("user") => {
                flush_pending_tool_results(&mut params, &mut pending_tool_use_ids);
                convert_user_message(message, &mut params)?;
                index += 1;
            }
            Some("assistant") => {
                // Aborted or errored turns can hold partial tool calls that
                // never get a result, which would poison every later request.
                if matches!(
                    message["stopReason"].as_str(),
                    Some("aborted") | Some("error")
                ) {
                    index += 1;
                    continue;
                }
                // A run of assistant turns is not something Pi emits, but if it
                // happens the outstanding tool uses are patched first anyway.
                flush_pending_tool_results(&mut params, &mut pending_tool_use_ids);

                let (blocks, emitted) =
                    convert_assistant_message(message, is_oauth, model, &mut tool_ids)?;
                if !blocks.is_empty() {
                    params.push(json!({"role": "assistant", "content": blocks}));
                    pending_tool_use_ids = emitted;
                }
                index += 1;
            }
            Some("toolResult") => {
                let mut tool_results: Vec<Value> = Vec::new();
                let mut satisfied: HashSet<String> = HashSet::new();

                let mut next = index;
                while next < messages.len() && messages[next]["role"].as_str() == Some("toolResult")
                {
                    let result = &messages[next];
                    let tool_use_id = tool_ids.anthropic_id(tool_call_id(result)?);
                    let content = result.get("content").and_then(Value::as_array);
                    tool_results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": convert_tool_result_content(content.map(Vec::as_slice).unwrap_or_default())?,
                        // Anthropic defaults `is_error` to false, so writing the
                        // value Pi implies is equivalent to Pi omitting it.
                        "is_error": result["isError"].as_bool().unwrap_or(false),
                    }));
                    satisfied.insert(tool_use_id);
                    next += 1;
                }

                // Orphans must live in the same user message as the real
                // results, so every tool_use is followed by matching results.
                for id in &pending_tool_use_ids {
                    if !satisfied.contains(id) {
                        tool_results.push(synthetic_tool_result(id));
                    }
                }
                pending_tool_use_ids.clear();

                params.push(json!({"role": "user", "content": tool_results}));
                index = next;
            }
            other => {
                return Err(format!(
                    "Invalid Pi message: unsupported role {}",
                    describe(other)
                ))
            }
        }
    }

    // The loop flushes pending tool uses before the next message, so a history
    // ending on an unresolved tool call still needs one more pass.
    flush_pending_tool_results(&mut params, &mut pending_tool_use_ids);

    // Anthropic prompt caching needs an explicit breakpoint, and plain-text user
    // turns must be promoted to a block so there is somewhere to attach it.
    if let Some(cache_control) = cache_control_for(cache_retention) {
        if let Some(last) = params.last_mut() {
            attach_cache_control(last, cache_control);
        }
    }

    Ok(params)
}

/// Converts Pi's tool definitions into Anthropic's.
pub fn convert_pi_tools_to_anthropic(
    tools: &[Value],
    is_oauth: bool,
) -> Result<Vec<Value>, String> {
    tools
        .iter()
        .map(|tool| {
            let name = tool["name"]
                .as_str()
                .ok_or_else(|| "Invalid Pi tool: name must be a string".to_string())?;
            let mut definition = Map::new();
            definition.insert(
                "name".to_string(),
                Value::String(if is_oauth {
                    to_claude_code_tool_name(name)
                } else {
                    name.to_string()
                }),
            );
            if let Some(description) = tool.get("description") {
                definition.insert("description".to_string(), description.clone());
            }
            let parameters = tool.get("parameters").unwrap_or(&Value::Null);
            definition.insert(
                "input_schema".to_string(),
                json!({
                    "type": "object",
                    "properties": non_null(parameters.get("properties")).unwrap_or_else(|| json!({})),
                    "required": non_null(parameters.get("required")).unwrap_or_else(|| json!([])),
                }),
            );
            Ok(Value::Object(definition))
        })
        .collect()
}

/// Anthropic tool ids, sanitized to its character set and made unique.
#[derive(Default)]
struct ToolIds {
    mapped: HashMap<String, String>,
    used: HashSet<String>,
}

impl ToolIds {
    fn anthropic_id(&mut self, id: &str) -> String {
        if let Some(existing) = self.mapped.get(id) {
            return existing.clone();
        }

        let mut base = sanitize_tool_id(id);
        if base.is_empty() {
            base = "tool".to_string();
        }

        let mut candidate = base.clone();
        let mut suffix = 1;
        while self.used.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }

        self.used.insert(candidate.clone());
        self.mapped.insert(id.to_string(), candidate.clone());
        candidate
    }
}

fn sanitize_tool_id(id: &str) -> String {
    sanitize_surrogates(id)
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn tool_call_id(message: &Value) -> Result<&str, String> {
    message["toolCallId"]
        .as_str()
        .ok_or_else(|| "Invalid Pi message: toolCallId must be a string".to_string())
}

fn synthetic_tool_result(tool_use_id: &str) -> Value {
    json!({
        "type": "tool_result",
        "tool_use_id": tool_use_id,
        "content": "No result provided",
        "is_error": true,
    })
}

fn flush_pending_tool_results(params: &mut Vec<Value>, pending: &mut Vec<String>) {
    if pending.is_empty() {
        return;
    }
    let blocks: Vec<Value> = pending.iter().map(|id| synthetic_tool_result(id)).collect();
    params.push(json!({"role": "user", "content": blocks}));
    pending.clear();
}

fn convert_user_message(message: &Value, params: &mut Vec<Value>) -> Result<(), String> {
    match message.get("content") {
        Some(Value::String(text)) => {
            if !text.trim().is_empty() {
                params.push(json!({"role": "user", "content": sanitize_surrogates(text)}));
            }
            Ok(())
        }
        Some(Value::Array(blocks)) => {
            let mut converted: Vec<Value> = Vec::new();
            for block in blocks {
                match block["type"].as_str() {
                    // Extensions may emit custom messages with empty text (a
                    // display-only message whose data lives in `details`).
                    // Anthropic rejects empty text blocks, and a cache
                    // breakpoint on one fails with "cache_control cannot be set
                    // for empty text blocks".
                    Some("text") => {
                        let text = block["text"].as_str().unwrap_or_default();
                        if !text.trim().is_empty() {
                            converted
                                .push(json!({"type": "text", "text": sanitize_surrogates(text)}));
                        }
                    }
                    Some("image") => converted.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": block["mimeType"].clone(),
                            "data": block["data"].clone(),
                        },
                    })),
                    other => {
                        return Err(format!(
                            "Invalid Pi message: unsupported user content block {}",
                            describe(other)
                        ))
                    }
                }
            }
            if !converted.is_empty() {
                params.push(json!({"role": "user", "content": converted}));
            }
            Ok(())
        }
        _ => Err("Invalid Pi message: user.content must be a string or an array".to_string()),
    }
}

fn convert_assistant_message(
    message: &Value,
    is_oauth: bool,
    model: &ModelRef,
    tool_ids: &mut ToolIds,
) -> Result<(Vec<Value>, Vec<String>), String> {
    let blocks = message["content"]
        .as_array()
        .ok_or_else(|| "Invalid Pi message: assistant.content must be an array".to_string())?;
    let replayed = message["provider"] == model.provider
        && message["api"] == model.api
        && message["model"] == model.id;

    let mut converted: Vec<Value> = Vec::new();
    let mut emitted_tool_use_ids: Vec<String> = Vec::new();

    for block in blocks {
        match block["type"].as_str() {
            Some("text") => {
                let text = block["text"].as_str().unwrap_or_default();
                if !text.trim().is_empty() {
                    converted.push(json!({"type": "text", "text": sanitize_surrogates(text)}));
                }
            }
            Some("thinking") => {
                let signature = block["thinkingSignature"]
                    .as_str()
                    .map(trim_signature)
                    .filter(|signature| !signature.is_empty());
                let still_thought = block["thinking"]
                    .as_str()
                    .filter(|thinking| !thinking.trim().is_empty());

                if block["redacted"].as_bool() == Some(true) {
                    if let (true, Some(signature)) = (replayed, signature) {
                        converted.push(json!({"type": "redacted_thinking", "data": signature}));
                    }
                    continue;
                }

                match (replayed, signature) {
                    (true, Some(signature)) => converted.push(json!({
                        "type": "thinking",
                        "thinking": sanitize_surrogates(block["thinking"].as_str().unwrap_or_default()),
                        "signature": signature,
                    })),
                    _ => {
                        if let Some(thinking) = still_thought {
                            converted
                                .push(json!({"type": "text", "text": sanitize_surrogates(thinking)}));
                        }
                    }
                }
            }
            Some("toolCall") => {
                let id = block["id"].as_str().ok_or_else(|| {
                    "Invalid Pi message: toolCall.id must be a string".to_string()
                })?;
                let name = block["name"].as_str().ok_or_else(|| {
                    "Invalid Pi message: toolCall.name must be a string".to_string()
                })?;
                let anthropic_id = tool_ids.anthropic_id(id);
                converted.push(json!({
                    "type": "tool_use",
                    "id": anthropic_id,
                    "name": if is_oauth { to_claude_code_tool_name(name) } else { name.to_string() },
                    "input": non_null(block.get("arguments")).unwrap_or_else(|| json!({})),
                }));
                emitted_tool_use_ids.push(anthropic_id);
            }
            other => {
                return Err(format!(
                    "Invalid Pi message: unsupported assistant content block {}",
                    describe(other)
                ))
            }
        }
    }

    Ok((converted, emitted_tool_use_ids))
}

/// A thinking signature, trimmed, as Pi stores it with surrounding whitespace.
fn trim_signature(signature: &str) -> &str {
    signature.trim()
}

fn convert_tool_result_content(content: &[Value]) -> Result<Value, String> {
    let has_images = content
        .iter()
        .any(|block| block["type"].as_str() == Some("image"));
    if !has_images {
        let text = content
            .iter()
            .filter(|block| block["type"].as_str() == Some("text"))
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(Value::String(sanitize_surrogates(&text).to_string()));
    }

    let mut blocks: Vec<Value> = Vec::with_capacity(content.len() + 1);
    for block in content {
        match block["type"].as_str() {
            Some("text") => blocks.push(json!({
                "type": "text",
                "text": sanitize_surrogates(block["text"].as_str().unwrap_or_default()),
            })),
            Some("image") => blocks.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": block["mimeType"].clone(),
                    "data": block["data"].clone(),
                },
            })),
            other => {
                return Err(format!(
                    "Invalid Pi message: unsupported tool result content block {}",
                    describe(other)
                ))
            }
        }
    }

    if !blocks
        .iter()
        .any(|block| block["type"].as_str() == Some("text"))
    {
        blocks.insert(0, json!({"type": "text", "text": "(see attached image)"}));
    }

    Ok(Value::Array(blocks))
}

/// Attaches a cache breakpoint to a user parameter's last block.
fn attach_cache_control(param: &mut Value, cache_control: Value) {
    if param["role"].as_str() != Some("user") {
        return;
    }

    match param.get("content").cloned().unwrap_or(Value::Null) {
        Value::String(text) if !text.is_empty() => {
            param["content"] =
                json!([{"type": "text", "text": text, "cache_control": cache_control}]);
        }
        Value::Array(mut blocks) if !blocks.is_empty() => {
            if let Some(block) = blocks.last_mut() {
                block["cache_control"] = cache_control;
            }
            param["content"] = Value::Array(blocks);
        }
        _ => {}
    }
}

fn non_null(value: Option<&Value>) -> Option<Value> {
    value.filter(|value| !value.is_null()).cloned()
}

fn describe(value: Option<&str>) -> String {
    match value {
        Some(value) => format!("\"{value}\""),
        None => "with no type".to_string(),
    }
}

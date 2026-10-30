//! Assistant-message state for one streamed Claude response.
//!
//! Anthropic reports cache reads and cache writes as separate token counters, so
//! the message Pi receives carries them as such: the host bills every turn from
//! these numbers, and collapsing them into `input` would overstate the price.
use crate::convert;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::future::Future;

/// Where the transport hands each assistant-message event to the host.
pub trait EventSink {
    fn emit(&mut self, event: Value) -> impl Future<Output = Result<(), String>> + Send;
}

/// Anthropic stop reasons collapse onto Pi's four terminal reasons; anything
/// else is a provider contract break the caller must surface.
pub fn map_stop_reason(reason: &str) -> &'static str {
    match reason {
        "end_turn" | "pause_turn" | "stop_sequence" => "stop",
        "max_tokens" => "length",
        "tool_use" => "toolUse",
        _ => "error",
    }
}

/// The message skeleton Pi streams into, plus the Anthropic index bookkeeping
/// needed to route deltas: content positions are local, indexes are the
/// provider's.
pub struct State {
    pub output: Value,
    model: Value,
    tools: Vec<Value>,
    oauth: bool,
    positions: BTreeMap<i64, usize>,
    /// Tool argument JSON arrives as fragments; `arguments` only holds what has
    /// parsed so far and is replaced wholesale when the block closes.
    partial_json: BTreeMap<i64, String>,
}

impl State {
    pub fn new(model: &Value, timestamp: i64) -> Self {
        Self {
            output: json!({
                "role": "assistant",
                "content": [],
                "api": model["api"],
                "provider": model["provider"],
                "model": model["id"],
                "usage": {
                    "input": 0,
                    "output": 0,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                    "totalTokens": 0,
                    "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0},
                },
                "stopReason": "stop",
                "timestamp": timestamp,
            }),
            model: model.clone(),
            tools: Vec::new(),
            oauth: false,
            positions: BTreeMap::new(),
            partial_json: BTreeMap::new(),
        }
    }

    /// OAuth requests see Claude Code tool names; the message must carry Pi's.
    pub fn name_tools(&mut self, oauth: bool, tools: &[Value]) {
        self.oauth = oauth;
        self.tools = tools.to_vec();
    }

    pub async fn start(&mut self, sink: &mut impl EventSink) -> Result<(), String> {
        sink.emit(json!({"type": "start", "partial": self.output}))
            .await
    }

    /// `message_start` reports the prompt side, `message_delta` the completion
    /// side; both land here so a partial usage report is never overwritten.
    pub fn apply_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage.and_then(Value::as_object) else {
            return;
        };
        let output = &mut self.output["usage"];
        for (wire, field) in [
            ("input_tokens", "input"),
            ("output_tokens", "output"),
            ("cache_read_input_tokens", "cacheRead"),
            ("cache_creation_input_tokens", "cacheWrite"),
        ] {
            if let Some(value) = usage.get(wire).and_then(Value::as_f64) {
                output[field] = value.into();
            }
        }
        if let Some(thinking) = usage
            .get("output_tokens_details")
            .and_then(|details| details.get("thinking_tokens"))
            .and_then(Value::as_f64)
        {
            output["reasoning"] = thinking.into();
        }
        let total: f64 = ["input", "output", "cacheRead", "cacheWrite"]
            .iter()
            .map(|field| output[*field].as_f64().unwrap_or(0.0))
            .sum();
        output["totalTokens"] = total.into();
        calculate_cost(&self.model, output);
    }

    pub async fn block_start(
        &mut self,
        index: i64,
        block: &Value,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        let position = self.output["content"]
            .as_array()
            .map_or(0, std::vec::Vec::len);
        let kind = block["type"].as_str().unwrap_or_default();
        let event = match kind {
            "text" => {
                self.push(json!({"type": "text", "text": ""}))?;
                json!({"type": "text_start", "contentIndex": position, "partial": self.output})
            }
            "thinking" => {
                self.push(json!({"type": "thinking", "thinking": "", "thinkingSignature": ""}))?;
                json!({"type": "thinking_start", "contentIndex": position, "partial": self.output})
            }
            "redacted_thinking" => {
                self.push(json!({
                    "type": "thinking",
                    "thinking": "[Reasoning redacted]",
                    "thinkingSignature": block["data"],
                    "redacted": true,
                }))?;
                json!({"type": "thinking_start", "contentIndex": position, "partial": self.output})
            }
            "tool_use" => {
                let name = block["name"]
                    .as_str()
                    .ok_or_else(|| "Anthropic tool_use block has no name".to_string())?;
                let name = if self.oauth {
                    convert::from_claude_code_tool_name(name, &self.tools)
                } else {
                    name.to_owned()
                };
                self.push(json!({
                    "type": "toolCall",
                    "id": block["id"],
                    "name": name,
                    "arguments": {},
                }))?;
                self.partial_json.insert(index, String::new());
                json!({
                    "type": "toolcall_start",
                    "contentIndex": position,
                    "id": block["id"],
                    "name": name,
                    "partial": self.output,
                })
            }
            other => {
                return Err(format!(
                    "Unsupported Anthropic content block type: {other:?}"
                ));
            }
        };
        self.positions.insert(index, position);
        sink.emit(event).await
    }

    pub async fn block_delta(
        &mut self,
        index: i64,
        delta: &Value,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        let position = self.position(index)?;
        let kind = delta["type"].as_str().unwrap_or_default();
        match kind {
            "text_delta" => {
                let text = delta["text"].as_str().unwrap_or_default();
                let block = &mut self.output["content"][position];
                let joined = format!("{}{}", block["text"].as_str().unwrap_or_default(), text);
                block["text"] = joined.into();
                let partial = self.output.clone();
                sink.emit(json!({
                    "type": "text_delta",
                    "contentIndex": position,
                    "delta": text,
                    "partial": partial,
                }))
                .await
            }
            "thinking_delta" => {
                let thinking = delta["thinking"].as_str().unwrap_or_default();
                let block = &mut self.output["content"][position];
                let joined = format!(
                    "{}{}",
                    block["thinking"].as_str().unwrap_or_default(),
                    thinking
                );
                block["thinking"] = joined.into();
                let partial = self.output.clone();
                sink.emit(json!({
                    "type": "thinking_delta",
                    "contentIndex": position,
                    "delta": thinking,
                    "partial": partial,
                }))
                .await
            }
            // The signature trails the thinking text; Pi only needs it on the block.
            "signature_delta" => {
                let signature = delta["signature"].as_str().unwrap_or_default();
                let block = &mut self.output["content"][position];
                let joined = format!(
                    "{}{}",
                    block["thinkingSignature"].as_str().unwrap_or_default(),
                    signature
                );
                block["thinkingSignature"] = joined.into();
                Ok(())
            }
            "input_json_delta" => {
                let fragment = delta["partial_json"].as_str().unwrap_or_default();
                let accumulated = self.partial_json.entry(index).or_default();
                accumulated.push_str(fragment);
                let arguments = serde_json::from_str::<Value>(accumulated)
                    .ok()
                    .filter(Value::is_object);
                if let Some(arguments) = arguments {
                    self.output["content"][position]["arguments"] = arguments;
                }
                let partial = self.output.clone();
                sink.emit(json!({
                    "type": "toolcall_delta",
                    "contentIndex": position,
                    "delta": fragment,
                    "partial": partial,
                }))
                .await
            }
            other => Err(format!(
                "Unsupported Anthropic content delta type: {other:?}"
            )),
        }
    }

    pub async fn block_stop(
        &mut self,
        index: i64,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        let position = self.position(index)?;
        self.positions.remove(&index);
        let partial_json = self.partial_json.remove(&index);
        let block = self.output["content"][position].clone();
        match block["type"].as_str() {
            Some("text") => {
                sink.emit(json!({
                    "type": "text_end",
                    "contentIndex": position,
                    "content": block["text"],
                    "partial": self.output,
                }))
                .await
            }
            Some("thinking") => {
                sink.emit(json!({
                    "type": "thinking_end",
                    "contentIndex": position,
                    "content": block["thinking"],
                    "partial": self.output,
                }))
                .await
            }
            Some("toolCall") => {
                if let Some(partial_json) = partial_json {
                    if let Ok(Value::Object(arguments)) = serde_json::from_str(&partial_json) {
                        self.output["content"][position]["arguments"] = Value::Object(arguments);
                    }
                }
                let tool_call = self.output["content"][position].clone();
                sink.emit(json!({
                    "type": "toolcall_end",
                    "contentIndex": position,
                    "toolCall": tool_call,
                    "partial": self.output,
                }))
                .await
            }
            _ => Ok(()),
        }
    }

    /// `message_delta` is the only event that names the stop reason.
    pub fn message_delta(&mut self, delta: &Value, usage: Option<&Value>) {
        if let Some(reason) = delta["stop_reason"].as_str() {
            let mapped = map_stop_reason(reason);
            self.output["stopReason"] = mapped.into();
            if mapped == "error" {
                let explanation = delta["stop_details"]["explanation"]
                    .as_str()
                    .map(|text| format!(" — {text}"))
                    .unwrap_or_default();
                self.output["errorMessage"] =
                    format!("Provider stop reason: {reason}{explanation}").into();
            }
        }
        self.apply_usage(usage);
    }

    pub async fn finish(
        &mut self,
        duration_ms: u64,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        self.output["durationMs"] = duration_ms.into();
        if self.output["stopReason"] == "error" {
            let message = &mut self.output;
            if message.get("errorMessage").is_none() {
                message["errorMessage"] = "Unrecognized stop reason from the API.".into();
            }
            return sink
                .emit(json!({"type": "error", "reason": "error", "error": self.output}))
                .await;
        }
        sink.emit(
            json!({"type": "done", "reason": self.output["stopReason"], "message": self.output}),
        )
        .await
    }

    pub async fn fail(
        &mut self,
        error: &str,
        cancelled: bool,
        duration_ms: u64,
        sink: &mut impl EventSink,
    ) -> Result<(), String> {
        self.output["stopReason"] = if cancelled { "aborted" } else { "error" }.into();
        self.output["errorMessage"] = error.into();
        self.output["durationMs"] = duration_ms.into();
        let reason = self.output["stopReason"].clone();
        sink.emit(json!({"type": "error", "reason": reason, "error": self.output}))
            .await
    }

    fn push(&mut self, block: Value) -> Result<(), String> {
        self.output["content"]
            .as_array_mut()
            .ok_or_else(|| "Assistant content is not an array".to_string())?
            .push(block);
        Ok(())
    }

    fn position(&self, index: i64) -> Result<usize, String> {
        self.positions.get(&index).copied().ok_or_else(|| {
            format!("Anthropic stream referenced content index {index} before it began")
        })
    }
}

/// Cost is computed from the model's per-million-token rates so the host can
/// report spend without knowing this provider's billing table.
fn calculate_cost(model: &Value, usage: &mut Value) {
    let rate = |field: &str| model["cost"][field].as_f64().unwrap_or(0.0) / 1_000_000.0;
    let tokens = |field: &str| usage[field].as_f64().unwrap_or(0.0);
    let input = tokens("input") * rate("input");
    let output = tokens("output") * rate("output");
    let cache_read = tokens("cacheRead") * rate("cacheRead");
    let cache_write = tokens("cacheWrite") * rate("cacheWrite");
    usage["cost"] = json!({
        "input": input,
        "output": output,
        "cacheRead": cache_read,
        "cacheWrite": cache_write,
        "total": input + output + cache_read + cache_write,
    });
}

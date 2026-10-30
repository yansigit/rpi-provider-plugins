//! Live probe: drives the crate's own `transport::prepare` + `Http` source
//! against the real Anthropic API. Reads the credential file at runtime and
//! never prints the secret, headers, or request body — only status metadata,
//! usage, and response text.
use rpi_anthropic_auth::billing::VersionSettings;
use rpi_anthropic_auth::stream::EventSink;
use rpi_anthropic_auth::transport::{self, Http};
use rpi_anthropic_auth::version::LearnedFloor;
use serde_json::{json, Value};
use std::collections::HashMap;
use tokio_util::sync::CancellationToken;

struct Counter {
    events: usize,
}

impl EventSink for Counter {
    async fn emit(&mut self, _event: Value) -> Result<(), String> {
        self.events += 1;
        Ok(())
    }
}

fn load_key(path: &str) -> Result<String, String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read credential file: {e}"))?;
    let v: Value =
        serde_json::from_str(&raw).map_err(|_| "credential file is not JSON".to_string())?;
    let key = v
        .get("claude-code")
        .and_then(|n| n.get("key"))
        .or_else(|| v.get("key"))
        .and_then(|k| k.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    if key.is_empty() {
        return Err("credential file has no key".to_string());
    }
    Ok(key)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: live_probe <credential-file> <model-id>");
        std::process::exit(2);
    }
    let key = match load_key(&args[1]) {
        Ok(k) => {
            println!("credential key_len={}", k.len(), );
            k
        }
        Err(e) => {
            println!("CREDENTIAL ERROR {e}");
            std::process::exit(1);
        }
    };
    let model_id = args[2].clone();
    let model = json!({
        "id": model_id,
        "name": model_id,
        "provider": "anthropic",
        "api": "anthropic-messages",
        "reasoning": true,
        "maxTokens": 8000,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
    });
    let context = json!({
        "systemPrompt": "You are Pi.",
        "messages": [{"role": "user", "content": "Reply with exactly: ok"}],
        "tools": [],
    });
    let options = json!({"apiKey": key});
    let settings = VersionSettings {
        pinned: "2.1.280".to_string(),
        has_override: false,
    };
    let prepared = match transport::prepare(
        &model,
        &context,
        &options,
        &HashMap::new(),
        &settings,
        &LearnedFloor::default(),
    ) {
        Ok(p) => p,
        Err(e) => {
            println!("PREPARE FAILED {e}");
            std::process::exit(1);
        }
    };
    println!(
        "prepared oauth={} url_host={} body_len={}",
        prepared.oauth,
        prepared.url,
        prepared.body.len()
    );
    let source = match Http::new() {
        Ok(h) => h,
        Err(e) => {
            println!("HTTP BUILD FAILED {e}");
            std::process::exit(1);
        }
    };
    let caller = CancellationToken::new();
    let mut sink = Counter { events: 0 };
    let output = transport::run(&source, &prepared, caller, &mut sink).await;
    // Never print headers/body/credential — only the assistant message shape.
    let texts: Vec<String> = output
        .get("content")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str()).map(str::to_owned)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    println!(
        "model={} stop={} events={}",
        output.get("model").and_then(|m| m.as_str()).unwrap_or("?"),
        output.get("stopReason").and_then(|s| s.as_str()).unwrap_or("?"),
        sink.events
    );
    println!("usage={}", output.get("usage").unwrap_or(&Value::Null));
    let joined = texts.join("");
    println!("text={}", joined.chars().take(200).collect::<String>());
    if output.get("error").is_some() {
        println!("error={}", output.get("error").unwrap_or(&Value::Null));
        std::process::exit(1);
    }
}

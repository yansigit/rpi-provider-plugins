//! Enrollment state decoding and authority rules, before any custody request.
use super::{client::safe_nonnegative, errors::Error, policy::ConsumerError};
use serde_json::{json, Value};

fn invalid_state() -> ConsumerError {
    ConsumerError::new("invalid-state", "invalid Claustrum enrollment state")
}
pub fn valid_token(value: &Value) -> bool {
    let token = crate::json::string(value);
    token.len() == 64
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn generation(value: &Value) -> bool {
    safe_nonnegative(value) && value.as_f64().unwrap() >= 1.0
}
fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}
fn optional_nonempty(value: &Value, key: &str) -> bool {
    value.get(key).map_or(true, nonempty)
}
fn pick(value: &Value, keys: &[&str]) -> Value {
    let mut out = json!({});
    for key in keys {
        if let Some(value) = value.get(key) {
            out[*key] = value.clone();
        }
    }
    out
}
/// Return only fields accepted by the original schema; never render parser errors.
pub fn decode_state(value: &Value) -> Result<Value, ConsumerError> {
    if !value.is_object()
        || value["version"].as_f64() != Some(1.0)
        || !nonempty(&value["proposedName"])
    {
        return Err(invalid_state());
    }
    let keys: &[&str] = match value["phase"].as_str() {
        Some("pending")
            if valid_token(&value["requestSecret"])
                && optional_nonempty(value, "requestId")
                && safe_nonnegative(&value["createdAt"])
                && safe_nonnegative(&value["updatedAt"]) =>
        {
            &[
                "version",
                "phase",
                "proposedName",
                "requestSecret",
                "requestId",
                "createdAt",
                "updatedAt",
            ]
        }
        Some("approved")
            if optional_nonempty(value, "approvedName")
                && generation(&value["tokenGeneration"])
                && safe_nonnegative(&value["updatedAt"]) =>
        {
            &[
                "version",
                "phase",
                "proposedName",
                "approvedName",
                "tokenGeneration",
                "updatedAt",
            ]
        }
        Some("denied" | "blocked")
            if value.get("errorCode").map_or(true, Value::is_string)
                && safe_nonnegative(&value["updatedAt"]) =>
        {
            &["version", "phase", "proposedName", "errorCode", "updatedAt"]
        }
        _ => return Err(invalid_state()),
    };
    Ok(pick(value, keys))
}
pub fn decode_token(value: &Value) -> Result<Value, ConsumerError> {
    if !value.is_object()
        || !valid_token(&value["token"])
        || !generation(&value["token_generation"])
    {
        return Err(ConsumerError::new(
            "invalid-token",
            "invalid Claustrum enrollment token file",
        ));
    }
    Ok(pick(value, &["token", "token_generation"]))
}
pub fn status_from_state(state: &Value) -> Value {
    let mut result = json!({"state":state["phase"],"proposedName":state["proposedName"]});
    match state["phase"].as_str().unwrap() {
        "pending" => {
            if let Some(id) = state.get("requestId") {
                result["requestId"] = id.clone();
            }
        }
        "approved" => {
            if let Some(name) = state.get("approvedName") {
                result["approvedName"] = name.clone();
            }
            result["tokenGeneration"] = state["tokenGeneration"].clone();
        }
        "blocked" => {
            result["code"] = state.get("errorCode").cloned().unwrap_or(json!("unknown"));
        }
        _ => {}
    }
    result
}
pub fn status(
    token: Option<&Value>,
    state: Option<&Value>,
    proposed_name: &str,
) -> Result<Value, ConsumerError> {
    let token = token.map(decode_token).transpose()?;
    let state = state.map(decode_state).transpose()?;
    if let Some(token) = token {
        let mut result = json!({"state":"approved","proposedName":state.as_ref().map(|s|s["proposedName"].clone()).unwrap_or(json!(proposed_name)),"tokenGeneration":token["token_generation"]});
        if let Some(state) = &state {
            if state["phase"] == "approved" {
                if let Some(name) = state.get("approvedName") {
                    result["approvedName"] = name.clone();
                }
            }
        }
        return Ok(result);
    }
    match state {
        None => Ok(json!({"state":"idle"})),
        Some(state) if state["phase"] == "approved" => Ok(
            json!({"state":"blocked","proposedName":state["proposedName"],"code":"missing_token"}),
        ),
        Some(state) => Ok(status_from_state(&state)),
    }
}
/// Invalid tokens cannot turn an approved ceremony back into a local login.
pub fn authority(
    token: Option<&Value>,
    state: Option<&Value>,
    proposed_name: &str,
) -> &'static str {
    match status(token, state, proposed_name) {
        Err(_) => {
            if state
                .and_then(|s| decode_state(s).ok())
                .is_some_and(|s| s["phase"] == "approved" && s["proposedName"] == proposed_name)
            {
                "vault"
            } else {
                "unreadable"
            }
        }
        Ok(status) => {
            if status
                .get("proposedName")
                .is_some_and(|n| n != proposed_name)
            {
                "unreadable"
            } else if status["state"] == "approved"
                || (status["state"] == "blocked" && status["code"] == "missing_token")
            {
                "vault"
            } else {
                "local"
            }
        }
    }
}
pub fn retryable(error: &Error) -> bool {
    match error.code.as_str() {
        "invalid_params" | "pending_exists" | "not_found" | "already_consumed" | "superseded"
        | "stale_generation" => false,
        "pending_queue_full" => true,
        _ => error.action == "retry",
    }
}
pub fn name(plugin: &str, host: &str) -> Result<String, ConsumerError> {
    let valid = |part: &str| {
        part.bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    if !valid(plugin) || !valid(host) {
        return Err(ConsumerError::new(
            "invalid-state",
            "Claustrum enrollment names use lowercase letters, digits and dashes",
        ));
    }
    Ok(format!("{plugin}-{host}"))
}

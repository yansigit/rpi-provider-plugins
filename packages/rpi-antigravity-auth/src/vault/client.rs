//! Enrolled-consumer wire policy, matching claustrum-client 0.12.0.
use super::errors::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Operation {
    Get,
    List,
    Propose,
    Poll,
    Rotate,
    Report,
}

/// Serialize only supplied optional fields, preserving the original wire keys and order.
pub fn request(operation: Operation, input: &Value) -> Value {
    let (method, fields): (&str, &[(&str, &str)]) = match operation {
        Operation::Get => (
            "credential.get_scoped",
            &[
                ("credential_id", "credentialId"),
                ("enrollment_token", "enrollmentToken"),
                ("min_ttl_ms", "minTtlMs"),
            ],
        ),
        Operation::List => (
            "credential.list_scoped",
            &[("enrollment_token", "enrollmentToken")],
        ),
        Operation::Propose => (
            "auth.enroll_propose",
            &[
                ("proposed_name", "name"),
                ("request_secret_hash", "requestSecretHash"),
            ],
        ),
        Operation::Poll => (
            "auth.enroll_poll",
            &[
                ("request_id", "requestId"),
                ("request_secret", "requestSecret"),
            ],
        ),
        Operation::Rotate => (
            "auth.enroll_rotate",
            &[
                ("token", "token"),
                ("expected_token_generation", "expectedTokenGeneration"),
            ],
        ),
        Operation::Report => (
            "credential.report_auth_failure",
            &[
                ("credential_id", "credentialId"),
                ("enrollment_token", "enrollmentToken"),
                ("provider_status", "providerStatus"),
                ("record_version", "recordVersion"),
                ("reporter_source", "reporterSource"),
            ],
        ),
    };
    let params: Map<String, Value> = fields
        .iter()
        .filter_map(|(wire, key)| input.get(key).map(|v| ((*wire).into(), v.clone())))
        .collect();
    json!({"method":method,"params":params})
}
fn integer(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|v| v.is_finite() && v.fract() == 0.0)
}
pub(crate) fn safe_nonnegative(value: &Value) -> bool {
    integer(value)
        && value
            .as_f64()
            .is_some_and(|v| (0.0..=9_007_199_254_740_991.0).contains(&v))
}
fn optional_string(object: &Value, field: &str) -> bool {
    object.get(field).map_or(true, Value::is_string)
}
fn optional_number(object: &Value, field: &str) -> bool {
    object.get(field).map_or(true, |v| {
        v.is_null() || v.as_f64().is_some_and(f64::is_finite)
    })
}
fn nonempty_string(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !s.is_empty())
}
fn strings(value: &Value) -> bool {
    value
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_string))
}
fn copy(output: &mut Value, input: &Value, fields: &[(&str, &str)]) {
    for (to, from) in fields {
        if let Some(value) = input.get(from) {
            output[*to] = value.clone();
        }
    }
}

pub fn decode(operation: Operation, response: &Value) -> Result<Value, Error> {
    let error = |code| Error::from_reply(response, code);
    let result = response
        .get("result")
        .filter(|r| r.is_object())
        .ok_or_else(|| error("invalid_response"))?;
    if result["error"].is_object() {
        return Err(error("invalid_response"));
    }
    match operation {
        Operation::Get => {
            let payload = result["payload"]
                .as_array()
                .filter(|bytes| {
                    !bytes.is_empty()
                        && bytes.iter().all(|b| {
                            integer(b) && b.as_f64().is_some_and(|b| (0.0..=255.0).contains(&b))
                        })
                })
                .ok_or_else(|| error("invalid_response"))?;
            if !safe_nonnegative(&result["record_version"]) {
                return Err(error("invalid_record_version"));
            }
            if !optional_number(result, "expires_at_ms") {
                return Err(error("invalid_expiry"));
            }
            if [
                "credential_id",
                "project_id",
                "account_id",
                "email",
                "org_name",
            ]
            .iter()
            .any(|k| !optional_string(result, k))
            {
                return Err(error("invalid_response"));
            }
            // The supervised decoder accepts wider wire shapes; validate the enrolled contract first.
            let bytes: Vec<u8> = payload.iter().map(|b| b.as_f64().unwrap() as u8).collect();
            let decoded = cortexkit_claustrum_client::decode_credential_reply(
                json!({"result":{"payload":bytes,"record_version":result["record_version"]}}),
            )
            .map_err(|_| error("invalid_response"))?;
            let material =
                std::str::from_utf8(decoded.expose()).map_err(|_| error("invalid_response"))?;
            let material = material.strip_prefix('\u{feff}').unwrap_or(material);
            let mut out = json!({"material":material,"recordVersion":result["record_version"],"expiresAtMs":result.get("expires_at_ms").unwrap_or(&Value::Null)});
            copy(
                &mut out,
                result,
                &[
                    ("credentialId", "credential_id"),
                    ("projectId", "project_id"),
                    ("accountId", "account_id"),
                    ("email", "email"),
                    ("orgName", "org_name"),
                ],
            );
            Ok(out)
        }
        Operation::List => decode_inventory(response, result),
        Operation::Propose => {
            if !nonempty_string(&result["request_id"]) {
                return Err(error("invalid_response"));
            }
            Ok(json!({"requestId":result["request_id"]}))
        }
        Operation::Poll => {
            let status = result["status"].as_str();
            if matches!(status, Some("pending" | "denied")) {
                return Ok(json!({"status":status}));
            }
            if status != Some("approved")
                || !result["name"].is_string()
                || !nonempty_string(&result["token"])
                || !integer(&result["token_generation"])
            {
                return Err(error("invalid_response"));
            }
            Ok(
                json!({"status":"approved","name":result["name"],"token":result["token"],"tokenGeneration":result["token_generation"]}),
            )
        }
        Operation::Rotate => {
            if !nonempty_string(&result["token"])
                || !integer(&result["token_generation"])
                || result["token_generation"].as_f64().unwrap() < 0.0
            {
                return Err(error("invalid_response"));
            }
            Ok(json!({"token":result["token"],"tokenGeneration":result["token_generation"]}))
        }
        Operation::Report => {
            if result["accepted"] != true {
                return Err(error("invalid_response"));
            }
            Ok(Value::Null)
        }
    }
}
fn decode_inventory(response: &Value, result: &Value) -> Result<Value, Error> {
    let error = |code| Error::from_reply(response, code);
    let rows = result["credentials"]
        .as_array()
        .ok_or_else(|| error("invalid_response"))?;
    if !nonempty_string(&result["view"]) {
        return Err(error("invalid_response"));
    }
    let mut decoded = Vec::with_capacity(rows.len());
    for row in rows {
        if !row.is_object() {
            return Err(error("invalid_response"));
        }
        if !safe_nonnegative(&row["record_version"]) {
            return Err(error("invalid_record_version"));
        }
        if ["id", "type", "state"].iter().any(|k| !row[*k].is_string())
            || !optional_number(row, "created_at_ms")
        {
            return Err(error("invalid_response"));
        }
        if let Some(tier) = row.get("plan_tier") {
            let valid = tier.as_str().is_some_and(|s| {
                !s.is_empty()
                    && s.len() <= 32
                    && s.as_bytes()[0].is_ascii_lowercase()
                    && s.bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            });
            if !valid {
                return Err(error("invalid_response"));
            }
        }
        if row
            .get("plan_tier_source")
            .is_some_and(|v| !matches!(v.as_str(), Some("operator" | "detected")))
            || row.get("auth_method").is_some_and(|v| {
                !matches!(
                    v.as_str(),
                    Some("apikey" | "chatgpt" | "antigravity" | "oauth")
                )
            })
        {
            return Err(error("invalid_response"));
        }
        if ["refresh_adapter", "account_id", "email", "org_name"]
            .iter()
            .any(|k| !optional_string(row, k))
        {
            return Err(error("invalid_response"));
        }
        if ["categories", "serves", "operations"]
            .iter()
            .any(|k| !strings(&row[*k]))
            || row.get("provider_ids").is_some_and(|v| !strings(v))
        {
            return Err(error("invalid_response"));
        }
        let mut out = json!({"id":row["id"],"categories":row["categories"],"credentialType":row["type"],"serves":row["serves"],"providerIds":row.get("provider_ids").cloned().unwrap_or_else(|| json!([]))});
        copy(
            &mut out,
            row,
            &[
                ("authMethod", "auth_method"),
                ("planTier", "plan_tier"),
                ("planTierSource", "plan_tier_source"),
                ("state", "state"),
                ("refreshAdapter", "refresh_adapter"),
                ("recordVersion", "record_version"),
                ("operations", "operations"),
            ],
        );
        out["createdAtMs"] = row.get("created_at_ms").cloned().unwrap_or(Value::Null);
        copy(
            &mut out,
            row,
            &[
                ("accountId", "account_id"),
                ("email", "email"),
                ("orgName", "org_name"),
            ],
        );
        decoded.push(out);
    }
    let mut out = json!({"rows":decoded,"view":result["view"]});
    if let Some(grants) = result.get("grants") {
        if !safe_nonnegative(grants) {
            return Err(error("invalid_response"));
        }
        out["grants"] = grants.clone();
    }
    if let Some(tuples) = result.get("grant_tuples") {
        let tuples = tuples.as_array().ok_or_else(|| error("invalid_response"))?;
        let mut decoded = Vec::with_capacity(tuples.len());
        for tuple in tuples {
            if tuple.as_object().map_or(true, |o| o.len() != 3)
                || ["selector_kind", "selector", "operation"]
                    .iter()
                    .any(|k| !tuple[*k].is_string())
            {
                return Err(error("invalid_response"));
            }
            decoded.push(json!({"selectorKind":tuple["selector_kind"],"selector":tuple["selector"],"operation":tuple["operation"]}));
        }
        out["grantTuples"] = decoded.into();
    }
    Ok(out)
}

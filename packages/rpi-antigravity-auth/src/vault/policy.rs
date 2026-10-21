//! Reachable Antigravity custody validation; no secret-bearing diagnostics.
use super::client::safe_nonnegative;
use crate::json::trim;
use serde_json::{json, Value};

pub const SERVING_MARGIN_MS: f64 = 300_000.0;
pub const KEPT_RECEIPT_MARGIN_MS: f64 = 60_000.0;
pub const CUSTODY_PLACEHOLDER_PREFIX: &str = "claustrum-tombstone:v1:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsumerError {
    pub kind: &'static str,
    pub message: String,
}
impl ConsumerError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ConsumerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ConsumerError {}

/// Unknown and unreviewed codes fail closed, even when their action is retry.
pub fn unreachable(code: &str) -> bool {
    matches!(
        code,
        "ECONNREFUSED"
            | "ENOENT"
            | "ECONNRESET"
            | "EPIPE"
            | "ETIMEDOUT"
            | "transport_error"
            | "unavailable"
            | "timeout"
            | "route_closed"
            | "daemon_draining"
            | "connection_closed"
            | "connection_dropped"
            | "request_deadline"
            | "deadline_exceeded_no_drop_observed"
            | "module_reloading"
            | "module_warming"
            | "target_unavailable"
            | "module_timeout"
            | "unknown_channel"
            | "stale_route_epoch"
    )
}
fn nonblank(value: &Value) -> bool {
    value.as_str().is_some_and(|s| !trim(s).is_empty())
}
fn includes(value: &Value, expected: &str) -> bool {
    value
        .as_array()
        .is_some_and(|a| a.iter().any(|v| v == expected))
}

/// The wire decoder validates shapes first; this selects this provider's family.
pub fn inventory(listed: &Value) -> Value {
    let candidates: Vec<_> = listed["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            includes(&row["operations"], "read")
                && includes(&row["categories"], "antigravity-native")
                && row["credentialType"] == "oauth"
                && row["refreshAdapter"] == "antigravity"
        })
        .collect();
    let mut counts = std::collections::HashMap::new();
    for row in &candidates {
        *counts.entry(row["id"].as_str().unwrap()).or_insert(0usize) += 1;
    }
    let mut credentials = vec![];
    let mut skipped = vec![];
    for row in &candidates {
        let reason = if !nonblank(&row["id"]) {
            Some("empty credential id")
        } else if counts[row["id"].as_str().unwrap()] > 1 {
            Some("duplicate credential id")
        } else if row.get("accountId").is_some() && !nonblank(&row["accountId"]) {
            Some("blank account identity")
        } else if !nonblank(&row["state"]) {
            Some("empty state")
        } else {
            None
        };
        if let Some(reason) = reason {
            let mut record = json!({});
            if nonblank(&row["id"]) {
                record["credentialId"] = row["id"].clone();
            }
            record["reason"] = json!(reason);
            skipped.push(record);
        } else {
            let mut record =
                json!({"credentialId":row["id"],"credentialType":"oauth","state":row["state"]});
            for (to, from) in [
                ("accountIdentity", "accountId"),
                ("email", "email"),
                ("orgName", "orgName"),
            ] {
                if let Some(value) = row.get(from) {
                    record[to] = value.clone();
                }
            }
            credentials.push(record);
        }
    }
    let mut result = json!({"view":listed["view"],"credentials":credentials,"skipped":skipped});
    if listed.get("grants").is_some() && listed.get("grantTuples").is_some() {
        result["grantReach"] = json!({"grants":listed["grants"],"tuples":listed["grantTuples"]});
    }
    result
}
fn invalid_material() -> ConsumerError {
    ConsumerError::new(
        "invalid-material",
        "Claustrum returned invalid credential material",
    )
}
fn access_token(material: &str) -> Result<String, ConsumerError> {
    let access = trim(material);
    let parsed: Value;
    let access = if access.starts_with('{') {
        parsed = serde_json::from_str(access).map_err(|_| invalid_material())?;
        parsed
            .get("access_token")
            .filter(|v| !v.is_null())
            .or_else(|| parsed.get("access"))
            .and_then(Value::as_str)
            .unwrap_or("")
    } else {
        access
    };
    if access.is_empty()
        || !access.bytes().all(|b| (0x21..=0x7e).contains(&b))
        || access.starts_with(CUSTODY_PLACEHOLDER_PREFIX)
    {
        return Err(invalid_material());
    }
    Ok(access.to_owned())
}

/// Serialized receipt metadata never contains the bearer. Custody supplies provenance separately.
#[derive(Clone)]
pub struct Authorized {
    pub metadata: Value,
    access_token: String,
}
impl std::fmt::Debug for Authorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Authorized")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}
impl Authorized {
    pub fn access_token(&self) -> &str {
        &self.access_token
    }
    pub fn usable_kept(&self, now: f64) -> bool {
        self.metadata["expiresAtMs"]
            .as_f64()
            .is_some_and(|expires| expires - now > KEPT_RECEIPT_MARGIN_MS)
    }
}

pub fn validate_receipt(
    identity: &Value,
    served: &Value,
    now: f64,
) -> Result<Authorized, ConsumerError> {
    if identity["credentialId"].as_str().unwrap_or("").is_empty() {
        return Err(ConsumerError::new(
            "route-unavailable",
            "Claustrum dispatch requires a credential id",
        ));
    }
    if identity.get("accountIdentity").is_none() {
        return Err(ConsumerError::new(
            "identity-unasserted",
            "Claustrum dispatch requires a known account identity",
        ));
    }
    if served
        .get("credentialId")
        .is_some_and(|id| id != &identity["credentialId"])
    {
        return Err(ConsumerError::new(
            "identity-changed",
            "Claustrum served credential identity changed",
        ));
    }
    if !safe_nonnegative(&served["recordVersion"])
        || !served["expiresAtMs"]
            .as_f64()
            .is_some_and(|expires| expires.is_finite() && expires - now >= SERVING_MARGIN_MS)
    {
        return Err(ConsumerError::new(
            "insufficient-validity",
            "Claustrum served credential has insufficient validity",
        ));
    }
    let access_token = access_token(served["material"].as_str().unwrap_or(""))?;
    if served.get("credentialId").is_none() || !nonblank(&served["accountId"]) {
        return Err(ConsumerError::new(
            "identity-unasserted",
            "Claustrum served credential did not assert its identity",
        ));
    }
    if served["accountId"] != identity["accountIdentity"] {
        return Err(ConsumerError::new(
            "identity-changed",
            "Claustrum served credential identity changed",
        ));
    }
    let mut metadata = json!({"credentialId":identity["credentialId"],"credentialType":identity["credentialType"],"accountIdentity":served["accountId"],"accountIdentitySource":"asserted","expectedAccountIdentity":identity["accountIdentity"],"assertedCredentialId":served["credentialId"],"assertedAccountIdentity":served["accountId"],"recordVersion":served["recordVersion"],"expiresAtMs":served["expiresAtMs"]});
    if let Some(project) = served.get("projectId") {
        metadata["projectId"] = project.clone();
    }
    Ok(Authorized {
        metadata,
        access_token,
    })
}

pub fn is_rotation(served: &Value, current: Option<&Value>) -> bool {
    current.is_some_and(|current| {
        current["fromKeptReceipt"] != true
            && served["fromKeptReceipt"] != true
            && current["credentialId"] == served["credentialId"]
            && current["accountIdentity"] == served["accountIdentity"]
            && current["recordVersion"] != served["recordVersion"]
    })
}

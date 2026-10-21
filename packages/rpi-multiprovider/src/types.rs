use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) fn trim(value: &str) -> &str {
    value.trim_matches(|c| matches!(c, '\t'..='\r' | ' ' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}'))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionPolicy {
    #[default]
    RoundRobin,
    WeightedRoundRobin,
    LeastInflight,
    Priority,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionBias {
    #[default]
    FirstAccount,
    None,
}

// Credential references remain private to lease holders; public snapshots use a whitelist.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub label: String,
    pub auth_kind: String,
    pub credential_ref: Value,
    pub enabled: Option<bool>,
    pub weight: Option<f64>,
    pub priority: Option<f64>,
    pub metadata: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPreference {
    pub account_id: String,
    pub enabled: bool,
    pub weight: f64,
    pub priority: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolPreference {
    pub provider_id: String,
    pub policy: SelectionPolicy,
    pub affinity: bool,
    pub accounts: Vec<AccountPreference>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolPatch {
    pub policy: Option<SelectionPolicy>,
    pub affinity: Option<bool>,
    pub accounts: Option<Vec<AccountPreference>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AffinityPin {
    pub account_id: String,
    pub explicit: bool,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireOptions {
    pub provider_id: String,
    pub model_id: Option<String>,
    pub affinity_key: Option<String>,
    #[serde(default)]
    pub exclude_account_ids: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptFailure {
    pub message: String,
    pub status: Option<u16>,
    pub headers: Option<Value>,
    pub assistant_message: Option<Value>,
    pub cause: Option<Value>,
    pub output_started: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureKind {
    RateLimit,
    Quota,
    Auth,
    Transient,
    Fatal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FailureScope {
    Account,
    AccountModel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disposition {
    pub kind: FailureKind,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cooldown_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<FailureScope>,
}

#[derive(Default, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum LeaseOutcome {
    Success,
    Failure {
        error: AttemptFailure,
    },
    #[default]
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub rate_limit_cooldown_ms: f64,
    pub quota_cooldown_ms: f64,
    pub auth_cooldown_ms: f64,
    pub transient_base_cooldown_ms: f64,
    pub max_cooldown_ms: f64,
    pub errors_before_switch: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            rate_limit_cooldown_ms: 60_000.0,
            quota_cooldown_ms: 900_000.0,
            auth_cooldown_ms: 300_000.0,
            transient_base_cooldown_ms: 1_000.0,
            max_cooldown_ms: 3_600_000.0,
            errors_before_switch: 3.0,
        }
    }
}

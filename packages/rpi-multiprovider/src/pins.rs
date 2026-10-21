use crate::services::{Error, Service};
use crate::types::trim;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SESSION_PIN_ENTRY_TYPE: &str = "pi-multiprovider:switch-account";
pub const SESSION_PIN_ENV: &str = "PI_MULTIPROVIDER_SESSION_PINS";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InheritedPin {
    pub pool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPin {
    pub pool: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

fn inherited_pin(value: &Value) -> Option<InheritedPin> {
    let pool = trim(value.get("pool")?.as_str()?);
    if pool.is_empty() {
        return None;
    }
    let account_id = match value.get("accountId") {
        None => None,
        Some(value) => {
            let id = trim(value.as_str()?);
            if id.is_empty() {
                return None;
            }
            Some(id.into())
        }
    };
    let label = value
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| !trim(label).is_empty())
        .map(str::to_string);
    Some(InheritedPin {
        pool: pool.into(),
        account_id,
        label,
    })
}

pub fn from_entries<'a>(entries: impl IntoIterator<Item = &'a Value>) -> Vec<SessionPin> {
    let mut latest = IndexMap::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("customType").and_then(Value::as_str) != Some(SESSION_PIN_ENTRY_TYPE)
        {
            continue;
        }
        let data = &entry["data"];
        let Some(pin) = inherited_pin(data) else {
            continue;
        };
        let Some(key) = data
            .get("key")
            .and_then(Value::as_str)
            .map(trim)
            .filter(|key| !key.is_empty())
        else {
            continue;
        };
        latest.insert(
            (pin.pool.clone(), key.to_string()),
            SessionPin {
                pool: pin.pool,
                key: key.into(),
                account_id: pin.account_id,
                label: pin.label,
            },
        );
    }
    latest.into_values().collect()
}

pub fn inherited_from_value(value: &Value) -> Vec<InheritedPin> {
    let mut latest = IndexMap::new();
    if let Some(pins) = value.as_array() {
        for pin in pins.iter().filter_map(inherited_pin) {
            latest.insert(pin.pool.clone(), pin);
        }
    }
    latest.into_values().collect()
}

pub fn inherited_from_env(env: &Value) -> Vec<InheritedPin> {
    env.get(SESSION_PIN_ENV)
        .and_then(Value::as_str)
        .and_then(|raw| serde_json::from_str(raw).ok())
        .map(|value| inherited_from_value(&value))
        .unwrap_or_default()
}

pub fn serialize_inherited(value: &Value) -> String {
    serde_json::to_string(&inherited_from_value(value)).expect("pin strings serialize")
}

pub async fn apply(
    pins: &[SessionPin],
    service: &Service,
    mut on_error: impl FnMut(&SessionPin, Error),
    mut on_applied: impl FnMut(&SessionPin),
) -> Vec<SessionPin> {
    let mut pending = Vec::new();
    for pin in pins {
        if !service.has_provider(&pin.pool) {
            pending.push(pin.clone());
            continue;
        }
        if let Some(account) = &pin.account_id {
            if let Err(error) = service.pin_account(&pin.pool, &pin.key, account).await {
                on_error(pin, error);
                continue;
            }
        } else {
            service.clear_affinity(Some(&pin.pool), Some(&pin.key));
        }
        on_applied(pin);
    }
    pending
}

#![allow(dead_code)]

use async_trait::async_trait;
use rpi_multiprovider::managed::{
    AccountStore, AuthCheck, AuthResult, ModelAuth, ModelRef, NativeApiKey, NativeInput,
    NativeOAuth, NativeProvider,
};
use rpi_multiprovider::types::Account;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub const DEFAULT_NOW: f64 = 1700000000000.0;
pub const ABORT_MESSAGE: &str = "multiprovider: operation cancelled";

pub type Trace = Arc<Mutex<Vec<Value>>>;

pub fn push(trace: &Trace, event: Value) {
    trace.lock().unwrap().push(event);
}

/// Compares JavaScript numbers by value: `0` and `0.0` are the same number.
pub fn normalize(value: &Value) -> Value {
    match value {
        Value::Number(number) => json!(number.as_f64().unwrap_or(f64::NAN)),
        Value::Array(items) => Value::Array(items.iter().map(normalize).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), normalize(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn signal_for(input: &Value) -> CancellationToken {
    if input["aborted"] == true {
        let token = CancellationToken::new();
        token.cancel();
        token
    } else {
        CancellationToken::new()
    }
}

/// `Account` as the JavaScript object: undefined fields are absent.
pub fn account_json(account: &Account) -> Value {
    let mut value = json!({
        "id": account.id,
        "label": account.label,
        "authKind": account.auth_kind,
        "credentialRef": account.credential_ref,
    });
    if let Some(enabled) = account.enabled {
        value["enabled"] = json!(enabled);
    }
    if let Some(weight) = account.weight {
        value["weight"] = json!(weight);
    }
    if let Some(priority) = account.priority {
        value["priority"] = json!(priority);
    }
    if let Some(metadata) = &account.metadata {
        value["metadata"] = metadata.clone();
    }
    value
}

/// Replaces `{"$ref": n, "path": [...]}` with a value recorded by an earlier action.
pub fn resolve_refs(value: &Value, results: &[Value]) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| resolve_refs(item, results))
                .collect(),
        ),
        Value::Object(map) => {
            if let Some(index) = map.get("$ref") {
                let outcome = &results[index.as_u64().unwrap() as usize];
                let result = outcome
                    .get("result")
                    .unwrap_or_else(|| panic!("ref has no result"))
                    .clone();
                let path: Vec<&str> = map
                    .get("path")
                    .and_then(Value::as_array)
                    .map(|keys| keys.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                if path.is_empty() || path == ["result"] {
                    return result;
                }
                let mut current = &result;
                for key in path {
                    current = current
                        .get(key)
                        .unwrap_or_else(|| panic!("ref path {key} missing"));
                }
                return current.clone();
            }
            Value::Object(
                map.iter()
                    .map(|(key, value)| (key.clone(), resolve_refs(value, results)))
                    .collect(),
            )
        }
        other => other.clone(),
    }
}

/// The fixture's native `respond`: `Ok(None)` for an undefined result, `Err` for a throw.
fn respond(responses: &Value, kind: &str, key: &str) -> Result<Option<Value>, String> {
    let entry = responses
        .get(kind)
        .and_then(|table| table.get(key))
        .ok_or_else(|| format!("unscripted {kind} {key}"))?;
    if let Some(message) = entry.get("throw").and_then(Value::as_str) {
        return Err(message.to_owned());
    }
    if entry.get("none") == Some(&Value::Bool(true)) {
        return Ok(None);
    }
    Ok(entry.get("value").cloned())
}

fn credential_event(input: &NativeInput) -> Value {
    let mut event = json!({ "aborted": input.signal.is_cancelled() });
    if let Some(credential) = &input.credential {
        event["credential"] = credential.clone();
    }
    event
}

fn key_of(input: &NativeInput) -> String {
    input
        .credential
        .as_ref()
        .and_then(|credential| credential.get("key"))
        .and_then(Value::as_str)
        .unwrap_or("<none>")
        .to_owned()
}

pub struct FixtureApiKey {
    name: String,
    login: bool,
    check_enabled: bool,
    responses: Value,
    trace: Trace,
}

#[async_trait]
impl NativeApiKey for FixtureApiKey {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn has_login(&self) -> bool {
        self.login
    }
    fn has_check(&self) -> bool {
        self.check_enabled
    }
    async fn check(&self, input: NativeInput) -> Result<Option<AuthCheck>, String> {
        let mut event = credential_event(&input);
        event["native"] = json!("apiKey.check");
        push(&self.trace, event);
        match respond(&self.responses, "check", &key_of(&input))? {
            Some(value) => serde_json::from_value(value)
                .map(Some)
                .map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }
    async fn resolve(&self, input: NativeInput) -> Result<Option<AuthResult>, String> {
        let mut event = credential_event(&input);
        event["native"] = json!("apiKey.resolve");
        push(&self.trace, event);
        match respond(&self.responses, "resolve", &key_of(&input))? {
            Some(value) => serde_json::from_value(value)
                .map(Some)
                .map_err(|e| e.to_string()),
            None => Ok(None),
        }
    }
}

pub struct FixtureOAuth {
    name: String,
    is_subscription: Option<bool>,
    login_label: Option<String>,
    responses: Value,
    trace: Trace,
}

#[async_trait]
impl NativeOAuth for FixtureOAuth {
    fn name(&self) -> String {
        self.name.clone()
    }
    fn is_subscription(&self) -> Option<bool> {
        self.is_subscription
    }
    fn login_label(&self) -> Option<String> {
        self.login_label.clone()
    }
    async fn refresh(&self, credential: Value, signal: CancellationToken) -> Result<Value, String> {
        push(
            &self.trace,
            json!({"native": "oauth.refresh", "credential": credential, "aborted": signal.is_cancelled()}),
        );
        let key = credential
            .get("refresh")
            .and_then(Value::as_str)
            .unwrap_or("undefined");
        Ok(respond(&self.responses, "refresh", key)?.unwrap_or(Value::Null))
    }
    async fn to_auth(&self, credential: &Value) -> Result<ModelAuth, String> {
        push(
            &self.trace,
            json!({"native": "oauth.toAuth", "credential": credential}),
        );
        let key = credential
            .get("access")
            .and_then(Value::as_str)
            .unwrap_or("undefined");
        match respond(&self.responses, "toAuth", key)? {
            Some(value) => serde_json::from_value(value).map_err(|e| e.to_string()),
            None => Err("Cannot read properties of undefined (reading 'headers')".to_owned()),
        }
    }
}

pub struct FixtureProvider {
    id: String,
    name: String,
    base_url: Option<String>,
    models: Vec<ModelRef>,
    api_key: Option<Arc<FixtureApiKey>>,
    oauth: Option<Arc<FixtureOAuth>>,
}

impl NativeProvider for FixtureProvider {
    fn id(&self) -> String {
        self.id.clone()
    }
    fn name(&self) -> String {
        self.name.clone()
    }
    fn base_url(&self) -> Option<String> {
        self.base_url.clone()
    }
    fn models(&self) -> Vec<ModelRef> {
        self.models.clone()
    }
    fn api_key(&self) -> Option<Arc<dyn NativeApiKey>> {
        self.api_key
            .clone()
            .map(|method| method as Arc<dyn NativeApiKey>)
    }
    fn oauth(&self) -> Option<Arc<dyn NativeOAuth>> {
        self.oauth
            .clone()
            .map(|method| method as Arc<dyn NativeOAuth>)
    }
}

/// Builds the scripted provider of a fixture scenario.
pub fn fixture_provider(spec: &Value, trace: &Trace) -> Arc<FixtureProvider> {
    let models = spec["models"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .map(|model| ModelRef {
                    id: model["id"].as_str().unwrap().to_owned(),
                    base_url: model["baseUrl"].as_str().map(str::to_owned),
                })
                .collect()
        })
        .unwrap_or_default();
    let api_key = spec
        .get("apiKey")
        .filter(|value| !value.is_null())
        .map(|native| {
            Arc::new(FixtureApiKey {
                name: native["name"].as_str().unwrap().to_owned(),
                login: native["login"] == true,
                check_enabled: native["check"] == true,
                responses: native["responses"].clone(),
                trace: trace.clone(),
            })
        });
    let oauth = spec
        .get("oauth")
        .filter(|value| !value.is_null())
        .map(|native| {
            Arc::new(FixtureOAuth {
                name: native["name"].as_str().unwrap().to_owned(),
                is_subscription: native["isSubscription"].as_bool(),
                login_label: native["loginLabel"].as_str().map(str::to_owned),
                responses: native["responses"].clone(),
                trace: trace.clone(),
            })
        });
    Arc::new(FixtureProvider {
        id: spec["id"].as_str().unwrap().to_owned(),
        name: spec["name"].as_str().unwrap().to_owned(),
        base_url: spec["baseUrl"].as_str().map(str::to_owned),
        models,
        api_key,
        oauth,
    })
}

/// The persisted account file, with its lock and clock, as the decision logic sees it.
/// Each call records the same event the upstream file operations produce.
pub struct FixtureStore {
    trace: Trace,
    now: f64,
    file: Mutex<Option<Value>>,
    locked: Mutex<bool>,
}

impl FixtureStore {
    pub fn new(trace: Trace, state: Option<Value>, now: f64) -> Self {
        Self {
            trace,
            now,
            file: Mutex::new(state),
            locked: Mutex::new(false),
        }
    }

    /// The parsed file after the run, or `null` when it was never written.
    pub fn final_state(&self) -> Value {
        self.file.lock().unwrap().clone().unwrap_or(Value::Null)
    }

    pub fn is_locked(&self) -> bool {
        *self.locked.lock().unwrap()
    }
}

#[async_trait]
impl AccountStore for FixtureStore {
    fn now_ms(&self) -> f64 {
        self.now
    }
    async fn read_state(&self) -> Result<Value, String> {
        push(&self.trace, json!({"store": "read"}));
        Ok(self
            .file
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| json!({"version": 1, "providers": {}})))
    }
    async fn lock(&self, signal: &CancellationToken) -> Result<(), String> {
        if signal.is_cancelled() {
            return Err(ABORT_MESSAGE.to_owned());
        }
        push(&self.trace, json!({"store": "lock"}));
        *self.locked.lock().unwrap() = true;
        Ok(())
    }
    async fn unlock(&self) {
        push(&self.trace, json!({"store": "unlock"}));
        *self.locked.lock().unwrap() = false;
    }
    async fn write_state(&self, state: &Value) -> Result<(), String> {
        push(&self.trace, json!({"store": "write"}));
        *self.file.lock().unwrap() = Some(state.clone());
        Ok(())
    }
}

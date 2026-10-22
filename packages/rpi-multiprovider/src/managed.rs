//! Port of `src/managed.ts` (the provider-owned account integration) and the
//! `resolveAccount` decision logic of `src/auth-store.ts`.
//!
//! Host-independent: the account store (with its lock and clock), the provider's
//! native auth methods and the request context are injected through traits, so
//! tests can script every callback and observe the lock, read and write order.
use crate::services::iso_date;
use crate::types::{trim, Account};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub const PI_UPSTREAM_ACCOUNT_ID: &str = "pi:default";
pub const MANAGEMENT_HINT: &str =
    "Use /multilogin to manage the pool and /multilogout to remove accounts quickly.";
pub const AUTH_MARKER_HEADER: &str = "x-pi-multiprovider-auth-source";
const UPSTREAM_LABEL: &str = "Pi default";
const UPSTREAM_SOURCE: &str = "auth.json, environment, or provider ambient auth";
const POOL_SOURCE: &str = "multiprovider account pool";
const POOL_NAME: &str = "Multiprovider account pool";
const PI_DEFAULT_SOURCE: &str = "Pi default auth";
const OAUTH_REFRESH_SKEW_MS: f64 = 5.0 * 60_000.0;
const ABORT_MESSAGE: &str = "multiprovider: operation cancelled";

/// `ProviderHeaders`: string or null values in insertion order.
pub type Headers = Map<String, Value>;
/// `ProviderEnv`: string values.
pub type Env = Map<String, Value>;
/// `StreamOptions & Record<string, unknown>` kept as the raw JSON object.
pub type RequestOptions = Map<String, Value>;

/// `ModelAuth`. An absent field and a present-but-undefined field are the same here.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelAuth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Headers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// `AuthResult`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthResult {
    pub auth: ModelAuth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// `AuthCheck`: `{ type, source? }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthCheck {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// `Model` as far as the managed integration reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelRef {
    pub id: String,
    pub base_url: Option<String>,
}

/// Input of the provider's native `check`/`resolve`: the stored credential
/// (absent when none is stored) and the cancellation signal.
#[derive(Clone)]
pub struct NativeInput {
    pub credential: Option<Value>,
    pub signal: CancellationToken,
}

/// The provider's own api-key auth method (`ApiKeyAuth`).
#[async_trait]
pub trait NativeApiKey: Send + Sync {
    fn name(&self) -> String;
    fn has_login(&self) -> bool {
        false
    }
    fn has_check(&self) -> bool {
        false
    }
    async fn check(&self, input: NativeInput) -> Result<Option<AuthCheck>, String>;
    async fn resolve(&self, input: NativeInput) -> Result<Option<AuthResult>, String>;
}

/// The provider's own OAuth method (`OAuthAuth`).
#[async_trait]
pub trait NativeOAuth: Send + Sync {
    fn name(&self) -> String;
    fn is_subscription(&self) -> Option<bool> {
        None
    }
    fn login_label(&self) -> Option<String> {
        None
    }
    async fn refresh(&self, credential: Value, signal: CancellationToken) -> Result<Value, String>;
    async fn to_auth(&self, credential: &Value) -> Result<ModelAuth, String>;
}

/// The provider as seen by the managed integration (`Provider`).
pub trait NativeProvider: Send + Sync {
    fn id(&self) -> String;
    fn name(&self) -> String;
    fn base_url(&self) -> Option<String>;
    fn models(&self) -> Vec<ModelRef>;
    fn api_key(&self) -> Option<Arc<dyn NativeApiKey>>;
    fn oauth(&self) -> Option<Arc<dyn NativeOAuth>>;
}

/// The persisted account store as the decision logic needs it. `lock` and `unlock`
/// bracket the store lock; every read and write goes through this trait so a host
/// (or a test) can observe the exact order.
#[async_trait]
pub trait AccountStore: Send + Sync {
    /// Clock for `Date.now()` and `new Date()`, in milliseconds.
    fn now_ms(&self) -> f64;
    /// Reads the whole persisted document; missing storage reads as an empty document.
    async fn read_state(&self) -> Result<Value, String>;
    /// Takes the lock. Cancellation is honoured before the lock is taken.
    async fn lock(&self, signal: &CancellationToken) -> Result<(), String>;
    async fn unlock(&self);
    async fn write_state(&self, state: &Value) -> Result<(), String>;
}

/// The request context handed to `resolveAuth` (`AccountRequestContext`).
pub struct AccountRequest {
    pub provider: Arc<dyn NativeProvider>,
    pub model: ModelRef,
    pub context: Value,
    pub request_options: RequestOptions,
    pub signal: CancellationToken,
}

/// A provider integration as the managed layer and the announcement consume it.
#[async_trait]
pub trait ProviderIntegration: Send + Sync {
    async fn accounts(&self) -> Result<Vec<Account>, String>;
    async fn resolve_auth(
        &self,
        account: &Account,
        signal: &CancellationToken,
        request: &AccountRequest,
    ) -> Result<AuthResult, String>;
}

fn abort_error() -> String {
    ABORT_MESSAGE.to_owned()
}

/// JavaScript truthiness of an optional JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().map_or(false, |n| n != 0.0 && !n.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// JavaScript `Number(value)` for the JSON shapes that reach the expiry check.
/// `None` (an absent field) is `undefined`, which compares false.
fn js_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(flag)) => f64::from(u8::from(*flag)),
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(text)) => {
            let text = trim(text);
            match text {
                "" => 0.0,
                "Infinity" | "+Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ if text
                    .chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) =>
                {
                    text.parse::<f64>().unwrap_or(f64::NAN)
                }
                _ => f64::NAN,
            }
        }
        Some(_) => f64::NAN,
    }
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn same_name(candidate: &str, name: &str) -> bool {
    candidate.to_lowercase() == name.to_lowercase()
}

fn find_header<'a>(headers: Option<&'a Headers>, name: &str) -> Option<&'a Value> {
    headers?
        .iter()
        .find(|(candidate, _)| same_name(candidate, name))
        .map(|(_, value)| value)
}

fn has_header(headers: Option<&Headers>, name: &str) -> bool {
    headers.map_or(false, |headers| {
        headers.keys().any(|candidate| same_name(candidate, name))
    })
}

fn remove_header(headers: &mut Headers, name: &str) {
    headers.retain(|candidate, _| !same_name(candidate, name));
}

fn object_copy(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// `UpstreamMarker`: the marker the managed layer writes into header values.
struct UpstreamMarker {
    available: bool,
    header_names: Vec<String>,
    env_names: Vec<String>,
    custom_base_url: bool,
}

impl UpstreamMarker {
    fn to_header_value(&self) -> String {
        json!({
            "available": self.available,
            "headerNames": self.header_names,
            "envNames": self.env_names,
            "customBaseUrl": self.custom_base_url,
        })
        .to_string()
    }
}

fn marker_for(auth: &ModelAuth, env: Option<&Env>, available: bool) -> UpstreamMarker {
    UpstreamMarker {
        available,
        header_names: auth
            .headers
            .iter()
            .flat_map(|headers| headers.keys())
            .filter(|name| name.to_lowercase() != AUTH_MARKER_HEADER)
            .cloned()
            .collect(),
        env_names: env.iter().flat_map(|env| env.keys()).cloned().collect(),
        custom_base_url: auth.base_url.is_some(),
    }
}

fn with_marker(mut auth: ModelAuth, marker: String) -> ModelAuth {
    let mut headers = auth.headers.take().unwrap_or_default();
    headers.insert(AUTH_MARKER_HEADER.to_owned(), Value::String(marker));
    auth.headers = Some(headers);
    auth
}

fn mark_model_auth(auth: ModelAuth, available: bool) -> ModelAuth {
    let marker = marker_for(&auth, None, available).to_header_value();
    with_marker(auth, marker)
}

fn mark_auth_result(result: AuthResult, available: bool) -> AuthResult {
    let marker = marker_for(&result.auth, result.env.as_ref(), available).to_header_value();
    AuthResult {
        auth: with_marker(result.auth, marker),
        env: result.env,
        source: result.source,
    }
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `readMarker`: the marker header's first case-insensitive match, parsed leniently.
fn read_marker(request_options: &RequestOptions) -> Option<UpstreamMarker> {
    let headers = request_options.get("headers").and_then(Value::as_object);
    let Value::String(text) = find_header(headers, AUTH_MARKER_HEADER)? else {
        return None;
    };
    let parsed: Value = serde_json::from_str(text).ok()?;
    let object = parsed.as_object()?;
    let available = object.get("available")?.as_bool()?;
    Some(UpstreamMarker {
        available,
        header_names: string_array(object.get("headerNames")),
        env_names: string_array(object.get("envNames")),
        custom_base_url: object.get("customBaseUrl") == Some(&Value::Bool(true)),
    })
}

/// `restoreBaseUrl`: the model's own base URL, else the provider's.
fn restore_base_url(provider: &dyn NativeProvider, model_id: &str) -> Option<String> {
    provider
        .models()
        .into_iter()
        .find(|model| model.id == model_id)
        .and_then(|model| model.base_url)
        .or_else(|| provider.base_url())
}

/// `sanitizeAttempt`: drops the upstream marker and the headers and environment
/// values that the marker attributes to another account.
fn sanitize_attempt(
    account_id: &str,
    resolution: &AuthResult,
    request_options: &RequestOptions,
) -> RequestOptions {
    let marker = read_marker(request_options);
    let mut options = request_options.clone();
    let mut headers = object_copy(options.get("headers"));
    remove_header(&mut headers, AUTH_MARKER_HEADER);
    if account_id != PI_UPSTREAM_ACCOUNT_ID {
        if let Some(marker) = &marker {
            for name in &marker.header_names {
                if !has_header(resolution.auth.headers.as_ref(), name) {
                    remove_header(&mut headers, name);
                }
            }
            let selected: HashSet<String> = resolution
                .env
                .iter()
                .flat_map(|env| env.keys().cloned())
                .collect();
            let mut env = object_copy(options.get("env"));
            for name in &marker.env_names {
                if !selected.contains(name) {
                    env.remove(name.as_str());
                }
            }
            if env.is_empty() {
                options.remove("env");
            } else {
                options.insert("env".to_owned(), Value::Object(env));
            }
        }
    }
    if headers.is_empty() {
        options.remove("headers");
    } else {
        options.insert("headers".to_owned(), Value::Object(headers));
    }
    options
}

/// `mergeProviderAuth`: the pool-aware api-key method and the marking OAuth wrapper.
pub fn merge_provider_auth(
    provider: &dyn NativeProvider,
    store: Arc<dyn AccountStore>,
) -> MergedAuth {
    let original = provider.api_key();
    let oauth = provider.oauth().map(|method| MergedOAuth {
        name: method.name(),
        is_subscription: method.is_subscription(),
        login_label: method.login_label(),
        original: method,
    });
    MergedAuth {
        api_key: MergedApiKey {
            name: original
                .as_ref()
                .map_or_else(|| POOL_NAME.to_owned(), |method| method.name()),
            has_login: original.as_ref().map_or(false, |method| method.has_login()),
            has_check: original.as_ref().map_or(false, |method| method.has_check()),
            provider_id: provider.id(),
            store,
            original,
        },
        oauth,
    }
}

/// The merged `ProviderAuth`.
pub struct MergedAuth {
    pub api_key: MergedApiKey,
    pub oauth: Option<MergedOAuth>,
}

impl MergedAuth {
    /// `Object.keys` of the merged auth object, in definition order.
    pub fn surface_keys(&self) -> Vec<&'static str> {
        let mut keys = vec!["apiKey"];
        if self.oauth.is_some() {
            keys.push("oauth");
        }
        keys
    }
}

/// The merged api-key method. Pool fallbacks run only when stored accounts exist.
pub struct MergedApiKey {
    pub name: String,
    pub has_login: bool,
    pub has_check: bool,
    provider_id: String,
    store: Arc<dyn AccountStore>,
    original: Option<Arc<dyn NativeApiKey>>,
}

impl MergedApiKey {
    /// `Object.keys` of the merged api-key method.
    pub fn surface_keys(&self) -> Vec<&'static str> {
        let mut keys = vec!["name"];
        if self.has_login {
            keys.push("login");
        }
        keys.push("check");
        keys.push("resolve");
        keys
    }

    async fn has_stored_accounts(&self) -> Result<bool, String> {
        has_accounts(&*self.store, &self.provider_id).await
    }

    pub async fn check(&self, input: NativeInput) -> Result<Option<AuthCheck>, String> {
        if let Some(original) = &self.original {
            if original.has_check() {
                if let Some(checked) = original.check(input).await? {
                    return Ok(Some(checked));
                }
            } else {
                match original.resolve(input).await {
                    Ok(Some(resolved)) => {
                        return Ok(Some(AuthCheck {
                            kind: "api_key".to_owned(),
                            source: resolved.source,
                        }))
                    }
                    Ok(None) => {}
                    Err(error) => {
                        if !self.has_stored_accounts().await? {
                            return Err(error);
                        }
                    }
                }
            }
        }
        if self.has_stored_accounts().await? {
            Ok(Some(AuthCheck {
                kind: "api_key".to_owned(),
                source: Some(POOL_SOURCE.to_owned()),
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn resolve(&self, input: NativeInput) -> Result<Option<AuthResult>, String> {
        if let Some(original) = &self.original {
            match original.resolve(input).await {
                Ok(Some(resolved)) => return Ok(Some(mark_auth_result(resolved, true))),
                Ok(None) => {}
                Err(error) => {
                    if !self.has_stored_accounts().await? {
                        return Err(error);
                    }
                }
            }
        }
        if self.has_stored_accounts().await? {
            Ok(Some(mark_auth_result(
                AuthResult {
                    auth: ModelAuth::default(),
                    env: None,
                    source: Some(POOL_SOURCE.to_owned()),
                },
                false,
            )))
        } else {
            Ok(None)
        }
    }
}

/// The wrapped OAuth method: refresh passes through, toAuth marks the result available.
pub struct MergedOAuth {
    pub name: String,
    pub is_subscription: Option<bool>,
    pub login_label: Option<String>,
    original: Arc<dyn NativeOAuth>,
}

impl MergedOAuth {
    /// `Object.keys` of the wrapped OAuth method, in definition order.
    pub fn surface_keys(&self) -> Vec<&'static str> {
        let mut keys = vec!["name"];
        if self.is_subscription.is_some() {
            keys.push("isSubscription");
        }
        if self.login_label.is_some() {
            keys.push("loginLabel");
        }
        keys.extend(["login", "refresh", "toAuth"]);
        keys
    }

    pub async fn refresh(
        &self,
        credential: Value,
        signal: CancellationToken,
    ) -> Result<Value, String> {
        self.original.refresh(credential, signal).await
    }

    pub async fn to_auth(&self, credential: &Value) -> Result<ModelAuth, String> {
        Ok(mark_model_auth(
            self.original.to_auth(credential).await?,
            true,
        ))
    }
}

/// `assertCredential`: a stored or refreshed credential must be an object with a known type.
fn assert_credential(value: &Value) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err("multiprovider: malformed stored credential".to_owned());
    };
    let Some(kind) = object.get("type") else {
        return Err("multiprovider: malformed stored credential".to_owned());
    };
    match kind.as_str() {
        Some("api_key") | Some("oauth") => Ok(()),
        _ => Err("multiprovider: unsupported stored credential type".to_owned()),
    }
}

fn find_account<'a>(state: &'a Value, provider_id: &str, account_id: &str) -> Option<&'a Value> {
    state["providers"].get(provider_id)?["accounts"]
        .as_array()?
        .iter()
        .find(|account| account.get("id").and_then(Value::as_str) == Some(account_id))
}

fn find_account_mut<'a>(
    state: &'a mut Value,
    provider_id: &str,
    account_id: &str,
) -> Option<&'a mut Value> {
    state
        .get_mut("providers")?
        .get_mut(provider_id)?
        .get_mut("accounts")?
        .as_array_mut()?
        .iter_mut()
        .find(|account| account.get("id").and_then(Value::as_str) == Some(account_id))
}

/// The locked section of `resolveAccount`: re-read, refresh if due, persist, return.
async fn refresh_locked(
    store: &dyn AccountStore,
    provider: &dyn NativeProvider,
    method: &dyn NativeOAuth,
    account_id: &str,
    signal: &CancellationToken,
) -> Result<(Value, String), String> {
    let provider_id = provider.id();
    let mut state = store.read_state().await?;
    let account = find_account_mut(&mut state, &provider_id, account_id)
        .filter(|account| account["credential"]["type"].as_str() == Some("oauth"))
        .ok_or_else(|| {
            format!("multiprovider: stored OAuth account \"{account_id}\" is unavailable")
        })?;
    let label = str_field(account, "label");
    let credential = account["credential"].clone();
    if js_number(credential.get("expires")) <= store.now_ms() + OAUTH_REFRESH_SKEW_MS {
        let refreshed = method.refresh(credential, signal.clone()).await?;
        assert_credential(&refreshed)?;
        if refreshed["type"].as_str() != Some("oauth") {
            return Err(format!(
                "multiprovider: provider \"{provider_id}\" returned invalid OAuth credentials"
            ));
        }
        account["credential"] = refreshed.clone();
        account["updatedAt"] = Value::String(iso_date(store.now_ms()));
        store.write_state(&state).await?;
        return Ok((refreshed, label));
    }
    Ok((credential, label))
}

/// `AuthStore.resolveAccount`: api-key accounts resolve directly; OAuth accounts
/// refresh under the lock when due, persist the rotation, release the lock, and
/// only then derive request auth.
pub async fn resolve_account(
    store: &dyn AccountStore,
    provider: &dyn NativeProvider,
    account_id: &str,
    signal: &CancellationToken,
) -> Result<AuthResult, String> {
    if signal.is_cancelled() {
        return Err(abort_error());
    }
    let state = store.read_state().await?;
    let account = match find_account(&state, &provider.id(), account_id) {
        Some(account) if truthy(account.get("enabled")) => account,
        _ => {
            return Err(format!(
                "multiprovider: stored account \"{account_id}\" is unavailable"
            ))
        }
    };
    let label = str_field(account, "label");
    let credential = account["credential"].clone();
    if credential["type"].as_str() == Some("api_key") {
        let method = provider.api_key().ok_or_else(|| {
            format!(
                "multiprovider: provider \"{}\" does not support API-key auth",
                provider.id()
            )
        })?;
        let resolution = method
            .resolve(NativeInput {
                credential: Some(credential),
                signal: signal.clone(),
            })
            .await?
            .ok_or_else(|| {
                format!("multiprovider: stored account \"{label}\" did not resolve auth")
            })?;
        let tail = resolution.source.clone().unwrap_or_else(|| method.name());
        return Ok(AuthResult {
            source: Some(format!("{label} · {tail}")),
            ..resolution
        });
    }
    let method = provider.oauth().ok_or_else(|| {
        format!(
            "multiprovider: provider \"{}\" does not support OAuth",
            provider.id()
        )
    })?;
    store.lock(signal).await?;
    let locked = refresh_locked(store, provider, &*method, account_id, signal).await;
    store.unlock().await;
    let (credential, label) = locked?;
    let auth = method.to_auth(&credential).await?;
    Ok(AuthResult {
        auth,
        env: None,
        source: Some(format!("{label} · {}", method.name())),
    })
}

/// `publicPool`: the persisted pool with every stored credential removed.
fn public_pool(provider_id: &str, pool: &Value) -> Value {
    let mut view = Map::new();
    view.insert("providerId".to_owned(), json!(provider_id));
    for key in ["policy", "affinity", "includeUpstream", "upstream"] {
        if let Some(value) = pool.get(key) {
            view.insert(key.to_owned(), value.clone());
        }
    }
    let accounts = pool
        .get("accounts")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|account| {
                    let mut public = account.clone();
                    if let Some(object) = public.as_object_mut() {
                        object.remove("credential");
                    }
                    public
                })
                .collect()
        })
        .unwrap_or_default();
    view.insert("accounts".to_owned(), Value::Array(accounts));
    Value::Object(view)
}

/// `getPool`: the public view of the provider's pool, if one is stored.
pub async fn get_pool(
    store: &dyn AccountStore,
    provider_id: &str,
) -> Result<Option<Value>, String> {
    let state = store.read_state().await?;
    Ok(state["providers"]
        .get(provider_id)
        .map(|pool| public_pool(provider_id, pool)))
}

/// `hasAccounts`: whether the pool holds at least one enabled stored account.
pub async fn has_accounts(store: &dyn AccountStore, provider_id: &str) -> Result<bool, String> {
    Ok(get_pool(store, provider_id).await?.map_or(false, |pool| {
        pool.get("accounts")
            .and_then(Value::as_array)
            .map_or(false, |accounts| {
                accounts
                    .iter()
                    .any(|account| truthy(account.get("enabled")))
            })
    }))
}

/// `createManagedIntegration(...).accounts()`: the Pi default account (when the
/// pool includes it), then every stored account.
async fn list_accounts(
    store: &dyn AccountStore,
    provider_id: &str,
) -> Result<Vec<Account>, String> {
    let Some(pool) = get_pool(store, provider_id).await? else {
        return Ok(Vec::new());
    };
    let mut accounts = Vec::new();
    if truthy(pool.get("includeUpstream")) {
        let upstream = pool.get("upstream");
        accounts.push(Account {
            id: PI_UPSTREAM_ACCOUNT_ID.to_owned(),
            label: upstream
                .and_then(|value| value.get("label"))
                .and_then(Value::as_str)
                .unwrap_or(UPSTREAM_LABEL)
                .to_owned(),
            auth_kind: "custom".to_owned(),
            credential_ref: json!(PI_UPSTREAM_ACCOUNT_ID),
            enabled: None,
            weight: Some(
                upstream
                    .and_then(|value| value.get("weight"))
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0),
            ),
            priority: Some(
                upstream
                    .and_then(|value| value.get("priority"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
            ),
            metadata: Some(json!({ "source": UPSTREAM_SOURCE })),
        });
    }
    for account in pool
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = str_field(account, "id");
        accounts.push(Account {
            label: str_field(account, "label"),
            auth_kind: str_field(account, "authKind"),
            credential_ref: Value::String(id.clone()),
            id,
            enabled: account.get("enabled").and_then(Value::as_bool),
            weight: account.get("weight").and_then(Value::as_f64),
            priority: account.get("priority").and_then(Value::as_f64),
            metadata: Some(json!({ "stored": true })),
        });
    }
    Ok(accounts)
}

/// The provider-owned integration: the Pi default account plus the stored pool.
pub struct ManagedIntegration {
    provider: Arc<dyn NativeProvider>,
    store: Arc<dyn AccountStore>,
    auth: MergedAuth,
}

impl ManagedIntegration {
    pub fn new(provider: Arc<dyn NativeProvider>, store: Arc<dyn AccountStore>) -> Self {
        let auth = merge_provider_auth(&*provider, store.clone());
        Self {
            provider,
            store,
            auth,
        }
    }

    pub fn id(&self) -> String {
        self.provider.id()
    }

    pub fn label(&self) -> String {
        self.provider.name()
    }

    pub fn auth(&self) -> &MergedAuth {
        &self.auth
    }

    pub fn management_hint(&self) -> &'static str {
        MANAGEMENT_HINT
    }

    /// `Object.keys` of the integration object, in definition order.
    pub fn surface_keys(&self) -> Vec<&'static str> {
        vec![
            "id",
            "label",
            "accounts",
            "auth",
            "managementHint",
            "excludeAccountIds",
            "resolveAuth",
            "sanitizeRequestOptions",
        ]
    }

    /// Excludes the Pi default account unless the upstream marker says it is available.
    pub fn exclude_account_ids(&self, request_options: &RequestOptions) -> Vec<String> {
        if read_marker(request_options).map_or(false, |marker| marker.available) {
            Vec::new()
        } else {
            vec![PI_UPSTREAM_ACCOUNT_ID.to_owned()]
        }
    }

    pub fn sanitize_request_options(
        &self,
        account: &Account,
        resolution: &AuthResult,
        request_options: &RequestOptions,
    ) -> RequestOptions {
        sanitize_attempt(&account.id, resolution, request_options)
    }
}

#[async_trait]
impl ProviderIntegration for ManagedIntegration {
    async fn accounts(&self) -> Result<Vec<Account>, String> {
        list_accounts(&*self.store, &self.provider.id()).await
    }

    async fn resolve_auth(
        &self,
        account: &Account,
        signal: &CancellationToken,
        request: &AccountRequest,
    ) -> Result<AuthResult, String> {
        let options = &request.request_options;
        if account.id == PI_UPSTREAM_ACCOUNT_ID {
            let env = match options.get("env") {
                None => None,
                Some(Value::Object(env)) => Some(env.clone()),
                Some(Value::String(text)) => Some(
                    text.chars()
                        .enumerate()
                        .map(|(index, c)| (index.to_string(), json!(c.to_string())))
                        .collect(),
                ),
                Some(Value::Array(items)) => Some(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| (index.to_string(), item.clone()))
                        .collect(),
                ),
                Some(_) => Some(Env::new()),
            };
            return Ok(AuthResult {
                auth: ModelAuth {
                    api_key: options
                        .get("apiKey")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    ..ModelAuth::default()
                },
                env,
                source: Some(PI_DEFAULT_SOURCE.to_owned()),
            });
        }
        let mut resolution = resolve_account(
            &*self.store,
            &*self.provider,
            account.credential_ref.as_str().unwrap_or_default(),
            signal,
        )
        .await?;
        let custom_base_url = read_marker(options).map_or(false, |marker| marker.custom_base_url);
        if custom_base_url && resolution.auth.base_url.is_none() {
            if let Some(base_url) = restore_base_url(&*self.provider, &request.model.id) {
                resolution.auth.base_url = Some(base_url);
            }
        }
        Ok(resolution)
    }
}

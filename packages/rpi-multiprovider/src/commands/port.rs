//! Side effects behind the command flows: the account store, the scheduler,
//! the session, provider registrations and the host's login runner. The
//! command flows call these methods where the upstream extension reads or
//! writes its module state, Pi's session or Pi's model registry.
//!
//! The Ui helpers at the bottom turn the upstream `ctx.ui` calls into native
//! dialog controls (see `docs/presentation-map.md`). Without interactive UI
//! they behave like Pi's no-op UI: nothing is shown and no answer is read.
use super::Ui;
use serde_json::{json, Value};
use std::future::Future;

#[derive(Clone, Debug, PartialEq)]
pub struct AuthMethod {
    pub name: String,
    pub login_label: Option<String>,
    /// Whether the method has an interactive login (`method.login` in Pi).
    pub login: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderInfo {
    pub id: String,
    pub name: String,
    pub oauth: Option<AuthMethod>,
    pub api_key: Option<AuthMethod>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuthStatus {
    pub configured: bool,
    pub label: Option<String>,
    pub source: Option<String>,
}

/// Pi's session runtime as the command flows read it (`probeSessionRuntime`).
#[derive(Clone, Debug, Default)]
pub struct RuntimeView {
    pub providers: Vec<ProviderInfo>,
    /// Providers absent from this list report `configured: false`.
    pub auth: Vec<(String, AuthStatus)>,
    pub oauth: Vec<String>,
}

impl RuntimeView {
    pub fn status(&self, id: &str) -> AuthStatus {
        self.auth
            .iter()
            .find(|(candidate, _)| candidate == id)
            .map(|(_, status)| status.clone())
            .unwrap_or_default()
    }
    pub fn is_using_oauth(&self, id: &str) -> bool {
        self.oauth.iter().any(|candidate| candidate == id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectedModel {
    pub provider: String,
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PoolMethod {
    pub label: String,
    pub value: String,
}

/// The account-pool settings a new pool is created with (`buffer` in the
/// upstream extension). The pool manager may change it before a pool exists.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolBuffer {
    pub policy: String,
    pub affinity: bool,
    pub include_upstream: bool,
    pub upstream: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PoolAction {
    Add { method: String },
    Reauth { account_id: String, method: String },
    Closed,
}

/// Upstream `ProviderAuthInteraction` as the login flows drive it. Events and
/// prompts keep Pi's JSON shape (`AuthEvent`, `AuthPrompt`). A cancelled
/// prompt fails with `Login cancelled`, as Pi's prompt promise does.
pub trait LoginInteraction: Sync {
    fn notify(&self, event: Value) -> impl Future<Output = Result<(), String>> + Send;
    fn prompt(&self, prompt: Value) -> impl Future<Output = Result<String, String>> + Send;
}

pub trait Port: Send + Sync {
    /// Pi's `ctx.mode`; the TUI-only commands require `"tui"`.
    fn mode(&self) -> String;
    fn session_id(&self) -> String;
    /// `getMultiAuthPath()`, shown in the save notifications.
    fn store_path(&self) -> String;
    /// One account-store operation, in the store's request shape
    /// (`op`, `provider`, `account`, `input`, `credential`).
    fn store(&self, request: Value) -> impl Future<Output = Result<Value, String>> + Send;
    /// Re-reads the store and reapplies it to the scheduler, providers and pins.
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// `service.snapshot()`.
    fn snapshot(&self) -> impl Future<Output = Result<Value, String>> + Send;
    /// Every provider in Pi's model registry.
    fn registered_providers(&self) -> Vec<ProviderInfo>;
    /// `baseProviders` name for an installed pool, if any.
    fn installed_provider_name(&self, id: &str) -> Option<String>;
    /// `ctx.modelRegistry.getProvider(id)?.name`.
    fn provider_name(&self, id: &str) -> Option<String>;
    fn is_virtual_provider(&self, id: &str) -> bool;
    fn virtual_provider_name(&self, id: &str) -> Option<String>;
    /// Whether a virtual provider registers this scheduler id (`provider::model`).
    fn is_virtual_pool(&self, provider_id: &str, model_id: &str) -> bool;
    fn runtime(&self) -> Option<RuntimeView>;
    /// `effectiveIntegration(id) !== undefined`.
    fn has_integration(&self, provider_id: &str) -> bool;
    fn model(&self) -> Option<SelectedModel>;
    fn service_has_provider(&self, id: &str) -> bool;
    fn reset_health(&self, id: &str, account_id: &str) -> Result<(), String>;
    fn affinity(&self, pool: &str, key: &str) -> Option<(String, bool)>;
    fn pin_account(
        &self,
        pool: &str,
        key: &str,
        account_id: &str,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn clear_affinity(&self, pool: &str, key: &str);
    /// Notifies followers of the session's active account for `pool`.
    fn announce_active_account(
        &self,
        pool: &str,
    ) -> impl Future<Output = Result<(), String>> + Send;
    fn append_entry(&self, custom_type: &str, data: Value);
    /// Runs the provider's login for `auth_type` through the host and returns
    /// the credential. Failures are reported as the message of the thrown error.
    fn login<I: LoginInteraction>(
        &self,
        provider: &ProviderInfo,
        auth_type: &str,
        interaction: &I,
    ) -> impl Future<Output = Result<Value, String>> + Send;
    /// The pool manager screen (`openPoolManager`). It is owned by the pool
    /// manager lane; `/multilogin` only needs its result.
    fn pool_manager(
        &self,
        provider: &ProviderInfo,
        methods: &[PoolMethod],
        buffer: &mut PoolBuffer,
    ) -> impl Future<Output = Result<PoolAction, String>> + Send;
}

pub const CANCELLED: &str = "Login cancelled";

pub async fn notify<U: Ui>(ui: &U, text: &str, kind: &str) -> Result<(), String> {
    if !ui.has_ui() {
        return Ok(());
    }
    ui.notify(text, kind).await
}

/// `ctx.ui.select(title, labels)`: the index of the chosen label. Labels that
/// repeat resolve to their first position, as `labels.indexOf` does upstream.
pub async fn select<U: Ui>(
    ui: &U,
    title: &str,
    labels: &[String],
) -> Result<Option<usize>, String> {
    if !ui.has_ui() {
        return Ok(None);
    }
    let choices: Vec<Value> = labels
        .iter()
        .enumerate()
        .map(|(index, label)| json!({"label": label, "value": index.to_string()}))
        .collect();
    let control = json!({"kind":"select","title":title,"choices":choices});
    let index = select_control(ui, control, labels.len()).await?;
    Ok(index.and_then(|index| labels.iter().position(|label| *label == labels[index])))
}

/// Shows a select control with `count` choices and returns the index of the
/// single chosen value.
pub async fn select_control<U: Ui>(
    ui: &U,
    control: Value,
    count: usize,
) -> Result<Option<usize>, String> {
    let Some(reply) = ui.show(control).await? else {
        return Ok(None);
    };
    if reply["type"] != "select" {
        return Err("unexpected reply to a select control".into());
    }
    let values = reply["values"]
        .as_array()
        .filter(|values| values.len() == 1);
    let index = values
        .and_then(|values| values[0].as_str())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|index| *index < count)
        .ok_or_else(|| "select reply must hold one choice index".to_owned())?;
    Ok(Some(index))
}

pub async fn confirm<U: Ui>(ui: &U, title: &str, description: &str) -> Result<bool, String> {
    if !ui.has_ui() {
        return Ok(false);
    }
    let reply = ui
        .show(json!({"kind":"confirm","title":title,"description":description}))
        .await?;
    Ok(reply.is_some_and(|reply| reply["confirmed"] == true))
}

/// `ctx.ui.input(title, placeholder)`; secret prompts are masked.
pub async fn input<U: Ui>(
    ui: &U,
    title: &str,
    placeholder: &str,
    masked: bool,
) -> Result<Option<String>, String> {
    if !ui.has_ui() {
        return Ok(None);
    }
    let reply = ui
        .show(json!({"kind":"input","title":title,"placeholder":placeholder,"masked":masked}))
        .await?;
    match reply {
        None => Ok(None),
        Some(reply) if reply["type"] == "input" => Ok(reply["value"].as_str().map(str::to_owned)),
        Some(_) => Err("unexpected reply to an input control".into()),
    }
}

/// `toLocaleTimeString()` for en-US in UTC, e.g. `11:14:21 AM`.
pub fn locale_time(ms: f64) -> String {
    let seconds = (ms / 1000.0).floor() as i64;
    let day = seconds.rem_euclid(86_400);
    let (hours, minutes, secs) = (day / 3600, day % 3600 / 60, day % 60);
    let hour12 = match hours % 12 {
        0 => 12,
        hour => hour,
    };
    let meridiem = if hours < 12 { "AM" } else { "PM" };
    format!("{hour12}:{minutes:02}:{secs:02} {meridiem}")
}

/// JavaScript's number-to-string for values that reach the status lines.
pub fn js_number(value: &Value) -> String {
    match value.as_f64() {
        Some(number) if number.fract() == 0.0 && number.abs() < 1e21 => {
            format!("{}", number as i64)
        }
        Some(number) => format!("{number}"),
        None => String::new(),
    }
}

//! Port of `src/virtual.ts`: virtual model templates, one scheduler per virtual model, and backend failover.
//!
//! The stream loop follows `lift.rs` where upstream does, but keeps the virtual-specific behaviour:
//! backend resolution happens before auth, unavailable backends fail without a cause, provider retries
//! are always disabled, and the lazy wrapper turns setup failures into a terminal error event.
use crate::lift::{Failover, Response, StreamKind};
use crate::services::{Classifier, Registration, Service};
use crate::types::{Account, AcquireOptions, AttemptFailure, Disposition, FailureKind, LeaseOutcome, SelectionBias};
use futures_util::{Stream, StreamExt};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Map, Value};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const VIRTUAL_ID_SEPARATOR: &str = "::";
pub const BACKEND_UNAVAILABLE_PREFIX: &str = "multiprovider: virtual backend unavailable";
// Placeholder credential the virtual provider reports so its models pass auth checks; real auth resolves per attempt.
const VIRTUAL_PLACEHOLDER_API_KEY: &str = "virtual-provider";
const DEFAULT_MAX_ATTEMPTS: f64 = 9_007_199_254_740_991.0;

// Persisted configs are JS objects: a present `null` is distinct from an absent key, so optional
// fields keep their presence. Unknown keys are kept because healing spreads the original config.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualBackend {
    pub provider_id: String,
    pub model_id: String,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub enabled: Option<Value>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub weight: Option<Value>,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub template: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl VirtualBackend {
    // `enabled !== false`: only a literal false disables a backend.
    fn is_enabled(&self) -> bool {
        !matches!(self.enabled, Some(Value::Bool(false)))
    }
    // `weight ?? 1`.
    fn weight_or_default(&self) -> f64 {
        match &self.weight {
            None => 1.0,
            Some(value) if value.is_null() => 1.0,
            Some(value) => value.as_f64().unwrap_or(1.0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualModelConfig {
    pub id: String,
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub label: Option<Value>,
    pub backends: Vec<VirtualBackend>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VirtualProviderConfig {
    pub id: String,
    pub label: String,
    pub models: Vec<VirtualModelConfig>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl VirtualModelConfig {
    // `model.label ?? model.id`, as the display name.
    fn display_label(&self) -> Value {
        match &self.label {
            Some(value) if !value.is_null() => value.clone(),
            _ => Value::String(self.id.clone()),
        }
    }
}

pub fn virtual_scheduler_id(virtual_provider_id: &str, model_id: &str) -> String {
    format!("{virtual_provider_id}{VIRTUAL_ID_SEPARATOR}{model_id}")
}

pub fn virtual_backend_account_id(backend: &VirtualBackend) -> String {
    format!("{}{VIRTUAL_ID_SEPARATOR}{}", backend.provider_id, backend.model_id)
}

pub type ProviderLabel = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

#[derive(Default)]
pub struct IntegrationOptions {
    pub provider_label: Option<ProviderLabel>,
    // Upstream copies this onto the registration, where the scheduler never reads it.
    pub max_account_attempts: Option<f64>,
}

pub struct VirtualIntegration {
    pub registration: Registration,
    pub max_account_attempts: Option<f64>,
}

impl VirtualIntegration {
    /// A second registration over the same closures, for registering one integration twice or in several services.
    pub fn to_registration(&self) -> Registration {
        Registration {
            id: self.registration.id.clone(),
            label: self.registration.label.clone(),
            accounts: self.registration.accounts.clone(),
            classify_failure: self.registration.classify_failure.clone(),
            management_hint: self.registration.management_hint.clone(),
            selection_bias: self.registration.selection_bias,
        }
    }
}

fn transport_failure(message: &str) -> bool {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    // JavaScript `.` never matches a line terminator or half of a surrogate pair; the input is lowercased
    // ASCII-only because the upstream `i` flag does not fold non-ASCII letters onto ASCII ones.
    let unit = r"[^\r\n\u{2028}\u{2029}\u{10000}-\u{10FFFF}]";
    let pattern = PATTERN.get_or_init(|| {
        let alternatives = [
            "fetch failed".to_string(),
            "network".to_string(),
            format!("connection{unit}?(?:error|refused|reset|lost|closed)"),
            "other side closed".to_string(),
            format!("upstream{unit}?connect"),
            "reset before headers".to_string(),
            "socket hang up".to_string(),
            "socket connection was closed".to_string(),
            "econn(?:aborted|refused|reset)".to_string(),
            "enotfound".to_string(),
            "eai_again".to_string(),
            "ehostunreach".to_string(),
            "enetunreach".to_string(),
            "etimedout".to_string(),
            "und_err".to_string(),
            "getaddrinfo".to_string(),
            "timed? ?out".to_string(),
            "terminated".to_string(),
            "not configured".to_string(),
        ];
        Regex::new(&alternatives.join("|")).expect("transport pattern is valid")
    });
    pattern.is_match(&message.to_ascii_lowercase())
}

fn classify(failure: &AttemptFailure, _account: &Account) -> Result<Option<Disposition>, String> {
    if failure.message.starts_with(BACKEND_UNAVAILABLE_PREFIX) || transport_failure(&failure.message) {
        Ok(Some(Disposition {
            kind: FailureKind::Transient,
            retryable: true,
            cooldown_ms: None,
            scope: None,
        }))
    } else {
        Ok(None)
    }
}

fn account_for(backend: &VirtualBackend, provider_label: Option<&ProviderLabel>) -> Account {
    let provider_name = provider_label
        .and_then(|label| label(&backend.provider_id))
        .unwrap_or_else(|| backend.provider_id.clone());
    Account {
        id: virtual_backend_account_id(backend),
        label: format!("{provider_name} · {}", backend.model_id),
        auth_kind: "custom".into(),
        credential_ref: serde_json::to_value(backend).expect("backend serializes"),
        enabled: None,
        weight: Some(backend.weight_or_default()),
        priority: None,
        metadata: Some(json!({"virtual": true, "providerId": backend.provider_id, "modelId": backend.model_id})),
    }
}

/// One scheduler registration per virtual model. Each pool rotates with `SelectionBias::None` so sessions
/// spread across providers instead of favouring the first one.
pub fn create_virtual_integrations(config: &VirtualProviderConfig, options: &IntegrationOptions) -> Vec<VirtualIntegration> {
    config
        .models
        .iter()
        .map(|model| {
            let backends = Arc::new(model.backends.clone());
            let provider_label = options.provider_label.clone();
            let label = match model.display_label() {
                Value::String(name) => format!("{} · {name}", config.label),
                other => format!("{} · {other}", config.label),
            };
            let registration = Registration {
                id: virtual_scheduler_id(&config.id, &model.id),
                label,
                accounts: Arc::new(move || {
                    let backends = backends.clone();
                    let provider_label = provider_label.clone();
                    Box::pin(async move {
                        Ok(backends
                            .iter()
                            .filter(|backend| backend.is_enabled())
                            .map(|backend| account_for(backend, provider_label.as_ref()))
                            .collect())
                    })
                }),
                classify_failure: Some(Arc::new(classify) as Arc<Classifier>),
                management_hint: None,
                selection_bias: SelectionBias::None,
            };
            VirtualIntegration {
                registration,
                max_account_attempts: options.max_account_attempts,
            }
        })
        .collect()
}

/// Snapshot the host-facing metadata a virtual model advertises before its backing providers register.
pub fn capture_virtual_model_template(model: &Value) -> Value {
    let mut template = Map::new();
    for key in ["api", "baseUrl", "reasoning", "thinkingLevelMap", "input", "contextWindow", "maxTokens"] {
        if let Some(value) = model.get(key) {
            template.insert(key.into(), value.clone());
        }
    }
    template.insert(
        "cost".into(),
        match model.get("cost") {
            Some(Value::Object(cost)) => Value::Object(cost.clone()),
            _ => json!({}),
        },
    );
    Value::Object(template)
}

/// Fill persisted templates from live backing models. Returns `None` when nothing changed, so callers
/// persist only real healing.
pub fn heal_virtual_templates(
    config: &VirtualProviderConfig,
    mut resolve: impl FnMut(&str, &str) -> Option<Value>,
) -> Option<VirtualProviderConfig> {
    let mut healed = config.clone();
    let mut added = false;
    for model in &mut healed.models {
        for backend in &mut model.backends {
            if !backend.is_enabled() || backend.template.is_some() {
                continue;
            }
            if let Some(template) = resolve(&backend.provider_id, &backend.model_id) {
                backend.template = Some(template);
                added = true;
            }
        }
    }
    added.then_some(healed)
}

/// Result of ambient (host-registered) auth for a backing provider, resolved per attempt.
#[derive(Clone, Debug, Default)]
pub struct AmbientAuth {
    pub api_key: Option<String>,
    pub headers: Option<Map<String, Value>>,
    pub base_url: Option<String>,
    pub env: Option<Map<String, Value>>,
}

#[derive(Clone, Debug)]
pub enum AmbientResolution {
    Resolved(AmbientAuth),
    // `ok: false`: the attempt proceeds without ambient auth; upstream never reads the error text.
    Unresolved { error: String },
}

/// A backing `(provider, model)` pair chosen for one attempt.
#[derive(Clone, Debug)]
pub struct Target {
    pub provider_id: String,
    pub model: Value,
}

/// One physical stream request handed to the host.
#[derive(Clone)]
pub struct Attempt {
    pub kind: StreamKind,
    pub provider_id: String,
    pub model: Value,
    pub context: Value,
    pub options: Map<String, Value>,
    /// The caller supplied `onResponse`; the host should invoke it once the response arrives.
    pub on_response: bool,
    pub signal: CancellationToken,
}

/// Host-side operations the virtual stream loop depends on. Mirrors `lift::Host`, extended with the
/// backing-provider lookups and ambient auth that upstream injects through `VirtualProviderDependencies`.
#[async_trait::async_trait]
pub trait VirtualHost: Send + Sync {
    type Stream: Stream<Item = Result<Value, String>> + Send + Unpin;
    /// Clock for lazy-stream timestamps and durations (milliseconds).
    fn now(&self) -> f64;
    /// `getAffinityKey()`, read synchronously when a stream is created. `Err` models a throw.
    fn affinity_key(&self) -> Result<String, String>;
    /// `getBackingProvider(id)?.getModels()`: `None` when the provider is not registered.
    fn backing_models(&self, provider_id: &str) -> Option<Vec<Value>>;
    /// `isBackendConfigured`; the default is upstream's `?? true` fallback.
    fn is_backend_configured(&self, _provider_id: &str) -> bool {
        true
    }
    /// `resolveAmbientAuth`; `Err` models a throw, which fails the attempt through the setup path.
    async fn resolve_ambient_auth(
        &self,
        provider_id: &str,
        model: &Value,
        signal: &CancellationToken,
    ) -> Result<AmbientResolution, String>;
    /// Starts one physical stream. Each start resets the private response receipt before sending.
    async fn start(&self, attempt: Attempt) -> Result<Self::Stream, String>;
    fn response(&self) -> Option<Response>;
    async fn emit(&self, event: Value);
    async fn pause(&self) {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    /// `onFailover`. The default is an absent hook.
    async fn failover(&self, _: Failover<'_>) -> Result<bool, String> {
        Ok(false)
    }
}

// Terminal events are buffered until the stream can no longer rotate to another backend.
struct Terminal {
    start: Option<Value>,
    event: Value,
}

/// The lazy-stream forwarder: stamps `durationMs` on terminal messages, as the pi-ai event stream does.
struct Outer<'a, H: VirtualHost> {
    host: &'a H,
    started_at: f64,
}

impl<H: VirtualHost> Outer<'_, H> {
    async fn push(&self, mut event: Value) {
        stamp_duration(&mut event, self.started_at, self.host.now());
        self.host.emit(event).await;
    }
}

fn stamp_duration(event: &mut Value, started_at: f64, now: f64) {
    let key = match event["type"].as_str() {
        Some("done") => "message",
        Some("error") => "error",
        _ => return,
    };
    let Some(message) = event.get_mut(key).and_then(Value::as_object_mut) else {
        return;
    };
    if message.contains_key("durationMs") {
        return;
    }
    if message.get("timestamp").and_then(Value::as_f64).is_some_and(|timestamp| timestamp < started_at) {
        return;
    }
    message.insert("durationMs".into(), json!((now - started_at).round().max(0.0)));
}

// Setup failures (`createSetupErrorMessage`) become a terminal error event shaped like the virtual model.
fn setup_error_event(model: &Value, error: &str, started_at: f64) -> Value {
    json!({"type": "error", "reason": "error", "error": {
        "role": "assistant",
        "content": [],
        "api": model.get("api").cloned().unwrap_or(Value::Null),
        "provider": model.get("provider").cloned().unwrap_or(Value::Null),
        "model": model.get("id").cloned().unwrap_or(Value::Null),
        "usage": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0,
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}},
        "stopReason": "error",
        "errorMessage": error,
        "timestamp": started_at,
    }})
}

fn failure_from(
    error: Option<String>,
    response: Option<Response>,
    output_started: bool,
    assistant_message: Option<&Value>,
) -> AttemptFailure {
    AttemptFailure {
        message: assistant_message
            .and_then(|message| message.get("errorMessage"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| error.clone())
            .unwrap_or_else(|| "undefined".into()),
        status: response.as_ref().map(|response| response.status),
        headers: response.map(|response| response.headers),
        assistant_message: assistant_message.cloned(),
        cause: error.map(|message| json!({"name": "Error", "message": message})),
        output_started,
    }
}

// The response receipt is only meaningful once a physical stream has been started for this attempt.
fn current_response<H: VirtualHost>(host: &H, live: bool) -> Option<Response> {
    if live {
        host.response()
    } else {
        None
    }
}

// `mergeHeaders`: names compare case-insensitively and the override wins.
fn merge_headers(
    base: Option<&Map<String, Value>>,
    overrides: Option<&Map<String, Value>>,
) -> Option<Map<String, Value>> {
    if base.is_none() && overrides.is_none() {
        return None;
    }
    let mut merged = base.cloned().unwrap_or_default();
    for (name, value) in overrides.into_iter().flatten() {
        let lower = name.to_lowercase();
        let stale: Vec<String> = merged
            .keys()
            .filter(|existing| existing.to_lowercase() == lower)
            .cloned()
            .collect();
        for existing in stale {
            merged.remove(&existing);
        }
        merged.insert(name.clone(), value.clone());
    }
    Some(merged)
}

fn attempt_options(requested: &Map<String, Value>, ambient: &AmbientResolution) -> Map<String, Value> {
    let mut options = requested.clone();
    options.remove("apiKey");
    if let AmbientResolution::Resolved(auth) = ambient {
        if let Some(api_key) = &auth.api_key {
            options.insert("apiKey".into(), json!(api_key));
        }
        let headers = merge_headers(
            requested.get("headers").and_then(Value::as_object),
            auth.headers.as_ref(),
        );
        if let Some(headers) = headers {
            options.insert("headers".into(), Value::Object(headers));
        }
        if auth.env.is_some() || requested.get("env").is_some() {
            let mut env = requested
                .get("env")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            for (key, value) in auth.env.iter().flatten() {
                env.insert(key.clone(), value.clone());
            }
            options.insert("env".into(), Value::Object(env));
        }
    }
    options.insert("maxRetries".into(), json!(0));
    options
}

fn resolve_target<H: VirtualHost>(host: &H, backend: &VirtualBackend) -> Result<Target, String> {
    let models = host.backing_models(&backend.provider_id).ok_or_else(|| {
        format!("{BACKEND_UNAVAILABLE_PREFIX}: provider \"{}\" is not registered", backend.provider_id)
    })?;
    let model = models
        .into_iter()
        .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(backend.model_id.as_str()))
        .ok_or_else(|| {
            format!(
                "{BACKEND_UNAVAILABLE_PREFIX}: provider \"{}\" has no model \"{}\"",
                backend.provider_id, backend.model_id
            )
        })?;
    Ok(Target {
        provider_id: backend.provider_id.clone(),
        model,
    })
}

// `handOffAffinity`: after a surfaced failover, re-point an implicit affinity at the next healthy account.
async fn hand_off(service: &Service, request: &Prepared, excluded: &[String]) {
    if !service
        .pool_preference(&request.scheduler_id)
        .is_ok_and(|pool| pool.affinity)
        || service
            .affinity(&request.scheduler_id, &request.affinity_key)
            .ok()
            .flatten()
            .is_some_and(|pin| pin.explicit)
    {
        return;
    }
    if let Ok(mut lease) = service
        .acquire(AcquireOptions {
            provider_id: request.scheduler_id.clone(),
            affinity_key: Some(request.affinity_key.clone()),
            exclude_account_ids: excluded.into(),
            ..Default::default()
        })
        .await
    {
        lease.release(LeaseOutcome::Cancelled);
    }
}

async fn replay<H: VirtualHost>(outer: &Outer<'_, H>, terminal: Terminal) {
    if let Some(start) = terminal.start {
        outer.push(start).await;
    }
    outer.push(terminal.event).await;
}

// Everything `virtualStream` captures synchronously when the stream is created.
struct Prepared {
    scheduler_id: String,
    model_id: String,
    provider_id: String,
    affinity_key: String,
    errors_before_switch: f64,
    max_attempts: f64,
    kind: StreamKind,
    context: Value,
    options: Map<String, Value>,
    signal: CancellationToken,
    on_response: bool,
}

enum Flow {
    End,
    Next,
    Surface,
}

async fn run<H: VirtualHost>(service: &Service, outer: &Outer<'_, H>, request: &Prepared) -> Result<(), String> {
    let host = outer.host;
    let mut attempted: Vec<String> = Vec::new();
    let mut attempts = 0usize;
    let mut last_terminal: Option<Terminal> = None;
    let mut last_setup_error: Option<String> = None;
    while (attempts as f64) < request.max_attempts {
        let mut lease = match service
            .acquire(AcquireOptions {
                provider_id: request.scheduler_id.clone(),
                model_id: Some(request.model_id.clone()),
                affinity_key: Some(request.affinity_key.clone()),
                exclude_account_ids: attempted.clone(),
            })
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Some(terminal) = last_terminal.take() {
                    replay(outer, terminal).await;
                    return Ok(());
                }
                return Err(last_setup_error.take().unwrap_or_else(|| error.to_string()));
            }
        };
        attempts += 1;
        attempted.push(lease.account.id.clone());
        let backend: VirtualBackend = serde_json::from_value(lease.account.credential_ref.clone())
            .map_err(|error| error.to_string())?;
        let mut settled = false;
        let mut output_started = false;
        let mut live = false;
        let mut start: Option<Value> = None;
        let outcome: Result<Flow, String> = async {
            let target = match resolve_target(host, &backend) {
                Ok(target) => target,
                Err(message) => {
                    last_setup_error = Some(message.clone());
                    let disposition = lease.release(LeaseOutcome::Failure {
                        error: AttemptFailure {
                            message: message.clone(),
                            status: None,
                            headers: None,
                            assistant_message: None,
                            cause: None,
                            output_started: false,
                        },
                    });
                    settled = true;
                    if disposition.is_some_and(|value| value.retryable)
                        && (attempts as f64) < request.max_attempts
                        && !request.signal.is_cancelled()
                    {
                        return Ok(Flow::Next);
                    }
                    return Err(message);
                }
            };
            let ambient = host
                .resolve_ambient_auth(&target.provider_id, &target.model, &request.signal)
                .await?;
            let options = attempt_options(&request.options, &ambient);
            let stream_model = match &ambient {
                AmbientResolution::Resolved(AmbientAuth { base_url: Some(base_url), .. }) => {
                    let mut model = target.model.clone();
                    model["baseUrl"] = json!(base_url);
                    model
                }
                _ => target.model.clone(),
            };
            let mut same_account_errors = 0usize;
            loop {
                live = false;
                start = None;
                let mut inner = host
                    .start(Attempt {
                        kind: request.kind,
                        provider_id: target.provider_id.clone(),
                        model: stream_model.clone(),
                        context: request.context.clone(),
                        options: options.clone(),
                        on_response: request.on_response,
                        signal: request.signal.clone(),
                    })
                    .await?;
                live = true;
                let mut retry_same = false;
                while let Some(item) = inner.next().await {
                    let event = item?;
                    match event["type"].as_str() {
                        Some("start") => {
                            start = Some(event);
                            continue;
                        }
                        Some("error") => {
                            if event["reason"] == "aborted" || request.signal.is_cancelled() {
                                lease.release(LeaseOutcome::Cancelled);
                                settled = true;
                            } else {
                                let failure = failure_from(
                                    None,
                                    current_response(host, live),
                                    output_started,
                                    event.get("error"),
                                );
                                // Pre-output errors are absorbed on the same backend until the threshold.
                                if !output_started && ((same_account_errors + 1) as f64) < request.errors_before_switch {
                                    same_account_errors += 1;
                                    host.pause().await;
                                    if request.signal.is_cancelled() {
                                        lease.release(LeaseOutcome::Cancelled);
                                        settled = true;
                                        return Ok(Flow::End);
                                    }
                                    retry_same = true;
                                    break;
                                }
                                let disposition = lease.release(LeaseOutcome::Failure {
                                    error: failure.clone(),
                                });
                                settled = true;
                                if !output_started
                                    && disposition.is_some_and(|value| value.retryable)
                                    && (attempts as f64) < request.max_attempts
                                {
                                    last_terminal = Some(Terminal {
                                        start: start.take(),
                                        event,
                                    });
                                    let surface = host
                                        .failover(Failover {
                                            provider_id: &request.scheduler_id,
                                            from_account_id: &lease.account.id,
                                            failure: &failure,
                                            errors_on_account: same_account_errors + 1,
                                        })
                                        .await?;
                                    return Ok(if surface { Flow::Surface } else { Flow::Next });
                                }
                            }
                            if !output_started {
                                if let Some(value) = start.take() {
                                    outer.push(value).await;
                                }
                            }
                            outer.push(event).await;
                            return Ok(Flow::End);
                        }
                        Some("done") => {
                            lease.release(LeaseOutcome::Success);
                            settled = true;
                            if !output_started {
                                if let Some(value) = start.take() {
                                    outer.push(value).await;
                                }
                            }
                            outer.push(event).await;
                            return Ok(Flow::End);
                        }
                        _ => {
                            if !output_started {
                                output_started = true;
                                if let Some(value) = start.take() {
                                    outer.push(value).await;
                                }
                            }
                            outer.push(event).await;
                        }
                    }
                }
                if !retry_same {
                    break;
                }
            }
            let error = "Provider stream ended without a terminal event".to_string();
            last_setup_error = Some(error.clone());
            let disposition = lease.release(LeaseOutcome::Failure {
                error: failure_from(Some(error.clone()), current_response(host, live), output_started, None),
            });
            settled = true;
            if !output_started
                && disposition.is_some_and(|value| value.retryable)
                && (attempts as f64) < request.max_attempts
            {
                return Ok(Flow::Next);
            }
            Err(error)
        }
        .await;
        match outcome {
            Ok(Flow::End) => return Ok(()),
            Ok(Flow::Next) => continue,
            Ok(Flow::Surface) => {
                hand_off(service, request, &attempted).await;
                break;
            }
            Err(error) => {
                if !settled {
                    let outcome = if request.signal.is_cancelled() {
                        LeaseOutcome::Cancelled
                    } else {
                        LeaseOutcome::Failure {
                            error: failure_from(Some(error.clone()), current_response(host, live), output_started, None),
                        }
                    };
                    let disposition = lease.release(outcome);
                    if !output_started
                        && disposition.is_some_and(|value| value.retryable)
                        && (attempts as f64) < request.max_attempts
                    {
                        last_setup_error = Some(error);
                        continue;
                    }
                }
                return Err(error);
            }
        }
    }
    if let Some(terminal) = last_terminal.take() {
        replay(outer, terminal).await;
        return Ok(());
    }
    Err(last_setup_error.unwrap_or_else(|| {
        format!(
            "multiprovider: exhausted virtual backends for \"{}\"",
            request.provider_id
        )
    }))
}

/// A stream whose setup has been captured synchronously; `drive` runs the loop and emits its events.
pub struct Pending<'a, H: VirtualHost> {
    provider: &'a VirtualProvider<H>,
    model: Value,
    started_at: f64,
    prepared: Result<Prepared, String>,
}

impl<H: VirtualHost> Pending<'_, H> {
    /// Runs the stream to completion. Setup or loop failures end the stream with an error event, as the lazy wrapper does.
    pub async fn drive(self) {
        let outer = Outer {
            host: self.provider.host.as_ref(),
            started_at: self.started_at,
        };
        let result = match &self.prepared {
            Ok(request) => run(&self.provider.service, &outer, request).await,
            Err(error) => Err(error.clone()),
        };
        if let Err(error) = result {
            outer.push(setup_error_event(&self.model, &error, self.started_at)).await;
        }
    }
}

pub struct VirtualProvider<H: VirtualHost> {
    service: Service,
    config: VirtualProviderConfig,
    host: Arc<H>,
    max_account_attempts: Option<f64>,
}

impl<H: VirtualHost> VirtualProvider<H> {
    pub fn new(service: Service, config: VirtualProviderConfig, host: Arc<H>, max_account_attempts: Option<f64>) -> Self {
        Self {
            service,
            config,
            host,
            max_account_attempts,
        }
    }

    pub fn id(&self) -> &str {
        &self.config.id
    }

    pub fn label(&self) -> &str {
        &self.config.label
    }

    /// `auth.apiKey.name`.
    pub fn auth_name(&self) -> String {
        format!("{} (virtual)", self.config.label)
    }

    /// Virtual models, each templated from its first enabled backing model, else its first persisted template.
    pub fn models(&self) -> Vec<Value> {
        self.config.models.iter().map(|model| self.virtual_model(model)).collect()
    }

    fn virtual_model(&self, model: &VirtualModelConfig) -> Value {
        let mut template: Option<Value> = None;
        for backend in &model.backends {
            if !backend.is_enabled() {
                continue;
            }
            let candidate = self.host.backing_models(&backend.provider_id).and_then(|models| {
                models
                    .into_iter()
                    .find(|item| item.get("id").and_then(Value::as_str) == Some(backend.model_id.as_str()))
            });
            if let Some(candidate) = candidate {
                template = Some(candidate);
                break;
            }
        }
        // Live backings win; before they register, fall back to the template captured at backend-pick time.
        let source: Option<&Value> = template.as_ref().or_else(|| {
            model
                .backends
                .iter()
                .find(|backend| backend.is_enabled() && backend.template.is_some())
                .and_then(|backend| backend.template.as_ref())
        });
        let field = |key: &str| source.and_then(|source| source.get(key)).filter(|value| !value.is_null());
        let mut out = Map::new();
        out.insert("id".into(), json!(model.id));
        out.insert("name".into(), model.display_label());
        out.insert("provider".into(), json!(self.config.id));
        out.insert("api".into(), field("api").cloned().unwrap_or_else(|| json!("openai-completions")));
        out.insert("baseUrl".into(), field("baseUrl").cloned().unwrap_or_else(|| json!("")));
        out.insert("reasoning".into(), field("reasoning").cloned().unwrap_or_else(|| json!(false)));
        if let Some(thinking) = source.and_then(|source| source.get("thinkingLevelMap")) {
            out.insert("thinkingLevelMap".into(), thinking.clone());
        }
        out.insert("input".into(), field("input").cloned().unwrap_or_else(|| json!(["text"])));
        out.insert(
            "cost".into(),
            field("cost").cloned().unwrap_or_else(|| json!({"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0})),
        );
        out.insert("contextWindow".into(), field("contextWindow").cloned().unwrap_or_else(|| json!(128_000)));
        out.insert("maxTokens".into(), field("maxTokens").cloned().unwrap_or_else(|| json!(8_192)));
        Value::Object(out)
    }

    /// `auth.apiKey.resolve()`: `None` when no enabled backend exists or none is configured.
    pub fn resolve_auth(&self) -> Option<Value> {
        let backends: Vec<&VirtualBackend> = self
            .config
            .models
            .iter()
            .flat_map(|model| model.backends.iter())
            .filter(|backend| backend.is_enabled())
            .collect();
        if backends.is_empty() {
            return None;
        }
        let configured = backends
            .iter()
            .any(|backend| self.host.is_backend_configured(&backend.provider_id));
        if !configured {
            return None;
        }
        Some(json!({"auth": {"apiKey": VIRTUAL_PLACEHOLDER_API_KEY}, "source": "virtual provider"}))
    }

    /// Creates a stream for `model` (a value from [`models`](Self::models)). Affinity and errors-before-switch are
    /// captured here, as upstream reads them synchronously; nothing else runs until the returned stream is driven.
    pub fn open(
        &self,
        kind: StreamKind,
        model: &Value,
        context: Value,
        options: Map<String, Value>,
        signal: CancellationToken,
        on_response: bool,
    ) -> Pending<'_, H> {
        let started_at = self.host.now();
        let prepared = self.prepare(kind, model, context, options, signal, on_response);
        Pending {
            provider: self,
            model: model.clone(),
            started_at,
            prepared,
        }
    }

    fn prepare(
        &self,
        kind: StreamKind,
        model: &Value,
        context: Value,
        options: Map<String, Value>,
        signal: CancellationToken,
        on_response: bool,
    ) -> Result<Prepared, String> {
        let model_id = model.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let affinity_key = self.host.affinity_key()?;
        Ok(Prepared {
            scheduler_id: virtual_scheduler_id(&self.config.id, &model_id),
            model_id,
            provider_id: self.config.id.clone(),
            affinity_key,
            errors_before_switch: self.service.errors_before_switch(),
            max_attempts: self.max_account_attempts.unwrap_or(DEFAULT_MAX_ATTEMPTS),
            kind,
            context,
            options,
            signal,
            on_response,
        })
    }

    /// Scheduler id for one of this provider's models.
    pub fn scheduler_id(&self, model_id: &str) -> String {
        virtual_scheduler_id(&self.config.id, model_id)
    }

    /// Provider id used in the exhaustion error message.
    pub fn provider_id(&self) -> &str {
        &self.config.id
    }
}

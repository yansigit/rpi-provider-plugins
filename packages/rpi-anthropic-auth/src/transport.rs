//! The Claude Code Messages transport: one request built from Pi's transcript,
//! streamed back through [`crate::stream`].
//!
//! The wire is Anthropic Messages with Claude Code's own furniture: OAuth
//! requests carry the Claude Code betas, the `claude-cli/<version>` user agent
//! and a billing header, and Anthropic gates new models on both version signals.
//! [`crate::billing`] reconciles them here, so a stale pin cannot outlive a
//! rejection that names the floor.
use crate::prompt;
use crate::stream::{EventSink, State};
use crate::version::{self, LearnedFloor, RecoveryHintReason};
use crate::{auth, billing, convert};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

/// The provider this transport serves; the host catalog owns its models.
pub const PROVIDER: &str = "anthropic";
pub const API: &str = "anthropic-messages";
pub const BASE_URL: &str = "https://api.anthropic.com";
pub const MESSAGES_PATH: &str = "/v1/messages";

/// Betas an OAuth (Claude Pro/Max) request must advertise. Fine-grained tool
/// streaming stays out: it emits unvalidated tool JSON that Pi cannot parse.
pub const REQUIRED_BETAS: [&str; 3] = [
    "claude-code-20250219",
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
];
/// API-key requests only need extended thinking.
pub const API_KEY_BETA: &str = "interleaved-thinking-2025-05-14";
pub const USER_AGENT_PREFIX: &str = "claude-cli/";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Pi spends at most a third of the model's window unless the caller is explicit.
pub const MAX_TOKENS_DIVISOR: u64 = 3;
pub const MIN_THINKING_BUDGET: u64 = 1024;
pub const DEFAULT_THINKING_BUDGET: u64 = 10240;

const REASONING_LEVELS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// Pi reasoning levels on the adaptive-thinking API map onto Anthropic effort.
fn effort_for(level: &str) -> &'static str {
    match level {
        "minimal" | "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" => "xhigh",
        "max" => "max",
        _ => "high",
    }
}

/// Budgets for the pre-adaptive (`thinking.type = "enabled"`) models.
fn default_budget(level: &str) -> u64 {
    match level {
        "minimal" => 1024,
        "low" => 4096,
        "medium" => 10240,
        "high" => 20480,
        "xhigh" => 32000,
        _ => DEFAULT_THINKING_BUDGET,
    }
}

/// Claude 4.0–4.5 and everything older take a token budget, not an effort level.
pub fn is_legacy_thinking_model(model_id: &str) -> bool {
    static LEGACY: OnceLock<Regex> = OnceLock::new();
    let legacy = LEGACY.get_or_init(|| {
        Regex::new(r"^claude-(?:opus|sonnet|haiku)-4-[0-5](?:-|$)|^claude-[0-3][-.]")
            .expect("the legacy thinking model pattern compiles")
    });
    legacy.is_match(&model_id.to_ascii_lowercase().replace('.', "-"))
}

/// The model's own compat flag wins; otherwise the model generation decides.
pub fn uses_adaptive_thinking(model: &Value, model_id: &str) -> bool {
    if let Some(forced) = model["compat"]["anthropicMessages"]["forceAdaptiveThinking"].as_bool() {
        return forced;
    }
    !is_legacy_thinking_model(model_id)
}

/// Everything one request needs that does not change between attempts.
pub struct Prepared {
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The wire body: already rebased to `version` when that was verifiable.
    pub body: String,
    /// The Claude Code version the body's billing header carries.
    pub version: String,
    pub model: Value,
    pub oauth: bool,
    pub tools: Vec<Value>,
    settings: billing::VersionSettings,
    sync: billing::BillingVersionSync,
    floor: LearnedFloor,
}

impl Prepared {
    fn request(&self, body: String) -> Request {
        Request {
            url: self.url.clone(),
            headers: self.headers.clone(),
            body,
        }
    }

    /// Moves the billing header to `to`, or reports that the body cannot be
    /// verified against the header shaping emitted.
    fn rebase(&self, to: &str) -> Option<String> {
        self.sync.rebuild_body(&self.body, &self.version, to)
    }
}

/// One outbound request; the transport never inspects it again.
pub struct Request {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// A response body as a chunk source: the HTTP client yields bytes, tests yield
/// fixtures, and the parser cannot tell them apart.
pub trait Body {
    fn next(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

pub struct Response<B> {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: B,
}

pub trait Source {
    type Body: Body;

    fn now_ms(&self) -> i64;

    fn open(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<Response<Self::Body>, String>> + Send;
}

/// Builds the request Pi's transcript, options and credentials imply.
///
/// `env` is the request environment: the host hands the process environment
/// unless the caller replaced authentication.
pub fn prepare(
    model: &Value,
    context: &Value,
    options: &Value,
    env: &HashMap<String, String>,
    settings: &billing::VersionSettings,
    floor: &LearnedFloor,
) -> Result<Prepared, String> {
    let api_key = options["apiKey"]
        .as_str()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| {
            "No Anthropic auth available. Run /login and choose Claude Pro/Max.".to_string()
        })?;
    let oauth = prompt::is_anthropic_oauth_access_token(api_key);
    let model_id = model["id"]
        .as_str()
        .ok_or_else(|| "Anthropic request is missing a model id".to_string())?;
    let model_ref = convert::ModelRef {
        provider: model["provider"].as_str().unwrap_or(PROVIDER).to_owned(),
        api: model["api"].as_str().unwrap_or(API).to_owned(),
        id: model_id.to_owned(),
    };

    let retention = convert::resolve_cache_retention(
        requested_retention(options["cacheRetention"].as_str()),
        env.get(convert::PI_CACHE_RETENTION_ENV).map(String::as_str),
    );
    let messages = convert_pi_messages(context, oauth, &model_ref, retention)?;
    let tools = context["tools"].as_array().cloned().unwrap_or_default();

    let max_tokens = max_tokens(model, options)?;
    let mut body = json!({
        "model": model_id,
        "messages": messages,
        "max_tokens": max_tokens,
        "stream": true,
    });
    if let Some(system) = system_blocks(context, oauth) {
        body["system"] = Value::Array(system);
    }
    if let Some(tools) = converted_tools(&tools, oauth)? {
        body["tools"] = tools;
    }
    apply_thinking(&mut body, model, options, max_tokens);
    body = prompt::shape_anthropic_oauth_payload(body, api_key);
    prepend_billing_header(&mut body, settings);

    let mut sync = billing::BillingVersionSync::new(floor.clone());
    sync.record_request(&body);
    let user_agent = format!("{USER_AGENT_PREFIX}{}", settings.pinned);
    let target = sync.target_version(settings, Some(&user_agent));
    let mut prepared = Prepared {
        url: format!("{}{MESSAGES_PATH}", base_url(model)),
        headers: headers(api_key, oauth, &user_agent, model, options),
        body: body.to_string(),
        version: settings.pinned.clone(),
        model: model.clone(),
        oauth,
        tools,
        settings: settings.clone(),
        sync,
        floor: floor.clone(),
    };
    if target != prepared.version {
        if let Some(rebased) = prepared.rebase(&target) {
            prepared.body = rebased;
            prepared.version = target;
        }
    }
    Ok(prepared)
}

/// Streams the response into `sink` and returns the final message. Every failure
/// path returns that message: the host renders the error from it, and the emit
/// channel must still see a terminal event.
pub async fn run<S: Source>(
    source: &S,
    prepared: &Prepared,
    caller: CancellationToken,
    sink: &mut impl EventSink,
) -> Value {
    let started = source.now_ms();
    let mut state = State::new(&prepared.model, started);
    state.name_tools(prepared.oauth, &prepared.tools);
    let result = async {
        state.start(sink).await?;
        let response = open(source, prepared).await?;
        consume(response, &mut state, &caller, sink).await
    }
    .await;
    let duration = (source.now_ms() - started).max(0) as u64;
    if let Err(error) = result {
        let cancelled = caller.is_cancelled();
        let _ = state.fail(&error, cancelled, duration, sink).await;
    } else if caller.is_cancelled() {
        let _ = state.fail("Request aborted", true, duration, sink).await;
    } else if let Err(error) = state.finish(duration, sink).await {
        let _ = state.fail(&error, false, duration, sink).await;
    }
    state.output
}

async fn open<S: Source>(source: &S, prepared: &Prepared) -> Result<Response<S::Body>, String> {
    let response = source.open(prepared.request(prepared.body.clone())).await?;
    if is_success(response.status) {
        return Ok(response);
    }
    let status = response.status;
    let text = read_text(response).await?;
    if prepared.settings.has_override {
        return Err(rejected(status, &text, &prepared.version, prepared));
    }
    // Only a named floor can be recovered from; anything else is reported
    // as-is with the hint the user can act on.
    let Some(required) = required_version(&text) else {
        return Err(rejected(status, &text, &prepared.version, prepared));
    };
    let Some(body) = prepared.rebase(&required) else {
        return Err(rejected(status, &text, &prepared.version, prepared));
    };
    prepared.floor.learn(&required);
    let response = source.open(prepared.request(body)).await?;
    if is_success(response.status) {
        return Ok(response);
    }
    let status = response.status;
    let text = read_text(response).await?;
    Err(rejected(status, &text, &required, prepared))
}

fn required_version(text: &str) -> Option<String> {
    version::read_claude_code_version_rejection(text)?.required_version
}

/// Builds the operator-facing message: the raw body names a version the user
/// cannot act on unaided, so the reason it could not recover is appended.
fn rejected(status: u16, text: &str, sent: &str, prepared: &Prepared) -> String {
    let Some(rejection) = version::read_claude_code_version_rejection(text) else {
        return format!("Anthropic request failed: HTTP {status} {}", text.trim());
    };
    let reason = if prepared.settings.has_override {
        RecoveryHintReason::Override {
            override_version: prepared.settings.pinned.clone(),
            required_version: rejection.required_version.clone(),
        }
    } else if rejection.required_version.is_none() {
        RecoveryHintReason::UpgradePi {
            sent_version: sent.to_owned(),
        }
    } else {
        RecoveryHintReason::SetOverride {
            required_version: rejection.required_version.clone(),
        }
    };
    format!(
        "Anthropic request failed: HTTP {status} {}",
        rejection.with_hint(&version::describe_recovery_hint(&reason))
    )
}

fn is_success(status: u16) -> bool {
    (200..300).contains(&status)
}

async fn read_text<B: Body>(response: Response<B>) -> Result<String, String> {
    let Response { status, body, .. } = response;
    let mut bytes = Vec::new();
    let mut body = body;
    while let Some(chunk) = body.next().await? {
        bytes.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.is_empty() {
        return Err(format!("Anthropic request failed: HTTP {status}"));
    }
    Ok(text)
}

/// The raw SSE loop: Anthropic frames each event as `event:`/`data:` pairs.
async fn consume<B: Body>(
    response: Response<B>,
    state: &mut State,
    caller: &CancellationToken,
    sink: &mut impl EventSink,
) -> Result<(), String> {
    let Response { body, .. } = response;
    let mut body = body;
    let mut bytes: Vec<u8> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut last_event = String::new();
    loop {
        // Cancellation is checked between chunks so an idle stream still aborts.
        let chunk = tokio::select! {
            biased;
            _ = caller.cancelled() => return Err("Request aborted".into()),
            chunk = body.next() => chunk?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        bytes.extend_from_slice(&chunk);
        while let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = bytes.drain(..=newline).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !data.is_empty() {
                    let payload = data.join("\n");
                    data.clear();
                    last_event = dispatch(&payload, state, sink).await?;
                }
                continue;
            }
            if let Some(value) = line.strip_prefix("data:") {
                data.push(value.trim_start().to_owned());
            } else if let Some(value) = line.strip_prefix("event:") {
                last_event = value.trim().to_owned();
            }
        }
    }
    if !data.is_empty() {
        dispatch(&data.join("\n"), state, sink).await?;
    }
    // Bun can surface a read error while the body closes after `message_stop`;
    // the message is already complete then, so a truncated tail is not an error.
    if last_event != "message_stop" {
        return Err("Anthropic stream ended before message_stop".to_string());
    }
    Ok(())
}

async fn dispatch(
    payload: &str,
    state: &mut State,
    sink: &mut impl EventSink,
) -> Result<String, String> {
    let event: Value = serde_json::from_str(payload)
        .map_err(|error| format!("Invalid Anthropic stream event: {error}"))?;
    let kind = event["type"].as_str().unwrap_or_default().to_owned();
    match kind.as_str() {
        "message_start" => state.apply_usage(event["message"].get("usage")),
        "content_block_start" => {
            state
                .block_start(
                    event["index"].as_i64().unwrap_or_default(),
                    &event["content_block"],
                    sink,
                )
                .await?;
        }
        "content_block_delta" => {
            state
                .block_delta(
                    event["index"].as_i64().unwrap_or_default(),
                    &event["delta"],
                    sink,
                )
                .await?;
        }
        "content_block_stop" => {
            state
                .block_stop(event["index"].as_i64().unwrap_or_default(), sink)
                .await?;
        }
        "message_delta" => {
            let usage = event.get("usage").cloned();
            state.message_delta(&event["delta"], usage.as_ref());
        }
        "error" => {
            let message = event["error"]["message"]
                .as_str()
                .unwrap_or("Anthropic reported a stream error");
            return Err(format!("Anthropic stream error: {message}"));
        }
        _ => {}
    }
    Ok(kind)
}

fn convert_pi_messages(
    context: &Value,
    oauth: bool,
    model: &convert::ModelRef,
    retention: convert::CacheRetention,
) -> Result<Vec<Value>, String> {
    let messages = context["messages"]
        .as_array()
        .ok_or_else(|| "Anthropic request is missing context messages".to_string())?;
    convert::convert_pi_messages_to_anthropic(messages, oauth, model, retention)
}

fn converted_tools(tools: &[Value], oauth: bool) -> Result<Option<Value>, String> {
    if tools.is_empty() {
        return Ok(None);
    }
    Ok(Some(Value::Array(convert::convert_pi_tools_to_anthropic(
        tools, oauth,
    )?)))
}

/// The prompt-side system blocks. Pi's owned sections are removed for OAuth
/// requests only: Claude Code's own prompt must not be preceded by another
/// agent's instructions.
fn system_blocks(context: &Value, oauth: bool) -> Option<Vec<Value>> {
    let system = context["systemPrompt"].as_str().unwrap_or_default();
    if system.trim().is_empty() {
        return None;
    }
    if oauth {
        return Some(prompt::shape_system_blocks(&[json!({
            "type": "text",
            "text": system,
        })]));
    }
    Some(vec![json!({"type": "text", "text": system})])
}

/// The billing header is prepended last: it must not be shaped away, and it must
/// not consume a cache breakpoint.
fn prepend_billing_header(body: &mut Value, settings: &billing::VersionSettings) {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    if let Some(blocks) =
        billing::prepend_billing_header(body.get("system"), &messages, &settings.pinned)
    {
        body["system"] = Value::Array(blocks);
    }
}

fn requested_retention(encoded: Option<&str>) -> Option<convert::CacheRetention> {
    match encoded {
        Some("Long") => Some(convert::CacheRetention::Long),
        Some("None") => Some(convert::CacheRetention::None),
        // The host encodes its default as `Short`; only an explicit long or off
        // request outranks `PI_CACHE_RETENTION=long`.
        _ => None,
    }
}

fn max_tokens(model: &Value, options: &Value) -> Result<u64, String> {
    if let Some(requested) = options["maxTokens"].as_u64().filter(|value| *value > 0) {
        return Ok(requested);
    }
    let ceiling = model["maxTokens"]
        .as_u64()
        .ok_or_else(|| "Anthropic model is missing maxTokens".to_string())?;
    Ok((ceiling / MAX_TOKENS_DIVISOR).max(1))
}

/// Adaptive thinking takes an effort level; older models take a token budget,
/// which must leave room for the reply itself.
fn apply_thinking(body: &mut Value, model: &Value, options: &Value, max_tokens: u64) {
    let Some(level) = options["reasoning"]
        .as_str()
        .map(|value| value.to_ascii_lowercase())
    else {
        return;
    };
    if !REASONING_LEVELS.contains(&level.as_str()) || model["reasoning"] != true || max_tokens <= 1
    {
        return;
    }
    let model_id = model["id"].as_str().unwrap_or_default();
    if uses_adaptive_thinking(model, model_id) {
        let effort = model["thinkingLevelMap"][&level]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| effort_for(&level).to_owned());
        body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
        body["output_config"] = json!({"effort": effort});
        return;
    }
    let requested = options["thinkingBudgets"][&level]
        .as_u64()
        .unwrap_or_else(|| default_budget(&level));
    let budget = requested.min(max_tokens - 1);
    if budget >= MIN_THINKING_BUDGET {
        body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
    }
}

fn base_url(model: &Value) -> String {
    let base = model["baseUrl"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(BASE_URL);
    base.trim_end_matches('/').to_owned()
}

fn headers(
    api_key: &str,
    oauth: bool,
    user_agent: &str,
    model: &Value,
    options: &Value,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = vec![
        ("accept".into(), "application/json".into()),
        (
            "anthropic-dangerous-direct-browser-access".into(),
            "true".into(),
        ),
        ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ("content-type".into(), "application/json".into()),
    ];
    if oauth {
        headers.push(("authorization".into(), format!("Bearer {api_key}")));
        headers.push(("anthropic-beta".into(), REQUIRED_BETAS.join(",")));
        headers.push(("user-agent".into(), user_agent.to_owned()));
        headers.push(("x-app".into(), "cli".into()));
    } else {
        headers.push(("x-api-key".into(), api_key.to_owned()));
        headers.push(("anthropic-beta".into(), API_KEY_BETA.into()));
    }
    for source in [model.get("headers"), options.get("headers")] {
        let Some(fields) = source.and_then(Value::as_object) else {
            continue;
        };
        for (name, value) in fields {
            override_header(&mut headers, name, value, oauth);
        }
    }
    headers
}

/// Case-insensitive merge: a `null` drops the header, and an OAuth request never
/// lets an ambient credential override its own.
fn override_header(headers: &mut Vec<(String, String)>, name: &str, value: &Value, oauth: bool) {
    if oauth
        && (name.eq_ignore_ascii_case("x-api-key") || name.eq_ignore_ascii_case("authorization"))
    {
        return;
    }
    let existing = headers
        .iter()
        .position(|(key, _)| key.eq_ignore_ascii_case(name));
    match value {
        Value::Null => {
            if let Some(index) = existing {
                headers.remove(index);
            }
        }
        Value::String(value) => match existing {
            Some(index) => headers[index].1 = value.clone(),
            None => headers.push((name.to_owned(), value.clone())),
        },
        _ => {}
    }
}

/// Epoch milliseconds, matching the timestamps the host expects.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// The OAuth client header user agent the billing sync reads its version from.
pub fn user_agent(settings: &billing::VersionSettings) -> String {
    format!("{USER_AGENT_PREFIX}{}", settings.pinned)
}

/// The `Port` implementation for [`auth`] is the same HTTP client; keeping the
/// constructor here means one place builds connections.
pub fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .build()
        .map_err(|error| format!("Could not build the Anthropic HTTP client: {error}"))
}

/// The live transport: one reqwest client, opened per request.
pub struct Http {
    pub client: reqwest::Client,
}

impl Http {
    pub fn new() -> Result<Self, String> {
        Ok(Self { client: client()? })
    }
}

pub struct HttpBody {
    response: reqwest::Response,
}

impl Body for HttpBody {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.response
            .chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(|error| format!("Anthropic response stream failed: {error}"))
    }
}

impl Source for Http {
    type Body = HttpBody;

    fn now_ms(&self) -> i64 {
        now_ms()
    }

    async fn open(&self, request: Request) -> Result<Response<Self::Body>, String> {
        let mut builder = self.client.post(&request.url).body(request.body);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder.send().await.map_err(request_error)?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        Ok(Response {
            status,
            headers,
            body: HttpBody { response },
        })
    }
}

/// Network failures name the host and the reason but never the request body: it
/// carries the prompt and, on OAuth requests, the credential.
fn request_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        return "Anthropic request timed out.".to_string();
    }
    if error.is_connect() {
        return format!("Could not reach Anthropic. Check your connection. ({error})");
    }
    format!("Anthropic request failed: {error}")
}

impl auth::Port for Http {
    async fn send(&self, request: auth::TokenRequest) -> Result<auth::TokenReply, String> {
        let mut builder = self.client.request(
            reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|_| "Invalid token request method".to_string())?,
            &request.url,
        );
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(|error| format!("Token exchange failed: {error}"))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        let body = response
            .text()
            .await
            .map_err(|error| format!("Token exchange failed: {error}"))?;
        Ok(auth::TokenReply {
            status,
            headers,
            body,
        })
    }

    async fn sleep(&self, delay_ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }

    fn now_ms(&self) -> i64 {
        now_ms()
    }
}

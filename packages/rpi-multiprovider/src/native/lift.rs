//! Host-facing half of the pinned account-attempt state machine.
//!
//! The crate's [`crate::lift`] owns every retry, tolerance, failover and terminal
//! decision; this module supplies only the four host effects it needs:
//!
//! * [`NativeHost::resolve`] turns one pooled account into the host's opaque
//!   authentication receipt through the private wrapper continuation port.
//! * [`NativeHost::start`] delegates one physical attempt to the unwrapped
//!   provider and streams its frames back in emission order.
//! * [`NativeHost::response`] reports the private response receipt of that
//!   physical attempt (status and lossless headers), never an agent event.
//! * [`NativeHost::emit`] forwards ordered frames to the caller.
//!
//! The continuation protocol is the one the SDK's `register_with_factory`
//! wrapper factory drives (`resolveAuth` / delegated `version:1` streams). It is
//! spoken directly here because this crate must not take a direct `rpi-ai`
//! dependency: the accounting crate pins its SDK revision and forbids lockfile
//! changes in this lane. Wire payloads remain exactly the published version-1
//! envelopes.
use crate::lift::{Host, Request, Response};
use crate::types::Account;
use futures_util::Stream;
use rpi_plugin_sdk::{
    callbacks::OwnedString, AuthRequestControl, PluginApi, ProviderStreamControl,
    ProviderStreamEmitFn, StbString, StbStringRef,
};
use serde_json::{json, Map, Value};
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const STORE_ID: &str = "multiprovider";
pub const UPSTREAM_ACCOUNT_ID: &str = "pi:default";

/// Authentication chosen for one account attempt.
#[derive(Clone)]
pub enum Auth {
    /// The provider's own registered auth (the upstream `pi:default` account).
    Ambient,
    /// A host-issued receipt for one pooled account.
    Receipt(String),
}

/// One delegated stream: frames and the private response receipt.
pub struct AttemptStream {
    frames: mpsc::Receiver<Value>,
}

impl Stream for AttemptStream {
    type Item = Result<Value, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.frames.poll_recv(cx) {
            Poll::Ready(Some(frame)) => Poll::Ready(Some(Ok(frame))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Per-invocation host adapter. Owned by the request future; every thread it
/// starts is joined before the foreign callback returns.
pub struct NativeHost {
    api: PluginApi,
    descriptor: Value,
    signal: CancellationToken,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    emit: Option<ProviderStreamEmitFn>,
    emit_data: usize,
    response: Arc<Mutex<Option<Response>>>,
    terminal: Mutex<Option<Value>>,
    attempts: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Options copied from the wrapper request, minus reserved/private fields.
    provider_options: Map<String, Value>,
    disable_provider_retries: bool,
}

impl NativeHost {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api: PluginApi,
        descriptor: Value,
        signal: CancellationToken,
        cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
        emit: Option<ProviderStreamEmitFn>,
        emit_data: *mut std::ffi::c_void,
        provider_options: Map<String, Value>,
        disable_provider_retries: bool,
    ) -> Self {
        Self {
            api,
            descriptor,
            signal,
            cancelled,
            emit,
            emit_data: emit_data as usize,
            response: Arc::new(Mutex::new(None)),
            terminal: Mutex::new(None),
            attempts: Mutex::new(Vec::new()),
            provider_options,
            disable_provider_retries,
        }
    }

    fn cancelled(&self) -> bool {
        self.signal.is_cancelled() || (self.cancelled)()
    }

    fn forward(&self, event: &Value) -> bool {
        let Some(emit) = self.emit else {
            return true;
        };
        emit(
            StbString::from_string(event.to_string()),
            self.emit_data as *mut std::ffi::c_void,
        ) == 0
    }

    fn remember_terminal(&self, event: &Value) {
        let message = match event["type"].as_str() {
            Some("done") => event.get("message"),
            Some("error") => event.get("error"),
            _ => None,
        };
        if let Some(message) = message {
            *self.terminal.lock().unwrap() = Some(message.clone());
        }
    }

    /// The final assistant message produced by the last delegated attempt.
    pub fn terminal(&self) -> Option<Value> {
        self.terminal.lock().unwrap().clone()
    }

    /// Wait for every delegated attempt before the borrowed callback data dies.
    pub async fn finish(&self) {
        let attempts = {
            let mut attempts = self.attempts.lock().unwrap();
            std::mem::take(&mut *attempts)
        };
        for attempt in attempts {
            let _ = attempt.await;
        }
    }

    fn attempt_options(&self) -> Value {
        let mut options = self.provider_options.clone();
        // Reserved private fields belong to the continuation, not to a patch.
        for reserved in [
            "authMode",
            "responseMetadata",
            "streamKind",
            "identity",
            "apiKey",
            "headers",
            "env",
        ] {
            options.remove(reserved);
        }
        if self.disable_provider_retries {
            options.insert("maxRetries".into(), json!(0));
        }
        Value::Object(options)
    }
}

extern "C" fn auth_cancelled(context: *mut std::ffi::c_void) -> u8 {
    u8::from(unsafe { &*context.cast::<CancellationToken>() }.is_cancelled())
}

/// Resolve one pooled account through the private continuation port. The host
/// call is synchronous, so it runs on the blocking pool rather than pinning the
/// registration's async worker.
fn resolve_auth(
    api: PluginApi,
    descriptor: Value,
    account_id: String,
    env: Value,
    signal: CancellationToken,
) -> Result<String, String> {
    let control = AuthRequestControl {
        struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
        is_cancelled: auth_cancelled,
        user_data: (&signal as *const CancellationToken).cast_mut().cast(),
    };
    let Some(call) = api.provider_continuation else {
        return Err("host lacks private provider continuation operations".into());
    };
    let request = json!({
        "version":1,
        "continuation":descriptor,
        "action":"resolveAuth",
        "storeId":STORE_ID,
        "accountId":account_id,
        "options":{"env":env},
    });
    let mut out = StbString::empty();
    let status = call(
        StbStringRef::from_str(&request.to_string()),
        &control,
        &mut out,
        api.user_data,
    );
    // Continuation results are host allocations.
    let out = unsafe { OwnedString::from_raw(out, Some(api.free_string)) };
    let value: Value = serde_json::from_str(&out.to_string_lossy())
        .map_err(|_| "invalid provider continuation response".to_owned())?;
    if status != 0 {
        return Err(value["error"]
            .as_str()
            .unwrap_or("provider continuation failed")
            .to_owned());
    }
    if value["version"] != 1 {
        return Err("unsupported provider continuation version".into());
    }
    value["receipt"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "missing account authentication receipt".to_owned())
}

/// Host response headers keep every name and value, converting byte values
/// lossily but deterministically. Names stay case-insensitive-mergeable as an
/// object, matching Pi's `Record<string, string>` projection.
fn receipt_headers(receipt: &Value) -> Value {
    let mut headers = Map::new();
    if let Some(entries) = receipt["headers"].as_array() {
        for entry in entries {
            let Some(pair) = entry.as_array() else {
                continue;
            };
            let (Some(name), Some(bytes)) = (pair.first().and_then(Value::as_str), pair.get(1))
            else {
                continue;
            };
            let text = match bytes {
                Value::String(text) => text.clone(),
                Value::Array(bytes) => {
                    let bytes: Vec<u8> = bytes
                        .iter()
                        .filter_map(Value::as_u64)
                        .map(|byte| byte as u8)
                        .collect();
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                _ => continue,
            };
            headers.insert(name.to_owned(), Value::String(text));
        }
    }
    Value::Object(headers)
}

struct Sink {
    api: PluginApi,
    frames: mpsc::Sender<Value>,
    terminal: Arc<Mutex<Option<Value>>>,
    response: Arc<Mutex<Option<Response>>>,
    emit: Option<ProviderStreamEmitFn>,
    emit_data: usize,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    signal: CancellationToken,
}

// SAFETY: the sink carries the registration's opaque host context, which the
// SDK contract requires callbacks to use from any worker thread.
unsafe impl Send for Sink {}

impl Sink {
    fn forward(&self, event: &Value) -> bool {
        let Some(emit) = self.emit else {
            return true;
        };
        emit(
            StbString::from_string(event.to_string()),
            self.emit_data as *mut std::ffi::c_void,
        ) == 0
    }
}

extern "C" fn delegated_emit(chunk: StbString, data: *mut std::ffi::c_void) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if data.is_null() {
            return 2;
        }
        let sink = unsafe { &mut *data.cast::<Sink>() };
        // Delegated frames are host allocations.
        let owned = unsafe { OwnedString::from_raw(chunk, Some(sink.api.free_string)) };
        let Ok(frame) = serde_json::from_str::<Value>(&owned.to_string_lossy()) else {
            return 1;
        };
        if frame["type"] == "response" {
            let status = frame["response"]["status"].as_u64().unwrap_or_default() as u16;
            *sink.response.lock().unwrap() = Some(Response {
                status,
                headers: receipt_headers(&frame["response"]),
            });
            return if sink.forward(&frame) { 0 } else { 1 };
        }
        if (sink.cancelled)() || sink.signal.is_cancelled() {
            sink.signal.cancel();
            return 1;
        }
        let is_terminal = matches!(frame["type"].as_str(), Some("done" | "error"));
        if is_terminal {
            let message = match frame["type"].as_str() {
                Some("done") => frame.get("message"),
                _ => frame.get("error"),
            };
            if let Some(message) = message {
                *sink.terminal.lock().unwrap() = Some(message.clone());
            }
        }
        // Frames are bounded so a consumer that stops reading stops the attempt.
        match sink.frames.blocking_send(frame) {
            Ok(()) => 0,
            Err(_) => 1,
        }
    })
}

/// Owned captures for one blocking delegated attempt.
struct Attempt {
    sink: Sink,
    api: PluginApi,
    request: Value,
    signal: CancellationToken,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Attempt {
    fn run(mut self) {
        let sink = &mut self.sink;
        let api = self.api;
        let request = &self.request;
        let signal = &self.signal;
        let cancelled = &self.cancelled;
        let pointer = (sink as *mut Sink).cast::<std::ffi::c_void>();
        let control = ProviderStreamControl {
            is_cancelled: stream_cancelled,
            wait_cancelled_ms: stream_wait_cancelled,
            user_data: (signal as *const CancellationToken).cast_mut().cast(),
        };
        let mut out = StbString::empty();
        let status = match api.delegate_provider_stream {
            Some(delegate) => delegate(
                StbStringRef::from_str(&request.to_string()),
                Some(delegated_emit),
                pointer,
                &control,
                None,
                std::ptr::null_mut(),
                &mut out,
                api.user_data,
            ),
            None => 1,
        };
        let out = unsafe { OwnedString::from_raw(out, Some(api.free_string)) };
        if status == 0 {
            if let Ok(value) = serde_json::from_str::<Value>(&out.to_string_lossy()) {
                if value["role"] == "assistant" {
                    *sink.terminal.lock().unwrap() = Some(value);
                }
            }
        }
        let terminal = sink.terminal.lock().unwrap().clone();
        let event = match (status, terminal) {
            (0, Some(message)) if cancelled() || signal.is_cancelled() => {
                json!({"type":"error","reason":"aborted","error":message})
            }
            (0, Some(message)) if message["stopReason"] == "error" => {
                json!({"type":"error","reason":"error","error":message})
            }
            (0, Some(message)) if message["stopReason"] == "aborted" => {
                json!({"type":"error","reason":"aborted","error":message})
            }
            (0, Some(message)) => {
                json!({"type":"done","reason":terminal_reason(&message),"message":message})
            }
            (_, Some(message)) => json!({"type":"error","reason":"error","error":message}),
            _ => json!({"type":"error","reason":"error","error":failure_message(request)}),
        };
        if let Some(message) = event.get("message").or_else(|| event.get("error")) {
            *sink.terminal.lock().unwrap() = Some(message.clone());
        }
        let _ = sink.frames.blocking_send(event);
    }
}

#[async_trait::async_trait]
impl Host for NativeHost {
    type Auth = Auth;
    type Stream = AttemptStream;

    async fn resolve(&self, account: &Account, _: &Request) -> Result<Self::Auth, String> {
        if account.id == UPSTREAM_ACCOUNT_ID {
            return Ok(Auth::Ambient);
        }
        if self.cancelled() {
            self.signal.cancel();
            return Err("This operation was aborted".into());
        }
        let env = self
            .provider_options
            .get("env")
            .filter(|env| env.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let api = self.api;
        let descriptor = self.descriptor.clone();
        let account_id = account.id.clone();
        let signal = self.signal.clone();
        let receipt = tokio::task::spawn_blocking(move || {
            resolve_auth(api, descriptor, account_id, env, signal)
        })
        .await
        .map_err(|_| "account authentication task stopped".to_owned())??;
        Ok(Auth::Receipt(receipt))
    }

    async fn start(&self, auth: &Self::Auth, _: &Request) -> Result<Self::Stream, String> {
        if self.cancelled() {
            self.signal.cancel();
            return Err("This operation was aborted".into());
        }
        let (frames, receiver) = mpsc::channel(64);
        let sink = Sink {
            api: self.api,
            frames,
            terminal: Arc::new(Mutex::new(None)),
            response: Arc::clone(&self.response),
            emit: self.emit,
            emit_data: self.emit_data,
            cancelled: Arc::clone(&self.cancelled),
            signal: self.signal.clone(),
        };
        let mut request = json!({
            "version":1,
            "continuation":self.descriptor,
            "options":self.attempt_options(),
        });
        if let Auth::Receipt(receipt) = auth {
            request["authReceipt"] = json!(receipt);
        }
        let api = self.api;
        let signal = self.signal.clone();
        let cancelled = Arc::clone(&self.cancelled);
        let task = Attempt {
            sink,
            api,
            request,
            signal,
            cancelled,
        };
        let handle = tokio::task::spawn_blocking(move || task.run());
        self.attempts.lock().unwrap().push(handle);
        Ok(AttemptStream { frames: receiver })
    }

    fn response(&self) -> Option<Response> {
        self.response.lock().unwrap().clone()
    }

    async fn emit(&self, event: Value) -> bool {
        if self.cancelled() {
            self.signal.cancel();
            return false;
        }
        self.remember_terminal(&event);
        self.forward(&event)
    }
}

fn terminal_reason(message: &Value) -> &'static str {
    match message["stopReason"].as_str() {
        Some("length") => "length",
        Some("toolUse") => "toolUse",
        Some("deferred") => "deferred",
        _ => "stop",
    }
}

fn failure_message(request: &Value) -> Value {
    let model = &request["model"];
    json!({
        "role":"assistant",
        "content":[],
        "api":model["api"],
        "provider":model["provider"],
        "model":model["id"],
        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        "stopReason":"error",
        "errorMessage":"Provider stream ended without a terminal event",
        "timestamp":0
    })
}

extern "C" fn stream_cancelled(data: *mut std::ffi::c_void, _: u32) -> i32 {
    i32::from(unsafe { &*data.cast::<CancellationToken>() }.is_cancelled())
}

extern "C" fn stream_wait_cancelled(data: *mut std::ffi::c_void, timeout_ms: u32) -> i32 {
    let signal = unsafe { &*data.cast::<CancellationToken>() };
    if signal.is_cancelled() {
        return 1;
    }
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_millis(u64::from(timeout_ms));
    while std::time::Instant::now() < deadline {
        if signal.is_cancelled() {
            return 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    i32::from(signal.is_cancelled())
}

impl std::fmt::Debug for NativeHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeHost")
            .field("descriptor", &self.descriptor["id"])
            .finish_non_exhaustive()
    }
}

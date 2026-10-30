//! C ABI boundary: registration, the auth port, the runtime port, the owned
//! commands and the streaming provider.
//!
//! Every credential this module handles is read from a request and returned in a
//! response. The only state it keeps is the in-flight browser login and the expiry
//! of the exchange it performed, which the host's credential store cannot report.
use crate::commands::{self, Status};
use crate::stream::EventSink;
use crate::transport::{self, Http};
use crate::version::LearnedFloor;
use crate::{auth, billing, usage};
use rpi_plugin_sdk::{
    self as sdk,
    callbacks::free_string,
    provider::{native::Client, ProviderContext},
    AuthRequestControl, PluginApi, ProviderStreamControl, ProviderStreamEmitFn, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::c_void;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A browser login the user may still complete, or a `wait` poll may claim.
struct Pending {
    login: auth::Login,
    callback: Option<Arc<auth::callback::CallbackServer>>,
    /// Filled by the callback thread; `None` while the browser has not returned.
    returned: Arc<Mutex<Option<Result<String, String>>>>,
}

/// A credential this process minted. The host persists it; only this process
/// knows when it expires.
struct Minted {
    access: String,
    expires: i64,
}

struct Shared {
    settings: billing::VersionSettings,
    floor: LearnedFloor,
    http: Http,
    pending: Mutex<Option<Pending>>,
    minted: Mutex<Option<Minted>>,
}

struct Runtime {
    context: ProviderContext,
    shared: Arc<Shared>,
    client: Client,
    stopping: CancellationToken,
}

impl Runtime {
    fn new(client: Client) -> Result<Self, String> {
        let settings = billing::VersionSettings::from_env().map_err(|error| error.to_string())?;
        Ok(Self {
            context: ProviderContext::new(),
            shared: Arc::new(Shared {
                settings,
                floor: LearnedFloor::default(),
                http: Http::new()?,
                pending: Mutex::new(None),
                minted: Mutex::new(None),
            }),
            client,
            stopping: CancellationToken::new(),
        })
    }
}

fn methods() -> Value {
    json!([
        {"id": "api_key", "name": "Anthropic API key", "kind": "api_key"},
        {"id": "oauth", "name": "Claude Pro/Max subscription", "kind": "oauth"},
    ])
}

pub fn register(api: &PluginApi) -> i32 {
    let result = (|| {
        let lifecycle = api
            .register_plugin_lifecycle
            .ok_or("rpi-anthropic-auth requires plugin lifecycle support; upgrade rpi")?;
        let declare = api
            .declare
            .ok_or("rpi-anthropic-auth requires provider declarations; upgrade rpi")?;
        let stream = api
            .register_streaming_provider
            .ok_or("rpi-anthropic-auth requires streaming providers; upgrade rpi")?;
        let auth = api
            .register_provider_auth
            .ok_or("rpi-anthropic-auth requires native provider authentication; upgrade rpi")?;
        let runtime = api
            .register_provider_runtime
            .ok_or("rpi-anthropic-auth requires provider preflight; upgrade rpi")?;
        let command = api
            .register_native_command
            .ok_or("rpi-anthropic-auth requires owned native commands; upgrade rpi")?;
        let client = Client::new(api)?.scoped()?;
        // A loader may run inside a host runtime; never nest Tokio block_on on
        // that thread.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let context = ProviderContext::new();
                    let signal = CancellationToken::new();
                    let result = context.run(
                        &signal,
                        client.require_capabilities(
                            &[
                                "assistant.durationMs",
                                "json.exactNumbers",
                                "auth.publicErrors",
                                "commands.owned",
                                "ui.notifications",
                            ],
                            signal.clone(),
                        ),
                    );
                    context.finish_shutdown();
                    result
                })
                .join()
        })
        .map_err(|_| "Anthropic capability worker stopped".to_owned())???;
        let state = Box::into_raw(Box::new(Runtime::new(client)?));
        let rc = lifecycle(begin_shutdown, finish_shutdown, state.cast());
        if rc != 0 {
            unsafe {
                drop(Box::from_raw(state));
            }
            return Err("Anthropic lifecycle registration failed".into());
        }
        // The host catalog owns the Anthropic models, so the declaration carries
        // none: `models: []` leaves the built-in catalog untouched.
        let declaration = json!({"providers":[{
            "id": transport::PROVIDER,
            "models": [],
            "apiKeyEnv": "ANTHROPIC_API_KEY",
            "requiresAuth": true,
        }]});
        let registration = json!({
            "version": 1,
            "id": transport::PROVIDER,
            "displayName": "Claude Pro/Max",
            "apiKey": true,
            "oauth": true,
            "methods": methods(),
        });
        let runtime_manifest = json!({
            "version": 1,
            "id": transport::PROVIDER,
            "actions": ["preflight"],
        });
        for rc in [
            command(
                StbStringRef::from_str(
                    &json!({
                        "version": 1,
                        "name": commands::STATUS_COMMAND,
                        "description": commands::STATUS_DESCRIPTION,
                    })
                    .to_string(),
                ),
                command_request,
                free_string,
                state.cast(),
            ),
            command(
                StbStringRef::from_str(
                    &json!({
                        "version": 1,
                        "name": commands::USAGE_COMMAND,
                        "description": commands::USAGE_DESCRIPTION,
                    })
                    .to_string(),
                ),
                command_request,
                free_string,
                state.cast(),
            ),
            declare(StbStringRef::from_str(&declaration.to_string())),
            runtime(
                StbStringRef::from_str(&runtime_manifest.to_string()),
                runtime_request,
                free_string,
                state.cast(),
            ),
            auth(
                StbStringRef::from_str(&registration.to_string()),
                auth_request,
                free_string,
                state.cast(),
            ),
            stream(
                StbStringRef::from_str(transport::PROVIDER),
                StbStringRef::from_str(transport::BASE_URL),
                StbStringRef::from_str(transport::API),
                stream_request,
                free_string,
                state.cast(),
            ),
        ] {
            if rc != 0 {
                return Err("Anthropic provider registration failed".into());
            }
        }
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("rpi-anthropic-auth: {error}");
            1
        }
    }
}

extern "C" fn begin_shutdown(context: *mut c_void) {
    sdk::guard_or((), || {
        let state = unsafe { &*context.cast::<Runtime>() };
        state.stopping.cancel();
    });
}

extern "C" fn finish_shutdown(context: *mut c_void) -> i32 {
    sdk::guard_or(1, || {
        let state = unsafe { Box::from_raw(context.cast::<Runtime>()) };
        state.stopping.cancel();
        abandon_pending(&state.shared);
        state.context.finish_shutdown();
        0
    })
}

fn output(out: *mut StbString, value: Value) -> i32 {
    if out.is_null() {
        return 2;
    }
    unsafe {
        *out = StbString::from_string(value.to_string());
    }
    0
}

fn execute<T: Send + 'static>(
    state: &Runtime,
    signal: &CancellationToken,
    cancelled: impl Fn() -> bool,
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    if cancelled() {
        signal.cancel();
    }
    state.context.run(signal, async {
        let task = tokio::spawn(work);
        poll_cancel(signal, cancelled, task)
            .await
            .map_err(|_| "Anthropic operation stopped".to_owned())?
    })?
}

async fn poll_cancel<T>(
    signal: &CancellationToken,
    cancelled: impl Fn() -> bool,
    work: impl Future<Output = T>,
) -> T {
    tokio::pin!(work);
    let mut timer = tokio::time::interval(Duration::from_millis(10));
    loop {
        if cancelled() {
            signal.cancel();
        }
        tokio::select! {biased; value=&mut work=>return value, _=timer.tick()=>{}}
    }
}

/// The runtime port: only `preflight`, which decides whether this transport can
/// serve a model identity before any credential is read.
extern "C" fn runtime_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let state = unsafe { &*context.cast::<Runtime>() };
        let result = (|| {
            let value: Value = serde_json::from_str(unsafe { input.as_str() })
                .map_err(|_| "Invalid Anthropic runtime request".to_owned())?;
            if !control.is_null()
                && unsafe { control.cast::<u32>().read() }
                    < std::mem::size_of::<AuthRequestControl>() as u32
            {
                return Err("Short Anthropic cancellation control".into());
            }
            let signal = state.stopping.child_token();
            execute(
                state,
                &signal,
                || unsafe { control.as_ref() }.is_some_and(|v| (v.is_cancelled)(v.user_data) != 0),
                async move {
                    match value["action"].as_str() {
                        Some("preflight") => {
                            if value["input"]["modelId"]
                                .as_str()
                                .filter(|id| !id.trim().is_empty())
                                .is_none()
                            {
                                return Err("Invalid Anthropic model ID".into());
                            }
                            Ok(Value::Null)
                        }
                        _ => Err("Unknown Anthropic runtime operation".into()),
                    }
                },
            )
        })();
        output(
            out,
            match result {
                Ok(result) => json!({"version":1,"result":result}),
                Err(error) => json!({"version":1,"error":error}),
            },
        )
    })
}

extern "C" fn auth_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let Ok(value) = serde_json::from_str::<Value>(unsafe { input.as_str() }) else {
            return 2;
        };
        if !control.is_null()
            && unsafe { control.cast::<u32>().read() }
                < std::mem::size_of::<AuthRequestControl>() as u32
        {
            return 2;
        }
        let state = unsafe { &*context.cast::<Runtime>() };
        let signal = state.stopping.child_token();
        let cancelled = || {
            unsafe { control.as_ref() }
                .is_some_and(|control| (control.is_cancelled)(control.user_data) != 0)
        };
        let shared = state.shared.clone();
        let work_signal = signal.clone();
        let result = execute(state, &signal, cancelled, async move {
            handle_auth(shared, &value, work_signal).await
        });
        output(
            out,
            match result {
                Ok(value) => value,
                Err(error) => json!({"version":1,"kind":"error","payload":{"message":error}}),
            },
        )
    })
}

async fn handle_auth(
    shared: Arc<Shared>,
    request: &Value,
    signal: CancellationToken,
) -> Result<Value, String> {
    if signal.is_cancelled() {
        return Err("This operation was aborted".into());
    }
    let input = &request["input"];
    let client = auth::ClientConfig::environment();
    let (kind, payload) = match request["action"].as_str() {
        Some("login") if input["methodId"] == "api_key" => (
            "interaction",
            json!({
                "kind": "input",
                "title": "Enter the Anthropic API key",
                "secret": true,
                "allowEmpty": true,
                "continuation": {"method": "api_key"},
            }),
        ),
        Some("login") if input["methodId"] == "oauth" => {
            let login = auth::Login::begin(client.id.clone())?;
            let state = login.state().to_owned();
            let pending = match auth::callback::CallbackServer::bind(&state) {
                Ok(server) => {
                    let server = Arc::new(server);
                    let login = login.use_local_callback(server.redirect_uri());
                    let returned = Arc::new(Mutex::new(None));
                    let thread_returned = returned.clone();
                    let thread_server = server.clone();
                    std::thread::spawn(move || {
                        let result = thread_server
                            .wait()
                            .ok_or_else(|| "The browser did not return a callback.".to_string());
                        *thread_returned
                            .lock()
                            .unwrap_or_else(|error| error.into_inner()) = Some(result);
                    });
                    let url = login.authorize_url();
                    replace_pending(
                        &shared,
                        Pending {
                            login,
                            callback: Some(server),
                            returned,
                        },
                    );
                    return Ok(json!({
                        "version": 1,
                        "kind": "interaction",
                        "payload": {
                            "kind": "wait",
                            "message": "Finish signing in to Claude in your browser.",
                            "pollAfterMs": 2000,
                            "url": url,
                            "continuation": {"method": "oauth"},
                        },
                    }));
                }
                // A busy loopback port is not fatal: the paste flow works on its own.
                Err(_) => auth::Login::begin(client.id)?,
            };
            let url = pending.authorize_url();
            replace_pending(
                &shared,
                Pending {
                    login: pending,
                    callback: None,
                    returned: Arc::new(Mutex::new(None)),
                },
            );
            (
                "interaction",
                json!({
                    "kind": "input",
                    "title": "Paste the Claude authorization code",
                    "url": url,
                    "secret": true,
                    "allowEmpty": false,
                    "continuation": {"method": "oauth"},
                }),
            )
        }
        Some("continue") if input["continuation"]["method"] == "api_key" => {
            let key = input["value"]
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .ok_or("Invalid Anthropic API key response")?;
            let key = key.trim().to_owned();
            *shared
                .minted
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Minted {
                access: key.clone(),
                expires: 0,
            });
            ("credential", json!({"type": "api_key", "key": key}))
        }
        Some("continue") if input["continuation"]["method"] == "oauth" => {
            let pending = shared
                .pending
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                .ok_or("There is no Claude login in progress.")?;
            let callback = pending
                .returned
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let input_value = input["value"].as_str().map(str::trim).unwrap_or_default();
            let outcome = match (callback, input_value.is_empty()) {
                (Some(Ok(code)), _) => Ok(code),
                (Some(Err(error)), true) => Err(error),
                (_, true) => {
                    // Still waiting: put the login back and ask the host to poll.
                    *shared
                        .pending
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(pending);
                    return Ok(json!({
                        "version": 1,
                        "kind": "interaction",
                        "payload": {
                            "kind": "wait",
                            "message": "Waiting for the browser sign-in to finish.",
                            "pollAfterMs": 2000,
                            "continuation": {"method": "oauth"},
                        },
                    }));
                }
                (_, false) => Ok(input_value.to_owned()),
            };
            let credentials = match outcome {
                Ok(code) => {
                    let exchange = pending.login.complete(&code)?;
                    auth::exchange(&shared.http, &client, &exchange).await?
                }
                Err(error) => return Err(error),
            };
            *shared
                .minted
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Minted {
                access: credentials.access.clone(),
                expires: credentials.expires,
            });
            (
                "credential",
                json!({
                    "type": "oauth",
                    "refresh": credentials.refresh,
                    "access": credentials.access,
                    "expires": credentials.expires,
                }),
            )
        }
        Some("refresh") => {
            let credential = &input["credential"];
            let refresh_token = credential["refresh"]
                .as_str()
                .filter(|token| !token.is_empty())
                .ok_or("Invalid Anthropic refresh credential")?;
            let credentials = auth::Credentials {
                access: credential["access"].as_str().unwrap_or_default().to_owned(),
                refresh: refresh_token.to_owned(),
                expires: credential["expires"].as_i64().unwrap_or_default(),
            };
            let refreshed = auth::refresh(&shared.http, &client, &credentials).await?;
            *shared
                .minted
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(Minted {
                access: refreshed.access.clone(),
                expires: refreshed.expires,
            });
            (
                "credential",
                json!({
                    "type": "oauth",
                    "refresh": refreshed.refresh,
                    "access": refreshed.access,
                    "expires": refreshed.expires,
                }),
            )
        }
        Some("toAuth") => {
            let access = input["credential"]["access"]
                .as_str()
                .filter(|access| !access.is_empty())
                .ok_or("Invalid Anthropic access credential")?;
            if let Some(minted) = shared
                .minted
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .as_mut()
                .filter(|minted| minted.access == access)
            {
                minted.expires = input["credential"]["expires"]
                    .as_i64()
                    .filter(|expires| *expires > 0)
                    .unwrap_or(minted.expires);
            }
            ("auth", json!({"apiKey": access}))
        }
        Some("resolveApiKey") => {
            let stored = input["credential"]["key"]
                .as_str()
                .filter(|key| !key.is_empty());
            let environment = input["env"]["ANTHROPIC_API_KEY"]
                .as_str()
                .filter(|key| !key.is_empty());
            match (stored, environment) {
                (Some(key), _) => (
                    "auth",
                    json!({"apiKey": key, "source": "stored credential"}),
                ),
                (None, Some(key)) => (
                    "auth",
                    json!({"apiKey": key, "source": "ANTHROPIC_API_KEY"}),
                ),
                (None, None) => ("unavailable", Value::Null),
            }
        }
        Some("revoke") => {
            abandon_pending(&shared);
            *shared
                .minted
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
            ("success", Value::Null)
        }
        _ => return Err("Unknown Anthropic authentication operation".into()),
    };
    Ok(json!({"version": 1, "kind": kind, "payload": payload}))
}

/// A new login supersedes the abandoned one; its listener must stop waiting.
fn replace_pending(shared: &Shared, pending: Pending) {
    if let Some(replaced) = shared
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .replace(pending)
    {
        if let Some(server) = replaced.callback {
            server.cancel();
        }
    }
}

fn abandon_pending(shared: &Shared) {
    let pending = shared
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    if let Some(server) = pending.and_then(|pending| pending.callback) {
        server.cancel();
    }
}

extern "C" fn command_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let state = unsafe { &*context.cast::<Runtime>() };
        let result = (|| {
            let value: Value = serde_json::from_str(unsafe { input.as_str() })
                .map_err(|_| "Invalid Anthropic command request".to_owned())?;
            if value["version"] != 1 {
                return Err("Invalid Anthropic command request".into());
            }
            if !control.is_null()
                && unsafe { control.cast::<u32>().read() }
                    < std::mem::size_of::<AuthRequestControl>() as u32
            {
                return Err("Short Anthropic cancellation control".into());
            }
            let name = value["command"].as_str().unwrap_or_default().to_owned();
            let shared = state.shared.clone();
            let client = state.client.scoped()?;
            let signal = state.stopping.child_token();
            let work_signal = signal.clone();
            execute(
                state,
                &signal,
                || unsafe { control.as_ref() }.is_some_and(|v| (v.is_cancelled)(v.user_data) != 0),
                async move {
                    let text = match name.as_str() {
                        commands::STATUS_COMMAND => {
                            run_status(&shared, &client, work_signal).await?
                        }
                        commands::USAGE_COMMAND => run_usage(&shared, &client, work_signal).await?,
                        _ => return Err("Unknown Anthropic command".into()),
                    };
                    Ok(json!({"kind": "message", "text": text}))
                },
            )
        })();
        output(
            out,
            match result {
                Ok(result) => json!({"version":1,"result":result}),
                Err(error) => json!({"version":1,"error":error}),
            },
        )
    })
}

/// The credential the host would use for the next request, or `None` when it has
/// none: `resolve` reports presence and source, never a secret.
async fn resolve(client: &Client, signal: CancellationToken) -> Option<Value> {
    let resolved = client
        .request(
            json!({
                "version": 1,
                "port": "auth",
                "action": "resolve",
                "providerId": transport::PROVIDER,
                "env": {},
            }),
            signal,
        )
        .await
        .ok()?;
    Some(resolved)
}

async fn shaped_providers(client: &Client, signal: CancellationToken) -> Vec<String> {
    let Ok(catalog) = client
        .request(json!({"port": "providers", "action": "inspect"}), signal)
        .await
    else {
        return Vec::new();
    };
    let Some(providers) = catalog["providers"].as_array() else {
        return Vec::new();
    };
    providers
        .iter()
        .filter_map(|provider| provider["id"].as_str().or_else(|| provider.as_str()))
        .filter(|id| commands::should_shape(id))
        .map(str::to_owned)
        .collect()
}

async fn run_status(
    shared: &Shared,
    client: &Client,
    signal: CancellationToken,
) -> Result<String, String> {
    let resolved = resolve(client, signal.clone()).await;
    let access = resolved
        .as_ref()
        .and_then(|value| value["apiKey"].as_str())
        .filter(|key| !key.is_empty());
    let source = resolved.as_ref().and_then(|value| value["source"].as_str());
    let expires = access.and_then(|access| {
        shared
            .minted
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|minted| minted.access == access && minted.expires > 0)
            .map(|minted| minted.expires)
    });
    let shaped = shaped_providers(client, signal).await;
    let status = Status {
        settings: &shared.settings,
        floor: &shared.floor,
        access,
        source,
        expires,
        shaped: &shaped,
    };
    Ok(commands::status(&status, transport::now_ms()))
}

async fn run_usage(
    shared: &Shared,
    client: &Client,
    signal: CancellationToken,
) -> Result<String, String> {
    let resolved = resolve(client, signal).await;
    let access = resolved
        .as_ref()
        .and_then(|value| value["apiKey"].as_str())
        .filter(|key| !key.is_empty())
        .ok_or("Run /login and choose Claude Pro/Max to view usage.")?;
    commands::usage_report(access)?;
    usage::fetch(&shared.http.client, access).await
}

struct Sink {
    emit: Option<ProviderStreamEmitFn>,
    context: usize,
    signal: CancellationToken,
}

impl EventSink for Sink {
    async fn emit(&mut self, event: Value) -> Result<(), String> {
        if let Some(emit) = self.emit {
            if emit(
                StbString::from_string(event.to_string()),
                self.context as *mut c_void,
            ) != 0
            {
                self.signal.cancel();
                return Err("This operation was aborted".into());
            }
        }
        Ok(())
    }
}

extern "C" fn stream_request(
    input: StbStringRef,
    emit: Option<ProviderStreamEmitFn>,
    data: *mut c_void,
    control: *const ProviderStreamControl,
    _: Option<sdk::ProviderStreamCompletionFn>,
    _: *mut c_void,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let Ok(value) = serde_json::from_str::<Value>(unsafe { input.as_str() }) else {
            return 2;
        };
        let state = unsafe { &*context.cast::<Runtime>() };
        let signal = state.stopping.child_token();
        let cancelled = || {
            unsafe { control.as_ref() }
                .is_some_and(|control| (control.is_cancelled)(control.user_data, 0) != 0)
        };
        let shared = state.shared.clone();
        let work_signal = signal.clone();
        let value_for_work = value.clone();
        let data = data as usize;
        let work = async move {
            let options = &value_for_work["options"];
            // `replace` means the host authorized this request itself; the
            // process environment must not leak into it.
            let mut environment: HashMap<String, String> = if options["authMode"] == "replace" {
                HashMap::new()
            } else {
                std::env::vars().collect()
            };
            if let Some(overrides) = options["env"].as_object() {
                for (key, overridden) in overrides {
                    if let Some(overridden) = overridden.as_str() {
                        environment.insert(key.clone(), overridden.into());
                    }
                }
            }
            let prepared = transport::prepare(
                &value_for_work["model"],
                &value_for_work["context"],
                options,
                &environment,
                &shared.settings,
                &shared.floor,
            )?;
            let mut sink = Sink {
                emit,
                context: data,
                signal: work_signal.clone(),
            };
            Ok::<_, String>(transport::run(&shared.http, &prepared, work_signal, &mut sink).await)
        };
        let result = execute(state, &signal, cancelled, work);
        let message = result.unwrap_or_else(|error| {
            let mut message = crate::stream::State::new(&value["model"], 0).output;
            message["stopReason"] = if signal.is_cancelled() {
                "aborted"
            } else {
                "error"
            }
            .into();
            message["errorMessage"] = error.into();
            message["durationMs"] = 0.into();
            message
        });
        output(out, message)
    })
}

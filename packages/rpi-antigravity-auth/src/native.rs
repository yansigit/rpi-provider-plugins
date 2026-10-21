//! C ABI ownership boundary. No request credentials live in global state.
mod commands;
mod vault;
use crate::{
    auth::{self, network::Network, Admission},
    catalog, driver,
    local::{Local, Shared},
    stream::{EventSink, State},
};
use rpi_plugin_sdk::{
    self as sdk,
    callbacks::free_string,
    provider::{native::Client, ProviderContext},
    AuthRequestControl, PluginApi, ProviderStreamControl, ProviderStreamEmitFn, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::{collections::HashMap, ffi::c_void, future::Future, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

struct Runtime {
    context: ProviderContext,
    shared: Arc<Shared>,
    vault: Arc<vault::State>,
    client: Client,
    stopping: CancellationToken,
    watch: Arc<commands::Watch>,
}
impl Runtime {
    fn new(client: Client) -> Self {
        Self {
            context: ProviderContext::new(),
            shared: Arc::new(Shared::default()),
            vault: Arc::new(vault::State::default()),
            client,
            stopping: CancellationToken::new(),
            watch: Arc::new(commands::Watch::default()),
        }
    }
}
struct Gate {
    signal: CancellationToken,
    vault: Arc<vault::State>,
}
impl Admission for Gate {
    async fn check(&self) -> Result<(), String> {
        if self.signal.is_cancelled() {
            Err("This operation was aborted".into())
        } else {
            self.vault.assert_local().await
        }
    }
    fn cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }
}
fn methods() -> Value {
    json!([{"id":"api_key","name":"Gemini API key","kind":"api_key"},{"id":"oauth","name":"Google Antigravity (CortexKit)","kind":"oauth"}])
}
fn models() -> Vec<Value> {
    catalog::models()
        .iter()
        .map(|model| {
            let mut model = model.clone();
            model["provider"] = catalog::PROVIDER.into();
            model["api"] = catalog::API.into();
            model["baseUrl"] = catalog::BASE_URL.into();
            model
        })
        .collect()
}
pub fn register(api: &PluginApi) -> i32 {
    crate::transport::trust::certificates();
    let result = (|| {
        let lifecycle = api
            .register_plugin_lifecycle
            .ok_or("rpi-antigravity-auth requires plugin lifecycle support; upgrade rpi")?;
        let declare = api
            .declare
            .ok_or("rpi-antigravity-auth requires provider declarations; upgrade rpi")?;
        let stream = api
            .register_streaming_provider
            .ok_or("rpi-antigravity-auth requires streaming providers; upgrade rpi")?;
        let auth = api
            .register_provider_auth
            .ok_or("rpi-antigravity-auth requires native provider authentication; upgrade rpi")?;
        let runtime = api
            .register_provider_runtime
            .ok_or("rpi-antigravity-auth requires provider preflight; upgrade rpi")?;
        let events = api
            .register_event_result_handler
            .ok_or("rpi-antigravity-auth requires awaited lifecycle events; upgrade rpi")?;
        let command = api
            .register_native_command
            .ok_or("rpi-antigravity-auth requires owned native commands; upgrade rpi")?;
        let client = Client::new(api)?.scoped()?;
        // A loader may run inside a host runtime; never nest Tokio block_on on that thread.
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
                                "auth.providerRequest",
                                "auth.providerPolicy",
                                "auth.configuredValues",
                                "auth.dynamicMethods",
                                "auth.credentialStoreLock",
                                "providers.initialize",
                                "context.inspect",
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
        .map_err(|_| "Antigravity capability worker stopped".to_owned())???;
        let state = Box::into_raw(Box::new(Runtime::new(client)));
        let rc = lifecycle(begin_shutdown, finish_shutdown, state.cast());
        if rc != 0 {
            unsafe {
                drop(Box::from_raw(state));
            }
            return Err("Antigravity lifecycle registration failed".into());
        }
        for rc in [
            command(StbStringRef::from_str(&json!({"version":1,"name":"antigravity","description":"Google Antigravity: the account Pi serves, and the Claustrum vault"}).to_string()),commands::request,free_string,state.cast()),
            events(sdk::EventTag::SessionStart,session_start,free_string,state.cast()),
            declare(StbStringRef::from_str(&json!({"providers":[{"id":catalog::PROVIDER,"replaceCatalog":true,"models":models(),"apiKeyEnv":"GEMINI_API_KEY","requiresAuth":true}]}).to_string())),
            runtime(StbStringRef::from_str(&json!({"version":1,"id":catalog::PROVIDER,"actions":["preflight","authMethods","initialize","credentialStore","requestAuth"]}).to_string()),runtime_request,free_string,state.cast()),
            auth(StbStringRef::from_str(&json!({"version":1,"id":catalog::PROVIDER,"displayName":"Google Antigravity (CortexKit)","apiKey":true,"oauth":true,"methods":methods()}).to_string()),auth_request,free_string,state.cast()),
            stream(StbStringRef::from_str(catalog::PROVIDER),StbStringRef::from_str(catalog::BASE_URL),StbStringRef::from_str(catalog::API),stream_request,free_string,state.cast()),
        ] {
            if rc!=0 { return Err("Antigravity provider registration failed".into()); }
        }
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("rpi-antigravity-auth: {error}");
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
        let _ = state.context.run(&CancellationToken::new(), async {
            state.watch.finish().await;
            state.vault.close().await;
        });
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
            .map_err(|_| "Antigravity operation stopped".to_owned())?
    })?
}
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
                .map_err(|_| "Invalid Antigravity runtime request".to_owned())?;
            if !control.is_null()
                && unsafe { control.cast::<u32>().read() }
                    < std::mem::size_of::<AuthRequestControl>() as u32
            {
                return Err("Short Antigravity cancellation control".into());
            }
            // This callback is already admitted under the host's lock, including during shutdown.
            if value["action"] == "credentialStore" {
                return state.vault.transaction(&value["input"]);
            }
            let vault = state.vault.clone();
            let client = state.client.scoped()?;
            let signal = state.stopping.child_token();
            let work_signal = signal.clone();
            execute(
                state,
                &signal,
                || unsafe { control.as_ref() }.is_some_and(|v| (v.is_cancelled)(v.user_data) != 0),
                async move {
                    if work_signal.is_cancelled() {
                        return Err("This operation was aborted".into());
                    }
                    match value["action"].as_str() {
                        Some("preflight") => {
                            let id = value["input"]["modelId"]
                                .as_str()
                                .ok_or("Invalid Antigravity model ID")?;
                            if catalog::is_retired(id) {
                                return Err(catalog::RETIRED_MESSAGE.into());
                            }
                            Ok(Value::Null)
                        }
                        Some("initialize") => {
                            vault.initialize(std::path::Path::new(
                                value["input"]["agentDir"]
                                    .as_str()
                                    .ok_or("Missing native agent directory")?,
                            ))?;
                            let _ = vault.synchronize(&client, work_signal).await;
                            Ok(Value::Null)
                        }
                        Some("authMethods") => Ok(if matches!(vault.mode(), vault::Mode::Local) {
                            methods()
                        } else {
                            json!([methods()[0].clone()])
                        }),
                        Some("requestAuth") => Ok(if matches!(vault.mode(), vault::Mode::Local) {
                            Value::Null
                        } else {
                            json!({"resolve":{"apiKey":true,"oauth":false,"fallbackApiKey":crate::vault::source::MARKER}})
                        }),
                        _ => Err("Unknown Antigravity runtime operation".into()),
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
extern "C" fn session_start(
    _: StbStringRef,
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
            if !control.is_null()
                && unsafe { control.cast::<u32>().read() }
                    < std::mem::size_of::<AuthRequestControl>() as u32
            {
                return Err("Short Antigravity cancellation control".into());
            }
            let client = state.client.scoped()?;
            let vault = state.vault.clone();
            let signal = state.stopping.child_token();
            let work_signal = signal.clone();
            execute(
                state,
                &signal,
                || unsafe { control.as_ref() }.is_some_and(|v| (v.is_cancelled)(v.user_data) != 0),
                async move {
                    let context = client
                        .request(
                            json!({"port":"context","action":"inspect"}),
                            work_signal.clone(),
                        )
                        .await?;
                    if context["owner"]["child"] != true {
                        vault.synchronize(&client, work_signal).await?;
                    }
                    Ok(Value::Null)
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
        let vault = state.vault.clone();
        let work_signal = signal.clone();
        let result = execute(state, &signal, cancelled, async move {
            handle_auth(shared, vault, &value, work_signal).await
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
    vault: Arc<vault::State>,
    request: &Value,
    signal: CancellationToken,
) -> Result<Value, String> {
    let input = &request["input"];
    let gate = Gate {
        signal: signal.clone(),
        vault,
    };
    if signal.is_cancelled() {
        return Err("This operation was aborted".into());
    }
    if request["action"] == "refresh"
        || (request["action"] == "login" && input["methodId"] == "oauth")
        || (request["action"] == "continue" && input["continuation"]["method"] == "oauth")
    {
        gate.check().await?;
    }
    let (kind, payload) = match request["action"].as_str() {
        Some("login") if input["methodId"] == "api_key" => (
            "interaction",
            json!({"kind":"input","title":"Enter Gemini API key","secret":true,"allowEmpty":true,"continuation":{"method":"api_key"}}),
        ),
        Some("login") if input["methodId"] == "oauth" => {
            let authorization = auth::authorize("")?;
            (
                "interaction",
                json!({"kind":"input","title":"Paste the Antigravity OAuth callback URL or code:","url":authorization.url,"allowEmpty":true,"continuation":{"method":"oauth","url":authorization.url}}),
            )
        }
        Some("continue") if input["continuation"]["method"] == "api_key" => {
            let key = input["value"]
                .as_str()
                .ok_or("Invalid Gemini API key response")?;
            ("credential", json!({"type":"api_key","key":key}))
        }
        Some("continue") if input["continuation"]["method"] == "oauth" => {
            let value = input["value"]
                .as_str()
                .ok_or("Invalid Antigravity OAuth callback")?;
            let url = input["continuation"]["url"]
                .as_str()
                .ok_or("Invalid Antigravity OAuth continuation")?;
            let (code, state) = auth::callback_input(value, url);
            let port = Network::new(std::env::vars().collect(), signal)?;
            let result = auth::exchange(&port, &gate, &code, &state).await;
            if result["type"] != "success" {
                return Err(format!(
                    "Antigravity OAuth exchange failed: {}",
                    result["error"].as_str().unwrap_or_default()
                ));
            }
            (
                "credential",
                json!({"type":"oauth","refresh":result["refresh"],"access":result["access"],"expires":result["expires"]}),
            )
        }
        Some("refresh") => {
            let refresh = input["credential"]["refresh"]
                .as_str()
                .ok_or("Invalid Antigravity refresh credential")?;
            let port = Network::new(std::env::vars().collect(), signal)?;
            ("credential", auth::refresh(&port, refresh).await?)
        }
        Some("toAuth") => {
            let access = input["credential"]["access"]
                .as_str()
                .ok_or("Invalid Antigravity access credential")?;
            let refresh = input["credential"]["refresh"].as_str().unwrap_or_default();
            shared.credentials.lock().unwrap().remember(access, refresh);
            ("auth", json!({"apiKey":access}))
        }
        Some("resolveApiKey") => {
            if let Some(key) = input["credential"]["key"]
                .as_str()
                .filter(|key| !key.is_empty())
            {
                (
                    "auth",
                    json!({"apiKey":key,"env":input["credential"].get("env").cloned().unwrap_or_else(||json!({})),"source":"stored credential"}),
                )
            } else if let Some(key) = input["env"]["GEMINI_API_KEY"]
                .as_str()
                .filter(|key| !key.is_empty())
            {
                ("auth", json!({"apiKey":key,"source":"GEMINI_API_KEY"}))
            } else {
                ("unavailable", Value::Null)
            }
        }
        Some("revoke") => ("success", Value::Null),
        _ => return Err("Unknown Antigravity authentication operation".into()),
    };
    Ok(json!({"version":1,"kind":kind,"payload":payload}))
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
        let vault = state.vault.clone();
        let stopping = state.stopping.clone();
        let work_signal = signal.clone();
        let value_for_work = value.clone();
        let data = data as usize;
        let work = async move {
            let value = value_for_work;
            let signal = work_signal;
            let options = &value["options"];
            let mut environment: HashMap<String, String> = if options["authMode"] == "replace" {
                HashMap::new()
            } else {
                std::env::vars().collect()
            };
            if let Some(overrides) = options["env"].as_object() {
                for (key, value) in overrides {
                    if let Some(value) = value.as_str() {
                        environment.insert(key.clone(), value.into());
                    }
                }
            }
            let port = Arc::new(Network::new(environment, stopping)?);
            let mut sink = Sink {
                emit,
                context: data,
                signal: signal.clone(),
            };
            let mode = vault.mode();
            let message = if matches!(mode, vault::Mode::Local) {
                let gate = Gate {
                    signal: signal.clone(),
                    vault,
                };
                let source = Local {
                    shared: &shared,
                    port,
                    admission: &gate,
                };
                driver::run(
                    &source,
                    &value["model"],
                    &value["context"],
                    options,
                    signal.clone(),
                    options["responseMetadata"] == true,
                    &mut sink,
                )
                .await
            } else {
                let instance = vault.vault()?;
                let refusal = match &mode {
                    vault::Mode::Refusing(reason) => Some(reason.as_str()),
                    _ => None,
                };
                let source = crate::vault::stream::Stream {
                    shared: &shared,
                    port,
                    vault: &instance,
                    refusal,
                };
                driver::run(
                    &source,
                    &value["model"],
                    &value["context"],
                    options,
                    signal.clone(),
                    options["responseMetadata"] == true,
                    &mut sink,
                )
                .await
            };
            Ok::<_, String>(message)
        };
        let result = execute(state, &signal, cancelled, work);
        let message = result.unwrap_or_else(|error| {
            let mut message = State::new(&value["model"], 0).output;
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

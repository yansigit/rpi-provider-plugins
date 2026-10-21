//! C ABI ownership boundary. No request credentials live in global state.
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
    shared: Shared,
    stopping: CancellationToken,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            context: ProviderContext::new(),
            shared: Shared::default(),
            stopping: CancellationToken::new(),
        }
    }
}
struct Gate(CancellationToken);
impl Admission for Gate {
    async fn check(&self) -> Result<(), String> {
        if self.0.is_cancelled() {
            Err("This operation was aborted".into())
        } else {
            Ok(())
        }
    }
    fn cancelled(&self) -> bool {
        self.0.is_cancelled()
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
        let state = Box::into_raw(Box::new(Runtime::default()));
        let rc = lifecycle(begin_shutdown, finish_shutdown, state.cast());
        if rc != 0 {
            unsafe {
                drop(Box::from_raw(state));
            }
            return Err("Antigravity lifecycle registration failed".into());
        }
        for rc in [
            declare(StbStringRef::from_str(&json!({"providers":[{"id":catalog::PROVIDER,"replaceCatalog":true,"models":models(),"apiKeyEnv":"GEMINI_API_KEY","requiresAuth":true}]}).to_string())),
            runtime(StbStringRef::from_str(&json!({"version":1,"id":catalog::PROVIDER,"actions":["preflight","authMethods"]}).to_string()),runtime_request,free_string,state.cast()),
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
        state.context.begin_shutdown();
    });
}
extern "C" fn finish_shutdown(context: *mut c_void) -> i32 {
    sdk::guard_or(1, || {
        let state = unsafe { Box::from_raw(context.cast::<Runtime>()) };
        state.stopping.cancel();
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
extern "C" fn runtime_request(
    input: StbStringRef,
    _: *const AuthRequestControl,
    out: *mut StbString,
    _: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        let result = (|| {
            let value: Value = serde_json::from_str(unsafe { input.as_str() })
                .map_err(|_| "Invalid Antigravity runtime request")?;
            match value["action"].as_str() {
                Some("preflight") => {
                    let id = value["input"]["modelId"]
                        .as_str()
                        .ok_or("Invalid Antigravity model ID")?;
                    if catalog::is_retired(id) {
                        return Err(catalog::RETIRED_MESSAGE);
                    }
                    Ok(Value::Null)
                }
                Some("authMethods") => Ok(methods()),
                _ => Err("Unknown Antigravity runtime operation"),
            }
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
        let result = state
            .context
            .run(
                &signal,
                poll_cancel(
                    &signal,
                    cancelled,
                    handle_auth(state, &value, signal.clone()),
                ),
            )
            .and_then(|result| result);
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
    state: &Runtime,
    request: &Value,
    signal: CancellationToken,
) -> Result<Value, String> {
    let input = &request["input"];
    let gate = Gate(signal.clone());
    gate.check().await?;
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
            state
                .shared
                .credentials
                .lock()
                .unwrap()
                .remember(access, refresh);
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
        let work = async {
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
            let port = Arc::new(Network::new(environment, state.stopping.clone())?);
            let gate = Gate(signal.clone());
            let source = Local {
                shared: &state.shared,
                port,
                admission: &gate,
            };
            let mut sink = Sink {
                emit,
                context: data as usize,
                signal: signal.clone(),
            };
            Ok::<_, String>(
                driver::run(
                    &source,
                    &value["model"],
                    &value["context"],
                    options,
                    signal.clone(),
                    options["responseMetadata"] == true,
                    &mut sink,
                )
                .await,
            )
        };
        let result = state
            .context
            .run(&signal, poll_cancel(&signal, cancelled, work))
            .and_then(|result| result);
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

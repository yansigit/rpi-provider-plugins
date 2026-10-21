//! Native registrations own the retained worker and every foreign callback.
mod state;
mod tools;
use rpi_plugin_sdk::{
    self as sdk,
    callbacks::{free_string, Output},
    provider::native::Client,
    AuthRequestControl, EventTag, PluginApi, ProviderStreamControl, ProviderStreamEmitFn,
    StbString, StbStringRef,
};
use serde_json::{json, Value};
use state::Runtime;
use std::{ffi::c_void, sync::Arc};

struct Registration {
    runtime: Arc<Runtime>,
    tools: Vec<Box<tools::Definition>>,
}
fn methods() -> Value {
    json!([{"id":"api_key","name":"Cursor API key","kind":"api_key"}])
}
const EVENTS: &[(EventTag, &str)] = &[
    (EventTag::ProjectTrust, "project_trust"),
    (EventTag::SessionStart, "session_start"),
    (EventTag::SessionInfoChanged, "session_info_changed"),
    (EventTag::SessionBeforeCompact, "session_before_compact"),
    (EventTag::SessionCompact, "session_compact"),
    (EventTag::SessionCompactFailed, "session_compact_failed"),
    (EventTag::SessionShutdown, "session_shutdown"),
    (EventTag::SessionBeforeTree, "session_before_tree"),
    (EventTag::SessionTree, "session_tree"),
    (EventTag::BeforeAgentStart, "before_agent_start"),
    (EventTag::ModelSelect, "model_select"),
    (EventTag::TurnStart, "turn_start"),
    (EventTag::TurnEnd, "turn_end"),
    (EventTag::MessageEnd, "message_end"),
    (EventTag::ToolCall, "tool_call"),
    (EventTag::ToolResult, "tool_result"),
    (EventTag::AgentEnd, "agent_end"),
];

pub fn register(api: &PluginApi) -> i32 {
    let result = (|| {
        let lifecycle = api
            .register_plugin_lifecycle
            .ok_or("Cursor requires native lifecycle support")?;
        let declare = api.declare.ok_or("Cursor requires provider declarations")?;
        let provider = api
            .register_streaming_provider
            .ok_or("Cursor requires native streaming providers")?;
        let auth = api
            .register_provider_auth
            .ok_or("Cursor requires provider-owned authentication")?;
        let runtime = api
            .register_provider_runtime
            .ok_or("Cursor requires native catalog refresh")?;
        let events = api
            .register_event_result_handler
            .ok_or("Cursor requires awaited result events")?;
        let command = api
            .register_native_command
            .ok_or("Cursor requires owned native commands")?;
        let flag = api.register_flag.ok_or("Cursor requires extension flags")?;
        api.register_owned_tool_with_wake
            .ok_or("Cursor requires owned wake-driven tools")?;
        let client = Client::new(api)?.scoped()?;
        let entry = crate::installed::worker_entry()?;
        let state = std::thread::scope(|scope| scope.spawn(|| Runtime::open(client, entry)).join())
            .map_err(|_| "Cursor initialization thread stopped")??;
        let manifest = state.manifest.clone();
        let registration = Box::into_raw(Box::new(Registration {
            runtime: state,
            tools: Vec::new(),
        }));
        if lifecycle(begin_shutdown, finish_shutdown, registration.cast()) != 0 {
            std::thread::scope(|scope| {
                let address = registration as usize;
                scope
                    .spawn(move || {
                        finish_shutdown(address as *mut c_void);
                    })
                    .join()
            })
            .map_err(|_| "Cursor cleanup thread stopped")?;
            return Err("Cursor lifecycle registration failed".into());
        }
        let check = |status| {
            if status == 0 {
                Ok(())
            } else {
                Err("Cursor native registration failed".to_owned())
            }
        };
        check(declare(StbStringRef::from_str(&json!({"providers":[{"id":"cursor","models":manifest["provider"]["models"],"apiKeyEnv":"CURSOR_API_KEY","requiresAuth":true}]}).to_string())))?;
        check(auth(StbStringRef::from_str(&json!({"version":1,"id":"cursor","displayName":"Cursor","apiKey":true,"oauth":false,"methods":methods()}).to_string()), auth_request, free_string, registration.cast()))?;
        check(runtime(
            StbStringRef::from_str(
                r#"{"version":1,"id":"cursor","actions":["refreshCatalog","authMethods"]}"#,
            ),
            runtime_request,
            free_string,
            registration.cast(),
        ))?;
        check(provider(
            StbStringRef::from_str("cursor"),
            StbStringRef::from_str("https://cursor.com"),
            StbStringRef::from_str("cursor-sdk"),
            stream_request,
            free_string,
            registration.cast(),
        ))?;
        for item in manifest["commands"]
            .as_array()
            .ok_or("Invalid Cursor command manifest")?
        {
            check(command(
                StbStringRef::from_str(
                    &json!({"version":1,"name":item["name"],"description":item["description"]})
                        .to_string(),
                ),
                command_request,
                free_string,
                registration.cast(),
            ))?;
        }
        for item in manifest["flags"]
            .as_array()
            .ok_or("Invalid Cursor flag manifest")?
        {
            check(flag(
                StbStringRef::from_str(item["name"].as_str().ok_or("Invalid Cursor flag name")?),
                StbStringRef::from_str(
                    item["description"]
                        .as_str()
                        .ok_or("Invalid Cursor flag description")?,
                ),
            ))?;
        }
        let mut tags = Vec::new();
        for name in manifest["events"]
            .as_array()
            .ok_or("Invalid Cursor event manifest")?
        {
            let name = name.as_str().ok_or("Invalid Cursor event name")?;
            // Native receipts and trust provenance are supplied privately by the host boundary.
            if matches!(name, "before_provider_headers" | "project_trust") {
                continue;
            }
            tags.push(
                EVENTS
                    .iter()
                    .find(|(_, known)| *known == name)
                    .ok_or("Cursor event has no native mapping")?
                    .0,
            );
        }
        if !tags.contains(&EventTag::AgentEnd) {
            tags.push(EventTag::AgentEnd);
        }
        for tag in tags {
            check(events(tag, event_request, free_string, registration.cast()))?;
        }
        for item in manifest["tools"]
            .as_array()
            .ok_or("Invalid Cursor tool manifest")?
        {
            let registration = unsafe { &mut *registration };
            let definition = Box::new(tools::Definition::new(&registration.runtime, item)?);
            check(definition.register(api, item))?;
            registration.tools.push(definition);
        }
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("rpi-cursor-sdk: {error}");
            1
        }
    }
}
extern "C" fn begin_shutdown(data: *mut c_void) {
    sdk::guard_or((), || {
        if let Some(registration) = unsafe { data.cast::<Registration>().as_ref() } {
            registration.runtime.stopping.cancel();
        }
    });
}
extern "C" fn finish_shutdown(data: *mut c_void) -> i32 {
    sdk::guard_or(1, || {
        if data.is_null() {
            return 2;
        }
        let registration = unsafe { Box::from_raw(data.cast::<Registration>()) };
        if let Err(error) = registration.runtime.close() {
            eprintln!("rpi-cursor-sdk shutdown: {error}");
        }
        registration.runtime.context.finish_shutdown();
        0
    })
}
fn request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    data: *mut c_void,
    body: impl FnOnce(&Runtime, Value, &dyn Fn() -> bool) -> Result<Value, String>,
    auth: bool,
) -> i32 {
    sdk::guard_or(1, || {
        if out.is_null() || data.is_null() {
            return 2;
        }
        if !control.is_null()
            && unsafe { control.cast::<u32>().read() }
                < std::mem::size_of::<AuthRequestControl>() as u32
        {
            return 2;
        }
        let registration = unsafe { &*data.cast::<Registration>() };
        let cancelled = || {
            unsafe { control.as_ref() }
                .is_some_and(|control| (control.is_cancelled)(control.user_data) != 0)
        };
        let value = serde_json::from_str::<Value>(unsafe { input.as_str() })
            .map_err(|_| "Invalid native Cursor request".into())
            .and_then(|input| {
                if input["version"] == 1 {
                    body(&registration.runtime, input, &cancelled)
                } else {
                    Err("Unsupported Cursor request version".into())
                }
            });
        let response = match value {
            Ok(value) if auth => value,
            Ok(value) => json!({"version":1,"result":value}),
            Err(error) if auth => json!({"version":1,"kind":"error","payload":{"message":error}}),
            Err(error) => json!({"version":1,"error":error}),
        };
        unsafe { Output::from_raw(out) }.write(response.to_string());
        0
    })
}
extern "C" fn auth_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    data: *mut c_void,
) -> i32 {
    request(
        input,
        control,
        out,
        data,
        |runtime, input, cancelled| {
            runtime.invoke(
                runtime.client.scoped()?,
                "provider.auth",
                json!({"action":input["action"],"input":input["input"]}),
                &cancelled,
                |_, _| Ok(()),
            )
        },
        true,
    )
}
extern "C" fn runtime_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    data: *mut c_void,
) -> i32 {
    request(
        input,
        control,
        out,
        data,
        |runtime, input, cancelled| match input["action"].as_str() {
            Some("authMethods") => Ok(methods()),
            Some("refreshCatalog") => runtime.invoke(
                runtime.client.scoped()?,
                "provider.catalog",
                input["input"].clone(),
                &cancelled,
                |_, _| Ok(()),
            ),
            _ => Err("Unsupported Cursor runtime request".into()),
        },
        false,
    )
}
extern "C" fn command_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    data: *mut c_void,
) -> i32 {
    request(
        input,
        control,
        out,
        data,
        |runtime, input, cancelled| {
            runtime.invoke(
                runtime.client.scoped()?,
                "extension.command",
                json!({"name":input["command"],"args":input["args"]}),
                &cancelled,
                |_, _| Ok(()),
            )
        },
        false,
    )
}
extern "C" fn event_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    data: *mut c_void,
) -> i32 {
    request(
        input,
        control,
        out,
        data,
        |runtime, input, cancelled| {
            let (tag, name) = EVENTS
                .iter()
                .find(|(tag, _)| input["tag"].as_u64() == Some(*tag as u64))
                .ok_or("Unknown Cursor event")?;
            let client = runtime.client.scoped()?;
            let mut event = input["event"]
                .as_object()
                .ok_or("Invalid Cursor event payload")?
                .clone();
            event.insert("type".into(), (*name).into());
            let result = if *tag == EventTag::AgentEnd {
                Value::Null
            } else {
                runtime.invoke(
                    client.clone(),
                    "extension.event",
                    json!({"event":event}),
                    &cancelled,
                    |_, _| Ok(()),
                )?
            };
            if matches!(
                tag,
                EventTag::AgentEnd
                    | EventTag::SessionCompact
                    | EventTag::SessionCompactFailed
                    | EventTag::SessionTree
            ) {
                if let Some(id) = input["identity"]["operationId"].as_str() {
                    runtime.invoke(
                        client,
                        "operation.end",
                        json!({"operationId":id}),
                        &cancelled,
                        |_, _| Ok(()),
                    )?;
                }
            }
            if *tag == EventTag::SessionShutdown {
                runtime.close()?;
            }
            Ok(result)
        },
        false,
    )
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
        let Ok(input) = serde_json::from_str::<Value>(unsafe { input.as_str() }) else {
            return 2;
        };
        let runtime = &unsafe { &*context.cast::<Registration>() }.runtime;
        let cancelled = || {
            unsafe { control.as_ref() }
                .is_some_and(|control| (control.is_cancelled)(control.user_data, 0) != 0)
        };
        let mut partial = None;
        let result = runtime.client.scoped().and_then(|client| {
            runtime.invoke(
                client,
                "provider.stream",
                json!({
                    "entry": input["options"]["streamKind"],
                    "authMode": input["options"]["authMode"],
                    "model": input["model"],
                    "context": input["context"],
                    "options": input["options"],
                }),
                &cancelled,
                |event, _| {
                    if let Some(message) = event
                        .get("partial")
                        .or_else(|| event.get("message"))
                        .or_else(|| event.get("error"))
                        .filter(|value| value.is_object())
                    {
                        partial = Some(message.clone());
                    }
                    if emit.is_some_and(|emit| {
                        emit(StbString::from_string(event.to_string()), data) != 0
                    }) {
                        Err("Cursor stream cancelled by its consumer".into())
                    } else {
                        Ok(())
                    }
                },
            )
        });
        let message = result.unwrap_or_else(|error| {
            let mut message = partial.unwrap_or_else(|| json!({"role":"assistant","api":"cursor-sdk","provider":"cursor","model":input["model"]["id"],"content":[],"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"timestamp":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64}));
            message["stopReason"] = if cancelled() { "aborted" } else { "error" }.into();
            message["errorMessage"] = error.into();
            message
        });
        unsafe { Output::from_raw(out) }.write(message.to_string());
        0
    })
}

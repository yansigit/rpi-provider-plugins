//! Independent target DLL: physical execution asserts target-only authority.
use rpi_plugin_sdk::{self as sdk, PluginApi, StbString, StbStringRef};
use serde_json::{json, Value};
use std::ffi::c_void;
fn terminal(provider: &str, text: &str, failed: bool) -> Value {
    json!({"role":"assistant","content":if failed {json!([])} else {json!([{"type":"text","text":text}])},
        "api":"openai-completions","provider":provider,"model":"m",
        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        "stopReason":if failed {"error"} else {"stop"},"errorMessage":if failed {json!(text)} else {Value::Null},"timestamp":0})
}
extern "C" fn stream(
    input: StbStringRef,
    _: Option<sdk::ProviderStreamEmitFn>,
    _: *mut c_void,
    control: *const sdk::ProviderStreamControl,
    _: Option<sdk::ProviderStreamCompletionFn>,
    _: *mut c_void,
    out: *mut StbString,
    _: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        let input: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
        let provider = input["model"]["provider"].as_str().unwrap();
        let expected = if input["context"]["systemPrompt"]
            .as_str()
            .unwrap_or_default()
            .starts_with("pool")
        {
            "pooled-target-secret".to_owned()
        } else {
            format!("{provider}-secret")
        };
        assert_eq!(input["options"]["apiKey"], expected);
        assert!(!input.to_string().contains("source-secret"));
        assert!(input.get("__rpiContinuation").is_none());
        use std::io::Write;
        static ATTEMPTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let attempt = ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut trace = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::var_os("VIRTUAL_TRACE").unwrap())
            .unwrap();
        writeln!(trace, "{}", json!({"provider":provider,"attempt":attempt})).unwrap();
        // TS intentionally randomizes the initial virtual backend. Fail the
        // first physical attempt, whichever backend was chosen, then succeed.
        if input["context"]["systemPrompt"]
            .as_str()
            .unwrap_or_default()
            .starts_with("cancel")
        {
            let _ready =
                std::os::unix::net::UnixStream::connect(std::env::var_os("VIRTUAL_READY").unwrap())
                    .unwrap();
            assert!(!control.is_null());
            let control = unsafe { &*control };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while (control.is_cancelled)(control.user_data, 0) == 0 {
                assert!(
                    std::time::Instant::now() < deadline,
                    "target never cancelled"
                );
                std::thread::yield_now();
            }
            let mut message = terminal(provider, "cancelled", true);
            message["stopReason"] = json!("aborted");
            unsafe {
                *out = StbString::from_string(message.to_string());
            }
            return 0;
        }
        let fail = !input["context"]["systemPrompt"]
            .as_str()
            .unwrap_or_default()
            .starts_with("pool")
            && attempt
                == if std::env::var("VIRTUAL_FABRIC").as_deref() == Ok("complete") {
                    2
                } else {
                    0
                };
        unsafe {
            *out = StbString::from_string(
                terminal(
                    provider,
                    if fail {
                        "network error"
                    } else {
                        "executed:fixture-blue"
                    },
                    fail,
                )
                .to_string(),
            );
        }
        0
    })
}
static API: std::sync::Mutex<Option<PluginApi>> = std::sync::Mutex::new(None);
extern "C" fn fabric(
    input: StbStringRef,
    _: *const sdk::AuthRequestControl,
    out: *mut StbString,
    _: *mut c_void,
) -> i32 {
    sdk::guard_or(1, || {
        use std::io::Write;
        let request: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
        assert!(
            request["identity"]["sessionId"].is_string(),
            "Fabric calls need execution identity: {request}"
        );
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::env::var_os("VIRTUAL_SERVICE").unwrap())
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"method":request["method"],"identity":request["identity"]})
        )
        .unwrap();
        let result = match request["method"].as_str() {
            Some("capabilities") => {
                let _ready = std::os::unix::net::UnixStream::connect(
                    std::env::var_os("VIRTUAL_READY").unwrap(),
                )
                .unwrap();
                json!(["multiprovider.failoverCompaction.v1"])
            }
            Some("isIdle") if std::env::var("VIRTUAL_FABRIC").as_deref() == Ok("shutdown") => {
                json!({"isIdle":false})
            }
            Some("isIdle") | Some("compact") => {
                // Hold the worker until the harness observed agent_end, which
                // proves the package provider callback has returned.
                let go = std::env::var_os("VIRTUAL_GO").unwrap();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !std::path::Path::new(&go).exists() {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "post-turn worker not released"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if request["method"] == "compact"
                    && std::env::var("VIRTUAL_FABRIC").as_deref() == Ok("busy")
                {
                    unsafe {
                        *out = StbString::from_string(
                            json!({"version":1,"error":"selected execution scope is busy"})
                                .to_string(),
                        );
                    }
                    return 0;
                }
                let api = API.lock().unwrap().unwrap();
                let mut response = StbString::empty();
                let action = if request["method"] == "isIdle" {
                    sdk::RuntimeActionId::IsIdle
                } else {
                    sdk::RuntimeActionId::Compact
                };
                let rc = (api.runtime_action)(
                    action.into(),
                    StbStringRef::from_str("{}"),
                    &mut response,
                    api.user_data,
                );
                let response = unsafe {
                    sdk::callbacks::OwnedString::from_raw(response, Some(api.free_string))
                };
                let response: Value = serde_json::from_str(&response.to_string_lossy()).unwrap();
                writeln!(file, "{}", json!({"method":"result","action":request["method"],"status":rc,"result":response})).unwrap();
                if rc != 0 {
                    unsafe {
                        *out = StbString::from_string(
                            json!({"version":1,"error":response["error"]}).to_string(),
                        );
                    }
                    return 0;
                }
                if request["method"] == "compact" {
                    assert!(
                        response["outcome"]
                            .as_str()
                            .is_some_and(|outcome| outcome.starts_with("Completed {")),
                        "host compaction failed: {response}"
                    );
                    json!({"outcome":"complete"})
                } else {
                    response
                }
            }
            _ => panic!("unknown Fabric method"),
        };
        unsafe {
            *out = StbString::from_string(json!({"version":1,"result":result}).to_string());
        }
        0
    })
}
sdk::export_plugin!(|api: &PluginApi| {
    *API.lock().unwrap() = Some(*api);
    if std::env::var("VIRTUAL_FABRIC").as_deref() != Ok("off") {
        assert_eq!(api.register_service.unwrap()(StbStringRef::from_str(&json!({"version":1,"id":"pi-fabric","serviceVersion":1,"methods":["capabilities","isIdle","compact"],"topics":[]}).to_string()), fabric, sdk::callbacks::free_string, std::ptr::null_mut()), 0);
    }
    let providers: Vec<_> = ["fixture-red", "fixture-blue"].iter().map(|id| json!({"id":id,
        "models":[{"provider":id,"id":"m","name":"M","api":"openai-completions","baseUrl":"https://fixture.invalid",
        "reasoning":false,"input":["text"],"contextWindow":32000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}]})).collect();
    if api.declare.unwrap()(StbStringRef::from_str(
        &json!({"providers":providers}).to_string(),
    )) != 0
    {
        return 1;
    }
    for id in ["fixture-red", "fixture-blue"] {
        if api.register_streaming_provider.unwrap()(
            StbStringRef::from_str(id),
            StbStringRef::from_str("https://fixture.invalid"),
            StbStringRef::from_str("openai-completions"),
            stream,
            sdk::callbacks::free_string,
            std::ptr::null_mut(),
        ) != 0
        {
            return 1;
        }
    }
    0
});

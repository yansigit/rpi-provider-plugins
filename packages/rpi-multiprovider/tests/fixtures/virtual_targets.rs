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
            && attempt == 0;
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
            // Real-host lifecycle case keeps the failed execution busy until
            // session shutdown; ABI tests separately prove successful compact.
            Some("isIdle") => json!({"isIdle":false}),
            Some("compact") => panic!("shutdown must cancel delayed compaction before idle"),
            _ => panic!("unknown Fabric method"),
        };
        unsafe {
            *out = StbString::from_string(json!({"version":1,"result":result}).to_string());
        }
        0
    })
}
sdk::export_plugin!(|api: &PluginApi| {
    if std::env::var("VIRTUAL_FABRIC").as_deref() == Ok("1") {
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

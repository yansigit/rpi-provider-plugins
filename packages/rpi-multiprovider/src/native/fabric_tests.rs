use super::*;
use rpi_plugin_sdk::{
    callbacks::free_string, AuthRequestControl, PluginApi, StbString, StbStringRef,
};
use std::{
    ffi::c_void,
    sync::atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct Host {
    releases: AtomicUsize,
    compactions: AtomicUsize,
}
extern "C" fn runtime_action(_: u32, _: StbStringRef, _: *mut StbString, _: *mut c_void) -> i32 {
    1
}
extern "C" fn request(
    raw: StbStringRef,
    _: *const AuthRequestControl,
    output: *mut StbString,
    data: *mut c_void,
) -> i32 {
    let host = unsafe { &*data.cast::<Host>() };
    let value: Value = serde_json::from_str(unsafe { raw.as_str() }).unwrap();
    let result = match (value["port"].as_str(), value["action"].as_str()) {
        (Some("capabilities"), _) => json!(["services.scopeLease.v1"]),
        (Some("serviceLeases"), Some("acquire")) => json!({"lease":"test-lease"}),
        (Some("serviceLeases"), Some("release")) => {
            host.releases.fetch_add(1, Ordering::SeqCst);
            Value::Null
        }
        (_, Some("call")) if value["method"] == "isIdle" => json!({"isIdle":true}),
        (_, Some("call")) if value["method"] == "compact" => {
            host.compactions.fetch_add(1, Ordering::SeqCst);
            json!({"outcome":"complete"})
        }
        _ => Value::Null,
    };
    unsafe { *output = StbString::from_string(json!({"version":1,"result":result}).to_string()) };
    0
}
fn client(host: &Host) -> Client {
    Client::new(&PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string,
        runtime_action,
        user_data: (host as *const Host).cast_mut().cast(),
        native_request: Some(request),
        register_tool: None,
        register_command: None,
        register_shortcut: None,
        register_flag: None,
        register_provider: None,
        register_message_renderer: None,
        register_markdown_transformer: None,
        register_entry_renderer: None,
        register_event_handler: None,
        register_resources_discover: None,
        dispatch_event: None,
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
        register_streaming_provider: None,
        register_tool_with_wake: None,
        register_plugin_lifecycle: None,
        register_provider_auth: None,
        register_credential_store: None,
        register_provider_wrapper: None,
        delegate_provider_stream: None,
        provider_continuation: None,
        register_provider_runtime: None,
        register_event_result_handler: None,
        register_service: None,
        native_context: None,
        register_native_command: None,
        register_owned_tool_with_wake: None,
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_between_context_and_clock_releases_without_poisoning() {
    let host = Host::default();
    let client = client(&host);
    let state = Arc::new(Mutex::new(CompactionState::default()));
    for cancel_after_context in [true, false] {
        let lease = client
            .scope_service_lease(
                "pi-fabric",
                1,
                &["isIdle", "compact"],
                CancellationToken::new(),
            )
            .await
            .unwrap();
        // Retain a clone: observing release before dropping it proves explicit
        // cleanup, not the SDK's last-clone best-effort Drop fallback.
        let retained = lease.clone();
        let binding = Binding {
            lease,
            accounts: Some(2),
            signal: CancellationToken::new(),
            handle: tokio::runtime::Handle::current(),
            cancel_after_context,
        };
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            binding.run(&worker_state, "p");
            if cancel_after_context {
                assert!(binding.signal.is_cancelled());
                assert_eq!(binding.now_ms(), None, "cancellation is not a timestamp");
            }
        });
        assert!(worker.join().is_ok(), "compaction worker panicked");
        assert!(!state.is_poisoned(), "scope state was poisoned");
        if cancel_after_context {
            assert_eq!(state.lock().unwrap().last_failover_compact_at, 0);
            assert_eq!(host.compactions.load(Ordering::SeqCst), 0);
            assert_eq!(host.releases.load(Ordering::SeqCst), 1);
        } else {
            assert!(state.lock().unwrap().last_failover_compact_at > 0);
            assert_eq!(host.compactions.load(Ordering::SeqCst), 1);
            assert_eq!(host.releases.load(Ordering::SeqCst), 2);
        }
        drop(retained);
    }
}

//! Native loading against the published 0.12 vtable: dlopen this package's
//! cdylib, register through `rpi_plugin_register`, and check the call order of
//! `rpi_plugin_sdk::provider::register_with_context` and the declared manifest.
#![cfg(target_os = "linux")]
use rpi_plugin_sdk::{
    FreeStringFn, PluginApi, PluginBeginShutdownFn, PluginFinishShutdownFn,
    ProviderStreamRequestFn, StbString, StbStringRef, RPI_PLUGIN_ABI_VERSION_UNIFIED,
};
use serde_json::Value;
use std::{
    ffi::{c_void, CString},
    sync::Mutex,
};

static EVENTS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LIFECYCLE: Mutex<Option<(PluginBeginShutdownFn, PluginFinishShutdownFn, usize)>> =
    Mutex::new(None);

extern "C" fn declare(json: StbStringRef) -> i32 {
    EVENTS
        .lock()
        .unwrap()
        .push(format!("declare:{}", unsafe { json.as_str() }));
    0
}
extern "C" fn streaming(
    id: StbStringRef,
    base_url: StbStringRef,
    api: StbStringRef,
    _: ProviderStreamRequestFn,
    _: FreeStringFn,
    _: *mut c_void,
) -> i32 {
    EVENTS.lock().unwrap().push(format!(
        "streaming:{}:{}:{}",
        unsafe { id.as_str() },
        unsafe { base_url.as_str() },
        unsafe { api.as_str() }
    ));
    0
}
extern "C" fn lifecycle(
    begin: PluginBeginShutdownFn,
    finish: PluginFinishShutdownFn,
    data: *mut c_void,
) -> i32 {
    EVENTS.lock().unwrap().push("lifecycle".into());
    *LIFECYCLE.lock().unwrap() = Some((begin, finish, data as usize));
    0
}
extern "C" fn runtime_action(_: u32, _: StbStringRef, _: *mut StbString, _: *mut c_void) -> i32 {
    1
}

fn api() -> PluginApi {
    PluginApi {
        abi_version: RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string: rpi_plugin_sdk::callbacks::free_string,
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
        runtime_action,
        dispatch_event: None,
        user_data: std::ptr::null_mut(),
        declare: Some(declare),
        register_before_agent_start: None,
        register_oauth_provider: None,
        register_streaming_provider: Some(streaming),
        register_tool_with_wake: None,
        register_plugin_lifecycle: Some(lifecycle),
        register_provider_auth: None,
        register_credential_store: None,
        register_provider_wrapper: None,
        delegate_provider_stream: None,
        provider_continuation: None,
        native_request: None,
        register_provider_runtime: None,
        register_event_result_handler: None,
        register_service: None,
        native_context: None,
        register_native_command: None,
        register_owned_tool_with_wake: None,
    }
}

fn register(api: &PluginApi) -> i32 {
    let library = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("librpi_cursor_lite.so");
    let name = CString::new(library.as_os_str().as_encoded_bytes()).unwrap();
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    assert!(!handle.is_null(), "dlopen {}", library.display());
    let symbol = unsafe { libc::dlsym(handle, c"rpi_plugin_register".as_ptr()) };
    assert!(!symbol.is_null());
    let entry: extern "C" fn(*const PluginApi) -> i32 = unsafe { std::mem::transmute(symbol) };
    // The library stays loaded: the host owns it until its lifecycle finishes.
    entry(api)
}

#[test]
fn registers_in_context_order_with_a_strict_manifest_and_shuts_down_unopened() {
    assert_eq!(register(&api()), 0);
    let events = std::mem::take(&mut *EVENTS.lock().unwrap());
    assert_eq!(events.len(), 3, "{events:?}");
    assert_eq!(events[0], "lifecycle");
    assert_eq!(events[2], "streaming:cursor:https://cursor.com:cursor-sdk");
    let manifest: Value =
        serde_json::from_str(events[1].strip_prefix("declare:").unwrap()).unwrap();
    let provider = &manifest["providers"][0];
    // Upstream ProviderManifest denies unknown fields.
    let mut keys: Vec<_> = provider.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["apiKeyEnv", "id", "models", "requiresAuth"]);
    assert_eq!(provider["id"], "cursor");
    assert_eq!(provider["apiKeyEnv"], "CURSOR_API_KEY");
    assert_eq!(provider["requiresAuth"], true);
    let models = provider["models"].as_array().unwrap();
    assert!(models.iter().any(|model| model["id"] == "composer-2"));
    assert!(models
        .iter()
        .all(|model| model["provider"] == "cursor" && model["api"] == "cursor-sdk"));

    // No stream ran, so no worker exists; shutdown must still complete cleanly.
    let (begin, finish, data) = LIFECYCLE.lock().unwrap().take().unwrap();
    begin(data as *mut c_void);
    assert_eq!(finish(data as *mut c_void), 0);

    // A host without the lifecycle slot is refused before anything registers.
    let mut short = api();
    short.register_plugin_lifecycle = None;
    assert_eq!(register(&short), 1);
    assert!(EVENTS.lock().unwrap().is_empty());
}

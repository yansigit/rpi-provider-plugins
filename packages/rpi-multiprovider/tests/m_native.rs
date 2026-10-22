//! Native registration, composition, credential-store and reconcile checks.
//!
//! The suite drives the exported C entrypoint with a scripted host vtable, so
//! every callback crosses the real ABI boundary. The optional final test loads
//! the built cdylib with `dlopen` when Cargo has produced it (or when
//! `RPI_TEST_MULTIPROVIDER_LIBRARY` names it).
use rpi_multiprovider::native::credentials;
use rpi_multiprovider::store::Store;
use rpi_plugin_sdk::{
    callbacks::{free_string, OwnedString},
    AuthRequestControl, AuthRequestFn, CredentialStoreReleaseFn, EventTag, FreeStringFn, PluginApi,
    PluginBeginShutdownFn, PluginFinishShutdownFn, ProviderStreamControl, ProviderStreamEmitFn,
    ProviderStreamRequestFn, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    path::{Path, PathBuf},
    sync::Mutex,
};

thread_local! {
    static CURRENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn current_fake() -> &'static Fake {
    let pointer = CURRENT.with(|current| current.get());
    if pointer == 0 {
        panic!("no registration in progress");
    }
    unsafe { &*(pointer as *mut Fake) }
}

const CAPABILITIES: [&str; 6] = [
    "json.exactNumbers",
    "context.inspect",
    "context.bootstrap",
    "providers.childCatalogs",
    "providers.scopedCallbacks",
    "sessions.view",
];

#[derive(Clone)]
struct Inspect {
    child: bool,
    scope_id: Option<String>,
    session_id: String,
}

impl Default for Inspect {
    fn default() -> Self {
        Self {
            child: false,
            scope_id: None,
            session_id: "session-parent".into(),
        }
    }
}

#[derive(Clone)]
struct Attempt {
    frames: Vec<Value>,
    message: Value,
    status: i32,
}

#[derive(Default)]
struct Fake {
    agent: PathBuf,
    capabilities: Vec<String>,
    providers: Vec<String>,
    inspect: Mutex<Inspect>,
    entries: Mutex<Vec<Value>>,
    appended: Mutex<Vec<Value>>,
    emitted: Mutex<Vec<Value>>,
    continuations: Mutex<Vec<Value>>,
    delegates: Mutex<Vec<Value>>,
    scripts: Mutex<HashMap<String, VecDeque<Attempt>>>,
    hooks: Mutex<Option<(PluginBeginShutdownFn, PluginFinishShutdownFn, usize)>>,
    wrapper: Mutex<Option<(Value, ProviderStreamRequestFn, FreeStringFn, usize)>>,
    store: Mutex<
        Option<(
            Value,
            AuthRequestFn,
            CredentialStoreReleaseFn,
            FreeStringFn,
            usize,
        )>,
    >,
    events: Mutex<Vec<(u64, AuthRequestFn, FreeStringFn, usize)>>,
}

impl Fake {
    fn new(agent: &Path) -> Self {
        Self {
            agent: agent.to_path_buf(),
            capabilities: CAPABILITIES.iter().map(|name| (*name).into()).collect(),
            providers: vec!["example".into(), "plain".into()],
            ..Default::default()
        }
    }

    fn script(&self, account: &str, attempts: Vec<Attempt>) {
        self.scripts
            .lock()
            .unwrap()
            .insert(account.into(), attempts.into());
    }

    fn inspect(&self, inspect: Inspect) {
        *self.inspect.lock().unwrap() = inspect;
    }
}

fn assistant(text: &str, stop: &str, error: Option<&str>) -> Value {
    let content = if text.is_empty() {
        json!([])
    } else {
        json!([{"type":"text","text":text}])
    };
    let mut message = json!({
        "role":"assistant",
        "content":content,
        "api":"faux",
        "provider":"example",
        "model":"probe-model",
        "usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        "stopReason":stop,
        "timestamp":0
    });
    if let Some(error) = error {
        message["errorMessage"] = json!(error);
    }
    message
}

fn success() -> Attempt {
    Attempt {
        frames: vec![
            json!({"type":"start"}),
            json!({"type":"text_start","contentIndex":0}),
            json!({"type":"text_delta","contentIndex":0,"delta":"ok"}),
            json!({"type":"text_end","contentIndex":0,"content":"ok"}),
        ],
        message: assistant("ok", "stop", None),
        status: 0,
    }
}

fn failure(message: &str, status: Option<u16>) -> Attempt {
    let mut frames = Vec::new();
    if let Some(status) = status {
        frames.push(json!({"type":"response","response":{
            "status":status,"headers":[["retry-after",[50,48]]],
        }}));
    }
    Attempt {
        frames,
        message: assistant("", "error", Some(message)),
        status: 0,
    }
}

extern "C" fn fake_emit(raw: StbString, data: *mut c_void) -> i32 {
    let fake = unsafe { &*data.cast::<Fake>() };
    let owned = unsafe { OwnedString::from_raw(raw, Some(free_string)) };
    let value: Value = match serde_json::from_str(&owned.to_string_lossy()) {
        Ok(value) => value,
        Err(_) => return 1,
    };
    fake.emitted.lock().unwrap().push(value);
    0
}

extern "C" fn fake_continuation(
    input: StbStringRef,
    _: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    let fake = unsafe { &*context.cast::<Fake>() };
    let value: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
    fake.continuations.lock().unwrap().push(value.clone());
    let receipt = format!("receipt:{}", value["accountId"].as_str().unwrap());
    unsafe {
        *out = StbString::from_string(json!({"version":1,"receipt":receipt}).to_string());
    }
    0
}

extern "C" fn fake_delegate(
    input: StbStringRef,
    emit: Option<ProviderStreamEmitFn>,
    data: *mut c_void,
    _: *const ProviderStreamControl,
    _: Option<rpi_plugin_sdk::ProviderStreamCompletionFn>,
    _: *mut c_void,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    let fake = unsafe { &*context.cast::<Fake>() };
    let request: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
    let account = request["authReceipt"]
        .as_str()
        .and_then(|receipt| receipt.strip_prefix("receipt:"))
        .unwrap_or("ambient")
        .to_owned();
    let mut record = request.clone();
    record["account"] = json!(account);
    fake.delegates.lock().unwrap().push(record);
    let attempt = fake
        .scripts
        .lock()
        .unwrap()
        .get_mut(&account)
        .and_then(|attempts| attempts.pop_front())
        .unwrap_or_else(|| success());
    if let Some(emit) = emit {
        for frame in attempt.frames {
            if emit(StbString::from_string(frame.to_string()), data) != 0 {
                return 1;
            }
        }
    }
    unsafe {
        *out = StbString::from_string(attempt.message.to_string());
    }
    attempt.status
}

extern "C" fn fake_native_request(
    input: StbStringRef,
    _: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    let fake = unsafe { &*context.cast::<Fake>() };
    let value: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
    let result = match (value["port"].as_str(), value["action"].as_str()) {
        (Some("context"), Some("bootstrap")) => {
            json!({"cwd":"/tmp","agentDir":fake.agent,"sessionDir":"/tmp/sessions"})
        }
        (Some("capabilities"), Some("inspect")) => json!(fake.capabilities),
        (Some("context"), Some("inspect")) => {
            let inspect = fake.inspect.lock().unwrap().clone();
            json!({
                "owner":{"id":7,"generation":"generation-1","child":inspect.child},
                "identity":{"sessionId":inspect.session_id,"operationId":"op","purpose":"turn"},
                "scopeId":inspect.scope_id,
                "closing":false
            })
        }
        (Some("providers"), Some("inspect")) => json!({
            "revision":1,
            "providers":fake.providers.iter().map(|id| json!({"id":id,"name":id,"builtin":false})).collect::<Vec<_>>(),
        }),
        (Some("sessions"), Some("capture")) => {
            let entries = fake.entries.lock().unwrap().clone();
            json!({"origin":"origin-1","snapshot":{"entries":entries,"branches":[],"revision":1}})
        }
        (Some("sessions"), Some("journalAppend")) => {
            fake.appended.lock().unwrap().push(value["data"].clone());
            json!({"entry":{"id":format!("entry-{}", fake.appended.lock().unwrap().len())},"revision":2})
        }
        (Some("sessions"), Some("release")) => Value::Null,
        _ => {
            unsafe {
                *out = StbString::from_string(
                    json!({"version":1,"error":"unsupported fake request"}).to_string(),
                );
            }
            return 0;
        }
    };
    unsafe {
        *out = StbString::from_string(json!({"version":1,"result":result}).to_string());
    }
    0
}

extern "C" fn fake_capture(_: u32, data: *mut c_void) -> *mut c_void {
    data
}

extern "C" fn fake_register_wrapper(
    manifest: StbStringRef,
    request: ProviderStreamRequestFn,
    free: FreeStringFn,
    context: *mut c_void,
) -> i32 {
    let fake = current_fake();
    let manifest: Value = serde_json::from_str(unsafe { manifest.as_str() }).unwrap();
    *fake.wrapper.lock().unwrap() = Some((manifest, request, free, context as usize));
    0
}

extern "C" fn fake_register_store(
    manifest: StbStringRef,
    request: AuthRequestFn,
    release: CredentialStoreReleaseFn,
    free: FreeStringFn,
    context: *mut c_void,
) -> i32 {
    let fake = current_fake();
    let manifest: Value = serde_json::from_str(unsafe { manifest.as_str() }).unwrap();
    *fake.store.lock().unwrap() = Some((manifest, request, release, free, context as usize));
    0
}

extern "C" fn fake_register_event(
    tag: EventTag,
    handler: AuthRequestFn,
    free: FreeStringFn,
    context: *mut c_void,
) -> i32 {
    let fake = current_fake();
    fake.events
        .lock()
        .unwrap()
        .push((tag as u64, handler, free, context as usize));
    0
}

extern "C" fn fake_lifecycle(
    begin: PluginBeginShutdownFn,
    finish: PluginFinishShutdownFn,
    context: *mut c_void,
) -> i32 {
    let fake = current_fake();
    *fake.hooks.lock().unwrap() = Some((begin, finish, context as usize));
    0
}

extern "C" fn unused_runtime_action(
    _: u32,
    _: StbStringRef,
    _: *mut StbString,
    _: *mut c_void,
) -> i32 {
    1
}

fn plugin_api(pointer: *mut Fake) -> PluginApi {
    PluginApi {
        abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
        struct_size: std::mem::size_of::<PluginApi>() as u32,
        free_string,
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
        runtime_action: unused_runtime_action,
        dispatch_event: None,
        user_data: pointer.cast(),
        declare: None,
        register_before_agent_start: None,
        register_oauth_provider: None,
        register_streaming_provider: None,
        register_tool_with_wake: None,
        register_plugin_lifecycle: Some(fake_lifecycle),
        register_provider_auth: None,
        register_credential_store: Some(fake_register_store),
        register_provider_wrapper: Some(fake_register_wrapper),
        delegate_provider_stream: Some(fake_delegate),
        provider_continuation: Some(fake_continuation),
        native_request: Some(fake_native_request),
        register_provider_runtime: None,
        register_event_result_handler: Some(fake_register_event),
        register_service: None,
        native_context: Some(fake_capture),
        register_native_command: None,
        register_owned_tool_with_wake: None,
    }
}

fn account(id: &str, label: &str, credential: Value, enabled: bool) -> Value {
    json!({
        "id":id,"label":label,
        "authKind":if credential["type"] == "oauth" { "oauth" } else { "api-key" },
        "credential":credential,"enabled":enabled,"weight":1,"priority":1,
        "createdAt":"2023-11-14T22:13:20.000Z","updatedAt":"2023-11-14T22:13:20.000Z"
    })
}

fn write_store(path: &Path, accounts: Vec<Value>, errors_before_switch: Option<f64>) {
    let mut state = json!({
        "version":1,
        "providers":{
            "example":{"policy":"round-robin","affinity":true,"includeUpstream":false,"accounts":accounts}
        }
    });
    if let Some(value) = errors_before_switch {
        state["scheduler"] = json!({"errorsBeforeSwitch":value});
    }
    std::fs::write(path, format!("{state}\n")).unwrap();
}

struct Registered {
    fake: *mut Fake,
    wrapper: (Value, ProviderStreamRequestFn, FreeStringFn, usize),
    store: (
        Value,
        AuthRequestFn,
        CredentialStoreReleaseFn,
        FreeStringFn,
        usize,
    ),
    events: Vec<(u64, AuthRequestFn, FreeStringFn, usize)>,
}

fn register(agent: &Path, accounts: Vec<Value>, threshold: Option<f64>) -> Registered {
    std::fs::create_dir_all(agent).unwrap();
    write_store(&agent.join("multiprovider-auth.json"), accounts, threshold);
    let fake = Box::into_raw(Box::new(Fake::new(agent)));
    let api = plugin_api(fake);
    CURRENT.with(|current| current.set(fake as usize));
    let status = rpi_multiprovider::native::rpi_plugin_register(&api);
    CURRENT.with(|current| current.set(0));
    assert_eq!(status, 0, "native registration");
    let fake_ref = unsafe { &*fake };
    Registered {
        fake,
        wrapper: fake_ref.wrapper.lock().unwrap().clone().expect("wrapper"),
        store: fake_ref.store.lock().unwrap().clone().expect("store"),
        events: fake_ref.events.lock().unwrap().clone(),
    }
}

fn request(fake: *mut Fake, registered: &Registered, provider: &str) -> (Value, Vec<Value>) {
    unsafe { &*fake }.delegates.lock().unwrap().clear();
    unsafe { &*fake }.emitted.lock().unwrap().clear();
    let request = json!({
        "model":{"id":"probe-model","provider":provider,"api":"faux","baseUrl":"https://example.invalid"},
        "context":{"messages":[]},
        "options":{"streamKind":"streamSimple"},
        "__rpiContinuation":{"version":1,"id":"continuation-1","generation":"generation-1"}
    });
    let mut out = StbString::empty();
    let (_, invoke, free, wrapper_context) = &registered.wrapper;
    assert_eq!(
        invoke(
            StbStringRef::from_str(&request.to_string()),
            Some(fake_emit),
            fake.cast(),
            std::ptr::null(),
            None,
            std::ptr::null_mut(),
            &mut out,
            *wrapper_context as *mut c_void,
        ),
        0
    );
    let owned = unsafe { OwnedString::from_raw(out, Some(*free)) };
    let message: Value = serde_json::from_str(&owned.to_string_lossy()).unwrap();
    (message, unsafe { &*fake }.emitted.lock().unwrap().clone())
}

fn emit_event(registered: &Registered, tag: EventTag) {
    let fake = registered.fake;
    let handler = registered
        .events
        .iter()
        .find(|(known, ..)| *known == tag as u64)
        .map(|(_, handler, free, context)| (*handler, *free, *context))
        .expect("event handler");
    let inspect = unsafe { &*fake }.inspect.lock().unwrap().clone();
    let input = json!({
        "version":1,"tag":tag as u64,
        "event":{"reason":"startup"},
        "identity":{"sessionId":inspect.session_id,"operationId":"op"}
    });
    let mut out = StbString::empty();
    assert_eq!(
        handler.0(
            StbStringRef::from_str(&input.to_string()),
            std::ptr::null(),
            &mut out,
            handler.2 as *mut c_void,
        ),
        0
    );
    let owned = unsafe { OwnedString::from_raw(out, Some(handler.1)) };
    let response: Value = serde_json::from_str(&owned.to_string_lossy()).unwrap();
    assert!(response["error"].is_null(), "event error: {response}");
}

fn continuation_accounts(fake: *mut Fake) -> Vec<String> {
    unsafe { &*fake }
        .continuations
        .lock()
        .unwrap()
        .iter()
        .map(|request| request["accountId"].as_str().unwrap().to_owned())
        .collect()
}

fn delegated_accounts(fake: *mut Fake) -> Vec<String> {
    unsafe { &*fake }
        .delegates
        .lock()
        .unwrap()
        .iter()
        .filter_map(|request| request["account"].as_str().map(str::to_owned))
        .collect()
}

fn finish(fake: *mut Fake) {
    let hooks = unsafe { &*fake }
        .hooks
        .lock()
        .unwrap()
        .clone()
        .expect("hooks");
    hooks.0(hooks.2 as *mut c_void);
    assert_eq!(hooks.1(hooks.2 as *mut c_void), 0);
    unsafe {
        drop(Box::from_raw(fake));
    }
}

#[test]
fn registers_one_identity_preserving_wrapper_store_and_awaited_lifecycle() {
    let temp = tempfile_dir();
    let registered = register(&temp, Vec::new(), None);
    assert_eq!(
        registered.wrapper.0,
        json!({"version":1,"id":"multiprovider"}),
        "the wrapper applies to every provider and carries no provider list"
    );
    assert_eq!(
        registered.store.0,
        json!({"version":1,"id":"multiprovider"}),
        "the credential store is private to this plugin"
    );
    let tags: Vec<u64> = registered.events.iter().map(|(tag, ..)| *tag).collect();
    for tag in [
        EventTag::SessionStart,
        EventTag::BeforeAgentStart,
        EventTag::SessionShutdown,
    ] {
        assert!(tags.contains(&(tag as u64)), "missing event {tag:?}");
    }
    finish(registered.fake);
}

#[test]
fn missing_capability_or_slot_refuses_registration() {
    let temp = tempfile_dir();
    let fake = Box::into_raw(Box::new(Fake::new(&temp)));
    unsafe { &mut *fake }.capabilities.clear();
    let api = plugin_api(fake);
    CURRENT.with(|current| current.set(fake as usize));
    let status = rpi_multiprovider::native::rpi_plugin_register(&api);
    CURRENT.with(|current| current.set(0));
    assert_eq!(status, 1);
    unsafe {
        drop(Box::from_raw(fake));
    }

    let fake = Box::into_raw(Box::new(Fake::new(&temp)));
    let mut api = plugin_api(fake);
    api.register_event_result_handler = None;
    CURRENT.with(|current| current.set(fake as usize));
    let status = rpi_multiprovider::native::rpi_plugin_register(&api);
    CURRENT.with(|current| current.set(0));
    assert_eq!(status, 1);
    unsafe {
        drop(Box::from_raw(fake));
    }
}

#[test]
fn credential_store_transactions_match_the_persisted_account() {
    let temp = tempfile_dir();
    let store_path = temp.join("multiprovider-auth.json");
    std::fs::create_dir_all(&*temp).unwrap();
    write_store(
        &store_path,
        vec![account(
            "a",
            "Work",
            json!({"type":"api_key","key":"k-work","env":{"ACCOUNT_REGION":"west"}}),
            true,
        )],
        None,
    );
    let fake = Box::into_raw(Box::new(Fake::new(&temp)));
    let api = plugin_api(fake);
    CURRENT.with(|current| current.set(fake as usize));
    let status = rpi_multiprovider::native::rpi_plugin_register(&api);
    CURRENT.with(|current| current.set(0));
    assert_eq!(status, 0);
    let stored = unsafe { &*fake }.store.lock().unwrap().clone().unwrap();
    let (_, request_fn, release, free, store_context) = stored;

    let call = |action: &str, input: Value| -> Value {
        let mut out = StbString::empty();
        assert_eq!(
            request_fn(
                StbStringRef::from_str(
                    &json!({"version":1,"storeId":"multiprovider","providerId":"example",
                        "accountId":"a","action":action,"input":input})
                    .to_string(),
                ),
                std::ptr::null(),
                &mut out,
                store_context as *mut c_void,
            ),
            0
        );
        let owned = unsafe { OwnedString::from_raw(out, Some(free)) };
        serde_json::from_str(&owned.to_string_lossy()).unwrap()
    };

    let read = call("read", Value::Null);
    assert_eq!(read["kind"], "credential");
    assert_eq!(
        read["payload"],
        json!({"type":"api_key","key":"k-work","env":{"ACCOUNT_REGION":"west"}})
    );

    let locked = call("lock", Value::Null);
    assert_eq!(locked["kind"], "locked");
    let lock_id = locked["payload"]["lockId"].as_str().unwrap().to_owned();
    assert_eq!(locked["payload"]["credential"], read["payload"]);

    let replaced = call(
        "replace",
        json!({"lockId":lock_id,"credential":{
            "type":"oauth","access":"new-access","refresh":"rotated","expires":9876543
        }}),
    );
    assert_eq!(replaced["kind"], "success");
    assert_eq!(
        credentials::read(&store_path, "example", "a")
            .map_err(credentials::failure_text)
            .unwrap(),
        Some(json!({"type":"oauth","access":"new-access","refresh":"rotated","expires":9876543}))
    );
    assert_eq!(call("read", Value::Null)["payload"]["access"], "new-access");

    // Releasing is a leaf operation; the lock file is gone afterwards.
    release(
        StbStringRef::from_str(&lock_id),
        store_context as *mut c_void,
    );
    assert!(!store_path.with_extension("json.credentials.lock").exists());

    let removed = call("remove", json!({"lockId":"stale"}));
    assert_eq!(removed["kind"], "error");
    let locked = call("lock", Value::Null);
    let lock_id = locked["payload"]["lockId"].as_str().unwrap().to_owned();
    assert_eq!(call("remove", json!({"lockId":lock_id}))["kind"], "success");
    assert_eq!(
        credentials::read(&store_path, "example", "a")
            .map_err(credentials::failure_text)
            .unwrap(),
        None
    );
    assert_eq!(call("read", Value::Null)["payload"], Value::Null);

    // Cancellation is a structured error, never a silent credential.
    let control = AuthRequestControl {
        struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
        is_cancelled: cancelled,
        user_data: std::ptr::null_mut(),
    };
    let mut out = StbString::empty();
    assert_eq!(
        request_fn(
            StbStringRef::from_str(
                &json!({"version":1,"storeId":"multiprovider","providerId":"example",
                    "accountId":"a","action":"read","input":null})
                .to_string()
            ),
            &control,
            &mut out,
            store_context as *mut c_void,
        ),
        0
    );
    let owned = unsafe { OwnedString::from_raw(out, Some(free)) };
    let cancelled: Value = serde_json::from_str(&owned.to_string_lossy()).unwrap();
    assert_eq!(cancelled["kind"], "error");

    finish(fake);
}

extern "C" fn cancelled(_: *mut c_void) -> u8 {
    1
}

#[test]
fn pooled_attempts_resolve_receipts_switch_accounts_and_report_status() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![
            account("a", "A", json!({"type":"api_key","key":"k-a"}), true),
            account("b", "B", json!({"type":"api_key","key":"k-b"}), true),
        ],
        Some(1.0),
    );
    let fake = registered.fake;
    emit_event(&registered, EventTag::SessionStart);
    unsafe { &*fake }.script("a", vec![failure("opaque failure", Some(429)), success()]);
    unsafe { &*fake }.script("b", vec![success()]);

    let (message, emitted) = request(fake, &registered, "example");
    assert_eq!(message["stopReason"], "stop");
    assert_eq!(message["content"][0]["text"], "ok");
    assert_eq!(continuation_accounts(fake), vec!["a", "b"]);
    assert_eq!(delegated_accounts(fake), vec!["a", "b"]);
    // The private response receipt is forwarded to the host boundary (never as
    // transcript content), and the caller sees the output frames in order.
    assert!(emitted
        .iter()
        .any(|frame| frame["type"] == "text_delta" && frame["delta"] == "ok"));
    assert!(emitted
        .iter()
        .any(|frame| frame["type"] == "response" && frame["response"]["status"] == 429));

    let delegates = unsafe { &*fake }.delegates.lock().unwrap().clone();
    assert_eq!(delegates[0]["authReceipt"], "receipt:a");
    assert_eq!(delegates[0]["options"]["maxRetries"], 0);
    assert_eq!(delegates[1]["authReceipt"], "receipt:b");

    // A provider without a stored pool is delegated unchanged: no account lease
    // and no authentication receipt.
    let (message, _) = request(fake, &registered, "plain");
    assert_eq!(message["stopReason"], "stop");
    let delegates = unsafe { &*fake }.delegates.lock().unwrap().clone();
    assert_eq!(delegates.len(), 1);
    assert!(delegates[0]["authReceipt"].is_null());
    assert_eq!(delegates[0]["account"], "ambient");

    finish(fake);
}

#[test]
fn upstream_account_delegates_ambient_auth_without_a_pooled_receipt() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"k-a"}),
            true,
        )],
        Some(1.0),
    );
    let fake = registered.fake;
    let state = json!({
        "version":1,
        "scheduler":{"errorsBeforeSwitch":1},
        "providers":{"example":{
            "policy":"round-robin","affinity":true,"includeUpstream":true,
            "accounts":[account("a", "A", json!({"type":"api_key","key":"k-a"}), true)]
        }}
    });
    std::fs::write(temp.join("multiprovider-auth.json"), format!("{state}\n")).unwrap();
    unsafe { &*fake }.script("ambient", vec![success()]);
    emit_event(&registered, EventTag::SessionStart);
    unsafe { &*fake }.continuations.lock().unwrap().clear();
    let (message, _) = request(fake, &registered, "example");
    assert_eq!(message["stopReason"], "stop");
    assert!(continuation_accounts(fake).is_empty());
    let delegates = unsafe { &*fake }.delegates.lock().unwrap().clone();
    assert_eq!(delegates.len(), 1);
    assert!(delegates[0]["authReceipt"].is_null());
    assert_eq!(delegates[0]["account"], "ambient");
    finish(fake);
}

#[test]
fn recorded_pins_are_scope_local_and_survive_child_shutdown() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![
            account("a", "A", json!({"type":"api_key","key":"k-a"}), true),
            account("b", "B", json!({"type":"api_key","key":"k-b"}), true),
        ],
        Some(1.0),
    );
    let fake = registered.fake;
    unsafe { &*fake }.script("a", vec![success(); 8]);
    unsafe { &*fake }.script("b", vec![success(); 8]);

    unsafe { &*fake }.entries.lock().unwrap().extend([
        json!({"type":"custom","customType":"pi-multiprovider:switch-account",
            "data":{"pool":"example","key":"session-parent","accountId":"b"}}),
        json!({"type":"custom","customType":"foreign","data":{"pool":"example","key":"x"}}),
    ]);
    emit_event(&registered, EventTag::SessionStart);
    unsafe { &*fake }.continuations.lock().unwrap().clear();
    let _ = request(fake, &registered, "example");
    assert_eq!(continuation_accounts(fake), vec!["b"]);

    // A child session has its own journal and scope: its pin must not overwrite
    // the parent's, and closing it must leave the parent registered.
    unsafe { &*fake }.inspect(Inspect {
        child: true,
        scope_id: Some("child-scope".into()),
        session_id: "session-child".into(),
    });
    unsafe { &*fake }.entries.lock().unwrap().clear();
    unsafe { &*fake }.entries.lock().unwrap().push(
        json!({"type":"custom","customType":"pi-multiprovider:switch-account",
            "data":{"pool":"example","key":"session-child","accountId":"a"}}),
    );
    emit_event(&registered, EventTag::SessionStart);
    unsafe { &*fake }.continuations.lock().unwrap().clear();
    let _ = request(fake, &registered, "example");
    assert_eq!(continuation_accounts(fake), vec!["a"]);

    emit_event(&registered, EventTag::SessionShutdown);
    unsafe { &*fake }.inspect(Inspect::default());
    unsafe { &*fake }.continuations.lock().unwrap().clear();
    let _ = request(fake, &registered, "example");
    assert_eq!(continuation_accounts(fake), vec!["b"]);

    finish(fake);
}

#[test]
fn reconcile_re_registers_changed_pools_without_duplicate_schedulers() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![
            account("a", "A", json!({"type":"api_key","key":"k-a"}), true),
            account("b", "B", json!({"type":"api_key","key":"k-b"}), true),
        ],
        Some(1.0),
    );
    let fake = registered.fake;
    unsafe { &*fake }.script("a", vec![success(); 4]);
    unsafe { &*fake }.script("b", vec![success(); 4]);
    emit_event(&registered, EventTag::SessionStart);
    emit_event(&registered, EventTag::BeforeAgentStart);
    emit_event(&registered, EventTag::BeforeAgentStart);

    // Remove the pinned account from the store. The next reconcile must drop it
    // from the scheduler inventory and restore falls back to automatic instead
    // of failing the session.
    unsafe { &*fake }.entries.lock().unwrap().clear();
    unsafe { &*fake }.entries.lock().unwrap().push(
        json!({"type":"custom","customType":"pi-multiprovider:switch-account",
            "data":{"pool":"example","key":"session-parent","accountId":"b"}}),
    );
    emit_event(&registered, EventTag::SessionStart);
    write_store(
        &temp.join("multiprovider-auth.json"),
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"k-a"}),
            true,
        )],
        Some(1.0),
    );
    emit_event(&registered, EventTag::BeforeAgentStart);
    unsafe { &*fake }.continuations.lock().unwrap().clear();
    let _ = request(fake, &registered, "example");
    let accounts = continuation_accounts(fake);
    assert_eq!(accounts, vec!["a"], "stale pin falls back to the live pool");
    finish(fake);
}

#[test]
fn credential_adapter_matches_the_upstream_store_fixture() {
    let reference: Value = serde_json::from_str(include_str!("m_native-reference.json")).unwrap();
    let temp = tempfile_dir();
    let path = temp.join("multiprovider-auth.json");
    std::fs::create_dir_all(&*temp).unwrap();
    std::fs::write(&path, reference["state"].to_string()).unwrap();
    for account in reference["accounts"].as_array().unwrap() {
        let id = account["id"].as_str().unwrap();
        assert_eq!(
            credentials::read(&path, "example", id)
                .map_err(credentials::failure_text)
                .unwrap(),
            Some(account["credential"].clone()),
            "read credential for {id}"
        );
        let mapped =
            rpi_multiprovider::native::provider_accounts(&Store::new(path.clone()), "example")
                .unwrap();
        assert!(mapped.iter().any(|candidate| candidate["id"] == id));
    }
    let inventory =
        rpi_multiprovider::native::provider_accounts(&Store::new(path.clone()), "example").unwrap();
    let stored: Vec<Value> = inventory
        .into_iter()
        .filter(|account| account["id"] != "pi:default")
        .map(|account| {
            json!({
                "id":account["id"],"label":account["label"],"authKind":account["authKind"],
                "enabled":account["enabled"],"weight":account["weight"],
                "priority":account["priority"]
            })
        })
        .collect();
    assert!(
        numbers_equal(&json!(stored), &reference["inventory"]),
        "inventory {stored:?} != {}",
        reference["inventory"]
    );

    std::fs::write(&path, reference["replaced"]["state"].to_string()).unwrap();
    assert_eq!(
        credentials::read(
            &path,
            "example",
            reference["replaced"]["account"]["id"].as_str().unwrap(),
        )
        .map_err(credentials::failure_text)
        .unwrap(),
        Some(reference["replaced"]["account"]["credential"].clone())
    );

    assert_eq!(
        credentials::normalized(&json!({"type":"unsupported"})),
        Err(reference["malformed"]["error"]
            .as_str()
            .unwrap()
            .replace("multiprovider: ", ""))
    );
    assert_eq!(
        credentials::read(&path, "example", "missing")
            .map_err(credentials::failure_text)
            .unwrap(),
        None
    );
}

#[test]
fn inherited_plan_filters_recorded_pools_and_waits_for_schedulers() {
    use rpi_multiprovider::pins::InheritedPin;
    let recorded = vec![rpi_multiprovider::pins::SessionPin {
        pool: "example".into(),
        key: "session".into(),
        account_id: Some("a".into()),
        label: None,
    }];
    let inherited = vec![
        InheritedPin {
            pool: "example".into(),
            account_id: Some("b".into()),
            label: None,
        },
        InheritedPin {
            pool: "later".into(),
            account_id: Some("b".into()),
            label: Some("B".into()),
        },
        InheritedPin {
            pool: "unknown".into(),
            account_id: None,
            label: None,
        },
    ];
    let (ready, pending) = rpi_multiprovider::native::inherited_pin_plan(
        &recorded,
        &inherited,
        |pool| pool == "example" || pool == "later",
        Some("child-session"),
    );
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].pool, "later");
    assert_eq!(ready[0].key, "child-session");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].pool, "unknown");
    let (ready, pending) =
        rpi_multiprovider::native::inherited_pin_plan(&recorded, &inherited, |_| true, None);
    assert!(ready.is_empty());
    assert_eq!(pending.len(), 2);
}

fn numbers_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| numbers_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left)| {
                    right
                        .get(key)
                        .is_some_and(|right| numbers_equal(left, right))
                })
        }
        _ => left == right,
    }
}

struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tempfile_dir() -> TempDir {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let path = base.join(format!(
        ".rpi-multiprovider-native-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    TempDir(path)
}

#[cfg(unix)]
#[test]
fn built_library_registers_through_the_exported_symbol() {
    use std::ffi::{c_char, c_int, c_void, CString};

    extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlclose(handle: *mut c_void) -> c_int;
    }
    const RTLD_NOW: c_int = 2;

    let library = match std::env::var_os("RPI_TEST_MULTIPROVIDER_LIBRARY") {
        Some(path) => PathBuf::from(path),
        None => {
            let Some(exe) = std::env::current_exe().ok() else {
                return;
            };
            let profile = exe.parent().and_then(Path::parent);
            let mut found = None;
            if let Some(profile) = profile {
                for candidate in [
                    profile.join("librpi_multiprovider.so"),
                    profile.join("deps").join("librpi_multiprovider.so"),
                ] {
                    if candidate.is_file() {
                        found = Some(candidate);
                        break;
                    }
                }
            }
            match found {
                Some(path) => path,
                None => {
                    eprintln!("SKIP: build the cdylib or set RPI_TEST_MULTIPROVIDER_LIBRARY");
                    return;
                }
            }
        }
    };
    let name = CString::new(library.to_string_lossy().as_bytes()).unwrap();
    let handle = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
    assert!(!handle.is_null(), "could not load {}", library.display());
    let symbol = CString::new("rpi_plugin_register").unwrap();
    let entry = unsafe { dlsym(handle, symbol.as_ptr()) };
    assert!(!entry.is_null(), "missing exported registration symbol");
    let register: unsafe extern "C" fn(*const PluginApi) -> i32 =
        unsafe { std::mem::transmute(entry) };

    let temp = tempfile_dir();
    std::fs::write(
        temp.join("multiprovider-auth.json"),
        json!({"version":1,"providers":{"example":{
            "policy":"round-robin","affinity":true,"includeUpstream":false,
            "accounts":[{"id":"a","label":"A","authKind":"api-key",
                "credential":{"type":"api_key","key":"k-a"},"enabled":true,
                "weight":1,"priority":1,
                "createdAt":"2023-11-14T22:13:20.000Z","updatedAt":"2023-11-14T22:13:20.000Z"}]
        }}})
        .to_string(),
    )
    .unwrap();
    let fake = Box::into_raw(Box::new(Fake::new(&temp)));
    let api = plugin_api(fake);
    CURRENT.with(|current| current.set(fake as usize));
    let status = unsafe { register(&api) };
    CURRENT.with(|current| current.set(0));
    assert_eq!(status, 0);
    let registered = unsafe { &*fake };
    let wrapper = registered.wrapper.lock().unwrap().clone().unwrap();
    unsafe { &*fake }.script("a", vec![success()]);
    let request = json!({
        "model":{"id":"probe-model","provider":"example","api":"faux","baseUrl":"https://example.invalid"},
        "context":{"messages":[]},"options":{},
        "__rpiContinuation":{"version":1,"id":"continuation-1","generation":"generation-1"}
    });
    let mut out = StbString::empty();
    assert_eq!(
        (wrapper.1)(
            StbStringRef::from_str(&request.to_string()),
            Some(fake_emit),
            fake.cast(),
            std::ptr::null(),
            None,
            std::ptr::null_mut(),
            &mut out,
            wrapper.3 as *mut c_void,
        ),
        0
    );
    let message = unsafe { OwnedString::from_raw(out, Some(wrapper.2)) };
    let message: Value = serde_json::from_str(&message.to_string_lossy()).unwrap();
    assert_eq!(message["stopReason"], "stop");
    finish(fake);
    unsafe {
        dlclose(handle);
    }
}

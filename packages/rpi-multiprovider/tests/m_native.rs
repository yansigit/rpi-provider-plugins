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
    sync::{Arc, Mutex},
};

include!("m_virtual_native_cases.rs.inc");
include!("m_virtual_services_cases.rs.inc");

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

const CAPABILITIES: [&str; 9] = [
    "json.exactNumbers",
    "context.inspect",
    "context.bootstrap",
    "providers.childCatalogs",
    "providers.scopedCallbacks",
    "sessions.view",
    "commands.owned",
    "ui.notifications",
    "context.snapshot",
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
    has_ui: bool,
    mode: String,
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
    commands: Mutex<Vec<(Value, AuthRequestFn, FreeStringFn, usize)>>,
    command_requests: Mutex<Vec<Value>>,
    command_snapshots: Mutex<Vec<Value>>,
    provider_inspections: std::sync::atomic::AtomicUsize,
    fail_provider_inspect: std::sync::atomic::AtomicBool,
    fail_command_on_nth: std::sync::atomic::AtomicUsize,
    hang_dialog: std::sync::atomic::AtomicBool,
    dialog_waiting: std::sync::atomic::AtomicBool,
    catalog: Mutex<Option<Value>>,
    publications: Mutex<Vec<Value>>,
    snapshot_model: Mutex<Option<Value>>,
    fail_journal: std::sync::atomic::AtomicBool,
    /// Corrupts the account store while serving `sessions/journalAppend`, so
    /// the post-commit announcement finds an unreadable pool.
    corrupt_store_after_journal: std::sync::atomic::AtomicBool,
    cancel_dialog: std::sync::atomic::AtomicBool,
    fail_publication: std::sync::atomic::AtomicBool,
    v2: std::sync::atomic::AtomicBool,
    fail_first_virtual_attempt: std::sync::atomic::AtomicBool,
    fabric_capable: std::sync::atomic::AtomicBool,
    fabric_idle: std::sync::atomic::AtomicBool,
    fabric_calls: Mutex<Vec<Value>>,
    fabric_wake: std::sync::Condvar,
    /// Scripted dialog replies consumed before the default replies.
    dialog_replies: Mutex<VecDeque<Value>>,
    /// `auth` `methods` responses by provider id; absent providers fail.
    auth_methods: Mutex<HashMap<String, Value>>,
    /// Every `auth` port request, in order.
    auth_requests: Mutex<Vec<Value>>,
}

impl Fake {
    fn new(agent: &Path) -> Self {
        Self {
            agent: agent.to_path_buf(),
            has_ui: true,
            mode: "tui".into(),
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
    let result = match value["action"].as_str() {
        Some("releaseAuth" | "releaseTarget") => {
            Some(json!({"version":value["version"],"released":true}))
        }
        Some("capabilities") => Some(
            json!({"version":1,"delegationVersions":if fake.v2.load(std::sync::atomic::Ordering::SeqCst) {vec![1,2]} else {vec![1]},"opaqueTargets":true,"ambientReceipts":true,"epochRevocation":true}),
        ),
        Some("resolveTarget") => Some(
            json!({"version":2,"target":format!("target:{}",value["provider"].as_str().unwrap()),"model":{"provider":value["provider"],"id":value["model"],"api":"faux"}}),
        ),
        Some("auth.resolveAmbient") => Some(
            json!({"version":2,"receipt":format!("receipt:{}",value["provider"].as_str().unwrap())}),
        ),
        _ => None,
    };
    if let Some(result) = result {
        unsafe {
            *out = StbString::from_string(result.to_string());
        }
        return 0;
    }
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
    let attempt = if request["version"] == 2
        && fake
            .fail_first_virtual_attempt
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    {
        failure("network error", None)
    } else {
        fake.scripts
            .lock()
            .unwrap()
            .get_mut(&account)
            .and_then(|attempts| attempts.pop_front())
            .unwrap_or_else(|| success())
    };
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
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    let fake = unsafe { &*context.cast::<Fake>() };
    let value: Value = serde_json::from_str(unsafe { input.as_str() }).unwrap();
    if value["port"] == "providers" && value["action"] == "inspect" {
        fake.provider_inspections
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if fake
            .fail_provider_inspect
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            unsafe {
                *out = StbString::from_string(
                    json!({"version":1,"error":"provider inspection failed"}).to_string(),
                );
            }
            return 1;
        }
    }
    let result = match (value["port"].as_str(), value["action"].as_str()) {
        (Some("context"), Some("bootstrap")) => {
            json!({"cwd":"/tmp","agentDir":fake.agent,"sessionDir":"/tmp/sessions"})
        }
        (Some("capabilities"), Some("inspect")) => {
            let mut capabilities = fake.capabilities.clone();
            if fake.fabric_capable.load(std::sync::atomic::Ordering::SeqCst) {
                capabilities.push("services.scopeLease.v1".into());
            }
            json!(capabilities)
        },
        (Some("serviceLeases"), Some("acquire")) => json!({"lease":"fabric-lease"}),
        (Some("serviceLeases"), Some("release")) => {
            fake.fabric_calls.lock().unwrap().push(value.clone());
            Value::Null
        },
        (Some("services"), Some("list")) => if fake.fabric_capable.load(std::sync::atomic::Ordering::SeqCst) {
            json!([{"id":"pi-fabric","serviceVersion":1,"methods":["capabilities","isIdle","compact"]}])
        } else { json!([]) },
        (Some("services"), Some("call")) | (Some("serviceLeases"), Some("call")) => {
            fake.fabric_calls.lock().unwrap().push(value.clone());
            fake.fabric_wake.notify_all();
            match value["method"].as_str() {
                Some("capabilities") => json!(["multiprovider.failoverCompaction.v1"]),
                Some("isIdle") => json!({"isIdle":fake.fabric_idle.load(std::sync::atomic::Ordering::SeqCst)}),
                Some("compact") => json!({"outcome":"complete"}),
                _ => panic!("unexpected Fabric method: {value}"),
            }
        },
        (Some("context"), Some("snapshot")) => {
            let mut snapshot = json!({"hasUI":fake.has_ui,"mode":fake.mode});
            if let Some(model) = fake.snapshot_model.lock().unwrap().clone() {
                snapshot["model"] = model;
            }
            fake.command_snapshots
                .lock()
                .unwrap()
                .push(snapshot.clone());
            snapshot
        }
        (Some("context"), Some("inspect")) => {
            let inspect = fake.inspect.lock().unwrap().clone();
            json!({
                "owner":{"id":7,"generation":"generation-1","child":inspect.child},
                "identity":{"sessionId":inspect.session_id,"operationId":"op","purpose":"turn"},
                "scopeId":inspect.scope_id,
                "closing":false
            })
        }
        (Some("providers"), Some("inspect")) => fake.catalog.lock().unwrap().clone().unwrap_or_else(|| json!({
            "revision":1,
            "providers":fake.providers.iter().map(|id| json!({"id":id,"name":id,"builtin":false})).collect::<Vec<_>>(),
        })),
        (Some("providers"), Some("register" | "publish" | "unregister")) => {
            fake.publications.lock().unwrap().push(value.clone());
            if fake.fail_publication.load(std::sync::atomic::Ordering::SeqCst) {
                unsafe { *out = StbString::from_string(json!({"version":1,"error":"publication refused"}).to_string()); }
                return 1;
            }
            let mut catalog = fake.catalog.lock().unwrap();
            let providers = catalog.as_mut().unwrap()["providers"].as_array_mut().unwrap();
            providers.retain(|provider| provider["id"] != value["providerId"]);
            if value["action"] != "unregister" { providers.push(json!({"id":value["providerId"],"models":value["models"]})); }
            json!({"revision":2})
        },
        (Some("auth"), action) => {
            fake.auth_requests.lock().unwrap().push(value.clone());
            match action {
                Some("methods") => {
                    let methods = fake
                        .auth_methods
                        .lock()
                        .unwrap()
                        .get(value["providerId"].as_str().unwrap_or(""))
                        .cloned();
                    match methods {
                        Some(methods) => methods,
                        None => {
                            unsafe {
                                *out = StbString::from_string(
                                    json!({"version":1,"error":"provider is not registered"})
                                        .to_string(),
                                );
                            }
                            return 1;
                        }
                    }
                }
                Some("login") => json!({"kind":"interaction","loginId":"login-1",
                    "interaction":{"kind":"input","title":"Enter the key","secret":true}}),
                Some("continue") => {
                    // The host persists the login's credential into the
                    // requested store account before reporting success.
                    let login = fake
                        .auth_requests
                        .lock()
                        .unwrap()
                        .iter()
                        .rev()
                        .find(|request| request["action"] == "login")
                        .cloned()
                        .unwrap();
                    let (_, store_request, release, free, context) =
                        fake.store.lock().unwrap().clone().unwrap();
                    let call = |action: &str, input: Value| -> Value {
                        let request = json!({"version":1,"storeId":login["storeId"],
                            "providerId":login["providerId"],"accountId":login["accountId"],
                            "action":action,"input":input});
                        let mut reply = StbString::empty();
                        assert_eq!(
                            store_request(
                                StbStringRef::from_str(&request.to_string()),
                                std::ptr::null(),
                                &mut reply,
                                context as *mut c_void,
                            ),
                            0
                        );
                        let owned = unsafe { OwnedString::from_raw(reply, Some(free)) };
                        serde_json::from_str(&owned.to_string_lossy()).unwrap()
                    };
                    // Failures are reported like the host's, never as a
                    // panic across the callback boundary.
                    let fail = |message: String| {
                        unsafe {
                            *out = StbString::from_string(
                                json!({"version":1,"error":message}).to_string(),
                            );
                        }
                        1
                    };
                    let locked = call("lock", Value::Null);
                    if locked["kind"] != "locked" || !locked["payload"]["credential"].is_null() {
                        return fail(format!("unexpected lock reply {locked}"));
                    }
                    let lock = locked["payload"]["lockId"].as_str().unwrap().to_owned();
                    let replaced = call(
                        "replace",
                        json!({"lockId":lock,"credential":{"type":"api_key","key":value["value"]}}),
                    );
                    release(StbStringRef::from_str(&lock), context as *mut c_void);
                    if replaced["kind"] != "success" {
                        return fail(format!("credential store refused the login: {replaced}"));
                    }
                    json!({"kind":"success"})
                }
                Some("cancelLogin") => json!({"kind":"cancelled"}),
                _ => {
                    unsafe {
                        *out = StbString::from_string(
                            json!({"version":1,"error":"unknown authentication action"})
                                .to_string(),
                        );
                    }
                    return 1;
                }
            }
        }
        (Some("dialogs"), Some("show")) => {
            fake.command_requests.lock().unwrap().push(value.clone());
            if let Some(reply) = fake.dialog_replies.lock().unwrap().pop_front() {
                unsafe {
                    *out = StbString::from_string(
                        json!({"version":1,"result":reply}).to_string(),
                    );
                }
                return 0;
            }
            if fake.hang_dialog.load(std::sync::atomic::Ordering::SeqCst) {
                fake.dialog_waiting
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                while !unsafe { control.as_ref() }
                    .is_some_and(|control| (control.is_cancelled)(control.user_data) != 0)
                {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                fake.dialog_waiting
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                unsafe {
                    *out = StbString::from_string(
                        json!({"version":1,"error":"native dialog request cancelled"}).to_string(),
                    );
                }
                return 1;
            }
            match value["ui"]["kind"].as_str() {
                Some("select") => {
                    if fake
                        .cancel_dialog
                        .load(std::sync::atomic::Ordering::SeqCst)
                    {
                        Value::Null
                    } else {
                        json!({"type":"select","values":["0"]})
                    }
                }
                Some("confirm") => json!({"confirmed":true}),
                _ => Value::Null,
            }
        }
        (Some("ui"), Some("notify")) => {
            fake.command_requests.lock().unwrap().push(value.clone());
            Value::Null
        }
        (Some("sessions"), Some("capture")) => {
            let entries = fake.entries.lock().unwrap().clone();
            json!({"origin":"origin-1","snapshot":{"entries":entries,"branches":[],"revision":1}})
        }
        (Some("sessions"), Some("journalAppend")) => {
            if fake
                .fail_journal
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                unsafe {
                    *out = StbString::from_string(
                        json!({"version":1,"error":"fake journal append refused"}).to_string(),
                    );
                }
                return 1;
            }
            fake.appended.lock().unwrap().push(value["data"].clone());
            if fake
                .corrupt_store_after_journal
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                // The pin (or its clearing) and its journal entry are already
                // durable here; the announcement rebuilds the pool from the
                // store, which this makes unreadable.
                std::fs::write(fake.agent.join("multiprovider-auth.json"), b"{not json")
                    .unwrap();
            }
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

extern "C" fn fake_register_command(
    manifest: StbStringRef,
    handler: AuthRequestFn,
    free: FreeStringFn,
    context: *mut c_void,
) -> i32 {
    let fake = current_fake();
    let manifest: Value = serde_json::from_str(unsafe { manifest.as_str() }).unwrap();
    let mut commands = fake.commands.lock().unwrap();
    let fail_at = fake
        .fail_command_on_nth
        .load(std::sync::atomic::Ordering::SeqCst);
    if fail_at != 0 && commands.len() + 1 == fail_at {
        // Model the host's transaction rollback: prior registrations are no
        // longer callable when this callback returns failure.
        commands.clear();
        return 1;
    }
    commands.push((manifest, handler, free, context as usize));
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

extern "C" fn fake_declare(raw: StbStringRef) -> i32 {
    let manifest: Value = serde_json::from_str(unsafe { raw.as_str() }).unwrap();
    assert_eq!(manifest["providers"][0]["id"], "multiprovider-virtual-base");
    assert_eq!(manifest["providers"][0]["models"], json!([]));
    0
}
extern "C" fn fake_register_base(
    id: StbStringRef,
    _: StbStringRef,
    _: StbStringRef,
    _: rpi_plugin_sdk::ProviderStreamRequestFn,
    _: FreeStringFn,
    _: *mut c_void,
) -> i32 {
    assert_eq!(unsafe { id.as_str() }, "multiprovider-virtual-base");
    0
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
        declare: Some(fake_declare),
        register_before_agent_start: None,
        register_oauth_provider: None,
        register_streaming_provider: Some(fake_register_base),
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
        register_native_command: Some(fake_register_command),
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
    commands: Vec<(Value, AuthRequestFn, FreeStringFn, usize)>,
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
        commands: fake_ref.commands.lock().unwrap().clone(),
    }
}

fn request(fake: *mut Fake, registered: &Registered, provider: &str) -> (Value, Vec<Value>) {
    unsafe { &*fake }.delegates.lock().unwrap().clear();
    unsafe { &*fake }.emitted.lock().unwrap().clear();
    let request = json!({
        "model":{"id":"probe-model","provider":provider,"api":"faux","baseUrl":"https://example.invalid"},
        "context":{"messages":[]},
        "options":{"streamKind":"streamSimple","apiKey":"source-secret","headers":{"authorization":"source-secret"},"env":{"SOURCE_SECRET":"source-secret"},"identity":{"sessionId":"forged"}},
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
    let requests = unsafe { &*fake }.continuations.lock().unwrap();
    let resolved: Vec<_> = requests
        .iter()
        .filter(|request| request["action"] == "resolveAuth")
        .map(|request| request["accountId"].as_str().unwrap().to_owned())
        .collect();
    for account in &resolved {
        assert!(
            requests
                .iter()
                .any(|request| request["action"] == "releaseAuth"
                    && request["receipt"] == format!("receipt:{account}")),
            "receipt was not released: {account}"
        );
    }
    resolved
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
    assert_eq!(
        unsafe { &*registered.fake }
            .commands
            .lock()
            .unwrap()
            .iter()
            .map(|(m, ..)| m["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "multilogin",
            "multilogout",
            "accounts",
            "switch-account",
            "vprovider"
        ]
    );
    for tag in [
        EventTag::SessionStart,
        EventTag::BeforeAgentStart,
        EventTag::SessionShutdown,
    ] {
        assert!(tags.contains(&(tag as u64)), "missing event {tag:?}");
    }
    finish(registered.fake);
}

fn invoke_registered_command(registered: &Registered, name: &str) -> Value {
    invoke_registered_command_args(registered, name, "")
}

fn invoke_registered_command_args(registered: &Registered, name: &str, args: &str) -> Value {
    let (_, callback, free, context) = registered
        .commands
        .iter()
        .find(|(manifest, ..)| manifest["name"] == name)
        .unwrap();
    let input = json!({"version":1,"command":name,"args":args}).to_string();
    let mut out = StbString::empty();
    assert_eq!(
        callback(
            StbStringRef::from_str(&input),
            std::ptr::null(),
            &mut out,
            *context as *mut c_void
        ),
        0
    );
    let owned = unsafe { OwnedString::from_raw(out, Some(*free)) };
    serde_json::from_str(&owned.to_string_lossy()).unwrap()
}

#[test]
fn exported_plugin_runs_accounts_and_logout_against_live_store_and_scheduler() {
    let temp = tempfile_dir();
    let registered = register(&temp, Vec::new(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .fail_provider_inspect
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = invoke_registered_command(&registered, "accounts");
    assert!(result.get("error").is_none(), "{result}");
    assert!(unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["port"] == "ui" && request["action"] == "notify"));
    // This one providers/inspect call belongs to the required reconciliation's
    // provider discovery; NativePort construction adds no catalog preflight.
    assert_eq!(
        unsafe { &*fake }
            .provider_inspections
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    finish(fake);

    let temp = tempfile_dir();
    let upstream_auth = temp.join("auth.json");
    std::fs::write(&upstream_auth, r#"{"provider":"unchanged"}"#).unwrap();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    let result = invoke_registered_command(&registered, "multilogout");
    assert!(result.get("error").is_none(), "{result}");
    let state = std::fs::read_to_string(temp.join("multiprovider-auth.json")).unwrap();
    assert!(
        !state.contains("\"id\":\"a\""),
        "account remains in store: {state}"
    );
    let accounts = invoke_registered_command(&registered, "accounts");
    assert!(accounts.get("error").is_none(), "{accounts}");
    assert!(unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["action"] == "notify"
            && request["text"] == "No account pools are configured. Use /multilogin to add one."));
    assert_eq!(
        std::fs::read_to_string(upstream_auth).unwrap(),
        r#"{"provider":"unchanged"}"#
    );
    finish(fake);
}

fn switch_model() -> Value {
    json!({"provider":"example","id":"probe-model","name":"probe-model"})
}

fn switch_accounts() -> Vec<Value> {
    vec![
        account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        ),
        account(
            "b",
            "B",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        ),
    ]
}

fn notified(fake: *mut Fake, text: &str) -> bool {
    unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| {
            request["port"] == "ui" && request["action"] == "notify" && request["text"] == text
        })
}

#[test]
fn exported_switch_account_pins_and_journals_through_production_registration() {
    let temp = tempfile_dir();
    let registered = register(&temp, switch_accounts(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    let result = invoke_registered_command_args(&registered, "switch-account", "A");
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["result"], Value::Null);
    assert!(
        notified(fake, "Switched to A for this session. Pool settings are unchanged; new requests from this session use it."),
        "successful switch notifies the new account"
    );
    assert_eq!(
        unsafe { &*fake }.appended.lock().unwrap().as_slice(),
        &[json!({"pool":"example","key":"session-parent","accountId":"a","label":"A"})],
        "the pin decision is journaled with the command scope's origin"
    );
    // The pin landed in the scheduler the command reads: an empty reference
    // falls back to the picker with the pinned account marked current.
    let result = invoke_registered_command_args(&registered, "switch-account", "");
    assert!(result.get("error").is_none(), "{result}");
    let picker = unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["port"] == "dialogs")
        .cloned()
        .last()
        .expect("fallback switch shows the account picker");
    let choices = picker["ui"]["choices"].as_array().unwrap();
    assert!(
        choices.iter().any(|choice| choice["label"]
            .as_str()
            .is_some_and(|label| label.contains("A (api-key)") && label.contains("current"))),
        "the pinned account is marked current: {choices:?}"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_without_ui_leaves_store_and_journal_untouched() {
    let temp = tempfile_dir();
    let registered = register(&temp, switch_accounts(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    unsafe { &mut *fake }.has_ui = false;
    let before = std::fs::read(temp.join("multiprovider-auth.json")).unwrap();
    let result = invoke_registered_command_args(&registered, "switch-account", "A");
    assert!(result.get("error").is_none(), "{result}");
    // The shared notify helper stays silent without interactive UI, exactly
    // like the upstream no-UI trace.
    assert!(
        !unsafe { &*fake }
            .command_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["port"] == "dialogs" || request["port"] == "ui"),
        "headless switch emits no UI output"
    );
    assert!(
        unsafe { &*fake }.appended.lock().unwrap().is_empty(),
        "headless switch journals nothing"
    );
    assert_eq!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before,
        "headless switch mutates no stored account"
    );
    assert!(
        !unsafe { &*fake }
            .command_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["port"] == "dialogs"),
        "headless switch shows no dialog"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_journal_failure_reports_without_success_ui() {
    let temp = tempfile_dir();
    let registered = register(&temp, switch_accounts(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    unsafe { &*fake }
        .fail_journal
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = invoke_registered_command_args(&registered, "switch-account", "A");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("fake journal append refused")),
        "journal failure surfaces through the envelope: {result}"
    );
    assert!(
        !notified(fake, "Switched to A for this session. Pool settings are unchanged; new requests from this session use it."),
        "a switch that was not journaled claims no success"
    );
    assert!(
        unsafe { &*fake }.appended.lock().unwrap().is_empty(),
        "refused journal records nothing"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_unknown_provider_reports_no_pools() {
    let temp = tempfile_dir();
    let registered = register(&temp, switch_accounts(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(json!({"provider":"zeta","id":"probe-model","name":"probe-model"}));
    let result = invoke_registered_command_args(&registered, "switch-account", "");
    assert!(result.get("error").is_none(), "{result}");
    assert!(
        notified(
            fake,
            "zeta has no pooled accounts. Use /multilogin to add one."
        ),
        "an unregistered model provider reports no pools"
    );
    assert!(
        unsafe { &*fake }.appended.lock().unwrap().is_empty(),
        "a switch with no pool journals nothing"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_lists_upstream_while_host_reports_unknown_auth() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    // `unknown` is the status the pinned host actually emits: every provider is
    // registered as `AuthStatus::Unknown` and `update_auth_status` has no
    // production caller. Upstream includes the account because the check is
    // `configured !== false`, not `configured === true`.
    unsafe { &*fake }.catalog.lock().unwrap().replace(json!({
        "revision": 2,
        "providers": [{"id":"example","name":"example","builtin":false,"authStatus":"unknown"}]
    }));
    // The shared test store disables the upstream account; this case needs
    // the pool the scheduler would serve once ambient auth exists.
    let store_path = temp.join("multiprovider-auth.json");
    let mut state: Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    state["providers"]["example"]["includeUpstream"] = json!(true);
    std::fs::write(&store_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    let result = invoke_registered_command_args(&registered, "switch-account", "");
    assert!(result.get("error").is_none(), "{result}");
    // The scripted dialog picks Automatic, clearing the session pin.
    assert!(
        notified(fake, "Cleared this session's pinned account. The next request re-selects using the pool strategy."),
        "automatic switch clears the session pin"
    );
    let picker = unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["port"] == "dialogs")
        .cloned()
        .last()
        .expect("automatic switch shows the account picker first");
    let choices = picker["ui"]["choices"].as_array().unwrap();
    assert!(
        choices
            .get(1)
            .and_then(|choice| choice["label"].as_str())
            .is_some_and(|label| label.contains("Pi default (upstream)")),
        "an unknown host status keeps the upstream account switchable: {choices:?}"
    );
    assert_eq!(
        unsafe { &*fake }.appended.lock().unwrap().as_slice(),
        &[json!({"pool":"example","key":"session-parent"})],
        "clearing the pin journals pool and key only"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_hides_upstream_while_host_reports_unauthenticated() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    unsafe { &*fake }.catalog.lock().unwrap().replace(json!({
        "revision": 2,
        "providers": [{"id":"example","name":"example","builtin":false,"authStatus":"unauthenticated"}]
    }));
    let store_path = temp.join("multiprovider-auth.json");
    let mut state: Value =
        serde_json::from_str(&std::fs::read_to_string(&store_path).unwrap()).unwrap();
    state["providers"]["example"]["includeUpstream"] = json!(true);
    std::fs::write(&store_path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    let result = invoke_registered_command_args(&registered, "switch-account", "");
    assert!(result.get("error").is_none(), "{result}");
    let picker = unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| request["port"] == "dialogs")
        .cloned()
        .last()
        .expect("the switch still shows the account picker");
    let choices = picker["ui"]["choices"].as_array().unwrap();
    assert!(
        !choices.iter().any(|choice| choice["label"]
            .as_str()
            .is_some_and(|label| label.contains("Pi default (upstream)"))),
        "an explicit unauthenticated status excludes the upstream account: {choices:?}"
    );
    assert!(
        choices
            .get(1)
            .and_then(|choice| choice["label"].as_str())
            .is_some_and(|label| label.contains("A (api-key)")),
        "the stored account stays switchable: {choices:?}"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_reports_a_failed_announcement_without_failing_the_switch() {
    let temp = tempfile_dir();
    let registered = register(&temp, switch_accounts(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    let store_path = temp.join("multiprovider-auth.json");
    let store = std::fs::read(&store_path).unwrap();
    unsafe { &*fake }
        .corrupt_store_after_journal
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let result = invoke_registered_command_args(&registered, "switch-account", "A");
    // The host refilled nothing; restore the store for the shutdown hooks.
    std::fs::write(&store_path, &store).unwrap();
    assert!(
        result.get("error").is_none(),
        "a failed announcement never fails a pin that is already journaled: {result}"
    );
    assert!(
        notified(fake, "Switched to A for this session. Pool settings are unchanged; new requests from this session use it."),
        "the committed switch still reports success"
    );
    assert_eq!(
        unsafe { &*fake }.appended.lock().unwrap().as_slice(),
        &[json!({"pool":"example","key":"session-parent","accountId":"a","label":"A"})],
        "the pin decision stays journaled"
    );
    assert!(
        unsafe { &*fake }
            .command_requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| request["port"] == "ui"
                && request["kind"] == "warning"
                && request["text"]
                    .as_str()
                    .is_some_and(|text| text.contains("sibling extensions were not notified"))),
        "the lost announcement is reported instead of swallowed silently"
    );
    finish(fake);
}

#[test]
fn exported_switch_account_picker_cancel_leaves_pin_and_journal_untouched() {
    let temp = tempfile_dir();
    let registered = register(&temp, switch_accounts(), None);
    let fake = registered.fake;
    unsafe { &*fake }
        .snapshot_model
        .lock()
        .unwrap()
        .replace(switch_model());
    unsafe { &*fake }
        .cancel_dialog
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let before = std::fs::read(temp.join("multiprovider-auth.json")).unwrap();
    let result = invoke_registered_command_args(&registered, "switch-account", "");
    assert!(result.get("error").is_none(), "{result}");
    assert_eq!(result["result"], Value::Null);
    assert!(
        unsafe { &*fake }.appended.lock().unwrap().is_empty(),
        "cancelled switch journals nothing"
    );
    assert_eq!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before,
        "cancelled switch mutates no stored account"
    );
    finish(fake);
}

#[test]
fn provider_catalog_is_lazy_and_lookup_failures_propagate_only_for_picker() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &*fake }
        .fail_provider_inspect
        .store(true, std::sync::atomic::Ordering::SeqCst);
    unsafe { &mut *fake }.has_ui = false;
    let no_ui = invoke_registered_command(&registered, "multilogout");
    assert!(no_ui.get("error").is_none(), "{no_ui}");
    assert_eq!(
        unsafe { &*fake }
            .provider_inspections
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    unsafe { &mut *fake }.has_ui = true;
    unsafe { &mut *fake }.mode = "rpc".into();
    let rpc = invoke_registered_command(&registered, "multilogout");
    assert!(rpc.get("error").is_none(), "{rpc}");
    assert_eq!(
        unsafe { &*fake }
            .provider_inspections
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    unsafe { &mut *fake }.mode = "tui".into();
    let before = std::fs::read(temp.join("multiprovider-auth.json")).unwrap();
    let direct = invoke_registered_command_args(&registered, "multilogout", "example");
    assert!(direct.get("error").is_none(), "{direct}");
    assert_eq!(
        unsafe { &*fake }
            .provider_inspections
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_ne!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before
    );
    finish(fake);

    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &*fake }
        .fail_provider_inspect
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let before = std::fs::read(temp.join("multiprovider-auth.json")).unwrap();
    let picker = invoke_registered_command(&registered, "multilogout");
    assert!(
        picker["error"]
            .as_str()
            .is_some_and(|error| error.contains("provider inspection failed")),
        "{picker}"
    );
    assert_eq!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before
    );
    assert_eq!(
        unsafe { &*fake }
            .provider_inspections
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    finish(fake);
}

#[test]
fn exported_logout_pending_dialog_cancels_and_preserves_store() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &*fake }
        .hang_dialog
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let before = std::fs::read(temp.join("multiprovider-auth.json")).unwrap();
    let (_, callback, free, context) = registered
        .commands
        .iter()
        .find(|(manifest, ..)| manifest["name"] == "multilogout")
        .unwrap()
        .clone();
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task_cancel = cancelled.clone();
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let control = AuthRequestControl {
            struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
            is_cancelled: command_cancelled,
            user_data: Arc::as_ptr(&task_cancel).cast_mut().cast(),
        };
        let mut out = StbString::empty();
        let status = callback(
            StbStringRef::from_str(r#"{"version":1,"command":"multilogout","args":""}"#),
            &control,
            &mut out,
            context as *mut c_void,
        );
        let owned = unsafe { OwnedString::from_raw(out, Some(free)) };
        let reply: Value = serde_json::from_str(&owned.to_string_lossy()).unwrap();
        let _ = send.send((status, reply));
    });
    let start = std::time::Instant::now();
    while !unsafe { &*fake }
        .dialog_waiting
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "logout never reached pending dialog"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    cancelled.store(true, std::sync::atomic::Ordering::SeqCst);
    let (_, reply) = receive
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("command did not drain after cancellation");
    assert!(
        reply["error"]
            .as_str()
            .is_some_and(|error| error.contains("cancel")),
        "{reply}"
    );
    assert_eq!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before
    );
    finish(fake);
}

#[test]
fn exported_commands_use_snapshot_mode_and_no_ui_without_mutating_store() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "a",
            "A",
            json!({"type":"api_key","key":"local-fixture"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &mut *fake }.mode = "rpc".into();
    let before = std::fs::read(temp.join("multiprovider-auth.json")).unwrap();
    let (_, callback, free, context) = registered
        .commands
        .iter()
        .find(|(manifest, ..)| manifest["name"] == "multilogout")
        .unwrap();
    let control = AuthRequestControl {
        struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
        is_cancelled: cancelled,
        user_data: std::ptr::null_mut(),
    };
    let mut out = StbString::empty();
    assert_eq!(
        callback(
            StbStringRef::from_str(r#"{"version":1,"command":"multilogout","args":""}"#),
            &control,
            &mut out,
            *context as *mut c_void
        ),
        0
    );
    let reply = unsafe { OwnedString::from_raw(out, Some(*free)) };
    let reply: Value = serde_json::from_str(&reply.to_string_lossy()).unwrap();
    assert!(reply["error"].as_str().unwrap().contains("aborted"));
    assert_eq!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before
    );
    let result = invoke_registered_command(&registered, "multilogout");
    assert_eq!(result["result"], Value::Null);
    assert_eq!(
        std::fs::read(temp.join("multiprovider-auth.json")).unwrap(),
        before
    );
    assert!(unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["port"] == "ui" && request["kind"] == "warning"));

    unsafe { &mut *fake }.mode = "tui".into();
    unsafe { &mut *fake }.has_ui = false;
    unsafe { &mut *fake }
        .command_requests
        .lock()
        .unwrap()
        .clear();
    let result = invoke_registered_command(&registered, "accounts");
    assert_eq!(result["result"], Value::Null);
    assert!(!unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["port"] == "dialogs" || request["port"] == "ui"));
    finish(fake);
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
fn partial_native_command_registration_rolls_back_before_runtime_shutdown() {
    let temp = tempfile_dir();
    std::fs::create_dir_all(&*temp).unwrap();
    write_store(&temp.join("multiprovider-auth.json"), Vec::new(), None);
    let fake = Box::into_raw(Box::new(Fake::new(&temp)));
    unsafe { &*fake }
        .fail_command_on_nth
        .store(2, std::sync::atomic::Ordering::SeqCst);
    let api = plugin_api(fake);
    CURRENT.with(|current| current.set(fake as usize));
    let status = rpi_multiprovider::native::rpi_plugin_register(&api);
    CURRENT.with(|current| current.set(0));
    assert_eq!(
        status, 1,
        "failed command registration must reject the extension"
    );
    assert!(
        unsafe { &*fake }.commands.lock().unwrap().is_empty(),
        "host rollback must retire the first command before context release"
    );
    finish(fake);
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

extern "C" fn command_cancelled(data: *mut c_void) -> u8 {
    u8::from(
        unsafe { &*data.cast::<std::sync::atomic::AtomicBool>() }
            .load(std::sync::atomic::Ordering::SeqCst),
    )
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
    let command = unsafe { &*fake }
        .commands
        .lock()
        .unwrap()
        .iter()
        .find(|(manifest, ..)| manifest["name"] == "accounts")
        .cloned()
        .expect("loaded /accounts registration");
    let mut command_out = StbString::empty();
    assert_eq!(
        (command.1)(
            StbStringRef::from_str(r#"{"version":1,"command":"accounts","args":""}"#),
            std::ptr::null(),
            &mut command_out,
            command.3 as *mut c_void
        ),
        0
    );
    let command_reply = unsafe { OwnedString::from_raw(command_out, Some(command.2)) };
    let command_reply: Value = serde_json::from_str(&command_reply.to_string_lossy()).unwrap();
    assert!(
        command_reply.get("error").is_none(),
        "loaded command failed: {command_reply}"
    );
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
struct LoadedPluginGuard {
    handle: *mut c_void,
    fake: *mut Fake,
    close: unsafe extern "C" fn(*mut c_void) -> i32,
}

#[cfg(unix)]
impl Drop for LoadedPluginGuard {
    fn drop(&mut self) {
        let hooks = unsafe { &*self.fake }
            .hooks
            .lock()
            .ok()
            .and_then(|hooks| hooks.clone());
        if let Some((begin, finish, context)) = hooks {
            begin(context as *mut c_void);
            let _ = finish(context as *mut c_void);
        }
        unsafe {
            drop(Box::from_raw(self.fake));
            (self.close)(self.handle);
        }
    }
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
    let _guard = LoadedPluginGuard {
        handle,
        fake,
        close: dlclose,
    };
    let api = plugin_api(fake);
    CURRENT.with(|current| current.set(fake as usize));
    let status = unsafe { register(&api) };
    CURRENT.with(|current| current.set(0));
    assert_eq!(status, 0);
    let registered = unsafe { &*fake };
    let command_registrations = registered.commands.lock().unwrap().clone();
    assert_eq!(
        command_registrations
            .iter()
            .map(|(manifest, ..)| manifest["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "multilogin",
            "multilogout",
            "accounts",
            "switch-account",
            "vprovider"
        ],
        "loaded cdylib must register exactly the supported commands in canonical order"
    );
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

    let (_, command, command_free, command_context) = command_registrations
        .iter()
        .find(|(manifest, ..)| manifest["name"] == "accounts")
        .unwrap();
    let mut command_out = StbString::empty();
    assert_eq!(
        command(
            StbStringRef::from_str(r#"{"version":1,"command":"accounts","args":""}"#),
            std::ptr::null(),
            &mut command_out,
            *command_context as *mut c_void,
        ),
        0,
        "loaded /accounts callback reports through its response envelope"
    );
    let command_reply = unsafe { OwnedString::from_raw(command_out, Some(*command_free)) };
    let command_reply: Value = serde_json::from_str(&command_reply.to_string_lossy()).unwrap();
    assert_eq!(command_reply, json!({"version":1,"result":null}));
    assert_eq!(
        unsafe { &*fake }
            .command_snapshots
            .lock()
            .unwrap()
            .last()
            .cloned(),
        Some(json!({"hasUI":true,"mode":"tui"})),
        "loaded /accounts sees invocation UI/mode snapshot"
    );
    let account_dialog = unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .find(|request| request["port"] == "dialogs")
        .cloned()
        .expect("loaded /accounts must show its account listing");
    assert_eq!(account_dialog["ui"]["kind"], "select");
    let choices = account_dialog["ui"]["choices"].as_array().unwrap();
    assert!(choices.iter().any(|choice| choice["label"]
        .as_str()
        .is_some_and(|label| label.contains("example"))));
    assert!(
        choices.iter().any(|choice| choice["label"]
            .as_str()
            .is_some_and(|label| label.contains("A (api-key)"))),
        "expected pooled-account line in loaded dialog: {account_dialog}"
    );
}

fn auth_requests(fake: *mut Fake) -> Vec<Value> {
    unsafe { &*fake }.auth_requests.lock().unwrap().clone()
}

fn stored_accounts(agent: &Path, provider: &str) -> Vec<Value> {
    let file: Value = serde_json::from_str(
        &std::fs::read_to_string(agent.join("multiprovider-auth.json")).unwrap(),
    )
    .unwrap();
    file["providers"][provider]["accounts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// One registered store callback request, as the host makes it.
fn store_call(
    registered: &Registered,
    provider: &str,
    account: &str,
    action: &str,
    input: Value,
) -> Value {
    let (_, request, _, free, context) = &registered.store;
    let input = json!({"version":1,"storeId":"multiprovider","providerId":provider,
        "accountId":account,"action":action,"input":input});
    let mut out = StbString::empty();
    assert_eq!(
        request(
            StbStringRef::from_str(&input.to_string()),
            std::ptr::null(),
            &mut out,
            *context as *mut c_void
        ),
        0
    );
    let owned = unsafe { OwnedString::from_raw(out, Some(*free)) };
    serde_json::from_str(&owned.to_string_lossy()).unwrap()
}

/// After its login ends, a reservation no longer captures credentials: a
/// replace through the dropped id reaches the account file, which has no such
/// account, and fails without writing.
fn assert_reservation_dropped(registered: &Registered, provider: &str, pending: &str) {
    let locked = store_call(registered, provider, pending, "lock", Value::Null);
    assert_eq!(locked["kind"], "locked", "{locked}");
    let lock = locked["payload"]["lockId"].as_str().unwrap().to_owned();
    let replaced = store_call(
        registered,
        provider,
        pending,
        "replace",
        json!({"lockId":lock,"credential":{"type":"api_key","key":"late"}}),
    );
    let (_, _, release, _, context) = &registered.store;
    release(StbStringRef::from_str(&lock), *context as *mut c_void);
    assert_eq!(replaced["kind"], "error", "{replaced}");
}

#[test]
fn multilogin_adds_an_account_through_a_private_pending_login() {
    let temp = tempfile_dir();
    let registered = register(&temp, Vec::new(), None);
    let fake = registered.fake;
    unsafe { &*fake }.auth_methods.lock().unwrap().insert(
        "example".into(),
        json!([{"id":"key","kind":"api_key","name":"API key"}]),
    );
    *unsafe { &*fake }.dialog_replies.lock().unwrap() = VecDeque::from(vec![
        json!({"type":"open","id":"accounts","query":""}),
        json!({"type":"open","id":"accounts.add","query":""}),
        json!({"type":"select","values":["api_key"]}),
        json!({"type":"input","value":"Work"}),
        json!({"type":"input","value":"sk-pooled"}),
        json!({"type":"cancel"}),
    ]);

    let result = invoke_registered_command(&registered, "multilogin");
    assert!(result.get("error").is_none(), "{result}");

    // The provider without readable methods offers no login, so the only
    // option is selected without a dialog; the host login targets a private
    // pending account of this plugin's store.
    let requests = auth_requests(fake);
    let login = requests
        .iter()
        .find(|request| request["action"] == "login")
        .expect("host login");
    assert_eq!(login["providerId"], "example");
    assert_eq!(login["methodId"], "key");
    assert_eq!(login["storeId"], "multiprovider");
    let pending = login["accountId"].as_str().unwrap().to_owned();
    assert!(
        pending.starts_with(credentials::PENDING_ACCOUNT_PREFIX),
        "{pending}"
    );
    assert!(requests
        .iter()
        .any(|request| request["action"] == "continue" && request["value"] == "sk-pooled"));
    assert!(!requests
        .iter()
        .any(|request| request["action"] == "cancelLogin"));

    // The captured credential is committed through the store's `add`, under a
    // minted account id; the reservation itself is never persisted.
    let accounts = stored_accounts(&temp, "example");
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    assert_eq!(accounts[0]["label"], "Work");
    assert_eq!(
        accounts[0]["credential"],
        json!({"type":"api_key","key":"sk-pooled"})
    );
    assert_ne!(accounts[0]["id"], json!(pending));
    let path = temp.join("multiprovider-auth.json");
    assert!(notified(
        fake,
        &format!(
            "Added Work to example. Credentials saved to {}",
            path.display()
        )
    ));
    // The secret prompt was masked and titled by the login dialog.
    assert!(unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["ui"]["kind"] == "input"
            && request["ui"]["masked"] == true
            && request["ui"]["title"] == "Add example account · Enter the key"));
    assert_reservation_dropped(&registered, "example", &pending);
    assert_eq!(stored_accounts(&temp, "example").len(), 1);
    finish(fake);
}

#[test]
fn multilogin_reauthenticates_a_stored_account_through_a_pending_login() {
    let temp = tempfile_dir();
    let registered = register(
        &temp,
        vec![account(
            "team",
            "Team",
            json!({"type":"api_key","key":"sk-old"}),
            true,
        )],
        None,
    );
    let fake = registered.fake;
    unsafe { &*fake }.auth_methods.lock().unwrap().insert(
        "example".into(),
        json!([{"id":"key","kind":"api_key","name":"API key"}]),
    );
    *unsafe { &*fake }.dialog_replies.lock().unwrap() = VecDeque::from(vec![
        json!({"type":"open","id":"accounts","query":""}),
        json!({"type":"open","id":"stored.team","query":""}),
        json!({"type":"open","id":"account.team.reauth","query":""}),
        json!({"type":"select","values":["api_key"]}),
        json!({"type":"input","value":"sk-new"}),
        json!({"type":"cancel"}),
    ]);

    let result = invoke_registered_command(&registered, "multilogin");
    assert!(result.get("error").is_none(), "{result}");

    // Persist-first: the host login never targets the stored account; the
    // stored credential changes only through the command's own `replace`.
    let requests = auth_requests(fake);
    let login = requests
        .iter()
        .find(|request| request["action"] == "login")
        .expect("host login");
    let pending = login["accountId"].as_str().unwrap().to_owned();
    assert!(
        pending.starts_with(credentials::PENDING_ACCOUNT_PREFIX),
        "{pending}"
    );
    let accounts = stored_accounts(&temp, "example");
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    assert_eq!(accounts[0]["id"], "team");
    assert_eq!(accounts[0]["label"], "Team");
    assert_eq!(
        accounts[0]["credential"],
        json!({"type":"api_key","key":"sk-new"})
    );
    let path = temp.join("multiprovider-auth.json");
    assert!(notified(
        fake,
        &format!(
            "Reauthenticated Team for example. Credentials saved to {}",
            path.display()
        )
    ));
    assert!(unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["ui"]["kind"] == "input"
            && request["ui"]["masked"] == true
            && request["ui"]["title"] == "Reauthenticate Team · Enter the key"));
    assert_reservation_dropped(&registered, "example", &pending);
    finish(fake);
}

#[test]
fn multilogin_pastes_a_key_when_the_host_reports_no_interactive_login() {
    let temp = tempfile_dir();
    let registered = register(&temp, Vec::new(), None);
    let fake = registered.fake;
    unsafe { &*fake }.auth_methods.lock().unwrap().insert(
        "example".into(),
        json!([{"id":"key","kind":"api_key","name":"API key","interactiveLogin":false}]),
    );
    *unsafe { &*fake }.dialog_replies.lock().unwrap() = VecDeque::from(vec![
        json!({"type":"open","id":"accounts","query":""}),
        json!({"type":"open","id":"accounts.add","query":""}),
        json!({"type":"select","values":["api_key_paste"]}),
        json!({"type":"input","value":"Work"}),
        json!({"type":"input","value":"  sk-pasted  "}),
        json!({"type":"cancel"}),
    ]);

    let result = invoke_registered_command(&registered, "multilogin");
    assert!(result.get("error").is_none(), "{result}");

    // Pi's api_key_paste prompts directly; the host login is never started.
    assert!(!auth_requests(fake)
        .iter()
        .any(|request| request["action"] == "login"));
    let accounts = stored_accounts(&temp, "example");
    assert_eq!(accounts.len(), 1, "{accounts:?}");
    assert_eq!(
        accounts[0]["credential"],
        json!({"type":"api_key","key":"sk-pasted"})
    );
    // Pi's `promptApiKeyCredential` (src/multilogin.ts:350) prompts through
    // `ctx.ui.input("Enter " + name + ":", "")`, and `ui.input` has no masking
    // option at all (pi-coding-agent `dist/core/extensions/types.d.ts`:78,
    // `ExtensionUIDialogOptions` holds only `signal`/`timeout`), so the pasted
    // key dialog is titled by the method name and is not masked.
    assert!(unsafe { &*fake }
        .command_requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request["ui"]["kind"] == "input"
            && request["ui"]["masked"] == false
            && request["ui"]["title"] == "Enter API key:"));
    finish(fake);
}

#[test]
fn cancelled_multilogin_cancels_the_host_login_and_stores_nothing() {
    let temp = tempfile_dir();
    let registered = register(&temp, Vec::new(), None);
    let fake = registered.fake;
    unsafe { &*fake }.auth_methods.lock().unwrap().insert(
        "example".into(),
        json!([{"id":"key","kind":"api_key","name":"API key"}]),
    );
    *unsafe { &*fake }.dialog_replies.lock().unwrap() = VecDeque::from(vec![
        json!({"type":"open","id":"accounts","query":""}),
        json!({"type":"open","id":"accounts.add","query":""}),
        json!({"type":"select","values":["api_key"]}),
        json!({"type":"input","value":"Work"}),
        json!({"type":"cancel"}),
        json!({"type":"cancel"}),
    ]);

    let result = invoke_registered_command(&registered, "multilogin");
    assert!(result.get("error").is_none(), "{result}");

    let requests = auth_requests(fake);
    let pending = requests
        .iter()
        .find(|request| request["action"] == "login")
        .and_then(|request| request["accountId"].as_str())
        .unwrap()
        .to_owned();
    assert!(requests
        .iter()
        .any(|request| request["action"] == "cancelLogin" && request["loginId"] == "login-1"));
    assert!(!requests
        .iter()
        .any(|request| request["action"] == "continue"));
    assert!(stored_accounts(&temp, "example").is_empty());
    assert_reservation_dropped(&registered, "example", &pending);
    assert!(stored_accounts(&temp, "example").is_empty());
    finish(fake);
}

#[test]
fn switch_account_never_reads_login_methods() {
    let temp = tempfile_dir();
    let registered = register(&temp, Vec::new(), None);
    let fake = registered.fake;
    let _ = invoke_registered_command(&registered, "switch-account");
    assert!(auth_requests(fake).is_empty());
    finish(fake);
}

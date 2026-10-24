//! Native command acceptance: registration, dispatch envelope, the dialogs/UI
//! port projection, cancellation, and the upstream traces.
//!
//! The fake host below answers `context/snapshot`, `dialogs/show` and
//! `ui/notify` exactly as the real native host does, records the observed
//! control/notification order, and hands the scripted replies back. The command
//! flows therefore run through the same `Dialogs` adapter the loaded library
//! uses. Each scenario from the M-CMD-A1/M-CMD-A2 differential fixtures is
//! replayed through `{version:1,command,args}` and its complete trace,
//! notifications and error are compared with the fixture the unchanged upstream
//! extension produced.
use rpi_multiprovider::commands::port::{
    AuthMethod, AuthStatus, LoginInteraction, PoolAction, PoolBuffer, PoolMethod, Port,
    ProviderInfo, RuntimeView, SelectedModel,
};
use rpi_multiprovider::commands::vprovider::{CandidateModel, CandidateProvider, VproviderSources};
use rpi_multiprovider::commands::Ui;
use rpi_multiprovider::native::commands::{self, Dialogs, Runner, COMMANDS};
use rpi_plugin_sdk::{
    callbacks::free_string, AuthRequestControl, AuthRequestFn, FreeStringFn, PluginApi, StbString,
    StbStringRef,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, VecDeque},
    ffi::c_void,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};

struct ProbeRunner(Arc<Mutex<Vec<Value>>>);
impl Runner for ProbeRunner {
    fn run<'a>(
        &'a self,
        client: rpi_plugin_sdk::provider::native::Client,
        ui: Dialogs,
        signal: CancellationToken,
        command: String,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        let seen = self.0.clone();
        Box::pin(async move {
            let result = client
                .request(
                    json!({"port":"context","action":"snapshot"}),
                    signal.clone(),
                )
                .await?;
            seen.lock()
                .unwrap()
                .push(json!({"command":command,"args":args,
                "hasUI":ui.has_ui(),"cancelled":signal.is_cancelled(),"snapshot":result}));
            Ok(())
        })
    }
}
use tokio_util::sync::CancellationToken;

const COMMANDS_FIXTURE: &str = include_str!("commands-reference.json");
const A2_FIXTURE: &str = include_str!("commands-a2-reference.json");
const MANIFEST_FIXTURE: &str = include_str!("m_native_cmd-reference.json");

/// Scenarios from the two upstream-derived fixtures that the native layer can
/// replay without a second store: they read the session view, the provider
/// registry and the service snapshot, and answer dialogs. Store mutations stay
/// covered by the M-CMD-A1/M-CMD-A2 suites.
const SCENARIOS: [(&str, &str); 16] = [
    ("commands", "multilogin no UI rejects without output"),
    ("commands", "multilogin non-TUI mode notifies"),
    ("commands", "multilogin unknown provider reference"),
    ("commands", "multilogin no providers with login methods"),
    ("commands", "multilogin provider selector cancel"),
    ("commands", "multilogout no UI rejects without output"),
    ("commands", "multilogout non-TUI mode notifies"),
    ("commands", "multilogout without stored accounts"),
    ("commands", "accounts without pools"),
    ("commands", "accounts without UI prints nothing"),
    ("commands", "accounts lists every provider"),
    ("commands", "switch-account no UI rejects without output"),
    ("commands", "switch-account without a selected model"),
    ("a2", "vprovider requires interactive mode"),
    ("a2", "vprovider requires tui mode"),
    ("a2", "dismisses the root dialog"),
];

// ---------------------------------------------------------------------------
// Fake native host
// ---------------------------------------------------------------------------

struct Host {
    has_ui: bool,
    mode: String,
    answers: Mutex<VecDeque<Value>>,
    trace: Mutex<Vec<Value>>,
    notifications: Mutex<Vec<Value>>,
    calls: Mutex<Vec<String>>,
    requests: Mutex<Vec<Value>>,
    waiting: AtomicBool,
}

impl Host {
    fn new(has_ui: bool, mode: &str, answers: Vec<Value>) -> Self {
        Self {
            has_ui,
            mode: mode.to_owned(),
            answers: Mutex::new(answers.into_iter().collect()),
            trace: Mutex::new(Vec::new()),
            notifications: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            waiting: AtomicBool::new(false),
        }
    }
}

extern "C" fn host_native_request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    if out.is_null() || context.is_null() {
        return 2;
    }
    let host = unsafe { &*context.cast::<Host>() };
    let request: Value = match serde_json::from_str(unsafe { input.as_str() }) {
        Ok(value) => value,
        Err(_) => return 2,
    };
    host.requests.lock().unwrap().push(request.clone());
    let cancelled = || {
        unsafe { control.as_ref() }
            .is_some_and(|control| (control.is_cancelled)(control.user_data) != 0)
    };
    let (status, response) = if request["port"] == "context" && request["action"] == "snapshot" {
        (
            0,
            json!({"version":1,"result":{"hasUI":host.has_ui,"mode":host.mode}}),
        )
    } else if request["port"] == "dialogs" {
        host.calls.lock().unwrap().push("dialogs".into());
        let answer = host.answers.lock().unwrap().pop_front();
        match answer {
            Some(answer) if answer["__hang"] == true => {
                host.waiting.store(true, Ordering::SeqCst);
                while !cancelled() {
                    std::thread::sleep(Duration::from_millis(1));
                }
                (
                    1,
                    json!({"version":1,"error":"native dialog request cancelled"}),
                )
            }
            Some(answer) => {
                host.trace
                    .lock()
                    .unwrap()
                    .push(json!({"control":request["ui"],"reply":answer}));
                (0, json!({"version":1,"result":answer}))
            }
            None => (1, json!({"version":1,"error":"unscripted native dialog"})),
        }
    } else if request["port"] == "ui" {
        host.calls.lock().unwrap().push("notify".into());
        let entry = json!({"text":request["text"],"kind":request["kind"]});
        host.notifications.lock().unwrap().push(entry.clone());
        host.trace.lock().unwrap().push(json!({"notify":entry}));
        (0, json!({"version":1,"result":null}))
    } else {
        (1, json!({"version":1,"error":"unexpected native port"}))
    };
    unsafe {
        *out = StbString::from_string(response.to_string());
    }
    status
}

/// Operation 0 hands back the same state pointer; the leased handle is opaque.
extern "C" fn host_native_context(operation: u32, context: *mut c_void) -> *mut c_void {
    if operation == 0 {
        context
    } else {
        std::ptr::null_mut()
    }
}

#[derive(Clone)]
struct Registration {
    manifest: Value,
    request: AuthRequestFn,
    context: usize,
}

static REGISTRATIONS: Mutex<Vec<Registration>> = Mutex::new(Vec::new());
static FAIL_REGISTRATION: AtomicBool = AtomicBool::new(false);
/// The fake registrar receives no context, so registrations are collected in a
/// process-global list. Tests run serially to keep that list scenario-local.
static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

extern "C" fn host_register_native_command(
    manifest: StbStringRef,
    request: AuthRequestFn,
    _: FreeStringFn,
    context: *mut c_void,
) -> i32 {
    if FAIL_REGISTRATION.load(Ordering::SeqCst) {
        return 1;
    }
    let manifest: Value = serde_json::from_str(unsafe { manifest.as_str() }).unwrap();
    REGISTRATIONS.lock().unwrap().push(Registration {
        manifest,
        request,
        context: context as usize,
    });
    0
}

extern "C" fn noop_runtime_action(
    _: u32,
    _: StbStringRef,
    _: *mut StbString,
    _: *mut c_void,
) -> i32 {
    0
}

fn api(host: *mut Host) -> PluginApi {
    PluginApi {
        abi_version: 4,
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
        runtime_action: noop_runtime_action,
        dispatch_event: None,
        user_data: host.cast(),
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
        native_request: Some(host_native_request),
        register_provider_runtime: None,
        register_event_result_handler: None,
        register_service: None,
        native_context: Some(host_native_context),
        register_native_command: Some(host_register_native_command),
        register_owned_tool_with_wake: None,
    }
}

extern "C" fn test_cancelled(user_data: *mut c_void) -> u8 {
    u8::from(unsafe { &*(user_data.cast::<AtomicBool>()) }.load(Ordering::SeqCst))
}

fn invoke(registration: &Registration, request: &Value, cancelled: &AtomicBool) -> Value {
    let control = AuthRequestControl {
        struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
        is_cancelled: test_cancelled,
        user_data: (cancelled as *const AtomicBool).cast_mut().cast(),
    };
    let input = request.to_string();
    let mut out = StbString::empty();
    let status = (registration.request)(
        StbStringRef::from_str(&input),
        &control,
        &mut out,
        registration.context as *mut c_void,
    );
    assert_eq!(status, 0, "callback must report through its JSON envelope");
    let text = unsafe { out.to_string_lossy() };
    unsafe { free_string(out) };
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("invalid callback JSON: {text}"))
}

fn invoke_short_control(registration: &Registration, request: &Value) -> Value {
    let control = AuthRequestControl {
        struct_size: 4,
        is_cancelled: test_cancelled,
        user_data: std::ptr::null_mut(),
    };
    let input = request.to_string();
    let mut out = StbString::empty();
    let status = (registration.request)(
        StbStringRef::from_str(&input),
        &control,
        &mut out,
        registration.context as *mut c_void,
    );
    assert_eq!(status, 0);
    let text = unsafe { out.to_string_lossy() };
    unsafe { free_string(out) };
    serde_json::from_str(&text).unwrap()
}

// ---------------------------------------------------------------------------
// Fake command host (Port + VproviderSources)
// ---------------------------------------------------------------------------

struct FakePort {
    world: Value,
    catalog: Value,
    providers: Vec<String>,
    runtime: Option<Value>,
    model: Option<SelectedModel>,
    snapshot: Option<Value>,
    mode: String,
}

fn method(value: Option<&Value>) -> Option<AuthMethod> {
    value
        .filter(|value| value.is_object())
        .map(|value| AuthMethod {
            name: value["name"].as_str().unwrap_or("").to_owned(),
            login_label: value["loginLabel"].as_str().map(str::to_owned),
            login: !value["login"].is_null(),
        })
}

impl FakePort {
    fn from(input: &Value, world: &Value, catalog: &Value) -> Self {
        let providers = input["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let model = input["model"].as_object().map(|model| SelectedModel {
            provider: model["provider"].as_str().unwrap_or("").to_owned(),
            id: model["id"].as_str().unwrap_or("").to_owned(),
            name: model["name"].as_str().unwrap_or("").to_owned(),
        });
        Self {
            world: world.clone(),
            catalog: catalog.clone(),
            providers,
            runtime: (!input["runtime"].is_null()).then(|| input["runtime"].clone()),
            model,
            snapshot: (!input["serviceSnapshot"].is_null())
                .then(|| input["serviceSnapshot"].clone()),
            mode: input["mode"].as_str().unwrap_or("").to_owned(),
        }
    }
    fn info(&self, id: &str) -> ProviderInfo {
        let definition = &self.world[id];
        ProviderInfo {
            id: id.to_owned(),
            name: definition["name"].as_str().unwrap_or("").to_owned(),
            oauth: method(definition.get("oauth")),
            api_key: method(definition.get("apiKey")),
        }
    }
}

impl Port for FakePort {
    fn mode(&self) -> String {
        self.mode.clone()
    }
    fn session_id(&self) -> String {
        "session-1".into()
    }
    fn store_path(&self) -> String {
        String::new()
    }
    fn store(
        &self,
        request: Value,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        async move {
            match request["op"].as_str() {
                Some("list") | Some("listVirtual") => Ok(json!([])),
                Some("pool") => Ok(Value::Null),
                other => Err(format!("unexpected store operation {other:?}")),
            }
        }
    }
    fn reconcile(&self) -> impl std::future::Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn snapshot(&self) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        let snapshot = self.snapshot.clone();
        async move { snapshot.ok_or_else(|| "snapshot was not recorded".to_owned()) }
    }
    fn registered_providers(&self) -> Vec<ProviderInfo> {
        self.providers.iter().map(|id| self.info(id)).collect()
    }
    fn installed_provider_name(&self, _: &str) -> Option<String> {
        None
    }
    fn provider_name(&self, id: &str) -> Option<String> {
        self.providers
            .iter()
            .any(|candidate| candidate == id)
            .then(|| self.info(id).name)
    }
    fn is_virtual_provider(&self, _: &str) -> bool {
        false
    }
    fn virtual_provider_name(&self, _: &str) -> Option<String> {
        None
    }
    fn is_virtual_pool(&self, _: &str, _: &str) -> bool {
        false
    }
    fn runtime(&self) -> Option<RuntimeView> {
        let spec = self.runtime.as_ref()?;
        let providers = spec["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| self.providers.iter().any(|candidate| candidate == id))
            .map(|id| self.info(id))
            .collect();
        let auth = spec["auth"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(id, status)| {
                (
                    id.clone(),
                    AuthStatus {
                        configured: status["configured"] == true,
                        label: status["label"].as_str().map(str::to_owned),
                        source: status["source"].as_str().map(str::to_owned),
                    },
                )
            })
            .collect();
        let oauth = spec["oauth"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(_, using)| **using == true)
            .map(|(id, _)| id.clone())
            .collect();
        Some(RuntimeView {
            providers,
            auth,
            oauth,
        })
    }
    fn has_integration(&self, _: &str) -> bool {
        false
    }
    fn model(&self) -> Option<SelectedModel> {
        self.model.clone()
    }
    fn service_has_provider(&self, _: &str) -> bool {
        false
    }
    fn reset_health(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn affinity(&self, _: &str, _: &str) -> Option<(String, bool)> {
        None
    }
    fn pin_account(
        &self,
        _: &str,
        _: &str,
        _: &str,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        async { Err("unscripted pin".into()) }
    }
    fn clear_affinity(&self, _: &str, _: &str) {}
    fn announce_active_account(
        &self,
        _: &str,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send {
        async { Err("unscripted announcement".into()) }
    }
    fn append_entry(
        &self,
        _: &str,
        _: Value,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn login<I: LoginInteraction>(
        &self,
        _: &ProviderInfo,
        _: &str,
        _: &I,
    ) -> impl std::future::Future<Output = Result<Value, String>> + Send {
        async { Err("unscripted login".into()) }
    }
    fn pool_manager(
        &self,
        _: &ProviderInfo,
        _: &[PoolMethod],
        _: &mut PoolBuffer,
    ) -> impl std::future::Future<Output = Result<PoolAction, String>> + Send {
        async { Err("unscripted pool manager".into()) }
    }
}

impl VproviderSources for FakePort {
    fn providers_with_models(&self) -> Vec<CandidateProvider> {
        self.providers
            .iter()
            .map(|id| CandidateProvider {
                id: id.clone(),
                name: self.info(id).name,
                models: self.catalog[id]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|model| CandidateModel {
                        id: model["id"].as_str().unwrap_or("").to_owned(),
                        name: model["name"].as_str().unwrap_or("").to_owned(),
                        model: model.clone(),
                    })
                    .collect(),
            })
            .collect()
    }
    fn provider_registered(&self, _: &str) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

fn assert_same(path: &str, actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Number(left), Value::Number(right)) => {
            assert_eq!(left.as_f64(), right.as_f64(), "{path}");
        }
        (Value::Array(left), Value::Array(right)) => {
            assert_eq!(left.len(), right.len(), "{path}: array length");
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                assert_same(&format!("{path}[{index}]"), left, right);
            }
        }
        (Value::Object(left), Value::Object(right)) => {
            assert_eq!(
                left.keys().collect::<BTreeSet<_>>(),
                right.keys().collect::<BTreeSet<_>>(),
                "{path}: keys"
            );
            for (key, right) in right {
                assert_same(&format!("{path}.{key}"), &left[key], right);
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
}

fn scenario<'a>(fixture: &'a Value, name: &str) -> &'a Value {
    fixture["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .find(|scenario| scenario["name"] == name)
        .unwrap_or_else(|| panic!("missing scenario {name}"))
}

/// Registers the commands against `port` and returns the live handle with the
/// captured registrations. The handle must outlive every invocation: it owns the
/// callback contexts.
fn register(host: &Host, port: FakePort) -> (commands::Commands<FakePort>, Vec<Registration>) {
    REGISTRATIONS.lock().unwrap().clear();
    let api = api(host as *const Host as *mut Host);
    let mut handle =
        commands::Commands::new(&api, Arc::new(port), CancellationToken::new()).unwrap();
    handle.register().unwrap();
    let registrations = REGISTRATIONS.lock().unwrap();
    let registrations = registrations
        .iter()
        .map(|registration| Registration {
            manifest: registration.manifest.clone(),
            request: registration.request,
            context: registration.context,
        })
        .collect();
    (handle, registrations)
}

fn registration_for<'a>(registrations: &'a [Registration], command: &str) -> &'a Registration {
    registrations
        .iter()
        .find(|registration| registration.manifest["name"] == command)
        .unwrap_or_else(|| panic!("missing registration for {command}"))
}

fn run_fixture_scenario(
    fixture: &Value,
    catalog: &Value,
    name: &str,
) -> (Vec<Value>, Vec<Value>, Value) {
    let scenario = scenario(fixture, name);
    let input = &scenario["input"];
    let host = Host::new(
        input["hasUI"] == true,
        input["mode"].as_str().unwrap_or(""),
        input["answers"].as_array().cloned().unwrap_or_default(),
    );
    let port = FakePort::from(input, &fixture["world"], catalog);
    let (_handle, registrations) = register(&host, port);
    let registration = registration_for(&registrations, input["command"].as_str().unwrap());
    let cancelled = AtomicBool::new(false);
    let output = invoke(
        registration,
        &json!({"version":1,"command":input["command"],"args":input["args"]}),
        &cancelled,
    );
    let trace = host.trace.lock().unwrap().clone();
    let notifications = host.notifications.lock().unwrap().clone();
    (trace, notifications, output)
}

#[test]
fn native_commands_match_the_upstream_manifests() {
    let _serial = serial();
    let fixture: Value = serde_json::from_str(MANIFEST_FIXTURE).unwrap();
    let manifest = &fixture["commands"];
    assert_eq!(manifest.as_array().unwrap().len(), COMMANDS.len());
    let host = Host::new(true, "tui", vec![]);
    let (_handle, registrations) = register(
        &host,
        FakePort::from(
            &json!({"providers":[],"runtime":null}),
            &json!({}),
            &json!({}),
        ),
    );
    assert_eq!(registrations.len(), COMMANDS.len());
    for (index, command) in COMMANDS.iter().enumerate() {
        let registered = &registrations[index].manifest;
        assert_eq!(registered["version"], 1);
        assert_eq!(registered["name"], command.name);
        assert_same(
            &format!("commands[{index}]"),
            &json!({"name":registered["name"],"description":registered["description"]}),
            &manifest[index],
        );
        assert_eq!(
            command.description,
            manifest[index]["description"].as_str().unwrap()
        );
    }
}

#[test]
fn native_command_flows_match_the_upstream_fixtures() {
    let _serial = serial();
    let commands_fixture: Value = serde_json::from_str(COMMANDS_FIXTURE).unwrap();
    let a2_fixture: Value = serde_json::from_str(A2_FIXTURE).unwrap();
    for (source, name) in SCENARIOS {
        let (fixture, catalog) = if source == "commands" {
            (&commands_fixture, &Value::Null)
        } else {
            (&a2_fixture, &a2_fixture["catalog"])
        };
        let scenario = scenario(fixture, name);
        let (trace, notifications, output) = run_fixture_scenario(fixture, catalog, name);
        assert_same(
            &format!("{name}: trace"),
            &Value::Array(trace),
            &scenario["expected"]["trace"],
        );
        assert_same(
            &format!("{name}: notifications"),
            &Value::Array(notifications),
            &scenario["expected"]["notifications"],
        );
        match scenario["expected"]["error"].as_str() {
            Some(error) => assert_eq!(output["error"], error, "{name}: error"),
            None => {
                assert!(
                    output.get("error").is_none(),
                    "{name}: unexpected error {output}"
                );
                assert_eq!(output["result"], Value::Null, "{name}: result");
            }
        }
    }
}

#[test]
fn native_dialog_cancel_type_is_a_cancel() {
    let _serial = serial();
    let fixture: Value = serde_json::from_str(COMMANDS_FIXTURE).unwrap();
    let name = "multilogin provider selector cancel";
    let scenario = scenario(&fixture, name);
    let input = &scenario["input"];
    let host = Host::new(true, "tui", vec![json!({"type":"cancel"})]);
    let port = FakePort::from(input, &fixture["world"], &Value::Null);
    let (_handle, registrations) = register(&host, port);
    let registration = registration_for(&registrations, "multilogin");
    let output = invoke(
        registration,
        &json!({"version":1,"command":"multilogin","args":""}),
        &AtomicBool::new(false),
    );
    assert!(output.get("error").is_none(), "{output}");
    assert_eq!(output["result"], Value::Null);
    assert!(host.notifications.lock().unwrap().is_empty());
    assert_eq!(host.trace.lock().unwrap().len(), 1);
}

#[test]
fn native_command_envelope_rejects_bad_requests() {
    let _serial = serial();
    let host = Host::new(true, "tui", vec![]);
    let port = FakePort::from(
        &json!({"providers":[],"runtime":null,"mode":"tui"}),
        &json!({}),
        &json!({}),
    );
    let (_handle, registrations) = register(&host, port);
    let registration = registration_for(&registrations, "accounts");
    let cancelled = AtomicBool::new(false);
    for (request, message) in [
        (
            json!({"version":2,"command":"accounts"}),
            "Unsupported Multiprovider command version",
        ),
        (json!({"version":1}), "Missing Multiprovider command name"),
        (
            json!({"version":1,"command":"nope"}),
            "Unknown Multiprovider command \"nope\"",
        ),
        (
            json!({"version":1,"command":"accounts","args":7}),
            "Invalid Multiprovider command arguments",
        ),
    ] {
        let output = invoke(registration, &request, &cancelled);
        assert_eq!(output["error"], message, "{request}");
    }
    let short = invoke_short_control(registration, &json!({"version":1,"command":"accounts"}));
    assert_eq!(short["error"], "Short Multiprovider cancellation control");

    // Malformed JSON is rejected before any host port is touched.
    let control = AuthRequestControl {
        struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
        is_cancelled: test_cancelled,
        user_data: std::ptr::null_mut(),
    };
    let mut out = StbString::empty();
    let status = (registration.request)(
        StbStringRef::from_str("{not json"),
        &control,
        &mut out,
        registration.context as *mut c_void,
    );
    assert_eq!(status, 0);
    let text = unsafe { out.to_string_lossy() };
    unsafe { free_string(out) };
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["error"],
        "Invalid Multiprovider command request"
    );
}

#[test]
fn registration_requires_the_native_command_contract() {
    let _serial = serial();
    let host = Host::new(true, "tui", vec![]);
    let port = FakePort::from(
        &json!({"providers":[],"runtime":null,"mode":"tui"}),
        &json!({}),
        &json!({}),
    );
    let mut plugin_api = api(&host as *const Host as *mut Host);
    plugin_api.register_native_command = None;
    let error = match commands::Commands::<FakePort>::new(
        &plugin_api,
        Arc::new(port),
        CancellationToken::new(),
    ) {
        Ok(_) => panic!("a host without owned commands must be refused"),
        Err(error) => error,
    };
    assert!(error.contains("owned native commands"), "{error}");

    let host = Host::new(true, "tui", vec![]);
    let port = FakePort::from(
        &json!({"providers":[],"runtime":null,"mode":"tui"}),
        &json!({}),
        &json!({}),
    );
    let mut plugin_api = api(&host as *const Host as *mut Host);
    plugin_api.native_request = None;
    let error = match commands::Commands::<FakePort>::new(
        &plugin_api,
        Arc::new(port),
        CancellationToken::new(),
    ) {
        Ok(_) => panic!("a host without the native runtime port must be refused"),
        Err(error) => error,
    };
    assert!(error.contains("newer rpi host"), "{error}");

    let host = Host::new(true, "tui", vec![]);
    let port = FakePort::from(
        &json!({"providers":[],"runtime":null,"mode":"tui"}),
        &json!({}),
        &json!({}),
    );
    REGISTRATIONS.lock().unwrap().clear();
    let api = api(&host as *const Host as *mut Host);
    let mut handle =
        commands::Commands::new(&api, Arc::new(port), CancellationToken::new()).unwrap();
    FAIL_REGISTRATION.store(true, Ordering::SeqCst);
    let error = handle.register().unwrap_err();
    FAIL_REGISTRATION.store(false, Ordering::SeqCst);
    assert!(error.contains("could not register /multilogin"), "{error}");
}

#[test]
fn native_ports_receive_the_documented_envelopes() {
    let _serial = serial();
    let fixture: Value = serde_json::from_str(COMMANDS_FIXTURE).unwrap();
    let input = &scenario(&fixture, "accounts without pools")["input"];
    let host = Host::new(true, "tui", vec![]);
    let port = FakePort::from(input, &fixture["world"], &Value::Null);
    let (_handle, registrations) = register(&host, port);
    let registration = registration_for(&registrations, "accounts");
    let output = invoke(
        registration,
        &json!({"version":1,"command":"accounts","args":""}),
        &AtomicBool::new(false),
    );
    assert!(output.get("error").is_none(), "{output}");
    assert_same(
        "requests",
        &Value::Array(host.requests.lock().unwrap().clone()),
        &json!([
            {"version":1,"port":"context","action":"snapshot"},
            {"version":1,"port":"ui","action":"notify","text":"No account pools are configured. Use /multilogin to add one.","kind":"info"},
        ]),
    );
}

#[test]
fn a_detached_ui_shows_no_controls_and_sends_no_notifications() {
    let _serial = serial();
    let fixture: Value = serde_json::from_str(COMMANDS_FIXTURE).unwrap();
    let input = &scenario(&fixture, "accounts without UI prints nothing")["input"];
    let host = Host::new(false, "tui", vec![]);
    let port = FakePort::from(input, &fixture["world"], &Value::Null);
    let (_handle, registrations) = register(&host, port);
    let registration = registration_for(&registrations, "accounts");
    let output = invoke(
        registration,
        &json!({"version":1,"command":"accounts","args":""}),
        &AtomicBool::new(false),
    );
    assert!(output.get("error").is_none(), "{output}");
    assert!(
        host.calls.lock().unwrap().is_empty(),
        "{:?}",
        host.calls.lock().unwrap()
    );
    // Only the authoritative view query reaches the host.
    assert_eq!(host.requests.lock().unwrap().len(), 1);
    assert_eq!(host.requests.lock().unwrap()[0]["action"], "snapshot");
}

#[test]
fn cancellation_stops_a_pending_dialog() {
    let _serial = serial();
    let fixture: Value = serde_json::from_str(COMMANDS_FIXTURE).unwrap();
    let input = &scenario(&fixture, "multilogin provider selector cancel")["input"];
    let host = Arc::new(Host::new(true, "tui", vec![json!({"__hang":true})]));
    let port = FakePort::from(input, &fixture["world"], &Value::Null);
    let (_handle, registrations) = register(&host, port);
    let registration = registration_for(&registrations, "multilogin").clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancelled.clone();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let output = invoke(
            &registration,
            &json!({"version":1,"command":"multilogin","args":""}),
            &worker_cancel,
        );
        let _ = send.send(output);
    });
    let started = Instant::now();
    while !host.waiting.load(Ordering::SeqCst) {
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the command never reached the dialog"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    cancelled.store(true, Ordering::SeqCst);
    let output = receive
        .recv_timeout(Duration::from_secs(10))
        .expect("cancellation did not stop the command");
    let error = output["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{output}"));
    assert!(error.contains("cancel"), "{error}");
    assert!(host
        .calls
        .lock()
        .unwrap()
        .iter()
        .any(|call| call == "dialogs"));
}

#[test]
fn the_reference_check_is_part_of_the_gate() {
    let _serial = serial();
    // The fixture is generated from the unchanged upstream extension; this
    // test keeps the documented regeneration command honest when the Rust
    // suite runs without Node (the reference check is run separately).
    let fixture: Value = serde_json::from_str(MANIFEST_FIXTURE).unwrap();
    assert_eq!(
        fixture["reference"]["upstream"],
        "bd97d65a36f541935dc91e33ba15d7910a30f9d7"
    );
    assert_eq!(fixture["reference"]["pi"], "1.3.0");
    assert_eq!(fixture["pinned"].as_object().unwrap().len(), 14);
}

#[test]
fn custom_runner_subset_registers_in_canonical_order_and_receives_scoped_invocation() {
    let _serial = serial();
    REGISTRATIONS.lock().unwrap().clear();
    let host = Host::new(true, "tui", vec![]);
    let api = api(&host as *const Host as *mut Host);
    let observed = Arc::new(Mutex::new(Vec::new()));
    let mut handle = commands::Commands::<()>::with_runner(
        &api,
        Arc::new(ProbeRunner(observed.clone())),
        CancellationToken::new(),
        vec!["accounts", "multilogout"],
    )
    .unwrap();
    handle.register().unwrap();
    let registrations = REGISTRATIONS.lock().unwrap();
    assert_eq!(
        registrations
            .iter()
            .map(|r| r.manifest["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["multilogout", "accounts"]
    );
    let accounts = registrations
        .iter()
        .find(|r| r.manifest["name"] == "accounts")
        .unwrap();
    let output = invoke(
        accounts,
        &json!({"version":1,"command":"accounts","args":"hello"}),
        &AtomicBool::new(false),
    );
    assert_eq!(output["result"], Value::Null);
    assert_eq!(
        observed.lock().unwrap()[0],
        json!({"command":"accounts","args":"hello",
        "hasUI":true,"cancelled":false,"snapshot":{"hasUI":true,"mode":"tui"}})
    );
    assert_eq!(
        host.requests.lock().unwrap().len(),
        2,
        "callback captures invocation snapshot and scoped runner request"
    );
}

#[test]
fn unsupported_subset_is_refused_before_registration_and_unadvertised_dispatch_before_host_calls() {
    let _serial = serial();
    REGISTRATIONS.lock().unwrap().clear();
    let host = Host::new(true, "tui", vec![]);
    let api = api(&host as *const Host as *mut Host);
    let observed = Arc::new(Mutex::new(Vec::new()));
    assert!(commands::Commands::<()>::with_runner(
        &api,
        Arc::new(ProbeRunner(observed.clone())),
        CancellationToken::new(),
        vec!["made-up"]
    )
    .is_err());
    assert!(REGISTRATIONS.lock().unwrap().is_empty());

    let mut handle = commands::Commands::<()>::with_runner(
        &api,
        Arc::new(ProbeRunner(observed.clone())),
        CancellationToken::new(),
        vec!["accounts"],
    )
    .unwrap();
    handle.register().unwrap();
    let registration = REGISTRATIONS.lock().unwrap()[0].clone();
    let calls_before = host.requests.lock().unwrap().len();
    let output = invoke(
        &registration,
        &json!({"version":1,"command":"multilogin","args":""}),
        &AtomicBool::new(false),
    );
    assert!(output["error"]
        .as_str()
        .unwrap()
        .contains("Unknown Multiprovider command"));
    assert_eq!(host.requests.lock().unwrap().len(), calls_before);
    assert!(observed.lock().unwrap().is_empty());
}

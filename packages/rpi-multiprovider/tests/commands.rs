// Replays tests/commands-reference.json (generated from the unchanged upstream
// extension) through the Rust command flows. Each scenario gets a fake Ui that
// replays the scripted native dialog answers and records the projected trace,
// and a fake Port backed by the real account store in a temporary directory.
// The complete trace, notifications, appended session entries, final
// multiprovider-auth.json and thrown message are compared with the fixture.
use rpi_multiprovider::commands::login::{self, LoginResult};
use rpi_multiprovider::commands::port::{
    locale_time, AuthMethod, AuthStatus, LoginInteraction, PoolAction, PoolBuffer, PoolMethod,
    Port, ProviderInfo, RuntimeView, SelectedModel,
};
use rpi_multiprovider::commands::{accounts, logout, switch, Ui};
use rpi_multiprovider::store::{Failure, Store};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const SESSION_PIN_ENTRY: &str = "pi-multiprovider:switch-account";

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn failure_text(failure: Failure) -> String {
    match failure {
        Failure::Public(error) => error.to_string(),
        Failure::Syntax { line, column, .. } => format!("syntax error at {line}:{column}"),
        Failure::Io {
            operation, error, ..
        } => format!("{operation}: {error}"),
    }
}

/// Everything a scenario records: the projected trace, appended entries and
/// the normalized path of the account file.
struct Run {
    world: Value,
    auth_path: String,
    trace: Mutex<Vec<Value>>,
    answers: Mutex<VecDeque<Value>>,
    pool_actions: Mutex<VecDeque<Value>>,
    entries: Mutex<Vec<Value>>,
}

impl Run {
    fn norm(&self, text: &str) -> String {
        if self.auth_path.is_empty() {
            return text.to_owned();
        }
        text.replace(&self.auth_path, "$AUTH_PATH")
    }
    fn show(&self, control: Value) -> Result<Option<Value>, String> {
        let reply = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| format!("unscripted control {control}"))?;
        self.trace
            .lock()
            .unwrap()
            .push(json!({"control": control, "reply": reply}));
        Ok(if reply.is_null() { None } else { Some(reply) })
    }
}

struct FakeUi {
    has_ui: bool,
    run: Arc<Run>,
}

impl Ui for FakeUi {
    fn has_ui(&self) -> bool {
        self.has_ui
    }
    fn show(&self, control: Value) -> impl Future<Output = Result<Option<Value>, String>> + Send {
        let run = self.run.clone();
        async move { run.show(control) }
    }
    fn notify(&self, text: &str, kind: &str) -> impl Future<Output = Result<(), String>> + Send {
        let run = self.run.clone();
        let entry = json!({"notify": {"text": run.norm(text), "kind": kind}});
        let has_ui = self.has_ui;
        async move {
            if has_ui {
                run.trace.lock().unwrap().push(entry);
            }
            Ok(())
        }
    }
}

struct FakePort {
    run: Arc<Run>,
    store: Store,
    mode: String,
    model: Option<SelectedModel>,
    snapshot: Option<Value>,
    runtime: Option<Value>,
    provider_ids: Vec<String>,
    installed: Mutex<BTreeSet<String>>,
    virtuals: Mutex<Vec<Value>>,
    pins: Mutex<HashMap<(String, String), (String, bool)>>,
}

fn method(value: &Value) -> AuthMethod {
    AuthMethod {
        name: value["name"].as_str().unwrap_or("").to_owned(),
        login_label: value["loginLabel"].as_str().map(str::to_owned),
        login: !value["login"].is_null(),
    }
}

impl FakePort {
    /// The registry entry Pi holds for a provider: once its pool is installed
    /// the managed integration replaces the API-key method (`mergeProviderAuth`).
    fn info(&self, id: &str) -> ProviderInfo {
        let definition = &self.run.world[id];
        let oauth = definition.get("oauth").map(method);
        let mut api_key = definition.get("apiKey").map(method);
        if self.installed.lock().unwrap().contains(id) {
            let login = api_key.as_ref().is_some_and(|method| method.login);
            api_key = Some(AuthMethod {
                name: api_key
                    .as_ref()
                    .map(|method| method.name.clone())
                    .unwrap_or_else(|| "Multiprovider account pool".into()),
                login_label: None,
                login,
            });
        }
        ProviderInfo {
            id: id.to_owned(),
            name: definition["name"].as_str().unwrap_or("").to_owned(),
            oauth,
            api_key,
        }
    }
    fn snapshot_account(&self, pool: &str, account: &str) -> Option<Value> {
        let snapshot = self.snapshot.as_ref()?;
        snapshot["providers"]
            .as_array()?
            .iter()
            .find(|candidate| candidate["id"] == pool)?["accounts"]
            .as_array()?
            .iter()
            .find(|candidate| candidate["id"] == account)
            .cloned()
    }
}

fn fill(value: &Value, vars: &HashMap<String, String>) -> Value {
    match value {
        Value::String(text) => {
            let mut out = String::new();
            let mut rest = text.as_str();
            while let Some(start) = rest.find('{') {
                out.push_str(&rest[..start]);
                let after = &rest[start + 1..];
                let end = after.find('}').expect("unterminated placeholder");
                let name = &after[..end];
                let value = vars
                    .get(name)
                    .unwrap_or_else(|| panic!("unknown placeholder {name}"));
                out.push_str(value);
                rest = &after[end + 1..];
            }
            out.push_str(rest);
            Value::String(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| fill(item, vars)).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, item)| (key.clone(), fill(item, vars)))
                .collect::<Map<_, _>>(),
        ),
        other => other.clone(),
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
        self.run.auth_path.clone()
    }
    fn store(&self, request: Value) -> impl Future<Output = Result<Value, String>> + Send {
        async move {
            self.store
                .request(request, CancellationToken::new())
                .await
                .map_err(failure_text)
        }
    }
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send {
        async move {
            let ids = self
                .store
                .request(json!({"op": "list"}), CancellationToken::new())
                .await
                .map_err(failure_text)?;
            let virtuals = self
                .store
                .request(json!({"op": "listVirtual"}), CancellationToken::new())
                .await
                .map_err(failure_text)?;
            *self.installed.lock().unwrap() = ids
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect();
            *self.virtuals.lock().unwrap() = virtuals.as_array().cloned().unwrap_or_default();
            Ok(())
        }
    }
    fn snapshot(&self) -> impl Future<Output = Result<Value, String>> + Send {
        async move {
            self.snapshot
                .clone()
                .ok_or_else(|| "snapshot was not recorded".to_owned())
        }
    }
    fn registered_providers(&self) -> Vec<ProviderInfo> {
        self.provider_ids.iter().map(|id| self.info(id)).collect()
    }
    fn installed_provider_name(&self, id: &str) -> Option<String> {
        if self.installed.lock().unwrap().contains(id) {
            Some(self.info(id).name)
        } else {
            None
        }
    }
    fn provider_name(&self, id: &str) -> Option<String> {
        self.provider_ids
            .iter()
            .any(|candidate| candidate == id)
            .then(|| self.info(id).name)
    }
    fn is_virtual_provider(&self, id: &str) -> bool {
        self.virtuals
            .lock()
            .unwrap()
            .iter()
            .any(|config| config["id"] == id)
    }
    fn virtual_provider_name(&self, id: &str) -> Option<String> {
        self.virtuals
            .lock()
            .unwrap()
            .iter()
            .find(|config| config["id"] == id)
            .and_then(|config| config["label"].as_str().map(str::to_owned))
    }
    fn is_virtual_pool(&self, provider_id: &str, model_id: &str) -> bool {
        self.virtuals.lock().unwrap().iter().any(|config| {
            config["id"] == provider_id
                && config["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|model| model["id"] == model_id)
        })
    }
    fn runtime(&self) -> Option<RuntimeView> {
        let spec = self.runtime.as_ref()?;
        let providers = spec["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| self.provider_ids.iter().any(|candidate| candidate == id))
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
    fn has_integration(&self, provider_id: &str) -> bool {
        self.installed.lock().unwrap().contains(provider_id)
    }
    fn model(&self) -> Option<SelectedModel> {
        self.model.clone()
    }
    fn service_has_provider(&self, id: &str) -> bool {
        self.installed.lock().unwrap().contains(id)
    }
    fn reset_health(&self, _id: &str, _account_id: &str) -> Result<(), String> {
        Ok(())
    }
    fn affinity(&self, pool: &str, key: &str) -> Option<(String, bool)> {
        self.pins
            .lock()
            .unwrap()
            .get(&(pool.to_owned(), key.to_owned()))
            .cloned()
    }
    fn pin_account(
        &self,
        pool: &str,
        key: &str,
        account_id: &str,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async move {
            if self.snapshot.is_none() {
                return Err("snapshot was not recorded".into());
            }
            let account = self.snapshot_account(pool, account_id).ok_or_else(|| {
                format!("multiprovider: provider \"{pool}\" has no account \"{account_id}\"")
            })?;
            if account["enabled"] != true {
                return Err(format!(
                    "multiprovider: account \"{}\" is disabled",
                    account["label"].as_str().unwrap_or("")
                ));
            }
            self.pins.lock().unwrap().insert(
                (pool.to_owned(), key.to_owned()),
                (account_id.to_owned(), true),
            );
            Ok(())
        }
    }
    fn clear_affinity(&self, pool: &str, key: &str) {
        self.pins
            .lock()
            .unwrap()
            .remove(&(pool.to_owned(), key.to_owned()));
    }
    fn announce_active_account(
        &self,
        _pool: &str,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn append_entry(&self, custom_type: &str, data: Value) {
        self.run
            .entries
            .lock()
            .unwrap()
            .push(json!({"customType": custom_type, "data": data}));
    }
    fn login<I: LoginInteraction>(
        &self,
        provider: &ProviderInfo,
        auth_type: &str,
        interaction: &I,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        let key = if auth_type == "oauth" {
            "oauth"
        } else {
            "apiKey"
        };
        let script = self.run.world[provider.id.as_str()][key]["login"].clone();
        async move {
            if script.is_null() {
                return Err(format!("no {auth_type} login script for {}", provider.id));
            }
            let mut vars = HashMap::new();
            for step in script["steps"].as_array().cloned().unwrap_or_default() {
                if !step["notify"].is_null() {
                    interaction.notify(step["notify"].clone()).await?;
                } else {
                    let answer = interaction.prompt(step["prompt"].clone()).await?;
                    vars.insert(step["as"].as_str().unwrap().to_owned(), answer);
                }
            }
            if let Some(failure) = script["fail"].as_str() {
                return Err(failure.to_owned());
            }
            Ok(fill(&script["credential"], &vars))
        }
    }
    fn pool_manager(
        &self,
        provider: &ProviderInfo,
        methods: &[PoolMethod],
        _buffer: &mut PoolBuffer,
    ) -> impl Future<Output = Result<PoolAction, String>> + Send {
        let methods: Vec<Value> = methods
            .iter()
            .map(|method| json!({"label": method.label, "value": method.value}))
            .collect();
        let entry = json!({"pool": {"provider": provider.id, "methods": methods}});
        async move {
            self.run.trace.lock().unwrap().push(entry);
            let action = self
                .run
                .pool_actions
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "unscripted pool manager call".to_owned())?;
            Ok(match action["type"].as_str() {
                Some("add") => PoolAction::Add {
                    method: action["method"].as_str().unwrap().to_owned(),
                },
                Some("reauth") => PoolAction::Reauth {
                    account_id: action["accountId"].as_str().unwrap().to_owned(),
                    method: action["method"].as_str().unwrap().to_owned(),
                },
                _ => PoolAction::Closed,
            })
        }
    }
}

/// Compares two JSON values; numbers compare by value (JS `0` equals `0.0`).
fn assert_same(path: &str, actual: &Value, expected: &Value) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(e)) => assert_eq!(a.as_f64(), e.as_f64(), "{path}"),
        (Value::Array(a), Value::Array(e)) => {
            assert_eq!(a.len(), e.len(), "{path}: array length");
            for (index, (a, e)) in a.iter().zip(e).enumerate() {
                assert_same(&format!("{path}[{index}]"), a, e);
            }
        }
        (Value::Object(a), Value::Object(e)) => {
            assert_eq!(
                a.keys().collect::<BTreeSet<_>>(),
                e.keys().collect::<BTreeSet<_>>(),
                "{path}: keys"
            );
            for (key, e) in e {
                assert_same(&format!("{path}.{key}"), &a[key], e);
            }
        }
        _ => assert_eq!(actual, expected, "{path}"),
    }
}

async fn run_command(world: &Value, scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let input = &scenario["input"];
    let root = Scratch(std::env::temp_dir().join(format!(
        "rpi-multiprovider-commands-{}",
        uuid::Uuid::new_v4()
    )));
    fs::create_dir(&root.0).unwrap();
    let auth_path = root.0.join("multiprovider-auth.json");
    if let Some(initial) = input["initialAuth"].as_str() {
        fs::write(&auth_path, initial).unwrap();
    }

    let sequence = Arc::new(AtomicU64::new(input["idSequence"].as_u64().unwrap()));
    let store = Store::with_sources(
        auth_path.clone(),
        Arc::new(|| 1_700_000_000_000.0),
        Arc::new(move || format!("fixture-{}", sequence.fetch_add(1, Ordering::SeqCst) + 1)),
    );
    let run = Arc::new(Run {
        world: world.clone(),
        auth_path: auth_path.to_string_lossy().into_owned(),
        trace: Mutex::new(Vec::new()),
        answers: Mutex::new(
            input["answers"]
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
        ),
        pool_actions: Mutex::new(
            input["poolActions"]
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .collect(),
        ),
        entries: Mutex::new(Vec::new()),
    });
    let model = input["model"].as_object().map(|model| SelectedModel {
        provider: model["provider"].as_str().unwrap().to_owned(),
        id: model["id"].as_str().unwrap().to_owned(),
        name: model["name"].as_str().unwrap().to_owned(),
    });
    let port = FakePort {
        run: run.clone(),
        store,
        mode: input["mode"].as_str().unwrap_or("tui").to_owned(),
        model,
        snapshot: input
            .get("serviceSnapshot")
            .filter(|value| !value.is_null())
            .cloned(),
        runtime: input
            .get("runtime")
            .filter(|value| !value.is_null())
            .cloned(),
        provider_ids: input["providers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect(),
        installed: Mutex::new(BTreeSet::new()),
        virtuals: Mutex::new(Vec::new()),
        pins: Mutex::new(HashMap::new()),
    };
    // Pi's session_start reconciles once after setup, before the command runs.
    port.reconcile().await.unwrap();
    for entry in input["entries"].as_array().unwrap() {
        if entry["customType"] != SESSION_PIN_ENTRY {
            continue;
        }
        let data = &entry["data"];
        let key = (
            data["pool"].as_str().unwrap().to_owned(),
            data["key"].as_str().unwrap().to_owned(),
        );
        match data["accountId"].as_str() {
            Some(account) => {
                port.pins
                    .lock()
                    .unwrap()
                    .insert(key, (account.to_owned(), true));
            }
            None => {
                port.pins.lock().unwrap().remove(&key);
            }
        }
    }

    let ui = FakeUi {
        has_ui: input["hasUI"] != false,
        run: run.clone(),
    };
    let args = input["args"].as_str().unwrap();
    let result = match input["command"].as_str().unwrap() {
        "multilogin" => login::multilogin(&ui, &port, args).await,
        "multilogout" => logout::multilogout(&ui, &port, args).await,
        "accounts" => accounts::accounts(&ui, &port).await,
        "switch-account" => switch::switch_account(&ui, &port, args).await,
        other => panic!("{name}: unknown command {other}"),
    };
    assert!(
        run.answers.lock().unwrap().is_empty(),
        "{name}: every scripted answer is consumed"
    );
    assert!(
        run.pool_actions.lock().unwrap().is_empty(),
        "{name}: every scripted pool action is consumed"
    );

    let trace = run.trace.lock().unwrap().clone();
    let notifications: Vec<Value> = trace
        .iter()
        .filter(|item| !item["notify"].is_null())
        .map(|item| item["notify"].clone())
        .collect();
    let auth_file = match fs::read_to_string(&auth_path) {
        Ok(text) => json!(text),
        Err(_) => Value::Null,
    };
    let actual = json!({
        "trace": trace,
        "notifications": notifications,
        "entries": run.entries.lock().unwrap().clone(),
        "authFile": auth_file,
        "error": result.err().map(|message| run.norm(&message)),
    });
    assert_same(name, &actual, &scenario["expected"]);
}

async fn run_direct_login(world: &Value, scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let root = Scratch(std::env::temp_dir().join(format!(
        "rpi-multiprovider-commands-{}",
        uuid::Uuid::new_v4()
    )));
    fs::create_dir(&root.0).unwrap();
    let store = Store::new(root.0.join("multiprovider-auth.json"));
    let run = Arc::new(Run {
        world: world.clone(),
        auth_path: String::new(),
        trace: Mutex::new(Vec::new()),
        answers: Mutex::new(VecDeque::new()),
        pool_actions: Mutex::new(VecDeque::new()),
        entries: Mutex::new(Vec::new()),
    });
    let port = FakePort {
        run: run.clone(),
        store,
        mode: "tui".into(),
        model: None,
        snapshot: None,
        runtime: None,
        provider_ids: world.as_object().unwrap().keys().cloned().collect(),
        installed: Mutex::new(BTreeSet::new()),
        virtuals: Mutex::new(Vec::new()),
        pins: Mutex::new(HashMap::new()),
    };
    let mut flows = Vec::new();
    for flow in scenario["input"]["flows"].as_array().unwrap() {
        let provider = port.info(flow["provider"].as_str().unwrap());
        let auth_type = flow["authType"].as_str().unwrap();
        let interaction = Recorder::new(flow["answers"].as_array().unwrap());
        let result = match port.login(&provider, auth_type, &interaction).await {
            Ok(credential) => json!({"credential": credential}),
            Err(message) => json!({"error": message}),
        };
        flows.push(json!({
            "prompts": interaction.prompts.lock().unwrap().clone(),
            "events": interaction.events.lock().unwrap().clone(),
            "result": result,
        }));
    }
    assert_same(name, &json!({"flows": flows}), &scenario["expected"]);
}

struct Recorder {
    prompts: Mutex<Vec<Value>>,
    events: Mutex<Vec<Value>>,
    answers: Mutex<VecDeque<String>>,
}

impl Recorder {
    fn new(answers: &[Value]) -> Self {
        Self {
            prompts: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            answers: Mutex::new(
                answers
                    .iter()
                    .map(|answer| answer.as_str().unwrap().to_owned())
                    .collect(),
            ),
        }
    }
}

impl LoginInteraction for Recorder {
    fn notify(&self, event: Value) -> impl Future<Output = Result<(), String>> + Send {
        async move {
            self.events.lock().unwrap().push(event);
            Ok(())
        }
    }
    fn prompt(&self, prompt: Value) -> impl Future<Output = Result<String, String>> + Send {
        async move {
            self.prompts.lock().unwrap().push(prompt);
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "unscripted prompt".to_owned())
        }
    }
}

fn dialog_ui(world: &Value, answers: &[Value]) -> (FakeUi, Arc<Run>) {
    let run = Arc::new(Run {
        world: world.clone(),
        auth_path: String::new(),
        trace: Mutex::new(Vec::new()),
        answers: Mutex::new(answers.iter().cloned().collect()),
        pool_actions: Mutex::new(VecDeque::new()),
        entries: Mutex::new(Vec::new()),
    });
    (
        FakeUi {
            has_ui: true,
            run: run.clone(),
        },
        run,
    )
}

async fn run_host_select(scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let input = &scenario["input"];
    let (ui, run) = dialog_ui(&Value::Null, input["answers"].as_array().unwrap());
    let options = input["options"].clone();
    let dialog = login::LoginDialog::new(&ui, input["title"].as_str().unwrap());
    let outcome = match dialog
        .prompt(json!({"type": "select", "message": input["message"], "options": options}))
        .await
    {
        Ok(value) => json!({"value": value}),
        Err(message) => json!({"error": message}),
    };
    let actual = json!({"trace": run.trace.lock().unwrap().clone(), "outcome": outcome});
    assert_same(name, &actual, &scenario["expected"]);
}

async fn run_login_dialog(world: &Value, scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let input = &scenario["input"];
    let (ui, run) = dialog_ui(world, input["answers"].as_array().unwrap());
    let port = FakePort {
        run: run.clone(),
        store: Store::new(std::env::temp_dir().join(format!(
            "rpi-multiprovider-unused-{}.json",
            uuid::Uuid::new_v4()
        ))),
        mode: "tui".into(),
        model: None,
        snapshot: None,
        runtime: None,
        provider_ids: world.as_object().unwrap().keys().cloned().collect(),
        installed: Mutex::new(BTreeSet::new()),
        virtuals: Mutex::new(Vec::new()),
        pins: Mutex::new(HashMap::new()),
    };
    let provider = port.info(input["provider"].as_str().unwrap());
    let title = input["title"].as_str();
    let result: LoginResult = login::show_login_dialog(
        &ui,
        &port,
        &provider,
        input["authType"].as_str().unwrap(),
        title,
    )
    .await
    .unwrap();
    let outcome = match result {
        None => Value::Null,
        Some(Ok(credential)) => json!({"credential": credential}),
        Some(Err(message)) => json!({"error": message}),
    };
    let actual = json!({"trace": run.trace.lock().unwrap().clone(), "outcome": outcome});
    assert_same(name, &actual, &scenario["expected"]);
}

#[tokio::test]
async fn command_flows_match_the_upstream_reference() {
    // Upstream sorts with the process locale; the fixture was generated under en-US.
    std::env::set_var("LC_ALL", "en_US.UTF-8");
    let reference: Value = serde_json::from_str(include_str!("commands-reference.json")).unwrap();
    let world = reference["world"].clone();
    for scenario in reference["scenarios"].as_array().unwrap() {
        match scenario["kind"].as_str().unwrap() {
            "command" => run_command(&world, scenario).await,
            "direct-login" => run_direct_login(&world, scenario).await,
            "host-select" => run_host_select(scenario).await,
            "login-dialog" => run_login_dialog(&world, scenario).await,
            other => panic!("unknown scenario kind {other}"),
        }
    }
}

#[test]
fn locale_time_matches_en_us_utc() {
    assert_eq!(locale_time(1_700_000_000_000.0), "10:13:20 PM");
    assert_eq!(
        locale_time(1_700_000_000_000.0 + 3_600_000.0 * 13.0 + 61_000.0),
        "11:14:21 AM"
    );
    assert_eq!(
        locale_time(1_700_000_000_000.0 - 1_000.0 * 60.0 * 60.0 * 22.0),
        "12:13:20 AM"
    );
}

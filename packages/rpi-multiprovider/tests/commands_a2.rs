//! Replays tests/commands-a2-reference.json (generated from the unchanged
//! upstream `extensions/pool-manager.ts`, `extensions/settings-ui.ts` and the
//! `/vprovider` handler) through the Rust command ports.
//!
//! Each scenario gets a fake `Ui` that replays the scripted native dialog
//! answers and records the projected trace, and a fake `Port` backed by the real
//! account store in a temporary directory. The complete trace (controls with
//! their replies, store requests, reconcile and scheduler-default calls),
//! notifications, the pool buffer, the pool action, the appended session entries
//! and the final multiprovider-auth.json are compared with the fixture.
use rpi_multiprovider::commands::pool_manager::{self, PoolDefaults};
use rpi_multiprovider::commands::port::{
    AuthStatus, LoginInteraction, PoolAction, PoolBuffer, PoolMethod, Port, ProviderInfo,
    RuntimeView, SelectedModel,
};
use rpi_multiprovider::commands::settings;
use rpi_multiprovider::commands::vprovider::{self, CandidateModel, CandidateProvider, VproviderSources};
use rpi_multiprovider::commands::Ui;
use rpi_multiprovider::store::{Failure, Store};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = include_str!("commands-a2-reference.json");

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

/// Everything a scenario records: the projected trace and the scripted answers.
struct Run {
    auth_path: String,
    /// The pool manager fixture records every store/reconcile call; the
    /// /vprovider fixture records the UI trace only, because the extension's
    /// `reconcile` performs store reads the port abstracts as one call.
    log_calls: bool,
    trace: Mutex<Vec<Value>>,
    answers: Mutex<VecDeque<Value>>,
    world: Value,
    catalog: Value,
    mode: String,
    runtime: Option<Value>,
    provider_ids: Vec<String>,
    installed: Mutex<BTreeSet<String>>,
    virtuals: Mutex<Vec<Value>>,
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
}

impl FakePort {
    fn info(&self, id: &str) -> ProviderInfo {
        ProviderInfo {
            id: id.to_owned(),
            name: self.run.world[id]["name"].as_str().unwrap_or(id).to_owned(),
            oauth: None,
            api_key: None,
        }
    }
    fn is_registered_virtual(&self, id: &str) -> bool {
        self.run
            .virtuals
            .lock()
            .unwrap()
            .iter()
            .any(|config| config["id"] == id)
    }
}

impl Port for FakePort {
    fn mode(&self) -> String {
        self.run.mode.clone()
    }
    fn session_id(&self) -> String {
        "session-1".into()
    }
    fn store_path(&self) -> String {
        self.run.auth_path.clone()
    }
    fn store(&self, request: Value) -> impl Future<Output = Result<Value, String>> + Send {
        let run = self.run.clone();
        let store = self.store.clone();
        async move {
            if run.log_calls {
                run.trace.lock().unwrap().push(json!({"store": request}));
            }
            store
                .request(request, CancellationToken::new())
                .await
                .map_err(failure_text)
        }
    }
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send {
        async move {
            if self.run.log_calls {
                self.run.trace.lock().unwrap().push(json!({"reconcile": true}));
            }
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
            *self.run.installed.lock().unwrap() = ids
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|id| id.as_str().map(str::to_owned))
                .collect();
            *self.run.virtuals.lock().unwrap() = virtuals.as_array().cloned().unwrap_or_default();
            Ok(())
        }
    }
    fn snapshot(&self) -> impl Future<Output = Result<Value, String>> + Send {
        async { Err("snapshot was not recorded".into()) }
    }
    fn registered_providers(&self) -> Vec<ProviderInfo> {
        self.run
            .provider_ids
            .iter()
            .map(|id| self.info(id))
            .collect()
    }
    fn installed_provider_name(&self, id: &str) -> Option<String> {
        self.run
            .installed
            .lock()
            .unwrap()
            .contains(id)
            .then(|| self.info(id).name)
    }
    fn provider_name(&self, id: &str) -> Option<String> {
        self.run
            .provider_ids
            .iter()
            .any(|candidate| candidate == id)
            .then(|| self.info(id).name)
    }
    fn is_virtual_provider(&self, id: &str) -> bool {
        self.is_registered_virtual(id)
    }
    fn virtual_provider_name(&self, id: &str) -> Option<String> {
        self.run
            .virtuals
            .lock()
            .unwrap()
            .iter()
            .find(|config| config["id"] == id)
            .and_then(|config| config["label"].as_str().map(str::to_owned))
    }
    fn is_virtual_pool(&self, provider_id: &str, model_id: &str) -> bool {
        self.run.virtuals.lock().unwrap().iter().any(|config| {
            config["id"] == provider_id
                && config["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|model| model["id"] == model_id)
        })
    }
    fn runtime(&self) -> Option<RuntimeView> {
        let spec = self.run.runtime.as_ref()?;
        let providers = spec["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|id| self.run.provider_ids.iter().any(|candidate| candidate == id))
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
        self.run.installed.lock().unwrap().contains(provider_id)
    }
    fn model(&self) -> Option<SelectedModel> {
        None
    }
    fn service_has_provider(&self, id: &str) -> bool {
        self.run.installed.lock().unwrap().contains(id)
    }
    fn reset_health(&self, _id: &str, _account_id: &str) -> Result<(), String> {
        Ok(())
    }
    fn affinity(&self, _pool: &str, _key: &str) -> Option<(String, bool)> {
        None
    }
    fn pin_account(
        &self,
        _pool: &str,
        _key: &str,
        _account_id: &str,
    ) -> impl Future<Output = Result<(), String>> + Send {
        async { Ok(()) }
    }
    fn clear_affinity(&self, _pool: &str, _key: &str) {}
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
        _provider: &ProviderInfo,
        _auth_type: &str,
        _interaction: &I,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        async { Err("the login flows are covered by the commands suite".into()) }
    }
    fn pool_manager(
        &self,
        _provider: &ProviderInfo,
        _methods: &[PoolMethod],
        _buffer: &mut PoolBuffer,
    ) -> impl Future<Output = Result<PoolAction, String>> + Send {
        // The pool manager is driven directly through `pool_manager::open`.
        async { Err("the pool manager is driven directly in this suite".into()) }
    }
}

impl PoolDefaults for FakePort {
    fn update_scheduler_defaults(&self, settings: Value) {
        if self.run.log_calls {
            self.run
                .trace
                .lock()
                .unwrap()
                .push(json!({"schedulerDefaults": settings}));
        }
    }
}

impl VproviderSources for FakePort {
    fn providers_with_models(&self) -> Vec<CandidateProvider> {
        self.run
            .provider_ids
            .iter()
            .map(|id| CandidateProvider {
                id: id.clone(),
                name: self.info(id).name,
                models: self.run.catalog[id]
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
    fn provider_registered(&self, id: &str) -> bool {
        self.run.provider_ids.iter().any(|candidate| candidate == id) || self.is_registered_virtual(id)
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

fn build_run(scenario: &Value, world: &Value, catalog: &Value, log_calls: bool) -> (Arc<Run>, Store, Scratch) {
    let input = &scenario["input"];
    let root = Scratch(std::env::temp_dir().join(format!(
        "rpi-multiprovider-commands-a2-{}",
        uuid::Uuid::new_v4()
    )));
    fs::create_dir(&root.0).unwrap();
    let auth_path = root.0.join("multiprovider-auth.json");
    if let Some(initial) = input["initialAuth"].as_str() {
        fs::write(&auth_path, initial).unwrap();
    }
    let store = Store::with_sources(
        auth_path.clone(),
        Arc::new(|| 1_700_000_000_000.0),
        Arc::new(|| "fixture-1".into()),
    );
    let run = Arc::new(Run {
        auth_path: auth_path.to_string_lossy().into_owned(),
        log_calls,
        trace: Mutex::new(Vec::new()),
        answers: Mutex::new(
            input["answers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        ),
        world: world.clone(),
        catalog: catalog.clone(),
        mode: input["mode"].as_str().unwrap_or("tui").to_owned(),
        runtime: input.get("runtime").filter(|value| !value.is_null()).cloned(),
        provider_ids: input["providers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|id| id.as_str().map(str::to_owned))
            .collect(),
        installed: Mutex::new(BTreeSet::new()),
        virtuals: Mutex::new(Vec::new()),
        entries: Mutex::new(Vec::new()),
    });
    (run, store, root)
}

fn notifications(trace: &[Value]) -> Vec<Value> {
    trace
        .iter()
        .filter(|item| !item["notify"].is_null())
        .map(|item| item["notify"].clone())
        .collect()
}

fn auth_file(auth_path: &str) -> Value {
    match fs::read_to_string(auth_path) {
        Ok(text) => json!(text),
        Err(_) => Value::Null,
    }
}

fn action_json(action: PoolAction) -> Value {
    match action {
        PoolAction::Closed => json!({"type": "closed"}),
        PoolAction::Add { method } => json!({"type": "add", "method": method}),
        PoolAction::Reauth {
            account_id,
            method,
        } => json!({"type": "reauth", "accountId": account_id, "method": method}),
    }
}

async fn run_pool_screen(world: &Value, catalog: &Value, scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let input = &scenario["input"];
    let (run, store, _scratch) = build_run(scenario, world, catalog, true);
    let port = FakePort {
        run: run.clone(),
        store,
    };
    let ui = FakeUi {
        has_ui: input["hasUI"] != false,
        run: run.clone(),
    };
    let provider_id = input["provider"].as_str().unwrap();
    let provider = ProviderInfo {
        id: provider_id.to_owned(),
        name: world[provider_id]["name"].as_str().unwrap_or(provider_id).to_owned(),
        oauth: None,
        api_key: None,
    };
    let methods: Vec<PoolMethod> = input["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|method| PoolMethod {
            label: method["label"].as_str().unwrap().to_owned(),
            value: method["value"].as_str().unwrap().to_owned(),
        })
        .collect();
    let buffer = &input["initialBuffer"];
    let mut pool_buffer = PoolBuffer {
        policy: buffer["policy"].as_str().unwrap().to_owned(),
        affinity: buffer["affinity"].as_bool().unwrap(),
        include_upstream: buffer["includeUpstream"].as_bool().unwrap(),
        upstream: buffer["upstream"].clone(),
    };
    let result = pool_manager::open(&ui, &port, &provider, &methods, &mut pool_buffer).await;
    let trace = run.trace.lock().unwrap().clone();
    let actual = json!({
        "trace": trace,
        "notifications": notifications(&trace),
        "buffer": {
            "policy": pool_buffer.policy,
            "affinity": pool_buffer.affinity,
            "includeUpstream": pool_buffer.include_upstream,
            "upstream": pool_buffer.upstream,
        },
        "action": match &result { Ok(action) => action_json(action.clone()), Err(_) => Value::Null },
        "authFile": auth_file(&run.auth_path),
        "error": result.err().map(|message| run.norm(&message)),
    });
    assert_same(name, &actual, &scenario["expected"]);
}

async fn run_vprovider(world: &Value, catalog: &Value, scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let input = &scenario["input"];
    let (run, store, _scratch) = build_run(scenario, world, catalog, false);
    let port = FakePort {
        run: run.clone(),
        store,
    };
    // Pi's session_start reconciles once before the command runs.
    port.reconcile().await.unwrap();
    run.trace.lock().unwrap().clear();
    let ui = FakeUi {
        has_ui: input["hasUI"] != false,
        run: run.clone(),
    };
    let result = vprovider::vprovider(&ui, &port, input["args"].as_str().unwrap_or("")).await;
    let trace = run.trace.lock().unwrap().clone();
    let actual = json!({
        "trace": trace,
        "notifications": notifications(&trace),
        "entries": run.entries.lock().unwrap().clone(),
        "authFile": auth_file(&run.auth_path),
        "error": result.err().map(|message| run.norm(&message)),
    });
    assert_same(name, &actual, &scenario["expected"]);
}

fn run_format_ms(scenario: &Value) {
    let name = scenario["name"].as_str().unwrap();
    let values = scenario["input"]["values"].as_array().unwrap();
    let formatted: Vec<Value> = values
        .iter()
        .map(|value| json!(settings::format_ms(value.as_f64().unwrap())))
        .collect();
    assert_same(name, &json!({"formatted": formatted}), &scenario["expected"]);
}

#[tokio::test]
async fn format_ms_matches_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut cases = 0;
    for scenario in fixture["scenarios"].as_array().unwrap() {
        if scenario["kind"] == "format-ms" {
            run_format_ms(scenario);
            cases += 1;
        }
    }
    assert_eq!(cases, 1, "one formatMs scenario");
}

#[tokio::test]
async fn pool_screen_matches_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let world = &fixture["world"];
    let catalog = &fixture["catalog"];
    let mut cases = 0;
    for scenario in fixture["scenarios"].as_array().unwrap() {
        if scenario["kind"] == "pool-screen" {
            run_pool_screen(world, catalog, scenario).await;
            cases += 1;
        }
    }
    assert_eq!(cases, 22, "pool manager scenarios");
}

#[tokio::test]
async fn vprovider_matches_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let world = &fixture["world"];
    let catalog = &fixture["catalog"];
    let mut cases = 0;
    for scenario in fixture["scenarios"].as_array().unwrap() {
        if scenario["kind"] == "vprovider" {
            run_vprovider(world, catalog, scenario).await;
            cases += 1;
        }
    }
    assert_eq!(cases, 24, "vprovider scenarios");
}

//! Native service surface: `register.ts` announcements and the
//! `announcement.ts` methods/topics over the host's versioned private service.
//!
//! The differential fixture is produced by `m_native_svc-reference.mjs` from the
//! unchanged upstream sources. Registration cases replay the real
//! `registerMultiProvider` emits; service cases replay the upstream announcement
//! spec through the same method inputs and notification hooks the native binding
//! uses. Native-only cases pin the envelope validation, publication ordering and
//! the non-secret payload whitelist.

mod common;

use async_trait::async_trait;
use common::*;
use rpi_multiprovider::announcement::{
    ActiveAccount, ActiveAccountChangedEvent, Listener, ServiceContext,
};
use rpi_multiprovider::managed::{
    AccountRequest, AuthResult, ModelRef, NativeProvider, ProviderIntegration,
};
use rpi_multiprovider::native::services::{
    manifest, manifest_json, ContextLookup, Dependencies, NativeServices, Port, ProviderDescriptor,
    METHOD_ACCOUNTS, METHOD_GET_ACTIVE_ACCOUNT, METHOD_PROVIDERS,
    METHOD_RESOLVE_ACTIVE_ACCOUNT_AUTH, SERVICE_ID, SERVICE_VERSION, TOPIC_ACTIVE_ACCOUNT_CHANGED,
    TOPIC_PROVIDER_REGISTERED, TOPIC_PROVIDER_UNREGISTERED,
};
use rpi_multiprovider::services::{Options, Registration, Service};
use rpi_multiprovider::types::{Account, AcquireOptions, LeaseOutcome, SelectionBias};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = include_str!("m_native_svc-reference.json");
const SESSION: &str = "session-1";

#[derive(Clone)]
struct Ctx {
    model: Option<ModelRef>,
}

impl ServiceContext for Ctx {
    fn model(&self) -> Option<ModelRef> {
        self.model.clone()
    }
}

#[derive(Default)]
struct Recorder {
    publications: Mutex<Vec<Value>>,
    failure: Mutex<Option<String>>,
    sequences: AtomicU64,
}

impl Recorder {
    fn published(&self) -> Vec<Value> {
        self.publications.lock().unwrap().clone()
    }

    fn fail_with(&self, message: &str) {
        *self.failure.lock().unwrap() = Some(message.to_owned());
    }
}

#[async_trait]
impl Port for Recorder {
    async fn publish(
        &self,
        service: &str,
        service_version: u32,
        topic: &str,
        value: Value,
    ) -> Result<u64, String> {
        assert_eq!(service, SERVICE_ID);
        assert_eq!(service_version, SERVICE_VERSION);
        if let Some(message) = self.failure.lock().unwrap().clone() {
            return Err(message);
        }
        let sequence = self.sequences.fetch_add(1, Ordering::SeqCst) + 1;
        self.publications
            .lock()
            .unwrap()
            .push(json!({ "topic": topic, "value": value }));
        Ok(sequence)
    }
}

struct Scripted {
    trace: Trace,
    accounts: Mutex<Vec<Account>>,
    accounts_error: Mutex<Option<String>>,
    resolve: Mutex<Map<String, Value>>,
}

#[async_trait]
impl ProviderIntegration for Scripted {
    async fn accounts(&self) -> Result<Vec<Account>, String> {
        if let Some(message) = self.accounts_error.lock().unwrap().clone() {
            return Err(message);
        }
        Ok(self.accounts.lock().unwrap().clone())
    }

    async fn resolve_auth(
        &self,
        account: &Account,
        signal: &CancellationToken,
        request: &AccountRequest,
    ) -> Result<AuthResult, String> {
        push(
            &self.trace,
            json!({
                "integration": "resolveAuth",
                "account": account.id,
                "aborted": signal.is_cancelled(),
                "model": request.model.id,
                "context": request.context,
                "requestOptions": request.request_options,
            }),
        );
        let resolve = self.resolve.lock().unwrap();
        let entry = resolve
            .get(&account.id)
            .ok_or_else(|| format!("unscripted resolve {}", account.id))?;
        if let Some(message) = entry.get("throw").and_then(Value::as_str) {
            return Err(message.to_owned());
        }
        serde_json::from_value(entry["value"].clone()).map_err(|error| error.to_string())
    }
}

fn descriptor(value: &Value) -> ProviderDescriptor {
    serde_json::from_value(value.clone()).unwrap()
}

fn active_json(account: &ActiveAccount) -> Value {
    json!({ "id": account.id, "label": account.label, "authKind": account.auth_kind })
}

fn active_from(value: &Value) -> ActiveAccount {
    ActiveAccount {
        id: value["id"].as_str().unwrap().to_owned(),
        label: value["label"].as_str().unwrap().to_owned(),
        auth_kind: value["authKind"].as_str().unwrap().to_owned(),
    }
}

fn signal_for(step: &Value) -> Option<CancellationToken> {
    match step["signal"].as_str() {
        None | Some("none") => None,
        Some("aborted") => {
            let token = CancellationToken::new();
            token.cancel();
            Some(token)
        }
        Some(_) => Some(CancellationToken::new()),
    }
}

/// A service whose dependencies are deliberately inert; registration cases only
/// exercise the inventory and publication path.
fn inert_services(port: Arc<Recorder>) -> NativeServices<Ctx> {
    NativeServices::new(Dependencies {
        scheduler: Service::new(Options::default()),
        get_integration: Arc::new(|_: &str| None),
        get_base_provider: Arc::new(|_: &str, _: &Ctx| None),
        affinity_key_for: Arc::new(|_: &dyn ProviderIntegration, _: &Ctx, _: &str| {
            Ok(SESSION.to_owned())
        }),
        context: Arc::new(|| None),
        port,
    })
}

async fn registration_actual(case: &Value) -> Value {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port.clone());
    for registration in case["registrations"].as_array().unwrap() {
        services
            .register_provider(descriptor(registration))
            .await
            .unwrap();
    }
    let starts = case["sessionStarts"].as_u64().unwrap();
    for _ in 0..starts {
        services.session_start().await.unwrap();
    }
    json!({
        "name": case["name"],
        "publications": port.published(),
        "inventory": services.providers(),
    })
}

struct Run {
    trace: Trace,
    services: NativeServices<Ctx>,
    handles: Mutex<HashMap<String, Box<dyn Fn() + Send + Sync>>>,
    listeners: Mutex<HashMap<String, Arc<Listener<Ctx>>>>,
    definitions: Value,
}

fn listener_for(run: &Arc<Run>, name: &str) -> Arc<Listener<Ctx>> {
    if let Some(existing) = run.listeners.lock().unwrap().get(name) {
        return existing.clone();
    }
    let owner = run.clone();
    let name_owned = name.to_owned();
    let listener: Arc<Listener<Ctx>> = Arc::new(move |event: &ActiveAccountChangedEvent<Ctx>| {
        let account = match &event.account {
            Some(account) => active_json(account),
            None => Value::Null,
        };
        push(
            &owner.trace,
            json!({
                "listener": name_owned,
                "providerId": event.provider_id,
                "account": account,
                "ctxSessionId": SESSION,
            }),
        );
        let definition = owner
            .definitions
            .get(&name_owned)
            .cloned()
            .unwrap_or(Value::Null);
        for call in definition["calls"].as_array().into_iter().flatten() {
            let handle = call["handle"].as_str().unwrap().to_owned();
            if call["op"] == "subscribe" {
                let listener = listener_for(&owner, call["listener"].as_str().unwrap());
                let unsubscribe = owner
                    .services
                    .on_active_account_changed("example", listener);
                owner.handles.lock().unwrap().insert(handle, unsubscribe);
            } else {
                match owner.handles.lock().unwrap().get(&handle) {
                    Some(unsubscribe) => unsubscribe(),
                    None => return Err("handle is not a function".to_owned()),
                }
            }
        }
        match definition.get("throws").and_then(Value::as_str) {
            Some(message) => Err(message.to_owned()),
            None => Ok(()),
        }
    });
    run.listeners
        .lock()
        .unwrap()
        .insert(name.to_owned(), listener.clone());
    listener
}

async fn run_step(
    step: &Value,
    run: &Arc<Run>,
    scheduler: &Service,
    scripted: &Scripted,
    ctx: &Ctx,
) -> Result<Value, String> {
    let provider = step["provider"].as_str().unwrap_or("example");
    let key = step["key"].as_str().unwrap_or(SESSION);
    match step["op"].as_str().unwrap() {
        "pin" => {
            scheduler
                .pin_account("example", key, step["account"].as_str().unwrap())
                .await
                .map_err(|error| error.to_string())?;
            Ok(Value::Null)
        }
        "acquire" => {
            let mut lease = scheduler
                .acquire(AcquireOptions {
                    provider_id: "example".into(),
                    affinity_key: Some(key.to_owned()),
                    ..Default::default()
                })
                .await
                .map_err(|error| error.to_string())?;
            let account = lease.account.id.clone();
            lease.release(LeaseOutcome::Success);
            Ok(json!(account))
        }
        "setAccounts" => {
            *scripted.accounts.lock().unwrap() =
                serde_json::from_value(step["accounts"].clone()).unwrap();
            Ok(Value::Null)
        }
        "setAccountsError" => {
            *scripted.accounts_error.lock().unwrap() =
                step["message"].as_str().map(str::to_owned);
            Ok(Value::Null)
        }
        "setResolve" => {
            scripted.resolve.lock().unwrap().insert(
                step["accountId"].as_str().unwrap().to_owned(),
                step["entry"].clone(),
            );
            Ok(Value::Null)
        }
        "subscribe" => {
            let listener = listener_for(run, step["listener"].as_str().unwrap());
            let unsubscribe = run
                .services
                .on_active_account_changed(provider, listener);
            run.handles
                .lock()
                .unwrap()
                .insert(step["handle"].as_str().unwrap().to_owned(), unsubscribe);
            Ok(Value::Null)
        }
        "unsubscribe" => {
            let handles = run.handles.lock().unwrap();
            let unsubscribe = handles
                .get(step["handle"].as_str().unwrap())
                .ok_or("handle is not a function")?;
            unsubscribe();
            Ok(Value::Null)
        }
        "notify" => {
            let account = step
                .get("account")
                .filter(|value| !value.is_null())
                .map(active_from);
            run.services
                .notify_active_account_changed(provider, ctx, account)
                .await?;
            Ok(Value::Null)
        }
        "call" => {
            let signal = signal_for(step);
            let envelope = json!({
                "version": 1,
                "service": SERVICE_ID,
                "serviceVersion": SERVICE_VERSION,
                "method": step["method"],
                "input": step.get("input").cloned().unwrap_or(Value::Null),
                "caller": { "id": 1, "child": 0 },
                "identity": {},
            });
            let response: Value =
                serde_json::from_str(&run.services.respond(&envelope.to_string(), signal.as_ref()).await)
                    .unwrap();
            if let Some(error) = response["error"].as_str() {
                return Err(error.to_owned());
            }
            Ok(response["result"].clone())
        }
        other => Err(format!("unknown step {other}")),
    }
}

async fn run_service_case(case: &Value) -> Value {
    let trace: Trace = Arc::default();
    let provider_spec = json!({ "id": "example", "name": "Example", "models": [{ "id": "model" }] });
    let provider: Arc<dyn NativeProvider> = fixture_provider(&provider_spec, &trace);
    let config = &case["integration"];
    let scripted = Arc::new(Scripted {
        trace: trace.clone(),
        accounts: Mutex::new(
            serde_json::from_value(config["accounts"].clone()).unwrap_or_default(),
        ),
        accounts_error: Mutex::new(None),
        resolve: Mutex::new(config["resolve"].as_object().cloned().unwrap_or_default()),
    });
    let integration: Arc<dyn ProviderIntegration> = scripted.clone();
    let mut options = Options {
        now: Arc::new(move || DEFAULT_NOW),
        random_id: Arc::new(|| "lease-1".to_owned()),
        random_int: Arc::new(|_: f64| -> Result<u64, rpi_multiprovider::services::Error> { Ok(1) }),
        ..Default::default()
    };
    if let Some(affinity) = case["affinity"].as_bool() {
        options.affinity = affinity;
    }
    let scheduler = Service::new(options);
    let source = integration.clone();
    scheduler
        .register(Registration {
            id: "example".into(),
            label: "Example".into(),
            accounts: Arc::new(move || {
                let source = source.clone();
                Box::pin(async move { source.accounts().await })
            }),
            classify_failure: None,
            management_hint: None,
            selection_bias: if case["selectionBias"] == "none" {
                SelectionBias::None
            } else {
                SelectionBias::FirstAccount
            },
        })
        .unwrap();

    let ctx = Ctx {
        model: case["ctxModel"].as_str().map(|id| ModelRef {
            id: id.to_owned(),
            base_url: None,
        }),
    };
    let lookup_integration = integration.clone();
    let base_provider = provider.clone();
    let affinity_trace = trace.clone();
    let context: Arc<ContextLookup<Ctx>> = {
        let ctx = ctx.clone();
        Arc::new(move || Some(ctx.clone()))
    };
    let port = Arc::new(Recorder::default());
    let services = NativeServices::new(Dependencies {
        scheduler: scheduler.clone(),
        get_integration: Arc::new(move |id: &str| {
            (id == "example").then(|| lookup_integration.clone())
        }),
        get_base_provider: Arc::new(move |id: &str, _ctx: &Ctx| {
            (id == "example").then(|| base_provider.clone())
        }),
        affinity_key_for: Arc::new(
            move |_integration: &dyn ProviderIntegration, _ctx: &Ctx, id: &str| {
                push(&affinity_trace, json!({ "affinityKeyFor": id }));
                Ok(SESSION.to_owned())
            },
        ),
        context,
        port: port.clone(),
    });
    for registration in case["registrations"].as_array().unwrap() {
        services
            .register_provider(descriptor(registration))
            .await
            .unwrap();
    }
    let run = Arc::new(Run {
        trace: trace.clone(),
        services,
        handles: Mutex::new(HashMap::new()),
        listeners: Mutex::new(HashMap::new()),
        definitions: case["listeners"].clone(),
    });

    let mut results: Vec<Value> = Vec::new();
    for step in case["steps"].as_array().unwrap() {
        let before = trace.lock().unwrap().len();
        let mut outcome = match run_step(step, &run, &scheduler, &scripted, &ctx).await {
            Ok(result) => json!({ "op": step["op"], "result": result }),
            Err(error) => json!({ "op": step["op"], "error": error }),
        };
        if let Some(method) = step["method"].as_str() {
            outcome["method"] = json!(method);
        }
        outcome["trace"] = Value::Array(trace.lock().unwrap()[before..].to_vec());
        results.push(outcome);
    }
    json!({
        "name": case["name"],
        "publications": port.published(),
        "results": results,
    })
}

#[tokio::test]
async fn registration_cases_match_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"]["registration"].as_array().unwrap();
    assert_eq!(cases.len(), 3);
    for case in cases {
        let actual = registration_actual(case).await;
        let expected = json!({
            "name": case["name"],
            "publications": case["publications"],
            "inventory": case["inventory"],
        });
        assert_eq!(
            normalize(&actual),
            normalize(&expected),
            "case {}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn service_cases_match_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"]["service"].as_array().unwrap();
    assert_eq!(cases.len(), 10);
    for case in cases {
        let actual = run_service_case(case).await;
        let expected = json!({
            "name": case["name"],
            "publications": case["publications"],
            "results": case["results"],
        });
        assert_eq!(
            normalize(&actual),
            normalize(&expected),
            "case {}",
            case["name"]
        );
    }
}

fn identity_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 256
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
}

#[test]
fn manifest_matches_the_host_service_contract() {
    let value = manifest();
    assert_eq!(value["version"], 1);
    assert_eq!(value["id"], SERVICE_ID);
    assert_eq!(value["serviceVersion"], SERVICE_VERSION);
    let mut keys: Vec<&String> = value.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        vec!["id", "methods", "serviceVersion", "topics", "version"]
    );
    let methods: Vec<&str> = value["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|method| method.as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        vec![
            METHOD_PROVIDERS,
            METHOD_ACCOUNTS,
            METHOD_GET_ACTIVE_ACCOUNT,
            METHOD_RESOLVE_ACTIVE_ACCOUNT_AUTH,
        ]
    );
    let topics: Vec<&str> = value["topics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|topic| topic.as_str().unwrap())
        .collect();
    assert_eq!(
        topics,
        vec![
            TOPIC_PROVIDER_REGISTERED,
            TOPIC_PROVIDER_UNREGISTERED,
            TOPIC_ACTIVE_ACCOUNT_CHANGED,
        ]
    );
    for name in [SERVICE_ID].iter().chain(methods.iter()).chain(topics.iter()) {
        assert!(identity_ok(name), "{name} is not a host identifier");
    }
    let mut unique = methods.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), methods.len());
    let mut unique = topics.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), topics.len());
    assert_eq!(manifest_json(), value.to_string());
}

async fn response(services: &NativeServices<Ctx>, request: Value) -> Value {
    serde_json::from_str(&services.respond(&request.to_string(), None).await).unwrap()
}

fn envelope(method: &str, input: Value) -> Value {
    json!({
        "version": 1,
        "service": SERVICE_ID,
        "serviceVersion": SERVICE_VERSION,
        "method": method,
        "input": input,
    })
}

#[tokio::test]
async fn envelope_validation_is_explicit() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port);
    let cases = [
        (
            "{not json",
            "multiprovider: invalid service request",
        ),
        (
            r#"[]"#,
            "multiprovider: invalid service request",
        ),
        (
            r#"{"version":2,"service":"pi-multiprovider","serviceVersion":1,"method":"providers"}"#,
            "multiprovider: unsupported service request version",
        ),
        (
            r#"{"version":1,"service":"other","serviceVersion":1,"method":"providers"}"#,
            "multiprovider: unknown service",
        ),
        (
            r#"{"version":1,"service":"pi-multiprovider","serviceVersion":2,"method":"providers"}"#,
            "multiprovider: unknown service version",
        ),
        (
            r#"{"version":1,"service":"pi-multiprovider","serviceVersion":1,"method":7}"#,
            "multiprovider: service method must be a string",
        ),
    ];
    for (request, message) in cases {
        let value: Value = serde_json::from_str(&services.respond(request, None).await).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["error"], message);
        assert_eq!(value.get("result"), None);
    }
    // No request above may publish or touch listeners.
    assert!(services.providers().is_empty());
}

#[tokio::test]
async fn method_and_input_errors_are_refused() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port);
    assert_eq!(
        services
            .call("missing", &json!({}), None)
            .await
            .unwrap_err(),
        "unknown service method \"missing\""
    );
    assert_eq!(
        services
            .call(METHOD_GET_ACTIVE_ACCOUNT, &json!(null), None)
            .await
            .unwrap_err(),
        "multiprovider: service input must be an object"
    );
    assert_eq!(
        services
            .call(METHOD_ACCOUNTS, &json!({}), None)
            .await
            .unwrap_err(),
        "multiprovider: providerId must be a string"
    );
    assert_eq!(
        services
            .call(METHOD_ACCOUNTS, &json!({"providerId": 3}), None)
            .await
            .unwrap_err(),
        "multiprovider: providerId must be a string"
    );
}

#[tokio::test]
async fn methods_require_a_session_context() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port);
    for method in [METHOD_GET_ACTIVE_ACCOUNT, METHOD_RESOLVE_ACTIVE_ACCOUNT_AUTH] {
        assert_eq!(
            services
                .call(method, &json!({"providerId": "example"}), None)
                .await
                .unwrap_err(),
            "multiprovider: session context unavailable"
        );
        let response = response(&services, envelope(method, json!({"providerId": "example"}))).await;
        assert_eq!(
            response["error"],
            "multiprovider: session context unavailable"
        );
    }
}

#[tokio::test]
async fn publication_sequences_are_monotonic_and_unregister_is_idempotent() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port.clone());
    assert_eq!(
        services
            .register_provider(ProviderDescriptor::new("alpha", "Alpha"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        services
            .register_provider(ProviderDescriptor::new("beta", "Beta"))
            .await
            .unwrap(),
        2
    );
    assert_eq!(services.session_start().await.unwrap(), vec![3, 4]);
    assert_eq!(services.unregister_provider("alpha").await.unwrap(), Some(5));
    assert_eq!(services.unregister_provider("alpha").await.unwrap(), None);
    assert_eq!(
        services.providers(),
        vec![ProviderDescriptor::new("beta", "Beta")]
    );
    let topics: Vec<String> = port
        .published()
        .iter()
        .map(|publication| publication["topic"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        topics,
        vec![
            TOPIC_PROVIDER_REGISTERED,
            TOPIC_PROVIDER_REGISTERED,
            TOPIC_PROVIDER_REGISTERED,
            TOPIC_PROVIDER_REGISTERED,
            TOPIC_PROVIDER_UNREGISTERED,
        ]
    );
    // Re-registering beta after removal keeps the announced history one entry.
    services
        .register_provider(ProviderDescriptor::new("beta", "Beta 2"))
        .await
        .unwrap();
    services.session_start().await.unwrap();
    let values: Vec<Value> = port
        .published()
        .iter()
        .filter(|publication| publication["topic"] == TOPIC_PROVIDER_REGISTERED)
        .map(|publication| publication["value"].clone())
        .collect();
    assert_eq!(
        values.last().unwrap(),
        &json!({"id": "beta", "label": "Beta 2"})
    );
}

#[tokio::test]
async fn publication_failures_propagate_but_keep_local_state() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port.clone());
    port.fail_with("publish unavailable");
    assert_eq!(
        services
            .register_provider(ProviderDescriptor::new("alpha", "Alpha"))
            .await
            .unwrap_err(),
        "publish unavailable"
    );
    assert_eq!(
        services.providers(),
        vec![ProviderDescriptor::new("alpha", "Alpha")]
    );
    assert!(port.published().is_empty());
    port.failure.lock().unwrap().take();
    services.session_start().await.unwrap();
    assert_eq!(port.published().len(), 1);
}

#[tokio::test]
async fn notification_fans_out_to_listeners_before_publishing() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port.clone());
    services
        .register_provider(ProviderDescriptor::new("alpha", "Alpha"))
        .await
        .unwrap();
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let sink = seen.clone();
    let listener: Arc<Listener<Ctx>> = Arc::new(move |event: &ActiveAccountChangedEvent<Ctx>| {
        sink.lock().unwrap().push(json!({
            "providerId": event.provider_id,
            "account": event.account.as_ref().map(active_json),
        }));
        Ok(())
    });
    let _unsubscribe = services.on_active_account_changed("alpha", listener);
    let ctx = Ctx { model: None };
    let account = ActiveAccount {
        id: "a".into(),
        label: "Work".into(),
        auth_kind: "oauth".into(),
    };
    services
        .notify_active_account_changed("alpha", &ctx, Some(account.clone()))
        .await
        .unwrap();
    services
        .notify_active_account_changed("alpha", &ctx, None)
        .await
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            json!({"providerId": "alpha", "account": active_json(&account)}),
            json!({"providerId": "alpha", "account": null}),
        ]
    );
    let notifications: Vec<Value> = port
        .published()
        .into_iter()
        .filter(|publication| publication["topic"] == TOPIC_ACTIVE_ACCOUNT_CHANGED)
        .collect();
    assert_eq!(
        notifications,
        vec![
            json!({
                "topic": TOPIC_ACTIVE_ACCOUNT_CHANGED,
                "value": {"providerId": "alpha", "account": active_json(&account)},
            }),
            json!({
                "topic": TOPIC_ACTIVE_ACCOUNT_CHANGED,
                "value": {"providerId": "alpha", "account": null},
            }),
        ]
    );
}

#[tokio::test]
async fn a_failed_publication_still_notifies_in_process_listeners() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port.clone());
    let seen = Arc::new(Mutex::new(0));
    let sink = seen.clone();
    let listener: Arc<Listener<Ctx>> = Arc::new(move |_: &ActiveAccountChangedEvent<Ctx>| {
        *sink.lock().unwrap() += 1;
        Ok(())
    });
    let _unsubscribe = services.on_active_account_changed("alpha", listener);
    port.fail_with("publish unavailable");
    let err = services
        .notify_active_account_changed("alpha", &Ctx { model: None }, None)
        .await
        .unwrap_err();
    assert_eq!(err, "publish unavailable");
    assert_eq!(*seen.lock().unwrap(), 1);
}

#[tokio::test]
async fn account_inventory_is_whitelisted() {
    let port = Arc::new(Recorder::default());
    let trace: Trace = Arc::default();
    let scripted = Arc::new(Scripted {
        trace: trace.clone(),
        accounts: Mutex::new(vec![Account {
            id: "a".into(),
            label: "Work".into(),
            auth_kind: "oauth".into(),
            credential_ref: json!({"access": "secret-access", "refresh": "secret-refresh"}),
            enabled: Some(false),
            weight: Some(7.0),
            priority: Some(2.0),
            metadata: Some(json!({"team": "secret-team"})),
        }]),
        accounts_error: Mutex::new(None),
        resolve: Mutex::new(Map::new()),
    });
    let integration: Arc<dyn ProviderIntegration> = scripted.clone();
    let lookup = integration.clone();
    let services = NativeServices::new(Dependencies {
        scheduler: Service::new(Options::default()),
        get_integration: Arc::new(move |id: &str| (id == "example").then(|| lookup.clone())),
        get_base_provider: Arc::new(|_: &str, _: &Ctx| None),
        affinity_key_for: Arc::new(|_: &dyn ProviderIntegration, _: &Ctx, _: &str| Ok(SESSION.into())),
        context: Arc::new(|| Some(Ctx { model: None })),
        port,
    });
    let accounts = services.accounts("example").await.unwrap();
    assert_eq!(
        accounts,
        json!({
            "providerId": "example",
            "accounts": [{"id": "a", "label": "Work", "authKind": "oauth"}],
        })
    );
    let text = accounts.to_string();
    for secret in [
        "secret-access",
        "secret-refresh",
        "credentialRef",
        "enabled",
        "weight",
        "priority",
        "metadata",
        "secret-team",
    ] {
        assert!(!text.contains(secret), "account inventory exposed {secret}");
    }
    assert_eq!(services.accounts("missing").await.unwrap(), Value::Null);
}

#[tokio::test]
async fn provider_registration_payloads_are_non_secret() {
    let port = Arc::new(Recorder::default());
    let services = inert_services(port.clone());
    let mut descriptor = ProviderDescriptor::new("alpha", "Alpha");
    descriptor.management_hint = Some("Use /multilogin".into());
    descriptor.selection_bias = Some(SelectionBias::None);
    services.register_provider(descriptor).await.unwrap();
    let published = port.published();
    assert_eq!(
        published,
        vec![json!({
            "topic": TOPIC_PROVIDER_REGISTERED,
            "value": {
                "id": "alpha",
                "label": "Alpha",
                "managementHint": "Use /multilogin",
                "selectionBias": "none",
            },
        })]
    );
    let providers = services
        .call(METHOD_PROVIDERS, &Value::Null, None)
        .await
        .unwrap();
    assert_eq!(providers, json!({"providers": [published[0]["value"]]}));
}

#[tokio::test]
async fn absent_optional_descriptor_fields_stay_absent() {
    let descriptor = ProviderDescriptor::new("alpha", "Alpha");
    assert_eq!(
        serde_json::to_value(&descriptor).unwrap(),
        json!({"id": "alpha", "label": "Alpha"})
    );
}

#[test]
fn service_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<NativeServices<Ctx>>();
    assert_send_sync::<Arc<NativeServices<Ctx>>>();
    assert_send_sync::<ProviderDescriptor>();
}

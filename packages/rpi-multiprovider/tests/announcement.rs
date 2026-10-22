mod common;

use async_trait::async_trait;
use common::*;
use rpi_multiprovider::announcement::{
    ActiveAccount, ActiveAccountChangedEvent, AnnouncementDependencies, Listener,
    ServiceAnnouncement, ServiceContext, Unsubscribe,
};
use rpi_multiprovider::managed::{
    AccountRequest, AccountStore, AuthResult, ManagedIntegration, ModelRef, NativeProvider,
    ProviderIntegration,
};
use rpi_multiprovider::services::{Options, Registration, Service};
use rpi_multiprovider::types::{Account, AcquireOptions, LeaseOutcome, SelectionBias};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = include_str!("managed-reference.json");
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

/// The shared run state the listener closures reach back into (`listenerFor`, `handles`).
struct Run {
    trace: Trace,
    announcement: ServiceAnnouncement<Ctx>,
    handles: Mutex<HashMap<String, Unsubscribe>>,
    listeners: Mutex<HashMap<String, Arc<Listener<Ctx>>>>,
    definitions: Value,
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

/// `listenerFor(name)`: one memoised listener per name, scripted by its definition.
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
                    .announcement
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
            let account = step["account"].as_str().unwrap();
            scheduler
                .pin_account("example", key, account)
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
            *scripted.accounts_error.lock().unwrap() = step["message"].as_str().map(str::to_owned);
            Ok(Value::Null)
        }
        "setResolve" => {
            scripted.resolve.lock().unwrap().insert(
                step["accountId"].as_str().unwrap().to_owned(),
                step["entry"].clone(),
            );
            Ok(Value::Null)
        }
        "active" => {
            let active = run.announcement.get_active_account(provider, ctx).await?;
            Ok(active.map_or(Value::Null, |account| active_json(&account)))
        }
        "auth" => {
            let signal = match step["signal"].as_str() {
                None | Some("none") => None,
                Some("aborted") => {
                    let token = CancellationToken::new();
                    token.cancel();
                    Some(token)
                }
                Some(_) => Some(CancellationToken::new()),
            };
            let auth = run
                .announcement
                .resolve_active_account_auth(provider, ctx, signal.as_ref())
                .await?;
            Ok(match auth {
                Some(auth) => serde_json::to_value(auth).unwrap(),
                None => Value::Null,
            })
        }
        "subscribe" => {
            let listener = listener_for(run, step["listener"].as_str().unwrap());
            let unsubscribe = run
                .announcement
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
            run.announcement
                .notify_active_account_changed(provider, ctx, account);
            Ok(Value::Null)
        }
        other => Err(format!("unknown step {other}")),
    }
}

async fn run_case(case: &Value) -> Value {
    let trace: Trace = Arc::default();
    let now = case["now"].as_f64().unwrap_or(DEFAULT_NOW);
    let state = (!case["state"].is_null()).then(|| case["state"].clone());
    let store = Arc::new(FixtureStore::new(trace.clone(), state, now));

    let default_spec = json!({"id": "example", "name": "Example", "models": [{"id": "model"}]});
    let spec = if case["provider"].is_null() {
        default_spec
    } else {
        case["provider"].clone()
    };
    let provider: Arc<dyn NativeProvider> = fixture_provider(&spec, &trace);

    let config = &case["integration"];
    let scripted = Arc::new(Scripted {
        trace: trace.clone(),
        accounts: Mutex::new(
            serde_json::from_value(config["accounts"].clone()).unwrap_or_default(),
        ),
        accounts_error: Mutex::new(None),
        resolve: Mutex::new(config["resolve"].as_object().cloned().unwrap_or_default()),
    });
    let integration: Arc<dyn ProviderIntegration> = if config["kind"] == "managed" {
        Arc::new(ManagedIntegration::new(
            provider.clone(),
            store.clone() as Arc<dyn AccountStore>,
        ))
    } else {
        scripted.clone()
    };

    let mut options = Options {
        now: Arc::new(move || now),
        random_id: Arc::new(|| "lease-1".to_owned()),
        random_int: Arc::new(|_: f64| -> Result<u64, rpi_multiprovider::services::Error> { Ok(1) }),
        ..Default::default()
    };
    if let Some(affinity) = case["affinity"].as_bool() {
        options.affinity = affinity;
    }
    let scheduler = Service::new(options);
    if case["unregistered"] != true {
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
    }

    let ctx = Ctx {
        model: case["ctxModel"].as_str().map(|id| ModelRef {
            id: id.to_owned(),
            base_url: None,
        }),
    };
    let base_available = case["noBaseProvider"] != true;
    let affinity_error = case["affinityKeyError"].as_str().map(str::to_owned);
    let key_trace = trace.clone();
    let lookup_integration = integration.clone();
    let deps = AnnouncementDependencies::<Ctx> {
        scheduler: scheduler.clone(),
        get_integration: Arc::new(move |id: &str| {
            (id == "example").then(|| lookup_integration.clone())
        }),
        get_base_provider: Arc::new(move |id: &str, _ctx: &Ctx| {
            (id == "example" && base_available).then(|| provider.clone())
        }),
        affinity_key_for: Arc::new(
            move |_integration: &dyn ProviderIntegration, _ctx: &Ctx, id: &str| {
                push(&key_trace, json!({ "affinityKeyFor": id }));
                match &affinity_error {
                    Some(message) => Err(message.clone()),
                    None => Ok(SESSION.to_owned()),
                }
            },
        ),
    };
    let run = Arc::new(Run {
        trace: trace.clone(),
        announcement: ServiceAnnouncement::new(deps),
        handles: Mutex::new(HashMap::new()),
        listeners: Mutex::new(HashMap::new()),
        definitions: case["listeners"].clone(),
    });

    let mut results: Vec<Value> = Vec::new();
    for step in case["steps"].as_array().unwrap() {
        let input = resolve_refs(step, &results);
        let before = trace.lock().unwrap().len();
        let mut outcome = match run_step(&input, &run, &scheduler, &scripted, &ctx).await {
            Ok(result) => json!({ "op": step["op"], "result": result }),
            Err(error) => json!({ "op": step["op"], "error": error }),
        };
        outcome["trace"] = Value::Array(trace.lock().unwrap()[before..].to_vec());
        results.push(outcome);
    }
    assert!(!store.is_locked(), "{} left the lock held", case["name"]);
    json!({
        "name": case["name"],
        "results": results,
        "finalState": store.final_state(),
    })
}

#[tokio::test]
async fn announcement_cases_match_the_upstream_reference() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = fixture["cases"]["announcement"].as_array().unwrap();
    assert_eq!(cases.len(), 27);
    for case in cases {
        let actual = run_case(case).await;
        let expected = json!({
            "name": case["name"],
            "results": case["results"],
            "finalState": case["finalState"],
        });
        assert_eq!(
            normalize(&actual),
            normalize(&expected),
            "case {}",
            case["name"]
        );
    }
}

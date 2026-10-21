use rpi_multiprovider::services::{Error, Lease, Options, Registration, Service};
use rpi_multiprovider::types::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

fn parse<T: serde::de::DeserializeOwned>(value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap()
}
fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap()
}

struct Runner {
    service: Service,
    inventory: HashMap<String, Arc<Mutex<Vec<Account>>>>,
    registrations: Vec<Arc<Registration>>,
    leases: HashMap<String, Lease>,
    now: Arc<AtomicU64>,
}
impl Runner {
    fn new(options: &Value) -> Self {
        let now = Arc::new(AtomicU64::new(1000));
        let clock = now.clone();
        let offset = options["randomOffset"].as_u64().unwrap_or(0);
        let next = AtomicU64::new(0);
        let service = Service::new(Options {
            now: Arc::new(move || clock.load(Ordering::SeqCst) as f64),
            random_id: Arc::new(move || {
                format!("lease-{}", next.fetch_add(1, Ordering::SeqCst) + 1)
            }),
            random_int: if options["nativeRandom"] == true {
                Options::default().random_int
            } else {
                Arc::new(move |max| Ok(offset % max as u64))
            },
            settings: parse(options),
            default_policy: options.get("defaultPolicy").map(parse).unwrap_or_default(),
            affinity: options["affinity"].as_bool().unwrap_or(true),
        });
        Self {
            service,
            inventory: HashMap::new(),
            registrations: Vec::new(),
            leases: HashMap::new(),
            now,
        }
    }
    async fn action(&mut self, action: &Value) -> Result<Value, Error> {
        match string(action, "op") {
            "pins" => {
                let events = std::cell::RefCell::new(Vec::new());
                let pending = rpi_multiprovider::pins::apply(
                    &parse::<Vec<rpi_multiprovider::pins::SessionPin>>(&action["pins"]),
                    &self.service,
                    |pin, error| {
                        events
                            .borrow_mut()
                            .push(json!({"error":error.to_string(),"pin":pin}))
                    },
                    |pin| events.borrow_mut().push(json!({"applied":pin})),
                )
                .await;
                return Ok(json!({"pending":pending,"events":events.into_inner()}));
            }
            "register" => {
                let inventory = Arc::new(Mutex::new(parse(&action["accounts"])));
                self.inventory
                    .insert(string(action, "id").into(), inventory.clone());
                let registration = self.service.register(Registration {
                    id: string(action, "id").into(),
                    label: string(action, "label").into(),
                    selection_bias: action.get("selectionBias").map(parse).unwrap_or_default(),
                    management_hint: action["managementHint"].as_str().map(str::to_string),
                    accounts: Arc::new(move || {
                        let inventory = inventory.lock().unwrap().clone();
                        Box::pin(async move { Ok(inventory) })
                    }),
                    classify_failure: if action["classify"] == true {
                        Some(Arc::new(|failure, _account| {
                            if failure
                                .cause
                                .as_ref()
                                .is_some_and(|cause| cause["throws"] == true)
                            {
                                return Err("fixture failure".into());
                            }
                            Ok(failure
                                .cause
                                .as_ref()
                                .and_then(|cause| cause.get("disposition"))
                                .map(parse))
                        }))
                    } else {
                        None
                    },
                })?;
                self.registrations.push(registration);
            }
            "inventory" => {
                *self.inventory[string(action, "providerId")].lock().unwrap() =
                    parse(&action["accounts"])
            }
            "unregister" => self
                .service
                .unregister(&self.registrations[action["registration"].as_u64().unwrap() as usize]),
            "acquire" => {
                let lease = self.service.acquire(parse(action)).await?;
                let result = json!({"id":lease.id,"providerId":lease.provider_id,"accountId":lease.account.id,"credentialRef":lease.account.credential_ref,"acquiredAt":lease.acquired_at});
                self.leases.insert(lease.id.clone(), lease);
                return Ok(result);
            }
            "release" => {
                return Ok(json!(self
                    .leases
                    .get_mut(string(action, "lease"))
                    .unwrap()
                    .release(parse(&action["outcome"]))))
            }
            "snapshot" => {
                let snapshot = self.service.snapshot().await?;
                assert!(!snapshot.to_string().contains("secret-"));
                return Ok(snapshot);
            }
            "now" => self
                .now
                .store(action["value"].as_u64().unwrap(), Ordering::SeqCst),
            "pin" => {
                self.service
                    .pin_account(
                        string(action, "providerId"),
                        string(action, "key"),
                        string(action, "accountId"),
                    )
                    .await?
            }
            "affinity" => {
                return Ok(json!(self
                    .service
                    .affinity(string(action, "providerId"), string(action, "key"))?))
            }
            "clear" => self
                .service
                .clear_affinity(action["providerId"].as_str(), action["key"].as_str()),
            "reset" => self
                .service
                .reset_health(string(action, "providerId"), string(action, "accountId"))?,
            "pool" => {
                return self
                    .service
                    .update_pool(string(action, "providerId"), parse(&action["patch"]))
                    .await
            }
            "preference" => {
                return Ok(json!(self
                    .service
                    .pool_preference(string(action, "providerId"))?))
            }
            "settings" => self.service.update_settings(&action["patch"])?,
            "errors" => return Ok(json!(self.service.errors_before_switch())),
            "enabled" => {
                return Ok(json!(
                    self.service
                        .has_enabled_accounts(string(action, "providerId"))
                        .await?
                ))
            }
            other => panic!("unknown fixture action {other}"),
        }
        Ok(Value::Null)
    }
}

fn equivalent(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equivalent(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, a)| b.get(key).is_some_and(|b| equivalent(a, b)))
        }
        _ => actual == expected,
    }
}

#[tokio::test]
async fn pinned_scheduler_differential_traces() {
    let fixture: Value = serde_json::from_str(include_str!("scheduler-reference.json")).unwrap();
    assert_eq!(
        fixture["reference"],
        "bd97d65a36f541935dc91e33ba15d7910a30f9d7"
    );
    for case in fixture["cases"].as_array().unwrap() {
        let mut runner = Runner::new(&case["options"]);
        for (index, action) in case["actions"].as_array().unwrap().iter().enumerate() {
            let actual = match runner.action(action).await {
                Ok(result) => json!({"result":result}),
                Err(error) => {
                    let mut value = json!({"name":error.name(),"message":error.to_string()});
                    if let Error::NoAccountAvailable(_, Some(at)) = error {
                        value["nextAvailableAt"] = json!(at);
                    }
                    json!({"error":value})
                }
            };
            let expected = &case["results"][index];
            assert!(
                equivalent(&actual, expected),
                "case {} action {index} {action}\nactual {actual}\nexpected {expected}",
                case["name"]
            );
        }
    }
}

#[tokio::test]
async fn dropping_outstanding_lease_releases_inflight_without_changing_health() {
    let fixture: Value = serde_json::from_str(include_str!("scheduler-reference.json")).unwrap();
    let mut runner = Runner::new(&json!({}));
    runner
        .action(&fixture["cases"][1]["actions"][0])
        .await
        .unwrap();
    let lease = runner
        .service
        .acquire(AcquireOptions {
            provider_id: "example".into(),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        runner.service.snapshot().await.unwrap()["providers"][0]["accounts"][0]["inFlight"],
        1
    );
    drop(lease);
    let snapshot = runner.service.snapshot().await.unwrap();
    let account = &snapshot["providers"][0]["accounts"][0];
    assert_eq!(account["inFlight"], 0);
    assert_eq!(account["consecutiveFailures"], 0);
    assert!(account.get("lastFailureKind").is_none());
}

#[tokio::test]
async fn snapshot_observes_live_registrations_across_inventory_wait() {
    let fixture: Value = serde_json::from_str(include_str!("scheduler-reference.json")).unwrap();
    let accounts: Vec<Account> = parse(&fixture["cases"][1]["actions"][0]["accounts"]);
    let service = Service::new(Options {
        now: Arc::new(|| 1000.0),
        ..Default::default()
    });
    let started = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let make = |id: &str, label: &str| {
        let inventory = accounts.clone();
        Registration {
            id: id.into(),
            label: label.into(),
            accounts: Arc::new(move || {
                let inventory = inventory.clone();
                Box::pin(async move { Ok(inventory) })
            }),
            selection_bias: SelectionBias::default(),
            management_hint: None,
            classify_failure: None,
        }
    };
    let mut first = make("a", "Old");
    let (start, finish) = (started.clone(), resume.clone());
    let first_accounts = accounts.clone();
    first.accounts = Arc::new(move || {
        let (start, finish, inventory) = (start.clone(), finish.clone(), first_accounts.clone());
        Box::pin(async move {
            start.notify_one();
            finish.notified().await;
            Ok(inventory)
        })
    });
    let original = service.register(first).unwrap();
    let removed = service.register(make("b", "Removed")).unwrap();
    let clone = service.clone();
    let snapshot = tokio::spawn(async move { clone.snapshot().await.unwrap() });
    started.notified().await;
    service.unregister(&removed);
    service.unregister(&original);
    service.register(make("c", "Added")).unwrap();
    let mut replacement = make("a", "New");
    replacement.selection_bias = SelectionBias::None;
    service.register(replacement).unwrap();
    resume.notify_one();
    let actual = snapshot.await.unwrap();
    assert!(equivalent(
        &actual,
        &fixture["asyncChecks"]["snapshotMutation"]
    ));
}

#[tokio::test]
async fn native_entropy_retains_exact_weighted_cycle_shares() {
    let fixture: Value = serde_json::from_str(include_str!("scheduler-reference.json")).unwrap();
    let accounts: Vec<Account> = parse(&fixture["cases"][1]["actions"][0]["accounts"]);
    let service = Service::new(Options {
        default_policy: SelectionPolicy::WeightedRoundRobin,
        affinity: false,
        ..Default::default()
    });
    service
        .register(Registration {
            id: "example".into(),
            label: "Example".into(),
            accounts: Arc::new(move || {
                let accounts = accounts.clone();
                Box::pin(async move { Ok(accounts) })
            }),
            selection_bias: SelectionBias::default(),
            classify_failure: None,
            management_hint: None,
        })
        .unwrap();
    let mut counts = HashMap::new();
    for _ in 0..8 {
        let mut lease = service
            .acquire(AcquireOptions {
                provider_id: "example".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(uuid::Uuid::parse_str(&lease.id).is_ok());
        *counts.entry(lease.account.id.clone()).or_insert(0) += 1;
        lease.release(LeaseOutcome::Success);
    }
    assert_eq!(counts["a"], 6);
    assert_eq!(counts["b"], 2);
}

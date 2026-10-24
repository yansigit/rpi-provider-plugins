use rpi_multiprovider::lift::{self, Failover, Host, Request, Response, StreamKind};
use rpi_multiprovider::services::{Options, Registration, Service};
use rpi_multiprovider::types::{Account, AttemptFailure};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn failure(value: &AttemptFailure) -> Value {
    let mut result = json!({"message":value.message,"outputStarted":value.output_started});
    if let Some(status) = value.status {
        result["status"] = json!(status);
    }
    for (key, value) in [
        ("headers", &value.headers),
        ("assistantMessage", &value.assistant_message),
        ("cause", &value.cause),
    ] {
        if let Some(value) = value {
            result[key] = value.clone();
        }
    }
    result
}
struct Fixture {
    case: Value,
    trace: Arc<Mutex<Vec<Value>>>,
    events: Mutex<Vec<Value>>,
    counts: Mutex<HashMap<String, usize>>,
    response: Mutex<Option<Response>>,
    now: Arc<AtomicU64>,
    signal: CancellationToken,
}
#[async_trait::async_trait]
impl Host for Fixture {
    type Auth = String;
    type Stream = futures_util::stream::Iter<std::vec::IntoIter<Result<Value, String>>>;
    async fn resolve(&self, account: &Account, _: &Request) -> Result<String, String> {
        self.trace.lock().unwrap().push(json!({"auth":account.id}));
        if let Some(error) = self.case["authErrors"][&account.id].as_str() {
            return Err(error.into());
        }
        Ok(account.id.clone())
    }
    async fn start(&self, auth: &String, request: &Request) -> Result<Self::Stream, String> {
        *self.response.lock().unwrap() = None;
        self.trace.lock().unwrap().push(json!({"send":auth,"kind":match request.kind {StreamKind::Full=>"stream",StreamKind::Simple=>"streamSimple"},"maxRetries":if request.disable_provider_retries {0} else {7}}));
        let mut counts = self.counts.lock().unwrap();
        let index = counts.entry(auth.clone()).or_default();
        let script = &self.case["sends"][auth][*index];
        *index += 1;
        if script["abort"] == true {
            self.signal.cancel();
        }
        if let Some(error) = script["throw"].as_str() {
            return Err(error.into());
        }
        if let Some(status) = script["response"]["status"].as_u64() {
            *self.response.lock().unwrap() = Some(Response {
                status: status as u16,
                headers: script["response"]["headers"].clone(),
            });
        }
        let events = script["events"]
            .as_array()
            .expect("fixture supplies each send")
            .iter()
            .map(|event| {
                if let Some(error) = event["throw"].as_str() {
                    Err(error.into())
                } else {
                    Ok(event.clone())
                }
            })
            .collect::<Vec<_>>();
        Ok(futures_util::stream::iter(events))
    }
    fn response(&self) -> Option<Response> {
        self.response.lock().unwrap().clone()
    }
    async fn emit(&self, event: Value) -> bool {
        self.events.lock().unwrap().push(event);
        true
    }
    async fn pause(&self) {
        self.trace.lock().unwrap().push(json!({"pause":250}));
        self.now.fetch_add(250, Ordering::SeqCst);
        if self.case["abortOnPause"] == true {
            self.signal.cancel();
        }
    }
    async fn failover(&self, info: Failover<'_>) -> Result<bool, String> {
        self.trace.lock().unwrap().push(json!({"failover":{"providerId":info.provider_id,"fromAccountId":info.from_account_id,"failure":failure(info.failure),"errorsOnAccount":info.errors_on_account}}));
        if let Some(error) = self.case["failoverError"].as_str() {
            return Err(error.into());
        }
        Ok(self.case["surface"] == true)
    }
}
fn normalize_numbers(value: Value) -> Value {
    match value {
        Value::Array(values) => values.into_iter().map(normalize_numbers).collect(),
        Value::Object(values) => values
            .into_iter()
            .map(|(key, value)| (key, normalize_numbers(value)))
            .collect(),
        Value::Number(number) => json!(number.as_f64().unwrap()),
        value => value,
    }
}
#[tokio::test]
async fn original_stream_attempts_events_and_health_match() {
    let reference: Value = serde_json::from_str(include_str!("lift-reference.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let now = Arc::new(AtomicU64::new(1_700_000_000_000));
        let clock = now.clone();
        let sequence = AtomicU64::new(0);
        let service = Service::new(Options {
            now: Arc::new(move || clock.load(Ordering::SeqCst) as f64),
            random_id: Arc::new(move || {
                format!("lease-{}", sequence.fetch_add(1, Ordering::SeqCst) + 1)
            }),
            settings: serde_json::from_value(case.clone()).unwrap(),
            affinity: case["affinity"].as_bool().unwrap_or(true),
            ..Options::default()
        });
        let trace = Arc::new(Mutex::new(Vec::new()));
        let failures = trace.clone();
        let classification = case.get("classification").cloned();
        service.register(Registration {
            id:"p".into(),label:"P".into(),selection_bias:Default::default(),management_hint:None,
            accounts:Arc::new(||Box::pin(async {
                Ok(["a","b"].into_iter().map(|id|serde_json::from_value::<Account>(json!({"id":id,"label":id.to_uppercase(),"authKind":"api-key","credentialRef":id})).unwrap()).collect())
            })),
            classify_failure:Some(Arc::new(move |error,account| {
                failures.lock().unwrap().push(json!({"failure":failure(error),"account":account.id}));
                Ok(classification.as_ref().map(|value|serde_json::from_value(value.clone()).unwrap()))
            })),
        }).unwrap();
        if let Some(account) = case["pin"].as_str() {
            service.pin_account("p", "session", account).await.unwrap();
        }
        let signal = CancellationToken::new();
        let host = Fixture {
            case: case.clone(),
            trace: trace.clone(),
            events: Mutex::new(Vec::new()),
            counts: Mutex::new(HashMap::new()),
            response: Mutex::new(None),
            now: now.clone(),
            signal: signal.clone(),
        };
        let request = Request {
            provider_id: "p".into(),
            model_id: "m".into(),
            affinity_key: case["affinityKey"].as_str().map(str::to_owned),
            exclude_account_ids: case
                .get("excluded")
                .map(|value| serde_json::from_value(value.clone()).unwrap())
                .unwrap_or_default(),
            max_account_attempts: case["maxAccountAttempts"]
                .as_f64()
                .unwrap_or(9_007_199_254_740_991.0),
            disable_provider_retries: case["disableProviderRetries"].as_bool().unwrap_or(true),
            kind: if case["kind"] == "stream" {
                StreamKind::Full
            } else {
                StreamKind::Simple
            },
            signal,
        };
        if let Err(error) = lift::run(&service, &host, &request).await {
            host.events.lock().unwrap().push(json!({"type":"error","reason":"error","error":{
                "role":"assistant","content":[],"api":"test-api","provider":"p","model":"m",
                "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
                "stopReason":"error","errorMessage":error,"timestamp":1_700_000_000_000i64,"durationMs":now.load(Ordering::SeqCst)-1_700_000_000_000,
            }}));
        }
        let actual = json!({"trace":*trace.lock().unwrap(),"events":*host.events.lock().unwrap(),"snapshot":service.snapshot().await.unwrap(),"affinity":service.affinity("p","session").unwrap(),"now":now.load(Ordering::SeqCst)});
        assert_eq!(
            normalize_numbers(actual),
            normalize_numbers(case["result"].clone()),
            "{}",
            case["name"]
        );
    }
}

struct StoppedHost {
    entered: tokio::sync::Notify,
    hold_auth: bool,
    emitted: AtomicU64,
}
#[async_trait::async_trait]
impl Host for StoppedHost {
    type Auth = ();
    type Stream = futures_util::stream::Iter<std::vec::IntoIter<Result<Value, String>>>;
    async fn resolve(&self, _: &Account, _: &Request) -> Result<(), String> {
        self.entered.notify_one();
        if self.hold_auth {
            std::future::pending().await
        } else {
            Ok(())
        }
    }
    async fn start(&self, _: &(), _: &Request) -> Result<Self::Stream, String> {
        Ok(futures_util::stream::iter(vec![
            Ok(json!({"type":"start"})),
            Ok(json!({"type":"thinking_start","contentIndex":0})),
        ]))
    }
    fn response(&self) -> Option<Response> {
        None
    }
    async fn emit(&self, _: Value) -> bool {
        self.emitted.fetch_add(1, Ordering::SeqCst);
        false
    }
}
#[tokio::test]
async fn cancelled_auth_and_stopped_consumers_release_the_lease_without_penalty() {
    for hold_auth in [true, false] {
        let service = Service::new(Options::default());
        service
            .register(Registration {
                id: "p".into(),
                label: "P".into(),
                selection_bias: Default::default(),
                management_hint: None,
                classify_failure: None,
                accounts: Arc::new(|| {
                    Box::pin(async {
                        Ok(vec![serde_json::from_value::<Account>(
                            json!({"id":"a","label":"A","authKind":"api-key","credentialRef":"a"}),
                        )
                        .unwrap()])
                    })
                }),
            })
            .unwrap();
        let baseline = service.snapshot().await.unwrap();
        let in_flight = |snapshot: &Value| -> Vec<u64> {
            snapshot["providers"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|pool| pool["accounts"].as_array().unwrap())
                .map(|account| account["inFlight"].as_u64().unwrap())
                .collect()
        };
        assert!(in_flight(&baseline).iter().all(|count| *count == 0));
        let host = Arc::new(StoppedHost {
            entered: tokio::sync::Notify::new(),
            hold_auth,
            emitted: AtomicU64::new(0),
        });
        let work = host.clone();
        let routing = service.clone();
        let task = tokio::spawn(async move {
            lift::run(
                &routing,
                &*work,
                &Request {
                    provider_id: "p".into(),
                    model_id: "m".into(),
                    affinity_key: None,
                    exclude_account_ids: Vec::new(),
                    max_account_attempts: 5.0,
                    disable_provider_retries: true,
                    kind: StreamKind::Simple,
                    signal: CancellationToken::new(),
                },
            )
            .await
        });
        host.entered.notified().await;
        if hold_auth {
            assert_eq!(
                service.snapshot().await.unwrap()["providers"][0]["accounts"][0]["inFlight"],
                1
            );
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            task.await.unwrap().unwrap();
            assert_eq!(host.emitted.load(Ordering::SeqCst), 1);
        }
        // Await above is the producer drain barrier, not merely cancellation
        // acknowledgement. Every pool/account must recover its exact baseline.
        let snapshot = service.snapshot().await.unwrap();
        assert_eq!(in_flight(&snapshot), in_flight(&baseline));
        let account = &snapshot["providers"][0]["accounts"][0];
        assert_eq!(account["inFlight"], 0);
        assert_eq!(account["consecutiveFailures"], 0);
        assert_eq!(account["status"], "ready");
    }
}

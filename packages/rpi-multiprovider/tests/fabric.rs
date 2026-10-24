use rpi_multiprovider::fabric::{
    decide, decide_failover, run_compaction, CompactOutcome, CompactionOutcome, CompactionState,
    FabricHost,
};
use rpi_multiprovider::lift::{self, Failover, Host, Request, Response, StreamKind};
use rpi_multiprovider::retry::AssistantMessage;
use rpi_multiprovider::services::{Options, Registration, Service};
use rpi_multiprovider::types::{Account, PoolPatch, SelectionPolicy};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = include_str!("retry-reference.json");

/// Scripted `FabricHost`: service availability, idle probe, pool snapshot and
/// compaction outcome all come from the fixture input.
struct Fabric {
    tools: String,
    pool: Option<usize>,
    pool_error: Option<String>,
    idle: Mutex<VecDeque<bool>>,
    idle_default: bool,
    compact_outcome: CompactOutcome,
    clock: Arc<AtomicI64>,
    accounts_queried: AtomicUsize,
    polls: AtomicUsize,
    compact_calls: AtomicUsize,
    notify: Mutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl FabricHost for Fabric {
    fn tool_names(&self) -> Result<Vec<String>, String> {
        match self.tools.as_str() {
            "available" => Ok(vec!["fabric_exec".to_owned()]),
            "error" => Err("tool registry unavailable".to_owned()),
            _ => Ok(Vec::new()),
        }
    }
    fn now_ms(&self) -> Option<i64> {
        Some(self.clock.load(Ordering::SeqCst))
    }
    fn is_idle(&self) -> bool {
        let mut idle = self.idle.lock().unwrap();
        idle.pop_front().unwrap_or(self.idle_default)
    }
    async fn sleep_ms(&self, ms: u64) {
        self.polls.fetch_add(1, Ordering::SeqCst);
        self.clock.fetch_add(ms as i64, Ordering::SeqCst);
    }
    async fn enabled_accounts(&self, _: &str) -> Result<Option<usize>, String> {
        self.accounts_queried.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.pool_error {
            return Err(error.clone());
        }
        Ok(self.pool)
    }
    fn compact(&self) -> CompactOutcome {
        self.compact_calls.fetch_add(1, Ordering::SeqCst);
        self.compact_outcome
    }
    fn notify(&self, message: &str, level: &str) {
        self.notify.lock().unwrap().push(json!([message, level]));
    }
}

/// One fixture step, driving the real `lift::run` so attempts/events come from
/// the same state machine the extension uses.
struct Step {
    input: Value,
    fabric: Arc<Fabric>,
    clock: Arc<AtomicI64>,
    state: Arc<futures_util::lock::Mutex<CompactionState>>,
    events: Mutex<Vec<Value>>,
    attempts: Mutex<Vec<String>>,
    handled: AtomicBool,
}

#[async_trait::async_trait]
impl Host for Step {
    type Auth = String;
    type Stream = futures_util::stream::Iter<std::vec::IntoIter<Result<Value, String>>>;

    async fn resolve(&self, account: &Account, _: &Request) -> Result<String, String> {
        Ok(account.id.clone())
    }
    async fn start(&self, auth: &String, _: &Request) -> Result<Self::Stream, String> {
        self.attempts.lock().unwrap().push(auth.clone());
        let input = &self.input;
        let exhausted = input["exhausted"].is_null()
            || input["exhausted"]
                .as_array()
                .unwrap()
                .iter()
                .any(|label| label.as_str() == Some(auth.as_str()));
        let failure = &input["failure"];
        let mut result = json!({
            "role": "assistant", "api": "probe-api", "provider": "probe", "model": "m",
            "content": [], "timestamp": self.clock.load(Ordering::SeqCst),
            "stopReason": failure["stopReason"].as_str().unwrap_or("stop"),
            "usage": {"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
                "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        });
        if let Some(message) = failure.get("errorMessage").and_then(Value::as_str) {
            result["errorMessage"] = json!(message);
        }
        let mut events = vec![Ok(json!({"type": "start", "partial": result}))];
        if input["visible"] == true && exhausted {
            events.push(Ok(json!({
                "type": "text_delta", "contentIndex": 0, "delta": "visible", "partial": result,
            })));
        }
        if input["aborted"] == true {
            result["stopReason"] = json!("aborted");
            events.push(Ok(json!({"type": "error", "reason": "aborted", "error": result})));
        } else if exhausted {
            events.push(Ok(json!({"type": "error", "reason": "error", "error": result})));
        } else {
            events.push(Ok(json!({"type": "done", "reason": "stop", "message": result})));
        }
        Ok(futures_util::stream::iter(events))
    }
    fn response(&self) -> Option<Response> {
        None
    }
    async fn emit(&self, event: Value) -> bool {
        self.events.lock().unwrap().push(event);
        true
    }
    async fn failover(&self, info: Failover<'_>) -> Result<bool, String> {
        let handled = self.input["handled"].as_bool().unwrap_or(false);
        let decision = decide_failover(&*self.fabric, &info, handled);
        self.handled.store(decision.handled, Ordering::SeqCst);
        if decision.compaction {
            let mut state = self.state.lock().await;
            run_compaction(&*self.fabric, &mut state, info.provider_id).await;
        }
        Ok(decision.handled)
    }
}

fn accounts(input: &Value) -> Vec<Account> {
    input["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| {
            serde_json::from_value(json!({
                "id": account["label"], "label": account["label"], "authKind": "api-key",
                "credentialRef": account["label"], "enabled": account["enabled"],
                "priority": account["priority"],
            }))
            .unwrap()
        })
        .collect()
}

fn fabric(input: &Value, clock: &Arc<AtomicI64>) -> Fabric {
    Fabric {
        tools: input["fabric"].as_str().unwrap().to_owned(),
        pool: input["pool"]["enabled"].as_u64().map(|value| value as usize),
        pool_error: input["pool"]["error"].as_str().map(str::to_owned),
        idle: Mutex::new(
            input["idle"]["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_bool().unwrap())
                .collect(),
        ),
        idle_default: input["idle"]["default"].as_bool().unwrap(),
        compact_outcome: if input["compactOutcome"].as_str() == Some("error") {
            CompactOutcome::Error
        } else {
            CompactOutcome::Complete
        },
        clock: clock.clone(),
        accounts_queried: AtomicUsize::new(0),
        polls: AtomicUsize::new(0),
        compact_calls: AtomicUsize::new(0),
        notify: Mutex::new(Vec::new()),
    }
}

/// Replays every failover case through the real lift state machine with the
/// decision layer injected, and compares the complete expected object.
#[tokio::test]
async fn failover_cases_match_the_upstream_reference() {
    let reference: Value = serde_json::from_str(FIXTURE).unwrap();
    let cases = reference["failover"].as_array().unwrap();
    assert!(!cases.is_empty(), "the reference records failover cases");
    let clock = Arc::new(AtomicI64::new(1_700_000_000_000));
    let service_clock = clock.clone();
    let service = Service::new(Options {
        now: Arc::new(move || service_clock.load(Ordering::SeqCst) as f64),
        settings: serde_json::from_value(json!({"errorsBeforeSwitch": 1})).unwrap(),
        ..Options::default()
    });
    let state = Arc::new(futures_util::lock::Mutex::new(CompactionState::default()));
    for (index, case) in cases.iter().enumerate() {
        let input = &case["input"];
        clock.fetch_add(input["gapMs"].as_i64().unwrap(), Ordering::SeqCst);
        let provider_id = format!("p-{index}");
        let provided = accounts(input);
        service
            .register(Registration {
                id: provider_id.clone(),
                label: provider_id.clone(),
                accounts: Arc::new(move || {
                    let provided = provided.clone();
                    Box::pin(async move { Ok(provided) })
                }),
                classify_failure: None,
                management_hint: None,
                selection_bias: Default::default(),
            })
            .unwrap();
        service
            .update_pool(
                &provider_id,
                PoolPatch {
                    policy: Some(SelectionPolicy::Priority),
                    ..PoolPatch::default()
                },
            )
            .await
            .unwrap();
        let fabric = Arc::new(fabric(input, &clock));
        let step = Arc::new(Step {
            input: input.clone(),
            fabric: fabric.clone(),
            clock: clock.clone(),
            state: state.clone(),
            events: Mutex::new(Vec::new()),
            attempts: Mutex::new(Vec::new()),
            handled: AtomicBool::new(false),
        });
        let request = Request {
            provider_id: provider_id.clone(),
            model_id: "m".to_owned(),
            affinity_key: None,
            exclude_account_ids: Vec::new(),
            max_account_attempts: 9_007_199_254_740_991.0,
            disable_provider_retries: true,
            kind: StreamKind::Simple,
            signal: CancellationToken::new(),
        };
        if let Err(error) = lift::run(&service, &*step, &request).await {
            step.events
                .lock()
                .unwrap()
                .push(json!({"type": "error", "reason": "error", "error": {"errorMessage": error}}));
        }
        let events: Vec<String> = step
            .events
            .lock()
            .unwrap()
            .iter()
            .map(|event| event["type"].as_str().unwrap().to_owned())
            .collect();
        let actual = json!({
            "handled": step.handled.load(Ordering::SeqCst),
            "attempts": step.attempts.lock().unwrap().clone(),
            "events": events,
            "compactCalls": fabric.compact_calls.load(Ordering::SeqCst),
            "notify": fabric.notify.lock().unwrap().clone(),
            "accountsQueried": fabric.accounts_queried.load(Ordering::SeqCst) > 0,
            "polls": fabric.polls.load(Ordering::SeqCst),
        });
        assert_eq!(actual, case["expected"], "{}", case["name"]);
    }
}

/// Guard paths that a live extension cannot reach: a missing context and a
/// provider that is absent from the pool snapshot.
#[tokio::test]
async fn compaction_guards_that_the_extension_cannot_reach() {
    struct Guard {
        context: bool,
        pool: Option<usize>,
    }
    #[async_trait::async_trait]
    impl FabricHost for Guard {
        fn has_context(&self) -> bool {
            self.context
        }
        fn tool_names(&self) -> Result<Vec<String>, String> {
            Ok(vec!["fabric_exec".to_owned()])
        }
        fn now_ms(&self) -> Option<i64> {
            Some(1_700_000_000_000)
        }
        fn is_idle(&self) -> bool {
            true
        }
        async fn sleep_ms(&self, _: u64) {}
        async fn enabled_accounts(&self, _: &str) -> Result<Option<usize>, String> {
            Ok(self.pool)
        }
        fn compact(&self) -> CompactOutcome {
            CompactOutcome::Complete
        }
        fn notify(&self, _: &str, _: &str) {}
    }
    let mut state = CompactionState::default();
    let no_context = Guard { context: false, pool: Some(3) };
    assert_eq!(
        run_compaction(&no_context, &mut state, "p").await,
        CompactionOutcome::NoContext
    );
    let no_pool = Guard { context: true, pool: None };
    assert_eq!(
        run_compaction(&no_pool, &mut state, "p").await,
        CompactionOutcome::NoPool
    );
}

/// `handleFailover` returns the integration's verdict unchanged unless fabric
/// claims the transition, and an absent or throwing tool listing never does.
#[tokio::test]
async fn handled_passthrough_and_fabric_availability() {
    struct Tools(String);
    #[async_trait::async_trait]
    impl FabricHost for Tools {
        fn tool_names(&self) -> Result<Vec<String>, String> {
            match self.0.as_str() {
                "available" => Ok(vec!["fabric_exec".to_owned()]),
                "error" => Err("unavailable".to_owned()),
                _ => Ok(vec!["read".to_owned()]),
            }
        }
        fn now_ms(&self) -> Option<i64> {
            Some(0)
        }
        fn is_idle(&self) -> bool {
            true
        }
        async fn sleep_ms(&self, _: u64) {}
        async fn enabled_accounts(&self, _: &str) -> Result<Option<usize>, String> {
            Ok(Some(2))
        }
        fn compact(&self) -> CompactOutcome {
            CompactOutcome::Complete
        }
        fn notify(&self, _: &str, _: &str) {}
    }
    let retryable = AssistantMessage {
        stop_reason: "error".to_owned(),
        error_message: Some("HTTP 503 upstream".to_owned()),
    };
    let quota = AssistantMessage {
        stop_reason: "error".to_owned(),
        error_message: Some("quota exceeded".to_owned()),
    };
    let absent = Tools("absent".to_owned());
    let error = Tools("error".to_owned());
    let available = Tools("available".to_owned());
    // Without fabric the integration's verdict is returned unchanged.
    assert_eq!(
        decide(&absent, Some(&retryable), true),
        rpi_multiprovider::fabric::Decision { handled: true, compaction: false }
    );
    assert_eq!(
        decide(&error, Some(&retryable), false),
        rpi_multiprovider::fabric::Decision { handled: false, compaction: false }
    );
    // Fabric claims a retryable failure and requests compaction.
    assert_eq!(
        decide(&available, Some(&retryable), false),
        rpi_multiprovider::fabric::Decision { handled: true, compaction: true }
    );
    // Non-retryable and missing messages keep the integration's verdict.
    assert_eq!(
        decide(&available, Some(&quota), false),
        rpi_multiprovider::fabric::Decision { handled: false, compaction: false }
    );
    assert_eq!(
        decide(&available, None, true),
        rpi_multiprovider::fabric::Decision { handled: true, compaction: false }
    );
}

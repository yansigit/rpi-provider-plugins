use rpi_antigravity_auth::{
    driver::{self, Body, Deadline, Response, Secrets, Source, PRE_RESPONSE_MS, TIMEOUT_MESSAGE},
    metadata::SessionStore,
    request,
    stream::EventSink,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Events(Vec<Value>);
impl EventSink for Events {
    async fn emit(&mut self, event: Value) -> Result<(), String> {
        self.0.push(event);
        Ok(())
    }
}
struct ScriptedBody {
    chunks: VecDeque<(u64, Result<Vec<u8>, String>)>,
    hang: bool,
    dropped: Arc<AtomicBool>,
}
impl Body for ScriptedBody {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        match self.chunks.pop_front() {
            Some((delay, result)) => {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                result.map(Some)
            }
            None if self.hang => std::future::pending().await,
            None => Ok(None),
        }
    }
}
impl Drop for ScriptedBody {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}
struct ScriptedSource<'a> {
    case: &'a Value,
    sessions: &'a Mutex<SessionStore>,
    ids: Mutex<VecDeque<String>>,
    dropped: Arc<AtomicBool>,
    delay: u64,
    hang: bool,
}
impl Source for ScriptedSource<'_> {
    type Body = ScriptedBody;
    fn now_ms(&self) -> i64 {
        1784285195116
    }
    fn next_id(&self) -> String {
        self.ids
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected UUID allocation")
    }
    fn sessions(&self) -> &Mutex<SessionStore> {
        self.sessions
    }
    async fn open(
        &self,
        model: &Value,
        context: &Value,
        options: &Value,
        _: &Deadline,
        secrets: &mut Secrets,
    ) -> Result<Response<Self::Body>, String> {
        let prepared = request::prepare(
            model,
            context,
            options,
            &mut self.sessions.lock().unwrap(),
            self.now_ms(),
            &mut || self.next_id(),
        )?;
        assert_eq!(
            prepared.body("fixture-project"),
            self.case["requests"][0]["body"].as_str().unwrap(),
            "{}",
            self.case["name"]
        );
        secrets.credential("fixture-access");
        secrets.project("fixture-project");
        Ok(Response {
            status: self.case["status"].as_u64().unwrap() as u16,
            headers: vec![("fixture".into(), b"private".to_vec())],
            body: ScriptedBody {
                chunks: self.case["chunks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|chunk| (self.delay, Ok(chunk.as_str().unwrap().as_bytes().to_vec())))
                    .collect(),
                hang: self.hang,
                dropped: self.dropped.clone(),
            },
        })
    }
}
#[tokio::test(start_paused = true)]
async fn driver_matches_all_original_requests_events_and_results() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/stream.json")).unwrap();
    for private_receipts in [false, true] {
        let sessions = Mutex::new(SessionStore::new(String::new()));
        for case in fixture["cases"].as_array().unwrap() {
            let dropped = Arc::new(AtomicBool::new(false));
            let source = ScriptedSource {
                case,
                sessions: &sessions,
                ids: Mutex::new(serde_json::from_value(case["ids"].clone()).unwrap()),
                dropped: dropped.clone(),
                delay: 0,
                hang: false,
            };
            let mut events = Events::default();
            let mut options = case["options"].clone();
            options["apiKey"] = "fixture-marker".into();
            let result = driver::run(
                &source,
                &case["model"],
                &case["context"],
                &options,
                CancellationToken::new(),
                private_receipts,
                &mut events,
            )
            .await;
            if private_receipts && !case["requests"].as_array().unwrap().is_empty() {
                let receipt = events.0.remove(0);
                assert_eq!(
                    receipt,
                    json!({"type":"response","response":{"status":case["status"],"headers":[["fixture",b"private".to_vec()]]}})
                );
            }
            assert_eq!(result, case["result"], "result: {}", case["name"]);
            assert_eq!(json!(events.0), case["events"], "events: {}", case["name"]);
            assert!(
                source.ids.lock().unwrap().is_empty(),
                "unconsumed UUIDs: {}",
                case["name"]
            );
            if !case["requests"].as_array().unwrap().is_empty() {
                assert!(dropped.load(Ordering::SeqCst));
            }
        }
    }
}
#[tokio::test(start_paused = true)]
async fn private_receipt_precedes_start_and_never_leaks_into_public_events_or_result() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/stream.json")).unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["status"] == 200 && case["events"][0]["type"] == "start")
        .unwrap();
    for private in [true, false] {
        let sessions = Mutex::new(SessionStore::new(String::new()));
        let source = ScriptedSource {
            case,
            sessions: &sessions,
            ids: Mutex::new(serde_json::from_value(case["ids"].clone()).unwrap()),
            dropped: Arc::new(AtomicBool::new(false)),
            delay: 0,
            hang: false,
        };
        let mut sink = Events::default();
        let result = driver::run(
            &source,
            &case["model"],
            &case["context"],
            &case["options"],
            CancellationToken::new(),
            private,
            &mut sink,
        )
        .await;
        if private {
            assert_eq!(sink.0[0]["type"], "response");
            assert_eq!(sink.0[1]["type"], "start");
            assert_eq!(
                sink.0.remove(0)["response"]["headers"],
                json!([["fixture", b"private".to_vec()]])
            );
        } else {
            assert_eq!(sink.0[0]["type"], "start");
        }
        assert!(sink.0.iter().all(|event| event["type"] != "response"));
        assert_eq!(json!(sink.0), case["events"]);
        assert_eq!(result, case["result"]);
        let public = serde_json::to_string(&(sink.0, result)).unwrap();
        assert!(!public.contains("headers"));
        assert!(!public.contains("fixture-access"));
        assert!(!public.contains("fixture-project"));
        assert!(!public.contains("[112,114,105,118,97,116,101]"));
    }
}

#[tokio::test(start_paused = true)]
async fn absolute_deadline_cancellation_priority_and_accepted_headers() {
    let caller = CancellationToken::new();
    let deadline = Deadline::new(caller.clone());
    tokio::time::advance(Duration::from_millis(PRE_RESPONSE_MS - 1)).await;
    assert!(deadline.check().is_ok());
    let error = deadline
        .wait(std::future::pending::<Result<(), String>>())
        .await
        .unwrap_err();
    assert_eq!(error, TIMEOUT_MESSAGE);
    assert!(deadline.signal.is_cancelled());
    caller.cancel();
    assert_eq!(deadline.check().unwrap_err(), "This operation was aborted");
    let deadline = Deadline::new(CancellationToken::new());
    deadline.accept_headers().unwrap();
    tokio::time::advance(Duration::from_millis(PRE_RESPONSE_MS + 1)).await;
    assert!(deadline.check().is_ok());
    assert_eq!(deadline.resume().unwrap_err(), TIMEOUT_MESSAGE);
}
#[tokio::test(start_paused = true)]
async fn trailing_usage_timeout_releases_body_and_keeps_completed_text() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/stream.json")).unwrap();
    let original = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "stream/late-usage")
        .unwrap();
    let mut case = original.clone();
    case["chunks"].as_array_mut().unwrap().truncate(1);
    let sessions = Mutex::new(SessionStore::new(String::new()));
    let dropped = Arc::new(AtomicBool::new(false));
    let source = ScriptedSource {
        case: &case,
        sessions: &sessions,
        ids: Mutex::new(serde_json::from_value(case["ids"].clone()).unwrap()),
        dropped: dropped.clone(),
        delay: 0,
        hang: true,
    };
    let mut events = Events::default();
    let result = driver::run(
        &source,
        &case["model"],
        &case["context"],
        &case["options"],
        CancellationToken::new(),
        false,
        &mut events,
    )
    .await;
    assert_eq!(result["stopReason"], "stop");
    assert_eq!(result["durationMs"], 1000);
    assert_eq!(result["usage"]["totalTokens"], 0);
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(events.0.last().unwrap()["type"], "done");
}

#[tokio::test(start_paused = true)]
async fn accepting_headers_disarms_an_already_waiting_outer_deadline() {
    let deadline = Deadline::new(CancellationToken::new());
    let result = deadline
        .wait(async {
            deadline.accept_headers()?;
            tokio::time::sleep(Duration::from_millis(PRE_RESPONSE_MS + 1)).await;
            Ok(())
        })
        .await;
    assert!(result.is_ok());
    assert_eq!(deadline.resume().unwrap_err(), TIMEOUT_MESSAGE);
}

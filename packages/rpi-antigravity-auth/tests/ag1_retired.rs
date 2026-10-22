//! Retired-model preflight refusal counters: offline, before auth or network.
//! Compares the native local driver to the unchanged upstream Pi 1.3.0
//! reference (tests/fixtures/ag1-retired.json) for the retired alias set.
use rpi_antigravity_auth::{
    auth::{self, Admission},
    driver::{self, Response},
    local::{Local, Shared},
    project::ProjectPort,
    request::PreparedRequest,
    stream::EventSink,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio_util::sync::CancellationToken;

struct Port {
    ids: Mutex<VecDeque<String>>,
    requests: Mutex<Vec<String>>,
    body: String,
}
struct Body(Option<Vec<u8>>);
impl driver::Body for Body {
    async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.0.take())
    }
}
impl auth::Body for Body {
    async fn text(self) -> Result<String, String> {
        unreachable!("managed project needs no lookup")
    }
}
impl auth::Port for Port {
    type Body = Body;
    async fn send(&self, _: auth::Request) -> Result<auth::Reply<Body>, String> {
        panic!("retired preflight reached the network")
    }
    fn now_ms(&self) -> f64 {
        1784285195116.0
    }
    fn user_agent(&self) -> String {
        "unused".into()
    }
}
impl ProjectPort for Port {
    async fn wait(&self, _: u64) {
        panic!("retired preflight reached onboarding")
    }
}
impl rpi_antigravity_auth::local::Port for Port {
    type StreamBody = Body;
    fn next_id(&self) -> String {
        self.ids
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected UUID")
    }
    async fn dispatch(
        &self,
        prepared: PreparedRequest,
        access: &str,
        project: &str,
        _: CancellationToken,
    ) -> Result<Response<Body>, String> {
        assert_eq!(access, "fixture-access");
        assert_eq!(project, "fixture-managed");
        self.requests.lock().unwrap().push(prepared.body(project));
        Ok(Response {
            status: 200,
            headers: vec![],
            body: Body(Some(self.body.as_bytes().to_vec())),
        })
    }
}
struct Gate {
    calls: AtomicU64,
    signal: CancellationToken,
}
impl Admission for Gate {
    async fn check(&self) -> Result<(), String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn cancelled(&self) -> bool {
        self.signal.is_cancelled()
    }
}
#[derive(Default)]
struct Events(Vec<Value>);
impl EventSink for Events {
    async fn emit(&mut self, value: Value) -> Result<(), String> {
        self.0.push(value);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn retired_preflight_refuses_before_auth_or_network() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/ag1-retired.json")).unwrap();
    let shared = Shared::default();
    for case in fixture["cases"].as_array().unwrap() {
        let signal = CancellationToken::new();
        let gate = Gate {
            calls: AtomicU64::new(0),
            signal: signal.clone(),
        };
        let port = Arc::new(Port {
            ids: Mutex::new(serde_json::from_value(case["ids"].clone()).unwrap()),
            requests: Mutex::new(vec![]),
            body: fixture["body"].as_str().unwrap().into(),
        });
        shared
            .credentials
            .lock()
            .unwrap()
            .remember("fixture-access", case["packed"].as_str().unwrap());
        let source = Local {
            shared: &shared,
            port: port.clone(),
            admission: &gate,
        };
        let mut events = Events::default();
        let result = driver::run(
            &source,
            &case["model"],
            &case["context"],
            &case["options"],
            signal,
            false,
            &mut events,
        )
        .await;
        assert_eq!(result, case["result"], "result: {}", case["model"]["id"]);
        assert_eq!(
            json!(events.0),
            case["events"],
            "events: {}",
            case["model"]["id"]
        );
        assert_eq!(
            gate.calls.load(Ordering::SeqCst),
            case["admissions"].as_u64().unwrap(),
            "admissions: {}",
            case["model"]["id"]
        );
        assert_eq!(
            *port.requests.lock().unwrap(),
            case["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|request| request["body"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            "network sends: {}",
            case["model"]["id"]
        );
        assert!(
            port.ids.lock().unwrap().is_empty(),
            "remaining UUIDs: {}",
            case["model"]["id"]
        );
    }
}

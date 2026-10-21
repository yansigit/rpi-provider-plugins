use rpi_antigravity_auth::vault::{
    client::Operation,
    connection::{Connector, Enrolled, Failure, Transport},
    errors::Error,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
};

struct State {
    responses: Mutex<Vec<VecDeque<Value>>>,
    connections: Mutex<VecDeque<Value>>,
    next: Mutex<usize>,
    now: AtomicI64,
    trace: Mutex<Vec<Value>>,
    connecting: tokio::sync::Notify,
    gate: Option<Arc<tokio::sync::Semaphore>>,
}
impl State {
    fn new(case: &Value, gate: bool) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(serde_json::from_value(case["responses"].clone()).unwrap()),
            connections: Mutex::new(serde_json::from_value(case["connections"].clone()).unwrap()),
            next: Mutex::new(0),
            now: AtomicI64::new(100000),
            trace: Mutex::new(vec![]),
            connecting: tokio::sync::Notify::new(),
            gate: gate.then(|| Arc::new(tokio::sync::Semaphore::new(0))),
        })
    }
}
fn failure(value: &Value) -> Failure {
    match value["type"].as_str().unwrap() {
        "subc" => Failure::Subc {
            terminal: value["kind"] == "terminal",
            code: value["code"].as_str().map(str::to_owned),
        },
        "credential" => Failure::Credential(Error {
            code: "fixture_code".into(),
            class: "permanent",
            action: "gone",
            unknown_class: None,
        }),
        "other" => Failure::Other,
        _ => panic!("unknown failure"),
    }
}
struct Script(Arc<State>);
struct Connection(Arc<State>, usize);
impl Connector for Script {
    type Transport = Connection;
    fn now_ms(&self) -> i64 {
        self.0.now.load(Ordering::SeqCst)
    }
    async fn connect(&self) -> Result<Connection, Failure> {
        self.0.trace.lock().unwrap().push(
            json!(["connect",{"connectionFile":"/fixture/connection","handshakeTimeoutMs":10000}]),
        );
        self.0.connecting.notify_one();
        if let Some(gate) = &self.0.gate {
            gate.acquire().await.unwrap().forget();
        }
        let response = self
            .0
            .connections
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Value::Null);
        if let Some(error) = response.get("error") {
            return Err(failure(error));
        }
        let mut next = self.0.next.lock().unwrap();
        *next += 1;
        Ok(Connection(self.0.clone(), *next))
    }
}
impl Transport for Connection {
    async fn call(&self, request: Value) -> Result<Value, Failure> {
        self.0.trace.lock().unwrap().push(json!(["call",self.1,"claustrum",request["method"],request["params"],{"identity":{"project_root":"/fixture/project","harness":"opencode","session":"store-fixture"},"consumerIdentity":null}]));
        let reply = self.0.responses.lock().unwrap()[self.1]
            .pop_front()
            .expect("script exhausted");
        if let Some(error) = reply.get("error") {
            Err(failure(error))
        } else {
            Ok(reply)
        }
    }
    async fn close(&self) {
        self.0.trace.lock().unwrap().push(json!(["close", self.1]));
    }
}
fn result(value: Result<Value, Error>) -> Value {
    match value {
        Ok(value) => json!({"value":value}),
        Err(e) => {
            json!({"error":{"code":e.code,"class":e.class,"action":e.action,"message":e.to_string()}})
        }
    }
}
fn client(state: &Arc<State>) -> Arc<Enrolled<Script>> {
    Arc::new(Enrolled::with_connection(
        Script(state.clone()),
        Connection(state.clone(), 0),
    ))
}
fn input() -> Value {
    json!({"enrollmentToken":"fixture-token"})
}

#[tokio::test]
async fn reconnect_backoff_and_public_errors_match_original_client() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-connection.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let state = State::new(case, false);
        let client = client(&state);
        let mut results = vec![];
        for step in case["steps"].as_array().unwrap() {
            if step == "close" {
                client.close().await;
            } else if let Some(now) = step.as_i64() {
                state.now.store(now, Ordering::SeqCst);
            } else {
                results.push(result(client.call(Operation::List, input()).await));
            }
        }
        client.close().await;
        assert_eq!(json!(results), case["results"], "{}", case["name"]);
        assert_eq!(
            json!(*state.trace.lock().unwrap()),
            case["trace"],
            "{}",
            case["name"]
        );
    }
}

#[tokio::test]
async fn concurrent_reconnect_failure_is_shared_and_abandoned_waiter_finishes() {
    let terminal = json!({"error":{"type":"subc","kind":"terminal"}});
    let state = State::new(
        &json!({"responses":[[terminal,terminal]],"connections":[{"error":{"type":"subc","kind":"terminal","code":"connect_failed"}}]}),
        true,
    );
    let client = client(&state);
    let abandoned = client.call(Operation::List, input());
    drop(abandoned);
    state.connecting.notified().await;
    let second = client.call(Operation::List, input());
    // Both calls have reached the shared reconnect before completing it.
    while state
        .trace
        .lock()
        .unwrap()
        .iter()
        .filter(|v| v[0] == "call")
        .count()
        != 2
    {
        tokio::task::yield_now().await;
    }
    tokio::task::yield_now().await;
    state.gate.as_ref().unwrap().add_permits(1);
    assert_eq!(second.await.unwrap_err().code, "connect_failed");
    assert_eq!(
        state
            .trace
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v[0] == "connect")
            .count(),
        1
    );
    client.close().await;
}

#[tokio::test]
async fn shutdown_closes_a_connection_that_arrives_after_it() {
    let state = State::new(
        &json!({"responses":[[{"error":{"type":"subc","kind":"terminal"}}]],"connections":[]}),
        true,
    );
    let client = client(&state);
    let pending = client.call(Operation::List, input());
    state.connecting.notified().await;
    client.close().await;
    state.gate.as_ref().unwrap().add_permits(1);
    assert_eq!(pending.await.unwrap_err().code, "transport_error");
    let trace = state.trace.lock().unwrap();
    assert_eq!(trace[trace.len() - 2], json!(["close", 0]));
    assert_eq!(trace[trace.len() - 1], json!(["close", 1]));
    assert_eq!(trace.iter().filter(|v| v[0] == "call").count(), 1);
}

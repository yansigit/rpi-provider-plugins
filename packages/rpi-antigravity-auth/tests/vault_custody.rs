use futures_util::future::BoxFuture;
use rpi_antigravity_auth::vault::{
    client::Operation,
    custody::{Custody, Failure, Port, Receipt},
    errors::Error,
    policy::ConsumerError,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

fn identity() -> Value {
    json!({"credentialId":"id","credentialType":"oauth","accountIdentity":"account"})
}
fn token() -> Value {
    json!({"token":"a".repeat(64),"token_generation":1})
}
fn receipt() -> Value {
    json!({"credentialId":"id","accountId":"account","material":"fixture-access","recordVersion":3,"expiresAtMs":901000,"projectId":"project"})
}
fn inventory() -> Value {
    json!({"view":"view-1","rows":[{"id":"id","credentialType":"oauth","categories":["antigravity-native"],"operations":["read"],"refreshAdapter":"antigravity","state":"active","accountId":"account"}]})
}
fn decode_error(value: &Value) -> Failure {
    match value["type"].as_str() {
        Some("consumer") => Failure::Consumer(ConsumerError::new(
            "not-enrolled",
            value["message"].as_str().unwrap(),
        )),
        Some("other") => Failure::Other,
        _ => Failure::Wire(Error::from_reply(
            &json!({"result":{"error":{"code":value["code"],"class":value.get("class").cloned().unwrap_or(json!("transient"))}}}),
            "invalid_response",
        )),
    }
}
fn error_value(error: Failure) -> Value {
    match error {
        Failure::Wire(e) => {
            json!({"name":"ClaustrumCredentialError","message":e.to_string(),"code":e.code,"class":e.class,"action":e.action})
        }
        Failure::Consumer(e) => {
            json!({"name":"ClaustrumConsumerError","message":e.message,"kind":e.kind})
        }
        Failure::Unreachable => {
            json!({"name":"ClaustrumUnreachableError","message":"Claustrum scoped operation unavailable","kind":"unavailable"})
        }
        Failure::Cancelled => {
            json!({"name":"AbortError","message":"This operation was aborted","code":20})
        }
        e => panic!("unexpected fixture error: {e}"),
    }
}
struct Data {
    now: u64,
    token: Value,
    responses: HashMap<String, VecDeque<Value>>,
    held: HashMap<String, oneshot::Sender<Value>>,
    calls: Vec<Value>,
    logs: Vec<Value>,
    timer_trace: Vec<Value>,
    next_timer: u64,
    timers: BTreeMap<u64, (u64, oneshot::Sender<()>)>,
}
struct Fixture(Arc<Mutex<Data>>);
impl Fixture {
    fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(Mutex::new(Data {
            now: 1000,
            token: token(),
            responses: HashMap::new(),
            held: HashMap::new(),
            calls: vec![],
            logs: vec![],
            timer_trace: vec![],
            next_timer: 0,
            timers: BTreeMap::new(),
        }))))
    }
}
struct Timer {
    id: u64,
    data: Arc<Mutex<Data>>,
}
impl Drop for Timer {
    fn drop(&mut self) {
        let mut data = self.data.lock().unwrap();
        if data.timers.remove(&self.id).is_some() {
            data.timer_trace.push(json!(["clear", self.id]));
        }
    }
}
impl Port for Fixture {
    fn now_ms(&self) -> f64 {
        self.0.lock().unwrap().now as f64
    }
    async fn token(&self) -> Result<Value, Failure> {
        let mut data = self.0.lock().unwrap();
        let now = data.now;
        data.calls.push(json!({"op":"token","at":now}));
        if let Some(error) = data.token.get("error") {
            return Err(decode_error(error));
        }
        Ok(data.token.clone())
    }
    async fn call(&self, operation: Operation, input: Value) -> Result<Value, Failure> {
        let (response, pending) = {
            let mut data = self.0.lock().unwrap();
            let now = data.now;
            let name = match operation {
                Operation::Get => "get",
                Operation::List => "list",
                Operation::Report => "report",
                _ => panic!("custody cannot enroll"),
            };
            let recorded = if name == "list" {
                input["enrollmentToken"].clone()
            } else {
                input
            };
            data.calls
                .push(json!({"op":name,"input":recorded,"at":now}));
            let response = data
                .responses
                .get_mut(name)
                .and_then(VecDeque::pop_front)
                .unwrap_or(Value::Null);
            let pending = response["hold"].as_str().map(|id| {
                let (send, receive) = oneshot::channel();
                data.held.insert(id.into(), send);
                receive
            });
            (response, pending)
        };
        let response = match pending {
            Some(pending) => pending.await.unwrap(),
            None => response,
        };
        if let Some(error) = response.get("error") {
            return Err(decode_error(error));
        }
        Ok(response
            .get("value")
            .cloned()
            .unwrap_or_else(|| match operation {
                Operation::Get => receipt(),
                Operation::List => inventory(),
                _ => Value::Null,
            }))
    }
    fn delay(&self, milliseconds: u64) -> BoxFuture<'static, ()> {
        let (send, receive) = oneshot::channel();
        let timer = {
            let mut data = self.0.lock().unwrap();
            data.next_timer += 1;
            let id = data.next_timer;
            let at = data.now + milliseconds;
            data.timers.insert(id, (at, send));
            data.timer_trace.push(json!(["set", id, milliseconds]));
            Timer {
                id,
                data: self.0.clone(),
            }
        };
        Box::pin(async move {
            let _timer = timer;
            receive.await.unwrap();
        })
    }
    fn log(&self, level: &'static str, message: &'static str, fields: Value) {
        self.0
            .lock()
            .unwrap()
            .logs
            .push(json!([level, message, fields]));
    }
    async fn close(&self) {
        let mut data = self.0.lock().unwrap();
        let now = data.now;
        data.calls.push(json!({"op":"close","at":now}));
    }
}
async fn flush() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn receipt_outage_and_report_traces_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-custody.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let port = Fixture::new();
        let custody = Custody::new(port.clone());
        let foreign = Custody::new(Fixture::new());
        let foreign_receipt = foreign
            .authorize(&identity(), &CancellationToken::new(), "route")
            .await
            .unwrap();
        let mut attempts: HashMap<String, Arc<Receipt>> = HashMap::new();
        let mut pending = HashMap::<
            String,
            (
                CancellationToken,
                tokio::task::JoinHandle<Result<Arc<Receipt>, Failure>>,
            ),
        >::new();
        let mut results = vec![];
        for step in case["steps"].as_array().unwrap() {
            let id = step["id"].as_str().unwrap_or("id");
            let result: Result<Option<Value>, Failure> = async {
                let value = match step["op"].as_str().unwrap() {
                    "reply" => { port.0.lock().unwrap().responses.entry(step["method"].as_str().unwrap().into()).or_default().push_back(step["response"].clone()); None }
                    "start" => {
                        let cancel = CancellationToken::new();
                        let signal = cancel.clone(); let source = custody.clone();
                        let identity = step.get("identity").cloned().unwrap_or_else(identity);
                        pending.insert(id.into(), (cancel, tokio::spawn(async move { source.authorize(&identity, &signal, "route").await }))); None
                    }
                    "cancel" => { pending[id].0.cancel(); None }
                    "release" => {
                        port.0.lock().unwrap().held.remove(id).unwrap().send(step.get("response").cloned().unwrap_or(Value::Null)).unwrap(); None
                    }
                    "wait" => {
                        let attempt = pending.remove(id).unwrap().1.await.unwrap()?;
                        let value = json!({"metadata":attempt.metadata(),"accessToken":attempt.access_token()});
                        attempts.insert(id.into(), attempt); Some(value)
                    }
                    "token" => { port.0.lock().unwrap().token = step["value"].clone(); None }
                    "time" => { port.0.lock().unwrap().now = step["value"].as_u64().unwrap(); None }
                    "timer" => {
                        let send = {
                            let mut data = port.0.lock().unwrap();
                            let id = *data.timers.iter().min_by_key(|(id,(at,_))| (*at,**id)).unwrap().0;
                            let (at,send) = data.timers.remove(&id).unwrap();
                            data.now = at; data.timer_trace.push(json!(["fire",id])); send
                        };
                        send.send(()).unwrap(); flush().await; None
                    }
                    "authorize" => {
                        let attempt = custody.authorize(step.get("identity").unwrap_or(&identity()), &CancellationToken::new(), step["route"].as_str().unwrap_or("route")).await?;
                        let value = json!({"metadata":attempt.metadata(),"accessToken":attempt.access_token()});
                        attempts.insert(id.into(), attempt); Some(value)
                    }
                    "discover" => Some(custody.discover(&CancellationToken::new()).await?),
                    "report" => {
                        let attempt = if step["forged"] == true { &foreign_receipt } else { &attempts[id] };
                        custody.report_failure(attempt, step["status"].as_u64().unwrap_or(401) as u16, "direct").await?; None
                    }
                    "reportTogether" => {
                        let (first, second) = tokio::join!(custody.report_failure(&attempts[id],401,"direct"),custody.report_failure(&attempts[id],401,"direct")); first?; second?; None
                    }
                    "forget" => { custody.forget(id); None }
                    "retain" => { custody.retain(step["identities"].as_array().unwrap()); None }
                    "lost" => { custody.transport_lost(step.get("error").map(decode_error)); None }
                    "reachable" => { custody.reachable(); None }
                    "listFailed" => { custody.list_failed(&decode_error(&step["error"])); None }
                    "state" => Some(custody.kept_receipt_state()),
                    "close" => { custody.close().await; None }
                    _ => panic!("invalid fixture step"),
                };
                Ok(value)
            }.await;
            flush().await;
            results.push(match result {
                Ok(Some(value)) => json!({"ok":true,"value":value}),
                Ok(None) => json!({"ok":true}),
                Err(error) => json!({"error":error_value(error)}),
            });
        }
        custody.close().await;
        foreign.close().await;
        flush().await;
        let data = port.0.lock().unwrap();
        assert_eq!(json!(results), case["results"], "{} results", case["name"]);
        assert_eq!(json!(data.calls), case["calls"], "{} calls", case["name"]);
        assert_eq!(json!(data.logs), case["logs"], "{} logs", case["name"]);
        assert_eq!(
            json!(data.timer_trace),
            case["timers"],
            "{} timers",
            case["name"]
        );
        assert!(data.timers.is_empty());
    }
}

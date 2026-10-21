use futures_util::future::BoxFuture;
use rpi_antigravity_auth::vault::{
    ceremony::{Failure as StorageError, Slot, Store},
    client::Operation,
    consumer::Port,
    custody::{Failure, Port as CustodyPort},
    enrollment,
    errors::Error,
    file_store::FileStore,
    policy::ConsumerError,
    roster::Projection,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::oneshot;
fn consumer_error(kind: &'static str, message: &str) -> Failure {
    Failure::Consumer(ConsumerError::new(kind, message))
}
fn failure(spec: &Value) -> Failure {
    if spec.get("code").is_some() {
        Failure::Wire(Error::from_reply(
            &json!({"result":{"error":{"code":spec["code"],"class":spec.get("class").cloned().unwrap_or(json!("permanent"))}}}),
            "invalid_response",
        ))
    } else {
        consumer_error("fixture", "fixture failure")
    }
}
pub fn outcome(value: Result<Value, Failure>) -> Value {
    match value {
        Ok(value) => json!({"value":value}),
        Err(e) => {
            let error = match e {
                Failure::Consumer(e) | Failure::Enrollment(StorageError::Consumer(e)) => {
                    json!({"name":"ClaustrumConsumerError","message":e.message,"kind":e.kind})
                }
                Failure::Wire(e) => {
                    json!({"name":"ClaustrumCredentialError","message":e.to_string(),"code":e.code,"class":e.class,"action":e.action})
                }
                Failure::Cancelled => {
                    json!({"name":"AbortError","message":"This operation was aborted","code":20})
                }
                e => panic!("unexpected fixture error {e}"),
            };
            json!({"error":error})
        }
    }
}
pub struct Data {
    pub now: f64,
    pub active: Value,
    pub next_client: u64,
    pub responses: HashMap<String, VecDeque<Value>>,
    pub held: HashMap<String, oneshot::Sender<()>>,
    pub calls: Vec<Value>,
}
pub struct Fixture {
    pub data: Arc<Mutex<Data>>,
    pub files: FileStore,
    pub inventory: Value,
    pub receipt: Value,
}
pub struct Client {
    fixture: Arc<Fixture>,
    id: u64,
}
pub struct Runtime(pub Arc<Fixture>);
impl Fixture {
    async fn response(&self, op: &str, fallback: Value) -> Result<Value, Failure> {
        let (spec, held) = {
            let mut d = self.data.lock().unwrap();
            let spec = d
                .responses
                .entry(op.into())
                .or_default()
                .pop_front()
                .unwrap_or(Value::Null);
            let held = spec["hold"].as_str().map(|name| {
                let (s, r) = oneshot::channel();
                d.held.insert(name.into(), s);
                r
            });
            (spec, held)
        };
        if let Some(held) = held {
            held.await.unwrap();
        }
        if let Some(e) = spec.get("error") {
            return Err(failure(e));
        }
        Ok(spec.get("value").cloned().unwrap_or(fallback))
    }
}
impl Port for Runtime {
    type Client = Client;
    async fn connect(&self) -> Result<Arc<Client>, Failure> {
        let id = {
            let mut d = self.0.data.lock().unwrap();
            d.next_client += 1;
            let id = d.next_client;
            d.calls.push(json!(["connect", id]));
            id
        };
        self.0.response("connect", Value::Null).await?;
        Ok(Arc::new(Client {
            fixture: self.0.clone(),
            id,
        }))
    }
    async fn active(&self) -> Result<bool, Failure> {
        let mut d = self.0.data.lock().unwrap();
        d.calls.push(json!(["active"]));
        let value = if let Some(values) = d.active.as_array_mut() {
            if values.len() > 1 {
                values.remove(0)
            } else {
                values[0].clone()
            }
        } else {
            d.active.clone()
        };
        Ok(value.as_bool().unwrap())
    }
    fn projection(&self) -> Projection {
        Projection::new("en-US", self.0.data.lock().unwrap().now).unwrap()
    }
    fn on_roster(&self, roster: &Value) {
        self.0
            .data
            .lock()
            .unwrap()
            .calls
            .push(json!(["roster", roster]));
    }
}
impl CustodyPort for Client {
    fn now_ms(&self) -> f64 {
        self.fixture.data.lock().unwrap().now
    }
    async fn token(&self) -> Result<Value, Failure> {
        let value = self.fixture.files.read(Slot::Token).await?.ok_or_else(|| {
            consumer_error("not-enrolled", "Claustrum enrollment is not configured")
        })?;
        enrollment::decode_token(&value).map_err(Failure::Consumer)
    }
    async fn call(&self, operation: Operation, input: Value) -> Result<Value, Failure> {
        let (op, fallback) = match operation {
            Operation::List => ("list", self.fixture.inventory.clone()),
            Operation::Get => ("get", self.fixture.receipt.clone()),
            Operation::Report => ("report", Value::Null),
            _ => panic!("unexpected operation"),
        };
        self.fixture.data.lock().unwrap().calls.push(json!([
            op,
            self.id,
            if op == "list" {
                input["enrollmentToken"].clone()
            } else {
                input
            }
        ]));
        self.fixture.response(op, fallback).await
    }
    fn delay(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(Duration::from_millis(ms)))
    }
    fn log(&self, level: &'static str, message: &'static str, fields: Value) {
        self.fixture
            .data
            .lock()
            .unwrap()
            .calls
            .push(json!(["log", level, message, fields]));
    }
    async fn close(&self) {
        self.fixture
            .data
            .lock()
            .unwrap()
            .calls
            .push(json!(["close", self.id]));
    }
}
pub struct Scratch(pub PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn save(path: &Path, bytes: &str) {
    std::fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}
pub async fn flush() {
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
}

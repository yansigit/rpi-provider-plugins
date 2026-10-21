use rpi_antigravity_auth::vault::{
    ceremony::{Client, Failure, Lease, Manager, Slot, Store},
    client::Operation,
    errors::Error,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

#[derive(Clone)]
struct Memory(Arc<Mutex<Data>>);
struct Data {
    state: Option<Value>,
    token: Option<Value>,
    trace: Vec<Value>,
    script: VecDeque<Value>,
    fail_token: bool,
    fail_approved: bool,
    locked: bool,
    fail_assert: usize,
    assertions: usize,
}
impl Data {
    fn snapshot(&self) -> Value {
        json!({"state":self.state,"token":self.token})
    }
}
struct Guard(Memory);
impl Lease for Guard {
    async fn assert_owned(&self) -> Result<(), Failure> {
        let mut data = self.0 .0.lock().unwrap();
        data.assertions += 1;
        if data.assertions == data.fail_assert {
            Err(Failure::Storage("lease-lost".into()))
        } else {
            Ok(())
        }
    }
    async fn release(self) -> Result<(), Failure> {
        self.0 .0.lock().unwrap().locked = false;
        Ok(())
    }
}
impl Store for Memory {
    type Lease = Guard;
    async fn acquire(&self) -> Result<Option<Guard>, Failure> {
        let mut data = self.0.lock().unwrap();
        if data.locked {
            Ok(None)
        } else {
            data.locked = true;
            Ok(Some(Guard(self.clone())))
        }
    }
    async fn read(&self, slot: Slot) -> Result<Option<Value>, Failure> {
        let data = self.0.lock().unwrap();
        Ok(match slot {
            Slot::State => data.state.clone(),
            Slot::Token => data.token.clone(),
        })
    }
    async fn write(&self, slot: Slot, value: Value) -> Result<(), Failure> {
        let mut data = self.0.lock().unwrap();
        match slot {
            Slot::Token => {
                let before = data.snapshot();
                data.trace
                    .push(json!({"operation":"write-token","input":value,"before":before}));
                if data.fail_token {
                    return Err(Failure::Storage("fixture-write-failed".into()));
                }
                data.token = Some(value);
            }
            Slot::State => {
                if data.fail_approved && value["phase"] == "approved" {
                    return Err(Failure::Storage("fixture-state-failed".into()));
                }
                data.state = Some(value);
            }
        };
        Ok(())
    }
    async fn remove(&self, slot: Slot) -> Result<bool, Failure> {
        let mut data = self.0.lock().unwrap();
        Ok(match slot {
            Slot::State => data.state.take().is_some(),
            Slot::Token => data.token.take().is_some(),
        })
    }
}
impl Client for Memory {
    async fn invoke(&self, operation: Operation, input: Value) -> Result<Value, Error> {
        let mut data = self.0.lock().unwrap();
        let before = data.snapshot();
        data.trace
            .push(json!({"operation":operation,"input":input,"before":before}));
        let value = data.script.pop_front().expect("script exhausted");
        if let Some(error) = value.get("error") {
            let action = match error["action"].as_str().unwrap() {
                "retry" => "retry",
                "gone" => "gone",
                "reauth" => "reauth",
                _ => "reduce_and_retry",
            };
            Err(Error {
                code: error["code"].as_str().unwrap().into(),
                class: "transient",
                action,
                unknown_class: None,
            })
        } else {
            Ok(value)
        }
    }
}
fn result(value: Result<Value, Failure>) -> Value {
    match value {
        Ok(value) => json!({"value":value}),
        Err(Failure::Consumer(e)) => {
            json!({"error":{"name":"ClaustrumConsumerError","kind":e.kind,"message":e.message}})
        }
        Err(Failure::Storage(message)) => json!({"error":{"name":"Error","message":message}}),
        Err(Failure::Wire(e)) => {
            json!({"error":{"name":"ClaustrumCredentialError","message":e.to_string()}})
        }
    }
}
fn setup(case: &Value) -> (Memory, Manager<Memory, Memory>, Arc<AtomicU64>) {
    let memory = Memory(Arc::new(Mutex::new(Data {
        state: case["initial"].get("state").cloned(),
        token: case["initial"].get("token").cloned(),
        trace: vec![],
        script: serde_json::from_value(case["replies"].clone()).unwrap(),
        fail_token: case["failTokenWrite"].as_bool().unwrap(),
        fail_approved: false,
        locked: false,
        fail_assert: 0,
        assertions: 0,
    })));
    let now = Arc::new(AtomicU64::new(42000));
    let clock = now.clone();
    let secret = case["mintedSecret"].as_str().unwrap().to_owned();
    let manager = Manager {
        store: memory.clone(),
        client: memory.clone(),
        proposed_name: "antigravity-auth-pi".into(),
        now: Arc::new(move || clock.load(Ordering::SeqCst)),
        mint_secret: Arc::new(move || Ok(secret.clone())),
    };
    (memory, manager, now)
}
#[tokio::test]
async fn ceremony_state_transitions_and_rpc_write_order_match_original() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-ceremony.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let (memory, manager, clock) = setup(case);
        let mut results = vec![];
        for step in case["steps"].as_array().unwrap() {
            let value = match step.as_str().unwrap() {
                "status" => manager.status().await,
                "reconcile" => manager.reconcile().await,
                "reset" => manager.reset_terminal().await.map(|s| json!(s)),
                "disconnect" => manager.disconnect().await.map(|s| json!(s)),
                _ => panic!("unknown step"),
            };
            results
                .push(json!({"result":result(value),"after":memory.0.lock().unwrap().snapshot()}));
            clock.fetch_add(1000, Ordering::SeqCst);
        }
        assert_eq!(json!(results), case["results"], "{}", case["name"]);
        assert_eq!(
            json!(memory.0.lock().unwrap().trace),
            case["trace"],
            "{}",
            case["name"]
        );
        assert!(!memory.0.lock().unwrap().locked);
    }
}
#[tokio::test]
async fn issued_token_survives_state_write_failure_and_disconnect_is_fenced() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-ceremony.json")).unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "new approval")
        .unwrap();
    let (memory, manager, _) = setup(case);
    memory.0.lock().unwrap().fail_approved = true;
    assert!(matches!(
        manager.reconcile().await,
        Err(Failure::Storage(_))
    ));
    {
        let data = memory.0.lock().unwrap();
        assert!(data.token.is_some());
        assert_eq!(data.state.as_ref().unwrap()["phase"], "pending");
    }
    memory.0.lock().unwrap().fail_approved = false;
    assert_eq!(manager.reconcile().await.unwrap()["state"], "approved");
    memory.0.lock().unwrap().fail_assert = 2;
    assert!(manager.disconnect().await.is_err());
    {
        let data = memory.0.lock().unwrap();
        assert!(data.token.is_none());
        assert_eq!(data.state.as_ref().unwrap()["phase"], "approved");
    }
    assert_eq!(manager.status().await.unwrap()["code"], "missing_token");
    assert_eq!(manager.disconnect().await.unwrap(), "disconnected");
}
#[tokio::test]
async fn busy_ceremony_does_not_read_write_or_contact_vault() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-ceremony.json")).unwrap();
    let (memory, manager, _) = setup(&fixture["cases"][0]);
    memory.0.lock().unwrap().locked = true;
    assert_eq!(manager.reconcile().await.unwrap(), json!({"state":"busy"}));
    assert_eq!(manager.reset_terminal().await.unwrap(), "busy");
    assert_eq!(manager.disconnect().await.unwrap(), "busy");
    assert!(memory.0.lock().unwrap().trace.is_empty());
}

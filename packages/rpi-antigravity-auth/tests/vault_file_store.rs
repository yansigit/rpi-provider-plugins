#![cfg(unix)]
use rpi_antigravity_auth::vault::{
    ceremony::{Client, Failure, Manager, Slot, Store},
    client::Operation,
    errors::Error,
    file_store::FileStore,
};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct Disk {
    store: FileStore,
    data: Arc<Mutex<Data>>,
}
struct Data {
    script: VecDeque<Value>,
    trace: Vec<Value>,
    fail_token: bool,
}
impl Disk {
    async fn snapshot(&self) -> Value {
        json!({"state":self.store.read(Slot::State).await.unwrap(),"token":self.store.read(Slot::Token).await.unwrap()})
    }
}
impl Store for Disk {
    type Lease = <FileStore as Store>::Lease;
    async fn acquire(&self) -> Result<Option<Self::Lease>, Failure> {
        self.store.acquire().await
    }
    async fn read(&self, slot: Slot) -> Result<Option<Value>, Failure> {
        self.store.read(slot).await
    }
    async fn write(&self, slot: Slot, value: Value) -> Result<(), Failure> {
        if slot == Slot::Token {
            let before = self.snapshot().await;
            let mut data = self.data.lock().unwrap();
            data.trace
                .push(json!({"operation":"write-token","input":value,"before":before}));
            if data.fail_token {
                return Err(Failure::Storage("fixture-write-failed".into()));
            }
        }
        self.store.write(slot, value).await
    }
    async fn remove(&self, slot: Slot) -> Result<bool, Failure> {
        self.store.remove(slot).await
    }
}
impl Client for Disk {
    async fn invoke(&self, operation: Operation, input: Value) -> Result<Value, Error> {
        let before = self.snapshot().await;
        let mut data = self.data.lock().unwrap();
        data.trace
            .push(json!({"operation":operation,"input":input,"before":before}));
        let value = data.script.pop_front().expect("script exhausted");
        if let Some(e) = value.get("error") {
            Err(Error {
                code: e["code"].as_str().unwrap().into(),
                class: "transient",
                action: match e["action"].as_str().unwrap() {
                    "retry" => "retry",
                    "gone" => "gone",
                    "reauth" => "reauth",
                    _ => "reduce_and_retry",
                },
                unknown_class: None,
            })
        } else {
            Ok(value)
        }
    }
}
fn result(value: Result<Value, Failure>) -> Value {
    match value {
        Ok(v) => json!({"value":v}),
        Err(Failure::Consumer(e)) => {
            json!({"error":{"name":"ClaustrumConsumerError","kind":e.kind,"message":e.message}})
        }
        Err(Failure::Wire(e)) => {
            json!({"error":{"name":"ClaustrumCredentialError","message":e.to_string()}})
        }
        Err(e) => json!({"error":{"name":"Error","message":e.to_string()}}),
    }
}
#[tokio::test]
async fn original_ceremonies_persist_through_real_private_files_and_leases() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-ceremony.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("rpi-vault-store-{}", uuid::Uuid::new_v4())));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch.0)
            .unwrap();
        let store =
            FileStore::new(scratch.0.join("state.json"), scratch.0.join("token.json")).unwrap();
        for (key, slot) in [("state", Slot::State), ("token", Slot::Token)] {
            if let Some(value) = case["initial"].get(key) {
                fs::write(store.path(slot), format!("{value}\n")).unwrap();
                fs::set_permissions(store.path(slot), fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        let disk = Disk {
            store,
            data: Arc::new(Mutex::new(Data {
                script: serde_json::from_value(case["replies"].clone()).unwrap(),
                trace: vec![],
                fail_token: case["failTokenWrite"].as_bool().unwrap(),
            })),
        };
        let now = Arc::new(AtomicU64::new(42000));
        let clock = now.clone();
        let secret = case["mintedSecret"].as_str().unwrap().to_owned();
        let manager = Manager {
            store: disk.clone(),
            client: disk.clone(),
            proposed_name: "antigravity-auth-pi".into(),
            now: Arc::new(move || clock.load(Ordering::SeqCst)),
            mint_secret: Arc::new(move || Ok(secret.clone())),
        };
        let mut results = vec![];
        for step in case["steps"].as_array().unwrap() {
            let value = match step.as_str().unwrap() {
                "status" => manager.status().await,
                "reconcile" => manager.reconcile().await,
                "reset" => manager.reset_terminal().await.map(|s| json!(s)),
                "disconnect" => manager.disconnect().await.map(|s| json!(s)),
                _ => panic!("unknown step"),
            };
            results.push(json!({"result":result(value),"after":disk.snapshot().await}));
            now.fetch_add(1000, Ordering::SeqCst);
        }
        assert_eq!(json!(results), case["results"], "{}", case["name"]);
        assert_eq!(
            json!(disk.data.lock().unwrap().trace),
            case["trace"],
            "{}",
            case["name"]
        );
        for entry in fs::read_dir(&scratch.0).unwrap() {
            let entry = entry.unwrap();
            assert!(
                matches!(
                    entry.file_name().to_str(),
                    Some("state.json" | "token.json")
                ),
                "left an unreleased lease or staged secret"
            );
            assert_eq!(
                entry.metadata().unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}

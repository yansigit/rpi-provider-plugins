#![cfg(unix)]
use rpi_antigravity_auth::vault::{
    ceremony::Failure,
    client::Operation,
    errors::Error as WireError,
    file_store::FileStore,
    setup::{self, Error, Port, Setup},
    source::Source,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    sync::{Arc, Mutex},
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct Fixture {
    spec: Value,
    source: Source,
    calls: Arc<Mutex<Vec<Value>>>,
}
impl Fixture {
    fn snapshot(&self) -> Value {
        let read = |p: &PathBuf| match fs::read(p) {
            Ok(v) => serde_json::from_slice::<Value>(&v).unwrap(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
            Err(e) if e.raw_os_error() == Some(libc::EISDIR) => json!({"error":"EISDIR"}),
            Err(e) => panic!("{e}"),
        };
        json!({"mode":read(&self.source.mode_file),"state":read(&self.source.store.state),"token":read(&self.source.store.token)})
    }
}
impl rpi_antigravity_auth::vault::ceremony::Client for Fixture {
    async fn invoke(&self, op: Operation, input: Value) -> Result<Value, WireError> {
        self.calls
            .lock()
            .unwrap()
            .push(json!([op, input, self.snapshot()]));
        Ok(match op {
            Operation::Propose => json!({"requestId":"request-1"}),
            Operation::Poll => self
                .spec
                .get("reply")
                .cloned()
                .unwrap_or(json!({"status":"pending"})),
            _ => panic!("unexpected call"),
        })
    }
}
impl Port for Fixture {
    type Client = Self;
    async fn connect(&self) -> Result<Self, Failure> {
        self.calls.lock().unwrap().push(json!(["connect"]));
        if self.spec["connectError"] == true {
            return Err(Failure::Storage("connect failed".into()));
        }
        Ok(self.clone())
    }
    async fn close(&self, _: Self) {
        self.calls.lock().unwrap().push(json!(["close"]));
    }
    async fn host_slot(&self) -> Result<String, Failure> {
        self.calls.lock().unwrap().push(json!(["hostSlot"]));
        Ok(setup::classify_host_slot(&self.spec["hostSlot"]).into())
    }
    async fn changed(&self, value: Value) -> Result<(), Failure> {
        self.calls
            .lock()
            .unwrap()
            .push(json!(["changed", value, self.snapshot()]));
        if self.spec["changeError"] == true {
            return Err(Failure::Storage("change failed".into()));
        }
        Ok(())
    }
    fn now_ms(&self) -> u64 {
        42000
    }
    fn mint_secret(&self) -> Result<String, Failure> {
        Ok("b".repeat(64))
    }
}
#[tokio::test]
async fn setup_transitions_match_original_pi_files_callbacks_and_connection_lifetime() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-setup.json")).unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("rpi-vault-setup-{}", uuid::Uuid::new_v4())));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch.0)
        .unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let dir = scratch.0.join(i.to_string());
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let source = Source::<FileStore>::new(&dir).unwrap();
        let spec = &case["spec"];
        for (path, key) in [
            (&source.mode_file, "mode"),
            (&source.store.state, "state"),
            (&source.store.token, "token"),
        ] {
            if let Some(value) = spec.get(key) {
                fs::write(path, value.to_string()).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        if spec["modeDirectory"] == true {
            fs::create_dir(&source.mode_file).unwrap();
        }
        let port = Arc::new(Fixture {
            spec: spec.clone(),
            source: source.clone(),
            calls: Arc::new(Mutex::new(vec![])),
        });
        let setup = Setup {
            source,
            port: port.clone(),
        };
        let result = match spec["action"].as_str().unwrap() {
            "status" => setup.status().await,
            "useVault" => setup.use_vault(spec["route"].as_str()).await,
            "useLocal" => setup.use_local().await,
            "disconnect" => setup.disconnect().await.map(|()| Value::Null),
            "connectStep" => setup.connect_step().await,
            _ => panic!("unexpected action"),
        };
        let result = match result {
            Ok(v) => json!({"value":v}),
            Err(e) => {
                let name = match &e {
                    Error::Refused(_) => "PiVaultSetupError",
                    Error::Other(Failure::Consumer(_)) => "ClaustrumConsumerError",
                    _ => "Error",
                };
                let message = if spec["modeDirectory"] == true {
                    "<mode-write-error>".into()
                } else {
                    e.to_string()
                };
                json!({"error":{"name":name,"message":message}})
            }
        };
        assert_eq!(result, case["result"], "result {i}: {spec}");
        assert_eq!(
            json!(*port.calls.lock().unwrap()),
            case["calls"],
            "calls {i}: {spec}"
        );
        assert_eq!(port.snapshot(), case["after"], "after {i}: {spec}");
        if spec["modeDirectory"] != true {
            assert!(!fs::read_dir(&dir).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
        }
    }
    for case in fixture["slots"].as_array().unwrap() {
        assert_eq!(
            setup::classify_host_slot(&case["hostSlot"]),
            case["result"].as_str().unwrap()
        );
    }
}

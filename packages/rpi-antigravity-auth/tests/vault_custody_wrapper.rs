//! The custody response wrapper: the opaque receipt `authorize()` returns.
//!
//! The wrapper is not the credential-slot transition (`vault::slot`) and not the
//! source mode synchronization (`vault::source`). Its public surface is the
//! enumerable receipt metadata plus the separately readable access token; the
//! token is not part of the enumerable value, and provenance lives outside the
//! wrapper so a copy cannot report a failure. This test compares that complete
//! surface with the unchanged upstream implementation on Node 24.
use futures_util::future::BoxFuture;
use rpi_antigravity_auth::vault::{
    client::Operation,
    custody::{Custody, Failure, Port, Receipt},
    policy::ConsumerError,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

fn identity() -> Value {
    json!({"credentialId":"id","credentialType":"oauth","accountIdentity":"account"})
}
fn token(value: &str, generation: u64) -> Value {
    json!({"token": value.repeat(64 / value.len()), "token_generation": generation})
}
struct Data {
    now: u64,
    token: Value,
    served: Value,
    calls: Vec<Value>,
}
struct Fixture(Arc<Mutex<Data>>);
impl Fixture {
    fn new(served: Value) -> Arc<Self> {
        Arc::new(Self(Arc::new(Mutex::new(Data {
            now: 1000,
            token: token("a", 1),
            served,
            calls: vec![],
        }))))
    }
    fn token(&self, value: Value) {
        self.0.lock().unwrap().token = value;
    }
    fn reports(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|call| call["op"] == "report")
            .map(|call| call["input"].clone())
            .collect()
    }
}
impl Port for Fixture {
    fn now_ms(&self) -> f64 {
        self.0.lock().unwrap().now as f64
    }
    async fn token(&self) -> Result<Value, Failure> {
        let data = self.0.lock().unwrap();
        Ok(data.token.clone())
    }
    async fn call(&self, operation: Operation, input: Value) -> Result<Value, Failure> {
        let mut data = self.0.lock().unwrap();
        let (name, response) = match operation {
            Operation::Get => ("get", data.served.clone()),
            Operation::Report => ("report", Value::Null),
            Operation::List => ("list", json!({"view":"view-1","rows":[],"skipped":[]})),
            _ => panic!("custody cannot enroll"),
        };
        data.calls.push(json!({"op":name,"input":input}));
        Ok(response)
    }
    fn delay(&self, _milliseconds: u64) -> BoxFuture<'static, ()> {
        Box::pin(std::future::pending())
    }
    fn log(&self, _level: &'static str, _message: &'static str, _fields: Value) {}
    async fn close(&self) {}
}

fn consumer_kind(error: Failure) -> &'static str {
    match error {
        Failure::Consumer(ConsumerError { kind, .. }) => kind,
        other => panic!("expected a consumer refusal, got {other}"),
    }
}

#[tokio::test]
async fn receipt_wrapper_shape_matches_the_original() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-custody-wrapper.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let port = Fixture::new(case["served"].clone());
        let custody = Custody::new(port.clone());
        let foreign = Custody::new(Fixture::new(case["served"].clone()));
        let forged: Arc<Receipt> = foreign
            .authorize(&identity(), &CancellationToken::new(), "route")
            .await
            .unwrap();

        let mut attempt = custody
            .authorize(&identity(), &CancellationToken::new(), "route")
            .await
            .unwrap();
        if case["kept"] == true {
            if case["changedToken"] == true {
                port.token(token("b", 2));
            }
            custody.transport_lost(None);
            attempt = custody
                .authorize(&identity(), &CancellationToken::new(), "route")
                .await
                .unwrap();
        }

        let metadata = attempt.metadata().clone();
        let access_token = attempt.access_token().to_owned();
        assert_eq!(metadata, case["metadata"], "{name} metadata");
        assert_eq!(
            json!(metadata.as_object().unwrap().keys().collect::<Vec<_>>()),
            case["keys"],
            "{name} enumerable keys"
        );
        assert_eq!(access_token, case["accessToken"], "{name} access token");
        assert_eq!(
            metadata, case["spread"],
            "{name} enumerable surface / copied wrapper"
        );
        assert_eq!(
            metadata.to_string().contains(&access_token),
            case["tokenInJson"],
            "{name} token hidden from the metadata"
        );
        assert_eq!(
            metadata.get("fromKeptReceipt") == Some(&Value::Bool(true)),
            case["fromKeptReceipt"],
            "{name} kept marker"
        );

        // A wrapper this custody did not issue is refused, not reported.
        let error = custody
            .report_failure(&forged, 401, "direct")
            .await
            .unwrap_err();
        assert_eq!(
            consumer_kind(error),
            case["provenanceErrorKind"].as_str().unwrap(),
            "{name} provenance"
        );

        let before = port.reports().len();
        custody
            .report_failure(&attempt, 401, "direct")
            .await
            .unwrap();
        let reports = port.reports();
        assert_eq!(
            reports.len() - before,
            case["reportCalls"].as_u64().unwrap() as usize,
            "{name} report calls"
        );
        let reported_token = reports
            .get(before)
            .and_then(|input| input["enrollmentToken"].as_str())
            .map(str::to_owned);
        assert_eq!(json!(reported_token), case["reportToken"], "{name} report token");

        custody.close().await;
        foreign.close().await;
    }
}

#[tokio::test]
#[ignore = "requires Node 24 and the pinned Antigravity reference"]
async fn node_reference_regenerates_the_committed_fixture() {
    let reference = std::process::Command::new(
        std::env::var_os("RPI_REFERENCE_NODE").expect("RPI_REFERENCE_NODE"),
    )
    .arg(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts/vault-custody-wrapper-reference.mjs"),
    )
    .env(
        "RPI_REFERENCE_DIR",
        std::env::var_os("RPI_REFERENCE_DIR").expect("RPI_REFERENCE_DIR"),
    )
    .output()
    .unwrap();
    assert!(
        reference.status.success(),
        "{}",
        String::from_utf8_lossy(&reference.stderr)
    );
}

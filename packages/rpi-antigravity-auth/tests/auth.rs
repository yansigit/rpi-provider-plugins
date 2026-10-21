use rpi_antigravity_auth::auth::{self, Admission};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
mod common;
use common::script;

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/auth.json")).unwrap()
}

#[test]
fn pkce_packing_expiry_and_cache_match_original() {
    let f = fixture();
    for case in f["authorizations"].as_array().unwrap() {
        let entropy: [u8; 32] = serde_json::from_value(case["entropy"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(auth::authorize_with_entropy(
                case["project"].as_str().unwrap(),
                &entropy
            ))
            .unwrap(),
            case["result"]
        );
    }
    for case in f["packed"].as_array().unwrap() {
        let parsed = auth::parse_refresh(case["input"].as_str().unwrap());
        assert_eq!(serde_json::to_value(&parsed).unwrap(), case["parsed"]);
        assert_eq!(
            auth::format_refresh(&parsed),
            case["formatted"].as_str().unwrap()
        );
    }
    for case in f["expiries"].as_array().unwrap() {
        assert_eq!(
            auth::expires_at(f["clock"].as_f64().unwrap(), &case["seconds"]),
            case["result"].as_f64().unwrap()
        );
    }
    for case in f["expired"].as_array().unwrap() {
        assert_eq!(
            auth::access_expired(&case["auth"], f["clock"].as_f64().unwrap()),
            case["result"].as_bool().unwrap()
        );
    }
    let mut cache = auth::CredentialCache::default();
    for case in f["cacheTrace"].as_array().unwrap() {
        cache.remember(
            case["access"].as_str().unwrap(),
            case["refresh"].as_str().unwrap(),
        );
        for (key, value) in case["values"].as_object().unwrap() {
            assert_eq!(json!(cache.get(key)), *value);
        }
    }
}

#[tokio::test]
async fn oauth_requests_credential_results_and_admission_order_match_original() {
    let f = fixture();
    for case in f["cases"].as_array().unwrap() {
        let port = script(f["clock"].as_f64().unwrap(), case);
        let gate = Gate {
            count: Mutex::new(0),
            refuse_at: case["refuseAt"].as_u64().unwrap_or(0),
            cancel_at: case["cancelAt"].as_u64().unwrap_or(0),
            trace: port.trace.clone(),
            record: true,
        };
        let result = if case["kind"] == "exchange" {
            auth::exchange(
                &port,
                &gate,
                case["code"].as_str().unwrap(),
                case["state"].as_str().unwrap(),
            )
            .await
        } else {
            match auth::refresh(&port, case["packed"].as_str().unwrap()).await {
                Ok(value) => json!({"value":value}),
                Err(error) => json!({"error":error}),
            }
        };
        assert_eq!(result, case["result"], "result: {case}");
        assert_eq!(
            json!(*port.trace.lock().unwrap()),
            case["trace"],
            "trace: {case}"
        );
        assert_eq!(
            port.queue.lock().unwrap().len() as u64,
            case["remaining"].as_u64().unwrap(),
            "{case}"
        );
        if case["kind"] == "exchange" {
            assert_eq!(
                *gate.count.lock().unwrap(),
                case["admissions"].as_u64().unwrap()
            );
        }
    }
}

#[tokio::test]
async fn pi_callback_url_and_code_inputs_match_integrated_host() {
    let f = fixture();
    for case in f["logins"].as_array().unwrap() {
        let port = script(f["clock"].as_f64().unwrap(), case);
        let authorization = &case["ui"][0]["url"];
        let (code, state) = auth::callback_input(
            case["input"].as_str().unwrap(),
            authorization.as_str().unwrap(),
        );
        let gate = Gate {
            count: Mutex::new(0),
            refuse_at: 0,
            cancel_at: 0,
            trace: port.trace.clone(),
            record: false,
        };
        let result = auth::exchange(&port, &gate, &code, &state).await;
        let mut credentials = json!({});
        for field in ["refresh", "access", "expires"] {
            if let Some(value) = result.get(field) {
                credentials[field] = value.clone();
            }
        }
        assert_eq!(json!({"value":credentials}), case["result"], "{case}");
        assert_eq!(json!(*port.trace.lock().unwrap()), case["trace"], "{case}");
    }
}

pub struct Gate {
    pub count: Mutex<u64>,
    pub refuse_at: u64,
    pub cancel_at: u64,
    pub trace: Arc<Mutex<Vec<Value>>>,
    pub record: bool,
}
impl Admission for Gate {
    async fn check(&self) -> Result<(), String> {
        if self.record {
            self.trace.lock().unwrap().push(json!({"kind":"admit"}));
        }
        let mut count = self.count.lock().unwrap();
        *count += 1;
        if *count == self.refuse_at {
            return Err("custody changed".into());
        }
        Ok(())
    }
    fn cancelled(&self) -> bool {
        self.cancel_at > 0 && *self.count.lock().unwrap() >= self.cancel_at
    }
}

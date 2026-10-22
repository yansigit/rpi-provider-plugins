//! AG-3 complete menu and notification traces: every deterministic walk of the
//! pinned `/antigravity` menu, replayed through the native renderer.
use rpi_antigravity_auth::{
    commands::{Accounts, Menu, Port},
    vault::{ceremony::Failure, setup::Error},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

struct Fixture {
    spec: Value,
    calls: Mutex<Vec<Value>>,
}
impl Fixture {
    fn perform(&self, call: Value) -> Result<(), Error> {
        self.calls.lock().unwrap().push(call);
        match self.spec["error"].as_str() {
            Some("refused") => Err(Error::Refused("Choose a vault account to use.")),
            Some(e) => Err(Failure::Storage(e.into()).into()),
            None => Ok(()),
        }
    }
}
impl Port for Fixture {
    async fn status(&self) -> Result<Value, Error> {
        self.calls.lock().unwrap().push(json!(["status"]));
        if let Some(e) = self.spec["statusError"].as_str() {
            return Err(Failure::Storage(e.into()).into());
        }
        Ok(self.spec["status"].clone())
    }
    async fn accounts(&self) -> Result<Option<Accounts>, String> {
        if self.spec["missingPort"] == true {
            return Ok(None);
        }
        self.calls.lock().unwrap().push(json!(["list"]));
        if let Some(e) = self.spec["listError"].as_str() {
            return Err(e.into());
        }
        Ok(Some(Accounts {
            list: self.spec["rows"].as_array().cloned().unwrap_or_default(),
            reach: self.spec.get("reach").cloned(),
        }))
    }
    async fn use_vault(&self, route: Option<String>) -> Result<(), Error> {
        self.perform(json!(["useVault", route]))
    }
    async fn use_local(&self) -> Result<(), Error> {
        self.perform(json!(["useLocal"]))
    }
    async fn disconnect(&self) -> Result<(), Error> {
        self.perform(json!(["disconnect"]))
    }
    async fn change_route(&self, route: String, accept: bool) -> Result<(), Error> {
        self.perform(json!([if accept { "accept" } else { "decline" }, route]))
    }
    async fn connect_step(&self) -> Result<Value, Error> {
        self.perform(json!(["connectStep"]))?;
        Ok(self
            .spec
            .get("step")
            .cloned()
            .unwrap_or(json!({"state":"approved"})))
    }
    fn watch(&self) {
        self.calls.lock().unwrap().push(json!(["watch"]));
    }
}
struct UiFixture {
    answers: Mutex<std::collections::VecDeque<Value>>,
    calls: Mutex<Vec<Value>>,
}
impl rpi_antigravity_auth::commands::render::Ui for UiFixture {
    async fn select(&self, title: String, labels: Vec<String>) -> Result<Option<usize>, String> {
        self.calls
            .lock()
            .unwrap()
            .push(json!(["select", title, labels]));
        Ok(self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .and_then(|v| v.as_u64().map(|v| v as usize)))
    }
    async fn confirm(&self, title: String, message: String) -> Result<bool, String> {
        self.calls
            .lock()
            .unwrap()
            .push(json!(["confirm", title, message]));
        Ok(self.answers.lock().unwrap().pop_front() == Some(Value::Bool(true)))
    }
    async fn notify(&self, text: &str, kind: &str) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(json!(["notify", text, kind]));
        Ok(())
    }
}
#[tokio::test]
async fn complete_menu_walks_notifications_and_cancellation_match_upstream() {
    let fixture: Value = serde_json::from_str(include_str!("ag3-menu.json")).unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let port = Arc::new(Fixture {
            spec: case["spec"].clone(),
            calls: Mutex::new(vec![]),
        });
        let menu = Menu {
            port: port.clone(),
            salt: [7; 16],
        };
        let ui = UiFixture {
            answers: Mutex::new(
                case["answers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect(),
            ),
            calls: Mutex::new(vec![]),
        };
        rpi_antigravity_auth::commands::render::run(&menu, &ui)
            .await
            .unwrap();
        assert_eq!(
            json!(*ui.calls.lock().unwrap()),
            case["uiTrace"],
            "uiTrace {i}: {} {:?}",
            case["spec"]["status"],
            case["answers"]
        );
        assert_eq!(
            json!(*port.calls.lock().unwrap()),
            case["calls"],
            "calls {i}"
        );
        // The reference records the invocation-level notifier separately from the
        // walk's Ui::notify trace. No walk uses it, and the native renderer has no
        // invocation-level sink, so the array must be present and empty and every
        // notification must already appear in uiTrace above.
        assert_eq!(
            case["notifications"],
            json!([]),
            "notifications {i}: success and error notifications belong to uiTrace"
        );
        assert!(ui.answers.lock().unwrap().is_empty(), "unused answers {i}");
        for private in [
            "owner@example.test",
            "other@example.test",
            "private-label",
            "another-private-route",
        ] {
            assert!(
                !json!(*ui.calls.lock().unwrap()).to_string().contains(private),
                "private account data escaped in walk {i}"
            );
        }
    }
    for case in fixture["cases"].as_array().unwrap() {
        for trace in case["uiTrace"].as_array().unwrap() {
            if trace[0] == "notify" {
                let text = trace[1].as_str().unwrap();
                for secret in ["secret=never-print", "Bearer secret.token"] {
                    assert!(!text.contains(secret), "secret escaped: {text}");
                }
            }
        }
    }
}

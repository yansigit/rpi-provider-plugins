use rpi_antigravity_auth::{
    commands::{seam, Accounts, Menu, Port},
    vault::{ceremony::Failure, setup::Error},
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
struct Fixture {
    index: std::sync::atomic::AtomicUsize,
    now: std::sync::atomic::AtomicU64,
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
        if let Some(steps) = self.spec["steps"].as_array() {
            let index = self
                .index
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .min(steps.len() - 1);
            let step = &steps[index];
            if let Some(error) = step["error"].as_str() {
                return Err(Failure::Storage(error.into()).into());
            }
            return Ok(step.clone());
        }
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
#[tokio::test]
async fn menu_models_actions_refresh_order_and_redaction_match_upstream() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-menu.json")).unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        if case.get("answers").is_some() {
            continue;
        }
        let port = Arc::new(Fixture {
            index: 0.into(),
            now: 1000.into(),
            spec: case["spec"].clone(),
            calls: Mutex::new(vec![]),
        });
        let menu = Menu {
            port: port.clone(),
            salt: [7; 16],
        };
        let result = if let Some(request) = case.get("request") {
            menu.apply(request).await
        } else {
            menu.open().await
        };
        assert_eq!(
            result, case["result"],
            "menu {i}: {} {}",
            case["spec"], case["request"]
        );
        assert_eq!(
            json!(*port.calls.lock().unwrap()),
            case["calls"],
            "calls {i}"
        );
        for private in [
            "owner@example.test",
            "other@example.test",
            "private-label",
            "another-private-route",
        ] {
            assert!(
                !result.to_string().contains(private),
                "private account data escaped in {i}"
            );
        }
        assert!(case["notifications"].as_array().unwrap().is_empty());
    }
    for (i, case) in fixture["texts"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            seam::text(case["input"].as_str().unwrap()),
            case["result"].as_str().unwrap(),
            "redaction {i}"
        );
    }
    for case in fixture["folders"].as_array().unwrap() {
        let result = seam::folder_refusal(case["input"].as_str().unwrap())
            .map(|text| json!({"ok":false,"code":"vault-folder-shared","text":text}))
            .unwrap_or(Value::Null);
        assert_eq!(result, case["result"]);
    }
}

impl rpi_antigravity_auth::commands::watch::Clock for Fixture {
    fn now_ms(&self) -> f64 {
        self.now.load(std::sync::atomic::Ordering::SeqCst) as f64
    }
    async fn sleep(&self, milliseconds: u64) {
        self.calls
            .lock()
            .unwrap()
            .push(json!(["sleep", milliseconds]));
        self.now
            .fetch_add(600000, std::sync::atomic::Ordering::SeqCst);
    }
    async fn source_status(&self) -> Result<Value, Error> {
        self.calls.lock().unwrap().push(json!(["sourceStatus"]));
        Ok(json!({"enrollment":{"state":if self.spec["idle"]==true {"idle"}else{"pending"}}}))
    }
}
#[tokio::test]
async fn approval_watch_matches_original_completion_and_forgetting_behavior() {
    use rpi_antigravity_auth::commands::watch;
    use tokio_util::sync::CancellationToken;
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-watch.json")).unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let mut spec = case["spec"].clone();
        spec["missingPort"] = true.into();
        let port = Arc::new(Fixture {
            index: 0.into(),
            now: 1000.into(),
            spec,
            calls: Mutex::new(vec![]),
        });
        let menu = Menu {
            port: port.clone(),
            salt: [7; 16],
        };
        let result = watch::run(&menu, &CancellationToken::new()).await;
        assert_eq!(
            json!(result.into_iter().collect::<Vec<_>>()),
            case["notifications"],
            "notification {i}"
        );
        assert_eq!(
            json!(*port.calls.lock().unwrap()),
            case["trace"],
            "trace {i}"
        );
        port.calls.lock().unwrap().clear();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(watch::run(&menu, &cancelled).await.is_none());
        assert!(port.calls.lock().unwrap().is_empty());
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
async fn native_menu_walk_preserves_original_choices_confirmation_and_notifications() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-menu.json")).unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let Some(answers) = case.get("answers") else {
            continue;
        };
        let port = Arc::new(Fixture {
            index: 0.into(),
            now: 1000.into(),
            spec: case["spec"].clone(),
            calls: Mutex::new(vec![]),
        });
        let menu = Menu {
            port: port.clone(),
            salt: [7; 16],
        };
        let ui = UiFixture {
            answers: Mutex::new(answers.as_array().unwrap().iter().cloned().collect()),
            calls: Mutex::new(vec![]),
        };
        rpi_antigravity_auth::commands::render::run(&menu, &ui)
            .await
            .unwrap();
        assert_eq!(json!(*ui.calls.lock().unwrap()), case["uiTrace"], "UI {i}");
        assert_eq!(
            json!(*port.calls.lock().unwrap()),
            case["calls"],
            "calls {i}"
        );
        assert!(ui.answers.lock().unwrap().is_empty());
    }
}

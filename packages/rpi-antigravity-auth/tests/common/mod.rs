use rpi_antigravity_auth::auth::{Body, Port, Reply, Request};
use serde_json::{json, Value};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
pub struct Script {
    pub clock: Mutex<f64>,
    pub user_agent: String,
    pub queue: Mutex<VecDeque<Value>>,
    pub trace: Arc<Mutex<Vec<Value>>>,
}
pub struct ResponseBody {
    script: Value,
    pub trace: Arc<Mutex<Vec<Value>>>,
}
impl Body for ResponseBody {
    async fn text(self) -> Result<String, String> {
        self.trace.lock().unwrap().push(json!({"kind":"read"}));
        if let Some(error) = self.script["readError"].as_str() {
            return Err(error.into());
        }
        Ok(self.script["raw"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| self.script["json"].to_string()))
    }
}
impl Port for Script {
    type Body = ResponseBody;
    async fn send(&self, request: Request) -> Result<Reply<Self::Body>, String> {
        self.trace
            .lock()
            .unwrap()
            .push(json!({"kind":"send","request":request}));
        let response = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected send");
        if let Some(error) = response["error"].as_str() {
            return Err(error.into());
        }
        Ok(Reply {
            status: response["status"].as_u64().unwrap() as u16,
            status_text: response["statusText"].as_str().unwrap_or_default().into(),
            body: ResponseBody {
                script: response,
                trace: self.trace.clone(),
            },
        })
    }
    fn now_ms(&self) -> f64 {
        *self.clock.lock().unwrap()
    }
    fn user_agent(&self) -> String {
        self.user_agent.clone()
    }
}
impl rpi_antigravity_auth::project::ProjectPort for Script {
    async fn wait(&self, milliseconds: u64) {
        self.trace
            .lock()
            .unwrap()
            .push(json!({"kind":"wait","milliseconds":milliseconds}));
        *self.clock.lock().unwrap() += milliseconds as f64;
        tokio::task::yield_now().await;
    }
}
pub fn script(clock: f64, case: &Value) -> Script {
    let user_agent = case["trace"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|event| {
            event["request"]["headers"].as_array().and_then(|headers| {
                headers
                    .iter()
                    .find(|header| header[0] == "User-Agent")
                    .and_then(|header| header[1].as_str())
            })
        })
        .unwrap_or("unused");
    Script {
        clock: Mutex::new(clock),
        user_agent: user_agent.into(),
        queue: Mutex::new(serde_json::from_value(case["responses"].clone()).unwrap()),
        trace: Arc::new(Mutex::new(Vec::new())),
    }
}

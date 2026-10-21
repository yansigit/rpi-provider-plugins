use rpi_antigravity_auth::project::{self, ProjectStore};
use serde_json::{json, Value};
use std::sync::Arc;
mod common;

#[tokio::test]
async fn discovery_onboarding_cache_and_failures_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/project.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let port = Arc::new(common::script(
            fixture["initialClock"].as_f64().unwrap(),
            case,
        ));
        let store = Arc::new(ProjectStore::default());
        let mut results = Vec::new();
        for op in case["operations"].as_array().unwrap() {
            let result = match op["op"].as_str().unwrap() {
                "ensure" => store.ensure(port.clone(), op["auth"].clone()).await,
                "parallel" => {
                    let values = op["auths"].as_array().unwrap();
                    assert_eq!(values.len(), 2);
                    let (one, two) = tokio::join!(
                        store.ensure(port.clone(), values[0].clone()),
                        store.ensure(port.clone(), values[1].clone())
                    );
                    one.and_then(|one| two.map(|two| json!([one, two])))
                }
                "invalidate" => {
                    store.invalidate(op["refresh"].as_str());
                    Ok(Value::Null)
                }
                "clearFailures" => {
                    store.clear_provision_failures();
                    Ok(Value::Null)
                }
                "advance" => {
                    *port.clock.lock().unwrap() += op["milliseconds"].as_f64().unwrap();
                    Ok(Value::Null)
                }
                "onboard" => Ok(project::onboard(
                    port.as_ref(),
                    op["access"].as_str().unwrap(),
                    &op["tier"],
                    op["project"].as_str(),
                    op["attempts"].as_u64().unwrap() as u32,
                    op["delay"].as_u64().unwrap(),
                )
                .await
                .unwrap_or(Value::Null)),
                other => panic!("unknown operation: {other}"),
            };
            results.push(match result {
                Ok(value) => json!({"value":value}),
                Err(error) => json!({"error":error}),
            });
        }
        assert_eq!(json!(results), case["results"], "result: {}", case["name"]);
        assert_eq!(
            json!(*port.trace.lock().unwrap()),
            case["trace"],
            "trace: {}",
            case["name"]
        );
        assert_eq!(*port.clock.lock().unwrap(), case["clock"].as_f64().unwrap());
        assert!(port.queue.lock().unwrap().is_empty(), "{}", case["name"]);
    }
}

#[tokio::test]
async fn cancelled_waiter_does_not_cancel_shared_project_resolution() {
    use rpi_antigravity_auth::auth::{Port, Reply, Request};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    };
    use tokio::sync::Semaphore;
    struct Blocked {
        script: common::Script,
        entered: Semaphore,
        release: Semaphore,
        sends: AtomicUsize,
    }
    impl Port for Blocked {
        type Body = common::ResponseBody;
        async fn send(&self, request: Request) -> Result<Reply<Self::Body>, String> {
            self.sends.fetch_add(1, Ordering::SeqCst);
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
            self.script.send(request).await
        }
        fn now_ms(&self) -> f64 {
            0.0
        }
        fn user_agent(&self) -> String {
            "fixture".into()
        }
    }
    impl project::ProjectPort for Blocked {
        async fn wait(&self, _: u64) {
            panic!("unexpected retry");
        }
    }
    let port = Arc::new(Blocked {
        script: common::Script {
            clock: Mutex::new(0.0),
            user_agent: "fixture".into(),
            queue: Mutex::new(
                [json!({"status":200,"json":{"cloudaicompanionProject":"managed"}})].into(),
            ),
            trace: Arc::new(Mutex::new(Vec::new())),
        },
        entered: Semaphore::new(0),
        release: Semaphore::new(0),
        sends: AtomicUsize::new(0),
    });
    let store = Arc::new(ProjectStore::default());
    let initial = json!({"type":"oauth","refresh":"token|project","access":"first"});
    let waiter = tokio::spawn({
        let store = store.clone();
        let port = port.clone();
        async move { store.ensure(port, initial).await }
    });
    port.entered.acquire().await.unwrap().forget();
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    let second = tokio::spawn({
        let store = store.clone();
        let port = port.clone();
        async move {
            store
                .ensure(
                    port,
                    json!({"type":"oauth","refresh":"token|other","access":"second"}),
                )
                .await
        }
    });
    tokio::task::yield_now().await;
    assert_eq!(port.sends.load(Ordering::SeqCst), 1);
    port.release.add_permits(1);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result["auth"]["access"], "first");
    assert_eq!(result["auth"]["refresh"], "token|project|managed");
    assert_eq!(result["effectiveProjectId"], "managed");
    assert_eq!(port.sends.load(Ordering::SeqCst), 1);
}

use super::*;
use std::sync::atomic::AtomicUsize;
fn fixture(name: &'static str, calls: Arc<Mutex<Vec<Value>>>) -> Invocation {
    let next = Arc::new(AtomicUsize::new(0));
    let make = |persistent: bool| {
        let calls = calls.clone();
        let next = next.clone();
        Arc::new(
            move |input: Value, signal: CancellationToken| -> HostResult {
                let calls = calls.clone();
                let next = next.clone();
                Box::pin(async move {
                    calls.lock().unwrap().push(json!({"owner":name,"persistent":persistent,"input":input,"cancelled":signal.is_cancelled()}));
                    Ok(match input["action"].as_str() {
                        Some("capture") => {
                            json!({"origin":format!("{name}-{}",next.fetch_add(1, Ordering::SeqCst)),"snapshot":{"metadata":{"id":name}}})
                        }
                        Some("location") => json!({"path":null}),
                        _ => Value::Null,
                    })
                })
            },
        ) as Request
    };
    Invocation {
        live: make(false),
        persistence: make(true),
    }
}
fn call(method: &str, input: Value) -> HostCall {
    HostCall {
        parent: "standing".into(),
        method: method.into(),
        input,
        signal: CancellationToken::new(),
    }
}
fn scoped(origin: &str, action: &str) -> HostCall {
    call(
        "scoped",
        json!({"scope":origin,"method":"native","input":{"port":"sessions","origin":origin,"action":action}}),
    )
}
#[tokio::test]
async fn standing_callbacks_keep_the_original_owner_and_release_only_after_acknowledgement() {
    let router = Arc::new(Router::default());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let parent = router.bind(Some(fixture("parent", trace.clone())));
    let child = router.bind(Some(fixture("child", trace.clone())));
    let capture = || call("native", json!({"port":"sessions","action":"capture"}));
    let a = parent(capture()).await.unwrap();
    let b = child(capture()).await.unwrap();
    let a = a["origin"].as_str().unwrap();
    let b = b["origin"].as_str().unwrap();
    assert!(child(call(
        "native",
        json!({"port":"sessions","action":"release","origin":b})
    ))
    .await
    .is_err());
    let standing = router.standing();
    assert!(standing(capture()).await.is_err());
    let late = scoped(b, "journalAppend");
    late.signal.cancel();
    standing(late).await.unwrap();
    let last = trace.lock().unwrap().last().unwrap().clone();
    assert_eq!(last["owner"], "child");
    assert_eq!(last["persistent"], true);
    assert_eq!(last["cancelled"], false);
    let mut redirected = scoped(a, "view");
    redirected.input["input"]["origin"] = b.into();
    assert!(standing(redirected).await.is_err());
    standing(scoped(b, "release")).await.unwrap();
    assert!(standing(scoped(b, "view")).await.is_err());
    standing(scoped(a, "view")).await.unwrap();
    assert!(standing(call(
        "scoped",
        json!({"scope":a,"method":"journals","input":{"origin":a,"action":"open","kind":"cloud"}})
    ))
    .await
    .unwrap_err()
    .contains("persistent session storage"));
    router.close().await.unwrap();
    assert!(standing(scoped(a, "view")).await.is_err());
    assert_eq!(router.capacity.available_permits(), 128);
    let trace = trace.lock().unwrap();
    for owner in ["parent", "child"] {
        assert_eq!(
            trace
                .iter()
                .filter(|v| v["owner"] == owner && v["input"]["action"] == "release")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn capture_racing_shutdown_releases_the_returned_origin_instead_of_publishing_it() {
    let router = Arc::new(Router::default());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let invocation = fixture("late", trace.clone());
    let live = invocation.live.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let invocation = Invocation {
        live: {
            let entered = entered.clone();
            let released = released.clone();
            Arc::new(move |input, signal| {
                let entered = entered.clone();
                let released = released.clone();
                let live = live.clone();
                Box::pin(async move {
                    entered.notify_one();
                    released.notified().await;
                    live(input, signal).await
                })
            })
        },
        ..invocation
    };
    let callback = router.bind(Some(invocation));
    let pending = tokio::spawn(async move {
        callback(call(
            "native",
            json!({"port":"sessions","action":"capture"}),
        ))
        .await
    });
    entered.notified().await;
    router.close().await.unwrap();
    released.notify_one();
    assert!(pending
        .await
        .unwrap()
        .unwrap_err()
        .contains("closed during capture"));
    assert!(router.state.lock().unwrap().origins.is_empty());
    assert_eq!(
        trace.lock().unwrap().last().unwrap()["input"]["action"],
        "release"
    );
}

#[tokio::test]
async fn late_journal_initialization_cannot_reopen_a_released_origin() {
    let router = Arc::new(Router::default());
    let invocation = fixture("journal", Arc::new(Mutex::new(Vec::new())));
    let persistent = invocation.persistence.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let released = Arc::new(tokio::sync::Notify::new());
    let path = std::env::temp_dir().join("rpi-router-unused-session.jsonl");
    let invocation = Invocation {
        persistence: {
            let entered = entered.clone();
            let released = released.clone();
            Arc::new(move |input, signal| {
                let entered = entered.clone();
                let released = released.clone();
                let persistent = persistent.clone();
                let path = path.clone();
                Box::pin(async move {
                    if input["action"] == "location" {
                        entered.notify_one();
                        released.notified().await;
                        Ok(json!({"path":path}))
                    } else {
                        persistent(input, signal).await
                    }
                })
            })
        },
        ..invocation
    };
    let captured = router.bind(Some(invocation))(call(
        "native",
        json!({"port":"sessions","action":"capture"}),
    ))
    .await
    .unwrap();
    let id = captured["origin"].as_str().unwrap().to_owned();
    let standing = router.standing();
    let pending = tokio::spawn(async move {
        standing(call("scoped",json!({"scope":id,"method":"journals","input":{"origin":id,"action":"open","kind":"cloud"}}))).await
    });
    entered.notified().await;
    router.close().await.unwrap();
    released.notify_one();
    assert!(pending
        .await
        .unwrap()
        .unwrap_err()
        .contains("journal origin is closed"));
    assert_eq!(router.capacity.available_permits(), 128);
}

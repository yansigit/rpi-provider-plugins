use rpi_cursor_sdk::worker::{HostHandler, Worker, WorkerConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

async fn worker(host: HostHandler) -> Worker {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Worker::spawn(
        WorkerConfig {
            node: std::env::var_os("RPI_CURSOR_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "node".into()),
            entry: package.join("worker/test/rpi/fixture-worker.mjs"),
            cwd: std::env::temp_dir(),
        },
        host,
    )
    .await
    .expect("build worker with Node 24+ before running this suite")
}

fn echo() -> HostHandler {
    Arc::new(|call| {
        Box::pin(async move {
            assert_eq!(call.method, "fixture.host");
            assert!(call.parent.starts_with('h'));
            Ok(call.input)
        })
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn concurrent_calls_keep_identity_and_wire_admission_in_the_same_order() {
    let worker = Arc::new(worker(echo()).await);
    let mut callers = Vec::new();
    for lane in 0..16 {
        let worker = Arc::clone(&worker);
        callers.push(tokio::spawn(async move {
            for i in 0..40 {
                let value = json!({"lane":lane,"iteration":i,"text":"x".repeat(if i == 0 { 256*1024 } else { 1 })});
                if i % 2 == 0 {
                    let result = worker.call("burst", json!({"count":1})).await.unwrap().result().await.unwrap();
                    assert_eq!(result, json!({"count":1}));
                } else {
                    let result = worker.call("echo", value.clone()).await.unwrap().result().await.unwrap();
                    assert_eq!(result, value);
                }
            }
        }));
    }
    for caller in callers {
        bounded(caller).await.unwrap();
    }
    let worker = Arc::try_unwrap(worker).ok().expect("all callers returned");
    worker.shutdown().await.unwrap();
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("worker operation hung")
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn calls_reenter_host_without_using_stdout_or_stderr() {
    let worker = worker(echo()).await;
    for (method, input, expected) in [
        ("echo", json!({"unicode":"🦀"}), json!({"unicode":"🦀"})),
        (
            "nested",
            json!({"roundTrip":true}),
            json!({"roundTrip":true}),
        ),
        ("stdio", Value::Null, Value::Bool(true)),
    ] {
        assert_eq!(
            bounded(worker.call(method, input).await.unwrap().result())
                .await
                .unwrap(),
            expected
        );
    }
    let error = bounded(worker.call("failure", Value::Null).await.unwrap().result())
        .await
        .unwrap_err();
    assert_eq!(error, "Cursor worker request failed");
    worker.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn stream_is_lossless_and_result_only_discards_without_stalling() {
    let worker = worker(echo()).await;
    let mut call = worker.call("burst", json!({"count":512})).await.unwrap();
    for i in 0..512 {
        assert_eq!(bounded(call.next()).await.unwrap()["i"], i);
    }
    assert!(bounded(call.next()).await.is_none());
    assert_eq!(bounded(call.result()).await.unwrap(), json!({"count":512}));
    assert_eq!(
        bounded(
            worker
                .call("burst", json!({"count":512}))
                .await
                .unwrap()
                .result()
        )
        .await
        .unwrap(),
        json!({"count":512})
    );
    let mut oversized = worker
        .call("burst", json!({"count":2,"bytes":4*1024*1024+100}))
        .await
        .unwrap();
    for i in 0..2 {
        let event = bounded(oversized.next()).await.unwrap();
        assert_eq!(event["i"], i);
        assert_eq!(event["text"].as_str().unwrap().len(), 4 * 1024 * 1024 + 100);
    }
    bounded(oversized.result()).await.unwrap();
    worker.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn cancellation_and_consumer_drop_release_synchronous_pressure() {
    let worker = worker(echo()).await;
    let call = worker.call("burst", json!({"count":10000})).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    call.cancel().await.unwrap();
    assert!(bounded(call.result()).await.is_err());
    let dropped = worker.call("burst", json!({"count":10000})).await.unwrap();
    drop(dropped);
    assert_eq!(
        bounded(
            worker
                .call("echo", json!("still live"))
                .await
                .unwrap()
                .result()
        )
        .await
        .unwrap(),
        "still live"
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn crash_preserves_queued_partial_and_reports_uncertain_completion() {
    let worker = worker(echo()).await;
    let mut call = worker.call("crash", Value::Null).await.unwrap();
    assert_eq!(
        bounded(call.next()).await.unwrap(),
        json!({"partial":"preserved"})
    );
    assert!(bounded(call.result()).await.unwrap_err().contains("closed"));
    worker.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn shutdown_waits_for_admitted_host_work_after_cancellation() {
    let admitted = Arc::new(Notify::new());
    let cancelled = Arc::new(Notify::new());
    let persist = Arc::new(Notify::new());
    let host: HostHandler = {
        let (admitted, cancelled, persist) = (admitted.clone(), cancelled.clone(), persist.clone());
        Arc::new(move |call| {
            let (admitted, cancelled, persist) =
                (admitted.clone(), cancelled.clone(), persist.clone());
            Box::pin(async move {
                admitted.notify_one();
                call.signal.cancelled().await;
                cancelled.notify_one();
                persist.notified().await;
                Ok(Value::Null)
            })
        })
    };
    let worker = worker(host).await;
    let call = worker.call("nested", Value::Null).await.unwrap();
    bounded(admitted.notified()).await;
    let shutdown = tokio::spawn(worker.shutdown());
    bounded(cancelled.notified()).await;
    assert!(
        !shutdown.is_finished(),
        "must retain callback until persistence finishes"
    );
    persist.notify_one();
    bounded(shutdown).await.unwrap().unwrap();
    assert!(call.result().await.is_err());
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn request_cancellation_reaches_a_pending_host_call_once() {
    let admitted = Arc::new(Notify::new());
    let host: HostHandler = {
        let admitted = admitted.clone();
        Arc::new(move |call| {
            let admitted = admitted.clone();
            Box::pin(async move {
                admitted.notify_one();
                call.signal.cancelled().await;
                Ok(json!({"cancelled":true}))
            })
        })
    };
    let worker = worker(host).await;
    let call = worker.call("nested", Value::Null).await.unwrap();
    bounded(admitted.notified()).await;
    for _ in 0..2000 {
        call.cancel().await.unwrap();
    }
    assert_eq!(
        bounded(call.result()).await.unwrap(),
        json!({"cancelled":true})
    );
    worker.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires Node 24+ and npm run build in worker/"]
async fn call_capacity_is_explicit_and_a_rejected_large_request_does_not_poison_peer() {
    let worker = worker(echo()).await;
    assert!(worker
        .call("echo", json!("x".repeat(64 * 1024 * 1024)))
        .await
        .is_err());
    assert_eq!(
        bounded(
            worker
                .call("echo", json!("healthy"))
                .await
                .unwrap()
                .result()
        )
        .await
        .unwrap(),
        "healthy"
    );
    let mut calls = Vec::new();
    for _ in 0..16 {
        calls.push(worker.call("hold", Value::Null).await.unwrap());
    }
    assert!(worker.call("echo", Value::Null).await.is_err());
    for call in &calls {
        call.cancel().await.unwrap();
    }
    for call in calls {
        assert_eq!(
            bounded(call.result()).await.unwrap(),
            json!({"cancelled":true})
        );
    }
    worker.shutdown().await.unwrap();
}

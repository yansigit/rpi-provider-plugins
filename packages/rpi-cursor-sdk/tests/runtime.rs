use rpi_cursor_sdk::{
    native_host::Router,
    runtime::{drive, Owner, Session},
    worker::{HostHandler, WorkerConfig},
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio_util::sync::CancellationToken;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "rpi-cursor-runtime-{:x}",
            u128::from_ne_bytes(random)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn config(&self) -> WorkerConfig {
        WorkerConfig {
            node: std::env::var_os("RPI_CURSOR_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| "node".into()),
            entry: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime/main.mjs"),
            cwd: self.0.clone(),
        }
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24+ and built/staged packaged runtime"]
async fn retained_runtime_initializes_once_serves_private_auth_and_drains_shutdown() {
    let directory = Directory::new();
    let session = Session::open(directory.config(), json!({"cwd":directory.0,"agentDir":directory.0.join("agent"),"sessionDir":directory.0.join("sessions")}), Arc::new(Router::default())).await.unwrap();
    assert_eq!(session.manifest["provider"]["id"], "cursor");
    assert_eq!(session.manifest["commands"].as_array().unwrap().len(), 10);
    let calls = Arc::new(AtomicUsize::new(0));
    let host: HostHandler = {
        let calls = calls.clone();
        Arc::new(move |call| {
            let calls = calls.clone();
            Box::pin(async move {
                assert_eq!(call.method, "native");
                assert_eq!(call.input["port"], "context");
                assert_eq!(call.input["action"], "flags");
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"cursor-no-fast":true}))
            })
        })
    };
    let signal = CancellationToken::new();
    let login = session
        .call(
            "provider.auth",
            json!({"action":"login","input":{"methodId":"api_key"}}),
            host.clone(),
            &signal,
            &|| false,
        )
        .await
        .unwrap();
    let login = drive(login, &signal, &|| false, |_| {
        panic!("authentication must not emit model events")
    })
    .await
    .unwrap();
    assert_eq!(login["kind"], "interaction");
    let finish = session.call("provider.auth",json!({"action":"continue","input":{"continuation":login["payload"]["continuation"],"value":"fixture-api-key"}}),host.clone(),&signal,&|| false).await.unwrap();
    let finished = drive(finish, &signal, &|| false, |_| panic!("unexpected event"))
        .await
        .unwrap();
    assert_eq!(finished["kind"], "credential");
    assert_eq!(finished["payload"]["key"], "fixture-api-key");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    signal.cancel();
    assert!(session
        .call("provider.auth", json!({}), host, &signal, &|| false)
        .await
        .is_err());
    session.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24+ and built/staged packaged runtime"]
async fn failed_runtime_bootstrap_returns_the_adapter_error_and_reaps_the_worker() {
    let directory = Directory::new();
    let result = Session::open(
        directory.config(),
        json!({"agentDir":false}),
        Arc::new(Router::default()),
    )
    .await;
    assert!(result
        .err()
        .unwrap()
        .contains("Invalid native Cursor locations"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Node 24+ and built/staged packaged runtime"]
async fn closing_waits_for_reentrant_callbacks_and_does_not_close_another_owner() {
    let directory = Directory::new();
    let mut owners = Vec::new();
    for name in ["parent", "child"] {
        let session = Session::open(directory.config(), json!({"cwd":directory.0,"agentDir":directory.0.join(name),"sessionDir":directory.0.join("sessions")}), Arc::new(Router::default())).await.unwrap();
        owners.push(Owner::new(session, CancellationToken::new()));
    }
    let child = owners.pop().unwrap();
    let parent = owners.pop().unwrap();
    let outer = child.enter().unwrap();
    let reentrant = child.enter().unwrap();
    let closing = tokio::spawn({
        let child = child.clone();
        async move { child.close().await }
    });
    outer.signal.cancelled().await;
    assert!(reentrant.signal.is_cancelled());
    assert!(child.enter().is_err());
    assert!(!closing.is_finished());
    drop(outer);
    assert!(!closing.is_finished());
    drop(reentrant);
    tokio::time::timeout(std::time::Duration::from_secs(10), closing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    child.close().await.unwrap();
    let retained = parent.enter().unwrap();
    assert!(!retained.signal.is_cancelled());
    assert_eq!(retained.session().manifest["provider"]["id"], "cursor");
    drop(retained);
    parent.close().await.unwrap();
}

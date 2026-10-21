use futures_util::future::BoxFuture;
use rpi_antigravity_auth::vault::{
    ceremony::{Failure as StorageError, Lease},
    client::Operation,
    custody::{Custody, Failure, Port},
    policy::ConsumerError,
    roster::Projection,
    roster_store::{Change, Fence, RosterStore},
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rpi-roster-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn save(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}
fn takeover(path: &Path, lock: &str) {
    let mut target = path.as_os_str().to_owned();
    target.push(format!(".{lock}.lock"));
    save(Path::new(&target),json!({"ownerId":"successor","expiresAt":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()+60000}).to_string());
}
fn fixture_error(message: &str) -> StorageError {
    ConsumerError::new("fixture", message).into()
}
fn storage_error(error: StorageError) -> Value {
    match error {
        StorageError::Consumer(e) => {
            json!({"name":"ClaustrumConsumerError","message":e.message,"kind":e.kind})
        }
        StorageError::LockLost { .. } => {
            json!({"name":"LockOwnershipError","message":error.to_string()})
        }
        StorageError::LockContention { .. } => {
            json!({"name":"LockContentionError","message":error.to_string()})
        }
        e => panic!("unexpected storage error {e}"),
    }
}
fn result(value: Result<Value, Failure>, dir: &Path) -> Value {
    match value {
        Ok(value) => json!({"value":value}),
        Err(e) => {
            let mut error = match e {
                Failure::Enrollment(e) => storage_error(e),
                Failure::Consumer(e) => {
                    json!({"name":"ClaustrumConsumerError","message":e.message,"kind":e.kind})
                }
                Failure::Cancelled => {
                    json!({"name":"AbortError","message":"This operation was aborted","code":20})
                }
                e => panic!("unexpected error {e}"),
            };
            error["message"] = json!(error["message"]
                .as_str()
                .unwrap()
                .replace(&dir.to_string_lossy().to_string(), "<DIR>"));
            json!({"error":error})
        }
    }
}
struct Fixture {
    name: String,
    path: PathBuf,
    trace: Arc<Mutex<Vec<Value>>>,
    signal: CancellationToken,
}
impl Port for Fixture {
    fn now_ms(&self) -> f64 {
        1000.0
    }
    async fn token(&self) -> Result<Value, Failure> {
        Ok(json!({"token":"a".repeat(64),"token_generation":1}))
    }
    async fn call(&self, op: Operation, _: Value) -> Result<Value, Failure> {
        assert!(matches!(op, Operation::List));
        self.trace.lock().unwrap().push(json!(["discover"]));
        match self.name.as_str() {
            "discovery-error" => {
                return Err(Failure::Consumer(ConsumerError::new(
                    "fixture",
                    "fixture discovery failed",
                )))
            }
            "cancel-discovery" => self.signal.cancel(),
            "lost-discovery-lease" => takeover(&self.path, "claustrum-roster"),
            _ => {}
        }
        Ok(
            json!({"view":"new","rows":[{"id":"id","credentialType":"oauth","state":"active","accountId":"account","operations":["read"],"categories":["antigravity-native"],"refreshAdapter":"antigravity"}]}),
        )
    }
    fn delay(&self, ms: u64) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(std::time::Duration::from_millis(ms)))
    }
    fn log(&self, _: &'static str, _: &'static str, _: Value) {}
    async fn close(&self) {}
}
#[tokio::test]
async fn roster_commits_and_lease_losses_match_original_storage_traces() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/vault-roster-store.json")).unwrap();
    for case in fixtures["cases"].as_array().unwrap() {
        let scratch = Scratch::new();
        let store = RosterStore::new(scratch.0.join("roster.json")).unwrap();
        let name = case["name"].as_str().unwrap().to_owned();
        if let Some(before) = case["before"].as_str() {
            save(&store.path, before);
        }
        if name.starts_with("busy") {
            takeover(&store.path, "claustrum-roster");
        }
        if name == "write-lock-timeout" {
            takeover(&store.path, "claustrum-roster-write");
        }
        let started = std::time::Instant::now();
        let trace = Arc::new(Mutex::new(vec![]));
        let signal = CancellationToken::new();
        if name == "cancel-before" {
            signal.cancel();
        }
        let value = if case["mutation"] == true {
            let operation = name.clone();
            let log = trace.clone();
            let path = store.path.clone();
            let fence_name = name.clone();
            let fence_trace = trace.clone();
            let fence_path = store.path.clone();
            let fence: Fence = Arc::new(move || {
                let name = fence_name.clone();
                let log = fence_trace.clone();
                let path = fence_path.clone();
                Box::pin(async move {
                    log.lock().unwrap().push(json!(["before-publish"]));
                    if name == "fence-error" {
                        return Err(fixture_error("fixture fence failed"));
                    }
                    if name == "lost-write-lease-at-rename" {
                        takeover(&path, "claustrum-roster-write");
                    }
                    Ok(())
                })
            });
            store
                .mutate(
                    move |current, lease| {
                        Box::pin(async move {
                            log.lock().unwrap().push(json!(["change", current]));
                            if operation == "change-error" {
                                return Err(fixture_error("fixture change failed"));
                            }
                            if operation == "lost-write-lease-in-change" {
                                takeover(&path, "claustrum-roster-write");
                            }
                            lease.assert_owned().await?;
                            let mut next = current.unwrap();
                            next["view"] = json!("changed");
                            next["number"] = json!(1e21);
                            next["extra"] = json!({"10":10,"2":2,"nested":[1.0,"line\ntext"]});
                            Ok(Change {
                                next: (operation != "no-change").then_some(next),
                                result: json!("committed"),
                            })
                        })
                    },
                    Some(fence),
                )
                .await
                .map_err(Failure::Enrollment)
        } else {
            let port = Arc::new(Fixture {
                name: name.clone(),
                path: store.path.clone(),
                trace: trace.clone(),
                signal: signal.clone(),
            });
            let custody = Custody::new(port);
            let active_name = name.clone();
            let active_trace = trace.clone();
            let count = Arc::new(AtomicUsize::new(0));
            let result = store
                .refresh(
                    |signal| {
                        let custody = custody.clone();
                        async move { custody.discover(&signal).await }
                    },
                    signal,
                    move || {
                        active_trace.lock().unwrap().push(json!(["active"]));
                        let active = !(active_name == "inactive-before"
                            || active_name == "inactive-after"
                                && count.fetch_add(1, Ordering::SeqCst) > 0);
                        async move { Ok(active) }
                    },
                    || Projection::new("en-US", 1000.0).unwrap(),
                )
                .await
                .map(|v| v.unwrap_or(Value::Null));
            custody.close().await;
            result
        };
        if name == "write-lock-timeout" {
            assert!(started.elapsed() >= std::time::Duration::from_millis(5000));
        }
        assert_eq!(result(value, &scratch.0), case["result"], "result {name}");
        assert_eq!(json!(*trace.lock().unwrap()), case["trace"], "trace {name}");
        assert_eq!(
            json!(fs::read_to_string(&store.path).ok()),
            case["after"],
            "bytes {name}"
        );
        let mut files: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        assert_eq!(json!(files), case["remaining"], "files {name}");
    }
}
#[tokio::test]
async fn contending_mutations_read_the_previous_committed_roster() {
    let scratch = Scratch::new();
    let store = RosterStore::new(scratch.0.join("roster.json")).unwrap();
    save(
        &store.path,
        json!({"version":1,"view":"0","rows":[],"declined":[]}).to_string(),
    );
    let (entered, wait_entered) = tokio::sync::oneshot::channel();
    let (release, wait_release) = tokio::sync::oneshot::channel();
    let first = store.clone();
    let first = tokio::spawn(async move {
        first
            .mutate(
                move |current, _| {
                    Box::pin(async move {
                        assert_eq!(current.as_ref().unwrap()["view"], "0");
                        entered.send(()).unwrap();
                        wait_release.await.unwrap();
                        let mut next = current.unwrap();
                        next["view"] = json!("1");
                        Ok::<_, StorageError>(Change {
                            next: Some(next),
                            result: (),
                        })
                    })
                },
                None,
            )
            .await
    });
    wait_entered.await.unwrap();
    let second = store.clone();
    let second = tokio::spawn(async move {
        second
            .mutate(
                move |current, _| {
                    Box::pin(async move {
                        assert_eq!(current.as_ref().unwrap()["view"], "1");
                        let mut next = current.unwrap();
                        next["view"] = json!("2");
                        Ok::<_, StorageError>(Change {
                            next: Some(next),
                            result: (),
                        })
                    })
                },
                None,
            )
            .await
    });
    release.send(()).unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(store.read().await.unwrap().unwrap()["view"], "2");
}

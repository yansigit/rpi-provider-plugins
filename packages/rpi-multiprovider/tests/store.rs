use rpi_multiprovider::store::{Failure, Store};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn failure(error: Failure) -> Value {
    match error {
        Failure::Public(error) => {
            json!({"error":{"name":error.name(),"message":error.to_string()}})
        }
        Failure::Syntax { line, column, .. } => {
            panic!("unexpected syntax error at {line}:{column}")
        }
        Failure::Io {
            operation,
            path,
            error,
        } => panic!("{operation} {}: {error}", path.display()),
    }
}

#[tokio::test]
async fn all_pinned_store_records_and_public_views_match() {
    let reference: Value = serde_json::from_str(include_str!("store-reference.json")).unwrap();
    for case in reference["cases"].as_array().unwrap() {
        let root = Scratch(
            std::env::temp_dir().join(format!("rpi-multiprovider-store-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&root.0).unwrap();
        let path = root.0.join("multiprovider-auth.json");
        let now = Arc::new(AtomicU64::new(1_700_000_000_000));
        let clock = now.clone();
        let next = AtomicU64::new(0);
        let store = Store::with_sources(
            path.clone(),
            Arc::new(move || clock.load(Ordering::SeqCst) as f64),
            Arc::new(move || format!("fixture-{}", next.fetch_add(1, Ordering::SeqCst) + 1)),
        );
        if let Some(initial) = case.get("initial") {
            std::fs::write(
                &path,
                format!("{}\n", serde_json::to_string_pretty(initial).unwrap()),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
        let mut last = "missing".to_string();
        for (index, action) in case["actions"].as_array().unwrap().iter().enumerate() {
            let mut actual = if action["op"] == "time" {
                now.store(action["value"].as_u64().unwrap(), Ordering::SeqCst);
                json!({"result":null})
            } else {
                let mut request = action.clone();
                if request.get("provider").is_none() {
                    request["provider"] = json!("example");
                }
                if request.get("account").is_none() {
                    request["account"] = json!(last);
                }
                match store.request(request, CancellationToken::new()).await {
                    Ok(value) => {
                        if action["op"] == "add" {
                            last = value["id"].as_str().unwrap().into();
                        }
                        assert!(!value.to_string().contains("fixture-secret"));
                        json!({"result":value})
                    }
                    Err(error) => failure(error),
                }
            };
            actual["persisted"] = json!(std::fs::read_to_string(&path).ok());
            let expected = case["results"][index].clone();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                actual["mode"] = json!(std::fs::metadata(&path)
                    .ok()
                    .map(|metadata| metadata.permissions().mode() & 0o777));
            }
            #[cfg(not(unix))]
            let expected = {
                let mut expected = expected;
                expected.as_object_mut().unwrap().remove("mode");
                expected
            };
            assert_eq!(
                actual, expected,
                "case {} operation {index}: {action}",
                case["name"]
            );
        }
    }
}

fn scratch() -> Scratch {
    let root = Scratch(
        std::env::temp_dir().join(format!("rpi-multiprovider-store-{}", uuid::Uuid::new_v4())),
    );
    std::fs::create_dir(&root.0).unwrap();
    root
}
fn add(label: &str) -> Value {
    json!({"op":"add","provider":"example","input":{"label":label,"credential":{"type":"api_key","key":"fixture-secret"}}})
}
async fn checked(store: &Store, request: Value) -> Value {
    match store.request(request, CancellationToken::new()).await {
        Ok(value) => value,
        Err(error) => panic!("unexpected store failure: {}", failure(error)),
    }
}

#[test]
#[ignore = "subprocess entry invoked by concurrent_processes_preserve_every_account"]
fn process_writer() {
    let path = PathBuf::from(std::env::var_os("RPI_MULTIPROVIDER_TEST_STORE").unwrap());
    let label = std::env::var("RPI_MULTIPROVIDER_TEST_LABEL").unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(checked(&Store::new(path), add(&label)));
}

#[tokio::test]
async fn concurrent_processes_preserve_every_account() {
    let root = scratch();
    let path = root.0.join("accounts.json");
    let mut children = Vec::new();
    for index in 0..8 {
        children.push(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "process_writer", "--ignored", "--nocapture"])
                .env("RPI_MULTIPROVIDER_TEST_STORE", &path)
                .env("RPI_MULTIPROVIDER_TEST_LABEL", format!("Account {index}"))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let pool = checked(
        &Store::new(path.clone()),
        json!({"op":"pool","provider":"example"}),
    )
    .await;
    let accounts = pool["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 8);
    for index in 0..8 {
        assert_eq!(
            accounts
                .iter()
                .filter(|account| account["label"] == format!("Account {index}"))
                .count(),
            1
        );
    }
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn waiting_cancel_does_not_remove_another_process_lock() {
    let root = scratch();
    let path = root.0.join("accounts.json");
    let lock = root.0.join("accounts.json.lock");
    std::fs::write(&lock, "held by another owner").unwrap();
    let signal = CancellationToken::new();
    let cancel = signal.clone();
    let pending =
        tokio::spawn(async move { Store::new(path).request(add("waiting"), signal).await });
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), pending)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        failure(result.err().expect("cancelled request"))["error"]["message"],
        "multiprovider: operation cancelled"
    );
    assert_eq!(
        std::fs::read_to_string(lock).unwrap(),
        "held by another owner"
    );
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
}

#[tokio::test]
async fn failed_temporary_write_preserves_store_and_releases_lock() {
    let root = scratch();
    let path = root.0.join("accounts.json");
    checked(&Store::new(path.clone()), add("before")).await;
    let before = std::fs::read(&path).unwrap();
    let temporary = root.0.join(format!(
        ".multiprovider-auth.json.{}.blocked.tmp",
        std::process::id()
    ));
    std::fs::create_dir(&temporary).unwrap();
    let store = Store::with_sources(
        path.clone(),
        Arc::new(|| 1_700_000_000_000.0),
        Arc::new(|| "blocked".into()),
    );
    assert!(matches!(
        store.request(add("after"), CancellationToken::new()).await,
        Err(Failure::Io {
            operation: "open",
            ..
        })
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!root.0.join("accounts.json.lock").exists());
    std::fs::remove_dir(temporary).unwrap();
    checked(&Store::new(path), add("retry")).await;
}

#[tokio::test]
async fn admitted_write_finishes_after_its_waiter_is_dropped() {
    use std::sync::{Condvar, Mutex};
    let root = scratch();
    let path = root.0.join("accounts.json");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let callback_gate = gate.clone();
    let count = AtomicU64::new(0);
    let store = Store::with_sources(
        path.clone(),
        Arc::new(|| 1_700_000_000_000.0),
        Arc::new(move || {
            let index = count.fetch_add(1, Ordering::SeqCst);
            if index == 1 {
                entered.lock().unwrap().take().unwrap().send(()).unwrap();
                let (ready, changed) = &*callback_gate;
                let mut ready = ready.lock().unwrap();
                while !*ready {
                    ready = changed.wait(ready).unwrap();
                }
            }
            format!("fixture-{index}")
        }),
    );
    let task = tokio::spawn(async move {
        store
            .request(add("persisted"), CancellationToken::new())
            .await
    });
    waiting.await.unwrap();
    assert!(root.0.join("accounts.json.lock").exists());
    task.abort();
    assert!(matches!(task.await, Err(error) if error.is_cancelled()));
    {
        let (ready, changed) = &*gate;
        *ready.lock().unwrap() = true;
        changed.notify_one();
    }
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while root.0.join("accounts.json.lock").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pool = checked(&Store::new(path), json!({"op":"pool","provider":"example"})).await;
    assert_eq!(pool["accounts"][0]["label"], "persisted");
    assert_eq!(std::fs::read_dir(&root.0).unwrap().count(), 1);
}

#[tokio::test]
async fn stale_lock_is_replaced_but_a_live_lock_times_out_without_unlink() {
    use std::time::{Duration, UNIX_EPOCH};
    let root = scratch();
    let path = root.0.join("accounts.json");
    let lock = root.0.join("accounts.json.lock");
    let file = std::fs::File::create(&lock).unwrap();
    file.set_times(
        std::fs::FileTimes::new()
            .set_modified(UNIX_EPOCH + Duration::from_millis(1_700_000_000_000)),
    )
    .unwrap();
    drop(file);
    let store = Store::with_sources(
        path.clone(),
        Arc::new(|| 1_700_000_120_001.0),
        Arc::new(|| "stale-test".into()),
    );
    checked(&store, add("recovered")).await;
    assert!(!lock.exists());
    std::fs::write(&lock, "live").unwrap();
    let time = AtomicU64::new(0);
    let store = Store::with_sources(
        path,
        Arc::new(move || 1_700_000_000_000.0 + time.fetch_add(10_000, Ordering::SeqCst) as f64),
        Arc::new(|| "timeout-test".into()),
    );
    let error = store
        .request(add("timed out"), CancellationToken::new())
        .await
        .err()
        .expect("lock timeout");
    assert_eq!(
        failure(error)["error"]["message"],
        format!(
            "multiprovider: timed out waiting for auth store lock at {}",
            lock.display()
        )
    );
    assert_eq!(std::fs::read_to_string(lock).unwrap(), "live");
}

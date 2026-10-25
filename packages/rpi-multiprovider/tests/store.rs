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
    // Upstream sorts with the process's default locale; the fixture was
    // generated under en-US, so pin the same locale for the native collator.
    std::env::set_var("LC_ALL", "en_US.UTF-8");
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

#[tokio::test]
async fn private_auth_transactions_bind_identity_and_commit_before_release() {
    use rpi_multiprovider::store::port::{Port, STORE_ID};
    let root = scratch();
    let path = root.0.join("accounts.json");
    let store = Store::new(path.clone());
    let saved = checked(&store, add("private")).await;
    let port = Port::new(store.clone());
    let request = |action: &str, input: Value| json!({"version":1,"storeId":STORE_ID,"providerId":"example","accountId":saved["id"],"action":action,"input":input});
    let checked_port = |input| async {
        match port.request(input, CancellationToken::new()).await {
            Ok(value) => value,
            Err(error) => panic!("unexpected private port failure: {}", failure(error)),
        }
    };
    let read = checked_port(request("read", Value::Null)).await;
    assert_eq!(read["kind"], "credential");
    assert_eq!(read["payload"]["key"], "fixture-secret");
    let locked = checked_port(request("lock", Value::Null)).await;
    let token = locked["payload"]["lockId"].as_str().unwrap();
    assert!(root.0.join("accounts.json.lock").exists());
    let credential = json!({"type":"oauth","access":"fixture-access","refresh":"fixture-rotated","expires":1_800_000_000_123i64,"projectId":"fixture-project"});
    let replacement = request("replace", json!({"lockId":token,"credential":credential}));
    let mut forged = replacement.clone();
    forged["accountId"] = json!("different-account");
    assert_eq!(
        failure(
            port.request(forged, CancellationToken::new())
                .await
                .err()
                .expect("identity mismatch")
        )["error"]["message"],
        "multiprovider: credential transaction identity mismatch"
    );
    assert_eq!(checked_port(replacement).await["kind"], "success");
    let stored: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored["providers"]["example"]["accounts"][0]["credential"],
        credential
    );
    assert!(root.0.join("accounts.json.lock").exists());
    port.release(token);
    port.release(token);
    assert!(!root.0.join("accounts.json.lock").exists());
    assert!(port
        .request(
            request("remove", json!({"lockId":token})),
            CancellationToken::new()
        )
        .await
        .is_err());
    checked(&store, json!({"op":"account","provider":"example","account":saved["id"],"input":{"enabled":false}})).await;
    assert!(port
        .request(request("read", Value::Null), CancellationToken::new())
        .await
        .is_err());
}

/// `/multilogin`'s in-flight logins own the `pending-login-` id namespace in
/// memory; a stored account never claims it, whichever side tries.
#[tokio::test]
async fn reserved_login_ids_are_never_stored_accounts() {
    let root = scratch();
    let path = root.0.join("accounts.json");
    let store = Store::new(path.clone());
    checked(&store, add("Real")).await;

    // The store never mints a reserved id for a new account.
    let minting = Store::with_sources(
        path.clone(),
        Arc::new(|| 1.0),
        Arc::new(|| "pending-login-minted".to_owned()),
    );
    assert_eq!(
        failure(
            minting
                .request(add("Minted"), CancellationToken::new())
                .await
                .err()
                .expect("reserved id was minted")
        )["error"]["message"],
        "multiprovider: account id \"pending-login-minted\" is reserved for in-flight logins"
    );
    let accounts = checked(&store, json!({"op":"pool","provider":"example"})).await["accounts"]
        .as_array()
        .cloned()
        .unwrap();
    assert_eq!(
        accounts.len(),
        1,
        "a refused add stores nothing: {accounts:?}"
    );
    assert_eq!(accounts[0]["label"], "Real");
}

/// Account ids stored under one provider, read straight from the file.
fn stored_ids(file: &Value, provider: &str) -> Vec<String> {
    file["providers"][provider]["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|account| account["id"].as_str().unwrap().to_owned())
        .collect()
}

/// A hand-edited `pending-login-*` record must not brick the store. Listing,
/// use and the scheduler skip it, the other accounts keep working, writes leave
/// it in the file, and `remove` deletes it through the request path.
#[tokio::test]
async fn reserved_records_are_skipped_and_removable_through_the_request_path() {
    use rpi_multiprovider::native::credentials;
    use rpi_multiprovider::store::port::{Port, STORE_ID};

    let root = scratch();
    let path = root.0.join("accounts.json");
    let store = Store::new(path.clone());
    let saved = checked(&store, add("Real")).await;
    let saved_id = saved["id"].as_str().unwrap().to_owned();

    // The same pool holds a planted record, and a second provider holds only one.
    let mut file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let mut forged = file["providers"]["example"]["accounts"][0].clone();
    forged["id"] = json!("pending-login-forged");
    forged["label"] = json!("Forged");
    file["providers"]["example"]["accounts"]
        .as_array_mut()
        .unwrap()
        .push(forged.clone());
    let mut orphan = forged.clone();
    orphan["id"] = json!("pending-login-orphan");
    file["providers"]["orphan"] = json!({"policy":"round-robin","affinity":true,
        "includeUpstream":true,"accounts":[orphan]});
    std::fs::write(&path, file.to_string()).unwrap();

    // Listing, presence and the pool never show a record.
    assert_eq!(
        checked(&store, json!({"op":"list"})).await,
        json!(["example"])
    );
    let pool = checked(&store, json!({"op":"pool","provider":"example"})).await;
    let listed: Vec<&str> = pool["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|account| account["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, [saved_id.as_str()]);
    assert_eq!(
        checked(&store, json!({"op":"has","provider":"example"})).await,
        json!(true)
    );
    assert_eq!(
        checked(&store, json!({"op":"has","provider":"orphan"})).await,
        json!(false)
    );

    // The scheduler's inventory never schedules a record, and the host cannot
    // read or replace one.
    let inventory = native_accounts(&store);
    assert!(inventory.contains(&saved_id), "{inventory:?}");
    assert!(!inventory.iter().any(|id| id.starts_with("pending-login-")));
    let port = Port::new(store.clone());
    let read = json!({"version":1,"storeId":STORE_ID,"providerId":"example",
        "accountId":"pending-login-forged","action":"read","input":null});
    assert!(port.request(read, CancellationToken::new()).await.is_err());
    assert!(matches!(
        credentials::read(&path, "example", "pending-login-forged"),
        Ok(None)
    ));

    // Accounts keep working: one is relabelled and another is added.
    checked(
        &store,
        json!({"op":"account","provider":"example","account":saved_id,"input":{"label":"Renamed"}}),
    )
    .await;
    let second = checked(&store, add("Second")).await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    // Addressing the record directly is refused with the reserved diagnostic.
    let refused = store
        .request(
            json!({"op":"account","provider":"example","account":"pending-login-forged","input":{"label":"Edited"}}),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("a reserved record was edited");
    assert_eq!(
        failure(refused)["error"]["message"],
        "multiprovider: account id \"pending-login-forged\" is reserved for in-flight logins"
    );

    // Writes leave the records where they were.
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored_ids(&file, "example"),
        [
            saved_id.clone(),
            "pending-login-forged".into(),
            second_id.clone()
        ]
    );
    assert_eq!(stored_ids(&file, "orphan"), ["pending-login-orphan"]);
    assert_eq!(
        file["providers"]["example"]["accounts"][0]["label"],
        "Renamed"
    );

    // `remove` deletes a record, and the orphan's pool goes with its last one.
    assert_eq!(
        checked(
            &store,
            json!({"op":"remove","provider":"example","account":"pending-login-forged"})
        )
        .await,
        json!(true)
    );
    assert_eq!(
        checked(
            &store,
            json!({"op":"remove","provider":"orphan","account":"pending-login-orphan"})
        )
        .await,
        json!(true)
    );
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(stored_ids(&file, "example"), [saved_id, second_id]);
    assert!(file["providers"].get("orphan").is_none(), "{file}");
}

/// Account ids the scheduler inventory serves for `example`.
fn native_accounts(store: &Store) -> Vec<String> {
    rpi_multiprovider::native::provider_accounts(store, "example")
        .unwrap()
        .iter()
        .map(|account| account["id"].as_str().unwrap().to_owned())
        .collect()
}

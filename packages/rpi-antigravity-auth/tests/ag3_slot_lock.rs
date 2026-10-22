#![cfg(unix)]
//! AG-3 slot-lock faults: the pinned renewable lease and credential slot driven
//! through corrupt identity, marker corruption, loss/compromise, cancellation
//! before commit and a real competing child process.
use rpi_antigravity_auth::vault::{
    ceremony::Lease,
    file_lease::{FileLease, Options},
    slot::{Error as SlotError, Slot, Step},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, UNIX_EPOCH},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scratch(prefix: &str) -> Scratch {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Scratch(path)
}
fn save(path: &Path, value: &Value) {
    fs::write(path, format!("{value}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
fn touch(path: &Path, ms: u64) {
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(ms)))
        .unwrap();
}

// ---------------------------------------------------------------------------
// Renewable file lease
// ---------------------------------------------------------------------------
fn lease_normalize(value: &Value, owner: &str) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let value = if key == "ownerId" && value.as_str() == Some(owner) {
                        json!("<OWNER>")
                    } else {
                        lease_normalize(value, owner)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.iter().map(|v| lease_normalize(v, owner)).collect())
        }
        _ => value.clone(),
    }
}
fn lease_snapshot(lock: &Path, marker: &Path, owner: &str) -> Value {
    let value = match fs::read(lock) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(json!("malformed")),
        Err(error) if error.raw_os_error() == Some(libc::EISDIR) => {
            match fs::read(lock.join("owner.json")) {
                Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or(json!("malformed")),
                Err(_) => Value::Null,
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(_) => json!("malformed"),
    };
    let marker_value = fs::read(marker.join("owner.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let marker_kind = fs::symlink_metadata(marker)
        .ok()
        .map(|stat| {
            if stat.is_dir() {
                json!("directory")
            } else if stat.file_type().is_symlink() {
                json!("symlink")
            } else {
                json!("file")
            }
        })
        .unwrap_or(Value::Null);
    json!({
        "lock": lease_normalize(&value, owner),
        "marker": marker_value.map(|m| if m["ownerId"] == "other-marker" { json!("other-marker") } else { json!("present") }).unwrap_or(Value::Null),
        "markerKind": marker_kind,
    })
}
fn lease_mutate(kind: &str, lock: &Path, marker: &Path, now: u64) {
    if kind == "takeover" {
        save(lock, &json!({"ownerId":"successor","expiresAt":now+60000}));
    } else {
        fs::create_dir_all(marker).unwrap();
        save(
            &marker.join("owner.json"),
            &json!({"ownerId":"other-marker","createdAt":now}),
        );
        touch(marker, now);
    }
}
#[tokio::test]
async fn pinned_lease_corrupt_identity_marker_loss_and_cancellation_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("ag3-slot-lock.json")).unwrap();
    for case in fixture["lease"].as_array().unwrap() {
        let spec = &case["spec"];
        let scratch = scratch("ag3-lease");
        let path = scratch.0.join("state.json");
        let lock_path = scratch.0.join("state.json.ceremony.lock");
        let marker = scratch.0.join("state.json.ceremony.lock.evicting");
        if let Some(initial) = spec.get("initial") {
            let age = spec["age"].as_u64().unwrap_or(0);
            if spec["legacy"] == true {
                fs::create_dir(&lock_path).unwrap();
                if spec["malformedOwner"] == true {
                    fs::write(lock_path.join("owner.json"), "broken").unwrap();
                } else {
                    save(&lock_path.join("owner.json"), initial);
                }
            } else {
                save(&lock_path, initial);
                if spec["symlink"] == true {
                    fs::remove_file(&lock_path).unwrap();
                    let target = scratch.0.join("state.json.ceremony.lock.target");
                    save(&target, initial);
                    symlink(&target, &lock_path).unwrap();
                }
            }
            touch(&lock_path, 100000 - age);
        }
        if let Some(kind) = spec.get("markerKind").and_then(Value::as_str) {
            match kind {
                "file" => fs::write(&marker, "marker\n").unwrap(),
                "malformed" => {
                    fs::create_dir_all(&marker).unwrap();
                    fs::write(marker.join("owner.json"), "broken").unwrap();
                }
                "other" => {
                    fs::create_dir_all(&marker).unwrap();
                    save(
                        &marker.join("owner.json"),
                        &json!({"ownerId":"other-marker","createdAt":100000}),
                    );
                }
                _ => panic!("unknown marker kind {kind}"),
            }
            touch(&marker, 100000 - spec["markerAge"].as_u64().unwrap_or(0));
        }
        let clock = Arc::new(AtomicU64::new(100000));
        let trace = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut options = Options::new(path, "ceremony", 30000, spec["renew"] == true);
        let now = clock.clone();
        options.now = Arc::new(move || now.load(Ordering::SeqCst) as f64);
        options.backoff = Arc::new(|| 0);
        let events = trace.clone();
        let fired = AtomicBool::new(false);
        let race = spec["race"].clone();
        let now = clock.clone();
        let lp = lock_path.clone();
        let mp = marker.clone();
        options.step = Arc::new(move |step| {
            events.lock().unwrap().push(step.into());
            if race["step"] == step && !fired.swap(true, Ordering::SeqCst) {
                lease_mutate(
                    race["mutation"].as_str().unwrap(),
                    &lp,
                    &mp,
                    now.load(Ordering::SeqCst),
                );
            }
        });
        let mut lease = FileLease::acquire(options).await.unwrap();
        let owner = lease
            .as_ref()
            .map(|l| l.owner_id().to_owned())
            .unwrap_or_default();
        let mut results = vec![
            json!({"operation":"acquire","acquired":lease.is_some(),"after":lease_snapshot(&lock_path,&marker,&owner)}),
        ];
        if lease.is_some() {
            for step in spec["steps"].as_array().unwrap() {
                let step = step.as_str().unwrap();
                let mut result = json!({"ok":true});
                let mut lost = lease.as_ref().map(|l| l.loss().is_some()).unwrap_or(false);
                match step {
                    "corruptLock" => fs::write(&lock_path, "broken").unwrap(),
                    "corruptLockDir" => {
                        let _ = fs::remove_file(&lock_path);
                        fs::create_dir(&lock_path).unwrap();
                        fs::write(lock_path.join("owner.json"), "broken").unwrap();
                    }
                    "removeLock" => {
                        if lock_path.is_dir() {
                            fs::remove_dir_all(&lock_path).unwrap();
                        } else {
                            let _ = fs::remove_file(&lock_path);
                        }
                    }
                    "renew" => {
                        clock.fetch_add(10000, Ordering::SeqCst);
                        lease.as_ref().unwrap().renew_now().await.unwrap();
                    }
                    "assert" | "release" => {
                        let outcome = if step == "assert" {
                            lease.as_ref().unwrap().assert_owned().await
                        } else {
                            lease.take().unwrap().release().await
                        };
                        if let Err(error) = outcome {
                            result = json!({"error":{"name":"LockOwnershipError","message":error.to_string().replace(scratch.0.to_str().unwrap(),"<DIR>")}});
                        }
                    }
                    _ => panic!("unknown lease step {step}"),
                }
                if lease.is_some() {
                    lost = lease.as_ref().unwrap().loss().is_some();
                }
                results.push(json!({"operation":step,"result":result,"lost":lost,"after":lease_snapshot(&lock_path,&marker,&owner)}));
            }
        }
        assert_eq!(json!(results), case["results"], "{}", spec["name"]);
        assert_eq!(json!(*trace.lock().unwrap()), case["trace"], "{}", spec["name"]);
    }
}

// ---------------------------------------------------------------------------
// Credential slot: interrupted commit and a second instance
// ---------------------------------------------------------------------------
fn slot_snapshot(path: &Path, hash: &str) -> Value {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(error) => panic!("{error}"),
        Ok(stat) if stat.is_dir() => json!({"kind":"directory"}),
        Ok(stat) if stat.is_symlink() => json!({"kind":"symlink"}),
        Ok(stat) => {
            let bytes = fs::read(path).unwrap();
            match String::from_utf8(bytes.clone()) {
                Ok(text) => {
                    json!({"kind":"file","mode":stat.permissions().mode()&0o777,"text":text.replace(hash,"<auth-identity>")})
                }
                Err(_) => {
                    json!({"kind":"file","mode":stat.permissions().mode()&0o777,"bytes":bytes})
                }
            }
        }
    }
}
#[test]
fn pinned_slot_interrupted_commit_and_second_instance_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("ag3-slot-lock.json")).unwrap();
    for case in fixture["slot"].as_array().unwrap() {
        let spec = &case["spec"];
        let scratch = scratch("ag3-slot");
        let dir = scratch.0.join("case");
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let auth = dir.join("auth.json");
        let stash_dir = dir.join("vault");
        let stash = stash_dir.join("stash.json");
        fs::DirBuilder::new().mode(0o700).create(&stash_dir).unwrap();
        if let Some(raw) = spec["raw"].as_str() {
            fs::write(&auth, raw).unwrap();
            fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let hash: String = Sha256::digest(json!(auth).to_string().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let hook = spec["hook"].as_str().unwrap_or("").to_owned();
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let build = |events: Arc<Mutex<Vec<String>>>, fired: Arc<AtomicBool>| {
            let auth_path = auth.clone();
            let hook = hook.clone();
            Slot::new(&auth, &stash, "google", "antigravity-vault-custody")
                .unwrap()
                .with_hook(Arc::new(move |step, path| {
                    let target = if path == auth_path { "auth" } else { "stash" };
                    if step == Step::BeforeRename {
                        let name = format!("before-rename-{target}");
                        events.lock().unwrap().push(name.clone());
                        if name == hook && !fired.swap(true, Ordering::SeqCst) {
                            return Err(SlotError::Slot("io"));
                        }
                        return Ok(());
                    }
                    let name = match step {
                        Step::StashWritten => "stash-written",
                        Step::AuthWritten => "auth-written",
                        Step::StashDeleted => "stash-deleted",
                        _ => "",
                    };
                    if name.is_empty() {
                        return Ok(());
                    }
                    events.lock().unwrap().push(name.into());
                    if name == hook && !fired.swap(true, Ordering::SeqCst) {
                        return Err(SlotError::Slot("io"));
                    }
                    Ok(())
                }))
        };
        let slot = build(events.clone(), fired.clone());
        let other = build(events.clone(), fired.clone());
        for (index, action) in spec["steps"].as_array().unwrap().iter().enumerate() {
            let op = action["op"].as_str().unwrap();
            let value: Result<Option<Value>, SlotError> = match op {
                "inspect" => slot.inspect().map(Some),
                "enter" => slot.enter().map(|()| Some(Value::Null)),
                "exit" => slot.exit().map(|value| Some(json!(value))),
                "otherInspect" => other.inspect().map(Some),
                "otherExit" => other.exit().map(|value| Some(json!(value))),
                "write" => {
                    fs::write(&auth, action["raw"].as_str().unwrap()).unwrap();
                    Ok(None)
                }
                _ => panic!("unknown slot op {op}"),
            };
            let outcome = match value {
                Ok(Some(value)) => json!({"value":value}),
                Ok(None) => json!({}),
                Err(SlotError::Slot(code)) => {
                    json!({"error":{"name":"PiSlotError","message":format!("Pi slot: {code}"),"code":code}})
                }
                Err(error) => panic!("{error}"),
            };
            let actual = json!({"outcome":outcome,"events":*events.lock().unwrap(),"auth":slot_snapshot(&auth,&hash),"stash":slot_snapshot(&stash,&hash)});
            assert_eq!(
                actual, case["results"][index],
                "{} step {index}",
                spec["name"]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Competing processes
// ---------------------------------------------------------------------------
async fn wait_for(path: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(std::time::Instant::now() < deadline, "timed out: {path:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
#[tokio::test]
async fn ag3_child_lease_holder() {
    let Some(directory) = std::env::var_os("RPI_AG3_CHILD_DIR") else {
        return;
    };
    let dir = PathBuf::from(directory);
    let mut options = Options::new(dir.join("state.json"), "ceremony", 30_000, false);
    options.now = Arc::new(|| 100_000.0);
    options.backoff = Arc::new(|| 0);
    let lease = FileLease::acquire(options)
        .await
        .unwrap()
        .expect("child must acquire the free lease");
    fs::write(
        dir.join("held.json"),
        format!("{{\"ownerId\":\"{}\"}}\n", lease.owner_id()),
    )
    .unwrap();
    while !dir.join("release").exists() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    lease.release().await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn competing_child_process_holds_then_hands_off_the_lease() {
    let fixture: Value = serde_json::from_str(include_str!("ag3-slot-lock.json")).unwrap();
    let expected = &fixture["process"][0];
    let scratch = scratch("ag3-process");
    let dir = scratch.0.clone();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "ag3_child_lease_holder", "--nocapture"])
        .env("RPI_AG3_CHILD_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for(&dir.join("held.json")).await;
    let options = || {
        let mut options = Options::new(dir.join("state.json"), "ceremony", 30_000, false);
        options.now = Arc::new(|| 100_000.0);
        options.backoff = Arc::new(|| 0);
        options
    };
    let contender = FileLease::acquire(options()).await.unwrap();
    let contender_value = contender.is_some().then_some(json!("acquired"));
    if let Some(contender) = contender {
        contender.release().await.unwrap();
    }
    fs::write(dir.join("release"), "").unwrap();
    let status = child.wait().unwrap();
    let successor = FileLease::acquire(options())
        .await
        .unwrap()
        .expect("the free lease must be acquirable after the child releases it");
    let saved: Value =
        serde_json::from_slice(&fs::read(dir.join("state.json.ceremony.lock")).unwrap()).unwrap();
    let successor_value = json!({"ownerId":"<OWNER>","expiresAt":saved["expiresAt"]});
    successor.release().await.unwrap();
    let mut leftover: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    leftover.sort();
    let actual = json!({
        "name": expected["name"],
        "childExit": status.code(),
        "contender": contender_value,
        "successor": successor_value,
        "leftover": leftover,
    });
    assert_eq!(actual, *expected);
}

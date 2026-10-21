#![cfg(unix)]
use rpi_antigravity_auth::vault::{
    ceremony::Lease,
    file_lease::{FileLease, Options},
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
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
fn scratch() -> Scratch {
    let path = std::env::temp_dir().join(format!("rpi-vault-lease-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Scratch(path)
}
fn save(path: &Path, value: &Value) {
    fs::write(path, format!("{value}\n")).unwrap();
}
fn touch(path: &Path, ms: u64) {
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_millis(ms)))
        .unwrap();
}
fn mutate(kind: &str, path: &Path, marker: &Path, now: u64) {
    if kind == "takeover" {
        save(path, &json!({"ownerId":"successor","expiresAt":now+60000}));
    } else {
        fs::create_dir_all(marker).unwrap();
        save(
            &marker.join("owner.json"),
            &json!({"ownerId":"other-marker","createdAt":now}),
        );
        touch(marker, now);
    }
}
fn snapshot(path: &Path, marker: &Path, owner: &str) -> Value {
    let bytes = fs::read(if path.is_dir() {
        path.join("owner.json")
    } else {
        path.into()
    });
    let mut value = match bytes {
        Ok(b) => serde_json::from_slice(&b).unwrap_or(json!("malformed")),
        Err(_) => Value::Null,
    };
    if value["ownerId"] == owner {
        value["ownerId"] = json!("<OWNER>");
    }
    let marker = fs::read(marker.join("owner.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    json!({"lock":value,"marker":marker.map(|m|if m["ownerId"]=="other-marker"{"other-marker"}else{"present"})})
}
#[tokio::test]
async fn lease_acquisition_renewal_and_races_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-lease.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let scratch = scratch();
        let path = scratch.0.join("state.json");
        let lock_path = scratch.0.join("state.json.ceremony.lock");
        let marker = scratch.0.join("state.json.ceremony.lock.evicting");
        let spec = &case["spec"];
        if let Some(initial) = spec.get("initial") {
            if spec["legacy"] == true {
                fs::create_dir(&lock_path).unwrap();
                save(&lock_path.join("owner.json"), initial);
            } else if initial == "malformed" {
                fs::write(&lock_path, "broken").unwrap();
            } else {
                save(&lock_path, initial);
            }
            touch(&lock_path, 100000 - spec["age"].as_u64().unwrap_or(0));
        }
        if let Some(age) = spec["markerAge"].as_u64() {
            mutate("marker", &lock_path, &marker, 100000);
            touch(&marker, 100000 - age);
        }
        let clock = Arc::new(AtomicU64::new(100000));
        let trace = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut options = Options::new(path, "ceremony", 30000, false);
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
                mutate(
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
            json!({"operation":"acquire","acquired":lease.is_some(),"after":snapshot(&lock_path,&marker,&owner)}),
        ];
        if lease.is_some() {
            for step in spec["steps"].as_array().unwrap() {
                let mut result = json!({"ok":true});
                let step = step.as_str().unwrap();
                let mut lost = lease.as_ref().unwrap().loss().is_some();
                match step {
                    "expire" => {
                        clock.fetch_add(30000, Ordering::SeqCst);
                    }
                    "takeover" | "marker" => {
                        mutate(step, &lock_path, &marker, clock.load(Ordering::SeqCst))
                    }
                    "renew" => {
                        clock.fetch_add(10000, Ordering::SeqCst);
                        if lost {
                            result = json!({"scheduled":false});
                        } else {
                            lease.as_ref().unwrap().renew_now().await.unwrap();
                            lost = lease.as_ref().unwrap().loss().is_some();
                        }
                    }
                    "assert" | "release" => {
                        let outcome = if step == "assert" {
                            let value = lease.as_ref().unwrap().assert_owned().await;
                            lost = lease.as_ref().unwrap().loss().is_some();
                            value
                        } else {
                            lease.take().unwrap().release().await
                        };
                        if let Err(error) = outcome {
                            result = json!({"error":{"name":"LockOwnershipError","message":error.to_string().replace(scratch.0.to_str().unwrap(),"<DIR>")}});
                        }
                    }
                    _ => panic!("unknown step"),
                }
                results.push(json!({"operation":step,"result":result,"lost":lost,"after":snapshot(&lock_path,&marker,&owner)}));
            }
        }
        assert_eq!(json!(results), case["results"], "{spec}");
        assert_eq!(json!(*trace.lock().unwrap()), case["trace"], "{spec}");
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_runtimes_contend_and_background_renewal_stops_on_release() {
    let scratch = scratch();
    let path = scratch.0.join("state.json");
    let clock = Arc::new(AtomicU64::new(100000));
    let now = clock.clone();
    let mut options = Options::new(path.clone(), "ceremony", 30000, true);
    options.now = Arc::new(move || now.load(Ordering::SeqCst) as f64);
    options.renewal_interval = Some(Duration::from_millis(10));
    let (sent, mut received) = tokio::sync::mpsc::unbounded_channel();
    options.step = Arc::new(move |step| {
        if step == "renewal-finished" {
            let _ = sent.send(());
        }
    });
    let lease = FileLease::acquire(options).await.unwrap().unwrap();
    clock.store(110000, Ordering::SeqCst);
    tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .unwrap();
    let mut competing = Options::new(path.clone(), "ceremony", 30000, false);
    competing.now = Arc::new(|| 110000.0);
    assert!(FileLease::acquire(competing).await.unwrap().is_none());
    let saved: Value =
        serde_json::from_slice(&fs::read(scratch.0.join("state.json.ceremony.lock")).unwrap())
            .unwrap();
    assert_eq!(saved["expiresAt"], 140000.0);
    lease.release().await.unwrap();
    let successor = FileLease::acquire(Options::new(path, "ceremony", 30000, false))
        .await
        .unwrap()
        .unwrap();
    successor.assert_owned().await.unwrap();
    successor.release().await.unwrap();
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}

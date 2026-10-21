#![cfg(target_os = "linux")]
use rpi_antigravity_auth::vault::{
    ceremony::{Client, Manager, Slot, Store},
    client::Operation,
    errors::Error,
    file_store::FileStore,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct Refuse(Arc<AtomicUsize>);
impl Client for Refuse {
    async fn invoke(&self, _operation: Operation, _input: Value) -> Result<Value, Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("persistence failed before any RPC")
    }
}
#[tokio::test]
async fn fsync_failure_subprocess() {
    if std::env::var_os("RPI_FSYNC_TEST_CHILD").is_none() {
        return;
    }
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-fsync.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let scratch =
            Scratch(std::env::temp_dir().join(format!("rpi-vault-fsync-{}", uuid::Uuid::new_v4())));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&scratch.0)
            .unwrap();
        let token = case["kind"] == "token";
        let store = FileStore::new(
            scratch.0.join(if token {
                "state.json"
            } else {
                "sync-failure.json"
            }),
            scratch.0.join(if token {
                "sync-failure.json"
            } else {
                "token.json"
            }),
        )
        .unwrap();
        let old = json!({"token":"a".repeat(64),"token_generation":1});
        fs::write(&store.token, format!("{old}\n")).unwrap();
        fs::set_permissions(&store.token, fs::Permissions::from_mode(0o600)).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let manager = Manager {
            store: store.clone(),
            client: Refuse(calls.clone()),
            proposed_name: "antigravity-auth-pi".into(),
            now: Arc::new(|| 42000),
            mint_secret: Arc::new(|| Ok("a".repeat(64))),
        };
        let error = if token {
            store
                .write(
                    Slot::Token,
                    json!({"token":"b".repeat(64),"token_generation":2}),
                )
                .await
                .unwrap_err()
        } else {
            manager.reconcile().await.unwrap_err()
        };
        let result =
            json!({"error":{"name":"Error","code":error.io_code(),"message":error.to_string()}});
        let mut entries: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        assert_eq!(
            json!({"kind":case["kind"],"result":result,"calls":calls.load(Ordering::SeqCst),"token":store.read(Slot::Token).await.unwrap(),"entries":entries}),
            *case
        );
    }
}
#[test]
fn failed_fsync_matches_original_without_acknowledging_or_leaking_staged_credentials() {
    let scratch = Scratch(
        std::env::temp_dir().join(format!("rpi-vault-fsync-shim-{}", uuid::Uuid::new_v4())),
    );
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch.0)
        .unwrap();
    let shim = scratch.0.join("fsync-failure.so");
    let compiler = Command::new("cc")
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC",
        ])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fsync-failure.c"))
        .arg("-ldl")
        .arg("-o")
        .arg(&shim)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "fsync_failure_subprocess", "--nocapture"])
        .env("LD_PRELOAD", &shim)
        .env("RPI_FSYNC_FAILURE_MATCH", ".sync-failure.json.")
        .env("RPI_FSYNC_TEST_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

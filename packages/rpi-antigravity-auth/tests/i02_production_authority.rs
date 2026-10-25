//! I02 real FileStore/Source boundary, plus the optional loaded-host fixture.
use rpi_antigravity_auth::vault::{
    ceremony::{Slot, Store},
    file_store::FileStore,
    import_guard::FRESH_ENROLLMENT_NOTICE,
    source::Source,
};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn raw_enrollment_provenance_is_refused_before_consumer_decoding() {    let scratch =
        Scratch(std::env::temp_dir().join(format!("vault-authority-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let base = [
        ("pi-mode.json", json!({"version":1,"mode":"custody"})),
        (
            "pi-enrollment-state.json",
            json!({"version":1,"phase":"approved","proposedName":"antigravity-auth-pi","approvedName":"antigravity-auth-pi","tokenGeneration":2,"updatedAt":1729000000000_f64}),
        ),
        (
            "pi-enrollment.json",
            json!({"token":"a".repeat(64),"token_generation":2}),
        ),
    ];
    let save = |name: &str, value: &Value| {
        let path = scratch.0.join(name);
        std::fs::write(&path, value.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    };
    let source = Source::new(&scratch.0).unwrap();
    for (name, value) in &base {
        save(name, value);
    }
    assert_eq!(
        source.request_mode().await.unwrap()["record"]["mode"],
        "custody"
    );
    for (name, value) in &base {
        for marker in [
            json!({"imported":false}),
            json!({"imported":{"sessionVersion":999}}),
            json!({"nested":{"importedFrom":"pi"}}),
        ] {
            let mut imported = value.clone();
            imported
                .as_object_mut()
                .unwrap()
                .extend(marker.as_object().unwrap().clone());
            save(name, &imported);
            assert_eq!(
                source.request_mode().await.unwrap_err(),
                FRESH_ENROLLMENT_NOTICE,
                "{name}"
            );
            assert_eq!(
                source.current().await.unwrap_err(),
                FRESH_ENROLLMENT_NOTICE,
                "{name}"
            );
            save(name, value);
        }
    }
    assert_eq!(source.current().await.unwrap()["mode"], "custody");
    save("pi-enrollment.json.bak", &json!({}));
    assert_eq!(
        source.request_mode().await.unwrap_err(),
        FRESH_ENROLLMENT_NOTICE
    );
}

/// A crashed or concurrent write leaves its staging file behind. Staging files are
/// never read and carry no authority, so they must not refuse a healthy vault.
#[tokio::test]
async fn interrupted_write_staging_never_refuses_the_vault() {
    let scratch = vault_dir();
    let save = |name: &str, value: &Value| write_private(&scratch.0.join(name), &value.to_string());
    save("pi-mode.json", &json!({"version":1,"mode":"custody"}));
    let state = json!({"version":1,"phase":"approved","proposedName":"antigravity-auth-pi","approvedName":"antigravity-auth-pi","tokenGeneration":2,"updatedAt":1729000000000_f64});
    let token = json!({"token":"a".repeat(64),"token_generation":2});
    save("pi-enrollment-state.json", &state);
    save("pi-enrollment.json", &token);
    let source = Source::new(&scratch.0).unwrap();
    assert_eq!(
        source.request_mode().await.unwrap()["record"]["mode"],
        "custody"
    );

    // The exact names the atomic writers stage and rename, left by a process that
    // died between write and rename.
    let pid = std::process::id();
    let staging = uuid::Uuid::new_v4();
    for name in [
        format!(".pi-enrollment-state.json.{pid}.{staging}.tmp"),
        format!(".pi-enrollment.json.{pid}.0123456789abcdef01234567.tmp"),
        format!("pi-mode.json.{staging}.tmp"),
        format!("google-login-stash.json.{staging}.tmp"),
        format!("pi-enrollment-state.json.ceremony.lock.{staging}.tmp"),
    ] {
        write_private(&scratch.0.join(&name), "{\"token\":\"partial");
    }

    let store = FileStore::new(
        scratch.0.join("pi-enrollment-state.json"),
        scratch.0.join("pi-enrollment.json"),
    )
    .unwrap();
    assert_eq!(store.read(Slot::State).await.unwrap(), Some(state));
    assert_eq!(store.read(Slot::Token).await.unwrap(), Some(token));
    assert_eq!(source.current().await.unwrap()["mode"], "custody");
    assert_eq!(
        source.request_mode().await.unwrap()["record"]["mode"],
        "custody"
    );

    // Real import evidence next to the staging files still refuses.
    write_private(&scratch.0.join("pi-enrollment.json.bak"), "{}");
    assert_eq!(
        source.request_mode().await.unwrap_err(),
        FRESH_ENROLLMENT_NOTICE
    );
}

/// Writers stage next to the live records; a concurrent reader must never observe
/// the staging file as imported evidence.
#[tokio::test]
async fn concurrent_writes_do_not_refuse_reads() {
    let scratch = vault_dir();
    let store = FileStore::new(
        scratch.0.join("pi-enrollment-state.json"),
        scratch.0.join("pi-enrollment.json"),
    )
    .unwrap();
    let token = json!({"token":"a".repeat(64),"token_generation":2});
    store.write(Slot::Token, token.clone()).await.unwrap();
    let reads = async {
        for _ in 0..64 {
            assert_eq!(store.read(Slot::Token).await.unwrap(), Some(token.clone()));
        }
    };
    let writes = async {
        for _ in 0..64 {
            store.write(Slot::Token, token.clone()).await.unwrap();
        }
    };
    tokio::join!(reads, writes);
    store.write(Slot::Token, token.clone()).await.unwrap();
    let leftovers = std::fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect::<Vec<_>>();
    assert_eq!(leftovers, Vec::<String>::new());
}

fn vault_dir() -> Scratch {
    let scratch =
        Scratch(std::env::temp_dir().join(format!("vault-authority-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&scratch.0).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&scratch.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    scratch
}

fn write_private(path: &std::path::Path, contents: &str) {
    std::fs::write(path, contents).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn loaded_host_refuses_imported_vault_markers_without_touching_existing_pi_login() {
    if std::env::var_os("RPI_TEST_HOST").is_none()
        || std::env::var_os("RPI_TEST_ANTIGRAVITY_LIBRARY").is_none()
    {        eprintln!(
            "SKIP: set RPI_TEST_HOST and RPI_TEST_ANTIGRAVITY_LIBRARY for loaded I02 acceptance"
        );
        return;
    }
    let output = Command::new("python3")
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/native-import-authority.py"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("\"loadedMaliciousMarkers\": 15"));
}

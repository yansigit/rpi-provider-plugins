//! I02 real FileStore/Source boundary, plus the optional loaded-host fixture.
use rpi_antigravity_auth::vault::{import_guard::FRESH_ENROLLMENT_NOTICE, source::Source};
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn raw_enrollment_provenance_is_refused_before_consumer_decoding() {
    let scratch =
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

#[test]
fn loaded_host_refuses_imported_vault_markers_without_touching_existing_pi_login() {
    if std::env::var_os("RPI_TEST_HOST").is_none()
        || std::env::var_os("RPI_TEST_ANTIGRAVITY_LIBRARY").is_none()
    {
        eprintln!(
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

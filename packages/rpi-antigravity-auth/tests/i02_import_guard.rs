//! I02: the Antigravity vault half. Custody classifications come from the
//! unchanged pinned Pi slot implementation; the committed fixture records them
//! and the import-origin decisions. Every admission object is compared whole,
//! and the cash-free layouts are reproduced through this crate's own slot port.

use rpi_antigravity_auth::vault::import_guard::{self, Admission, Custody, Files};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

fn fixture() -> Value {
    serde_json::from_str(include_str!("i02-import-guard.json")).unwrap()
}

fn files(value: &Value) -> Files {
    let get = |key: &str| value.get(key).cloned().filter(|value| !value.is_null());
    Files {
        mode: get("mode"),
        enrollment_state: get("enrollment_state"),
        enrollment_token: get("enrollment_token"),
        stash: get("stash"),
        artifacts: value
            .get("artifacts")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .map(|name| name.as_str().unwrap().to_owned())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn custody(value: &Value) -> Custody {
    Custody::parse(
        value["slot"].as_str().unwrap(),
        value["stash"].as_str().unwrap(),
    )
    .unwrap()
}

#[test]
fn every_admission_matches_the_reference_case() {
    let fixture = fixture();
    let cases = fixture["markerCases"].as_array().unwrap();
    let mut refused = 0;
    for case in cases {
        let admission = import_guard::admit(&files(&case["files"]), &custody(&case["custody"]));
        assert_eq!(admission.to_json(), case["result"], "{}", case["id"]);
        if !admission.to_json()["allowed"].as_bool().unwrap() {
            refused += 1;
        }
    }
    assert_eq!(fixture["counts"]["markerCases"], json!(cases.len()));
    assert_eq!(fixture["counts"]["custodyCases"], json!(6));
    assert_eq!(
        fixture["reference"],
        "a97f9055017f3c1546afbfe61a19d50b428be3d8"
    );
    assert_eq!(fixture["pi"], "1.3.0");
    assert_eq!(fixture["upstreamHashes"].as_object().unwrap().len(), 3);
    assert_eq!(refused, 19);
}

#[test]
fn imported_and_foreign_vault_state_requires_fresh_enrollment() {
    let fixture = fixture();
    for case in fixture["markerCases"].as_array().unwrap() {
        let admission = import_guard::admit(&files(&case["files"]), &custody(&case["custody"]));
        let serialized = admission.to_json();
        let code = serialized["code"].as_str().unwrap_or_default();
        let imported = matches!(
            code,
            "imported-custody"
                | "imported-location"
                | "active-backup"
                | "forged-custody-origin"
                | "unsafe-source-path"
                | "invalid-import-marker"
                | "unsupported-imported-version"
        );
        if imported {
            assert!(!serialized["allowed"].as_bool().unwrap(), "{}", case["id"]);
            assert!(
                serialized["freshEnrollment"].as_bool().unwrap(),
                "{}",
                case["id"]
            );
            assert_eq!(
                serialized["notice"],
                json!(import_guard::FRESH_ENROLLMENT_NOTICE),
                "{}",
                case["id"]
            );
        }
        if let Admission::Refuse(refusal) = admission {
            assert!(!refusal.code.is_empty(), "{}", case["id"]);
            assert!(!refusal.notice.is_empty(), "{}", case["id"]);
        }
    }
}

// ---- slot layouts reproduced through this crate's port --------------------
fn scratch(name: &str) -> PathBuf {
    let home = std::env::var_os("HOME").expect("HOME");
    let directory = PathBuf::from(home).join(format!(
        ".rpi-i02-{}-{}-{name}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    directory
}

fn write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

fn slot_for(directory: &Path) -> (PathBuf, PathBuf) {
    (
        directory.join("auth.json"),
        directory.join("google-login-stash.json"),
    )
}

#[test]
fn custody_layouts_match_the_pinned_slot_implementation() {
    let fixture = fixture();
    for case in fixture["custodyCases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let directory = scratch(id);
        let (auth, stash) = slot_for(&directory);
        let login = br#"{"google":{"type":"api_key","key":"real-login-key"}}"#;
        let placeholder = br#"{"google":{"type":"api_key","key":"antigravity-vault-custody"}}"#;
        match id {
            "empty" => {}
            "login" | "entered" => write(&auth, login),
            "placeholder" => write(&auth, placeholder),
            "lookalike" => write(
                &auth,
                br#"{"google":{"type":"api_key","key":"antigravity-vault-custody-extra"}}"#,
            ),
            "foreign-stash" => {
                let source = scratch("foreign-source");
                let (source_auth, source_stash) = slot_for(&source);
                write(&source_auth, login);
                rpi_antigravity_auth::vault::slot::Slot::new(
                    &source_auth,
                    &source_stash,
                    "google",
                    "antigravity-vault-custody",
                )
                .unwrap()
                .enter()
                .unwrap();
                fs::copy(&source_auth, &auth).unwrap();
                fs::copy(&source_stash, &stash).unwrap();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
                    fs::set_permissions(&stash, fs::Permissions::from_mode(0o600)).unwrap();
                }
                let raw = rpi_antigravity_auth::vault::slot::Slot::new(
                    &auth,
                    &stash,
                    "google",
                    "antigravity-vault-custody",
                )
                .unwrap()
                .inspect();
                assert_eq!(
                    raw.err().map(|error| error.to_string()).as_deref(),
                    Some("Pi slot: invalid-stash"),
                    "foreign stash must be rejected by the slot port itself"
                );
                fs::remove_dir_all(&source).unwrap();
            }
            other => panic!("unknown layout {other}"),
        }
        if id == "entered" {
            rpi_antigravity_auth::vault::slot::Slot::new(
                &auth,
                &stash,
                "google",
                "antigravity-vault-custody",
            )
            .unwrap()
            .enter()
            .unwrap();
        }
        let inspected = import_guard::inspect(&auth, &stash, "google", "antigravity-vault-custody")
            .unwrap()
            .to_json();
        assert_eq!(inspected["slot"], case["upstream"]["slot"], "{id}");
        assert_eq!(inspected["stash"], case["upstream"]["stash"], "{id}");
        fs::remove_dir_all(&directory).unwrap();
    }
}

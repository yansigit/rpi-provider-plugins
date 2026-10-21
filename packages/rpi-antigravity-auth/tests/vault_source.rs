#![cfg(unix)]
use rpi_antigravity_auth::vault::source::{read_mode, Source};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn write(path: &Path, spec: &Value) {
    if path.is_dir() {
        fs::remove_dir_all(path).unwrap();
    } else {
        let _ = fs::remove_file(path);
    }
    if spec["directory"] == true {
        fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    } else if spec.get("value").is_some() || spec.get("raw").is_some() {
        let text = spec["raw"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| spec["value"].to_string());
        fs::write(path, text).unwrap();
        fs::set_permissions(
            path,
            fs::Permissions::from_mode(spec["mode"].as_u64().unwrap_or(0o600) as u32),
        )
        .unwrap();
    }
}
#[tokio::test]
async fn source_admission_uses_enrollment_authority_and_original_settings_rules() {
    let fixtures: Value = serde_json::from_str(include_str!("fixtures/vault-source.json")).unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("rpi-source-{}", uuid::Uuid::new_v4())));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch.0)
        .unwrap();
    let source = Source::new(&scratch.0).unwrap();
    for (i, case) in fixtures["cases"].as_array().unwrap().iter().enumerate() {
        let spec = &case["spec"];
        for (path, value) in [
            (&source.mode_file, &spec["mode"]),
            (&source.store.state, &spec["state"]),
            (&source.store.token, &spec["token"]),
        ] {
            write(path, value);
        }
        assert_eq!(
            read_mode(&source.mode_file),
            case["modeRead"],
            "mode {i}: {spec}"
        );
        assert_eq!(source.status().await, case["status"], "status {i}: {spec}");
        let mode = match source.request_mode().await {
            Ok(value) => json!({"value":value}),
            Err(error) => json!({"error":error}),
        };
        assert_eq!(mode, case["requestMode"], "request {i}: {spec}");
    }
}

#[tokio::test]
async fn slot_synchronization_matches_original_factory_admission_and_bytes() {
    use rpi_antigravity_auth::vault::{slot::Slot, source::MARKER};
    use sha2::{Digest, Sha256};
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-startup.json")).unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("rpi-vault-startup-{}", uuid::Uuid::new_v4())));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&scratch.0)
        .unwrap();
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let dir = scratch.0.join(i.to_string());
        let vault = dir.join("vault");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&vault)
            .unwrap();
        let source = Source::new(&vault).unwrap();
        let spec = &case["spec"];
        for (path, key) in [
            (&source.mode_file, "mode"),
            (&source.store.state, "state"),
            (&source.store.token, "token"),
        ] {
            write(
                path,
                &spec
                    .get(key)
                    .map(|value| json!({"value":value}))
                    .unwrap_or(json!({})),
            );
        }
        let auth = dir.join("auth.json");
        let stash = vault.join("google-login-stash.json");
        write(&auth, &json!({"raw":spec["auth"]}));
        let slot = Slot::new(&auth, &stash, "google", MARKER).unwrap();
        let paths = [
            ("state", source.store.state.clone()),
            ("token", source.store.token.clone()),
            ("mode", source.mode_file.clone()),
        ];
        let result = source
            .synchronize(|mode| async move {
                let result = if mode["mode"] == "custody" {
                    slot.enter().map(|()| Value::Null)
                } else {
                    slot.exit().map(|result| json!(result))
                }
                .map_err(|error| error.to_string())?;
                for (key, path) in paths {
                    if let Some(value) = spec["afterSwap"].get(key) {
                        write(
                            &path,
                            &if value.is_null() {
                                json!({})
                            } else {
                                json!({"value":value})
                            },
                        );
                    }
                }
                Ok(result)
            })
            .await;
        if case["configuration"]["refusal"].is_null() {
            let mode = result.unwrap();
            assert_eq!(
                mode["mode"] == "local",
                case["configuration"]["oauth"],
                "configuration {i}"
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(format!("{error} Resolve it from /antigravity; Antigravity requests are refused until then. No request was sent."),case["configuration"]["refusal"],"refusal {i}");
        }
        let hash: String = Sha256::digest(json!(auth).to_string().as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        for (path, key) in [(&auth, "auth"), (&stash, "stash")] {
            let actual = fs::read_to_string(path)
                .map(|text| json!(text.replace(&hash, "<auth-identity>")))
                .unwrap_or(Value::Null);
            assert_eq!(actual, case[key], "{key} case {i}");
        }
    }
}

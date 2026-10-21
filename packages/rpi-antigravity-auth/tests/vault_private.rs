#![cfg(unix)]
use rpi_antigravity_auth::vault::{
    ceremony::Failure,
    enrollment,
    policy::ConsumerError,
    private::posix::{self, Metadata, ParentDecision},
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::PathBuf,
};
fn metadata(v: &Value) -> Metadata {
    Metadata {
        mode: v["mode"].as_u64().unwrap() as u32,
        uid: v["uid"].as_u64().unwrap() as u32,
        gid: v["gid"].as_u64().unwrap() as u32,
        directory: v["directory"].as_bool().unwrap(),
    }
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn scratch() -> Scratch {
    let path = std::env::temp_dir().join(format!("rpi-vault-private-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    Scratch(path)
}
#[test]
fn parent_policy_matches_original_owner_group_and_sticky_checks() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-private.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let passwd: Vec<u8> =
            serde_json::from_value(case["accountFiles"]["passwd"].clone()).unwrap();
        let group: Vec<u8> = serde_json::from_value(case["accountFiles"]["group"].clone()).unwrap();
        let mut child = None;
        let mut result = json!({"ok":true});
        for (index, pair) in case["chain"].as_array().unwrap().iter().enumerate() {
            let entries = [metadata(&pair["lstat"]), metadata(&pair["stat"])];
            for entry in entries {
                let reason = match posix::ancestor(entry, child.as_ref(), 1000, Some(1000)) {
                    ParentDecision::Allowed => None,
                    ParentDecision::Refused(reason) => Some(reason),
                    ParentDecision::PrivateGroup => {
                        posix::private_group_reason(&passwd, &group, 1000, 1000, false)
                    }
                };
                if let Some(reason) = reason {
                    result = json!({"ok":false,"reason":reason,"path":(["/fixture/child","/fixture","/"][index])});
                    break;
                }
            }
            if result["ok"] == false {
                break;
            }
            child = Some(entries);
        }
        assert_eq!(result, case["result"], "{case}");
    }
}
#[test]
fn secret_file_permissions_symlinks_bounds_and_redaction_match_original() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-private.json")).unwrap();
    let scratch = scratch();
    let path = scratch.0.join("token.json");
    let target = scratch.0.join("target.json");
    for case in fixture["files"].as_array().unwrap() {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(&path);
        let spec = &case["spec"];
        if spec["directory"] == true {
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        } else if spec["symlink"] == true {
            fs::write(&target, "{}").unwrap();
            symlink(&target, &path).unwrap();
        } else if spec["missing"] != true {
            fs::write(
                &path,
                spec["raw"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| spec["value"].to_string()),
            )
            .unwrap();
            fs::set_permissions(
                &path,
                fs::Permissions::from_mode(spec["mode"].as_u64().unwrap() as u32),
            )
            .unwrap();
        }
        let result = posix::read_bounded_json(&path)
            .and_then(|v| {
                v.ok_or_else(|| {
                    Failure::from(ConsumerError::new(
                        "not-enrolled",
                        "Claustrum enrollment is not configured",
                    ))
                })
            })
            .and_then(|v| enrollment::decode_token(&v).map_err(Failure::from));
        let result = match result {
            Ok(value) => json!({"value":value}),
            Err(Failure::Consumer(error)) => {
                json!({"error":{"name":"ClaustrumConsumerError","kind":error.kind,"message":error.message}})
            }
            Err(error) => panic!("unexpected private file result: {error}"),
        };
        assert_eq!(result, case["result"], "{}", spec["name"]);
        assert!(!result.to_string().contains("secret-do-not-echo"));
    }
}
#[test]
fn atomic_state_write_is_private_and_failure_keeps_previous_state() {
    let scratch = scratch();
    let folder = scratch.0.join("state");
    fs::DirBuilder::new().mode(0o755).create(&folder).unwrap();
    let path = folder.join("state.json");
    posix::write_state(&path, &json!({"version":1,"phase":"pending"})).unwrap();
    assert_eq!(
        fs::metadata(&folder).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let old = fs::read(&path).unwrap();
    assert!(posix::write_state(&path, &json!({"secret":"x".repeat(17000)})).is_err());
    assert_eq!(fs::read(&path).unwrap(), old);
    let occupied = folder.join("directory.json");
    fs::create_dir(&occupied).unwrap();
    assert!(posix::write_state(&occupied, &json!({"secret":"fixture"})).is_err());
    assert_eq!(
        fs::read_dir(&folder).unwrap().count(),
        2,
        "failed publication leaves no staged secret"
    );
    let shared = scratch.0.join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(posix::write_state(&shared.join("nested/state.json"), &json!({})).is_err());
}
#[test]
fn token_publication_matches_original_validation_and_filesystem_policy() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-token-file.json")).unwrap();
    let temp_pattern = regex::Regex::new(r"\.token\.json\.\d+\.[a-f0-9]{24}\.tmp").unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let scratch = scratch();
        let parent = scratch.0.join("parent");
        let spec = &case["spec"];
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(
            &parent,
            fs::Permissions::from_mode(spec["parentMode"].as_u64().unwrap() as u32),
        )
        .unwrap();
        let mut path = parent.join("token.json");
        match spec["target"].as_str() {
            Some("missing-parents") => path = parent.join("nested/deep/token.json"),
            Some("directory") => fs::create_dir(&path).unwrap(),
            Some("symlink") => {
                fs::write(scratch.0.join("untouched"), "original").unwrap();
                symlink(scratch.0.join("untouched"), &path).unwrap();
            }
            Some("dangling-parent") => {
                symlink(scratch.0.join("absent"), parent.join("link")).unwrap();
                path = parent.join("link/token.json");
            }
            Some("file-parent") => {
                fs::write(parent.join("file"), "original").unwrap();
                path = parent.join("file/token.json");
            }
            _ => {}
        }
        let mut result = match posix::write_token(&path, &spec["value"]) {
            Ok(()) => {
                json!({"ok":true,"bytes":fs::read_to_string(&path).unwrap(),"mode":fs::metadata(&path).unwrap().permissions().mode()&0o777})
            }
            Err(error) => {
                let message = error
                    .to_string()
                    .replace(scratch.0.to_str().unwrap(), "<DIR>");
                let mut value = json!({"name":"Error","message":temp_pattern.replace_all(&message,".token.json.<PID>.<RANDOM>.tmp")});
                if let Some(code) = error.io_code() {
                    value["code"] = json!(code);
                }
                json!({"error":value})
            }
        };
        if spec["target"] == "symlink" {
            result["untouched"] = json!(fs::read_to_string(scratch.0.join("untouched")).unwrap());
        }
        result["parentMode"] = json!(fs::metadata(&parent).unwrap().permissions().mode() & 0o777);
        let mut entries: Vec<_> = fs::read_dir(&parent)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        result["entries"] = json!(entries);
        assert_eq!(result, case["result"], "{spec}");
    }
}

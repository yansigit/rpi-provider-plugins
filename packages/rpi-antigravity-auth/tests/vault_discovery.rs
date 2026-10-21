#![cfg(unix)]
use rpi_antigravity_auth::vault::discovery;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn connection_discovery_and_user_tokens_match_the_original() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/vault-discovery.json")).unwrap();
    let scratch =
        Scratch(std::env::temp_dir().join(format!("rpi-discovery-{}", uuid::Uuid::new_v4())));
    let root = scratch.0.to_str().unwrap();
    let expand = |v: &str| v.replace("$ROOT", root);
    for (i, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let _ = fs::remove_dir_all(scratch.0.join("files"));
        fs::create_dir_all(scratch.0.join("files/tmp")).unwrap();
        let spec = &case["spec"];
        let env: HashMap<_, _> = spec["env"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), expand(v.as_str().unwrap())))
            .collect();
        for file in spec["files"].as_array().unwrap() {
            let path = PathBuf::from(expand(file["path"].as_str().unwrap()));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if file["directory"] == true {
                fs::create_dir(&path).unwrap();
            } else if let Some(link) = file["link"].as_str() {
                std::os::unix::fs::symlink(expand(link), &path).unwrap();
            } else {
                fs::write(path, b"{}").unwrap();
            }
        }
        let result = discovery::resolve(
            spec["explicit"].as_str(),
            &env,
            &scratch.0.join("files/tmp"),
            case["uid"].as_u64().map(|v| v as u32),
            || Ok(Path::new("$OS_HOME").into()),
        )
        .unwrap();
        assert_eq!(
            result.to_str().unwrap().replace(root, "$ROOT"),
            case["result"].as_str().unwrap(),
            "discovery {i}"
        );
    }
    for case in fixture["tokens"].as_array().unwrap() {
        let env: HashMap<String, String> = serde_json::from_value(case["env"].clone()).unwrap();
        assert_eq!(
            discovery::user_token(case["uid"].as_u64().map(|v| v as u32), &env),
            case["result"].as_str().unwrap()
        );
    }
    assert!(discovery::os_home().unwrap().is_absolute());
}

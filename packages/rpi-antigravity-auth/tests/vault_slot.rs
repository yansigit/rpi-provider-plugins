#![cfg(unix)]
use rpi_antigravity_auth::vault::slot::{Error, Slot, Step};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn snapshot(path: &Path, hash: &str) -> Value {
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
fn private_write(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
#[test]
fn credential_slot_matches_original_bytes_refusals_and_crash_recovery() {
    let fixture: Value = serde_json::from_str(include_str!("fixtures/vault-slot.json")).unwrap();
    let root = std::env::temp_dir().join(format!("rpi-slot-{}", uuid::Uuid::new_v4()));
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let scratch = Scratch(root);
    for (index, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let spec = &case["spec"];
        let dir = scratch.0.join(index.to_string());
        fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        let auth = dir.join("auth.json");
        let stash_dir = dir.join("vault");
        let stash = stash_dir.join("stash.json");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&stash_dir)
            .unwrap();
        if let Some(raw) = spec["raw"].as_str() {
            private_write(&auth, raw.as_bytes());
        }
        let hash: String = Sha256::digest(json!(auth).to_string().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let hook = spec["hook"].as_str().unwrap_or("").to_owned();
        let hook_at = spec["hookAt"].as_u64().unwrap_or(0) as usize;
        let events = Arc::new(Mutex::new(Vec::<String>::new()));
        let events_hook = events.clone();
        let fired = AtomicBool::new(false);
        let at = Arc::new(AtomicUsize::new(0));
        let at_hook = at.clone();
        let auth_hook = auth.clone();
        let slot = Slot::new(&auth, &stash, "google", "antigravity-vault-custody")
            .unwrap()
            .with_hook(Arc::new(move |step, path| {
                let target = if path == auth_hook { "auth" } else { "stash" };
                let name = match step {
                    Step::StashWritten => "stash-written",
                    Step::AuthWritten => "auth-written",
                    Step::StashDeleted => "stash-deleted",
                    _ => "",
                };
                if !name.is_empty() {
                    events_hook.lock().unwrap().push(name.into());
                }
                if fired.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if (!name.is_empty() && name == hook)
                    || (step == Step::BeforeRename && hook == format!("before-rename-{target}"))
                {
                    fired.store(true, Ordering::SeqCst);
                    return Err(Error::Slot("io"));
                }
                if step == Step::AfterRead && at_hook.load(Ordering::SeqCst) == hook_at {
                    if hook == format!("unlink-{target}") {
                        fired.store(true, Ordering::SeqCst);
                        fs::remove_file(path).unwrap();
                    } else if hook == format!("replace-{target}") {
                        fired.store(true, Ordering::SeqCst);
                        fs::rename(
                            path,
                            path.with_file_name(format!(
                                "{}.old",
                                path.file_name().unwrap().to_string_lossy()
                            )),
                        )
                        .unwrap();
                        private_write(path, b"{}");
                    }
                }
                Ok(())
            }));
        for (i, action) in spec["steps"].as_array().unwrap().iter().enumerate() {
            at.store(i, Ordering::SeqCst);
            let op = action["op"].as_str().unwrap();
            let path = match action["target"].as_str() {
                Some("stash") => &stash,
                Some("stashDir") => &stash_dir,
                _ => &auth,
            };
            let value: Result<Option<Value>, Error> = match op {
                "inspect" => slot.inspect().map(Some),
                "enter" => slot.enter().map(|()| Some(Value::Null)),
                "exit" => slot.exit().map(|value| Some(json!(value))),
                _ => {
                    if op == "chmod" {
                        fs::set_permissions(
                            path,
                            fs::Permissions::from_mode(action["mode"].as_u64().unwrap() as u32),
                        )
                        .unwrap();
                    } else if op == "write" {
                        fs::write(path, action["raw"].as_str().unwrap()).unwrap();
                    } else if op == "patch" {
                        let mut saved: Value =
                            serde_json::from_slice(&fs::read(&stash).unwrap()).unwrap();
                        saved[action["field"].as_str().unwrap()] = action["value"].clone();
                        fs::write(&stash, saved.to_string()).unwrap();
                    } else {
                        if path.is_dir() {
                            fs::remove_dir_all(path).unwrap();
                        } else {
                            let _ = fs::remove_file(path);
                        }
                        match op {
                            "directory" => fs::DirBuilder::new().mode(0o700).create(path).unwrap(),
                            "symlink" => {
                                let target = path.with_file_name(format!(
                                    "{}.target",
                                    path.file_name().unwrap().to_string_lossy()
                                ));
                                private_write(&target, b"{}");
                                symlink(target, path).unwrap();
                            }
                            "invalid-utf8" => private_write(path, &[255]),
                            "remove" => {}
                            _ => panic!("unknown fixture operation {op}"),
                        }
                    }
                    Ok(None)
                }
            };
            let outcome = match value {
                Ok(Some(value)) => json!({"value":value}),
                Ok(None) => json!({}),
                Err(Error::Slot(code)) => {
                    json!({"error":{"name":"PiSlotError","message":format!("Pi slot: {code}"),"code":code}})
                }
                Err(error) => panic!("{error}"),
            };
            let actual = json!({"outcome":outcome,"events":*events.lock().unwrap(),"auth":snapshot(&auth,&hash),"stash":snapshot(&stash,&hash)});
            assert_eq!(
                actual, case["results"][i],
                "case {index} {} action {i}: {action}",
                spec["name"]
            );
        }
    }
}

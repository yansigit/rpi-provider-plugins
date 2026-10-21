//! Blocking Windows filesystem operations. Callers keep these off the runtime reader.
use super::{perform, perform_legacy, Kind, Operation, PrivateError, Publication};
use crate::vault::{ceremony::Failure, policy::ConsumerError};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
fn private_io(path: &Path, stage: &'static str, error: std::io::Error) -> PrivateError {
    let Failure::Io(error) = super::io_error(error, stage, Some(path)) else {
        unreachable!()
    };
    PrivateError {
        path: path.into(),
        stage,
        code: error.code.clone(),
        reason: error.code,
        native_message: Some(error.message),
        win32_code: None,
        level: None,
        publication: None,
    }
}
fn run(
    path: &Path,
    op: Operation,
    kind: Kind,
    bytes: &[u8],
    stage: &'static str,
) -> Result<Vec<u8>, Failure> {
    perform(path, op, kind, bytes, stage).map_err(Failure::Private)
}
pub fn safe_parent(path: &Path) -> Result<(), Failure> {
    match perform(
        path,
        Operation::SafeParent,
        Kind::Directory,
        &[],
        "safe-parent",
    ) {
        Ok(_) => Ok(()),
        Err(mut e) => {
            if matches!(e.reason.as_str(), "io-error" | "wrong-kind") {
                fs::symlink_metadata(path)
                    .map_err(|e| Failure::Private(private_io(path, "safe-parent", e)))?;
            }
            if let Some(level) = e.level.take() {
                e.path = level;
            }
            // The public result API strips native I/O codes before constructing this error.
            e.code = "ERR_PRIVATE_FILES_SECURITY".into();
            e.win32_code = None;
            Err(Failure::Private(e))
        }
    }
}
pub fn ensure_directory(path: &Path) -> Result<(), Failure> {
    match fs::symlink_metadata(path) {
        Ok(_) => return safe_parent(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(Failure::Private(private_io(path, "directory", e))),
    }
    let parent = path.parent().filter(|p| *p != path).ok_or_else(|| {
        Failure::Private(private_io(
            path,
            "directory",
            std::io::Error::from_raw_os_error(3),
        ))
    })?;
    ensure_directory(parent)?;
    match run(path, Operation::Create, Kind::Directory, &[], "directory") {
        Err(e) if e.io_code() == Some("EEXIST") => {}
        result => {
            result?;
        }
    }
    safe_parent(path)
}
pub fn verify_file(path: &Path, migrate: bool) -> Result<(), Failure> {
    if let Err(mut e) = perform(path, Operation::Verify, Kind::Auto, &[], "verify") {
        if e.reason == "io-error" {
            run(path, Operation::Verify, Kind::File, &[], "verify")?;
        }
        if migrate
            && matches!(
                e.reason.as_str(),
                "extra-ace" | "not-protected" | "null-dacl" | "empty-dacl"
            )
        {
            safe_parent(path.parent().unwrap())?;
            run(path, Operation::Repair, Kind::File, &[], "verify")?;
            run(path, Operation::Verify, Kind::File, &[], "verify")?;
            static NOTICES: OnceLock<Mutex<HashSet<std::path::PathBuf>>> = OnceLock::new();
            if NOTICES
                .get_or_init(Mutex::default)
                .lock()
                .unwrap()
                .insert(path.into())
            {
                eprintln!("PrivateFileMigration: Private permissions tightened: {}. This object may have been readable before; rotate credentials if needed.",path.display());
            }
            return Ok(());
        }
        e.win32_code = None;
        e.code = "ERR_PRIVATE_FILES_SECURITY".into();
        return Err(Failure::Private(e));
    }
    run(path, Operation::Verify, Kind::File, &[], "verify").map(|_| ())
}
pub fn read(path: &Path, migrate: bool) -> Result<Vec<u8>, Failure> {
    if migrate {
        safe_parent(path.parent().unwrap())?;
        verify_file(path, true)?;
    }
    run(path, Operation::Read, Kind::File, &[], "read")
}
pub fn create(path: &Path, bytes: &[u8], directory: bool) -> Result<(), Failure> {
    safe_parent(path.parent().unwrap())?;
    run(
        path,
        Operation::Create,
        if directory {
            Kind::Directory
        } else {
            Kind::File
        },
        bytes,
        if directory {
            "directory-create"
        } else {
            "create"
        },
    )
    .map(|_| ())
}
pub fn read_bounded_json(path: &Path) -> Result<Option<Value>, Failure> {
    let result = (|| {
        ensure_directory(path.parent().unwrap())?;
        read(path, true)
    })();
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(e) if e.io_code() == Some("ENOENT") => return Ok(None),
        Err(e) => return Err(e),
    };
    let text = String::from_utf8_lossy(&bytes);
    if text.len() > 16384 {
        return Err(
            ConsumerError::new("unsafe-file", "Claustrum enrollment file is too large").into(),
        );
    }
    serde_json::from_str(&text).map(Some).map_err(|_| {
        ConsumerError::new(
            "invalid-state",
            "Claustrum enrollment file is not valid JSON",
        )
        .into()
    })
}
pub fn write_atomic(
    path: &Path,
    bytes: &[u8],
    deadline: Option<Instant>,
    before_rename: impl Fn() -> Result<(), Failure>,
) -> Result<(), Failure> {
    ensure_directory(path.parent().unwrap())?;
    match verify_file(path, true) {
        Err(e) if e.io_code() == Some("ENOENT") => {}
        result => result?,
    }
    let stage = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        uuid::Uuid::new_v4()
    ));
    let mut created = false;
    let mut outcome = Publication::default();
    let start = Instant::now();
    let result = (|| {
        create(&stage, bytes, false)?;
        created = true;
        let mut retry_deadline = None;
        let mut original = None;
        for delay in [0, 10, 20, 40, 80, 100] {
            let end = retry_deadline.into_iter().chain(deadline).min();
            if end.is_some_and(|end| Instant::now() >= end) {
                break;
            }
            if delay != 0 {
                std::thread::sleep(
                    end.map(|end| end.saturating_duration_since(Instant::now()))
                        .unwrap_or(Duration::MAX)
                        .min(Duration::from_millis(delay)),
                );
            }
            if end.is_some_and(|end| Instant::now() >= end) {
                break;
            }
            let fence = Instant::now();
            before_rename()?;
            if let Some(end) = &mut retry_deadline {
                *end += fence.elapsed();
            }
            if deadline.is_some_and(|end| Instant::now() >= end) {
                break;
            }
            outcome.attempts += 1;
            match fs::rename(&stage, path) {
                Ok(()) => {
                    created = false;
                    outcome.published = true;
                    outcome.file_durable = true;
                    outcome.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
                    verify_file(path, false)?;
                    return Ok(());
                }
                Err(e) => {
                    let error = private_io(path, "rename", e);
                    let retry = matches!(error.code.as_str(), "EBUSY" | "EPERM" | "EACCES");
                    original.get_or_insert(error);
                    if !retry {
                        break;
                    }
                    retry_deadline
                        .get_or_insert_with(|| Instant::now() + Duration::from_millis(500));
                }
            }
        }
        let error = original.unwrap_or_else(|| PrivateError {
            path: path.into(),
            stage: "rename",
            code: "ETIMEDOUT".into(),
            reason: "ETIMEDOUT".into(),
            native_message: None,
            win32_code: None,
            level: None,
            publication: None,
        });
        Err(Failure::Private(error))
    })();
    if let Err(error) = result {
        let mut error = match error {
            Failure::Private(e) => e,
            other => PrivateError {
                path: path.into(),
                stage: "before-rename",
                code: other.io_code().unwrap_or("ERR_PRIVATE_FILE").into(),
                reason: other.io_code().unwrap_or("io-error").into(),
                native_message: Some(other.to_string()),
                win32_code: None,
                level: None,
                publication: None,
            },
        };
        if !outcome.published {
            outcome.elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        }
        if created && fs::remove_file(&stage).is_err() {
            outcome.orphan = Some(stage);
        }
        error.publication = Some(outcome);
        return Err(Failure::Private(error));
    }
    Ok(())
}
pub fn write_state(path: &Path, value: &Value) -> Result<(), Failure> {
    ensure_directory(path.parent().unwrap())?;
    if let Err(e) = safe_parent(path.parent().unwrap()) {
        if let Failure::Private(e) = e {
            return Err(ConsumerError::new("unsafe-file",format!("Claustrum enrollment parent refused: {} ({}). Use a folder that other users cannot write or replace.",e.path.display(),e.reason)).into());
        }
        return Err(e);
    }
    let bytes = format!("{}\n", crate::json::describe(value));
    if bytes.len() > 16384 {
        return Err(
            ConsumerError::new("invalid-state", "Claustrum enrollment state is too large").into(),
        );
    }
    write_atomic(path, bytes.as_bytes(), None, || Ok(()))
}
fn token_parent(path: &Path) -> Result<(), Failure> {
    let mut current = path;
    loop {
        match fs::symlink_metadata(current){
   Ok(meta)=>return perform_legacy(current,Operation::Verify,if meta.is_dir(){Kind::Directory}else{Kind::File},&[],"verify").map(|_|()).map_err(|e|Failure::Storage(format!("enrollment token folder {} is not private: Windows private-path verification refused: {}",current.display(),e.native_message.as_deref().unwrap_or("Native security verification failed")))),
   Err(e) if e.kind()==std::io::ErrorKind::NotFound=>current=current.parent().ok_or_else(||super::io_error(e,"lstat",Some(current)))?,
   Err(e)=>return Err(Failure::Storage(format!("enrollment token folder {} is not private: Windows private-path verification refused: {}",current.display(),super::io_error(e,"lstat",Some(current))))),
  }
    }
}
fn token_parents(path: &Path) -> Result<(), Failure> {
    match fs::metadata(path) {
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(super::io_error(e, "stat", Some(path))),
    }
    if let Some(parent) = path.parent() {
        token_parents(parent)?;
    }
    match perform_legacy(path, Operation::Create, Kind::Directory, &[], "create") {
        Ok(_) => Ok(()),
        Err(e) if e.code == "EEXIST" && fs::metadata(path).is_ok_and(|m| m.is_dir()) => Ok(()),
        Err(e) => Err(Failure::Private(e)),
    }
}
pub fn write_token(path: &Path, value: &Value) -> Result<(), Failure> {
    super::validate_token(value)?;
    let parent = path.parent().unwrap();
    token_parents(parent)?;
    token_parent(parent)?;
    let mut random = [0u8; 12];
    getrandom::getrandom(&mut random).map_err(|e| Failure::Storage(e.to_string()))?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let temporary = parent.join(format!(
        ".{}.{}.{suffix}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id()
    ));
    let mut created = true;
    let result = (|| {
        if let Err(e) = perform_legacy(
            &temporary,
            Operation::Create,
            Kind::File,
            format!("{}\n", crate::json::describe(value)).as_bytes(),
            "create",
        ) {
            if e.code == "EEXIST" {
                created = false;
            }
            return Err(Failure::Private(e));
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| super::io_error(e, "open", Some(&temporary)))?;
        file.sync_all()
            .map_err(|e| super::io_error(e, "fsync", None))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| super::io_error(e, "rename", Some(path)))?;
        created = false;
        Ok(())
    })();
    if created {
        let _ = fs::remove_file(temporary);
    }
    result
}
pub fn remove_error(path: &Path, error: std::io::Error) -> Failure {
    Failure::Private(private_io(path, "remove", error))
}
pub fn release_error(path: &Path, code: &str) -> Failure {
    Failure::Private(PrivateError {
        path: path.into(),
        stage: "release-not-confirmed",
        code: code.into(),
        reason: code.into(),
        native_message: None,
        win32_code: None,
        level: None,
        publication: None,
    })
}

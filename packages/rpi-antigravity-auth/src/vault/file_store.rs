//! Authoritative, awaited enrollment storage; no queued acknowledgements.
#[cfg(unix)]
use super::private::posix as platform;
#[cfg(windows)]
use super::private::windows_store as platform;
use super::{
    ceremony::{Failure, Slot, Store},
    file_lease::{FileLease, Options},
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) fn read_raw_mode(path: &Path) -> Result<Option<Value>, Failure> {
    platform::read_bounded_json(path)
}

#[derive(Clone)]
pub struct FileStore {
    pub state: PathBuf,
    pub token: PathBuf,
}
impl FileStore {
    pub fn new(state: PathBuf, token: PathBuf) -> Result<Self, Failure> {
        let cwd =
            std::env::current_dir().map_err(|e| super::private::io_error(e, "getcwd", None))?;
        Ok(Self {
            state: if state.is_absolute() {
                state
            } else {
                cwd.join(state)
            },
            token: if token.is_absolute() {
                token
            } else {
                cwd.join(token)
            },
        })
    }
    /// Inspect raw records before any consumer decoder drops provenance fields.
    pub fn authority_files(&self) -> Result<super::import_guard::Files, Failure> {
        let directory = self.state.parent().unwrap();
        let artifacts = match fs::read_dir(directory) {
            Ok(entries) => entries
                .map(|entry| entry.map(|entry| entry.path().to_string_lossy().into_owned()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| super::private::io_error(e, "readdir", Some(directory)))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(super::private::io_error(error, "readdir", Some(directory))),
        };
        Ok(super::import_guard::Files {
            // Mode parse/write failures retain the source/setup error contract.
            mode: platform::read_bounded_json(&directory.join("pi-mode.json")).unwrap_or(None),
            enrollment_state: platform::read_bounded_json(&self.state).unwrap_or(None),
            enrollment_token: platform::read_bounded_json(&self.token).unwrap_or(None),
            stash: platform::read_bounded_json(&directory.join("google-login-stash.json"))?,
            artifacts,
        })
    }

    pub fn path(&self, slot: Slot) -> &Path {
        match slot {
            Slot::State => &self.state,
            Slot::Token => &self.token,
        }
    }
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Failure::Storage("Vault filesystem operation failed".into()))?
}
impl Store for FileStore {
    type Lease = FileLease;
    async fn acquire(&self) -> Result<Option<FileLease>, Failure> {
        let parent = self.state.parent().unwrap().to_owned();
        blocking(move || platform::ensure_directory(&parent)).await?;
        FileLease::acquire(Options::new(self.state.clone(), "ceremony", 30_000, true)).await
    }
    async fn read(&self, slot: Slot) -> Result<Option<Value>, Failure> {
        let store = self.clone();
        blocking(move || {
            // Guard the exact value returned, not a preceding read of the slot.
            let value = platform::read_bounded_json(store.path(slot))?;
            let mut files = store.authority_files()?;
            match slot {
                Slot::State => files.enrollment_state = value.clone(),
                Slot::Token => files.enrollment_token = value.clone(),
            }
            // A provenance refusal is not a storage fault: keep it typed so callers
            // do not have to compare notice text.
            super::import_guard::guard_storage(&files).map_err(Failure::Refused)?;
            Ok(value)
        })
        .await
    }
    async fn write(&self, slot: Slot, value: Value) -> Result<(), Failure> {
        let path = self.path(slot).to_owned();
        blocking(move || match slot {
            Slot::State => platform::write_state(&path, &value),
            Slot::Token => platform::write_token(&path, &value),
        })
        .await
    }
    async fn remove(&self, slot: Slot) -> Result<bool, Failure> {
        let path = self.path(slot).to_owned();
        blocking(move || match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(super::private::io_error(e, "unlink", Some(&path))),
        })
        .await
    }
}

//! Serialized roster mutations and discovery leases, with a final publication fence.
use super::{
    ceremony::{Failure as StorageError, Lease},
    custody::Failure,
    file_lease::{FileLease, Options},
    policy::ConsumerError,
    private,
    roster::{self, Projection},
};
use futures_util::future::BoxFuture;
use serde_json::Value;
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub type Fence = Arc<dyn Fn() -> BoxFuture<'static, Result<(), StorageError>> + Send + Sync>;
pub struct Change<T> {
    pub next: Option<Value>,
    pub result: T,
}
#[derive(Clone)]
pub struct RosterStore {
    pub path: PathBuf,
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, StorageError> + Send + 'static,
) -> Result<T, StorageError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| StorageError::Storage("Vault filesystem operation failed".into()))?
}
fn invalid() -> StorageError {
    ConsumerError::new("invalid-state", "Claustrum roster file is not valid").into()
}
fn read(path: &Path) -> Result<Option<Value>, StorageError> {
    #[cfg(unix)]
    let bytes = std::fs::read(path).map_err(|e| private::io_error(e, "open", Some(path)));
    #[cfg(windows)]
    let bytes = private::windows_store::read(path, true);
    let bytes = match bytes {
        Err(e) if e.io_code() == Some("ENOENT") => return Ok(None),
        result => result?,
    };
    let value = serde_json::from_str(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid())?;
    roster::decode(&value).map(Some).map_err(StorageError::from)
}
fn write(
    path: &Path,
    value: &Value,
    before: impl Fn() -> Result<(), StorageError>,
) -> Result<(), StorageError> {
    let bytes = format!("{}\n", crate::json::pretty(value));
    #[cfg(windows)]
    {
        private::windows_store::write_atomic(path, bytes.as_bytes(), None, before)
    }
    #[cfg(unix)]
    {
        use std::{
            fs,
            io::Write,
            os::unix::fs::{OpenOptionsExt, PermissionsExt},
        };
        fs::create_dir_all(path.parent().unwrap())
            .map_err(|e| private::io_error(e, "mkdir", path.parent()))?;
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
        let stage = PathBuf::from(name);
        let mut created = false;
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&stage)
                .map_err(|e| private::io_error(e, "open", Some(&stage)))?;
            created = true;
            file.write_all(bytes.as_bytes())
                .map_err(|e| private::io_error(e, "write", Some(&stage)))?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|e| private::io_error(e, "fchmod", Some(&stage)))?;
            drop(file);
            before()?;
            fs::rename(&stage, path).map_err(|e| private::rename_error(e, &stage, path))?;
            created = false;
            Ok(())
        })();
        if created {
            let _ = fs::remove_file(stage);
        }
        result
    }
}
impl RosterStore {
    pub fn new(path: PathBuf) -> Result<Self, StorageError> {
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .map_err(|e| private::io_error(e, "getcwd", None))?
                .join(path)
        };
        Ok(Self { path })
    }
    pub async fn read(&self) -> Result<Option<Value>, StorageError> {
        let path = self.path.clone();
        blocking(move || read(&path)).await
    }
    async fn write_lock(&self) -> Result<FileLease, StorageError> {
        let started = Instant::now();
        loop {
            if let Some(lease) = FileLease::acquire(Options::new(
                self.path.clone(),
                "claustrum-roster-write",
                10_000,
                false,
            ))
            .await?
            {
                return Ok(lease);
            }
            let remaining = Duration::from_millis(5000).saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(StorageError::LockContention {
                    target: self.path.clone(),
                    name: "claustrum-roster-write".into(),
                    timeout_ms: 5000,
                });
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(25))).await;
        }
    }
    pub async fn mutate<T: Send, E: From<StorageError>>(
        &self,
        change: impl for<'a> FnOnce(Option<Value>, &'a FileLease) -> BoxFuture<'a, Result<Change<T>, E>>,
        before_publish: Option<Fence>,
    ) -> Result<T, E> {
        let lease = Arc::new(self.write_lock().await.map_err(E::from)?);
        let result = async {
            let change = change(self.read().await.map_err(E::from)?, &lease).await?;
            if let Some(next) = change.next {
                lease.assert_owned().await.map_err(E::from)?;
                let path = self.path.clone();
                let fence = lease.clone();
                let runtime = tokio::runtime::Handle::current();
                blocking(move || {
                    write(&path, &next, || {
                        runtime.block_on(async {
                            if let Some(before) = &before_publish {
                                before().await?;
                            }
                            fence.assert_owned().await
                        })
                    })
                })
                .await
                .map_err(E::from)?;
            }
            Ok(change.result)
        }
        .await;
        match Arc::try_unwrap(lease) {
            Ok(lease) => lease.release().await.map_err(E::from)?,
            Err(_) => {
                return Err(E::from(StorageError::Storage(
                    "Roster write still owns its lease".into(),
                )))
            }
        }
        result
    }
    pub async fn change_route(&self, route: String, accept: bool) -> Result<(), StorageError> {
        self.mutate(
            move |current, _| {
                Box::pin(async move {
                    let next =
                        roster::change_route(&current.unwrap_or(Value::Null), &route, accept)?;
                    Ok(Change {
                        next: Some(next),
                        result: (),
                    })
                })
            },
            None,
        )
        .await
    }
    pub async fn refresh<
        D: Future<Output = Result<Value, Failure>> + Send,
        F: Future<Output = Result<bool, Failure>> + Send,
    >(
        &self,
        discover: impl FnOnce(CancellationToken) -> D + Send,
        signal: CancellationToken,
        active: impl Fn() -> F + Send + 'static,
        projection: impl Fn() -> Projection + Send + 'static,
    ) -> Result<Option<Value>, Failure> {
        check(&signal)?;
        if !active().await? {
            return Ok(None);
        }
        let lease = FileLease::acquire(Options::new(
            self.path.clone(),
            "claustrum-roster",
            30_000,
            true,
        ))
        .await?;
        let Some(lease) = lease else {
            let persisted = self.read().await?;
            check(&signal)?;
            if !persisted
                .as_ref()
                .is_some_and(|v| v["view"].as_str().is_some_and(|v| !v.is_empty()))
            {
                return Err(Failure::Consumer(ConsumerError::new(
                    "roster-busy",
                    "Claustrum account discovery is already in progress",
                )));
            }
            return Ok(persisted);
        };
        let lease = Arc::new(lease);
        let result = async {
            let inventory = discover(signal.clone()).await?;
            check(&signal)?;
            let fence = lease.clone();
            self.mutate(
                move |current, _| {
                    Box::pin(async move {
                        if !active().await? {
                            return Ok(Change {
                                next: None,
                                result: None,
                            });
                        }
                        check(&signal)?;
                        fence.assert_owned().await?;
                        let next = roster::project(current.as_ref(), &inventory, &projection());
                        let changed = current.as_ref().map(crate::json::describe).as_deref()
                            != Some(&crate::json::describe(&next));
                        Ok(Change {
                            next: changed.then(|| next.clone()),
                            result: Some(next),
                        })
                    })
                },
                None,
            )
            .await
        }
        .await;
        match Arc::try_unwrap(lease) {
            Ok(lease) => lease.release().await?,
            Err(_) => {
                return Err(
                    StorageError::Storage("Roster discovery still owns its lease".into()).into(),
                )
            }
        }
        result
    }
}
fn check(signal: &CancellationToken) -> Result<(), Failure> {
    if signal.is_cancelled() {
        Err(Failure::Cancelled)
    } else {
        Ok(())
    }
}

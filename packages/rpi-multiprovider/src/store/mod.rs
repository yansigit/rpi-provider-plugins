pub mod port;
mod records;
pub mod schema;

use crate::services::Error;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

pub const FILE_NAME: &str = "multiprovider-auth.json";

// Parsing failures stay structured until the native presentation policy is selected.
pub enum Failure {
    Public(Error),
    Syntax {
        input: String,
        line: usize,
        column: usize,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        error: std::io::Error,
    },
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Public(error)
    }
}
fn io(operation: &'static str, path: &Path, error: std::io::Error) -> Failure {
    Failure::Io {
        operation,
        path: path.into(),
        error,
    }
}
fn cancelled() -> Failure {
    Error::Message("multiprovider: operation cancelled".into()).into()
}

#[derive(Clone)]
pub struct Store {
    path: PathBuf,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    random_id: Arc<dyn Fn() -> String + Send + Sync>,
}
impl Store {
    pub fn new(path: PathBuf) -> Self {
        Self::with_sources(
            path,
            Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as f64
            }),
            Arc::new(|| uuid::Uuid::new_v4().to_string()),
        )
    }
    pub fn with_sources(
        path: PathBuf,
        now: Arc<dyn Fn() -> f64 + Send + Sync>,
        random_id: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        Self {
            path,
            now,
            random_id,
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn request(
        &self,
        request: Value,
        signal: CancellationToken,
    ) -> Result<Value, Failure> {
        let store = self.clone();
        let signal = signal.child_token();
        let _cancel = signal.clone().drop_guard();
        // The owning ProviderContext joins blocking work before unloading this library.
        tokio::task::spawn_blocking(move || store.request_blocking(request, &signal))
            .await
            .map_err(|_| {
                Failure::Public(Error::Message(
                    "multiprovider: account store task stopped".into(),
                ))
            })?
    }
    fn request_blocking(
        &self,
        mut request: Value,
        signal: &CancellationToken,
    ) -> Result<Value, Failure> {
        records::prepare(&mut request)?;
        let writable = records::writable(&request);
        let _lock = if writable {
            Some(self.lock(signal)?)
        } else {
            None
        };
        let mut state = self.read()?;
        let result = records::execute(&mut state, &request, &*self.now, &*self.random_id)?;
        if writable {
            self.write(&state)?;
        }
        Ok(result)
    }
    pub fn read(&self) -> Result<Value, Failure> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(json!({"version":1,"providers":{}}))
            }
            Err(error) => return Err(io("read", &self.path, error)),
        };
        let input = String::from_utf8_lossy(&bytes).into_owned();
        let state = serde_json::from_str(&input).map_err(|error| Failure::Syntax {
            input: input.clone(),
            line: error.line(),
            column: error.column(),
        })?;
        schema::state(&state)?;
        Ok(state)
    }
    fn directory(&self) -> &Path {
        self.path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
    }
    fn create_directory(&self) -> Result<(), Failure> {
        let mut options = fs::DirBuilder::new();
        options.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            options.mode(0o700);
        }
        options
            .create(self.directory())
            .map_err(|error| io("mkdir", self.directory(), error))
    }
    fn write(&self, state: &Value) -> Result<(), Failure> {
        self.create_directory()?;
        let temporary = self.directory().join(format!(
            ".{FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            (self.random_id)()
        ));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(&temporary)
                .map_err(|error| io("open", &temporary, error))?;
            let mut bytes =
                serde_json::to_vec_pretty(&records::ordered(state)).expect("validated JSON state");
            bytes.push(b'\n');
            file.write_all(&bytes)
                .map_err(|error| io("write", &temporary, error))?;
            drop(file);
            fs::rename(&temporary, &self.path).map_err(|error| io("rename", &self.path, error))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
                    .map_err(|error| io("chmod", &self.path, error))?;
            }
            Ok(())
        })();
        let _ = fs::remove_file(&temporary);
        result
    }
    fn lock(&self, signal: &CancellationToken) -> Result<Lock, Failure> {
        self.create_directory()?;
        let started = (self.now)();
        let mut path = self.path.as_os_str().to_owned();
        path.push(".lock");
        let path = PathBuf::from(path);
        loop {
            if signal.is_cancelled() {
                return Err(cancelled());
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Lock {
                        file: Some(file),
                        path,
                    })
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io("open", &path, error)),
            }
            match fs::metadata(&path).and_then(|metadata| metadata.modified()) {
                Ok(modified) => {
                    let modified = modified
                        .duration_since(UNIX_EPOCH)
                        .map(|value| value.as_secs_f64() * 1000.0)
                        .unwrap_or_else(|error| -error.duration().as_secs_f64() * 1000.0);
                    if (self.now)() - modified > 120_000.0 {
                        match fs::remove_file(&path) {
                            Ok(()) => continue,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(io("unlink", &path, error)),
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(io("stat", &path, error)),
            }
            if (self.now)() - started > 15_000.0 {
                return Err(Error::Message(format!(
                    "multiprovider: timed out waiting for auth store lock at {}",
                    path.display()
                ))
                .into());
            }
            pause(signal)?;
        }
    }
}
struct Lock {
    file: Option<File>,
    path: PathBuf,
}
impl Drop for Lock {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}
struct Wake(std::thread::Thread);
impl std::task::Wake for Wake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}
fn pause(signal: &CancellationToken) -> Result<(), Failure> {
    use std::future::Future;
    let end = std::time::Instant::now() + Duration::from_millis(25);
    let waker = std::task::Waker::from(Arc::new(Wake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut wait = Box::pin(signal.cancelled());
    loop {
        if wait.as_mut().poll(&mut context).is_ready() {
            return Err(cancelled());
        }
        let Some(remaining) = end.checked_duration_since(std::time::Instant::now()) else {
            return Ok(());
        };
        std::thread::park_timeout(remaining);
    }
}

//! The pinned renewable pathname lease, including its eviction-marker fences.
use super::{
    ceremony::{Failure, Lease},
    private,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

pub struct Options {
    pub path: PathBuf,
    pub name: String,
    pub ttl_ms: u64,
    pub renew: bool,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub step: Arc<dyn Fn(&str) + Send + Sync>,
    pub backoff: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub renewal_interval: Option<Duration>,
}
impl Options {
    pub fn new(path: PathBuf, name: impl Into<String>, ttl_ms: u64, renew: bool) -> Self {
        Self {
            path,
            name: name.into(),
            ttl_ms,
            renew,
            now: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64()
                    * 1000.0
            }),
            step: Arc::new(|_| {}),
            backoff: Arc::new(|| {
                let mut byte = [0];
                getrandom::getrandom(&mut byte).expect("OS random source failed");
                u64::from(byte[0] % 4)
            }),
            renewal_interval: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Loss {
    pub reason: &'static str,
    pub expected_owner: String,
    pub observed: Option<Value>,
}
struct Core {
    options: Options,
    path: PathBuf,
    marker: PathBuf,
    owner: String,
    marker_owner: String,
    released: AtomicBool,
    stop: CancellationToken,
    loss: watch::Sender<Option<Loss>>,
    terminal: Mutex<Option<Failure>>,
    execution: Mutex<()>,
}
pub struct FileLease {
    core: Arc<Core>,
    renewal: Option<tokio::task::JoinHandle<()>>,
}
impl Drop for FileLease {
    fn drop(&mut self) {
        self.core.stop.cancel();
    }
}
fn append(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    value.into()
}
fn read(path: &Path) -> Result<Vec<u8>, Failure> {
    #[cfg(windows)]
    {
        private::windows_store::read(path, true)
    }
    #[cfg(unix)]
    {
        fs::read(path).map_err(|e| private::io_error(e, "open", Some(path)))
    }
}
fn create(path: &Path, bytes: &[u8]) -> Result<(), Failure> {
    #[cfg(windows)]
    {
        private::windows_store::create(path, bytes, false)
    }
    #[cfg(unix)]
    {
        private::posix::create_exclusive(path, bytes)
            .map(drop)
            .map_err(|e| private::io_error(e, "open", Some(path)))
    }
}
fn create_marker(path: &Path) -> Result<(), Failure> {
    #[cfg(windows)]
    {
        private::windows_store::create(path, &[], true)
    }
    #[cfg(unix)]
    {
        fs::create_dir(path).map_err(|e| private::io_error(e, "mkdir", Some(path)))
    }
}
fn verify_directory(path: &Path) -> Result<(), Failure> {
    #[cfg(windows)]
    {
        private::windows_store::safe_parent(path)
    }
    #[cfg(unix)]
    {
        let _ = path;
        Ok(())
    }
}
fn remove_raw(path: &Path) -> std::io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    };
    match result {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
fn remove(path: &Path) -> Result<(), Failure> {
    match remove_raw(path) {
        Ok(()) => Ok(()),
        Err(e) => {
            #[cfg(windows)]
            {
                Err(private::windows_store::remove_error(path, e))
            }
            #[cfg(unix)]
            {
                let _ = e;
                Ok(())
            }
        }
    }
}
fn parse(bytes: Vec<u8>) -> Result<Value, Failure> {
    serde_json::from_str(&String::from_utf8_lossy(&bytes))
        .map_err(|_| Failure::Storage("Invalid lease JSON".into()))
}
fn number(value: Option<&Value>) -> f64 {
    let Some(value) = value else {
        return f64::NAN;
    };
    match value {
        Value::Null => 0.0,
        Value::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(_) | Value::Array(_) => {
            let value = crate::json::string(value);
            let value = crate::json::trim(&value);
            if value.is_empty() {
                return 0.0;
            }
            for (prefix, base) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(value) = value.strip_prefix(prefix) {
                    return u64::from_str_radix(value, base)
                        .map(|n| n as f64)
                        .unwrap_or(f64::NAN);
                }
            }
            value.parse().unwrap_or(f64::NAN)
        }
        _ => f64::NAN,
    }
}
fn expiry(owner: &Value) -> f64 {
    number(owner.get("expiresAt"))
}
#[derive(PartialEq, Debug)]
enum Identity {
    Owner(String, f64),
    Stat(u64, u64, f64, f64),
}
enum LockState {
    Missing,
    Live,
    Stale(Identity),
}
fn mtime(metadata: &fs::Metadata) -> f64 {
    match metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
    {
        Ok(d) => d.as_secs_f64() * 1000.0,
        Err(e) => -e.duration().as_secs_f64() * 1000.0,
    }
}
#[cfg(unix)]
fn file_id(_path: &Path, metadata: &fs::Metadata) -> Result<(u64, u64), Failure> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}
#[cfg(windows)]
fn file_id(path: &Path, _metadata: &fs::Metadata) -> Result<(u64, u64), Failure> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_READ_ATTRIBUTES,
    };
    let file = fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(7)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .map_err(|e| private::io_error(e, "stat", Some(path)))?;
    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // The handle stays live and Windows initializes the complete record on success.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } == 0 {
        return Err(private::io_error(
            std::io::Error::last_os_error(),
            "stat",
            Some(path),
        ));
    }
    let info = unsafe { info.assume_init() };
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow),
    ))
}
impl Core {
    fn step(&self, name: &str) {
        (self.options.step)(name);
    }
    fn now(&self) -> f64 {
        (self.options.now)()
    }
    fn backoff(&self) {
        std::thread::sleep(Duration::from_millis((self.options.backoff)()));
    }
    fn lost(&self) -> bool {
        self.loss.borrow().is_some()
    }
    fn record_loss(&self, reason: &'static str, observed: Option<Value>) {
        if self.released.load(Ordering::SeqCst) {
            return;
        }
        if self.loss.send_if_modified(|loss| {
            if loss.is_some() {
                false
            } else {
                *loss = Some(Loss {
                    reason,
                    expected_owner: self.owner.clone(),
                    observed,
                });
                true
            }
        }) {
            self.stop.cancel();
        }
    }
    fn ownership_error(&self, observed: Option<Value>) -> Failure {
        Failure::LockLost {
            target: self.options.path.clone(),
            name: self.options.name.clone(),
            expected_owner: self.owner.clone(),
            observed,
        }
    }
    fn owner(&self) -> Result<Value, Failure> {
        match read(&self.path) {
            Ok(bytes) => parse(bytes),
            Err(e)
                if e.io_code() == Some("EISDIR")
                    || matches!(&e,Failure::Private(p) if p.reason=="wrong-kind") =>
            {
                verify_directory(&self.path)?;
                parse(read(&self.path.join("owner.json"))?)
            }
            Err(e) => Err(e),
        }
    }
    fn bytes(&self) -> Vec<u8> {
        format!(
            "{}\n",
            crate::json::describe(
                &json!({"ownerId":self.owner,"expiresAt":self.now()+self.options.ttl_ms as f64})
            )
        )
        .into_bytes()
    }
    fn contention(&self, error: &Failure) -> Result<bool, Failure> {
        match error.io_code() {
            Some("EEXIST" | "EISDIR") => {
                #[cfg(windows)]
                {
                    if error.io_code() == Some("EISDIR") {
                        verify_directory(&self.path)?;
                    }
                    self.owner()?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }
    fn try_acquire(&self) -> Result<bool, Failure> {
        #[cfg(windows)]
        private::windows_store::ensure_directory(self.path.parent().unwrap())?;
        match create(&self.path, &self.bytes()) {
            Ok(()) => Ok(true),
            Err(e) if self.contention(&e)? => Ok(false),
            Err(e) if e.io_code() == Some("ENOENT") => {
                #[cfg(windows)]
                private::windows_store::ensure_directory(self.path.parent().unwrap())?;
                #[cfg(unix)]
                fs::create_dir_all(self.path.parent().unwrap())
                    .map_err(|e| private::io_error(e, "mkdir", self.path.parent()))?;
                match create(&self.path, &self.bytes()) {
                    Ok(()) => Ok(true),
                    Err(e) if self.contention(&e)? => Ok(false),
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        }
    }
    fn state(&self, notify: bool) -> Result<LockState, Failure> {
        if notify {
            self.step("lock-read-ready");
        }
        let owner = match self.owner() {
            Ok(owner) => Some(owner),
            Err(e) => {
                if cfg!(windows) && e.io_code() != Some("ENOENT") {
                    return Err(e);
                }
                None
            }
        };
        let expires = number(owner.as_ref().and_then(|v| v.get("expiresAt")));
        if expires.is_finite() && expires > self.now() {
            return Ok(LockState::Live);
        }
        if expires.is_finite() {
            if let Some(id) = owner.as_ref().and_then(|v| v["ownerId"].as_str()) {
                return Ok(LockState::Stale(Identity::Owner(id.into(), expires)));
            }
        }
        match fs::metadata(&self.path) {
            Ok(metadata) => {
                let modified = mtime(&metadata);
                let expires = if expires.is_finite() {
                    expires
                } else {
                    modified + self.options.ttl_ms as f64
                };
                if expires > self.now() {
                    Ok(LockState::Live)
                } else {
                    let (dev, ino) = file_id(&self.path, &metadata)?;
                    Ok(LockState::Stale(Identity::Stat(
                        dev, ino, modified, expires,
                    )))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.step("lock-missing");
                Ok(LockState::Missing)
            }
            Err(e) => Err(private::io_error(e, "stat", Some(&self.path))),
        }
    }
    fn owns_marker(&self) -> Result<bool, Failure> {
        match read(&self.marker.join("owner.json")).and_then(parse) {
            Ok(value) => Ok(value["ownerId"] == self.marker_owner),
            Err(e) => {
                if cfg!(windows) && e.io_code() != Some("ENOENT") {
                    Err(e)
                } else {
                    Ok(false)
                }
            }
        }
    }
    fn release_marker(&self) -> Result<(), Failure> {
        if self.owns_marker()? {
            remove(&self.marker)?;
        }
        Ok(())
    }
    fn acquire_marker(&self) -> Result<bool, Failure> {
        create_marker(&self.marker)?;
        let value = format!(
            "{}\n",
            crate::json::describe(&json!({"ownerId":self.marker_owner,"createdAt":self.now()}))
        );
        if let Err(error) = create(&self.marker.join("owner.json"), value.as_bytes()) {
            if error.io_code() == Some("ENOENT")
                || (!cfg!(windows) && matches!(error.io_code(), Some("EINVAL" | "ENOTDIR")))
            {
                return Ok(false);
            }
            self.release_marker()?;
            return Err(error);
        }
        self.step("eviction-marker-acquired");
        Ok(true)
    }
    fn recover_marker(&self) -> Result<&'static str, Failure> {
        let metadata = match fs::metadata(&self.marker) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("missing"),
            Err(e) => return Err(private::io_error(e, "stat", Some(&self.marker))),
        };
        verify_directory(&self.marker)?;
        if mtime(&metadata) + 5000.0 > self.now() {
            return Ok("fresh");
        }
        self.step("stale-marker-stat");
        let claimed = append(&self.marker, &format!(".{}", uuid::Uuid::new_v4()));
        match fs::rename(&self.marker, &claimed) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok("missing"),
            Err(e) => return Err(private::rename_error(e, &self.marker, &claimed)),
        }
        self.step("stale-marker-claimed");
        remove(&claimed)?;
        Ok("recovered")
    }
    fn with_marker(&self, action: impl FnOnce() -> Result<(), Failure>) -> Result<bool, Failure> {
        match self.acquire_marker() {
            Ok(false) => return Ok(false),
            Ok(true) => {}
            Err(e) if e.io_code() == Some("EEXIST") => {
                #[cfg(windows)]
                {
                    verify_directory(&self.marker)?;
                    if let Err(e) = read(&self.marker.join("owner.json")) {
                        if e.io_code() != Some("ENOENT") {
                            return Err(e);
                        }
                    }
                }
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        let result = action();
        self.release_marker()?;
        result?;
        Ok(true)
    }
    fn relinquish(&self) -> Result<(), Failure> {
        for _ in 0..8 {
            self.step("relinquish-read");
            let owner = match self.owner() {
                Ok(v) => v,
                Err(e) => return if cfg!(windows) { Err(e) } else { Ok(()) },
            };
            if owner["ownerId"] != self.owner {
                return Ok(());
            }
            // This deliberately retains the original read/remove window.
            match remove_raw(&self.path) {
                Ok(()) => return Ok(()),
                Err(e) if cfg!(windows) => {
                    return Err(private::io_error(e, "rm", Some(&self.path)))
                }
                Err(_) => self.backoff(),
            }
        }
        Ok(())
    }
    fn acquire(&self) -> Result<bool, Failure> {
        if self.try_acquire()? {
            return Ok(true);
        }
        for _ in 0..8 {
            if self.try_acquire()? {
                return Ok(true);
            }
            let observed = match self.state(true)? {
                LockState::Live => return Ok(false),
                LockState::Missing => continue,
                LockState::Stale(id) => id,
            };
            self.step("stale-lock-observed");
            match self.acquire_marker() {
                Ok(true) => {}
                Ok(false) => {
                    self.backoff();
                    continue;
                }
                Err(e) if e.io_code() == Some("EEXIST") => {
                    if self.recover_marker()? == "fresh" {
                        return Ok(false);
                    }
                    self.backoff();
                    continue;
                }
                Err(e) => return Err(e),
            }
            let result: Result<Option<bool>, Failure> = (|| {
                match self.state(true)? {
                    LockState::Live => return Ok(Some(false)),
                    LockState::Stale(id) if id == observed => {}
                    _ => return Ok(None),
                }
                if !self.owns_marker()? {
                    return Ok(Some(false));
                }
                self.step("stale-lock-confirmed");
                if !self.owns_marker()? {
                    return Ok(Some(false));
                }
                if !matches!(self.state(false)?,LockState::Stale(id) if id==observed) {
                    return Ok(None);
                }
                remove(&self.path)?;
                if !self.owns_marker()? {
                    return Ok(Some(false));
                }
                self.step("stale-lock-removed");
                if !self.try_acquire()? {
                    return Ok(Some(false));
                }
                self.step("stale-lock-recreated");
                if !self.owns_marker()? {
                    self.relinquish()?;
                    return Ok(Some(false));
                }
                Ok(Some(true))
            })();
            self.release_marker()?;
            if let Some(acquired) = result? {
                return Ok(acquired);
            }
        }
        Ok(false)
    }
    fn assert_owned(&self) -> Result<(), Failure> {
        if let Some(e) = self.terminal.lock().unwrap().clone() {
            return Err(e);
        }
        let observed = match self.owner() {
            Ok(owner) => {
                if !self.released.load(Ordering::SeqCst)
                    && !self.lost()
                    && owner["ownerId"] == self.owner
                    && expiry(&owner) > self.now()
                {
                    return Ok(());
                }
                self.record_loss(
                    if owner["ownerId"] != self.owner {
                        "taken-over"
                    } else {
                        "expired"
                    },
                    Some(owner.clone()),
                );
                Some(owner)
            }
            Err(e) => {
                self.record_loss("unreadable", None);
                if matches!(&e, Failure::Private(_)) && cfg!(windows) {
                    return Err(e);
                }
                None
            }
        };
        Err(self.ownership_error(observed))
    }
    fn write_owner(&self) -> Result<(), Failure> {
        let fence = || {
            let owner = self.owner()?;
            if owner["ownerId"] != self.owner
                || !(expiry(&owner) > self.now())
                || !self.owns_marker()?
            {
                return Err(self.ownership_error(None));
            }
            Ok(())
        };
        #[cfg(windows)]
        {
            let remaining = (expiry(&self.owner()?) - self.now()).max(0.0);
            private::windows_store::write_atomic(
                &self.path,
                &self.bytes(),
                Some(std::time::Instant::now() + Duration::from_secs_f64(remaining / 1000.0)),
                fence,
            )
        }
        #[cfg(unix)]
        {
            use std::{
                io::Write,
                os::unix::fs::{OpenOptionsExt, PermissionsExt},
            };
            let temporary = append(&self.path, &format!(".{}.tmp", uuid::Uuid::new_v4()));
            let mut created = false;
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&temporary)
                    .map_err(|e| private::io_error(e, "open", Some(&temporary)))?;
                created = true;
                file.write_all(&self.bytes())
                    .map_err(|e| private::io_error(e, "write", None))?;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|e| private::io_error(e, "fchmod", None))?;
                drop(file);
                fence()?;
                fs::rename(&temporary, &self.path)
                    .map_err(|e| private::rename_error(e, &temporary, &self.path))?;
                created = false;
                Ok(())
            })();
            if created {
                let _ = fs::remove_file(temporary);
            }
            result
        }
    }
    fn renew(&self) {
        let _execution = self.execution.lock().unwrap();
        let result = self.with_marker(|| {
            let owner = self.owner()?;
            if self.released.load(Ordering::SeqCst) || self.lost() {
                return Ok(());
            }
            if owner["ownerId"] != self.owner {
                self.record_loss("taken-over", Some(owner));
                return Ok(());
            }
            if !(expiry(&owner) > self.now()) {
                self.record_loss("expired", Some(owner));
                return Ok(());
            }
            for step in [
                "renewal-owner-confirmed",
                "renewal-write-fenced",
                "renewal-write-ready",
            ] {
                self.step(step);
                if self.released.load(Ordering::SeqCst) || self.lost() {
                    return Ok(());
                }
                if step != "renewal-write-ready" && !self.owns_marker()? {
                    return Ok(());
                }
            }
            self.write_owner()?;
            self.step("renewal-write-finished");
            if !self.owns_marker()? {
                self.record_loss("marker-lost", None);
                self.relinquish()?;
            }
            Ok(())
        });
        match result {
            Ok(false) => self.step("renewal-marker-unavailable"),
            Ok(true) => {}
            Err(error) => {
                if cfg!(windows) && matches!(&error, Failure::Private(_)) {
                    *self.terminal.lock().unwrap() = Some(error);
                    self.record_loss("renewal-failed", None);
                }
                match self.owner() {
                    Ok(owner) if owner["ownerId"] != self.owner => {
                        self.record_loss("taken-over", Some(owner))
                    }
                    Ok(owner) if !(expiry(&owner) > self.now()) => {
                        self.record_loss("expired", Some(owner))
                    }
                    Err(_) => self.record_loss("renewal-failed", None),
                    _ => {}
                }
            }
        }
        self.step("renewal-finished");
    }
    fn release(&self) -> Result<(), Failure> {
        let _execution = self.execution.lock().unwrap();
        for _ in 0..8 {
            let mut confirmed = false;
            let result = self.with_marker(|| {
                let owner = match self.owner() {
                    Ok(v) => v,
                    Err(e) if cfg!(windows) && e.io_code() == Some("ENOENT") => {
                        confirmed = true;
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                if owner["ownerId"] != self.owner {
                    confirmed = true;
                    return Ok(());
                }
                self.step("release-owner-confirmed");
                if !self.owns_marker()? {
                    return Ok(());
                }
                remove(&self.path)?;
                confirmed = true;
                Ok(())
            });
            match result {
                Ok(true) => {
                    #[cfg(windows)]
                    if !confirmed {
                        return Err(private::windows_store::release_error(&self.path, "EIO"));
                    }
                    return Ok(());
                }
                Ok(false) => {
                    if let Err(e) = self.recover_marker() {
                        return if cfg!(windows) { Err(e) } else { Ok(()) };
                    }
                }
                Err(e) => return if cfg!(windows) { Err(e) } else { Ok(()) },
            }
            self.backoff();
        }
        #[cfg(windows)]
        return Err(private::windows_store::release_error(
            &self.path,
            "ETIMEDOUT",
        ));
        #[cfg(unix)]
        Ok(())
    }
}
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Failure::Storage("Vault filesystem operation failed".into()))?
}
impl FileLease {
    pub async fn acquire(options: Options) -> Result<Option<Self>, Failure> {
        let path = append(&options.path, &format!(".{}.lock", options.name));
        let marker = append(&path, ".evicting");
        let (loss, _) = watch::channel(None);
        let core = Arc::new(Core {
            options,
            path,
            marker,
            owner: uuid::Uuid::new_v4().to_string(),
            marker_owner: uuid::Uuid::new_v4().to_string(),
            released: AtomicBool::new(false),
            stop: CancellationToken::new(),
            loss,
            terminal: Mutex::new(None),
            execution: Mutex::new(()),
        });
        let acquire = core.clone();
        if !blocking(move || acquire.acquire()).await? {
            return Ok(None);
        }
        let renewal = if core.options.renew {
            let weak = Arc::downgrade(&core);
            let stop = core.stop.clone();
            let delay = core
                .options
                .renewal_interval
                .unwrap_or(Duration::from_millis((core.options.ttl_ms / 3).max(1000)));
            Some(tokio::spawn(async move {
                loop {
                    tokio::select! {biased;_ = stop.cancelled()=>break,_=tokio::time::sleep(delay)=>{}}
                    let Some(core) = weak.upgrade() else {
                        break;
                    };
                    if core.lost() {
                        break;
                    }
                    if blocking(move || {
                        core.renew();
                        Ok(())
                    })
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
            }))
        } else {
            None
        };
        Ok(Some(Self { core, renewal }))
    }
    pub fn owner_id(&self) -> &str {
        &self.core.owner
    }
    pub fn loss(&self) -> Option<Loss> {
        self.core.loss.borrow().clone()
    }
    pub async fn when_lost(&self) -> Loss {
        let mut receiver = self.core.loss.subscribe();
        loop {
            if let Some(loss) = receiver.borrow().clone() {
                return loss;
            }
            let _ = receiver.changed().await;
        }
    }
    pub async fn renew_now(&self) -> Result<(), Failure> {
        let core = self.core.clone();
        blocking(move || {
            core.renew();
            Ok(())
        })
        .await
    }
}
impl Lease for FileLease {
    async fn assert_owned(&self) -> Result<(), Failure> {
        let core = self.core.clone();
        blocking(move || core.assert_owned()).await
    }
    async fn release(mut self) -> Result<(), Failure> {
        self.core.released.store(true, Ordering::SeqCst);
        self.core.stop.cancel();
        if let Some(task) = self.renewal.take() {
            let _ = task.await;
        }
        let core = self.core.clone();
        blocking(move || core.release()).await
    }
}

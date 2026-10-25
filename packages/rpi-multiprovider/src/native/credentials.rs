//! Private credential store for pooled accounts.
//!
//! The host's delegated authentication path asks a registered store for one
//! account at a time (`read`, `lock`, `replace`, `remove`). Upstream stores the
//! same credential object next to the account in `multiprovider-auth.json`, so
//! this adapter returns that object unchanged and writes replacements through
//! the already-verified account store.
use super::Runtime;
use crate::services::Error;
use crate::store::Failure;
use rpi_plugin_sdk::{callbacks::OwnedString, AuthRequestControl, StbString, StbStringRef};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

pub const CREDENTIAL_STORE_ID: &str = "multiprovider";

/// Prefix of the account ids reserved for in-flight logins, so a reservation is
/// recognizable in diagnostics. Reservations are looked up by exact id first.
/// The store never mints an account with this prefix, and it skips a stored
/// record that has one, so such a record is never served or scheduled.
pub const PENDING_ACCOUNT_PREFIX: &str = crate::store::RESERVED_ACCOUNT_PREFIX;

/// A private pending account: the persist-first destination of one host login.
///
/// The host's login always persists the credential it obtains into a store
/// account before reporting success. A new pool account has no record yet, so
/// `/multilogin` reserves a pending id, lets the host lock and replace it here,
/// and then commits the captured credential through the store's `add` (or
/// `replace` for reauthentication) exactly as upstream does with the login's
/// return value. The scheduler and the account file never see a reservation.
pub struct PendingLogin {
    pub provider: String,
    pub credential: Option<Value>,
}

/// Return the stored credential for one pooled account, or `None`.
///
/// This is the private half of `MultiAuthStore.getPool`: the public pool never
/// exposes credentials, while the host store port needs exactly the credential
/// object upstream persists.
pub fn read(path: &Path, provider: &str, account: &str) -> Result<Option<Value>, Failure> {
    let store = crate::store::Store::new(path.to_path_buf());
    let state = store.read()?;
    Ok(credential(&state, provider, account))
}

fn credential(state: &Value, provider: &str, account: &str) -> Option<Value> {
    if crate::store::is_reserved_account_id(account) {
        return None;
    }
    state["providers"]
        .get(provider)?
        .get("accounts")?
        .as_array()?
        .iter()
        .find(|candidate| candidate["id"].as_str() == Some(account))
        .map(|candidate| candidate["credential"].clone())
}

/// The credential wire value the host parses as `Credential`.
///
/// `api_key` keeps its optional key and environment; OAuth keeps access,
/// refresh and the millisecond expiry unchanged. A malformed record is refused
/// rather than silently rewritten.
pub fn normalized(value: &Value) -> Result<Value, String> {
    if !value.is_object() {
        return Err("malformed stored credential".into());
    }
    match value["type"].as_str() {
        Some("api_key") => {
            if value.get("key").is_some_and(|key| !key.is_string()) {
                return Err("malformed stored API key".into());
            }
            if value.get("env").is_some_and(|env| !env.is_object()) {
                return Err("malformed stored credential environment".into());
            }
            Ok(value.clone())
        }
        Some("oauth") => {
            if !value.get("access").is_some_and(Value::is_string)
                || !value.get("refresh").is_some_and(Value::is_string)
                || !value.get("expires").and_then(Value::as_i64).is_some()
            {
                return Err("malformed stored OAuth credential".into());
            }
            Ok(value.clone())
        }
        _ => Err("unsupported stored credential type".into()),
    }
}

/// One private cross-process lock held between `lock` and `release`.
pub struct Held {
    path: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".credentials.lock");
    PathBuf::from(name)
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs_f64() * 1000.0)
        .unwrap_or_default()
}

/// Acquire the per-store credential lock. Cancellation refuses a waiter; a
/// late acquisition is returned to the caller, which releases it immediately.
pub fn acquire(
    path: &Path,
    signal: &CancellationToken,
    cancelled: impl Fn() -> bool,
) -> Result<Box<Held>, Failure> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| Failure::Io {
                operation: "mkdir",
                path: parent.into(),
                error,
            })?;
        }
    }
    let path = lock_path(path);
    let started = now_ms();
    loop {
        if signal.is_cancelled() || cancelled() {
            return Err(Error::Message("multiprovider: operation cancelled".into()).into());
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                let _ = file.write_all(format!("{}\n", std::process::id()).as_bytes());
                return Ok(Box::new(Held { path }));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(Failure::Io {
                    operation: "open",
                    path,
                    error,
                })
            }
        }
        match fs::metadata(&path).and_then(|metadata| metadata.modified()) {
            Ok(modified) => {
                let modified = modified
                    .duration_since(UNIX_EPOCH)
                    .map(|value| value.as_secs_f64() * 1000.0)
                    .unwrap_or_default();
                if now_ms() - modified > 120_000.0 {
                    match fs::remove_file(&path) {
                        Ok(()) => continue,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(Failure::Io {
                                operation: "unlink",
                                path,
                                error,
                            })
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(Failure::Io {
                    operation: "stat",
                    path,
                    error,
                })
            }
        }
        if now_ms() - started > 15_000.0 {
            return Err(Error::Message(format!(
                "multiprovider: timed out waiting for credential store lock at {}",
                path.display()
            ))
            .into());
        }
        sleep(signal, &cancelled, Duration::from_millis(25))?;
    }
}

fn sleep(
    signal: &CancellationToken,
    cancelled: &impl Fn() -> bool,
    duration: Duration,
) -> Result<(), Failure> {
    let end = std::time::Instant::now() + duration;
    while std::time::Instant::now() < end {
        if signal.is_cancelled() || cancelled() {
            return Err(Error::Message("multiprovider: operation cancelled".into()).into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

fn value(out: *mut StbString, value: Value) -> i32 {
    if out.is_null() {
        return 2;
    }
    unsafe {
        *out = StbString::from_string(value.to_string());
    }
    0
}

fn cancelled(control: *const AuthRequestControl) -> bool {
    unsafe { control.as_ref() }
        .is_some_and(|control| (control.is_cancelled)(control.user_data) != 0)
}

/// Registered credential-store request callback.
pub extern "C" fn request(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut std::ffi::c_void,
) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        if !control.is_null()
            && unsafe { control.cast::<u32>().read() }
                < std::mem::size_of::<AuthRequestControl>() as u32
        {
            return 2;
        }
        let runtime = unsafe { &*context.cast::<Runtime>() };
        let Ok(request) = serde_json::from_str::<Value>(unsafe { input.as_str() }) else {
            return 2;
        };
        if request["version"] != 1 || request["storeId"] != CREDENTIAL_STORE_ID {
            return 2;
        }
        let provider = request["providerId"].as_str().unwrap_or("");
        let account = request["accountId"].as_str().unwrap_or("");
        if provider.is_empty() || account.is_empty() {
            return 2;
        }
        if cancelled(control) {
            return value(
                out,
                json!({"version":1,"kind":"error","payload":{"message":"operation cancelled"}}),
            );
        }
        let signal = CancellationToken::new();
        let cancelled = || cancelled(control);
        let result = match request["action"].as_str() {
            Some("read") => runtime.store_read(provider, account),
            Some("lock") => runtime.lock_credential(provider, account, &signal, &cancelled),
            Some("replace") => {
                let lock = request["input"]["lockId"].as_str().unwrap_or("");
                runtime.replace_credential(provider, account, lock, &request["input"]["credential"])
            }
            Some("remove") => {
                let lock = request["input"]["lockId"].as_str().unwrap_or("");
                runtime.remove_credential(provider, account, lock)
            }
            _ => Err("unknown credential store operation".into()),
        };
        value(
            out,
            match result {
                Ok(result) => result,
                Err(error) => json!({"version":1,"kind":"error","payload":{"message":error}}),
            },
        )
    })
}

/// Release callback: a leaf operation that only drops the file lock.
pub extern "C" fn release(lock_id: StbStringRef, context: *mut std::ffi::c_void) {
    rpi_plugin_sdk::guard_or((), || {
        if context.is_null() {
            return;
        }
        let runtime = unsafe { &*context.cast::<Runtime>() };
        let id = unsafe { lock_id.as_str() };
        runtime.release_lock(id);
    });
}

impl super::Shared {
    /// The reservation for `account`, checked against `provider`. `Ok(None)`
    /// means `account` is not reserved and names a stored account.
    fn pending(&self, provider: &str, account: &str) -> Result<Option<Option<Value>>, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "credential lock state poisoned".to_owned())?;
        match state.pending_logins.get(account) {
            None => Ok(None),
            Some(pending) if pending.provider == provider => Ok(Some(pending.credential.clone())),
            Some(_) => Err(format!(
                "multiprovider: stored account \"{account}\" is unavailable"
            )),
        }
    }

    /// Reserves a private pending account for one login of `provider`.
    pub(super) fn reserve_pending_login(&self, provider: &str) -> Result<String, String> {
        let id = format!("{PENDING_ACCOUNT_PREFIX}{}", uuid::Uuid::new_v4());
        self.state
            .lock()
            .map_err(|_| "credential lock state poisoned".to_owned())?
            .pending_logins
            .insert(
                id.clone(),
                PendingLogin {
                    provider: provider.to_owned(),
                    credential: None,
                },
            );
        Ok(id)
    }

    /// Drops a reservation, returning the credential the login persisted.
    pub(super) fn take_pending_login(&self, id: &str) -> Option<Value> {
        self.state
            .lock()
            .ok()?
            .pending_logins
            .remove(id)
            .and_then(|pending| pending.credential)
    }

    fn set_pending(&self, account: &str, credential: Option<Value>) -> Result<(), String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "credential lock state poisoned".to_owned())?;
        let pending = state
            .pending_logins
            .get_mut(account)
            .ok_or_else(|| format!("multiprovider: stored account \"{account}\" is unavailable"))?;
        pending.credential = credential;
        Ok(())
    }

    pub(super) fn store_read(&self, provider: &str, account: &str) -> Result<Value, String> {
        if let Some(credential) = self.pending(provider, account)? {
            return Ok(json!({"version":1,"kind":"credential","payload":credential}));
        }
        let credential = read(self.store.path(), provider, account)
            .map_err(|failure| failure_text(failure))?
            .map(|value| normalized(&value))
            .transpose()?;
        Ok(json!({"version":1,"kind":"credential","payload":credential}))
    }

    pub(super) fn lock_credential(
        &self,
        provider: &str,
        account: &str,
        signal: &CancellationToken,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Value, String> {
        let pending = self.pending(provider, account)?;
        let held = acquire(self.store.path(), signal, cancelled).map_err(failure_text)?;
        let credential = match pending {
            Some(credential) => credential,
            None => read(self.store.path(), provider, account)
                .map_err(failure_text)?
                .map(|value| normalized(&value))
                .transpose()?,
        };
        let id = uuid::Uuid::new_v4().to_string();
        self.state
            .lock()
            .map_err(|_| "credential lock state poisoned".to_owned())?
            .locks
            .insert(id.clone(), (provider.to_owned(), account.to_owned(), held));
        Ok(json!({"version":1,"kind":"locked","payload":{"lockId":id,"credential":credential}}))
    }

    pub(super) fn release_lock(&self, id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.locks.remove(id);
        }
    }

    fn check_lock(&self, provider: &str, account: &str, id: &str) -> Result<(), String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "credential lock state poisoned".to_owned())?;
        match state.locks.get(id) {
            Some((known_provider, known_account, _))
                if known_provider == provider && known_account == account =>
            {
                Ok(())
            }
            _ => Err("unknown credential lock".into()),
        }
    }

    pub(super) fn replace_credential(
        &self,
        provider: &str,
        account: &str,
        lock: &str,
        credential: &Value,
    ) -> Result<Value, String> {
        self.check_lock(provider, account, lock)?;
        let credential = normalized(credential)?;
        if self.pending(provider, account)?.is_some() {
            self.set_pending(account, Some(credential))?;
            return Ok(json!({"version":1,"kind":"success","payload":{}}));
        }
        let request = json!({
            "op":"replace","provider":provider,"account":account,"credential":credential,
        });
        self.store_write(request)?;
        Ok(json!({"version":1,"kind":"success","payload":{}}))
    }

    pub(super) fn remove_credential(
        &self,
        provider: &str,
        account: &str,
        lock: &str,
    ) -> Result<Value, String> {
        self.check_lock(provider, account, lock)?;
        if self.pending(provider, account)?.is_some() {
            self.set_pending(account, None)?;
            return Ok(json!({"version":1,"kind":"success","payload":{}}));
        }
        let request = json!({"op":"remove","provider":provider,"account":account});
        self.store_write(request)?;
        Ok(json!({"version":1,"kind":"success","payload":{}}))
    }

    /// Writes run on the registration runtime so shutdown drains them before the
    /// library is released.
    fn store_write(&self, request: Value) -> Result<Value, String> {
        let signal = CancellationToken::new();
        let store = self.store.clone();
        let work_signal = signal.clone();
        match self
            .context
            .run(&signal, store.request(request, work_signal))
        {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(failure)) => Err(failure_text(failure)),
            Err(error) => Err(error),
        }
    }
}

/// Host-visible text for a store failure. Syntax errors stay redacted.
pub fn failure_text(failure: Failure) -> String {
    match failure {
        Failure::Public(error) => error.to_string(),
        Failure::Syntax { .. } => "multiprovider: malformed account store".into(),
        Failure::Io {
            operation, path, ..
        } => format!(
            "multiprovider: could not {operation} account store at {}",
            path.display()
        ),
    }
}

/// Owned output helper for tests that call the C callbacks directly.
pub fn take(out: StbString) -> Value {
    let owned = unsafe { OwnedString::from_raw(out, Some(rpi_plugin_sdk::callbacks::free_string)) };
    serde_json::from_str(&owned.to_string_lossy()).unwrap_or(Value::Null)
}

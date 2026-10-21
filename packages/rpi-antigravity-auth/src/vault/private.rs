//! Private-file policy and OS boundary used by enrollment and credential custody.
use super::ceremony::Failure;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct IoError {
    pub code: String,
    pub message: String,
}
pub(crate) fn io_error(error: std::io::Error, op: &str, path: Option<&std::path::Path>) -> Failure {
    #[cfg(unix)]
    let detail = match error.raw_os_error() {
        Some(libc::ENOENT) => ("ENOENT", "no such file or directory"),
        Some(libc::EEXIST) => ("EEXIST", "file already exists"),
        Some(libc::EISDIR) => ("EISDIR", "illegal operation on a directory"),
        Some(libc::ENOTDIR) => ("ENOTDIR", "not a directory"),
        Some(libc::EACCES) => ("EACCES", "permission denied"),
        Some(libc::EPERM) => ("EPERM", "operation not permitted"),
        Some(libc::EIO) => ("EIO", "i/o error"),
        Some(libc::ENOSPC) => ("ENOSPC", "no space left on device"),
        Some(libc::ELOOP) => ("ELOOP", "too many symbolic links encountered"),
        Some(libc::EINVAL) => ("EINVAL", "invalid argument"),
        Some(libc::ENOTEMPTY) => ("ENOTEMPTY", "directory not empty"),
        Some(libc::EXDEV) => ("EXDEV", "cross-device link not permitted"),
        Some(libc::EROFS) => ("EROFS", "read-only file system"),
        _ => ("UNKNOWN", "unknown error"),
    };
    #[cfg(windows)]
    let detail = match error.raw_os_error() {
        Some(2 | 3) => ("ENOENT", "no such file or directory"),
        Some(80 | 183) => ("EEXIST", "file already exists"),
        Some(5) => ("EPERM", "operation not permitted"),
        Some(32 | 33) => ("EBUSY", "resource busy or locked"),
        Some(267) => ("ENOTDIR", "not a directory"),
        _ => ("UNKNOWN", "unknown error"),
    };
    Failure::Io(IoError {
        code: detail.0.into(),
        message: format!(
            "{}: {}, {}{}",
            detail.0,
            detail.1,
            op,
            path.map(|p| format!(" '{}'", p.display()))
                .unwrap_or_default()
        ),
    })
}
pub(crate) fn rename_error(
    error: std::io::Error,
    from: &std::path::Path,
    to: &std::path::Path,
) -> Failure {
    let Failure::Io(mut error) = io_error(error, "rename", Some(from)) else {
        unreachable!()
    };
    error.message.push_str(&format!(" -> '{}'", to.display()));
    Failure::Io(error)
}

#[derive(Clone, Debug)]
pub struct PrivateError {
    pub path: PathBuf,
    pub stage: &'static str,
    pub code: String,
    pub reason: String,
    pub native_message: Option<String>,
    pub win32_code: Option<u32>,
    pub level: Option<PathBuf>,
    pub publication: Option<Publication>,
}
#[derive(Clone, Debug, Default)]
pub struct Publication {
    pub published: bool,
    pub file_durable: bool,
    pub attempts: usize,
    pub elapsed_ms: f64,
    pub orphan: Option<PathBuf>,
}
impl std::fmt::Display for PrivateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f,"Private file refused at {}: {} ({}). Use a safe parent directory. Repair only a named secret file in safe ancestry whose owner the published policy trusts, without changing its owner or any existing directory. Previously exposed credentials may need rotation. Windows requires the private-files native prebuild and an ACL-persisting local filesystem such as NTFS.",self.stage,self.path.display(),self.reason)
    }
}
impl std::error::Error for PrivateError {}
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{perform, perform_legacy, Kind, Operation};
#[cfg(unix)]
pub mod posix;

fn validate_token(value: &serde_json::Value) -> Result<(), super::ceremony::Failure> {
    use super::ceremony::Failure;
    if !super::enrollment::valid_token(&value["token"]) {
        return Err(Failure::Storage(
            "enrollment token must be exactly 64 lowercase hex characters".into(),
        ));
    }
    if !value["token_generation"]
        .as_f64()
        .is_some_and(|n| n.fract() == 0.0 && (1.0..=9_007_199_254_740_991.0).contains(&n))
    {
        return Err(Failure::Storage(
            "enrollment token generation must be a positive safe integer".into(),
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub mod windows_store;

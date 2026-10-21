use super::PrivateError;
use std::{
    ffi::{c_char, c_void, CStr, OsString},
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr,
};
#[repr(C)]
struct NativeResult {
    job: *mut c_void,
    error: u32,
    reason: *const c_char,
    message: *const c_char,
    level: *const u16,
    bytes: *const u8,
    length: usize,
}
extern "C" {
    fn rpi_private_perform(
        op: i32,
        directory: i32,
        path: *const u16,
        bytes: *const u8,
        length: usize,
        result: *mut NativeResult,
    );
    fn rpi_private_release(job: *mut c_void);
    fn rpi_private_legacy_perform(
        op: i32,
        directory: i32,
        path: *const u16,
        bytes: *const u8,
        length: usize,
        result: *mut NativeResult,
    );
    fn rpi_private_legacy_release(job: *mut c_void);
}
struct OwnedResult(NativeResult, bool);
impl Drop for OwnedResult {
    fn drop(&mut self) {
        // The C boundary owns every buffer in this result and erases secret copies.
        unsafe {
            if self.1 {
                rpi_private_legacy_release(self.0.job)
            } else {
                rpi_private_release(self.0.job)
            }
        }
    }
}
#[derive(Clone, Copy)]
pub enum Operation {
    Create = 0,
    Read = 1,
    Verify = 2,
    Repair = 3,
    SafeParent = 4,
}
#[derive(Clone, Copy)]
pub enum Kind {
    Auto = -1,
    File = 0,
    Directory = 1,
}
/// Run on the blocking I/O pool; the original policy pins ancestors through completion.
pub fn perform(
    path: &Path,
    op: Operation,
    kind: Kind,
    bytes: &[u8],
    stage: &'static str,
) -> Result<Vec<u8>, PrivateError> {
    perform_version(path, op, kind, bytes, stage, false)
}
pub fn perform_legacy(
    path: &Path,
    op: Operation,
    kind: Kind,
    bytes: &[u8],
    stage: &'static str,
) -> Result<Vec<u8>, PrivateError> {
    assert!(
        !matches!(op, Operation::SafeParent),
        "the legacy publisher has no safe-parent operation"
    );
    perform_version(path, op, kind, bytes, stage, true)
}
fn perform_version(
    path: &Path,
    op: Operation,
    kind: Kind,
    bytes: &[u8],
    stage: &'static str,
    legacy: bool,
) -> Result<Vec<u8>, PrivateError> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(PrivateError {
            path: path.into(),
            stage,
            code: "ERR_PRIVATE_FILES_SECURITY".into(),
            reason: "invalid-path".into(),
            native_message: None,
            win32_code: None,
            level: None,
            publication: None,
        });
    }
    let mut wide = wide;
    wide.push(0);
    let mut result = OwnedResult(
        NativeResult {
            job: ptr::null_mut(),
            error: 0,
            reason: ptr::null(),
            message: ptr::null(),
            level: ptr::null(),
            bytes: ptr::null(),
            length: 0,
        },
        legacy,
    );
    // Inputs remain live for this synchronous call; output is owned until the guard drops.
    unsafe {
        let call = if legacy {
            rpi_private_legacy_perform
        } else {
            rpi_private_perform
        };
        call(
            op as i32,
            kind as i32,
            wide.as_ptr(),
            bytes.as_ptr(),
            bytes.len(),
            &mut result.0,
        )
    };
    if !result.0.reason.is_null() {
        // The native result retains NUL-terminated diagnostics through this scope.
        let reason = unsafe { CStr::from_ptr(result.0.reason) }
            .to_string_lossy()
            .into_owned();
        let level = if result.0.level.is_null() {
            None
        } else {
            let mut len = 0;
            // The native allocation is a terminated path owned by the result.
            unsafe {
                while *result.0.level.add(len) != 0 {
                    len += 1;
                }
                Some(PathBuf::from(OsString::from_wide(
                    std::slice::from_raw_parts(result.0.level, len),
                )))
            }
        };
        let code = if reason == "io-error" {
            match result.0.error {
                2 | 3 => "ENOENT",
                80 | 183 => "EEXIST",
                _ => "ERR_PRIVATE_FILES_SECURITY",
            }
        } else {
            "ERR_PRIVATE_FILES_SECURITY"
        };
        let native_message = if result.0.message.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(result.0.message) }
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        return Err(PrivateError {
            path: path.into(),
            stage,
            code: code.into(),
            reason,
            native_message,
            win32_code: Some(result.0.error),
            level,
            publication: None,
        });
    }
    if result.0.length == 0 {
        return Ok(Vec::new());
    }
    // Successful reads expose the checked file buffer, bounded to 64 MiB by the producer.
    Ok(unsafe { std::slice::from_raw_parts(result.0.bytes, result.0.length) }.to_vec())
}

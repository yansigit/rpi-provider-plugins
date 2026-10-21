//! Resolve packaged worker assets from this module, independent of the project cwd.
use std::path::PathBuf;

pub fn worker_entry() -> Result<PathBuf, String> {
    let module = module_path()?;
    let entry = module
        .parent()
        .ok_or("Cursor library has no installation directory")?
        .join("runtime/main.mjs");
    if !entry.is_file() {
        return Err("Cursor runtime/main.mjs is missing beside the installed library; reinstall rpi-cursor-sdk".into());
    }
    Ok(entry)
}
#[cfg(unix)]
fn module_path() -> Result<PathBuf, String> {
    use std::{
        ffi::{CStr, OsStr},
        os::unix::ffi::OsStrExt,
    };
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    // dladdr initializes this fixed record for a live function in this module.
    let info = unsafe {
        if libc::dladdr(
            worker_entry as *const () as *const libc::c_void,
            info.as_mut_ptr(),
        ) == 0
        {
            return Err("Cannot locate the installed Cursor library".into());
        }
        info.assume_init()
    };
    if info.dli_fname.is_null() {
        return Err("Cursor library location is unavailable".into());
    }
    let path = PathBuf::from(OsStr::from_bytes(
        unsafe { CStr::from_ptr(info.dli_fname) }.to_bytes(),
    ));
    path.canonicalize()
        .map_err(|_| "Cannot resolve the installed Cursor library".into())
}
#[cfg(windows)]
fn module_path() -> Result<PathBuf, String> {
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    let mut module = std::ptr::null_mut();
    // FROM_ADDRESS interprets this pointer as a function address, not UTF-16 text.
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            worker_entry as *const () as *const u16,
            &mut module,
        )
    } == 0
    {
        return Err("Cannot locate the installed Cursor library".into());
    }
    for size in [1024, 4096, 16384, 65536] {
        let mut name = vec![0u16; size];
        let length = unsafe { GetModuleFileNameW(module, name.as_mut_ptr(), size as u32) } as usize;
        if length == 0 {
            break;
        }
        if length < size {
            return Ok(PathBuf::from(OsString::from_wide(&name[..length])));
        }
    }
    Err("Cursor library location is unavailable or too long".into())
}
#[cfg(test)]
mod tests {
    #[test]
    fn module_location_is_the_loaded_image_not_the_working_directory() {
        assert_eq!(
            super::module_path().unwrap().canonicalize().unwrap(),
            std::env::current_exe().unwrap().canonicalize().unwrap()
        );
    }
}

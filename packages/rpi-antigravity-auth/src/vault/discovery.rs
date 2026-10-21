//! Claustrum's connection-file precedence; never guess between temp-directory daemons.
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

fn joined(base: &Path, suffix: &str) -> PathBuf {
    use std::path::Component;
    let mut result = PathBuf::new();
    for part in base.join(suffix).components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(result.components().next_back(), Some(Component::Normal(_))) {
                    result.pop();
                } else if !result.has_root() {
                    result.push(part);
                }
            }
            _ => result.push(part),
        }
    }
    result
}

pub fn user_token(uid: Option<u32>, env: &HashMap<String, String>) -> String {
    if let Some(uid) = uid {
        return uid.to_string();
    }
    for key in ["USER", "USERNAME", "HOME", "USERPROFILE"] {
        if let Some(value) = env.get(key).filter(|v| !v.is_empty()) {
            return value
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
        }
    }
    "unknown".into()
}

pub fn resolve(
    explicit: Option<&str>,
    env: &HashMap<String, String>,
    temp: &Path,
    uid: Option<u32>,
    os_home: impl FnOnce() -> std::io::Result<PathBuf>,
) -> std::io::Result<PathBuf> {
    for value in [
        explicit.map(crate::json::trim),
        env.get("SUBC_CONNECTION_FILE").map(String::as_str),
        env.get("CLAUSTRUM_SUBC_CONNECTION")
            .map(|v| crate::json::trim(v)),
    ]
    .into_iter()
    .flatten()
    {
        if !value.is_empty() {
            return Ok(value.into());
        }
    }
    let runtime = env
        .get("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|v| joined(Path::new(v), "subc-connection.json"));
    let home = env.get("HOME").filter(|v| !v.is_empty()).map(|v| {
        joined(
            Path::new(v),
            ".local/share/cortexkit/run/subc-connection.json",
        )
    });
    let exact = joined(
        temp,
        &format!("subc-{}.connection.json", user_token(uid, env)),
    );
    for path in runtime
        .iter()
        .chain(home.iter())
        .chain(std::iter::once(&exact))
    {
        if path.is_file() {
            return Ok(path.clone());
        }
    }
    let mut matches = std::fs::read_dir(temp)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            (name.starts_with("subc-") && name.ends_with(".connection.json") && e.path().is_file())
                .then(|| e.path())
        });
    if let Some(only) = matches.next() {
        if matches.next().is_none() {
            return Ok(only);
        }
    }
    runtime.or(home).map(Ok).unwrap_or_else(|| {
        os_home().map(|v| joined(&v, ".local/share/cortexkit/run/subc-connection.json"))
    })
}

#[cfg(unix)]
pub fn os_home() -> std::io::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut size = 16_384;
    loop {
        let mut bytes = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                entry.as_mut_ptr(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
                &mut result,
            )
        };
        if code == libc::ERANGE {
            size *= 2;
            continue;
        }
        if code != 0 {
            return Err(std::io::Error::from_raw_os_error(code));
        }
        if result.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "OS user home is unavailable",
            ));
        }
        let entry = unsafe { entry.assume_init() };
        return Ok(PathBuf::from(std::ffi::OsStr::from_bytes(
            unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) }.to_bytes(),
        )));
    }
}

#[cfg(windows)]
pub fn os_home() -> std::io::Result<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        Security::TOKEN_QUERY,
        System::Threading::{GetCurrentProcess, OpenProcessToken},
        UI::Shell::GetUserProfileDirectoryW,
    };
    let mut token = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut length = 0;
    unsafe {
        GetUserProfileDirectoryW(token, std::ptr::null_mut(), &mut length);
    }
    let mut value = vec![0; length as usize];
    let result = unsafe { GetUserProfileDirectoryW(token, value.as_mut_ptr(), &mut length) };
    let error = std::io::Error::last_os_error();
    unsafe {
        CloseHandle(token);
    }
    if result == 0 {
        return Err(error);
    }
    value.truncate(value.iter().position(|v| *v == 0).unwrap_or(value.len()));
    Ok(std::ffi::OsString::from_wide(&value).into())
}

pub fn connection_file(env: &HashMap<String, String>) -> std::io::Result<PathBuf> {
    #[cfg(unix)]
    let uid = Some(unsafe { libc::getuid() });
    #[cfg(windows)]
    let uid = None;
    resolve(None, env, &std::env::temp_dir(), uid, os_home)
}

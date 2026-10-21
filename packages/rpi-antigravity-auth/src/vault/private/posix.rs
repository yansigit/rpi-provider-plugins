use super::super::{ceremony::Failure, policy::ConsumerError};
use crate::json::trim;
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
const MAX_BYTES: usize = 16 * 1024;
fn unsafe_file(message: impl Into<String>) -> Failure {
    ConsumerError::new("unsafe-file", message).into()
}
fn io_error(error: std::io::Error, op: &str, path: &Path) -> Failure {
    super::io_error(
        error,
        op,
        if matches!(op, "read" | "write" | "fstat" | "fsync" | "fchmod") {
            None
        } else {
            Some(path)
        },
    )
}
fn uid() -> u32 {
    unsafe { libc::geteuid() }
}
fn gid() -> u32 {
    unsafe { libc::getegid() }
}

#[derive(Clone, Copy, Debug)]
pub struct Metadata {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub directory: bool,
}
impl From<fs::Metadata> for Metadata {
    fn from(value: fs::Metadata) -> Self {
        Self {
            mode: value.mode(),
            uid: value.uid(),
            gid: value.gid(),
            directory: value.is_dir(),
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum ParentDecision {
    Allowed,
    PrivateGroup,
    Refused(&'static str),
}
pub fn ancestor(
    entry: Metadata,
    child: Option<&[Metadata; 2]>,
    uid: u32,
    gid: Option<u32>,
) -> ParentDecision {
    use ParentDecision::*;
    if !entry.directory {
        return Refused("ancestor is not a directory");
    }
    if entry.uid != uid && entry.uid != 0 {
        return Refused("foreign owner");
    }
    if entry.mode & 0o022 == 0 {
        return Allowed;
    }
    if entry.uid == 0 && entry.mode & 0o1000 != 0 {
        return if child.is_some_and(|c| c.iter().all(|v| v.uid == uid && v.mode & 0o022 == 0)) {
            Allowed
        } else {
            Refused("sticky ancestor without an owner-only child")
        };
    }
    if entry.mode & 0o002 != 0 {
        return Refused("world-writable ancestor");
    }
    if entry.mode & 0o1000 != 0 || entry.uid != uid || gid != Some(entry.gid) {
        return Refused("group-writable ancestor (not a non-sticky user-private group)");
    }
    PrivateGroup
}
fn records(bytes: &[u8], fields: usize, legacy: bool) -> Option<Vec<Vec<String>>> {
    let text = if legacy {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        std::str::from_utf8(bytes)
            .ok()?
            .strip_prefix('\u{feff}')
            .unwrap_or(std::str::from_utf8(bytes).ok()?)
            .to_owned()
    };
    let mut records = vec![];
    for line in text.split('\n') {
        if trim(line).is_empty()
            || (if legacy {
                line
            } else {
                line.trim_start_matches(|c: char| trim(&c.to_string()).is_empty())
            })
            .starts_with('#')
        {
            continue;
        }
        let row: Vec<_> = line.split(':').map(str::to_owned).collect();
        let valid_name = |name: &str| {
            !name.is_empty()
                && !name
                    .chars()
                    .any(|c| trim(&c.to_string()).is_empty() || (legacy && c == ','))
        };
        let valid_id = |id: &str| {
            !id.is_empty()
                && id.bytes().all(|b| b.is_ascii_digit())
                && id.parse::<u64>().ok().is_some_and(|v| {
                    v <= if legacy {
                        9_007_199_254_740_991
                    } else {
                        u32::MAX as u64
                    }
                })
        };
        if row.len() != fields
            || !valid_name(&row[0])
            || !valid_id(&row[2])
            || (fields == 7 && !valid_id(&row[3]))
            || (fields == 4 && !row[3].is_empty() && row[3].split(',').any(|s| !valid_name(s)))
        {
            return None;
        }
        records.push(row);
    }
    Some(records)
}
pub fn private_group_reason(
    passwd: &[u8],
    group: &[u8],
    uid: u32,
    gid: u32,
    legacy: bool,
) -> Option<&'static str> {
    let Some(users) = records(passwd, 7, legacy) else {
        return Some("could not parse /etc/passwd to establish a user-private group");
    };
    let Some(groups) = records(group, 4, legacy) else {
        return Some("could not parse /etc/group to establish a user-private group");
    };
    let owners: Vec<_> = users
        .iter()
        .filter(|r| r[2].parse::<u64>() == Ok(uid as u64))
        .collect();
    let matching: Vec<_> = groups
        .iter()
        .filter(|r| r[2].parse::<u64>() == Ok(gid as u64))
        .collect();
    if owners.len() != 1 || matching.len() != 1 {
        return Some("group-writable ancestor (effective user or group is absent or ambiguous in account files)");
    }
    if !matching[0][3].is_empty()
        && matching[0][3]
            .split(',')
            .any(|member| member != owners[0][0])
    {
        return Some("group-writable ancestor (other group members)");
    }
    if users
        .iter()
        .any(|r| r[2].parse::<u64>() != Ok(uid as u64) && r[3].parse::<u64>() == Ok(gid as u64))
    {
        return Some("group-writable ancestor (another user's primary group)");
    }
    None
}
fn group_proof(uid: u32, gid: u32, legacy: bool) -> Option<&'static str> {
    let Ok(passwd) = fs::read("/etc/passwd") else {
        return Some("could not read /etc/passwd to establish a user-private group");
    };
    let Ok(group) = fs::read("/etc/group") else {
        return Some("could not read /etc/group to establish a user-private group");
    };
    private_group_reason(&passwd, &group, uid, gid, legacy)
}
#[derive(Debug)]
pub struct ParentRefusal {
    pub path: PathBuf,
    pub reason: &'static str,
}
pub fn safe_parent(path: &Path) -> Result<(), ParentRefusal> {
    check_path(path, true)
}
pub fn check_path(path: &Path, directory_only: bool) -> Result<(), ParentRefusal> {
    let refused = |path: &Path, reason| ParentRefusal {
        path: path.into(),
        reason,
    };
    let unresolved = || {
        refused(
            path,
            "could not resolve target or its nearest existing ancestor",
        )
    };
    let mut candidate = path.to_owned();
    let mut missing = false;
    let mut current = loop {
        match fs::canonicalize(&candidate) {
            Ok(existing) => {
                let metadata = fs::metadata(&existing).map_err(|_| unresolved())?;
                if missing && !metadata.is_dir() {
                    return Err(unresolved());
                }
                break if missing || directory_only || metadata.is_dir() {
                    existing
                } else {
                    existing.parent().ok_or_else(unresolved)?.to_owned()
                };
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !matches!(fs::symlink_metadata(&candidate),Err(e) if e.kind()==std::io::ErrorKind::NotFound)
                {
                    return Err(unresolved());
                }
                candidate = candidate.parent().ok_or_else(unresolved)?.to_owned();
                missing = true;
            }
            Err(_) => return Err(unresolved()),
        }
    };
    let mut child = None;
    let mut proof = None;
    let (uid, gid) = (uid(), gid());
    loop {
        let entries = [fs::symlink_metadata(&current), fs::metadata(&current)];
        let [a, b] = entries;
        let entries = [
            a.map(Metadata::from)
                .map_err(|_| refused(&current, "could not inspect ancestor"))?,
            b.map(Metadata::from)
                .map_err(|_| refused(&current, "could not inspect ancestor"))?,
        ];
        for entry in entries {
            match ancestor(entry, child.as_ref(), uid, Some(gid)) {
                ParentDecision::Allowed => {}
                ParentDecision::Refused(reason) => return Err(refused(&current, reason)),
                ParentDecision::PrivateGroup => {
                    let reason = *proof.get_or_insert_with(|| group_proof(uid, gid, false));
                    if let Some(reason) = reason {
                        return Err(refused(&current, reason));
                    }
                }
            }
        }
        let Some(parent) = current.parent() else {
            return Ok(());
        };
        child = Some(entries);
        current = parent.to_owned();
    }
}
pub fn ensure_directory(path: &Path) -> Result<(), Failure> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| io_error(e, "mkdir", path))?;
    let metadata = fs::metadata(path).map_err(|e| io_error(e, "stat", path))?;
    if metadata.uid() != uid() {
        return Err(unsafe_file(
            "Claustrum enrollment directory must be owned by the current user",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|e| io_error(e, "chmod", path))?;
    }
    Ok(())
}
pub fn refuse_writable_ancestor(path: &Path) -> Result<(), Failure> {
    let Ok(mut current) = fs::canonicalize(path) else {
        return Ok(());
    };
    let (uid, gid) = (uid(), gid());
    let mut private = None;
    loop {
        if let Ok(metadata) = fs::metadata(&current) {
            if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
                let exclusive =
                    *private.get_or_insert_with(|| group_proof(uid, gid, true).is_none());
                if metadata.mode() & 0o002 != 0
                    || metadata.uid() != uid
                    || metadata.gid() != gid
                    || !exclusive
                {
                    return Err(unsafe_file(format!("Claustrum enrollment path has a writable ancestor: {}. Another user could replace it. Run: chmod g-w,o-w {}",current.display(),current.display())));
                }
            }
        }
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_owned();
    }
    Ok(())
}
pub fn read_bounded_json(path: &Path) -> Result<Option<Value>, Failure> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(unsafe_file(
                "Claustrum enrollment file could not be opened safely",
            ))
        }
    };
    let metadata = file.metadata().map_err(|e| io_error(e, "fstat", path))?;
    if !metadata.is_file() {
        return Err(unsafe_file(
            "Claustrum enrollment file must be a regular file",
        ));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(unsafe_file("Claustrum enrollment file must be owner-only"));
    }
    if metadata.uid() != uid() {
        return Err(unsafe_file(
            "Claustrum enrollment file must be owned by the current user",
        ));
    }
    let mut bytes = vec![0u8; MAX_BYTES + 1];
    let len = file
        .read(&mut bytes)
        .map_err(|e| io_error(e, "read", path))?;
    if len > MAX_BYTES {
        return Err(unsafe_file("Claustrum enrollment file is too large"));
    }
    serde_json::from_str(&String::from_utf8_lossy(&bytes[..len]))
        .map(Some)
        .map_err(|_| {
            ConsumerError::new(
                "invalid-state",
                "Claustrum enrollment file is not valid JSON",
            )
            .into()
        })
}
pub fn create_exclusive(path: &Path, bytes: &[u8]) -> std::io::Result<File> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(file)
}
pub fn write_state(path: &Path, value: &Value) -> Result<(), Failure> {
    let parent = path.parent().unwrap_or(Path::new("."));
    ensure_directory(parent)?;
    refuse_writable_ancestor(parent)?;
    let bytes = format!("{}\n", crate::json::describe(value));
    if bytes.len() > MAX_BYTES {
        return Err(
            ConsumerError::new("invalid-state", "Claustrum enrollment state is too large").into(),
        );
    }
    let temporary = parent.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name().unwrap().to_string_lossy(),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut created = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| io_error(e, "open", &temporary))?;
        created = true;
        file.write_all(bytes.as_bytes())
            .map_err(|e| io_error(e, "write", &temporary))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| io_error(e, "fchmod", &temporary))?;
        file.sync_all()
            .map_err(|e| io_error(e, "fsync", &temporary))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| super::rename_error(e, &temporary, path))?;
        created = false;
        Ok(())
    })();
    if created {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn token_parent(path: &Path) -> Result<(), Failure> {
    check_path(path, false).map_err(|e| {
        Failure::Storage(format!(
            "enrollment token folder {} is not private: {}",
            e.path.display(),
            e.reason
        ))
    })
}
fn create_private_parents(path: &Path) -> Result<(), Failure> {
    match fs::metadata(path) {
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e, "stat", path)),
    };
    let parent = path.parent().unwrap_or(Path::new("/"));
    if parent != path {
        create_private_parents(parent)?;
    }
    let mut sticky = false;
    if let Err(e) = check_path(parent, false) {
        sticky = e.reason == "sticky ancestor without an owner-only child"
            && fs::canonicalize(parent).ok().as_ref() == Some(&e.path);
        if !sticky {
            return Err(Failure::Storage(format!(
                "Private creation refused: {}: {}",
                e.reason,
                e.path.display()
            )));
        }
    }
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::AlreadyExists
                && fs::metadata(path).is_ok_and(|m| m.is_dir()) =>
        {
            return Ok(())
        }
        Err(e) => return Err(io_error(e, "mkdir", path)),
    }
    if sticky {
        if let Err(e) = check_path(path, false) {
            let _ = fs::remove_dir(path);
            return Err(Failure::Storage(format!(
                "Private creation refused: {}: {}",
                e.reason,
                e.path.display()
            )));
        }
    }
    Ok(())
}
pub fn write_token(path: &Path, value: &Value) -> Result<(), Failure> {
    super::validate_token(value)?;
    let parent = path.parent().unwrap_or(Path::new("."));
    token_parent(parent)?;
    create_private_parents(parent)?;
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
        check_path(parent, false).map_err(|e| {
            Failure::Storage(format!(
                "Private creation refused: {}: {}",
                e.reason,
                e.path.display()
            ))
        })?;
        match create_exclusive(
            &temporary,
            format!("{}\n", crate::json::describe(value)).as_bytes(),
        ) {
            Ok(file) => drop(file),
            Err(e) => {
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    created = false;
                }
                return Err(io_error(e, "open", &temporary));
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&temporary)
            .map_err(|e| io_error(e, "open", &temporary))?;
        file.sync_all()
            .map_err(|e| io_error(e, "fsync", &temporary))?;
        drop(file);
        fs::rename(&temporary, path).map_err(|e| super::rename_error(e, &temporary, path))?;
        created = false;
        Ok(())
    })();
    if created {
        let _ = fs::remove_file(temporary);
    }
    result
}

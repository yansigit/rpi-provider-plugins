//! Pi's byte-preserving custody swap, called only under the host credential-file lock.
use super::{ceremony::Failure, private::PrivateError};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub enum Error {
    Slot(&'static str),
    Private(Box<PrivateError>),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Slot(code) => write!(f, "Pi slot: {code}"),
            Self::Private(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(_: std::io::Error) -> Self {
        Self::Slot("io")
    }
}
impl From<Failure> for Error {
    fn from(error: Failure) -> Self {
        match error {
            Failure::Private(error) => Self::Private(Box::new(error)),
            _ => Self::Slot("io"),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    AfterRead,
    BeforeRename,
    StashWritten,
    AuthWritten,
    StashDeleted,
}
pub type Hook = Arc<dyn Fn(Step, &Path) -> Result<(), Error> + Send + Sync>;

pub struct Slot {
    auth: PathBuf,
    stash: PathBuf,
    provider: String,
    placeholder: Value,
    auth_identity: String,
    placeholder_identity: String,
    hook: Option<Hook>,
}
struct Property {
    key: String,
    start: usize,
    value: usize,
    end: usize,
}
struct Auth {
    text: String,
    root: usize,
    properties: Vec<Property>,
    index: Option<usize>,
    entry: Value,
}
fn absolute(path: &Path) -> Result<PathBuf, Error> {
    let path = std::env::current_dir()?.join(path);
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}
fn canonical(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            format!(
                "{{{}}}",
                keys.into_iter()
                    .map(|key| format!("{}:{}", json!(key), canonical(&map[key])))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Value::Array(array) => format!(
            "[{}]",
            array.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        _ => crate::json::describe(value),
    }
}
fn digest(value: &Value) -> String {
    Sha256::digest(canonical(value).as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
impl Auth {
    fn parse(text: String, provider: &str) -> Result<Self, Error> {
        let invalid = || Error::Slot("invalid-auth");
        let value: Value = serde_json::from_str(&text).map_err(|_| invalid())?;
        if !value.is_object() {
            return Err(invalid());
        }
        let bytes = text.as_bytes();
        let root = bytes.iter().position(|b| *b == b'{').ok_or_else(invalid)?;
        let mut i = root + 1;
        let whitespace = |i: &mut usize| {
            while bytes.get(*i).is_some_and(u8::is_ascii_whitespace) {
                *i += 1;
            }
        };
        let string_end = |i: &mut usize| {
            *i += 1;
            while *i < bytes.len() {
                let byte = bytes[*i];
                *i += 1;
                if byte == b'"' {
                    break;
                }
                if byte == b'\\' {
                    *i += 1;
                }
            }
        };
        let mut properties = Vec::new();
        let mut names = HashSet::new();
        whitespace(&mut i);
        while bytes[i] != b'}' {
            let start = i;
            string_end(&mut i);
            let key: String = serde_json::from_str(&text[start..i]).map_err(|_| invalid())?;
            if !names.insert(key.clone()) {
                return Err(invalid());
            }
            whitespace(&mut i);
            i += 1;
            whitespace(&mut i);
            let offset = i;
            let mut depth = 0;
            while i < bytes.len() {
                match bytes[i] {
                    b'"' => {
                        string_end(&mut i);
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    b',' if depth == 0 => break,
                    _ => {}
                }
                i += 1;
            }
            let mut end = i;
            while bytes[end - 1].is_ascii_whitespace() {
                end -= 1;
            }
            properties.push(Property {
                key,
                start,
                value: offset,
                end,
            });
            whitespace(&mut i);
            if bytes[i] == b'}' {
                break;
            }
            i += 1;
            whitespace(&mut i);
        }
        let index = properties
            .iter()
            .position(|property| property.key == provider);
        let entry = value.get(provider).cloned().unwrap_or(Value::Null);
        if index.is_some() && !entry.is_object() {
            return Err(invalid());
        }
        Ok(Self {
            text,
            root,
            properties,
            index,
            entry,
        })
    }
    fn entry_text(&self) -> Option<&str> {
        self.index
            .map(|i| &self.text[self.properties[i].value..self.properties[i].end])
    }
    fn replace(&self, provider: &str, next: Option<&str>) -> String {
        let (start, end, insertion) = if let Some(index) = self.index {
            let property = &self.properties[index];
            match next {
                Some(next) => (property.value, property.end, next.to_owned()),
                None => {
                    let start = index
                        .checked_sub(1)
                        .map(|i| self.properties[i].end)
                        .unwrap_or(property.start);
                    let end = if index == 0 {
                        self.properties
                            .get(1)
                            .map(|p| p.start)
                            .unwrap_or(property.end)
                    } else {
                        property.end
                    };
                    (start, end, String::new())
                }
            }
        } else {
            let Some(next) = next else {
                return self.text.clone();
            };
            let last = self.properties.last();
            let offset = last.map(|p| p.end).unwrap_or(self.root + 1);
            (
                offset,
                offset,
                format!(
                    "{}{}:{next}",
                    if last.is_some() { "," } else { "" },
                    json!(provider)
                ),
            )
        };
        format!("{}{}{}", &self.text[..start], insertion, &self.text[end..])
    }
}
impl Slot {
    pub fn new(
        auth: &Path,
        stash: &Path,
        provider: &str,
        placeholder: &str,
    ) -> Result<Self, Error> {
        if provider.is_empty()
            || auth.as_os_str().is_empty()
            || stash.as_os_str().is_empty()
            || placeholder.is_empty()
            || placeholder.starts_with('!')
            || placeholder.contains('$')
        {
            return Err(Error::Slot("invalid-options"));
        }
        let auth = absolute(auth)?;
        let stash = absolute(stash)?;
        let mut lock = auth.as_os_str().to_os_string();
        lock.push(".lock");
        if auth == stash || stash == PathBuf::from(lock) {
            return Err(Error::Slot("invalid-options"));
        }
        Ok(Self {
            auth_identity: digest(&json!(auth)),
            placeholder_identity: digest(&json!(placeholder)),
            auth,
            stash,
            provider: provider.into(),
            placeholder: json!({"type":"api_key","key":placeholder}),
            hook: None,
        })
    }
    /// Fault-injection seam for the pinned read, rename and crash boundaries.
    pub fn with_hook(mut self, hook: Hook) -> Self {
        self.hook = Some(hook);
        self
    }
    fn step(&self, step: Step, path: &Path) -> Result<(), Error> {
        if let Some(hook) = &self.hook {
            hook(step, path)?;
        }
        Ok(())
    }
    fn read(&self, path: &Path, private: bool) -> Result<Option<String>, Error> {
        #[cfg(unix)]
        let bytes = {
            use std::{
                io::Read,
                os::unix::fs::{MetadataExt, OpenOptionsExt},
            };
            let mut file = match fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path)
            {
                Ok(file) => file,
                Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(None),
                Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {
                    return Err(Error::Slot("unsafe-path"))
                }
                Err(error) => return Err(error.into()),
            };
            let stat = file.metadata()?;
            if !stat.is_file() || (private && stat.mode() & 0o777 != 0o600) {
                return Err(Error::Slot("unsafe-path"));
            }
            self.step(Step::AfterRead, path)?;
            let current = fs::symlink_metadata(path).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Error::Slot("unsafe-path")
                } else {
                    error.into()
                }
            })?;
            if !current.is_file() || current.dev() != stat.dev() || current.ino() != stat.ino() {
                return Err(Error::Slot("unsafe-path"));
            }
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            bytes
        };
        #[cfg(windows)]
        let bytes = {
            self.step(Step::AfterRead, path)?;
            match super::private::windows_store::read(path, true) {
                Ok(bytes) => bytes,
                Err(error) if error.io_code() == Some("ENOENT") => return Ok(None),
                Err(error) => return Err(error.into()),
            }
        };
        String::from_utf8(bytes).map(Some).map_err(|_| {
            Error::Slot(if private {
                "invalid-stash"
            } else {
                "invalid-auth"
            })
        })
    }
    fn load(&self) -> Result<(Auth, Option<Value>), Error> {
        self.ensure_parent(&self.auth, false)?;
        let auth = Auth::parse(
            self.read(&self.auth, false)?.unwrap_or_else(|| "{}".into()),
            &self.provider,
        )?;
        let Some(text) = self.read(&self.stash, true)? else {
            return Ok((auth, None));
        };
        let invalid = || Error::Slot("invalid-stash");
        let stash: Value = serde_json::from_str(&text).map_err(|_| invalid())?;
        let valid_entry = match stash.get("entry") {
            Some(Value::Null | Value::Object(_)) => true,
            _ => false,
        };
        if stash["version"] != 1
            || stash["provider"] != self.provider
            || stash["authIdentity"] != self.auth_identity
            || stash["placeholderIdentity"] != self.placeholder_identity
            || !valid_entry
            || stash["sha256"] != digest(&stash["entry"])
        {
            return Err(invalid());
        }
        if stash["entry"].is_null() {
            if stash.get("entryJson") != Some(&Value::Null) {
                return Err(invalid());
            }
        } else {
            let entry: Value =
                serde_json::from_str(stash["entryJson"].as_str().ok_or_else(invalid)?)
                    .map_err(|_| invalid())?;
            if json!(digest(&entry)) != stash["sha256"] {
                return Err(invalid());
            }
        }
        Ok((auth, Some(stash)))
    }
    fn ensure_parent(&self, path: &Path, private: bool) -> Result<(), Error> {
        let parent = path.parent().unwrap_or(path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
            if private {
                let stat = fs::symlink_metadata(parent)?;
                if !stat.is_dir() || stat.permissions().mode() & 0o777 != 0o700 {
                    return Err(Error::Slot("unsafe-path"));
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = private;
            super::private::windows_store::ensure_directory(parent)?;
        }
        Ok(())
    }
    fn sync_parent(&self, path: &Path) -> Result<(), Error> {
        #[cfg(unix)]
        fs::File::open(path.parent().unwrap())?.sync_all()?;
        #[cfg(windows)]
        let _ = path;
        Ok(())
    }
    fn write(&self, path: &Path, text: &str) -> Result<(), Error> {
        #[cfg(unix)]
        {
            use std::{
                io::Write,
                os::unix::fs::{OpenOptionsExt, PermissionsExt},
            };
            fs::create_dir_all(path.parent().unwrap())?;
            let mut stage = path.as_os_str().to_os_string();
            stage.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
            let stage = PathBuf::from(stage);
            let mut created = false;
            let result = (|| {
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&stage)?;
                created = true;
                file.write_all(text.as_bytes())?;
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
                file.sync_all()?;
                drop(file);
                self.step(Step::BeforeRename, path)?;
                fs::rename(&stage, path)?;
                created = false;
                self.sync_parent(path)
            })();
            if created {
                let _ = fs::remove_file(&stage);
            }
            result
        }
        #[cfg(windows)]
        {
            super::private::windows_store::write_atomic(path, text.as_bytes(), None, || {
                self.step(Step::BeforeRename, path)
                    .map_err(|error| Failure::Storage(error.to_string()))
            })
            .map_err(Error::from)
        }
    }
    pub fn inspect(&self) -> Result<Value, Error> {
        let (auth, stash) = self.load()?;
        let slot = if digest(&auth.entry) == digest(&self.placeholder) {
            "placeholder"
        } else if auth.entry.is_null() {
            "empty"
        } else if stash
            .as_ref()
            .map_or(true, |stash| stash["sha256"] == digest(&auth.entry))
        {
            "original"
        } else {
            "foreign"
        };
        Ok(json!({"slot":slot,"stash":if stash.is_some() {"valid"} else {"missing"}}))
    }
    pub fn enter(&self) -> Result<(), Error> {
        let (auth, stash) = self.load()?;
        if digest(&auth.entry) == digest(&self.placeholder) {
            return if stash.is_some() {
                Ok(())
            } else {
                Err(Error::Slot("missing-stash"))
            };
        }
        if stash
            .as_ref()
            .is_some_and(|stash| stash["sha256"] != digest(&auth.entry))
        {
            return Err(Error::Slot("conflict"));
        }
        if stash.is_none() {
            self.ensure_parent(&self.stash, true)?;
            let saved = json!({"version":1,"provider":self.provider,"authIdentity":self.auth_identity,"placeholderIdentity":self.placeholder_identity,"entry":auth.entry,"entryJson":auth.entry_text(),"sha256":digest(&auth.entry)});
            self.write(&self.stash, &crate::json::describe(&saved))?;
            self.step(Step::StashWritten, &self.stash)?;
        }
        self.write(
            &self.auth,
            &auth.replace(
                &self.provider,
                Some(&crate::json::describe(&self.placeholder)),
            ),
        )?;
        self.step(Step::AuthWritten, &self.auth)
    }
    pub fn exit(&self) -> Result<&'static str, Error> {
        let (auth, stash) = self.load()?;
        let placeholder = digest(&auth.entry) == digest(&self.placeholder);
        let Some(stash) = stash else {
            return if placeholder {
                Err(Error::Slot("missing-stash"))
            } else {
                Ok("nothing-to-do")
            };
        };
        if placeholder {
            self.write(
                &self.auth,
                &auth.replace(&self.provider, stash["entryJson"].as_str()),
            )?;
            self.step(Step::AuthWritten, &self.auth)?;
        } else if stash["sha256"] != digest(&auth.entry) {
            return Ok("conflict");
        }
        fs::remove_file(&self.stash)?;
        self.sync_parent(&self.stash)?;
        self.step(Step::StashDeleted, &self.stash)?;
        Ok("restored")
    }
}

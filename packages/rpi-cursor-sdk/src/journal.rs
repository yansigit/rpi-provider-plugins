//! Byte-preserving host journal IO. The retained adapter owns record validation.
use fs2::FileExt;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub mod port;

pub const READ_CHUNK_BYTES: usize = 1024 * 1024;

pub struct Journal {
    path: PathBuf,
}

pub struct Snapshot {
    file: Option<File>,
    path: PathBuf,
    size: u64,
    offset: u64,
    hash: Sha256,
    finished: bool,
}

pub struct Chunk {
    pub bytes: Vec<u8>,
    /// Present only after the whole, locked snapshot has been read.
    pub digest: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AppendResult {
    Conflict,
    Committed { size: u64 },
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn options(write: bool) -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

fn regular(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(invalid("journal path is not a regular file"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(invalid("journal path is a reparse point"));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // The handle remains open through the call and the API initializes this fixed structure.
    let information = unsafe {
        if GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) == 0 {
            return Err(io::Error::last_os_error());
        }
        information.assume_init()
    };
    Ok((
        information.dwVolumeSerialNumber as u64,
        ((information.nFileIndexHigh as u64) << 32) | information.nFileIndexLow as u64,
    ))
}

fn verify(path: &Path, file: &File) -> io::Result<()> {
    regular(path)?;
    let current = options(false).open(path)?;
    if !file.metadata()?.is_file() || identity(file)? != identity(&current)? {
        return Err(invalid("journal path changed while open"));
    }
    Ok(())
}

fn open(path: &Path, write: bool) -> io::Result<File> {
    regular(path)?;
    let file = options(write).open(path)?;
    verify(path, &file)?;
    Ok(file)
}

impl Journal {
    /// Only the native owner resolves this path; IPC callers supply session handles, never paths.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The shared lock is retained through the last chunk (or dropping the snapshot).
    pub fn snapshot(&self, limit: Option<u64>) -> io::Result<Snapshot> {
        let file = match open(&self.path, false) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let size = if let Some(file) = &file {
            FileExt::lock_shared(file)?;
            verify(&self.path, file)?;
            file.metadata()?.len()
        } else {
            0
        };
        if limit.is_some_and(|limit| size > limit) {
            return Err(invalid("journal exceeds size limit"));
        }
        Ok(Snapshot {
            file,
            path: self.path.clone(),
            size,
            offset: 0,
            hash: Sha256::new(),
            finished: false,
        })
    }

    /// A conflict performs no write. Once writing starts, errors mean the outcome needs inspection.
    pub fn append(
        &self,
        frame: &[u8],
        expected_digest: Option<&str>,
        limit: Option<u64>,
        repair_boundary: bool,
    ) -> io::Result<AppendResult> {
        self.append_with_sync(frame, expected_digest, limit, repair_boundary, |file, _| {
            file.sync_all()
        })
    }

    fn append_with_sync(
        &self,
        frame: &[u8],
        expected_digest: Option<&str>,
        limit: Option<u64>,
        repair_boundary: bool,
        mut sync: impl FnMut(&File, bool) -> io::Result<()>,
    ) -> io::Result<AppendResult> {
        if frame.is_empty() || !frame.ends_with(b"\n") {
            return Err(invalid("journal append is not a complete frame"));
        }
        if expected_digest.is_some_and(|digest| {
            digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return Err(invalid("invalid journal snapshot digest"));
        }
        let mut file = match options(true).create_new(true).open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open(&self.path, true)?,
            Err(error) => return Err(error),
        };
        FileExt::lock_exclusive(&file)?;
        verify(&self.path, &file)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        let size = file.metadata()?.len();
        if limit.is_some_and(|limit| size > limit) {
            return Err(invalid("journal exceeds size limit"));
        }
        if let Some(expected) = expected_digest {
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            let mut observed = 0u64;
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                observed += count as u64;
                if observed > size {
                    return Err(invalid("journal changed while validating"));
                }
                hash.update(&buffer[..count]);
            }
            if observed != size || file.metadata()?.len() != size {
                return Err(invalid("journal changed while validating"));
            }
            if format!("{:x}", hash.finalize()) != expected {
                return Ok(AppendResult::Conflict);
            }
        }
        let mut boundary = false;
        if size > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            boundary = last[0] != b'\n';
            if boundary && !repair_boundary {
                return Err(invalid("incomplete journal frame"));
            }
        }
        let final_size = size
            .checked_add(frame.len() as u64)
            .and_then(|size| size.checked_add(u64::from(boundary)))
            .ok_or_else(|| invalid("journal size overflow"))?;
        if limit.is_some_and(|limit| final_size > limit) {
            return Err(invalid("journal exceeds size limit"));
        }
        file.seek(SeekFrom::End(0))?;
        if boundary {
            file.write_all(b"\n")?;
        }
        file.write_all(frame)?;
        sync(&file, false)?;
        #[cfg(unix)]
        {
            // Sync on every acknowledgement, including recovery after an earlier directory fsync failure.
            let directory = File::open(
                self.path
                    .parent()
                    .ok_or_else(|| invalid("journal has no parent"))?,
            )?;
            sync(&directory, true)?;
        }
        verify(&self.path, &file)?;
        if file.metadata()?.len() != final_size {
            return Err(invalid("journal changed during append"));
        }
        Ok(AppendResult::Committed { size: final_size })
    }
}

impl Snapshot {
    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn next_chunk(&mut self) -> io::Result<Chunk> {
        if self.finished {
            return Err(invalid("journal snapshot is already consumed"));
        }
        let count = (self.size - self.offset).min(READ_CHUNK_BYTES as u64) as usize;
        let mut bytes = vec![0; count];
        if let Some(file) = &mut self.file {
            file.read_exact(&mut bytes)?;
        }
        self.hash.update(&bytes);
        self.offset += count as u64;
        let digest = if self.offset == self.size {
            if let Some(file) = &self.file {
                verify(&self.path, file)?;
                if file.metadata()?.len() != self.size {
                    return Err(invalid("journal changed while reading"));
                }
            }
            self.finished = true;
            self.file.take();
            Some(format!("{:x}", self.hash.clone().finalize()))
        } else {
            None
        };
        Ok(Chunk { bytes, digest })
    }
}

#[cfg(test)]
mod tests;

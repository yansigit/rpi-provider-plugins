use super::*;
use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let mut random = [0; 16];
        getrandom::getrandom(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "rpi-cursor-journal-{:x}",
            u128::from_ne_bytes(random)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn journal(&self) -> Journal {
        Journal::new(self.0.join("journal"))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn read(journal: &Journal) -> (Vec<u8>, String) {
    let mut snapshot = journal.snapshot(None).unwrap();
    let mut bytes = vec![];
    loop {
        let chunk = snapshot.next_chunk().unwrap();
        bytes.extend(chunk.bytes);
        if let Some(digest) = chunk.digest {
            return (bytes, digest);
        }
    }
}

#[test]
fn bytes_boundaries_limits_and_private_mode() {
    let directory = Directory::new();
    let journal = directory.journal();
    let (bytes, empty) = read(&journal);
    assert!(bytes.is_empty());
    assert!(!journal.path.exists());
    let first = "{\"unicode\":\"é😀\"}\n".as_bytes();
    assert_eq!(
        journal.append(first, Some(&empty), None, false).unwrap(),
        AppendResult::Committed {
            size: first.len() as u64
        }
    );
    assert_eq!(read(&journal).0, first);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&journal.path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(
        journal.append(b"{}\n", Some(&empty), None, false).unwrap(),
        AppendResult::Conflict
    );
    assert_eq!(read(&journal).0, first);
    assert!(journal.snapshot(Some(1)).is_err());
    assert!(journal
        .append(b"{}\n", None, Some(first.len() as u64 + 2), false)
        .is_err());
    assert_eq!(read(&journal).0, first);
    fs::write(&journal.path, b"{\"torn\":").unwrap();
    assert!(journal.append(b"{}\n", None, None, false).is_err());
    assert_eq!(read(&journal).0, b"{\"torn\":");
    journal.append(b"{}\n", None, None, true).unwrap();
    assert_eq!(read(&journal).0, b"{\"torn\":\n{}\n");
}

#[test]
fn failed_sync_never_acknowledges_and_does_not_erase_uncertain_bytes() {
    for fail_directory in [false, true] {
        if fail_directory && cfg!(windows) {
            continue;
        }
        let directory = Directory::new();
        let journal = directory.journal();
        let mut calls = vec![];
        let result = journal.append_with_sync(
            b"{\"intent\":true}\n",
            None,
            None,
            false,
            |_, is_directory| {
                calls.push(is_directory);
                if is_directory == fail_directory {
                    Err(io::Error::new(
                        io::ErrorKind::Other,
                        "injected sync failure",
                    ))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_eq!(
            calls,
            if fail_directory {
                vec![false, true]
            } else {
                vec![false]
            }
        );
        assert_eq!(read(&journal).0, b"{\"intent\":true}\n");
        journal
            .append(b"{\"result\":true}\n", None, None, false)
            .unwrap();
        assert_eq!(read(&journal).0, b"{\"intent\":true}\n{\"result\":true}\n");
    }
}

#[test]
fn bounded_snapshot_retains_its_lock_until_consumed_or_dropped() {
    let directory = Directory::new();
    let journal = directory.journal();
    let bytes = [vec![b'x'; READ_CHUNK_BYTES * 2 + 7], b"\n".to_vec()].concat();
    journal.append(&bytes, None, None, false).unwrap();
    let mut snapshot = journal.snapshot(None).unwrap();
    assert_eq!(snapshot.size(), bytes.len() as u64);
    let chunk = snapshot.next_chunk().unwrap();
    assert_eq!(chunk.bytes.len(), READ_CHUNK_BYTES);
    assert!(chunk.digest.is_none());
    let path = journal.path.clone();
    let (started, ready) = mpsc::channel();
    let (completed, result) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        started.send(()).unwrap();
        completed
            .send(Journal::new(path).append(b"{}\n", None, None, false))
            .unwrap();
    });
    ready.recv().unwrap();
    assert!(result.recv_timeout(Duration::from_millis(30)).is_err());
    drop(snapshot);
    result
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    let (all, digest) = read(&journal);
    assert_eq!(all, [bytes, b"{}\n".to_vec()].concat());
    assert_eq!(digest, format!("{:x}", Sha256::digest(&all)));
}

#[test]
fn competing_validated_writers_retry_conflicts_without_losing_frames() {
    let directory = Directory::new();
    let path = directory.journal().path;
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|id| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let journal = Journal::new(path);
                barrier.wait();
                loop {
                    let (bytes, digest) = read(&journal);
                    let frames: Vec<_> = bytes
                        .split(|byte| *byte == b'\n')
                        .filter(|line| !line.is_empty())
                        .collect();
                    assert!(frames
                        .iter()
                        .all(|line| serde_json::from_slice::<serde_json::Value>(line).is_ok()));
                    let frame = format!("{{\"id\":{id},\"previous\":{}}}\n", frames.len());
                    if matches!(
                        journal
                            .append(frame.as_bytes(), Some(&digest), None, false)
                            .unwrap(),
                        AppendResult::Committed { .. }
                    ) {
                        break;
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let (bytes, _) = read(&Journal::new(path));
    let frames: Vec<serde_json::Value> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 8);
    let ids: std::collections::HashSet<_> = frames
        .iter()
        .map(|frame| frame["id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), 8);
    for (i, frame) in frames.iter().enumerate() {
        assert_eq!(frame["previous"], i);
    }
}

#[cfg(unix)]
#[test]
fn rejects_symlinks_and_swapped_open_files_without_touching_the_target() {
    use std::os::unix::fs::symlink;
    let directory = Directory::new();
    let journal = directory.journal();
    let other = directory.0.join("other");
    fs::write(&other, b"private").unwrap();
    symlink(&other, &journal.path).unwrap();
    assert!(journal.snapshot(None).is_err());
    assert!(journal.append(b"{}\n", None, None, false).is_err());
    assert_eq!(fs::read(&other).unwrap(), b"private");
    fs::remove_file(&journal.path).unwrap();
    journal.append(b"{}\n", None, None, false).unwrap();
    let mut snapshot = journal.snapshot(None).unwrap();
    fs::rename(&journal.path, directory.0.join("original")).unwrap();
    fs::write(&journal.path, b"replacement\n").unwrap();
    assert!(snapshot.next_chunk().is_err());
}

#[test]
fn journal_port_bounds_scopes_and_releases_abandoned_readers_on_shutdown() {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::json;
    let directory = Directory::new();
    let port = Arc::new(
        port::JournalPort::for_session(&directory.0.join("sessions.sqlite"), "one").unwrap(),
    );
    let other =
        port::JournalPort::for_session(&directory.0.join("sessions.sqlite"), "two").unwrap();
    let append = json!({"action":"append","kind":"cloud","bytes": STANDARD.encode(b"{}\n")});
    assert_eq!(
        port.request(append.clone()).unwrap()["durability"],
        "durable"
    );
    let opened = port
        .request(json!({"action":"open","kind":"cloud"}))
        .unwrap();
    assert!(other
        .request(json!({"action":"next","reader":opened["reader"]}))
        .is_err());
    assert!(port
        .request(json!({"action":"open","kind":"cloud","path":"/foreign"}))
        .is_err());
    let (ready, waiting) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let writer_port = Arc::clone(&port);
    let writer = std::thread::spawn(move || {
        ready.send(()).unwrap();
        done.send(writer_port.request(append)).unwrap();
    });
    waiting.recv().unwrap();
    assert!(completed.recv_timeout(Duration::from_millis(30)).is_err());
    port.close();
    completed
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    writer.join().unwrap();
    assert!(port
        .request(json!({"action":"open","kind":"cloud"}))
        .is_err());
    let opened = other
        .request(json!({"action":"open","kind":"cloud"}))
        .unwrap();
    assert_eq!(opened["size"], 0);
    other.close();
}

#[test]
fn cloud_authority_ignores_non_journal_siblings_and_refuses_tail_segments() {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let directory = Directory::new();
    let session = directory.0.join("sessions.sqlite");
    fs::write(&session, b"").unwrap();
    let port = port::JournalPort::for_native_session(&session, "one").unwrap();
    let open = json!({"action":"open","kind":"cloud"});
    let hash = format!("{:x}", Sha256::digest(b"one"));
    let name = format!(".cursor-cloud-lifecycle-{}.journal", &hash[..32]);
    // Backup, staging and prefix-sharing copies are not cloud authority: they are
    // never opened, and the locked journal's own frames are validated instead.
    let siblings = [".bak", ".backup", ".tmp", ".retired", ".partial"];
    for suffix in siblings {
        fs::write(directory.0.join(format!("{name}{suffix}")), b"{}\n").unwrap();
    }
    fs::write(
        directory
            .0
            .join(format!(".cursor-cloud-lifecycle-{}.journalextra", &hash[..32])),
        b"{}\n",
    )
    .unwrap();
    assert_eq!(port.request(open.clone()).unwrap()["size"], 0);
    for suffix in siblings {
        fs::remove_file(directory.0.join(format!("{name}{suffix}"))).unwrap();
    }
    // A rotated tail segment of the same journal is refused with its own message.
    fs::write(directory.0.join(format!("{name}.1")), b"{}\n").unwrap();
    assert_eq!(
        port.request(open).unwrap_err(),
        "native cloud journal has a superseded tail segment"
    );
    port.close();
}

#[test]
#[ignore = "internal child of the cross-process lock test"]
fn process_writer_child() {
    let Some(path) = std::env::var_os("RPI_JOURNAL_TEST_PATH") else {
        return;
    };
    let journal = Journal::new(path.into());
    let mut snapshot = read(&journal);
    println!("RPI_JOURNAL_READY");
    std::io::stdout().flush().unwrap();
    std::io::stdin().read_exact(&mut [0]).unwrap();
    for _ in 0..16 {
        loop {
            let count = snapshot
                .0
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .count();
            let frame = format!("{{\"sequence\":{count}}}\n");
            if matches!(
                journal
                    .append(frame.as_bytes(), Some(&snapshot.1), None, false)
                    .unwrap(),
                AppendResult::Committed { .. }
            ) {
                snapshot = read(&journal);
                break;
            }
            snapshot = read(&journal);
        }
    }
}

#[test]
fn separate_processes_serialize_validated_appends() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let directory = Directory::new();
    let journal = directory.journal();
    let mut children: Vec<_> = (0..4)
        .map(|_| {
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "journal::tests::process_writer_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("RPI_JOURNAL_TEST_PATH", &journal.path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in &mut children {
        let mut output = BufReader::new(child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert_ne!(
                output.read_line(&mut line).unwrap(),
                0,
                "journal child exited before ready"
            );
            if line.contains("RPI_JOURNAL_READY") {
                break;
            }
        }
        // Keep stdout open: the Rust test harness still prints its completion report.
        child.stdout = Some(output.into_inner());
    }
    for child in &mut children {
        child.stdin.take().unwrap().write_all(b"!").unwrap();
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let (bytes, _) = read(&journal);
    let rows: Vec<serde_json::Value> = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 64);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row["sequence"], i);
    }
}

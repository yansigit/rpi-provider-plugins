//! One session's journal port. Session ownership is checked before dispatch by the native caller.
use super::{AppendResult, Journal, Snapshot};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

const MAX_READERS: usize = 16;
const USAGE_LIMIT: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Kind {
    Cloud,
    Usage,
}

#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Request {
    Open {
        kind: Kind,
    },
    Next {
        reader: String,
    },
    Close {
        reader: String,
    },
    Append {
        kind: Kind,
        bytes: String,
        expected_digest: Option<String>,
    },
}

pub struct JournalPort {
    cloud: Journal,
    usage: Journal,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    closed: bool,
    readers: HashMap<String, Arc<Mutex<Snapshot>>>,
}

impl JournalPort {
    /// Inputs come from the captured host storage, never from a worker path or model descriptor.
    pub fn for_session(session_file: &Path, session_id: &str) -> Result<Self, String> {
        if !session_file.is_absolute() || session_id.is_empty() {
            return Err("invalid native journal origin".into());
        }
        let directory = session_file
            .parent()
            .ok_or("native journal origin has no directory")?;
        let hash = format!("{:x}", Sha256::digest(session_id.as_bytes()));
        let hash = &hash[..32];
        Ok(Self {
            cloud: Journal::new(directory.join(format!(".cursor-cloud-lifecycle-{hash}.journal"))),
            usage: Journal::new(directory.join(format!("cursor-usage-{hash}.journal"))),
            state: Mutex::new(State::default()),
        })
    }

    /// Run on a blocking host task; cancellation must not drop an admitted durability operation.
    pub fn request(&self, value: Value) -> Result<Value, String> {
        if self.state.lock().unwrap().closed {
            return Err("native journal port is closed".into());
        }
        let request: Request =
            serde_json::from_value(value).map_err(|_| "invalid native journal request")?;
        match request {
            Request::Open { kind } => {
                if self.state.lock().unwrap().readers.len() >= MAX_READERS {
                    return Err("native journal reader limit reached".into());
                }
                let snapshot = self
                    .journal(kind)
                    .snapshot(Self::limit(kind))
                    .map_err(io_error)?;
                let size = snapshot.size();
                if size > 9_007_199_254_740_991 {
                    return Err("native journal size exceeds the worker integer range".into());
                }
                let mut random = [0; 16];
                getrandom::getrandom(&mut random)
                    .map_err(|_| "native journal reader entropy unavailable")?;
                let id = format!("{:032x}", u128::from_ne_bytes(random));
                let mut state = self.state.lock().unwrap();
                if state.closed {
                    return Err("native journal port is closed".into());
                }
                let readers = &mut state.readers;
                if readers.len() >= MAX_READERS {
                    return Err("native journal reader limit reached".into());
                }
                readers.insert(id.clone(), Arc::new(Mutex::new(snapshot)));
                Ok(json!({"reader":id, "size":size}))
            }
            Request::Next { reader } => {
                let snapshot = self
                    .state
                    .lock()
                    .unwrap()
                    .readers
                    .get(&reader)
                    .cloned()
                    .ok_or("unknown native journal reader")?;
                let result = snapshot.lock().unwrap().next_chunk();
                if result.as_ref().map_or(true, |chunk| chunk.digest.is_some()) {
                    self.state.lock().unwrap().readers.remove(&reader);
                }
                let chunk = result.map_err(io_error)?;
                Ok(json!({"bytes": STANDARD.encode(chunk.bytes), "digest":chunk.digest}))
            }
            Request::Close { reader } => {
                self.state.lock().unwrap().readers.remove(&reader);
                Ok(Value::Null)
            }
            Request::Append {
                kind,
                bytes,
                expected_digest,
            } => {
                if matches!(kind, Kind::Usage) != expected_digest.is_some() {
                    return Err("invalid native journal append contract".into());
                }
                let bytes = STANDARD
                    .decode(bytes)
                    .map_err(|_| "invalid native journal frame bytes")?;
                let result = self
                    .journal(kind)
                    .append(
                        &bytes,
                        expected_digest.as_deref(),
                        Self::limit(kind),
                        matches!(kind, Kind::Cloud),
                    )
                    .map_err(io_error)?;
                Ok(match result {
                    AppendResult::Conflict => json!({"committed":false}),
                    AppendResult::Committed { size } => {
                        json!({"committed":true,"size":size,"durability":"durable"})
                    }
                })
            }
        }
    }

    /// Call before draining admitted host tasks on worker shutdown; this releases abandoned read locks.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        state.readers.clear();
    }

    fn journal(&self, kind: Kind) -> &Journal {
        match kind {
            Kind::Cloud => &self.cloud,
            Kind::Usage => &self.usage,
        }
    }
    fn limit(kind: Kind) -> Option<u64> {
        match kind {
            Kind::Cloud => None,
            Kind::Usage => Some(USAGE_LIMIT),
        }
    }
}

fn io_error(error: std::io::Error) -> String {
    format!("native journal IO failed: {error}; inspect the journal before retrying a mutation")
}

//! Imported history cannot activate Cursor management authority (ledger I02).
//!
//! The host importer copies Pi sessions into `<agent>/sessions/pi-import/` and
//! adds one header field (see the host `sessions.rs` module):
//!
//! ```json
//! "imported": {"source":"pi","sourcePath":"/home/u/.pi/agent/sessions/x.jsonl",
//!              "sha256":"<64 hex>","sessionVersion":3}
//! ```
//!
//! Such a snapshot is an archive. It has no host receipts, no acknowledged
//! entries and no Cursor SDK ownership, so resume, cleanup and cloud authority
//! must not be derived from it: those operations mutate durable Cursor state by
//! agent ID. The same applies when the marker is missing but the file still
//! lives in `pi-import/`, when the file is a Pi (session version 3 or older)
//! snapshot, when an entry carries its own origin marker, and when identifiers
//! or durable intent records cannot be trusted.
//!
//! A refused resume starts a fresh Cursor agent and reports
//! [`FRESH_AGENT_NOTICE`]; nothing here creates, deletes, archives or resumes
//! an agent. Native session projections and cloud journal reads call this module
//! before exposing management records to the retained adapter.
//!
//! Identifier rules are the pinned upstream ones: Pi's `assertValidSessionId`
//! for session and entry IDs, `isCursorLocalAgentId` for resume/cleanup agent
//! IDs and `CLOUD_AGENT_ID_PATTERN` for cloud agent IDs. `tests/i02*` compares
//! these predicates with the unchanged upstream code.

use serde_json::{json, Value};
use std::path::{Component, Path};

/// Directory the host importer writes archival Pi snapshots into.
pub const IMPORT_DIR: &str = "pi-import";
/// Resume entry custom type.
pub const RESUME_ENTRY: &str = "cursor-sdk-agent-resume";
/// Cleanup entry custom type.
pub const CLEANUP_ENTRY: &str = "cursor-sdk-agent-cleanup";
/// Cloud lifecycle entry custom type.
pub const CLOUD_ENTRY: &str = "cursor-cloud-lifecycle";
/// Current resume entry version; version 1 is the upstream legacy shape.
pub const RESUME_ENTRY_VERSION: u64 = 2;
/// Oldest resume entry version the upstream parser still reads.
pub const LEGACY_RESUME_ENTRY_VERSION: u64 = 1;
/// Current durable cloud lifecycle record version.
pub const CUSTODY_RECORD_VERSION: u64 = 1;
/// `rpi` session format; Pi snapshots are version 3 and older.
pub const NATIVE_SESSION_VERSION: u64 = 4;
/// Newest Pi session version the host importer copies.
pub const MAX_PI_SESSION_VERSION: u64 = 3;

/// Shown instead of a Cursor continuation when a session cannot be authorized.
pub const FRESH_AGENT_NOTICE: &str = "Imported Pi history cannot resume a Cursor agent. This conversation continues with a fresh Cursor agent; earlier Cursor branch continuity, cleanup and cloud records are unavailable.";

/// One management operation the retained adapter would like to perform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Authority {
    Resume,
    Cleanup,
    Cloud,
}

impl Authority {
    pub fn name(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Cleanup => "cleanup",
            Self::Cloud => "cloud",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "resume" => Some(Self::Resume),
            "cleanup" => Some(Self::Cleanup),
            "cloud" => Some(Self::Cloud),
            _ => None,
        }
    }
}

/// Where the session under evaluation came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Native,
    Imported,
    Unknown,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Imported => "imported",
            Self::Unknown => "unknown",
        }
    }
}

/// A file that would be read by cloud authority: a durable lifecycle journal
/// frame set, or a staging/backup sibling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Artifact {
    pub name: String,
    pub text: String,
}

impl Artifact {
    pub fn new(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            text: text.into(),
        }
    }
}

/// A pending durable mutation that must be reconciled before cloud or cleanup
/// authority is granted again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending {
    pub agent_id: String,
    pub action: String,
}

/// The complete answer the adapter acts on. Serialization is the test contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub authority: Authority,
    pub allowed: bool,
    pub code: Option<&'static str>,
    pub origin: Origin,
    pub fresh_agent: bool,
    pub notice: Option<&'static str>,
    pub agent_id: Option<String>,
    pub session_id: Option<String>,
    pub pending: Option<Pending>,
}

impl Decision {
    fn refuse(authority: Authority, origin: Origin, code: &'static str, fresh: bool) -> Self {
        Self {
            authority,
            allowed: false,
            code: Some(code),
            origin,
            fresh_agent: fresh,
            notice: if fresh {
                Some(FRESH_AGENT_NOTICE)
            } else {
                None
            },
            agent_id: None,
            session_id: None,
            pending: None,
        }
    }

    fn allow(authority: Authority, origin: Origin, session_id: String) -> Self {
        Self {
            authority,
            allowed: true,
            code: None,
            origin,
            fresh_agent: false,
            notice: None,
            agent_id: None,
            session_id: Some(session_id),
            pending: None,
        }
    }

    fn session(mut self, session_id: Option<&str>) -> Self {
        self.session_id = session_id.map(str::to_owned);
        self
    }

    fn agent(mut self, agent_id: Option<String>) -> Self {
        self.agent_id = agent_id;
        self
    }

    fn pending(mut self, pending: Pending) -> Self {
        self.pending = Some(pending);
        self
    }

    pub fn to_json(&self) -> Value {
        json!({
            "authority": self.authority.name(),
            "allowed": self.allowed,
            "code": self.code,
            "origin": self.origin.name(),
            "freshAgent": self.fresh_agent,
            "notice": self.notice,
            "agentId": self.agent_id,
            "sessionId": self.session_id,
            "pending": self.pending.as_ref().map(|pending| json!({
                "agentId": pending.agent_id,
                "action": pending.action,
            })),
        })
    }
}

/// Pi's `assertValidSessionId`: non-empty ASCII alphanumerics, `-`, `_`, `.`,
/// starting and ending with an alphanumeric. This rejects `.`, `..`, absolute
/// paths, separators, percent-escapes and control characters.
pub fn valid_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    match (bytes.first(), bytes.last()) {
        (Some(first), Some(last))
            if first.is_ascii_alphanumeric() && last.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Upstream `isCursorLocalAgentId`: `agent-` plus URL-safe local characters.
pub fn valid_local_agent_id(value: &str) -> bool {
    value.len() <= 256
        && value.starts_with("agent-")
        && value.len() > "agent-".len()
        && value["agent-".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// Upstream `CLOUD_AGENT_ID_PATTERN` (case-insensitive).
pub fn valid_cloud_agent_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 39 || !bytes[..3].eq_ignore_ascii_case(b"bc-") {
        return false;
    }
    let hex = |slice: &[u8]| slice.iter().all(u8::is_ascii_hexdigit);
    let groups = [3..11, 12..16, 17..21, 22..26, 27..39];
    if !groups.iter().all(|range| hex(&bytes[range.clone()])) {
        return false;
    }
    [11, 16, 21, 26].iter().all(|index| bytes[*index] == b'-')
}

fn bounded(value: &Value, maximum: usize) -> Option<&str> {
    let text = value.as_str()?;
    if text.is_empty() || text.len() > maximum {
        return None;
    }
    Some(text)
}

fn object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

fn marker(value: &Value) -> Result<(), &'static str> {
    let fields = object(value).ok_or("imported-marker-invalid")?;
    if fields.get("source").and_then(Value::as_str) != Some("pi") {
        return Err("imported-origin-forged");
    }
    let source_path = fields
        .get("sourcePath")
        .and_then(Value::as_str)
        .ok_or("imported-marker-invalid")?;
    if !safe_source_path(source_path) {
        return Err("unsafe-source-path");
    }
    let sha = fields
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or("imported-marker-invalid")?;
    if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("imported-marker-invalid");
    }
    let version = fields
        .get("sessionVersion")
        .and_then(Value::as_u64)
        .ok_or("imported-marker-invalid")?;
    if !(1..=MAX_PI_SESSION_VERSION).contains(&version) {
        return Err("unsupported-imported-version");
    }
    Ok(())
}

/// An absolute path with no parent component, no NUL and a `.jsonl` file name.
/// Accepts both separators so a marker written on Windows is still recognized
/// as an import marker (and therefore still refused as an origin).
fn safe_source_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') {
        return false;
    }
    let normalized = path.replace('\\', "/");
    let bytes = normalized.as_bytes();
    let absolute =
        normalized.starts_with('/') || (bytes.len() > 2 && bytes[1] == b':' && bytes[2] == b'/');
    if !absolute || !normalized.ends_with(".jsonl") {
        return false;
    }
    !normalized.split('/').any(|component| component == "..")
}

/// Any origin marker, including malformed/forged markers, removes authority.
pub fn has_import_origin(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.contains_key("imported")
                || fields.contains_key("importedFrom")
                || fields.get("origin").and_then(Value::as_str) == Some("imported")
                || fields.values().any(has_import_origin)
        }
        Value::Array(values) => values.iter().any(has_import_origin),
        _ => false,
    }
}

/// Admission for a typed native snapshot. Keep the tree and historical data,
/// but remove management custom types that could otherwise confer authority.
/// Native snapshots have no JSONL header: only this trusted adapter supplies v4.
pub fn guard_native_view(path: &Path, view: &mut Value) -> bool {
    let metadata = view["snapshot"]["metadata"].clone();
    let header = json!({"type":"session", "version":NATIVE_SESSION_VERSION, "id":metadata["id"]});
    let denied_origin = imported_location(path)
        || !metadata["id"].as_str().map_or(false, valid_session_id)
        || has_import_origin(&metadata)
        || view["snapshot"].get("imported").is_some()
        || view["snapshot"].get("importedFrom").is_some()
        || view["snapshot"]["origin"] == "imported"
        || metadata.get("version").map_or(false, |version| {
            version.as_u64() != Some(NATIVE_SESSION_VERSION)
        });
    let mut refused = denied_origin;
    let mut guard = |entry: &mut Value| {
        let kind = entry["customType"].as_str().unwrap_or_default();
        if !matches!(kind, RESUME_ENTRY | CLEANUP_ENTRY | CLOUD_ENTRY) {
            return;
        }
        let data = &entry["data"];
        let mut allowed = !denied_origin && !has_import_origin(entry) && entry_safe(entry);
        allowed &= match kind {
            RESUME_ENTRY => {
                let text = format!("{header}\n{entry}\n");
                evaluate(path, &text, Authority::Resume, &[]).allowed && local_candidates_safe(data)
            }
            CLEANUP_ENTRY => accepted_cleanup_entry(data).is_some() && local_candidates_safe(data),
            CLOUD_ENTRY => accepted_cloud_entry(data).is_some(),
            _ => false,
        };
        if !allowed {
            entry["customType"] = json!("rpi-refused-cursor-authority");
            refused = true;
        }
    };
    if let Some(entries) = view["snapshot"]["entries"].as_array_mut() {
        for entry in entries {
            guard(entry);
        }
    }
    if let Some(entries) = view["projection"]["entries"].as_array_mut() {
        for entry in entries {
            guard(&mut entry["sourceEntry"]);
        }
    }
    refused
}

fn local_candidates_safe(data: &Value) -> bool {
    [
        "cleanupCandidateAgentIds",
        "candidateAgentIds",
        "protectedAgentIds",
        "deletedAgentIds",
    ]
    .iter()
    .all(|key| {
        data.get(key).map_or(true, |ids| {
            ids.as_array().map_or(false, |ids| {
                ids.iter()
                    .all(|id| id.as_str().map_or(false, valid_local_agent_id))
            })
        })
    }) && ["cleanupCandidates", "failedAgentIds"].iter().all(|key| {
        data.get(key).map_or(true, |ids| {
            ids.as_array().map_or(false, |ids| {
                ids.iter()
                    .all(|id| id["agentId"].as_str().map_or(false, valid_local_agent_id))
            })
        })
    })
}

/// Validate the exact locked cloud journal frame before it reaches the worker.
/// Pending native intents remain readable for the existing reconciliation path.
pub fn native_cloud_frame_allowed(path: &Path, session_id: &str, frame: &Value) -> bool {
    !imported_location(path)
        && valid_session_id(session_id)
        && !has_import_origin(frame)
        && frame["version"].as_u64() == Some(CUSTODY_RECORD_VERSION)
        && frame["sessionId"].as_str() == Some(session_id)
        && frame["sessionFile"].as_str() == path.to_str()
        && (frame["anchorEntryId"].is_null()
            || frame["anchorEntryId"]
                .as_str()
                .map_or(false, valid_session_id))
        && accepted_cloud_entry(frame).is_some()
}

fn imported_location(path: &Path) -> bool {
    path.components().any(|component| match component {
        Component::Normal(name) => name.to_string_lossy() == IMPORT_DIR,
        _ => false,
    })
}

struct Session {
    entries: Vec<Value>,
    session_id: String,
}

impl Session {
    fn parse(text: &str) -> Result<Self, (Origin, &'static str)> {
        let mut lines = text.split('\n');
        let first = lines.next().unwrap_or_default().trim_end_matches('\r');
        if first.trim().is_empty() {
            return Err((Origin::Unknown, "invalid-session"));
        }
        let header: Value =
            serde_json::from_str(first).map_err(|_| (Origin::Unknown, "invalid-session"))?;
        if !header.is_object() {
            return Err((Origin::Unknown, "invalid-session"));
        }
        if let Some(value) = header.get("imported") {
            marker(value).map_err(|code| (Origin::Imported, code))?;
            return Err((Origin::Imported, "imported-origin"));
        }
        let origin = Origin::Native;
        if header.get("type").and_then(Value::as_str) != Some("session") {
            return Err((origin, "invalid-session"));
        }
        let version = header
            .get("version")
            .and_then(Value::as_u64)
            .ok_or((origin, "invalid-session"))?;
        if version < NATIVE_SESSION_VERSION {
            return Err((origin, "obsolete-session-version"));
        }
        if version > NATIVE_SESSION_VERSION {
            return Err((origin, "unknown-session-version"));
        }
        let session_id = header
            .get("id")
            .and_then(Value::as_str)
            .ok_or((origin, "unsafe-identifier"))?;
        if !valid_session_id(session_id) {
            return Err((origin, "unsafe-identifier"));
        }
        let session_id = session_id.to_owned();
        let mut entries = Vec::new();
        for line in lines {
            let line = line.trim_end_matches('\r');
            if line.trim().is_empty() {
                continue;
            }
            let entry: Value =
                serde_json::from_str(line).map_err(|_| (origin, "invalid-session"))?;
            if !entry.is_object() {
                return Err((origin, "invalid-session"));
            }
            if !entry_safe(&entry) {
                return Err((origin, "unsafe-identifier"));
            }
            entries.push(entry);
        }
        Ok(Self {
            entries,
            session_id,
        })
    }

    fn resume_handle(&self, path: &Path) -> Result<String, &'static str> {
        let mut handle: Option<String> = None;
        for entry in self.custom(RESUME_ENTRY) {
            let data = entry.get("data").ok_or("invalid-resume-entry")?;
            let resume = accepted_resume_entry(data).ok_or("invalid-resume-entry")?;
            if resume.version == LEGACY_RESUME_ENTRY_VERSION {
                return Err("obsolete-resume-entry");
            }
            if resume.version != RESUME_ENTRY_VERSION {
                return Err("unknown-resume-entry");
            }
            if let Some(session_id) = resume.session_id.as_deref() {
                if session_id != self.session_id {
                    return Err("mismatched-session-identity");
                }
            }
            if let Some(session_file) = resume.session_file.as_deref() {
                if !safe_absolute(session_file) || Path::new(session_file) != path {
                    return Err("mismatched-session-identity");
                }
            }
            handle = Some(resume.agent_id);
        }
        handle.ok_or("missing-resume-entry")
    }

    fn custom<'a>(&'a self, kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
        self.entries.iter().filter(move |entry| {
            entry.get("type").and_then(Value::as_str) == Some("custom")
                && entry.get("customType").and_then(Value::as_str) == Some(kind)
        })
    }
}

fn safe_absolute(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\0')
        && Path::new(path).is_absolute()
        && !Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

/// Entry-level origin and identifier checks shared by every authority.
fn entry_safe(entry: &Value) -> bool {
    // Origin markers are reported as `imported-entry-origin`, not as bad IDs.
    let id = match entry.get("id").and_then(Value::as_str) {
        Some(id) => id,
        None => return false,
    };
    if !valid_session_id(id) {
        return false;
    }
    if let Some(parent) = entry.get("parentId") {
        if !parent.is_null() && !parent.as_str().map_or(false, valid_session_id) {
            return false;
        }
    }
    true
}

/// Upstream `parseCursorSessionAgentResumeEntryData`, reduced to the fields the
/// guard needs. `None` means the upstream parser rejects the data.
pub fn accepted_resume_entry(value: &Value) -> Option<ResumeEntry> {
    let record = object(value)?;
    let version = record.get("version")?.as_u64()?;
    if version != LEGACY_RESUME_ENTRY_VERSION && version != RESUME_ENTRY_VERSION {
        return None;
    }
    if record.get("runtime").and_then(Value::as_str) != Some("local") {
        return None;
    }
    let agent_id = record.get("agentId")?.as_str()?.to_owned();
    if !valid_local_agent_id(&agent_id) {
        return None;
    }
    if !record.get("scopeKey")?.is_string() || !record.get("cwd")?.is_string() {
        return None;
    }
    if !record.get("poolKey")?.is_string()
        || !record.get("branchPathHash")?.is_string()
        || !record.get("createdAt")?.is_string()
    {
        return None;
    }
    record.get("compactionGeneration")?.as_f64()?;
    let send = record.get("sendState")?.as_object()?;
    send.get("bootstrapped")?.as_bool()?;
    send.get("contextFingerprint")?.as_str()?;
    send.get("incrementalSendCount")?.as_f64()?;
    let session_file = match record.get("sessionFile") {
        None => None,
        Some(value) => Some(value.as_str()?.to_owned()),
    };
    let session_id = match record.get("sessionId") {
        None => None,
        Some(value) => Some(value.as_str()?.to_owned()),
    };
    if let Some(value) = record.get("repoRoot") {
        value.as_str()?;
    }
    let store_identity = record.get("storeIdentity").map(store_identity_valid);
    if version == RESUME_ENTRY_VERSION && store_identity != Some(true) {
        return None;
    }
    if let Some(value) = record.get("cleanupCandidateAgentIds") {
        value.as_array()?;
    }
    Some(ResumeEntry {
        version,
        agent_id,
        session_file,
        session_id,
    })
}

fn store_identity_valid(value: &Value) -> bool {
    match value.as_object() {
        Some(record) => {
            record.get("version").and_then(Value::as_u64) == Some(1)
                && record
                    .get("stateRoot")
                    .and_then(Value::as_str)
                    .map_or(false, |root| !root.is_empty())
        }
        None => false,
    }
}

/// Fields the guard reads from an upstream-accepted resume entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeEntry {
    pub version: u64,
    pub agent_id: String,
    pub session_file: Option<String>,
    pub session_id: Option<String>,
}

/// Upstream `parseCleanupEntryData`, reduced.
pub fn accepted_cleanup_entry(value: &Value) -> Option<CleanupEntry> {
    let record = object(value)?;
    let action = record.get("action")?.as_str()?;
    if action != "dry-run" && action != "delete" {
        return None;
    }
    if record.get("runtime").and_then(Value::as_str) != Some("local") {
        return None;
    }
    record.get("timestamp")?.as_str()?;
    let phase = match record.get("phase") {
        None => None,
        Some(value) => {
            let phase = value.as_str()?;
            if phase != "intent" && phase != "result" {
                return None;
            }
            Some(phase.to_owned())
        }
    };
    if action == "dry-run" && phase.is_some() {
        return None;
    }
    let candidates = match record.get("candidateAgentIds") {
        None => vec![],
        Some(value) => value
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
    };
    Some(CleanupEntry {
        action: action.to_owned(),
        phase,
        candidate_agent_ids: candidates,
    })
}

/// Fields the guard reads from an upstream-accepted cleanup entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupEntry {
    pub action: String,
    pub phase: Option<String>,
    pub candidate_agent_ids: Vec<String>,
}

/// Upstream `parseCloudLifecycleEntryData` acceptance, without display
/// sanitization: over-long strings are rejected instead of truncated.
pub fn accepted_cloud_entry(value: &Value) -> Option<CloudEntry> {
    let record = object(value)?;
    let action = record.get("action")?.as_str()?;
    if !matches!(
        action,
        "record" | "archive_intent" | "delete_intent" | "archive" | "delete"
    ) {
        return None;
    }
    if record.get("runtime").and_then(Value::as_str) != Some("cloud") {
        return None;
    }
    let agent_id = record.get("agentId")?.as_str()?;
    if !valid_cloud_agent_id(agent_id) {
        return None;
    }
    let timestamp = bounded(record.get("timestamp")?, 64)?.to_owned();
    if let Some(run_id) = record.get("runId") {
        run_id.as_str()?;
    }
    Some(CloudEntry {
        action: action.to_owned(),
        agent_id: agent_id.to_owned(),
        timestamp,
    })
}

/// Fields the guard reads from an upstream-accepted cloud entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudEntry {
    pub action: String,
    pub agent_id: String,
    pub timestamp: String,
}

/// Upstream `readCursorCloudLifecycleAgents`, reduced to identity plus the
/// unresolved mutation. Order follows first appearance of the agent.
pub fn cloud_agents(entries: &[Value]) -> Vec<CloudAgent> {
    let mut agents: Vec<CloudAgent> = Vec::new();
    let mut mutations: Vec<(String, Option<Mutation>)> = Vec::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("custom")
            || entry.get("customType").and_then(Value::as_str) != Some(CLOUD_ENTRY)
        {
            continue;
        }
        let data = match entry.get("data").and_then(accepted_cloud_entry) {
            Some(data) => data,
            None => continue,
        };
        let index = match agents
            .iter()
            .position(|agent| agent.agent_id == data.agent_id)
        {
            Some(index) => index,
            None => {
                agents.push(CloudAgent {
                    agent_id: data.agent_id.clone(),
                    pending: None,
                    deleted: false,
                });
                mutations.push((data.agent_id.clone(), None));
                agents.len() - 1
            }
        };
        if data.action == "record" {
            continue;
        }
        let archive = data.action.starts_with("archive");
        let intent = data.action.ends_with("_intent");
        let agent_mutations = &mut mutations[index].1;
        let slot = match agent_mutations {
            None => {
                *agent_mutations = Some(Mutation::default());
                agent_mutations.as_mut().unwrap()
            }
            Some(slot) => slot,
        };
        let current = if archive {
            slot.archive.as_ref()
        } else {
            slot.delete.as_ref()
        };
        let comparison = match current {
            None => 1,
            Some(current) => compare_timestamps(&data.timestamp, &current.timestamp),
        };
        let replace = comparison > 0
            || (comparison == 0 && !intent && current.map_or(false, |current| current.intent));
        if replace {
            let next = MutationStamp {
                intent,
                timestamp: data.timestamp.clone(),
            };
            if archive {
                slot.archive = Some(next);
            } else {
                slot.delete = Some(next);
            }
        }
    }
    for (index, agent) in agents.iter_mut().enumerate() {
        let slot = match &mutations[index].1 {
            Some(slot) => slot,
            None => continue,
        };
        let pending = [
            slot.archive
                .as_ref()
                .filter(|stamp| stamp.intent)
                .map(|stamp| ("archive", stamp.timestamp.clone())),
            slot.delete
                .as_ref()
                .filter(|stamp| stamp.intent)
                .map(|stamp| ("delete", stamp.timestamp.clone())),
        ];
        let mut pending: Vec<(&'static str, String)> = pending.into_iter().flatten().collect();
        pending.sort_by(|left, right| compare_timestamps(&right.1, &left.1).cmp(&0));
        agent.pending = pending.into_iter().next();
        agent.deleted = slot.delete.as_ref().map_or(false, |stamp| !stamp.intent);
    }
    agents.retain(|agent| !agent.deleted);
    agents
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloudAgent {
    pub agent_id: String,
    pub pending: Option<(&'static str, String)>,
    pub deleted: bool,
}

#[derive(Default)]
struct Mutation {
    archive: Option<MutationStamp>,
    delete: Option<MutationStamp>,
}

struct MutationStamp {
    intent: bool,
    timestamp: String,
}

/// Fixed-width `Z` timestamps compare byte-wise in time order; anything else
/// falls back to the upstream `localeCompare` fallback.
fn compare_timestamps(left: &str, right: &str) -> i32 {
    let ordered = if z_timestamp(left) && z_timestamp(right) {
        left.cmp(right)
    } else {
        left.as_bytes().cmp(right.as_bytes())
    };
    match ordered {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

fn z_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.len() >= 20
        && value.ends_with('Z')
        && bytes.get(4) == Some(&b'-')
        && bytes.get(7) == Some(&b'-')
        && bytes.get(10) == Some(&b'T')
        && bytes.get(13) == Some(&b':')
        && bytes.get(16) == Some(&b':')
}

fn backup_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".bak")
        || lower.ends_with(".backup")
        || lower.ends_with(".tmp")
        || lower.contains(".journal.bak")
}

fn journal_records(artifact: &Artifact) -> Result<Vec<Value>, &'static str> {
    let mut records = Vec::new();
    for line in artifact.text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line).map_err(|_| "mismatched-custody")?;
        if !record.is_object() {
            return Err("mismatched-custody");
        }
        records.push(record);
    }
    Ok(records)
}

fn active_custody(
    session: &Session,
    path: &Path,
    artifacts: &[Artifact],
) -> Result<Option<Pending>, &'static str> {
    let session_file = path.to_str().ok_or("mismatched-custody")?;
    let mut entries = session.entries.clone();
    for artifact in artifacts {
        if artifact.name.contains("pi-import") {
            return Err("imported-origin");
        }
        if backup_name(&artifact.name) {
            return Err("active-backup");
        }
        if !artifact.name.contains(CLOUD_ENTRY) {
            continue;
        }
        for record in journal_records(artifact)? {
            if record.get("version").and_then(Value::as_u64) != Some(CUSTODY_RECORD_VERSION) {
                return Err("obsolete-custody-record");
            }
            if record.get("sessionId").and_then(Value::as_str) != Some(session.session_id.as_str())
                || record.get("sessionFile").and_then(Value::as_str) != Some(session_file)
            {
                return Err("mismatched-custody");
            }
            if !record
                .get("anchorEntryId")
                .map_or(true, |value| value.is_null() || value.is_string())
            {
                return Err("mismatched-custody");
            }
            if accepted_cloud_entry(&record).is_none() {
                return Err("mismatched-custody");
            }
            entries.push(json!({
                "type": "custom",
                "customType": CLOUD_ENTRY,
                "data": record,
            }));
        }
    }
    Ok(cloud_agents(&entries).into_iter().find_map(|agent| {
        agent.pending.map(|(action, _)| Pending {
            agent_id: agent.agent_id,
            action: action.to_owned(),
        })
    }))
}

/// Decide whether `authority` may run for the session at `path`.
///
/// `path` is the host-owned session file (the adapter never accepts a path from
/// a worker or a model descriptor); `artifacts` are the file names and contents
/// cloud authority would read next to it.
pub fn evaluate(path: &Path, text: &str, authority: Authority, artifacts: &[Artifact]) -> Decision {
    if imported_location(path) {
        return Decision::refuse(
            authority,
            Origin::Imported,
            "imported-location",
            is_resume(authority),
        );
    }
    let session = match Session::parse(text) {
        Ok(session) => session,
        Err((origin, code)) => {
            return Decision::refuse(authority, origin, code, is_resume(authority));
        }
    };
    for entry in &session.entries {
        if entry.get("imported").is_some()
            || entry
                .get("data")
                .and_then(Value::as_object)
                .map_or(false, |data| data.contains_key("imported"))
        {
            return Decision::refuse(
                authority,
                Origin::Imported,
                "imported-entry-origin",
                is_resume(authority),
            )
            .session(Some(session.session_id.as_str()));
        }
    }
    let refused = |code: &'static str| {
        let origin = if code == "imported-origin" {
            Origin::Imported
        } else {
            Origin::Native
        };
        Decision::refuse(authority, origin, code, is_resume(authority))
            .session(Some(session.session_id.as_str()))
    };
    match authority {
        Authority::Resume => match session.resume_handle(path) {
            Ok(agent_id) => Decision::allow(authority, Origin::Native, session.session_id.clone())
                .agent(Some(agent_id)),
            Err(code) => refused(code),
        },
        Authority::Cleanup => {
            let handle = match session.resume_handle(path) {
                Ok(handle) => handle,
                Err(code) => return refused(code),
            };
            let mut pending: Vec<String> = Vec::new();
            for entry in session.custom(CLEANUP_ENTRY) {
                let data = match entry.get("data").and_then(accepted_cleanup_entry) {
                    Some(data) => data,
                    None => return refused("invalid-cleanup-entry"),
                };
                if let Some(candidates) = entry
                    .get("data")
                    .and_then(|data| data.get("candidateAgentIds"))
                    .and_then(Value::as_array)
                {
                    for value in candidates {
                        if !value.as_str().map_or(false, valid_local_agent_id) {
                            return refused("unsafe-agent-id");
                        }
                    }
                }
                if let Some(protected) = entry
                    .get("data")
                    .and_then(|data| data.get("protectedAgentIds"))
                    .and_then(Value::as_array)
                {
                    for value in protected {
                        match value.as_str().filter(|id| valid_local_agent_id(id)) {
                            Some(_) => {}
                            None => return refused("unsafe-agent-id"),
                        }
                    }
                }
                if let Some(deleted) = entry
                    .get("data")
                    .and_then(|data| data.get("deletedAgentIds"))
                    .and_then(Value::as_array)
                {
                    for value in deleted {
                        if !value.as_str().map_or(false, valid_local_agent_id) {
                            return refused("unsafe-agent-id");
                        }
                    }
                }
                if let Some(failed) = entry
                    .get("data")
                    .and_then(|data| data.get("failedAgentIds"))
                    .and_then(Value::as_array)
                {
                    for failure in failed {
                        if !failure
                            .get("agentId")
                            .and_then(Value::as_str)
                            .map_or(false, valid_local_agent_id)
                        {
                            return refused("unsafe-agent-id");
                        }
                    }
                }
                if data.action == "delete" {
                    if data.phase.as_deref() == Some("intent") {
                        for candidate in &data.candidate_agent_ids {
                            if !pending.contains(candidate) {
                                pending.push(candidate.clone());
                            }
                        }
                    } else if data.phase.as_deref() == Some("result") {
                        pending.retain(|candidate| !data.candidate_agent_ids.contains(candidate));
                        if let Some(failed) = entry
                            .get("data")
                            .and_then(|data| data.get("failedAgentIds"))
                            .and_then(Value::as_array)
                        {
                            for failure in failed {
                                if let Some(id) = failure.get("agentId").and_then(Value::as_str) {
                                    pending.retain(|candidate| candidate != id);
                                }
                            }
                        }
                    }
                }
            }
            if !pending.is_empty() {
                pending.sort();
                return refused("active-custody").pending(Pending {
                    agent_id: pending[0].clone(),
                    action: "delete".into(),
                });
            }
            Decision::allow(authority, Origin::Native, session.session_id.clone())
                .agent(Some(handle))
        }
        Authority::Cloud => {
            for entry in session.custom(CLOUD_ENTRY) {
                let data = entry.get("data").and_then(accepted_cloud_entry);
                if data.is_none() {
                    let raw = entry.get("data").and_then(Value::as_object);
                    let agent = raw
                        .and_then(|record| record.get("agentId"))
                        .and_then(Value::as_str);
                    if agent.map_or(false, |id| !valid_cloud_agent_id(id)) {
                        return refused("unsafe-cloud-identifier");
                    }
                    return refused("invalid-cloud-entry");
                }
            }
            match active_custody(&session, path, artifacts) {
                Ok(None) => {}
                Ok(Some(pending)) => return refused("active-custody").pending(pending),
                Err(code) => return refused(code),
            }
            Decision::allow(authority, Origin::Native, session.session_id.clone())
        }
    }
}

fn is_resume(authority: Authority) -> bool {
    matches!(authority, Authority::Resume)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        r#"{"type":"session","version":4,"id":"0193aa1b-0000-7000-8000-000000000001"}"#;
    const STORE: &str = r#"{"version":1,"stateRoot":"/home/u/.cursor/store"}"#;

    fn resume_line(version: u64, agent: &str) -> String {
        format!(
            r#"{{"type":"custom","id":"entry0001","parentId":null,"customType":"cursor-sdk-agent-resume","data":{{"version":{version},"runtime":"local","agentId":"{agent}","scopeKey":"scope","cwd":"/p","poolKey":"pool","branchPathHash":"hash","compactionGeneration":0,"sendState":{{"bootstrapped":false,"contextFingerprint":"f","incrementalSendCount":0}},"createdAt":"2026-10-01T00:00:00.000Z","storeIdentity":{STORE}}}}}"#
        )
    }

    #[test]
    fn native_session_resumes() {
        let text = format!("{HEADER}\n{}\n", resume_line(2, "agent-abc"));
        let decision = evaluate(
            Path::new("/home/u/.rpi/sessions/a.jsonl"),
            &text,
            Authority::Resume,
            &[],
        );
        assert!(decision.allowed);
        assert_eq!(decision.agent_id.as_deref(), Some("agent-abc"));
    }

    #[test]
    fn imported_snapshot_never_resumes() {
        let header = HEADER.replace(
            r#""version":4"#,
            r#""version":3,"imported":{"source":"pi","sourcePath":"/home/u/.pi/a.jsonl","sha256":"0000000000000000000000000000000000000000000000000000000000000000","sessionVersion":3}"#,
        );
        let text = format!("{header}\n{}\n", resume_line(2, "agent-abc"));
        let decision = evaluate(
            Path::new("/home/u/.rpi/sessions/pi-import/a.jsonl"),
            &text,
            Authority::Resume,
            &[],
        );
        assert_eq!(decision.code, Some("imported-location"));
        assert!(decision.fresh_agent);
        assert_eq!(decision.notice, Some(FRESH_AGENT_NOTICE));
    }

    #[test]
    fn traversal_in_the_import_marker_is_reported() {
        let header = HEADER.replace(
            r#""version":4"#,
            r#""version":3,"imported":{"source":"pi","sourcePath":"/home/u/.pi/../../etc/a.jsonl","sha256":"0000000000000000000000000000000000000000000000000000000000000000","sessionVersion":3}"#,
        );
        let decision = evaluate(
            Path::new("/home/u/.rpi/sessions/a.jsonl"),
            &header,
            Authority::Resume,
            &[],
        );
        assert_eq!(decision.code, Some("unsafe-source-path"));
        assert_eq!(decision.origin, Origin::Imported);
    }

    #[test]
    fn identifier_rules_match_the_pinned_upstream_patterns() {
        assert!(valid_session_id("0193aa1b-0000-7000-8000-000000000001"));
        assert!(!valid_session_id("../a"));
        assert!(!valid_session_id("a/b"));
        assert!(!valid_session_id("."));
        assert!(valid_local_agent_id("agent-a_b-1"));
        assert!(!valid_local_agent_id("agent-"));
        assert!(!valid_local_agent_id("agent-../x"));
        assert!(valid_cloud_agent_id(
            "bc-00000000-0000-0000-0000-000000000001"
        ));
        assert!(!valid_cloud_agent_id(
            "bc-00000000-0000-0000-0000-00000000000z"
        ));
    }
}

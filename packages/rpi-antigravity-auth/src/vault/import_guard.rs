//! Imported vault state cannot activate Antigravity custody (ledger I02).
//!
//! The host importer copies Pi sessions and explicitly does **not** copy vault
//! state. Anything that nevertheless appears as vault state (a mode file, an
//! enrollment record or token, a credential stash, a backup) may have been
//! copied by hand from another agent. It has no enrollment with this agent's
//! vault and no custody agreement with the vault operator, so it must not
//! activate `custody` mode or serve a stored token. `admit` returns a refusal
//! carrying [`FRESH_ENROLLMENT_NOTICE`]; the caller then runs the normal
//! enrollment ceremony before any request.
//!
//! The recognized origin marker is the host importer's session header field:
//!
//! ```json
//! "imported": {"source":"pi","sourcePath":"/home/u/.pi/agent/sessions/x.jsonl",
//!              "sha256":"<64 hex>","sessionVersion":3}
//! ```
//!
//! Marker look-alikes are not custody. The placeholder key must be exactly
//! `antigravity-vault-custody` and the placeholder refresh must start with
//! `claustrum-tombstone:v1:`; near-misses such as
//! `antigravity-vault-custody-extra` stay foreign, as the pinned upstream
//! admission test requires. The Pi slot itself is inspected through
//! [`slot::Slot`], so `original` (a real login still in the slot), `foreign`
//! (a stash that does not belong to this agent) and `missing` are reported by
//! the same code that performs the custody swap.

use super::{enrollment, slot};
use serde_json::{json, Value};
use std::path::Path;

/// Shown when imported or foreign vault state was refused.
pub const FRESH_ENROLLMENT_NOTICE: &str = "Vault state that was not enrolled for this agent cannot serve Antigravity. Request vault access again; no imported custody, backup, token or local login is activated.";
/// Shown when local custody drift must be repaired instead of re-enrolled.
pub const RESTORE_NOTICE: &str = "The credential slot and vault mode disagree. Restore the saved login or switch the account source before continuing.";

/// Exact non-secret placeholder stored while the vault serves.
pub const CUSTODY_MARKER: &str = "antigravity-vault-custody";
/// Refresh prefix of the placeholder login.
pub const TOMBSTONE_PREFIX: &str = "claustrum-tombstone:v1:";
/// Provider key the slot is inspected under.
pub const PROVIDER: &str = "google";
/// Current vault mode record version.
pub const MODE_VERSION: u64 = 1;
/// Current custody stash version.
pub const STASH_VERSION: u64 = 1;
/// Newest Pi session version an import marker may name.
pub const MAX_PI_SESSION_VERSION: u64 = 3;

/// Pi slot classification, matching upstream `PiSlotInspection.slot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Original,
    Placeholder,
    Foreign,
    Empty,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Placeholder => "placeholder",
            Self::Foreign => "foreign",
            Self::Empty => "empty",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "original" => Some(Self::Original),
            "placeholder" => Some(Self::Placeholder),
            "foreign" => Some(Self::Foreign),
            "empty" => Some(Self::Empty),
            _ => None,
        }
    }
}

/// Pi slot stash classification, matching `PiSlotInspection.stash`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stash {
    Valid,
    Missing,
    /// A stash that failed validation (foreign identity, damaged, wrong version).
    Invalid,
}

impl Stash {
    pub fn name(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Missing => "missing",
            Self::Invalid => "invalid",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "valid" => Some(Self::Valid),
            "missing" => Some(Self::Missing),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// The state read from the auth slot, normally produced by [`inspect`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Custody {
    pub slot: Slot,
    pub stash: Stash,
}

impl Default for Custody {
    fn default() -> Self {
        Self {
            slot: Slot::Empty,
            stash: Stash::Missing,
        }
    }
}

impl Custody {
    pub fn to_json(&self) -> Value {
        json!({"slot": self.slot.name(), "stash": self.stash.name()})
    }

    /// Names as reported by the Pi slot inspection (or the `invalid` refusal
    /// state used when the stash does not belong to this agent).
    pub fn parse(slot: &str, stash: &str) -> Option<Self> {
        Some(Self {
            slot: Slot::parse(slot)?,
            stash: Stash::parse(stash)?,
        })
    }
}

/// Vault files as the host read them. Absent files stay `None` rather than
/// being replaced by defaults, so an unreadable copy can be refused.
#[derive(Clone, Debug, Default)]
pub struct Files {
    pub mode: Option<Value>,
    pub enrollment_state: Option<Value>,
    pub enrollment_token: Option<Value>,
    pub stash: Option<Value>,
    /// Names of sibling files in the vault directory (backups, imports, staging).
    pub artifacts: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Local,
    Custody,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Custody => "custody",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub fresh_enrollment: bool,
    pub notice: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    Allow {
        mode: Mode,
        route_id: Option<String>,
    },
    Refuse(Refusal),
}

impl Admission {
    pub fn to_json(&self) -> Value {
        match self {
            Self::Allow { mode, route_id } => json!({
                "allowed": true,
                "mode": mode.name(),
                "routeId": route_id,
                "code": null,
                "freshEnrollment": false,
                "notice": null,
            }),
            Self::Refuse(refusal) => json!({
                "allowed": false,
                "mode": Value::Null,
                "routeId": Value::Null,
                "code": refusal.code,
                "freshEnrollment": refusal.fresh_enrollment,
                "notice": refusal.notice,
            }),
        }
    }
}

fn refuse(code: &'static str, fresh: bool) -> Admission {
    Admission::Refuse(Refusal {
        code,
        fresh_enrollment: fresh,
        notice: if fresh {
            FRESH_ENROLLMENT_NOTICE
        } else {
            RESTORE_NOTICE
        },
    })
}

fn fresh(code: &'static str) -> Admission {
    refuse(code, true)
}

fn safe_source_path(path: &str) -> bool {
    if path.is_empty() || path.contains('\0') || path.contains('\\') || !path.ends_with(".jsonl") {
        return false;
    }
    let parsed = Path::new(path);
    parsed.is_absolute()
        && !parsed
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

/// Check one value for an import origin marker. `Ok(())` means no marker.
fn import_marker(value: &Value) -> Result<(), &'static str> {
    let object = match value.as_object() {
        Some(object) => object,
        None => return Ok(()),
    };
    // Legacy hand-written provenance is an import marker too.
    if object.get("origin").and_then(Value::as_str) == Some("imported")
        || object.contains_key("importedFrom")
    {
        return Err("imported-custody");
    }
    let marker = match object.get("imported") {
        Some(marker) => marker,
        None => return Ok(()),
    };
    let fields = marker.as_object().ok_or("invalid-import-marker")?;
    if fields.get("source").and_then(Value::as_str) != Some("pi") {
        return Err("forged-custody-origin");
    }
    let source_path = fields
        .get("sourcePath")
        .and_then(Value::as_str)
        .ok_or("invalid-import-marker")?;
    if !safe_source_path(source_path) {
        return Err("unsafe-source-path");
    }
    let sha = fields
        .get("sha256")
        .and_then(Value::as_str)
        .ok_or("invalid-import-marker")?;
    if sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("invalid-import-marker");
    }
    let version = fields
        .get("sessionVersion")
        .and_then(Value::as_u64)
        .ok_or("invalid-import-marker")?;
    if !(1..=MAX_PI_SESSION_VERSION).contains(&version) {
        return Err("unsupported-imported-version");
    }
    Err("imported-custody")
}

fn artifact(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    if lower.contains("pi-import") {
        return Some("imported-location");
    }
    if lower.ends_with(".bak")
        || lower.ends_with(".backup")
        || lower.ends_with(".tmp")
        || lower.contains(".retired")
    {
        return Some("active-backup");
    }
    None
}

/// Parse the vault mode record the way upstream `readPiVaultMode` does, with
/// explicit refusal codes instead of a generic unreadable reason.
fn mode_record(value: &Value) -> Result<(Mode, Option<String>), &'static str> {
    let record = value.as_object().ok_or("unknown-vault-mode")?;
    if record.contains_key("imported")
        || record.contains_key("importedFrom")
        || record.get("origin").and_then(Value::as_str) == Some("imported")
    {
        return Err("imported-custody");
    }
    for key in record.keys() {
        if !matches!(key.as_str(), "version" | "mode" | "routeId") {
            return Err("unknown-vault-mode");
        }
    }
    match record.get("version").and_then(Value::as_u64) {
        Some(MODE_VERSION) => {}
        Some(_) => return Err("obsolete-vault-mode"),
        None => return Err("unknown-vault-mode"),
    }
    let mode = match record.get("mode").and_then(Value::as_str) {
        Some("local") => Mode::Local,
        Some("custody") => Mode::Custody,
        _ => return Err("unknown-vault-mode"),
    };
    let route_id = match record.get("routeId") {
        None => None,
        Some(value) => match value.as_str() {
            Some(text) if !text.trim().is_empty() => Some(text.to_owned()),
            _ => return Err("unknown-vault-mode"),
        },
    };
    if mode == Mode::Local && route_id.is_some() {
        return Err("unknown-vault-mode");
    }
    Ok((mode, route_id))
}

fn enrollment_token(value: &Value) -> Result<u64, &'static str> {
    let object = value.as_object().ok_or("invalid-enrollment")?;
    if !enrollment::valid_token(object.get("token").unwrap_or(&Value::Null)) {
        return Err("invalid-enrollment");
    }
    let generation = object
        .get("token_generation")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 1.0)
        .ok_or("obsolete-enrollment")?;
    Ok(generation as u64)
}

fn enrollment_state(value: &Value) -> Result<(), &'static str> {
    let decoded = enrollment::decode_state(value).map_err(|_| "invalid-enrollment")?;
    if decoded["phase"] == "approved" {
        match decoded.get("tokenGeneration").and_then(Value::as_f64) {
            Some(generation) if generation >= 1.0 => {}
            _ => return Err("obsolete-enrollment"),
        }
    }
    Ok(())
}

/// Decide whether this agent may serve Antigravity from the vault.
///
/// `files` are the vault records the host read; `custody` is the slot
/// inspection from [`inspect`]. Nothing is written and no network is touched.
pub fn admit(files: &Files, custody: &Custody) -> Admission {
    for name in &files.artifacts {
        if let Some(code) = artifact(name) {
            return fresh(code);
        }
    }
    for value in [
        &files.mode,
        &files.enrollment_state,
        &files.enrollment_token,
        &files.stash,
    ]
    .into_iter()
    .flatten()
    {
        if let Err(code) = import_marker(value) {
            return fresh(code);
        }
    }
    if custody.slot == Slot::Foreign || custody.stash == Stash::Invalid {
        // A slot or stash that does not belong to this agent is not a login.
        return refuse("mismatched-custody", false);
    }
    let (mode, route_id) = match &files.mode {
        Some(value) => match mode_record(value) {
            Ok(record) => record,
            Err(code) => return fresh(code),
        },
        // Upstream treats a missing mode file as local.
        None => (Mode::Local, None),
    };
    if let Some(stash) = &files.stash {
        if stash.get("version").and_then(Value::as_u64) != Some(STASH_VERSION) {
            return fresh("obsolete-custody-record");
        }
        if let Some(provider) = stash.get("provider") {
            if provider.as_str() != Some(PROVIDER) {
                return refuse("mismatched-custody", false);
            }
        }
    }
    if mode == Mode::Local {
        // Custody leftovers are not a local login; the slot must be restored.
        if custody.slot == Slot::Placeholder || custody.stash == Stash::Valid {
            return refuse("active-custody", false);
        }
        return Admission::Allow {
            mode,
            route_id: None,
        };
    }
    // Custody mode: a slot that is not exactly the placeholder is either the
    // real login still in place or a near-miss marker; neither may be served.
    match custody.slot {
        Slot::Placeholder => {}
        Slot::Original | Slot::Empty => return refuse("unsynchronized-custody", false),
        Slot::Foreign => return refuse("mismatched-custody", false),
    }
    if custody.stash == Stash::Missing {
        return refuse("missing-custody", false);
    }
    let generation = match &files.enrollment_token {
        Some(token) => match enrollment_token(token) {
            Ok(generation) => generation,
            Err(code) => return fresh(code),
        },
        None => return fresh("missing-enrollment"),
    };
    if generation < 1 {
        return fresh("obsolete-enrollment");
    }
    if let Some(state) = &files.enrollment_state {
        if let Err(code) = enrollment_state(state) {
            return fresh(code);
        }
    }
    Admission::Allow { mode, route_id }
}

/// Inspect the Pi credential slot with the port that performs the custody
/// swap. Imported marker fields are reported separately by [`admit`].
pub fn inspect(
    auth: &Path,
    stash: &Path,
    provider: &str,
    placeholder: &str,
) -> Result<Custody, slot::Error> {
    let inspected = slot::Slot::new(auth, stash, provider, placeholder)?.inspect();
    let inspected = match inspected {
        Ok(inspected) => inspected,
        // The slot is not readable as this agent's login or stash. Refusing it
        // is the safe classification; `admit` then reports mismatched custody.
        Err(_) => {
            return Ok(Custody {
                slot: Slot::Foreign,
                stash: Stash::Invalid,
            })
        }
    };
    let slot_state = inspected["slot"]
        .as_str()
        .and_then(Slot::parse)
        .ok_or(slot::Error::Slot("invalid-auth"))?;
    let stash_state = inspected["stash"]
        .as_str()
        .and_then(Stash::parse)
        .ok_or(slot::Error::Slot("invalid-stash"))?;
    Ok(Custody {
        slot: slot_state,
        stash: stash_state,
    })
}

/// Expose the exact placeholder contract to callers that must not invent it.
pub fn placeholder_key() -> &'static str {
    CUSTODY_MARKER
}

/// Whether a stored refresh value is the vault placeholder rather than a login.
pub fn placeholder_refresh(value: &str) -> bool {
    value.starts_with(TOMBSTONE_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = r#"{"source":"pi","sourcePath":"/home/u/.pi/agent/sessions/a.jsonl","sha256":"0000000000000000000000000000000000000000000000000000000000000000","sessionVersion":3}"#;

    fn token() -> Value {
        json!({"token":"0".repeat(64),"token_generation":2})
    }

    #[test]
    fn custody_requires_a_fresh_enrollment_token() {
        let files = Files {
            mode: Some(json!({"version":1,"mode":"custody"})),
            ..Files::default()
        };
        let custody = Custody {
            slot: Slot::Placeholder,
            stash: Stash::Valid,
        };
        let refusal = match admit(&files, &custody) {
            Admission::Refuse(refusal) => refusal,
            other => panic!("expected refusal, got {other:?}"),
        };
        assert_eq!(refusal.code, "missing-enrollment");
        assert!(refusal.fresh_enrollment);
        assert_eq!(refusal.notice, FRESH_ENROLLMENT_NOTICE);
    }

    #[test]
    fn forged_and_traversing_markers_are_distinguished() {
        let forged = Files {
            mode: Some(json!({"version":1,"mode":"custody","imported":{"source":"rpi"}})),
            ..Files::default()
        };
        assert_eq!(
            admit(&forged, &Custody::default()),
            Admission::Refuse(Refusal {
                code: "forged-custody-origin",
                fresh_enrollment: true,
                notice: FRESH_ENROLLMENT_NOTICE,
            })
        );
        let traversal = Files {
            enrollment_state: Some(
                json!({"imported":{"source":"pi","sourcePath":"/a/../../b.jsonl","sha256":"0".repeat(64),"sessionVersion":3}}),
            ),
            ..Files::default()
        };
        assert!(matches!(
            admit(&traversal, &Custody::default()),
            Admission::Refuse(refusal) if refusal.code == "unsafe-source-path"
        ));
    }

    #[test]
    fn valid_custody_is_admitted_only_with_the_placeholder_slot() {
        let files = Files {
            mode: Some(json!({"version":1,"mode":"custody","routeId":"route-1"})),
            enrollment_token: Some(token()),
            stash: Some(json!({"version":1,"provider":"google"})),
            ..Files::default()
        };
        assert_eq!(
            admit(
                &files,
                &Custody {
                    slot: Slot::Placeholder,
                    stash: Stash::Valid,
                }
            ),
            Admission::Allow {
                mode: Mode::Custody,
                route_id: Some("route-1".into()),
            }
        );
        assert!(matches!(
            admit(
                &files,
                &Custody {
                    slot: Slot::Foreign,
                    stash: Stash::Valid,
                }
            ),
            Admission::Refuse(refusal) if refusal.code == "mismatched-custody"
        ));
    }

    #[test]
    fn imported_marker_is_never_valid() {
        let files = Files {
            stash: Some(
                json!({"version":1,"provider":"google","imported": serde_json::from_str::<Value>(MARKER).unwrap()}),
            ),
            ..Files::default()
        };
        assert!(matches!(
            admit(&files, &Custody::default()),
            Admission::Refuse(refusal) if refusal.code == "imported-custody" && refusal.fresh_enrollment
        ));
    }
}

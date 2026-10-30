//! Claude Code version pin, override, learned floor, and version rejections.
//!
//! Ports `src/claude-code-version.ts` and the `claude_code_version_too_old`
//! parsing from `src/version-rejection.ts` (gotgenes/pi-anthropic-auth).
//!
//! Anthropic gates newly released models on a minimum Claude Code version and
//! rejects OAuth requests below it with `error_code: claude_code_version_too_old`.
//! The version reported in the billing header is what Anthropic keys on, so this
//! module decides whether a new model works at all.
//!
//! There is no upstream source to import the pin from. Keep it in step with the
//! current release (`npm view @anthropic-ai/claude-code version`); do not read it
//! from a local `claude --version`, whose `stable` dist-tag lags `latest`.
//! Known floors: `claude-fable-5-1` requires >= 2.1.251; `claude-opus-5-5`
//! requires >= 2.1.280.

use serde_json::Value;
use std::fmt;
use std::sync::{Arc, Mutex};

/// Claude Code version embedded in the billing header.
pub const CLAUDE_CODE_VERSION: &str = "2.1.280";

/// Environment variable that overrides [`CLAUDE_CODE_VERSION`].
pub const CLAUDE_CODE_VERSION_ENV: &str = "PI_ANTHROPIC_AUTH_CLAUDE_CODE_VERSION";

const CLAUDE_CODE_VERSION_TOO_OLD: &str = "claude_code_version_too_old";

const HINT_PREFIX: &str = "[pi-anthropic-auth]";

const CURRENT_RELEASE: &str =
    "the current Claude Code release (npm view @anthropic-ai/claude-code dist-tags)";

/// A configured override that is not a bare `X.Y.Z` version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidClaudeCodeVersion {
    pub value: String,
}

impl fmt::Display for InvalidClaudeCodeVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let quoted =
            serde_json::to_string(&self.value).unwrap_or_else(|_| format!("\"{}\"", self.value));
        write!(
            formatter,
            "{CLAUDE_CODE_VERSION_ENV} must be a bare X.Y.Z version, received {quoted}"
        )
    }
}

impl std::error::Error for InvalidClaudeCodeVersion {}

/// Whether `candidate` is exactly `X.Y.Z`: three dot-separated digit runs.
pub fn is_bare_version(candidate: &str) -> bool {
    let mut parts = candidate.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    [major, minor, patch]
        .iter()
        .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Resolves the Claude Code version used in the billing header from the process
/// environment.
pub fn resolve_claude_code_version() -> Result<String, InvalidClaudeCodeVersion> {
    resolve_claude_code_version_from(|name| std::env::var(name).ok())
}

/// Resolves the Claude Code version from `get`, a variable-name lookup.
///
/// The override must be a bare `X.Y.Z` version: it is embedded in a salted hash
/// suffix, so a value Claude Code would never emit produces a billing header that
/// matches no real client. A malformed value is an error rather than a silent
/// fallback, so a typo surfaces instead of sending the bundled version the user
/// was trying to replace.
pub fn resolve_claude_code_version_from<F>(get: F) -> Result<String, InvalidClaudeCodeVersion>
where
    F: Fn(&str) -> Option<String>,
{
    let configured = get(CLAUDE_CODE_VERSION_ENV).unwrap_or_default();
    let configured = configured.trim();
    if configured.is_empty() {
        return Ok(CLAUDE_CODE_VERSION.to_string());
    }
    if !is_bare_version(configured) {
        return Err(InvalidClaudeCodeVersion {
            value: configured.to_string(),
        });
    }
    Ok(configured.to_string())
}

/// Reports whether an explicit [`CLAUDE_CODE_VERSION_ENV`] override is set.
///
/// An override is absolute: [`crate::billing::BillingVersionSync`] leaves a user's
/// pin alone rather than raising it from Pi's reported version.
pub fn has_claude_code_version_override() -> bool {
    has_claude_code_version_override_from(|name| std::env::var(name).ok())
}

/// Reports whether an override is set in `get`, a variable-name lookup.
pub fn has_claude_code_version_override_from<F>(get: F) -> bool
where
    F: Fn(&str) -> Option<String>,
{
    get(CLAUDE_CODE_VERSION_ENV).is_some_and(|value| !value.trim().is_empty())
}

/// Reads the Claude Code version out of Pi's own `user-agent` header.
///
/// Pi's built-in Anthropic transport sends `user-agent: claude-cli/<version>`
/// from a module-private constant, so this header is the only runtime handle on
/// the version Pi believes it is. The token must stand alone: `notclaude-cli/2.1.280`
/// and `claude-cli/2.1` both yield `None`.
pub fn read_claude_cli_version(user_agent: Option<&str>) -> Option<String> {
    user_agent?.split_whitespace().find_map(|token| {
        token
            .strip_prefix("claude-cli/")
            .filter(|version| is_bare_version(version))
            .map(str::to_string)
    })
}

/// Returns whichever of two `X.Y.Z` versions is higher, comparing numerically.
///
/// `baseline` wins ties and wins outright when `candidate` is absent or is not a
/// bare `X.Y.Z` version: an unparseable upstream signal must never lower the
/// version this package reports. An unparseable `baseline` also wins, matching
/// upstream, where every comparison against its `NaN` components is false.
pub fn higher_version(baseline: &str, candidate: Option<&str>) -> String {
    let Some(candidate) = candidate.filter(|candidate| is_bare_version(candidate)) else {
        return baseline.to_string();
    };
    if !is_bare_version(baseline) {
        return baseline.to_string();
    }

    let baseline_parts = numeric_parts(baseline);
    let candidate_parts = numeric_parts(candidate);
    for (baseline_part, candidate_part) in baseline_parts.iter().zip(candidate_parts.iter()) {
        if candidate_part != baseline_part {
            return if candidate_part > baseline_part {
                candidate.to_string()
            } else {
                baseline.to_string()
            };
        }
    }

    baseline.to_string()
}

fn numeric_parts(version: &str) -> [u64; 3] {
    let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0));
    [
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    ]
}

/// The highest Claude Code version Anthropic has demanded this process.
///
/// [`crate::billing::BillingVersionSync`] teaches it the floor named in a
/// `claude_code_version_too_old` rejection and applies it to every later request,
/// so only the first request after a floor rise pays the rejected round trip. One
/// floor serves every model: a floor Anthropic names is a released Claude Code
/// version, which real Claude Code sends to every model.
///
/// Clones share the same floor, mirroring the shared object the upstream transport
/// closes over.
#[derive(Clone, Debug, Default)]
pub struct LearnedFloor {
    learned: Arc<Mutex<Option<String>>>,
}

impl LearnedFloor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Raises the floor; a lower, equal, or unparseable version is ignored.
    pub fn learn(&self, version: &str) {
        if !is_bare_version(version) {
            return;
        }
        let mut learned = self
            .learned
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let next = match learned.as_deref() {
            Some(current) => higher_version(current, Some(version)),
            None => version.to_string(),
        };
        *learned = Some(next);
    }

    /// Returns `version` raised to the learned floor, if one has been learned.
    pub fn apply_to(&self, version: &str) -> String {
        let learned = self
            .learned
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        higher_version(version, learned.as_deref())
    }
}

/// A parsed `claude_code_version_too_old` rejection body.
///
/// Anthropic rejects an OAuth request for a gated model with a 400 whose
/// `error.details.error_code` is `claude_code_version_too_old` and whose message
/// names the required floor:
///
/// ```text
/// Claude Code 2.1.260 does not support this model; version 2.1.280 or newer is required.
/// ```
///
/// The wording is a convention, not a contract, so an unparseable floor degrades
/// to `None` rather than a guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeCodeVersionRejection {
    /// The floor Anthropic named, when its message carries a parseable one.
    pub required_version: Option<String>,
    body: Value,
    message: String,
}

impl ClaudeCodeVersionRejection {
    /// Returns the same error body with `hint` appended to `error.message`.
    ///
    /// Every other field — `type`, `error.type`, `error.details.error_code`,
    /// `request_id` — is preserved, so consumers keyed on them are unaffected.
    pub fn with_hint(&self, hint: &str) -> String {
        let message = if self.message.is_empty() {
            hint.to_string()
        } else {
            format!("{} {hint}", self.message)
        };
        let mut body = self.body.clone();
        if let Some(error) = body.get_mut("error").and_then(Value::as_object_mut) {
            error.insert("message".into(), Value::String(message));
        }
        body.to_string()
    }
}

/// Parses a response body as a `claude_code_version_too_old` rejection.
///
/// Returns `None` for anything else — a non-JSON body, another error code, or a
/// body with no `error` object — so the caller leaves that response untouched.
pub fn read_claude_code_version_rejection(body_text: &str) -> Option<ClaudeCodeVersionRejection> {
    let body: Value = serde_json::from_str(body_text).ok()?;
    let error = body.get("error")?.as_object()?;
    let error_code = error.get("details")?.get("error_code")?.as_str()?;
    if error_code != CLAUDE_CODE_VERSION_TOO_OLD {
        return None;
    }

    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Some(ClaudeCodeVersionRejection {
        required_version: required_version(&message),
        body,
        message,
    })
}

/// Matches the floor Anthropic names, not the rejected version before it.
fn required_version(message: &str) -> Option<String> {
    const NEEDLE: &str = "version ";
    let mut search_from = 0;
    while let Some(offset) = message[search_from..].find(NEEDLE) {
        let start = search_from + offset + NEEDLE.len();
        let candidate: String = message[start..]
            .chars()
            .take_while(|character| character.is_ascii_digit() || *character == '.')
            .collect();
        let rest = &message[start + candidate.len()..];
        if is_bare_version(&candidate) && rest.starts_with(" or newer is required") {
            return Some(candidate);
        }
        search_from = start;
    }
    None
}

/// Why automatic recovery could not turn a rejection into a success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryHintReason {
    /// An explicit env override is absolute, so recovery never runs.
    Override {
        override_version: String,
        required_version: Option<String>,
    },
    /// The billing header already met the floor; Pi's own version did not.
    UpgradePi { sent_version: String },
    /// No retry was possible, or the retry was rejected too.
    SetOverride { required_version: Option<String> },
}

/// Renders the user-facing hint appended to an unrecovered rejection.
pub fn describe_recovery_hint(reason: &RecoveryHintReason) -> String {
    match reason {
        RecoveryHintReason::Override {
            override_version,
            required_version,
        } => {
            let target = match required_version {
                Some(version) => format!("{version} or newer"),
                None => CURRENT_RELEASE.to_string(),
            };
            format!(
                "{HINT_PREFIX} {CLAUDE_CODE_VERSION_ENV}={override_version} is sent verbatim; \
                 raise it to {target}, or unset it to let pi-anthropic-auth recover automatically."
            )
        }
        RecoveryHintReason::UpgradePi { sent_version } => format!(
            "{HINT_PREFIX} The billing header already reported {sent_version}; this rejection \
             likely comes from Pi's own claude-cli version. Upgrade Pi."
        ),
        RecoveryHintReason::SetOverride { required_version } => {
            let assignment = match required_version {
                Some(version) => format!("{CLAUDE_CODE_VERSION_ENV}={version}"),
                None => format!("{CLAUDE_CODE_VERSION_ENV} to {CURRENT_RELEASE}"),
            };
            format!("{HINT_PREFIX} Automatic recovery did not succeed; set {assignment} and retry.")
        }
    }
}

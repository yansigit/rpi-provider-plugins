//! `x-anthropic-billing-header` construction and Claude Code version sync.
//!
//! Ports `src/billing-header.ts`, the billing-header half of
//! `src/request-shaping.ts`, and `src/billing-version-sync.ts`
//! (gotgenes/pi-anthropic-auth).
//!
//! The billing block identifies a request as Claude Code usage. Without it,
//! Anthropic classifies an OAuth request as third-party app usage and rejects it
//! with a 400 disguised as "You're out of extra usage." The recipe below must
//! match what Anthropic's backend expects, so the version is a parameter rather
//! than a constant: the transport rebuilds the same header at a different version
//! when Pi reports a newer one or a rejection names a higher floor.

use crate::version::{
    describe_recovery_hint, has_claude_code_version_override_from, higher_version,
    read_claude_cli_version, read_claude_code_version_rejection, resolve_claude_code_version_from,
    ClaudeCodeVersionRejection, InvalidClaudeCodeVersion, LearnedFloor, RecoveryHintReason,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Salt used in the billing header suffix hash.
pub const BILLING_HEADER_SALT: &str = "59cf53e54c78";

/// Character positions sampled from the first user message for the billing hash.
pub const BILLING_HEADER_POSITIONS: [usize; 3] = [4, 7, 20];

/// Entrypoint identifier included in the billing header.
pub const CLAUDE_CODE_ENTRYPOINT: &str = "sdk-cli";

/// Prefix every billing header carries, used to detect an already-shaped block.
pub const BILLING_HEADER_MARKER: &str = "x-anthropic-billing-header:";

fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded
}

/// Returns the text of the first user message, which seeds the billing hash.
///
/// Returns the empty string when the payload carries no user message with text
/// content; callers treat that as "no billing header to build".
pub fn get_first_user_text(messages: &[Value]) -> String {
    let Some(message) = messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
    else {
        return String::new();
    };

    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .find_map(|block| {
                if block.get("type").and_then(Value::as_str) != Some("text") {
                    return None;
                }
                block.get("text").and_then(Value::as_str)
            })
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    }
}

/// Returns the first user text of a whole request payload.
pub fn payload_first_user_text(payload: &Value) -> String {
    match payload.get("messages") {
        Some(Value::Array(messages)) => get_first_user_text(messages),
        _ => String::new(),
    }
}

/// Builds the `x-anthropic-billing-header` value for a request.
///
/// Returns `None` when there is no first user text to hash, since the header's
/// `cch` field is derived from it.
///
/// Sampling follows JavaScript string indexing (UTF-16 code units), so a position
/// landing on half of an astral character degrades to U+FFFD exactly as Node's
/// UTF-8 encoding of that lone surrogate does.
pub fn build_billing_header_value(message_text: &str, claude_code_version: &str) -> Option<String> {
    if message_text.is_empty() {
        return None;
    }

    let cch = &sha256_hex(message_text)[..5];
    let units: Vec<u16> = message_text.encode_utf16().collect();
    let sampled: String = BILLING_HEADER_POSITIONS
        .iter()
        .map(|index| {
            let unit = units.get(*index).copied().unwrap_or(u16::from(b'0'));
            String::from_utf16_lossy(&[unit])
        })
        .collect();
    let suffix = &sha256_hex(&format!(
        "{BILLING_HEADER_SALT}{sampled}{claude_code_version}"
    ))[..3];

    Some(format!(
        "{BILLING_HEADER_MARKER} cc_version={claude_code_version}.{suffix}; \
         cc_entrypoint={CLAUDE_CODE_ENTRYPOINT}; cch={cch};"
    ))
}

/// Coerces a payload's `system` field into an array of text blocks.
///
/// Anthropic accepts `system` as a bare string, an array of blocks, or nothing at
/// all; callers that need to inspect or rewrite it want one shape.
pub fn normalize_system_blocks(system: Option<&Value>) -> Vec<Value> {
    match system {
        Some(Value::Array(blocks)) => blocks.iter().map(normalize_system_block).collect(),
        Some(Value::Null) | None => Vec::new(),
        Some(block) => vec![normalize_system_block(block)],
    }
}

fn normalize_system_block(block: &Value) -> Value {
    match block {
        Value::String(text) => json!({ "type": "text", "text": text }),
        Value::Object(map) if map.get("text").is_some_and(Value::is_string) => {
            let mut normalized = map.clone();
            normalized.insert("type".into(), Value::String("text".into()));
            Value::Object(normalized)
        }
        _ => json!({ "type": "text", "text": "" }),
    }
}

/// Returns the payload's system blocks with the billing header prepended.
///
/// The inserted block carries `type` and `text` only — no `cache_control`, so the
/// header never consumes a cache breakpoint. An existing billing block is left in
/// place instead of duplicated.
///
/// Returns `None` when there is no first user text, so the caller keeps the
/// system value exactly as it was.
pub fn prepend_billing_header(
    system: Option<&Value>,
    messages: &[Value],
    claude_code_version: &str,
) -> Option<Vec<Value>> {
    let header = build_billing_header_value(&get_first_user_text(messages), claude_code_version)?;
    let mut blocks = normalize_system_blocks(system);

    if blocks.iter().any(is_billing_header_block) {
        return Some(blocks);
    }

    blocks.insert(0, json!({ "type": "text", "text": header }));
    Some(blocks)
}

fn is_billing_header_block(block: &Value) -> bool {
    block
        .get("text")
        .and_then(Value::as_str)
        .is_some_and(|text| text.contains(BILLING_HEADER_MARKER))
}

/// The Claude Code version signals a request is built from.
///
/// Resolved once per request from the environment: [`Self::from_env`] for
/// production, [`Self::from_vars`] for a caller-owned lookup (tests run in
/// parallel, so they must not mutate the process environment).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionSettings {
    /// [`crate::version::CLAUDE_CODE_VERSION`] or the explicit override.
    pub pinned: String,
    /// Whether the pin is an explicit user override, which is absolute.
    pub has_override: bool,
}

impl VersionSettings {
    pub fn from_env() -> Result<Self, InvalidClaudeCodeVersion> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    pub fn from_vars<F>(get: F) -> Result<Self, InvalidClaudeCodeVersion>
    where
        F: Fn(&str) -> Option<String>,
    {
        Ok(Self {
            pinned: resolve_claude_code_version_from(&get)?,
            has_override: has_claude_code_version_override_from(&get),
        })
    }
}

/// One transport-neutral response: the status and the body text.
///
/// Header rewriting (`content-length`, `content-encoding`, `request-id`) belongs
/// to the transport that owns the original response, not to this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncResponse {
    pub status: u16,
    pub body: String,
}

/// The response to return plus how many times the request was dispatched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncOutcome {
    pub response: SyncResponse,
    pub attempts: u8,
}

/// Per-request reconciliation of our `cc_version` with Pi's reported one.
///
/// Two independent Claude Code version signals reach Anthropic on an OAuth
/// request: Pi's `user-agent: claude-cli/<version>` and the `cc_version` in the
/// billing header this crate injects. Anthropic gates new models on the billing
/// header when it is present, so a stale pin blocks a model even when the host Pi
/// is new enough.
///
/// The bundled pin is a *floor*: when Pi reports a higher version, the billing
/// header is rebuilt at Pi's version on the way out. When Pi is at or below the
/// pin the request is sent exactly as shaping produced it.
///
/// When Anthropic raises a model's floor above both Pi and the pin, it rejects the
/// request and names the floor. The sync then retries once with the billing header
/// at that floor and teaches it to the shared [`LearnedFloor`], so later requests
/// go out at it directly. Only a 400 is ever read, so the success path is
/// untouched.
pub struct BillingVersionSync {
    first_user_text: String,
    learned_floor: LearnedFloor,
}

impl BillingVersionSync {
    pub fn new(learned_floor: LearnedFloor) -> Self {
        Self {
            first_user_text: String::new(),
            learned_floor,
        }
    }

    /// Records the payload whose billing header may need rebuilding.
    ///
    /// Called from the transport's `on_payload` hook, which runs before the
    /// dispatch of the same request, so the two share this object.
    pub fn record_request(&mut self, payload: &Value) {
        self.first_user_text = payload_first_user_text(payload);
    }

    /// The version this request's billing header should carry: the pin raised to
    /// Pi's reported version and to any learned floor. An explicit user pin is
    /// absolute and is never raised.
    pub fn target_version(&self, settings: &VersionSettings, user_agent: Option<&str>) -> String {
        if settings.has_override {
            return settings.pinned.clone();
        }
        self.learned_floor.apply_to(&higher_version(
            &settings.pinned,
            read_claude_cli_version(user_agent).as_deref(),
        ))
    }

    /// Returns `body` with its billing header moved from `from_version` to
    /// `to_version`, or `None` when the body cannot be rebuilt.
    ///
    /// Every bail-out is deliberate: we never construct a body we cannot verify
    /// against the exact header string shaping emitted moments earlier.
    pub fn rebuild_body(&self, body: &str, from_version: &str, to_version: &str) -> Option<String> {
        if self.first_user_text.is_empty() {
            return None;
        }

        let from_header = build_billing_header_value(&self.first_user_text, from_version)?;
        let to_header = build_billing_header_value(&self.first_user_text, to_version)?;
        if !body.contains(&from_header) {
            return None;
        }

        Some(body.replacen(&from_header, &to_header, 1))
    }

    /// Runs one request, retrying once at a floor Anthropic names.
    ///
    /// `dispatch` sends the body it is given and returns the status and body text
    /// of the response. A retry is dispatched at most once; a second rejection is
    /// explained in the returned body, never chased.
    pub fn fetch<F>(
        &self,
        settings: &VersionSettings,
        user_agent: Option<&str>,
        body: &str,
        mut dispatch: F,
    ) -> SyncOutcome
    where
        F: FnMut(&str) -> SyncResponse,
    {
        let pinned = settings.pinned.as_str();
        let target = self.target_version(settings, user_agent);
        let upgraded = if target == pinned {
            None
        } else {
            self.rebuild_body(body, pinned, &target)
        };
        // When the upgrade could not be verified, the body still carries the pin.
        let (sent_body, sent_version) = match upgraded {
            Some(ref upgraded) => (upgraded.as_str(), target.as_str()),
            None => (body, pinned),
        };

        let response = dispatch(sent_body);
        let Some(rejection) = read_rejection(&response) else {
            return SyncOutcome {
                response,
                attempts: 1,
            };
        };

        let recovery = self.plan_recovery(settings, &rejection, sent_body, sent_version);
        let retry = match recovery {
            Recovery::Retry {
                body,
                required_version,
            } => (body, required_version),
            Recovery::Hint { reason } => {
                return SyncOutcome {
                    response: hinted_response(&response, &rejection, &reason),
                    attempts: 1,
                };
            }
        };

        let retried = dispatch(&retry.0);
        let Some(retry_rejection) = read_rejection(&retried) else {
            return SyncOutcome {
                response: retried,
                attempts: 2,
            };
        };

        // One retry only: a second rejection is explained, never chased.
        let reason = match self.plan_recovery(settings, &retry_rejection, &retry.0, &retry.1) {
            Recovery::Hint { reason } => reason,
            Recovery::Retry {
                required_version, ..
            } => RecoveryHintReason::SetOverride {
                required_version: Some(required_version),
            },
        };
        SyncOutcome {
            response: hinted_response(&retried, &retry_rejection, &reason),
            attempts: 2,
        }
    }

    /// Decides whether a rejected request can be retried at the floor Anthropic
    /// named, and if not, which hint explains why.
    ///
    /// A named floor is learned even when this request cannot be retried, so the
    /// next request goes out at it. An explicit user pin is absolute, so it is
    /// neither retried nor taught to the floor.
    fn plan_recovery(
        &self,
        settings: &VersionSettings,
        rejection: &ClaudeCodeVersionRejection,
        sent_body: &str,
        sent_version: &str,
    ) -> Recovery {
        if settings.has_override {
            return Recovery::Hint {
                reason: RecoveryHintReason::Override {
                    override_version: settings.pinned.clone(),
                    required_version: rejection.required_version.clone(),
                },
            };
        }
        let Some(required_version) = rejection.required_version.clone() else {
            return Recovery::Hint {
                reason: RecoveryHintReason::SetOverride {
                    required_version: None,
                },
            };
        };

        self.learned_floor.learn(&required_version);
        let Some(body) = self.rebuild_body(sent_body, sent_version, &required_version) else {
            return Recovery::Hint {
                reason: RecoveryHintReason::SetOverride {
                    required_version: Some(required_version),
                },
            };
        };
        if higher_version(sent_version, Some(&required_version)) == sent_version {
            return Recovery::Hint {
                reason: RecoveryHintReason::UpgradePi {
                    sent_version: sent_version.to_string(),
                },
            };
        }

        Recovery::Retry {
            body,
            required_version,
        }
    }
}

/// What to do about a `claude_code_version_too_old` rejection.
enum Recovery {
    Retry {
        body: String,
        required_version: String,
    },
    Hint {
        reason: RecoveryHintReason,
    },
}

/// Reads a response as a `claude_code_version_too_old` rejection.
///
/// Only a 400 is inspected, so a success response and any other error reach the
/// caller exactly as received.
fn read_rejection(response: &SyncResponse) -> Option<ClaudeCodeVersionRejection> {
    if response.status != 400 {
        return None;
    }
    read_claude_code_version_rejection(&response.body)
}

/// Rebuilds a rejection with the recovery hint appended to `error.message`.
///
/// The status is kept, so the transport still raises the same client error; the
/// body is re-serialized because it is decoded and differently sized.
fn hinted_response(
    response: &SyncResponse,
    rejection: &ClaudeCodeVersionRejection,
    reason: &RecoveryHintReason,
) -> SyncResponse {
    SyncResponse {
        status: response.status,
        body: rejection.with_hint(&describe_recovery_hint(reason)),
    }
}

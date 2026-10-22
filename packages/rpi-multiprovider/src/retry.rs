//! Pi 1.3.0 `isRetryableAssistantError`, used by Fabric failover classification.
//!
//! Ports `@earendil-works/pi-ai` `dist/utils/retry.js` (Pi 1.3.0). The upstream
//! function is two case-insensitive regular expressions: a quota/billing list
//! that always wins, and a transient-failure list that decides the rest.
//!
//! The patterns are kept verbatim from upstream; the only rewrite is `.?`,
//! whose JavaScript dot excludes `\r`, `\u2028` and `\u2029` while the regex
//! crate's dot only excludes `\n`. Every `.` in the upstream patterns is a
//! metacharacter, so the substitution is total. Case folding follows the regex
//! crate's Unicode-aware `(?i)`; see `tests/retry-evidence.json` for the
//! non-ASCII limit this leaves.
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// JavaScript's `.` character class (non-Unicode regex: no `\r`, `\u2028`, `\u2029`).
const DOT: &str = r"[^\r\n\u{2028}\u{2029}]";

/// `NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERN`, in upstream order.
const NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERNS: &[&str] = &[
    // OpenCode Go/free-tier limits returned as 429 JSON error types by OpenCode's
    // Zen API. These are subscription/account limits, not transient throttles.
    "GoUsageLimitError",
    "FreeUsageLimitError",
    // OpenCode Go subscription-limit text asks users to enable available-balance
    // usage after rolling/weekly/monthly limits are reached.
    "Monthly usage limit reached",
    "available balance",
    // Generic quota/budget/billing exhaustion. `insufficient_quota` is OpenAI's
    // quota/billing error code; the other strings cover common gateway wording.
    "insufficient_quota",
    "out of budget",
    "quota exceeded",
    "billing",
    // Sign in with ChatGPT: the subscription's shared usage limit, which resets
    // after hours rather than seconds.
    "subscription_sharing_usage_limit_exceeded",
];

/// `RETRYABLE_PROVIDER_ERROR_PATTERN`, in upstream order.
const RETRYABLE_PROVIDER_ERROR_PATTERNS: &[&str] = &[
    // Generic provider load, HTTP status, and server-side transient failures.
    "overloaded",
    "server_busy",
    "servers are currently busy",
    "currently experiencing high demand",
    "model is at capacity",
    "rate.?limit",
    "too many requests",
    "429",
    "500",
    "502",
    "503",
    "504",
    "520",
    "524",
    // Also matches "The service is temporarily unavailable" (#10965).
    "service.?(?:is.?)?(?:temporarily.?)?unavailable",
    "server.?error",
    "internal.?error",
    // Wrapper/provider text for transient upstream failures, including OpenRouter
    // "Provider returned error" responses (#2264) and gateway wording such as
    // "The upstream provider returned an error" (#10842).
    "provider.?returned.?(?:an.?)?error",
    "exceeded request buffer limit while retrying upstream",
    // Gateway failure relaying the upstream response stream, sent as an OpenAI
    // Responses `response.failed` event without an error code (#10920).
    "unable to forward upstream",
    // Network, proxy, and fetch transport failures. This includes OpenAI Codex
    // raw-fetch failures such as "upstream connect", "connection refused", and
    // "reset before headers" (#733), plus OpenRouter connection drops (#3317).
    "network.?error",
    "connection.?error",
    "connection.?refused",
    "connection.?lost",
    "other side closed",
    "fetch failed",
    "getaddrinfo",
    "ENOTFOUND",
    "EAI_AGAIN",
    "upstream.?connect",
    "reset before headers",
    "socket hang up",
    "socket connection was closed",
    "timed? out",
    "timeout",
    "terminated",
    // WebSocket transports can report close/error text instead of HTTP/fetch text.
    "websocket.?closed",
    "websocket.?error",
    // Premature stream endings from SDKs and transports. Anthropic can throw
    // "stream ended without ..." and "Anthropic stream ended before message_stop"
    // (#4433); Bedrock/Smithy can throw an HTTP/2 no-response error (#3594).
    "ended without",
    "stream ended before message_stop",
    "stream ended before a terminal response event",
    "http2 request did not get a response",
    // Node ERR_HTTP2_STREAM_CANCEL: the HTTP/2 session died before the request was
    // sent, e.g. after the Bedrock SDK's 5-minute session timeout (#10379).
    "pending stream has been canceled",
    // Provider-requested retry delay cap failures should flow through the outer
    // retry policy so callers can surface/abort the backoff (#1123).
    "retry delay",
    // Explicit retry guidance emitted mid-stream by OpenAI Responses and Bedrock
    // stream exceptions (#6019).
    "you can retry your request",
    "try your request again",
    "please retry your request",
    // gRPC based providers (e.g. NVIDIA NIM)
    "ResourceExhausted",
    // Sign in with ChatGPT: usage or user data temporarily unavailable. Usage
    // failures can arrive mid-stream without an HTTP 503 in the message.
    "subscription_sharing_usage_unavailable",
    "subscription_sharing_user_unavailable",
];

fn compile(patterns: &[&str]) -> Regex {
    let joined = patterns
        .iter()
        .map(|pattern| pattern.replace('.', DOT))
        .collect::<Vec<_>>()
        .join("|");
    Regex::new(&format!("(?i){joined}")).expect("static retry patterns compile")
}

fn non_retryable() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| compile(NON_RETRYABLE_PROVIDER_LIMIT_ERROR_PATTERNS))
}

fn retryable() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| compile(RETRYABLE_PROVIDER_ERROR_PATTERNS))
}

/// The classifier's view of an assistant message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub stop_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

/// Classifies whether a failed assistant message looks like a transient provider
/// or transport error, so callers can decide if the last assistant turn should be
/// restarted. A quota/billing match wins over any transient match; a missing or
/// empty `errorMessage` is never retryable, and neither is a non-`error` stop
/// reason.
pub fn is_retryable_assistant_error(message: &AssistantMessage) -> bool {
    if message.stop_reason != "error" {
        return false;
    }
    let Some(error_message) = message.error_message.as_deref() else {
        return false;
    };
    if error_message.is_empty() {
        return false;
    }
    if non_retryable().is_match(error_message) {
        return false;
    }
    retryable().is_match(error_message)
}

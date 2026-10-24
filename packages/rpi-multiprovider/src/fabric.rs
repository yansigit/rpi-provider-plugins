//! Fabric failover compaction decisions from `extensions/multiprovider.ts`
//! (upstream bd97d65), Pi 1.3.0.
//!
//! Upstream, `handleFailover` claims the transition and launches a background
//! `runFailoverCompaction` when pi-fabric is installed and the buffered failure
//! is host-retryable. Compaction shrinks the session during the retry backoff
//! so the next account serves a small prefill instead of a cold full context.
//!
//! Fabric execution itself is out of scope: the decision layer is expressed
//! behind [`FabricHost`] so a caller can inject the real Pi context (tool
//! registry, idle probe, pool snapshot, `ctx.compact`) and tests can script it.
//! The native service binding is a later lane; [`decide_failover`] is the
//! integration point for [`crate::lift::Host::failover`].
use crate::lift::Failover;
use crate::retry::{is_retryable_assistant_error, AssistantMessage};
use async_trait::async_trait;
use serde_json::Value;

/// `FAILOVER_COMPACT_DEBOUNCE_MS`.
pub const FAILOVER_COMPACT_DEBOUNCE_MS: i64 = 30_000;
/// `FAILOVER_COMPACT_IDLE_TIMEOUT_MS`.
pub const FAILOVER_COMPACT_IDLE_TIMEOUT_MS: i64 = 15_000;
/// The 50 ms poll the idle wait sleeps between `ctx.isIdle()` probes.
pub const FAILOVER_COMPACT_POLL_MS: u64 = 50;
/// The tool whose presence makes deterministic compaction available.
pub const FABRIC_EXEC_TOOL: &str = "fabric_exec";
/// `onComplete` notice emitted after a successful compaction.
pub const FAILOVER_COMPACTED_NOTICE: &str =
    "multiprovider: compacted session context before account failover";

/// Whether `ctx.compact` reported completion (`onComplete`) or failure (`onError`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactOutcome {
    Complete,
    /// A newer run won idle-only admission; no error or notice is surfaced.
    Skipped,
    Error,
}

/// The host surface the failover decision layer needs. Every method mirrors one
/// upstream expression; the native binding supplies the real Pi context.
#[async_trait]
pub trait FabricHost: Send + Sync {
    /// `currentContext !== undefined`.
    fn has_context(&self) -> bool {
        true
    }
    /// `pi.getAllTools().map(tool => tool.name)`; an error models the upstream
    /// `try`/`catch` that makes a throwing tool listing count as unavailable.
    fn tool_names(&self) -> Result<Vec<String>, String>;
    /// `Date.now()` in milliseconds, or `None` when cancelled.
    fn now_ms(&self) -> Option<i64>;
    /// `ctx.isIdle()`.
    fn is_idle(&self) -> bool;
    /// The `setTimeout(resolve, 50)` inside the idle wait.
    async fn sleep_ms(&self, ms: u64);
    /// `(await service.snapshot()).providers.find(candidate => candidate.id === providerId)`
    /// then `.accounts.filter(account => account.enabled).length`: `Ok(None)` is
    /// a missing pool, `Err` is a throwing snapshot.
    async fn enabled_accounts(&self, provider_id: &str) -> Result<Option<usize>, String>;
    /// `ctx.compact({ onComplete, onError })`.
    fn compact(&self) -> CompactOutcome;
    /// `ctx.ui.notify(message, 'info')`.
    fn notify(&self, message: &str, level: &str);
}

/// The `lastFailoverCompactAt` closure variable, shared by every provider of
/// one extension instance.
#[derive(Clone, Copy, Debug, Default)]
pub struct CompactionState {
    pub last_failover_compact_at: i64,
}

/// Why a compaction attempt ended. Upstream collapses the guards into early
/// returns; the variants exist for tests and diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionOutcome {
    NoContext,
    Cancelled,
    Debounced,
    NoPool,
    SnapshotError,
    TooFewAccounts,
    NotIdle,
    Compacted,
}

/// `handleFailover`'s return value plus whether it launched compaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision {
    /// True tells the stream to surface the buffered error instead of rotating
    /// accounts inline.
    pub handled: bool,
    /// True means upstream would have called `void runFailoverCompaction(info)`.
    pub compaction: bool,
}

/// `isFabricCompactionAvailable()`.
pub fn is_fabric_compaction_available<H: FabricHost + ?Sized>(host: &H) -> bool {
    host.tool_names()
        .map(|names| names.iter().any(|name| name == FABRIC_EXEC_TOOL))
        .unwrap_or(false)
}

/// The synchronous part of `handleFailover`, after the integration's own
/// `onFailover` already produced `handled`. Claims the transition and requests
/// compaction only when pi-fabric is installed and the buffered failure is
/// host-retryable; otherwise it returns `handled` unchanged.
pub fn decide<H: FabricHost + ?Sized>(
    host: &H,
    failure: Option<&AssistantMessage>,
    handled: bool,
) -> Decision {
    if !is_fabric_compaction_available(host) {
        return Decision {
            handled,
            compaction: false,
        };
    }
    // Compaction surfaces the failure and relies on Pi to restart the turn.
    // Quota/auth failures are not host-retryable, so keep rotating inline.
    if failure.map(is_retryable_assistant_error) != Some(true) {
        return Decision {
            handled,
            compaction: false,
        };
    }
    Decision {
        handled: true,
        compaction: true,
    }
}

/// Extracts the assistant message from a lift failure for [`decide`].
pub fn assistant_message(value: Option<&Value>) -> Option<AssistantMessage> {
    let value = value?;
    Some(AssistantMessage {
        stop_reason: value.get("stopReason")?.as_str()?.to_owned(),
        error_message: value
            .get("errorMessage")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// [`decide`] for the lift's failover hook.
pub fn decide_failover<H: FabricHost + ?Sized>(
    host: &H,
    failover: &Failover<'_>,
    handled: bool,
) -> Decision {
    decide(
        host,
        assistant_message(failover.failure.assistant_message.as_ref()).as_ref(),
        handled,
    )
}

/// The body of `runFailoverCompaction`. Upstream fires this without awaiting it
/// (`void runFailoverCompaction(info)`); a caller may `tokio::spawn` it or await
/// it, which is what tests do.
pub async fn run_compaction<H: FabricHost + ?Sized>(
    host: &H,
    state: &mut CompactionState,
    provider_id: &str,
) -> CompactionOutcome {
    if !host.has_context() {
        return CompactionOutcome::NoContext;
    }
    let Some(started_at) = host.now_ms() else {
        return CompactionOutcome::Cancelled;
    };
    if started_at.saturating_sub(state.last_failover_compact_at) < FAILOVER_COMPACT_DEBOUNCE_MS {
        return CompactionOutcome::Debounced;
    }
    match host.enabled_accounts(provider_id).await {
        Err(_) => return CompactionOutcome::SnapshotError,
        Ok(None) => return CompactionOutcome::NoPool,
        Ok(Some(count)) if count < 2 => return CompactionOutcome::TooFewAccounts,
        Ok(Some(_)) => {}
    }
    // Wait out the failing run so compaction never aborts an active stream;
    // ctx.compact() aborts the current run as its first step.
    let deadline = started_at.saturating_add(FAILOVER_COMPACT_IDLE_TIMEOUT_MS);
    while !host.is_idle() {
        let Some(now) = host.now_ms() else {
            return CompactionOutcome::Cancelled;
        };
        if now >= deadline {
            break;
        }
        host.sleep_ms(FAILOVER_COMPACT_POLL_MS).await;
    }
    let Some(now) = host.now_ms() else {
        return CompactionOutcome::Cancelled;
    };
    if !host.is_idle()
        || now.saturating_sub(state.last_failover_compact_at) < FAILOVER_COMPACT_DEBOUNCE_MS
    {
        return CompactionOutcome::NotIdle;
    }
    state.last_failover_compact_at = now;
    if host.compact() == CompactOutcome::Complete {
        host.notify(FAILOVER_COMPACTED_NOTICE, "info");
    }
    CompactionOutcome::Compacted
}

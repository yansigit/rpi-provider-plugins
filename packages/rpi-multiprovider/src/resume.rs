//! The extension's session-pin resume layer (`applyRecordedPins`,
//! `session_start` pin derivation and the follower notification that follows a
//! restored pool).
//!
//! Journal parsing and replay themselves live in [`crate::pins`]; this module
//! owns the ordering the extension adds on top: recorded decisions first,
//! inherited env pins rebound onto the child session's affinity key and only
//! for pools the journal does not already decide, stale pins reported and
//! dropped, and one `notifyActiveAccountChanged` per pool that reached the
//! scheduler.
use crate::pins::{self, InheritedPin, SessionPin};
use crate::services::Service;
use indexmap::IndexSet;
use serde_json::{json, Map, Value};
use std::collections::HashSet;

/// `pendingSessionPins` / `pendingInheritedSessionPins` as `session_start`
/// leaves them before the first reconcile.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    pub session: Vec<SessionPin>,
    pub inherited: Vec<InheritedPin>,
}

/// The complete effect of one `applyRecordedPins` pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Restored {
    /// What stayed pending for the next reconcile.
    pub pending: Pending,
    /// Pools whose decision reached the scheduler, in first-restored order.
    pub restored_pools: Vec<String>,
    /// Custom journal entries appended during the pass, in order.
    pub journal: Vec<Value>,
    /// `ctx.ui.notify` calls as `[message, level]`, in order.
    pub notifications: Vec<(String, String)>,
}

/// `session_start`: the journal's latest decision per pool, then the inherited
/// env pins for pools the journal does not mention.
pub fn session_start_state(entries: &[Value], env: &Value) -> Pending {
    let session = pins::from_entries(entries);
    let recorded: HashSet<&str> = session.iter().map(|pin| pin.pool.as_str()).collect();
    let inherited = pins::inherited_from_env(env)
        .into_iter()
        .filter(|pin| !recorded.contains(pin.pool.as_str()))
        .collect();
    Pending { session, inherited }
}

fn restore_warning(pin: &SessionPin, error: &str) -> String {
    let target = match &pin.label {
        Some(label) => format!("\"{label}\""),
        None => pin.account_id.clone().unwrap_or_default(),
    };
    format!(
        "multiprovider: pinned account {target} could not be restored for \"{}\": {error}",
        pin.pool
    )
}

fn journal_entry(pin: &SessionPin) -> Value {
    let mut entry = Map::new();
    entry.insert("pool".into(), json!(pin.pool));
    entry.insert("key".into(), json!(pin.key));
    if let Some(account_id) = &pin.account_id {
        entry.insert("accountId".into(), json!(account_id));
    }
    if let Some(label) = &pin.label {
        entry.insert("label".into(), json!(label));
    }
    Value::Object(entry)
}

fn inherited(pin: &SessionPin) -> InheritedPin {
    InheritedPin {
        pool: pin.pool.clone(),
        account_id: pin.account_id.clone(),
        label: pin.label.clone(),
    }
}

/// `applyRecordedPins`. `affinity_key` is `affinityKeyForPool`: the integration's
/// own key when it defines one, otherwise the Pi session id.
pub async fn apply_recorded_pins(
    service: &Service,
    entries: &[Value],
    pending: Pending,
    affinity_key: impl Fn(&str) -> String,
) -> Restored {
    let mut restored: IndexSet<String> = IndexSet::new();
    let mut notifications: Vec<(String, String)> = Vec::new();
    let mut journal: Vec<Value> = Vec::new();
    let mut pending_session = pending.session;
    let mut pending_inherited = pending.inherited;

    if !pending_session.is_empty() {
        pending_session = pins::apply(
            &pending_session,
            service,
            |pin, error| {
                notifications.push((restore_warning(pin, &error.to_string()), "warning".into()))
            },
            |pin| {
                restored.insert(pin.pool.clone());
            },
        )
        .await;
    }

    if !pending_inherited.is_empty() {
        let mut recorded: HashSet<String> = pending_session
            .iter()
            .map(|pin| pin.pool.clone())
            .collect();
        for pin in pins::from_entries(entries) {
            recorded.insert(pin.pool);
        }
        let mut ready: Vec<SessionPin> = Vec::new();
        let mut still_pending: Vec<InheritedPin> = Vec::new();
        for pin in pending_inherited {
            if recorded.contains(&pin.pool) {
                continue;
            }
            if !service.has_provider(&pin.pool) {
                still_pending.push(pin);
                continue;
            }
            ready.push(SessionPin {
                pool: pin.pool.clone(),
                key: affinity_key(&pin.pool),
                account_id: pin.account_id,
                label: pin.label,
            });
        }
        let mut appended = Vec::new();
        let leftover = pins::apply(
            &ready,
            service,
            |pin, error| {
                notifications.push((restore_warning(pin, &error.to_string()), "warning".into()))
            },
            |pin| {
                restored.insert(pin.pool.clone());
                appended.push(journal_entry(pin));
            },
        )
        .await;
        journal.extend(appended);
        pending_inherited = still_pending
            .into_iter()
            .chain(leftover.iter().map(inherited))
            .collect();
    }

    Restored {
        pending: Pending {
            session: pending_session,
            inherited: pending_inherited,
        },
        restored_pools: restored.into_iter().collect(),
        journal,
        notifications,
    }
}

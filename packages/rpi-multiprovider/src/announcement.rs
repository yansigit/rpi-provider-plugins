//! Port of `src/announcement.ts`: active-account inspection and change notifications.
use crate::managed::{
    AccountRequest, AuthResult, Headers, ModelRef, NativeProvider, ProviderIntegration,
    PI_UPSTREAM_ACCOUNT_ID,
};
use crate::services::Service;
use crate::types::trim;
use serde::Serialize;
use serde_json::{json, Map};
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// The host context a notification and its lookups receive (`MultiProviderServiceContext`).
pub trait ServiceContext: Clone + Send + Sync + 'static {
    /// `ctx.model`, when the session has one selected.
    fn model(&self) -> Option<ModelRef>;
}

/// `ActiveAccount`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAccount {
    pub id: String,
    pub label: String,
    pub auth_kind: String,
}

/// `ActiveAccountAuth`.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAccountAuth {
    pub access_token: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// `ActiveAccountChangedEvent`.
pub struct ActiveAccountChangedEvent<C> {
    pub provider_id: String,
    pub account: Option<ActiveAccount>,
    pub ctx: C,
}

/// A change listener. An `Err` (the upstream throw) is isolated from sibling listeners.
pub type Listener<C> = dyn Fn(&ActiveAccountChangedEvent<C>) -> Result<(), String> + Send + Sync;
/// Removes one subscription. Callable more than once, like the upstream closure.
pub type Unsubscribe = Box<dyn Fn() + Send + Sync>;

pub type IntegrationLookup = dyn Fn(&str) -> Option<Arc<dyn ProviderIntegration>> + Send + Sync;
pub type BaseProviderLookup<C> = dyn Fn(&str, &C) -> Option<Arc<dyn NativeProvider>> + Send + Sync;
pub type AffinityKeyFor<C> =
    dyn Fn(&dyn ProviderIntegration, &C, &str) -> Result<String, String> + Send + Sync;

/// `AnnouncementDependencies`.
pub struct AnnouncementDependencies<C> {
    pub scheduler: Service,
    pub get_integration: Arc<IntegrationLookup>,
    pub get_base_provider: Arc<BaseProviderLookup<C>>,
    pub affinity_key_for: Arc<AffinityKeyFor<C>>,
}

struct Entry<C> {
    seq: u64,
    listener: Arc<Listener<C>>,
}

// One listener set per provider. `id` identifies the set object itself, so a notification
// that started on a set that was emptied and removed stops, as the upstream `Set` does.
struct ListenerSet<C> {
    id: u64,
    entries: Vec<Entry<C>>,
}

struct Registry<C> {
    next_set_id: u64,
    next_seq: u64,
    sets: HashMap<String, ListenerSet<C>>,
}

/// The in-process service announced to consumers (`MultiProviderServiceAnnouncement`
/// plus the `notifyActiveAccountChanged` hook used after `/switch-account`).
pub struct ServiceAnnouncement<C: ServiceContext> {
    deps: AnnouncementDependencies<C>,
    registry: Arc<Mutex<Registry<C>>>,
}

fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t'..='\r'
            | ' '
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// `/^bearer\s+(.+)$/i` applied to the trimmed value, then trimmed again. JavaScript `.`
/// does not match line terminators, and `\s+` is greedy with no room to backtrack past a
/// terminator, so the capture is the remainder after the whitespace run.
fn bearer_capture(value: &str) -> Option<String> {
    let text = trim(value);
    if text.len() < 6 || !text.is_char_boundary(6) || !text[..6].eq_ignore_ascii_case("bearer") {
        return None;
    }
    let rest = &text[6..];
    let body = rest.trim_start_matches(is_js_space);
    if body.len() == rest.len() || body.is_empty() || body.chars().any(is_line_terminator) {
        return None;
    }
    Some(trim(body).to_owned())
}

/// `bearerTokenFromHeaders`: the first `authorization` header with a bearer capture.
fn bearer_token_from_headers(headers: Option<&Headers>) -> Option<String> {
    for (name, value) in headers?.iter() {
        if name.to_lowercase() != "authorization" {
            continue;
        }
        let Some(text) = value.as_str() else {
            continue;
        };
        if let Some(token) = bearer_capture(text) {
            return Some(token);
        }
    }
    None
}

impl<C: ServiceContext> ServiceAnnouncement<C> {
    pub fn new(deps: AnnouncementDependencies<C>) -> Self {
        Self {
            deps,
            registry: Arc::new(Mutex::new(Registry {
                next_set_id: 0,
                next_seq: 0,
                sets: HashMap::new(),
            })),
        }
    }

    /// `activeAccount`: the session's explicit pin, else the scheduler's last selection
    /// while affinity is on. Key and pin lookups reject; a missing integration or pool does not.
    async fn active_account(
        &self,
        provider_id: &str,
        ctx: &C,
    ) -> Result<Option<ActiveAccount>, String> {
        let Some(integration) = (self.deps.get_integration)(provider_id) else {
            return Ok(None);
        };
        let Ok(preference) = self.deps.scheduler.pool_preference(provider_id) else {
            return Ok(None);
        };
        let key = (self.deps.affinity_key_for)(&*integration, ctx, provider_id)?;
        let pin = self
            .deps
            .scheduler
            .affinity(provider_id, &key)
            .map_err(|error| error.to_string())?;
        let Some(pin) = pin else {
            return Ok(None);
        };
        if !pin.explicit && !preference.affinity {
            return Ok(None);
        }
        let accounts = integration.accounts().await?;
        let Some(account) = accounts
            .into_iter()
            .find(|candidate| candidate.id == pin.account_id)
        else {
            return Ok(None);
        };
        Ok(Some(ActiveAccount {
            id: account.id,
            label: account.label,
            auth_kind: account.auth_kind,
        }))
    }

    /// `getActiveAccount`.
    pub async fn get_active_account(
        &self,
        provider_id: &str,
        ctx: &C,
    ) -> Result<Option<ActiveAccount>, String> {
        self.active_account(provider_id, ctx).await
    }

    /// `resolveActiveAccountAuth`: never resolves the upstream account. Resolver failures
    /// and blank tokens yield `None`; account listing and key errors reject.
    pub async fn resolve_active_account_auth(
        &self,
        provider_id: &str,
        ctx: &C,
        signal: Option<&CancellationToken>,
    ) -> Result<Option<ActiveAccountAuth>, String> {
        let Some(active) = self.active_account(provider_id, ctx).await? else {
            return Ok(None);
        };
        if active.id == PI_UPSTREAM_ACCOUNT_ID {
            return Ok(None);
        }
        let integration = (self.deps.get_integration)(provider_id);
        let base = (self.deps.get_base_provider)(provider_id, ctx);
        let (Some(integration), Some(base)) = (integration, base) else {
            return Ok(None);
        };
        let Some(model) = ctx.model().or_else(|| base.models().into_iter().next()) else {
            return Ok(None);
        };
        let accounts = integration.accounts().await?;
        let Some(account) = accounts
            .into_iter()
            .find(|candidate| candidate.id == active.id)
        else {
            return Ok(None);
        };
        let signal = signal.cloned().unwrap_or_default();
        let request = AccountRequest {
            provider: base,
            model,
            context: json!({ "messages": [] }),
            request_options: Map::new(),
            signal: signal.clone(),
        };
        let resolution: AuthResult =
            match integration.resolve_auth(&account, &signal, &request).await {
                Ok(resolution) => resolution,
                Err(_) => return Ok(None),
            };
        let token = resolution
            .auth
            .api_key
            .clone()
            .or_else(|| bearer_token_from_headers(resolution.auth.headers.as_ref()));
        let Some(token) = token else {
            return Ok(None);
        };
        let token = trim(&token);
        if token.is_empty() {
            return Ok(None);
        }
        Ok(Some(ActiveAccountAuth {
            access_token: token.to_owned(),
            label: active.label,
            source: resolution.source,
        }))
    }

    /// `onActiveAccountChanged`. Subscribing the same listener twice keeps one entry.
    pub fn on_active_account_changed(
        &self,
        provider_id: &str,
        listener: Arc<Listener<C>>,
    ) -> Unsubscribe {
        {
            let mut registry = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !registry.sets.contains_key(provider_id) {
                registry.next_set_id += 1;
                let id = registry.next_set_id;
                registry.sets.insert(
                    provider_id.to_owned(),
                    ListenerSet {
                        id,
                        entries: Vec::new(),
                    },
                );
            }
            let duplicate = registry.sets[provider_id]
                .entries
                .iter()
                .any(|entry| Arc::ptr_eq(&entry.listener, &listener));
            if !duplicate {
                registry.next_seq += 1;
                let seq = registry.next_seq;
                registry
                    .sets
                    .get_mut(provider_id)
                    .expect("set exists")
                    .entries
                    .push(Entry {
                        seq,
                        listener: listener.clone(),
                    });
            }
        }
        let registry = self.registry.clone();
        let provider_id = provider_id.to_owned();
        Box::new(move || {
            let mut registry = registry.lock().unwrap_or_else(|error| error.into_inner());
            let Some(set) = registry.sets.get_mut(&provider_id) else {
                return;
            };
            set.entries
                .retain(|entry| !Arc::ptr_eq(&entry.listener, &listener));
            if set.entries.is_empty() {
                registry.sets.remove(&provider_id);
            }
        })
    }

    /// `notifyActiveAccountChanged`: visits listeners in subscription order, live. A listener
    /// removed earlier in the same pass is skipped, one added during it is visited, and
    /// a listener that throws (or panics) does not stop the rest.
    pub fn notify_active_account_changed(
        &self,
        provider_id: &str,
        ctx: &C,
        account: Option<ActiveAccount>,
    ) {
        let set_id = {
            let registry = self
                .registry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match registry.sets.get(provider_id) {
                Some(set) => set.id,
                None => return,
            }
        };
        let mut last: Option<u64> = None;
        loop {
            let next = {
                let registry = self
                    .registry
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                match registry.sets.get(provider_id) {
                    Some(set) if set.id == set_id => set
                        .entries
                        .iter()
                        .find(|entry| last.map_or(true, |seq| entry.seq > seq))
                        .map(|entry| (entry.seq, entry.listener.clone())),
                    _ => None,
                }
            };
            let Some((seq, listener)) = next else {
                return;
            };
            last = Some(seq);
            let event = ActiveAccountChangedEvent {
                provider_id: provider_id.to_owned(),
                account: account.clone(),
                ctx: ctx.clone(),
            };
            let _ = catch_unwind(AssertUnwindSafe(|| listener(&event)));
        }
    }
}

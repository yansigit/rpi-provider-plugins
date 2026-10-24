//! Native rpi extension entry point for multiprovider.
//!
//! This module owns the process-facing registration and the session reconcile
//! loop. Provider request routing itself lives in [`lift`]; account-store
//! transactions live in [`credentials`].
//!
//! # Composition contract
//!
//! The extension registers exactly one provider wrapper with an omitted provider
//! list, so the host applies it in place to every concrete provider from its
//! unwrapped base. The host rebuilds those chains on every catalog publication,
//! which is what Pi's `install()` gets from lifting the current provider once and
//! from `isLiftOf()` recognizing its own composed copy. This module never wraps
//! another registration's wrapper or copies a provider object. Virtual aliases
//! are owned catalog entries backed by a fail-closed base; only this wrapper
//! has the continuation authority needed to dispatch them.
//!
//! A provider with no stored pool is delegated unchanged (ambient auth, no
//! scheduler lease), so installing multiprovider cannot alter its identity.
pub mod command_port;
pub mod commands;
pub mod credentials;
mod fabric;
pub mod lift;
pub mod services;
pub mod virtuals;

use crate::lift::Host;
use crate::pins::{self, InheritedPin, SessionPin};
use crate::services::{Options as ServiceOptions, Registration, Service};
use crate::store::Store;
use crate::types::{Account, PoolPatch, SelectionBias};
use lift::{NativeHost, STORE_ID, UPSTREAM_ACCOUNT_ID};
use rpi_plugin_sdk::{
    callbacks::OwnedString, AuthRequestControl, EventTag, PluginApi, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    ffi::c_void,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

pub const WRAPPER_ID: &str = "multiprovider";
pub const STORE_FILE: &str = "multiprovider-auth.json";
const MAX_ACCOUNT_ATTEMPTS: f64 = 9_007_199_254_740_991.0;

/// Effective capability floor for this implementation. Missing names are
/// reported at registration instead of silently degrading behavior.
const REQUIRED_CAPABILITIES: [&str; 6] = [
    "json.exactNumbers",
    "context.inspect",
    "context.bootstrap",
    "providers.childCatalogs",
    "providers.scopedCallbacks",
    "sessions.view",
];
const COMMAND_CAPABILITIES: [&str; 3] = ["commands.owned", "ui.notifications", "context.snapshot"];

type Inventory = Pin<Box<dyn std::future::Future<Output = Result<Vec<Account>, String>> + Send>>;

/// Per-execution-scope state. A parent and each child session have independent
/// pins and affinity; nothing here is keyed by process-global "current session".
#[derive(Default)]
pub(super) struct Scope {
    pub session_id: Option<String>,
    pub child: bool,
    pub pins: Vec<SessionPin>,
    pub pending: Vec<SessionPin>,
    pub inherited: Vec<InheritedPin>,
    pub pending_inherited: Vec<InheritedPin>,
    pub origin: Option<String>,
    pub restore_errors: Vec<String>,
}

pub(super) struct State {
    pub locks: HashMap<String, (String, String, Box<credentials::Held>)>,
    scopes: HashMap<Option<String>, Scope>,
    registrations: HashMap<String, Arc<Registration>>,
    warned_missing: HashSet<String>,
}

/// One native registration. Every callback borrows this value; the lifecycle
/// hooks free it only after awaited work has drained.
pub struct Runtime {
    pub(super) api: PluginApi,
    pub(super) shared: Arc<Shared>,
    commands: Mutex<Option<commands::Commands<()>>>,
}

/// Operational state shared by every native entry point and the future
/// command runner. It deliberately has no ownership of `Runtime` or Commands.
pub struct Shared {
    pub(super) context: rpi_plugin_sdk::provider::ProviderContext,
    pub(super) store: Store,
    pub(super) service: Service,
    pub(super) client: rpi_plugin_sdk::provider::native::Client,
    pub(super) state: Mutex<State>,
    pub(super) stopping: CancellationToken,
    registrations: tokio::sync::Mutex<()>,
    reconcile: tokio::sync::Mutex<()>,
    virtuals: tokio::sync::Mutex<HashMap<Option<String>, virtuals::Catalog>>,
    compactions: fabric::Tasks,
}

impl std::ops::Deref for Runtime {
    type Target = Shared;
    fn deref(&self) -> &Shared {
        &self.shared
    }
}

impl Runtime {
    fn open(api: &PluginApi) -> Result<Self, String> {
        let bootstrap = sync_request(api, json!({"port":"context","action":"bootstrap"}))?;
        let agent_dir = bootstrap["agentDir"]
            .as_str()
            .ok_or("host did not report its agent directory")?;
        let client = rpi_plugin_sdk::provider::native::Client::new(api)?.scoped()?;
        check_capabilities(api)?;
        Ok(Self {
            api: *api,
            commands: Mutex::new(None),
            shared: Arc::new(Shared {
                context: rpi_plugin_sdk::provider::ProviderContext::new(),
                store: Store::new(PathBuf::from(agent_dir).join(STORE_FILE)),
                service: Service::new(ServiceOptions::default()),
                client,
                state: Mutex::new(State {
                    locks: HashMap::new(),
                    scopes: HashMap::new(),
                    registrations: HashMap::new(),
                    warned_missing: HashSet::new(),
                }),
                stopping: CancellationToken::new(),
                registrations: tokio::sync::Mutex::new(()),
                reconcile: tokio::sync::Mutex::new(()),
                virtuals: tokio::sync::Mutex::new(HashMap::new()),
                compactions: fabric::Tasks::default(),
            }),
        })
    }
}

impl Shared {
    pub(super) async fn store_request(
        &self,
        request: Value,
        signal: CancellationToken,
    ) -> Result<Value, String> {
        self.store
            .request(request, signal)
            .await
            .map_err(credentials::failure_text)
    }

    async fn scoped(&self) -> Result<rpi_plugin_sdk::provider::native::Client, String> {
        let client = self.client.clone();
        tokio::task::spawn_blocking(move || client.scoped())
            .await
            .map_err(|_| "native context capture task stopped".to_owned())?
    }

    async fn inspect(
        &self,
        client: &rpi_plugin_sdk::provider::native::Client,
        signal: &CancellationToken,
    ) -> Result<Value, String> {
        client
            .request(json!({"port":"context","action":"inspect"}), signal.clone())
            .await
    }

    async fn pool(
        &self,
        provider: &str,
        signal: &CancellationToken,
    ) -> Result<Option<Value>, String> {
        let value = self
            .store_request(json!({"op":"pool","provider":provider}), signal.clone())
            .await?;
        Ok((!value.is_null()).then_some(value))
    }

    fn has_accounts(pool: &Value) -> bool {
        pool["accounts"]
            .as_array()
            .is_some_and(|accounts| !accounts.is_empty())
    }

    /// Register the scheduler for one stored pool on first use. Reconcile also
    /// calls this so a pool is live before the first provider request.
    async fn ensure_registration(&self, provider: &str) -> Result<(), String> {
        let _serial = self.registrations.lock().await;
        let exists = self
            .state
            .lock()
            .map_err(|_| "multiprovider state poisoned".to_owned())?
            .registrations
            .contains_key(provider);
        let store = self.store.clone();
        let id = provider.to_owned();
        if !exists {
            let accounts: Arc<dyn Fn() -> Inventory + Send + Sync> = {
                let store = store.clone();
                let id = id.clone();
                Arc::new(move || {
                    let store = store.clone();
                    let id = id.clone();
                    Box::pin(async move { inventory(&store, &id) })
                })
            };
            let registration = Registration {
                id: id.clone(),
                label: id.clone(),
                accounts,
                classify_failure: None,
                management_hint: Some(crate::managed::MANAGEMENT_HINT.to_owned()),
                selection_bias: SelectionBias::FirstAccount,
            };
            let registered = self
                .service
                .register(registration)
                .map_err(|error| error.to_string())?;
            self.state
                .lock()
                .map_err(|_| "multiprovider state poisoned".to_owned())?
                .registrations
                .insert(id.clone(), registered);
        }
        self.apply_pool_preferences(&id).await
    }

    async fn apply_pool_preferences(&self, provider: &str) -> Result<(), String> {
        let Some(pool) = self.pool(provider, &CancellationToken::new()).await? else {
            return Ok(());
        };
        let mut preferences = Vec::new();
        if let Some(accounts) = pool["accounts"].as_array() {
            for account in accounts {
                preferences.push(crate::types::AccountPreference {
                    account_id: account["id"].as_str().unwrap_or_default().to_owned(),
                    enabled: account["enabled"].as_bool().unwrap_or(true),
                    weight: account["weight"].as_f64().unwrap_or(1.0),
                    priority: account["priority"].as_f64().unwrap_or(0.0),
                });
            }
        }
        let policy =
            serde_json::from_value::<crate::types::SelectionPolicy>(pool["policy"].clone())
                .map_err(|_| "invalid stored pool policy".to_owned())?;
        self.service
            .update_pool(
                provider,
                PoolPatch {
                    policy: Some(policy),
                    affinity: Some(pool["affinity"].as_bool().unwrap_or(true)),
                    accounts: Some(preferences),
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    async fn unregister(&self, provider: &str) {
        let registration = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.registrations.remove(provider));
        if let Some(registration) = registration {
            self.service.unregister(&registration);
        }
    }

    /// Reconcile the store, scheduler defaults, registrations and recorded pins
    /// for the current execution scope.
    pub(super) async fn reconcile(
        &self,
        client: &rpi_plugin_sdk::provider::native::Client,
        signal: &CancellationToken,
        capture: bool,
    ) -> Result<(), String> {
        let _serial = self.reconcile.lock().await;
        let info = self.inspect(client, signal).await?;
        let scope_id = info["scopeId"].as_str().map(str::to_owned);
        let child = info["owner"]["child"] == true;
        let session_id = info["identity"]["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .map(str::to_owned);

        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "multiprovider state poisoned".to_owned())?;
            let scope = state.scopes.entry(scope_id.clone()).or_default();
            scope.child = child;
            if session_id.is_some() {
                scope.session_id = session_id;
            }
        }

        if capture {
            self.capture_session(client, scope_id.clone(), signal).await;
        }

        if let Ok(settings) = self
            .store_request(json!({"op":"getScheduler"}), signal.clone())
            .await
        {
            let _ = self.service.update_settings(&settings);
        }

        let ids: Vec<String> = self
            .store_request(json!({"op":"list"}), signal.clone())
            .await?
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let discovered = self.discover(client, signal).await.unwrap_or_default();

        let live: HashSet<&str> = ids.iter().map(String::as_str).collect();
        let stale: Vec<String> = self
            .state
            .lock()
            .map_err(|_| "multiprovider state poisoned".to_owned())?
            .registrations
            .keys()
            .filter(|id| !live.contains(id.as_str()))
            .cloned()
            .collect();
        for provider in stale {
            self.unregister(&provider).await;
        }

        for provider in &ids {
            if !discovered.is_empty() && !discovered.contains(provider) {
                if let Ok(mut state) = self.state.lock() {
                    if state.warned_missing.insert(provider.clone()) {
                        // Missing providers are reported through the reconcile
                        // state; the command/notification lane owns UI delivery.
                    }
                }
                continue;
            }
            self.ensure_registration(provider).await?;
        }

        self.reconcile_virtuals(client, scope_id.clone(), signal)
            .await?;
        self.apply_pins(scope_id, signal).await;
        Ok(())
    }

    async fn discover(
        &self,
        client: &rpi_plugin_sdk::provider::native::Client,
        signal: &CancellationToken,
    ) -> Result<HashSet<String>, String> {
        let value = client
            .request(
                json!({"port":"providers","action":"inspect"}),
                signal.clone(),
            )
            .await?;
        Ok(value["providers"]
            .as_array()
            .map(|providers| {
                providers
                    .iter()
                    .filter_map(|provider| provider["id"].as_str())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn capture_session(
        &self,
        client: &rpi_plugin_sdk::provider::native::Client,
        scope_id: Option<String>,
        signal: &CancellationToken,
    ) {
        let Ok(value) = client
            .request(
                json!({"port":"sessions","action":"capture"}),
                signal.clone(),
            )
            .await
        else {
            return;
        };
        let entries = value["snapshot"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let pins = pins::from_entries(entries.iter());
        let inherited = pins::inherited_from_env(&environment());
        let recorded: HashSet<String> = pins.iter().map(|pin| pin.pool.clone()).collect();
        let inherited: Vec<InheritedPin> = inherited
            .into_iter()
            .filter(|pin| !recorded.contains(&pin.pool))
            .collect();
        if let Ok(mut state) = self.state.lock() {
            let scope = state.scopes.entry(scope_id).or_default();
            scope.pending = pins.clone();
            scope.pins = pins;
            scope.pending_inherited = inherited.clone();
            scope.inherited = inherited;
            scope.origin = value["origin"].as_str().map(str::to_owned);
        }
    }

    async fn release_session(&self, scope_id: Option<String>) {
        let origin = self
            .state
            .lock()
            .ok()
            .and_then(|mut state| state.scopes.remove(&scope_id))
            .and_then(|scope| scope.origin);
        let Some(origin) = origin else {
            return;
        };
        let Ok(client) = self.scoped().await else {
            return;
        };
        let _ = client
            .request(
                json!({"port":"sessions","action":"release","origin":origin}),
                CancellationToken::new(),
            )
            .await;
    }

    async fn apply_pins(&self, scope_id: Option<String>, _signal: &CancellationToken) {
        let (pins, inherited, key, origin) = {
            let Ok(state) = self.state.lock() else {
                return;
            };
            let Some(scope) = state.scopes.get(&scope_id) else {
                return;
            };
            (
                scope.pending.clone(),
                scope.pending_inherited.clone(),
                scope.session_id.clone(),
                scope.origin.clone(),
            )
        };
        // Virtual schedulers belong to this scope's catalog, not the shared
        // ordinary-provider service. Clone the service before awaiting pins.
        let virtual_service = self
            .virtuals
            .lock()
            .await
            .get(&scope_id)
            .map(|catalog| catalog.service.clone());
        let service_for = |pool: &str| {
            virtual_service
                .as_ref()
                .filter(|service| service.has_provider(pool))
                .unwrap_or(&self.service)
        };
        let mut errors = Vec::new();
        let mut pending = Vec::new();
        for pin in &pins {
            pending.extend(
                pins::apply(
                    std::slice::from_ref(pin),
                    service_for(&pin.pool),
                    |pin, error| errors.push(restore_error(pin, &error)),
                    |_| {},
                )
                .await,
            );
        }

        // A pool that still records a decision — applied now or waiting for its
        // scheduler — wins over an inherited parent decision.
        let recorded_pools: HashSet<String> = pending
            .iter()
            .chain(pins.iter())
            .map(|pin| pin.pool.clone())
            .collect();
        let mut ready = Vec::new();
        let mut still_pending = Vec::new();
        for pin in inherited {
            if recorded_pools.contains(&pin.pool) {
                continue;
            }
            let Some(key) = key.as_deref().filter(|key| !key.is_empty()) else {
                still_pending.push(pin);
                continue;
            };
            if !service_for(&pin.pool).has_provider(&pin.pool) {
                still_pending.push(pin);
                continue;
            }
            ready.push(SessionPin {
                pool: pin.pool,
                key: key.to_owned(),
                account_id: pin.account_id,
                label: pin.label,
            });
        }
        let mut applied = Vec::new();
        let mut leftover = Vec::new();
        for pin in &ready {
            leftover.extend(
                pins::apply(
                    std::slice::from_ref(pin),
                    service_for(&pin.pool),
                    |pin, error| errors.push(restore_error(pin, &error)),
                    |pin| applied.push(pin.clone()),
                )
                .await,
            );
        }
        if let Some(origin) = origin {
            for pin in &applied {
                let data = json!({
                    "pool": pin.pool,
                    "key": pin.key,
                    "accountId": pin.account_id,
                    "label": pin.label,
                });
                let _ = self.append(origin.clone(), data).await;
            }
        }
        if let Ok(mut state) = self.state.lock() {
            if let Some(scope) = state.scopes.get_mut(&scope_id) {
                scope.pending = pending;
                scope.pending_inherited = leftover
                    .into_iter()
                    .map(|pin| InheritedPin {
                        pool: pin.pool,
                        account_id: pin.account_id,
                        label: pin.label,
                    })
                    .chain(still_pending)
                    .collect();
                scope.restore_errors.extend(errors);
            }
        }
    }

    /// Append one inherited decision through the captured origin. `journalAppend`
    /// keeps the origin's anchor, so several decisions in one reconcile each
    /// form their own parent link even after earlier appends advanced it.
    async fn append(&self, origin: String, data: Value) -> Result<(), String> {
        let client = self.scoped().await?;
        client
            .request(
                json!({"port":"sessions","action":"journalAppend","origin":origin,
                    "customType":pins::SESSION_PIN_ENTRY_TYPE,"data":data}),
                CancellationToken::new(),
            )
            .await
            .map(|_| ())
    }
}

fn restore_error(pin: &SessionPin, error: &crate::services::Error) -> String {
    let target = pin
        .label
        .clone()
        .or_else(|| pin.account_id.clone())
        .unwrap_or_default();
    format!(
        "multiprovider: pinned account \"{target}\" could not be restored for \"{}\": {error}",
        pin.pool
    )
}

fn environment() -> Value {
    Value::Object(
        std::env::vars()
            .map(|(key, value)| (key, Value::String(value)))
            .collect(),
    )
}

/// Build the scheduler inventory for one provider from the stored pool.
fn inventory(store: &Store, provider: &str) -> Result<Vec<Account>, String> {
    let state = store.read().map_err(credentials::failure_text)?;
    let Some(pool) = state["providers"].get(provider) else {
        return Ok(Vec::new());
    };
    let mut accounts = Vec::new();
    if pool["includeUpstream"].as_bool().unwrap_or(true) {
        let upstream = &pool["upstream"];
        accounts.push(Account {
            id: UPSTREAM_ACCOUNT_ID.into(),
            label: upstream["label"]
                .as_str()
                .unwrap_or("Pi default")
                .to_owned(),
            auth_kind: "custom".into(),
            credential_ref: json!(UPSTREAM_ACCOUNT_ID),
            enabled: Some(true),
            weight: upstream["weight"].as_f64().or(Some(1.0)),
            priority: upstream["priority"].as_f64().or(Some(0.0)),
            metadata: Some(json!({"source":"auth.json, environment, or provider ambient auth"})),
        });
    }
    for account in pool["accounts"].as_array().into_iter().flatten() {
        let Some(id) = account["id"].as_str() else {
            return Err("multiprovider: stored account has no id".into());
        };
        accounts.push(Account {
            id: id.to_owned(),
            label: account["label"].as_str().unwrap_or(id).to_owned(),
            auth_kind: account["authKind"].as_str().unwrap_or("api-key").to_owned(),
            credential_ref: json!(id),
            enabled: account["enabled"].as_bool(),
            weight: account["weight"].as_f64(),
            priority: account["priority"].as_f64(),
            metadata: Some(json!({"stored":true})),
        });
    }
    Ok(accounts)
}

fn sync_request(api: &PluginApi, mut request: Value) -> Result<Value, String> {
    request
        .as_object_mut()
        .ok_or("native request must be an object")?
        .insert("version".into(), 1.into());
    let Some(call) = api.native_request else {
        return Err("host lacks native runtime ports".into());
    };
    let signal = CancellationToken::new();
    let control = AuthRequestControl {
        struct_size: std::mem::size_of::<AuthRequestControl>() as u32,
        is_cancelled: no_cancel,
        user_data: (&signal as *const CancellationToken).cast_mut().cast(),
    };
    let mut out = StbString::empty();
    let status = call(
        StbStringRef::from_str(&request.to_string()),
        &control,
        &mut out,
        api.user_data,
    );
    let out = unsafe { OwnedString::from_raw(out, Some(api.free_string)) };
    let value: Value = serde_json::from_str(&out.to_string_lossy())
        .map_err(|_| "invalid native host response".to_owned())?;
    if status != 0 {
        return Err(value["error"]
            .as_str()
            .unwrap_or("native host request failed")
            .to_owned());
    }
    if value["version"] != 1 {
        return Err("unsupported native host response".into());
    }
    Ok(value
        .get("result")
        .cloned()
        .ok_or_else(|| "native host response omitted its result".to_owned())?)
}

extern "C" fn no_cancel(_: *mut c_void) -> u8 {
    0
}

fn check_capabilities(api: &PluginApi) -> Result<(), String> {
    let response = sync_request(api, json!({"port":"capabilities","action":"inspect"}))?;
    let available = response
        .as_array()
        .ok_or("invalid native capability response")?;
    let missing: Vec<&str> = REQUIRED_CAPABILITIES
        .iter()
        .chain(COMMAND_CAPABILITIES.iter())
        .copied()
        .filter(|name| !available.iter().any(|value| value.as_str() == Some(*name)))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "host is missing required native capabilities: {}",
            missing.join(", ")
        ))
    }
}

/// An alias base has no continuation and must never execute a backend itself.
extern "C" fn virtual_base(
    request: StbStringRef,
    _: Option<rpi_plugin_sdk::ProviderStreamEmitFn>,
    _: *mut c_void,
    _: *const rpi_plugin_sdk::ProviderStreamControl,
    _: Option<rpi_plugin_sdk::ProviderStreamCompletionFn>,
    _: *mut c_void,
    out: *mut StbString,
    _: *mut c_void,
) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if out.is_null() {
            return 2;
        }
        let Ok(request) = serde_json::from_str::<Value>(unsafe { request.as_str() }) else {
            return 2;
        };
        unsafe {
            *out = StbString::from_string(
                error_message(&request, "multiprovider: session not ready", false).to_string(),
            );
        }
        0
    })
}

fn register_virtual_base(api: &PluginApi, state: *mut c_void) -> Result<(), String> {
    let id = virtuals::IMPLEMENTATION_ID;
    let declare = api
        .declare
        .ok_or("multiprovider requires native provider declarations")?;
    let register = api
        .register_streaming_provider
        .ok_or("multiprovider requires native streaming providers")?;
    let status = declare(StbStringRef::from_str(
        &json!({"providers":[{"id":id,"models":[],"requiresAuth":false}]}).to_string(),
    ));
    if status != 0 {
        return Err(format!(
            "multiprovider virtual base declaration failed ({status})"
        ));
    }
    let status = register(
        StbStringRef::from_str(id),
        StbStringRef::from_str(""),
        StbStringRef::from_str("openai-completions"),
        virtual_base,
        rpi_plugin_sdk::callbacks::free_string,
        state,
    );
    if status != 0 {
        return Err(format!(
            "multiprovider virtual base registration failed ({status})"
        ));
    }
    Ok(())
}

fn wrapper_manifest() -> Value {
    json!({"version":1,"id":WRAPPER_ID})
}

fn store_manifest() -> Value {
    json!({"version":1,"id":STORE_ID})
}

extern "C" fn begin_shutdown(context: *mut c_void) {
    rpi_plugin_sdk::guard_or((), || {
        if !context.is_null() {
            let runtime = unsafe { &*context.cast::<Runtime>() };
            runtime.stopping.cancel();
            runtime.context.begin_shutdown();
        }
    });
}

extern "C" fn finish_shutdown(context: *mut c_void) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if context.is_null() {
            return 2;
        }
        let runtime = unsafe { Box::from_raw(context.cast::<Runtime>()) };
        // Session scopes release their origins on session_shutdown; the host
        // also retires this owner's remaining origins when the library unloads.
        let commands = runtime
            .commands
            .lock()
            .ok()
            .and_then(|mut commands| commands.take());
        drop(commands);
        runtime.compactions.drain(None);
        runtime.context.finish_shutdown();
        0
    })
}

extern "C" fn event(
    input: StbStringRef,
    control: *const AuthRequestControl,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        if !control.is_null()
            && unsafe { control.cast::<u32>().read() }
                < std::mem::size_of::<AuthRequestControl>() as u32
        {
            return 2;
        }
        let runtime = unsafe { &*context.cast::<Runtime>() };
        let Ok(value) = serde_json::from_str::<Value>(unsafe { input.as_str() }) else {
            return 2;
        };
        if value["version"] != 1 {
            return 2;
        }
        let tag = value["tag"].as_u64().unwrap_or_default();
        let signal = runtime.stopping.child_token();
        let client = rpi_plugin_sdk::provider::native::Client::new(&runtime.api)
            .and_then(|client| client.scoped());
        let result = runtime.context.run(&signal, async {
            let client = client?;
            match tag {
                x if x == EventTag::SessionStart as u64 => {
                    runtime.reconcile(&client, &signal, true).await
                }
                x if x == EventTag::BeforeAgentStart as u64 => {
                    runtime.reconcile(&client, &signal, false).await
                }
                x if x == EventTag::SessionShutdown as u64 => {
                    let info = runtime.inspect(&client, &signal).await?;
                    let scope_id = info["scopeId"].as_str().map(str::to_owned);
                    let shared = runtime.shared.clone();
                    let cleanup_scope = scope_id.clone();
                    tokio::task::spawn_blocking(move || {
                        shared.compactions.drain(Some(&cleanup_scope))
                    })
                    .await
                    .map_err(|e| e.to_string())?;
                    runtime.virtuals.lock().await.remove(&scope_id);
                    runtime.release_session(scope_id).await;
                    Ok(())
                }
                _ => Ok(()),
            }
        });
        let response = match result {
            Ok(Ok(())) => json!({"version":1,"result":null}),
            Ok(Err(error)) => json!({"version":1,"result":null,"error":error}),
            Err(error) => json!({"version":1,"result":null,"error":error}),
        };
        unsafe {
            *out = StbString::from_string(response.to_string());
        }
        0
    })
}

/// Wrapper invocation: one provider request reaches the pinned lift state
/// machine, or an unchanged ambient delegation when the provider has no pool.
extern "C" fn invoke(
    request: StbStringRef,
    emit: Option<rpi_plugin_sdk::ProviderStreamEmitFn>,
    data: *mut c_void,
    control: *const rpi_plugin_sdk::ProviderStreamControl,
    _: Option<rpi_plugin_sdk::ProviderStreamCompletionFn>,
    _: *mut c_void,
    out: *mut StbString,
    context: *mut c_void,
) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        if out.is_null() || context.is_null() {
            return 2;
        }
        let runtime = unsafe { &*context.cast::<Runtime>() };
        let Ok(request) = serde_json::from_str::<Value>(unsafe { request.as_str() }) else {
            return 2;
        };
        let descriptor = request["__rpiContinuation"].clone();
        if descriptor["version"] != 1
            || !descriptor["id"].is_string()
            || !descriptor["generation"].is_string()
        {
            return 2;
        }
        let provider_id = request["model"]["provider"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let model_id = request["model"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if provider_id.is_empty() || model_id.is_empty() {
            return 2;
        }
        let signal = CancellationToken::new();
        let control_address = control as usize;
        let cancelled = Arc::new(move || {
            let control = control_address as *const rpi_plugin_sdk::ProviderStreamControl;
            unsafe { control.as_ref() }
                .is_some_and(|control| (control.is_cancelled)(control.user_data, 0) != 0)
        });
        let options = request["options"].as_object().cloned().unwrap_or_default();
        let kind = match options.get("streamKind").and_then(Value::as_str) {
            Some("stream") => crate::lift::StreamKind::Full,
            _ => crate::lift::StreamKind::Simple,
        };
        let work_cancelled = Arc::clone(&cancelled);
        let work_signal = signal.clone();
        let work_request = request.clone();
        // Capture on the foreign callback thread. Re-capturing the startup
        // lease on a worker loses the selected session and child authority.
        let client = rpi_plugin_sdk::provider::native::Client::new(&runtime.api)
            .and_then(|client| client.scoped());
        let work = async move {
            let client = client?;
            let info = runtime.inspect(&client, &work_signal).await?;
            let scope_id = info["scopeId"].as_str().map(str::to_owned);
            let affinity_key = runtime
                .state
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .scopes
                        .get(&scope_id)
                        .and_then(|scope| scope.session_id.clone())
                        .or_else(|| info["identity"]["sessionId"].as_str().map(str::to_owned))
                })
                .filter(|key| !key.is_empty());
            let virtual_config = runtime
                .virtuals
                .lock()
                .await
                .get(&scope_id)
                .and_then(|catalog| {
                    catalog
                        .configs
                        .get(&provider_id)
                        .map(|config| (config.clone(), catalog.service.clone()))
                });
            let pool = runtime.pool(&provider_id, &work_signal).await?;
            if pool.as_ref().is_some_and(Shared::has_accounts) {
                runtime.ensure_registration(&provider_id).await?;
            }
            let host = NativeHost::new(
                runtime.api,
                descriptor,
                work_signal.clone(),
                work_cancelled,
                emit,
                data,
                options,
                true,
            )
            .with_model(work_request["model"].clone())
            .with_services(
                runtime.shared.clone(),
                scope_id.clone(),
                client.clone(),
                virtual_config
                    .as_ref()
                    .map(|(_, service)| service.clone())
                    .unwrap_or_else(|| runtime.service.clone()),
            );
            let lift_request = crate::lift::Request {
                provider_id: provider_id.clone(),
                model_id,
                affinity_key,
                exclude_account_ids: Vec::new(),
                max_account_attempts: MAX_ACCOUNT_ATTEMPTS,
                disable_provider_retries: true,
                kind,
                signal: work_signal.clone(),
            };
            if let Some((config, service)) = virtual_config {
                let catalog = client
                    .request(
                        json!({"port":"providers","action":"inspect"}),
                        work_signal.clone(),
                    )
                    .await?;
                let virtual_host = Arc::new(virtuals::NativeVirtualHost {
                    host,
                    catalog,
                    affinity: lift_request.affinity_key.clone().unwrap_or_default(),
                });
                let provider = crate::virtual_provider::VirtualProvider::new(
                    service,
                    config,
                    virtual_host.clone(),
                    None,
                );
                provider
                    .open(
                        kind,
                        &work_request["model"],
                        work_request["context"].clone(),
                        work_request["options"]
                            .as_object()
                            .cloned()
                            .unwrap_or_default(),
                        work_signal.clone(),
                        false,
                    )
                    .drive()
                    .await;
                virtual_host.host.finish().await;
                return virtual_host
                    .host
                    .terminal()
                    .ok_or_else(|| "Provider stream ended without a terminal event".to_owned());
            }
            let result = if pool.as_ref().is_some_and(Shared::has_accounts) {
                crate::lift::run(&runtime.service, &host, &lift_request).await
            } else {
                passthrough(&host, &lift_request).await
            };
            host.finish().await;
            match result {
                Ok(()) => host
                    .terminal()
                    .ok_or_else(|| "multiprovider: no terminal assistant message".to_owned()),
                Err(error) => Err(error),
            }
        };
        let result = runtime.context.run(&signal, work);
        let aborted = signal.is_cancelled() || cancelled();
        let message = match result {
            Ok(Ok(message)) => message,
            Ok(Err(error)) => error_message(&request, &error, aborted),
            Err(error) => error_message(&request, &error, aborted),
        };
        unsafe {
            *out = StbString::from_string(message.to_string());
        }
        0
    })
}

async fn passthrough(host: &NativeHost, request: &crate::lift::Request) -> Result<(), String> {
    let mut stream = host.start(&lift::Auth::Ambient, request).await?;
    while let Some(frame) = futures_util::StreamExt::next(&mut stream).await {
        if !host.emit(frame?).await {
            break;
        }
    }
    Ok(())
}

fn error_message(request: &Value, error: &str, aborted: bool) -> Value {
    let model = &request["model"];
    json!({
        "role":"assistant",
        "content":[],
        "api":model["api"],
        "provider":model["provider"],
        "model":model["id"],
        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
        "stopReason":if aborted { "aborted" } else { "error" },
        "errorMessage":error,
        "timestamp":0
    })
}

/// Register the native extension. Any missing slot or capability is a hard
/// registration failure, never a silent downgrade.
pub fn register(api: &PluginApi) -> i32 {
    let result = (|| {
        let lifecycle = api
            .register_plugin_lifecycle
            .ok_or("multiprovider requires native plugin lifecycle support; upgrade rpi")?;
        let wrapper = api
            .register_provider_wrapper
            .ok_or("multiprovider requires native provider wrappers; upgrade rpi")?;
        if api.delegate_provider_stream.is_none() {
            return Err("multiprovider requires delegated provider streaming; upgrade rpi".into());
        }
        if api.provider_continuation.is_none() {
            return Err(
                "multiprovider requires private provider continuations; upgrade rpi".into(),
            );
        }
        if api.native_request.is_none() || api.native_context.is_none() {
            return Err("multiprovider requires native runtime ports; upgrade rpi".into());
        }
        let store = api
            .register_credential_store
            .ok_or("multiprovider requires native credential stores; upgrade rpi")?;
        let events = api
            .register_event_result_handler
            .ok_or("multiprovider requires awaited lifecycle events; upgrade rpi")?;
        let state = Box::into_raw(Box::new(Runtime::open(api)?));
        if lifecycle(begin_shutdown, finish_shutdown, state.cast()) != 0 {
            unsafe {
                drop(Box::from_raw(state));
            }
            return Err("multiprovider lifecycle registration failed".into());
        }
        let check = |status: i32| {
            if status == 0 {
                Ok(())
            } else {
                Err("multiprovider native registration failed".to_owned())
            }
        };
        register_virtual_base(api, state.cast())?;
        for (id, models) in unsafe { &*state }.initial_virtuals()? {
            check(api.declare.unwrap()(StbStringRef::from_str(
                &json!({"providers":[{"id":id,"models":models,"requiresAuth":false}]}).to_string(),
            )))?;
            check(api.register_streaming_provider.unwrap()(
                StbStringRef::from_str(&id),
                StbStringRef::from_str(""),
                StbStringRef::from_str("openai-completions"),
                virtual_base,
                rpi_plugin_sdk::callbacks::free_string,
                state.cast(),
            ))?;
        }
        check(wrapper(
            StbStringRef::from_str(&wrapper_manifest().to_string()),
            invoke,
            rpi_plugin_sdk::callbacks::free_string,
            state.cast(),
        ))?;
        check(store(
            StbStringRef::from_str(&store_manifest().to_string()),
            credentials::request,
            credentials::release,
            rpi_plugin_sdk::callbacks::free_string,
            state.cast(),
        ))?;
        for tag in [
            EventTag::SessionStart,
            EventTag::BeforeAgentStart,
            EventTag::SessionShutdown,
        ] {
            check(events(
                tag,
                event,
                rpi_plugin_sdk::callbacks::free_string,
                state.cast(),
            ))?;
        }
        let mut commands = commands::Commands::<()>::with_runner(
            api,
            Arc::new(command_port::NativeRunner::new(
                unsafe { &*state }.shared.clone(),
            )),
            unsafe { &*state }.stopping.clone(),
            vec!["multilogout", "accounts", "vprovider"],
        )?;
        commands.register()?;
        *unsafe { &*state }
            .commands
            .lock()
            .map_err(|_| "multiprovider command owner poisoned".to_owned())? = Some(commands);
        Ok::<_, String>(())
    })();
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("rpi-multiprovider: {error}");
            1
        }
    }
}

rpi_plugin_sdk::export_plugin!(|api| crate::native::register(api));

/// Test-visible access to the store inventory mapping used by the scheduler.
pub fn provider_accounts(store: &Store, provider: &str) -> Result<Vec<Value>, String> {
    inventory(store, provider).map(|accounts| {
        accounts
            .into_iter()
            .map(|account| {
                json!({
                    "id":account.id,"label":account.label,"authKind":account.auth_kind,
                    "credentialRef":account.credential_ref,"enabled":account.enabled,
                    "weight":account.weight,"priority":account.priority,
                })
            })
            .collect()
    })
}

/// Exposed for the pin plan differential fixture.
pub fn inherited_pin_plan(
    recorded: &[SessionPin],
    inherited: &[InheritedPin],
    has_pool: impl Fn(&str) -> bool,
    key: Option<&str>,
) -> (Vec<SessionPin>, Vec<InheritedPin>) {
    let recorded_pools: HashSet<&str> = recorded.iter().map(|pin| pin.pool.as_str()).collect();
    let mut ready = Vec::new();
    let mut pending = Vec::new();
    for pin in inherited {
        if recorded_pools.contains(pin.pool.as_str()) {
            continue;
        }
        match key.filter(|key| !key.is_empty()) {
            Some(key) if has_pool(&pin.pool) => ready.push(SessionPin {
                pool: pin.pool.clone(),
                key: key.to_owned(),
                account_id: pin.account_id.clone(),
                label: pin.label.clone(),
            }),
            _ => pending.push(pin.clone()),
        }
    }
    (ready, pending)
}

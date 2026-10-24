//! Native runner and the narrow ports for the commands currently supported.
use super::{
    commands::{Dialogs, Runner},
    Shared,
};
use crate::announcement::{
    AffinityKeyFor, AnnouncementDependencies, BaseProviderLookup, IntegrationLookup,
    ServiceAnnouncement, ServiceContext,
};
use crate::commands::port::{
    AccountsPort, AuthStatus, LoginInteraction, LogoutPort, PoolAction, PoolBuffer, PoolMethod,
    Port, ProviderInfo, RuntimeView, SelectedModel,
};
use crate::managed::{AccountRequest, AuthResult, ModelRef, ProviderIntegration};
use crate::services::Service;
use crate::types::Account;
use crate::virtual_provider::VirtualProviderConfig;
use async_trait::async_trait;
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

pub struct NativeRunner {
    shared: Arc<Shared>,
}
impl NativeRunner {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

struct NativePort {
    shared: Arc<Shared>,
    client: Client,
    signal: CancellationToken,
    mode: String,
}

impl NativePort {
    async fn new(
        shared: Arc<Shared>,
        client: Client,
        signal: CancellationToken,
        mode: String,
    ) -> Result<Self, String> {
        Ok(Self {
            shared,
            client,
            signal,
            mode,
        })
    }
}

impl AccountsPort for NativePort {
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send {
        self.shared.reconcile(&self.client, &self.signal, false)
    }
    fn snapshot(&self) -> impl Future<Output = Result<Value, String>> + Send {
        async move {
            self.shared
                .service
                .snapshot()
                .await
                .map_err(|e| e.to_string())
        }
    }
}

impl LogoutPort for NativePort {
    fn mode(&self) -> String {
        self.mode.clone()
    }
    fn store(&self, request: Value) -> impl Future<Output = Result<Value, String>> + Send {
        self.shared.store_request(request, self.signal.clone())
    }
    fn installed_provider_name(
        &self,
        id: &str,
    ) -> impl Future<Output = Result<Option<String>, String>> + Send {
        let client = self.client.clone();
        let signal = self.signal.clone();
        let id = id.to_owned();
        async move {
            let catalog = client
                .request(json!({"port":"providers","action":"inspect"}), signal)
                .await?;
            let providers = catalog["providers"]
                .as_array()
                .ok_or_else(|| "invalid provider catalog response".to_owned())?;
            let Some(provider) = providers
                .iter()
                .find(|provider| provider["id"].as_str() == Some(&id))
            else {
                return Ok(None);
            };
            provider["name"]
                .as_str()
                .map(|name| Some(name.to_owned()))
                .ok_or_else(|| "provider catalog entry omitted its name".to_owned())
        }
    }
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send {
        self.shared.reconcile(&self.client, &self.signal, false)
    }
}

struct VirtualPort {
    port: NativePort,
    catalog: Value,
    virtual_ids: Vec<String>,
}
impl crate::commands::vprovider::VproviderPort for VirtualPort {
    fn mode(&self) -> String {
        self.port.mode.clone()
    }
    fn is_virtual_provider(&self, id: &str) -> bool {
        self.virtual_ids.iter().any(|value| value == id)
    }
    fn store(&self, request: Value) -> impl Future<Output = Result<Value, String>> + Send {
        self.port
            .shared
            .store_request(request, self.port.signal.clone())
    }
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send {
        self.port
            .shared
            .reconcile(&self.port.client, &self.port.signal, false)
    }
}
impl crate::commands::vprovider::VproviderSources for VirtualPort {
    fn provider_registered(&self, id: &str) -> bool {
        self.catalog["providers"]
            .as_array()
            .is_some_and(|providers| providers.iter().any(|provider| provider["id"] == id))
    }
    fn providers_with_models(&self) -> Vec<crate::commands::vprovider::CandidateProvider> {
        use crate::commands::vprovider::{CandidateModel, CandidateProvider};
        self.catalog["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|provider| CandidateProvider {
                id: provider["id"].as_str().unwrap_or_default().into(),
                name: provider["name"].as_str().unwrap_or_default().into(),
                models: provider["models"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|model| CandidateModel {
                        id: model["id"].as_str().unwrap_or_default().into(),
                        name: model["name"].as_str().unwrap_or_default().into(),
                        model: model.clone(),
                    })
                    .collect(),
            })
            .collect()
    }
}

impl Runner for NativeRunner {
    fn run<'a>(
        &'a self,
        client: Client,
        ui: Dialogs,
        signal: CancellationToken,
        command: String,
        args: String,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if command == "switch-account" {
                let port = SwitchPort::new(
                    self.shared.clone(),
                    client,
                    signal,
                    ui.mode().to_owned(),
                )
                .await?;
                return crate::commands::switch::switch_account(&ui, &port, &args).await;
            }
            let port =
                NativePort::new(self.shared.clone(), client, signal, ui.mode().to_owned()).await?;
            match command.as_str() {
                "accounts" => crate::commands::accounts::accounts(&ui, &port).await,
                "multilogout" => crate::commands::logout::multilogout(&ui, &port, &args).await,
                "vprovider" => {
                    let catalog = port
                        .client
                        .request(
                            json!({"port":"providers","action":"inspect"}),
                            port.signal.clone(),
                        )
                        .await?;
                    let configs = port
                        .shared
                        .store_request(json!({"op":"listVirtual"}), port.signal.clone())
                        .await?;
                    let virtual_ids = configs
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|config| config["id"].as_str().map(str::to_owned))
                        .collect();
                    crate::commands::vprovider::vprovider(
                        &ui,
                        &VirtualPort {
                            port,
                            catalog,
                            virtual_ids,
                        },
                        &args,
                    )
                    .await
                }
                _ => Err(format!("Unknown Multiprovider command \"{command}\"")),
            }
        })
    }
}

/// Cached command-time reads. The flow always calls `reconcile()` first, so
/// the cache is refreshed past the reconcile before any other method runs.
struct SwitchCache {
    /// `providers/inspect` response; validated to carry a providers array.
    catalog: Value,
    /// Stored virtual provider configs (`store` `listVirtual`).
    virtual_configs: Vec<VirtualProviderConfig>,
    /// The scope's virtual catalog service once `reconcile_virtuals` created
    /// it (`None` when the scope has no virtual providers).
    virtual_service: Option<Service>,
}

/// Production port for `/switch-account`: the full [`Port`] surface backed by
/// the shared scheduler/store plus the host catalog, model and session ports.
///
/// The [`Port`] trait's synchronous methods cannot await the scope's virtual
/// catalog mutex, so the catalog snapshot is cached at construction and
/// refreshed by `reconcile()` (which the flow always runs first). The
/// asynchronous methods read the live scope state instead, so pins, snapshots
/// and announcements observe the same schedulers that serve requests.
struct SwitchPort {
    shared: Arc<Shared>,
    client: Client,
    signal: CancellationToken,
    mode: String,
    /// Scope the command runs in (`context/inspect` `scopeId`). Pins, journal
    /// origins and virtual catalogs are all keyed by it, exactly like the
    /// request path's `scope.session_id` selection.
    scope_id: Option<String>,
    /// `identity.sessionId` at construction, used until the scope state has
    /// one (the flow's `reconcile()` refreshes it from the same source).
    session_fallback: String,
    model: Option<SelectedModel>,
    cache: Mutex<SwitchCache>,
}

impl SwitchPort {
    async fn new(
        shared: Arc<Shared>,
        client: Client,
        signal: CancellationToken,
        mode: String,
    ) -> Result<Self, String> {
        let inspect = shared.inspect(&client, &signal).await?;
        let scope_id = inspect["scopeId"].as_str().map(str::to_owned);
        let session_fallback = inspect["identity"]["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or_default()
            .to_owned();
        let view = client
            .request(json!({"port":"context","action":"snapshot"}), signal.clone())
            .await?;
        // The host always selects a model object; `name` falls back to the id
        // the same way a missing display name would still identify the model.
        let model = view["model"].as_object().and_then(|model| {
            let provider = model.get("provider")?.as_str()?;
            let id = model.get("id")?.as_str()?;
            let name = model
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id);
            Some(SelectedModel {
                provider: provider.to_owned(),
                id: id.to_owned(),
                name: name.to_owned(),
            })
        });
        let port = Self {
            shared,
            client,
            signal,
            mode,
            scope_id,
            session_fallback,
            model,
            cache: Mutex::new(SwitchCache {
                catalog: Value::Null,
                virtual_configs: Vec::new(),
                virtual_service: None,
            }),
        };
        port.refresh().await?;
        Ok(port)
    }

    /// Re-reads the host catalog and the scope's virtual state. Runs at
    /// construction and from `reconcile()`, after the shared reconcile has
    /// re-registered providers and rebuilt the scope's virtual catalog.
    async fn refresh(&self) -> Result<(), String> {
        let catalog = self
            .client
            .request(
                json!({"port":"providers","action":"inspect"}),
                self.signal.clone(),
            )
            .await?;
        if catalog["providers"].as_array().is_none() {
            return Err("invalid provider catalog response".to_owned());
        }
        let stored = self
            .shared
            .store_request(json!({"op":"listVirtual"}), self.signal.clone())
            .await?;
        let virtual_configs: Vec<VirtualProviderConfig> =
            serde_json::from_value(stored).map_err(|error| error.to_string())?;
        let virtual_service = self
            .shared
            .virtuals
            .lock()
            .await
            .get(&self.scope_id)
            .map(|catalog| catalog.service.clone());
        *self
            .cache
            .lock()
            .map_err(|_| "switch port cache poisoned".to_owned())? = SwitchCache {
            catalog,
            virtual_configs,
            virtual_service,
        };
        Ok(())
    }

    fn catalog_has(&self, id: &str) -> bool {
        self.cache
            .lock()
            .ok()
            .and_then(|cache| {
                cache.catalog["providers"].as_array().map(|providers| {
                    providers.iter().any(|provider| provider["id"] == id)
                })
            })
            .unwrap_or(false)
    }

    fn catalog_provider(&self, id: &str) -> Option<Value> {
        self.cache.lock().ok().and_then(|cache| {
            cache.catalog["providers"].as_array().and_then(|providers| {
                providers
                    .iter()
                    .find(|provider| provider["id"] == id)
                    .cloned()
            })
        })
    }

    /// Synchronous scheduler selection from the post-reconcile cache (see the
    /// struct docs). The async paths use [`Shared::command_service`], which
    /// reads the live scope catalog.
    fn cached_service(&self, pool: &str) -> Service {
        self.cache
            .lock()
            .ok()
            .and_then(|cache| {
                cache
                    .virtual_service
                    .clone()
                    .filter(|service| service.has_provider(pool))
            })
            .unwrap_or_else(|| self.shared.service.clone())
    }

    fn announce_model(&self) -> Option<ModelRef> {
        self.model.clone().map(|model| ModelRef {
            id: model.id,
            base_url: None,
        })
    }
}

/// `ServiceContext` for the switch announcement. The model only feeds the
/// resolve path, which the announcement never calls.
#[derive(Clone)]
struct SwitchAnnounceContext {
    model: Option<ModelRef>,
}

impl ServiceContext for SwitchAnnounceContext {
    fn model(&self) -> Option<ModelRef> {
        self.model.clone()
    }
}

/// The announce path's integration view: the snapshot's account list for the
/// pool. `getActiveAccount` only reads `accounts()`, so no managed
/// integration (and no credential access) is needed to report the pin the
/// switch just wrote.
struct PoolAccounts {
    accounts: Vec<Account>,
}

#[async_trait]
impl ProviderIntegration for PoolAccounts {
    async fn accounts(&self) -> Result<Vec<Account>, String> {
        Ok(self.accounts.clone())
    }
    async fn resolve_auth(
        &self,
        _account: &Account,
        _signal: &CancellationToken,
        _request: &AccountRequest,
    ) -> Result<AuthResult, String> {
        Err("resolving account auth is not part of command announce".to_owned())
    }
}

impl Port for SwitchPort {
    fn mode(&self) -> String {
        self.mode.clone()
    }
    fn session_id(&self) -> String {
        // The invoke path keys affinity by the scope's live session id, so a
        // pin this command writes must use the same key to take effect.
        self.shared
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state
                    .scopes
                    .get(&self.scope_id)
                    .and_then(|scope| scope.session_id.clone())
                    .filter(|id| !id.is_empty())
            })
            .unwrap_or_else(|| self.session_fallback.clone())
    }
    fn store_path(&self) -> String {
        // `/switch-account` never shows a save path; only the login flow does,
        // and it is unreachable through this port.
        String::new()
    }
    fn store(&self, request: Value) -> impl Future<Output = Result<Value, String>> + Send {
        self.shared.store_request(request, self.signal.clone())
    }
    fn reconcile(&self) -> impl Future<Output = Result<(), String>> + Send {
        async move {
            self.shared
                .reconcile(&self.client, &self.signal, false)
                .await?;
            self.refresh().await
        }
    }
    fn snapshot(&self) -> impl Future<Output = Result<Value, String>> + Send {
        async move { self.shared.command_snapshot(&self.scope_id).await }
    }
    fn registered_providers(&self) -> Vec<ProviderInfo> {
        // The host catalog carries no per-method login metadata (id, name and
        // kind only), so login presentation stays unwired: `/multilogin` is
        // not registered until the host exposes the auth-method metadata it
        // needs. `/switch-account` never reads these fields.
        self.cache
            .lock()
            .ok()
            .and_then(|cache| cache.catalog["providers"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(|provider| ProviderInfo {
                id: provider["id"].as_str().unwrap_or_default().to_owned(),
                name: provider["name"].as_str().unwrap_or_default().to_owned(),
                oauth: None,
                api_key: None,
            })
            .collect()
    }
    fn installed_provider_name(&self, id: &str) -> Option<String> {
        self.catalog_provider(id)
            .and_then(|provider| provider["name"].as_str().map(str::to_owned))
    }
    fn provider_name(&self, id: &str) -> Option<String> {
        self.catalog_provider(id)
            .and_then(|provider| provider["name"].as_str().map(str::to_owned))
    }
    fn is_virtual_provider(&self, id: &str) -> bool {
        self.cache.lock().ok().is_some_and(|cache| {
            cache.virtual_configs.iter().any(|config| config.id == id)
        })
    }
    fn virtual_provider_name(&self, id: &str) -> Option<String> {
        self.cache.lock().ok().and_then(|cache| {
            cache
                .virtual_configs
                .iter()
                .find(|config| config.id == id)
                .map(|config| config.label.clone())
        })
    }
    fn is_virtual_pool(&self, provider_id: &str, model_id: &str) -> bool {
        self.cache.lock().ok().is_some_and(|cache| {
            cache.virtual_configs.iter().any(|config| {
                config.id == provider_id
                    && config.models.iter().any(|model| model.id == model_id)
            })
        })
    }
    fn runtime(&self) -> Option<RuntimeView> {
        // The host exposes no session-runtime auth-status port; the provider
        // catalog's `authStatus` is the only production signal. Only
        // `unauthenticated` (or a missing status) reads as unconfigured, so
        // ambient credentials keep the upstream account switchable, matching
        // the runtime-absent default upstream (`probeSessionRuntime`
        // returning `undefined`).
        let cache = self.cache.lock().ok()?;
        let providers: Vec<ProviderInfo> = cache.catalog["providers"]
            .as_array()?
            .iter()
            .map(|provider| ProviderInfo {
                id: provider["id"].as_str().unwrap_or_default().to_owned(),
                name: provider["name"].as_str().unwrap_or_default().to_owned(),
                oauth: None,
                api_key: None,
            })
            .collect();
        let auth = cache.catalog["providers"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|provider| {
                let status = provider["authStatus"].as_str().unwrap_or("");
                (
                    provider["id"].as_str().unwrap_or_default().to_owned(),
                    AuthStatus {
                        configured: matches!(status, "authenticated" | "expired"),
                        label: None,
                        source: None,
                    },
                )
            })
            .collect();
        Some(RuntimeView {
            providers,
            auth,
            oauth: Vec::new(),
        })
    }
    fn has_integration(&self, provider_id: &str) -> bool {
        // Mirrors `effectiveIntegration`: a stored pool whose base provider is
        // still in the registry. There are no external integrations natively,
        // and virtual pools short-circuit before this is consulted.
        self.shared.service.has_provider(provider_id) && self.catalog_has(provider_id)
    }
    fn model(&self) -> Option<SelectedModel> {
        self.model.clone()
    }
    fn service_has_provider(&self, id: &str) -> bool {
        self.shared.service.has_provider(id)
            || self
                .cache
                .lock()
                .ok()
                .and_then(|cache| cache.virtual_service.clone())
                .is_some_and(|service| service.has_provider(id))
    }
    fn reset_health(&self, id: &str, account_id: &str) -> Result<(), String> {
        self.cached_service(id)
            .reset_health(id, account_id)
            .map_err(|error| error.to_string())
    }
    fn affinity(&self, pool: &str, key: &str) -> Option<(String, bool)> {
        // The scheduler throws for an unregistered pool where Pi's service
        // answers `undefined`; a missing registration reads as "no pin",
        // exactly like the upstream lookup.
        self.cached_service(pool)
            .affinity(pool, key)
            .ok()
            .flatten()
            .map(|pin| (pin.account_id, pin.explicit))
    }
    fn pin_account(
        &self,
        pool: &str,
        key: &str,
        account_id: &str,
    ) -> impl Future<Output = Result<(), String>> + Send {
        let service = self.shared.command_service(&self.scope_id, pool);
        let pool = pool.to_owned();
        let key = key.to_owned();
        let account_id = account_id.to_owned();
        async move {
            service
                .await
                .pin_account(&pool, &key, &account_id)
                .await
                .map_err(|error| error.to_string())
        }
    }
    fn clear_affinity(&self, pool: &str, key: &str) {
        self.cached_service(pool).clear_affinity(Some(pool), Some(key));
    }
    fn announce_active_account(
        &self,
        pool: &str,
    ) -> impl Future<Output = Result<(), String>> + Send {
        let shared = self.shared.clone();
        let scope_id = self.scope_id.clone();
        let pool = pool.to_owned();
        let context = SwitchAnnounceContext {
            model: self.announce_model(),
        };
        let key = self.session_id();
        async move {
            // The pin (or its clearing) is already in the scheduler; report
            // the active account through the same announcement path the
            // sibling service consumers use. With no subscribers registered
            // this notifies nobody, exactly like the upstream fan-out to an
            // empty listener set. Cross-extension delivery (service
            // registration plus the `activeAccountChanged` topic) is still
            // unwired in the native binding.
            let snapshot = shared.command_snapshot(&scope_id).await?;
            let accounts: Vec<Account> = snapshot["providers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|candidate| candidate["id"] == pool.as_str())
                .and_then(|provider| provider["accounts"].as_array().cloned())
                .unwrap_or_default()
                .iter()
                .map(|account| Account {
                    id: account["id"].as_str().unwrap_or_default().to_owned(),
                    label: account["label"].as_str().unwrap_or_default().to_owned(),
                    auth_kind: account["authKind"].as_str().unwrap_or_default().to_owned(),
                    credential_ref: Value::Null,
                    enabled: account["enabled"].as_bool(),
                    weight: account["weight"].as_f64(),
                    priority: account["priority"].as_f64(),
                    metadata: None,
                })
                .collect();
            let scheduler = shared.command_service(&scope_id, &pool).await;
            if !scheduler.has_provider(&pool) {
                return Ok(());
            }
            let pool_for_lookup = pool.clone();
            let get_integration: Arc<IntegrationLookup> = Arc::new(
                move |id: &str| -> Option<Arc<dyn ProviderIntegration>> {
                    if id != pool_for_lookup {
                        return None;
                    }
                    Some(Arc::new(PoolAccounts {
                        accounts: accounts.clone(),
                    }))
                },
            );
            let get_base_provider: Arc<BaseProviderLookup<SwitchAnnounceContext>> =
                Arc::new(|_, _| None);
            let affinity_key_for: Arc<AffinityKeyFor<SwitchAnnounceContext>> =
                Arc::new(move |_, _, _| Ok(key.clone()));
            let announcement = ServiceAnnouncement::new(AnnouncementDependencies {
                scheduler,
                get_integration,
                get_base_provider,
                affinity_key_for,
            });
            let account = announcement.get_active_account(&pool, &context).await?;
            announcement.notify_active_account_changed(&pool, &context, account);
            Ok(())
        }
    }
    fn append_entry(
        &self,
        custom_type: &str,
        data: Value,
    ) -> impl Future<Output = Result<(), String>> + Send {
        let shared = self.shared.clone();
        let client = self.client.clone();
        let signal = self.signal.clone();
        let scope_id = self.scope_id.clone();
        let custom_type = custom_type.to_owned();
        async move {
            // Command invocations never captured the session, so resolve the
            // command-scope origin (capturing first when needed) before
            // journaling. A missing origin fails the switch instead of
            // claiming a decision that cannot survive restoration.
            let origin = shared.command_origin(&client, &signal, scope_id).await?;
            shared.append_journal(origin, &custom_type, data).await
        }
    }
    fn login<I: LoginInteraction>(
        &self,
        _provider: &ProviderInfo,
        _auth_type: &str,
        _interaction: &I,
    ) -> impl Future<Output = Result<Value, String>> + Send {
        // Unreachable from `/switch-account`; `/multilogin` stays unregistered
        // until the host exposes the auth-method metadata its login needs.
        async move { Err("login is not supported by the switch-account port".to_owned()) }
    }
    fn pool_manager(
        &self,
        _provider: &ProviderInfo,
        _methods: &[PoolMethod],
        _buffer: &mut PoolBuffer,
    ) -> impl Future<Output = Result<PoolAction, String>> + Send {
        // Unreachable from `/switch-account`; owned by the pool manager lane.
        async move { Err("pool management is not supported by the switch-account port".to_owned()) }
    }
}
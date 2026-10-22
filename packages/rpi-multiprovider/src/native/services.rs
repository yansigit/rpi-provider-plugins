//! Native service surface for `src/register.ts` and `src/announcement.ts`.
//!
//! `register.ts` announces every integration to sibling extensions at load and
//! again on each `session_start`; `announcement.ts` answers active-account /
//! credential questions and fans account changes out per provider. The host
//! offers the equivalent as a session-owned private service (`register_service`
//! plus the `port:"services"` actions in `native_request`), so this module maps
//! the ported in-process contracts onto one versioned service:
//!
//! ```text
//! id pi-multiprovider, serviceVersion 1
//! methods: providers, accounts, getActiveAccount, resolveActiveAccountAuth
//! topics:  providerRegistered, providerUnregistered, activeAccountChanged
//! ```
//!
//! The module is deliberately host-independent. [`Port`] is the only host-facing
//! trait: the native binding turns it into `native_request {port:"services",
//! action:"publish"}` and feeds [`NativeServices::respond`] from the
//! `register_service` callback. Tests substitute a recording port, so the
//! service semantics are verified without a loaded library.
//!
//! Notification values are non-secret by construction: account descriptors are a
//! whitelist of `id`/`label`/`authKind`, credential references and resolved
//! tokens never reach a topic.

use crate::announcement::{
    ActiveAccount, AffinityKeyFor, AnnouncementDependencies, BaseProviderLookup, IntegrationLookup,
    Listener, ServiceAnnouncement, ServiceContext, Unsubscribe,
};
use crate::services::Service;
use crate::types::SelectionBias;
use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

/// Registered service id (`pi-multiprovider`).
pub const SERVICE_ID: &str = "pi-multiprovider";
/// Wire version of the service contract. The manifest's `serviceVersion` is
/// separate from the request envelope's `version`, as the host requires.
pub const SERVICE_VERSION: u32 = 1;
/// The request envelope version the host uses.
pub const WIRE_VERSION: u32 = 1;

/// Lists the provider integrations this session announced (`register.ts`).
pub const METHOD_PROVIDERS: &str = "providers";
/// Lists one provider's accounts, projected to non-secret fields.
pub const METHOD_ACCOUNTS: &str = "accounts";
/// `MultiProviderServiceAnnouncement.getActiveAccount`.
pub const METHOD_GET_ACTIVE_ACCOUNT: &str = "getActiveAccount";
/// `MultiProviderServiceAnnouncement.resolveActiveAccountAuth`.
pub const METHOD_RESOLVE_ACTIVE_ACCOUNT_AUTH: &str = "resolveActiveAccountAuth";

/// Published once per integration registration and again on every session start.
pub const TOPIC_PROVIDER_REGISTERED: &str = "providerRegistered";
/// Native-only companion to `providerRegistered`: a registration was withdrawn.
pub const TOPIC_PROVIDER_UNREGISTERED: &str = "providerUnregistered";
/// `ServiceAnnouncementHandle.notifyActiveAccountChanged`.
pub const TOPIC_ACTIVE_ACCOUNT_CHANGED: &str = "activeAccountChanged";

/// The host manifest, exactly as `register_service` receives it. Names stay
/// within the host's ASCII identifier alphabet and limits; the fields match
/// `ServiceManifest` (unknown fields are refused by the host).
pub fn manifest() -> Value {
    json!({
        "version": WIRE_VERSION,
        "id": SERVICE_ID,
        "serviceVersion": SERVICE_VERSION,
        "methods": [
            METHOD_PROVIDERS,
            METHOD_ACCOUNTS,
            METHOD_GET_ACTIVE_ACCOUNT,
            METHOD_RESOLVE_ACTIVE_ACCOUNT_AUTH,
        ],
        "topics": [
            TOPIC_PROVIDER_REGISTERED,
            TOPIC_PROVIDER_UNREGISTERED,
            TOPIC_ACTIVE_ACCOUNT_CHANGED,
        ],
    })
}

/// `manifest()` serialized for the `register_service` callback.
pub fn manifest_json() -> String {
    manifest().to_string()
}

/// The host side of the service: publish one topic notification. The binding
/// forwards this to `native_request`:
///
/// ```json
/// {"port":"services","action":"publish","service":"pi-multiprovider",
///  "serviceVersion":1,"topic":"…","value":…}
/// ```
///
/// The returned sequence is the host's committed notification sequence number.
#[async_trait]
pub trait Port: Send + Sync {
    async fn publish(
        &self,
        service: &str,
        service_version: u32,
        topic: &str,
        value: Value,
    ) -> Result<u64, String>;
}

/// The serializable projection of a `MultiProviderIntegration` (`register.ts`'s
/// event value). Functions, accounts and credentials stay in the plugin; only
/// what a service consumer needs to locate the provider crosses the boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_bias: Option<SelectionBias>,
}

impl ProviderDescriptor {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            management_hint: None,
            selection_bias: None,
        }
    }
}

/// `MultiProviderServiceContext` provider for the current session. The native
/// caller cannot pass a live host context through JSON, so the binding owns one
/// lookup per session; `None` means no session is attached yet.
pub type ContextLookup<C> = dyn Fn() -> Option<C> + Send + Sync;

/// Everything the service needs from the binding.
pub struct Dependencies<C: ServiceContext> {
    pub scheduler: Service,
    pub get_integration: Arc<IntegrationLookup>,
    pub get_base_provider: Arc<BaseProviderLookup<C>>,
    pub affinity_key_for: Arc<AffinityKeyFor<C>>,
    pub context: Arc<ContextLookup<C>>,
    pub port: Arc<dyn Port>,
}

struct Inventory {
    /// Every `registerMultiProvider` call, in order. `register.ts` installs one
    /// `session_start` closure per call, so a repeated id is announced again at
    /// session start with the value that call supplied — even if a later call
    /// replaced it. Preserved rather than fixed.
    announced: Vec<ProviderDescriptor>,
    /// The consumer-visible map: last value per id, first registration position,
    /// matching the extension's `Map.set` dedupe.
    latest: IndexMap<String, ProviderDescriptor>,
}

/// `createServiceAnnouncement` plus the native service registry surface.
pub struct NativeServices<C: ServiceContext> {
    announcement: ServiceAnnouncement<C>,
    get_integration: Arc<IntegrationLookup>,
    context: Arc<ContextLookup<C>>,
    port: Arc<dyn Port>,
    inventory: Mutex<Inventory>,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|error| error.into_inner())
}

fn encode<T: Serialize>(value: T) -> Result<Value, String> {
    serde_json::to_value(value).map_err(|error| error.to_string())
}

fn account_id(input: &Value) -> Result<String, String> {
    let object = input
        .as_object()
        .ok_or_else(|| "multiprovider: service input must be an object".to_owned())?;
    object
        .get("providerId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "multiprovider: providerId must be a string".to_owned())
}

impl<C: ServiceContext> NativeServices<C> {
    pub fn new(deps: Dependencies<C>) -> Self {
        let Dependencies {
            scheduler,
            get_integration,
            get_base_provider,
            affinity_key_for,
            context,
            port,
        } = deps;
        Self {
            announcement: ServiceAnnouncement::new(AnnouncementDependencies {
                scheduler,
                get_integration: get_integration.clone(),
                get_base_provider,
                affinity_key_for,
            }),
            get_integration,
            context,
            port,
            inventory: Mutex::new(Inventory {
                announced: Vec::new(),
                latest: IndexMap::new(),
            }),
        }
    }

    /// The in-process announcement the bundled extension still uses after
    /// `/switch-account`; it also installs change listeners.
    pub fn announcement(&self) -> &ServiceAnnouncement<C> {
        &self.announcement
    }

    /// `onActiveAccountChanged` passthrough.
    pub fn on_active_account_changed(
        &self,
        provider_id: &str,
        listener: Arc<Listener<C>>,
    ) -> Unsubscribe {
        self.announcement
            .on_active_account_changed(provider_id, listener)
    }

    /// The current registration inventory in announcement order.
    pub fn providers(&self) -> Vec<ProviderDescriptor> {
        lock(&self.inventory).latest.values().cloned().collect()
    }

    async fn publish(&self, topic: &str, value: Value) -> Result<u64, String> {
        self.port
            .publish(SERVICE_ID, SERVICE_VERSION, topic, value)
            .await
    }

    /// `registerMultiProvider`'s immediate announce. Registering an id again
    /// replaces it in place and announces again, as a second upstream
    /// `pi.events.emit` would. A publication failure returns the host error but
    /// keeps the local inventory (the integration is installed either way).
    pub async fn register_provider(
        &self,
        descriptor: ProviderDescriptor,
    ) -> Result<u64, String> {
        {
            let mut inventory = lock(&self.inventory);
            inventory.announced.push(descriptor.clone());
            inventory
                .latest
                .insert(descriptor.id.clone(), descriptor.clone());
        }
        self.publish(TOPIC_PROVIDER_REGISTERED, encode(descriptor)?).await
    }

    /// Native-only removal. Absent ids publish nothing and return `None`.
    pub async fn unregister_provider(&self, provider_id: &str) -> Result<Option<u64>, String> {
        let removed = {
            let mut inventory = lock(&self.inventory);
            inventory.announced.retain(|entry| entry.id != provider_id);
            inventory.latest.shift_remove(provider_id)
        };
        if removed.is_none() {
            return Ok(None);
        }
        let sequence = self
            .publish(
                TOPIC_PROVIDER_UNREGISTERED,
                json!({ "providerId": provider_id }),
            )
            .await?;
        Ok(Some(sequence))
    }

    /// `register.ts`'s `session_start` handler: re-announce every announced
    /// integration, in call order, with the value that call supplied.
    pub async fn session_start(&self) -> Result<Vec<u64>, String> {
        let announced = lock(&self.inventory).announced.clone();
        let mut sequences = Vec::with_capacity(announced.len());
        for descriptor in announced {
            sequences.push(self.publish(TOPIC_PROVIDER_REGISTERED, encode(descriptor)?).await?);
        }
        Ok(sequences)
    }

    /// Account inventory for one provider, whitelisted to `id`/`label`/`authKind`.
    /// An unknown provider is `null`, matching the announcement's "no integration
    /// returns nothing"; an inventory failure propagates as the upstream rejection.
    pub async fn accounts(&self, provider_id: &str) -> Result<Value, String> {
        let Some(integration) = (self.get_integration)(provider_id) else {
            return Ok(Value::Null);
        };
        let accounts = integration.accounts().await?;
        let accounts: Vec<Value> = accounts
            .iter()
            .map(|account| {
                json!({
                    "id": account.id,
                    "label": account.label,
                    "authKind": account.auth_kind,
                })
            })
            .collect();
        Ok(json!({ "providerId": provider_id, "accounts": accounts }))
    }

    fn context(&self) -> Result<C, String> {
        (self.context)().ok_or_else(|| "multiprovider: session context unavailable".to_owned())
    }

    async fn active_account(&self, provider_id: &str) -> Result<Value, String> {
        let ctx = self.context()?;
        let active = self.announcement.get_active_account(provider_id, &ctx).await?;
        active.map_or(Ok(Value::Null), encode)
    }

    async fn resolve_active_auth(
        &self,
        provider_id: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<Value, String> {
        let ctx = self.context()?;
        let auth = self
            .announcement
            .resolve_active_account_auth(provider_id, &ctx, signal)
            .await?;
        auth.map_or(Ok(Value::Null), encode)
    }

    /// Dispatch one manifest method. Input shapes are `{providerId}` for
    /// `accounts`/`getActiveAccount`/`resolveActiveAccountAuth` and ignored for
    /// `providers`. Unknown methods are refused, as the host refuses names the
    /// manifest did not list.
    pub async fn call(
        &self,
        method: &str,
        input: &Value,
        signal: Option<&CancellationToken>,
    ) -> Result<Value, String> {
        match method {
            METHOD_PROVIDERS => Ok(json!({ "providers": self.providers() })),
            METHOD_ACCOUNTS => self.accounts(&account_id(input)?).await,
            METHOD_GET_ACTIVE_ACCOUNT => self.active_account(&account_id(input)?).await,
            METHOD_RESOLVE_ACTIVE_ACCOUNT_AUTH => {
                self.resolve_active_auth(&account_id(input)?, signal).await
            }
            other => Err(format!("unknown service method \"{other}\"")),
        }
    }

    /// The `register_service` callback body: validate the host envelope, dispatch
    /// the method, and return the `{version:1,result}` / `{version:1,error}`
    /// object the host expects. The binding only has to pass the callback string
    /// through and keep the returned allocation alive.
    pub async fn respond(&self, request: &str, signal: Option<&CancellationToken>) -> String {
        let response = match self.handle(request, signal).await {
            Ok(result) => json!({ "version": WIRE_VERSION, "result": result }),
            Err(error) => json!({ "version": WIRE_VERSION, "error": error }),
        };
        response.to_string()
    }

    async fn handle(
        &self,
        request: &str,
        signal: Option<&CancellationToken>,
    ) -> Result<Value, String> {
        let value: Value = serde_json::from_str(request)
            .map_err(|_| "multiprovider: invalid service request".to_owned())?;
        let object: &Map<String, Value> = value
            .as_object()
            .ok_or_else(|| "multiprovider: invalid service request".to_owned())?;
        if object.get("version").and_then(Value::as_u64) != Some(u64::from(WIRE_VERSION)) {
            return Err("multiprovider: unsupported service request version".to_owned());
        }
        if object.get("service").and_then(Value::as_str) != Some(SERVICE_ID) {
            return Err("multiprovider: unknown service".to_owned());
        }
        if object.get("serviceVersion").and_then(Value::as_u64)
            != Some(u64::from(SERVICE_VERSION))
        {
            return Err("multiprovider: unknown service version".to_owned());
        }
        let method = object
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| "multiprovider: service method must be a string".to_owned())?;
        let input = object.get("input").cloned().unwrap_or(Value::Null);
        self.call(method, &input, signal).await
    }

    /// `ServiceAnnouncementHandle.notifyActiveAccountChanged`: in-process
    /// listeners first (the upstream fan-out), then the `activeAccountChanged`
    /// topic. A failed publication leaves the listeners notified and returns the
    /// host error; the binding decides whether to surface it.
    pub async fn notify_active_account_changed(
        &self,
        provider_id: &str,
        ctx: &C,
        account: Option<ActiveAccount>,
    ) -> Result<u64, String> {
        self.announcement
            .notify_active_account_changed(provider_id, ctx, account.clone());
        self.publish(
            TOPIC_ACTIVE_ACCOUNT_CHANGED,
            json!({ "providerId": provider_id, "account": account }),
        )
        .await
    }
}

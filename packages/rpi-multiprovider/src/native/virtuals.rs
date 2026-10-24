//! Owned virtual catalogs and secure, cross-provider stream effects.
//!
//! Retry policy stays in `virtual_provider`; this adapter never transports
//! credentials. Targets and authentication receipts are continuation-owned.
use super::{lift::NativeHost, Shared};
use crate::{
    lift::{Host, Response},
    services::Registration,
    virtual_provider::*,
};
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio_util::sync::CancellationToken;

pub const IMPLEMENTATION_ID: &str = "multiprovider-virtual-base";

pub(super) struct Catalog {
    pub service: crate::services::Service,
    pub configs: HashMap<String, VirtualProviderConfig>,
    models: HashMap<String, Vec<Value>>,
    registrations: HashMap<String, Vec<Arc<Registration>>>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            service: crate::services::Service::new(crate::services::Options::default()),
            configs: HashMap::new(),
            models: HashMap::new(),
            registrations: HashMap::new(),
        }
    }
}

pub struct NativeVirtualHost {
    pub host: NativeHost,
    pub catalog: Value,
    pub affinity: String,
}

#[async_trait::async_trait]
impl VirtualHost for NativeVirtualHost {
    type Stream = super::lift::AttemptStream;
    fn now(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    }
    fn affinity_key(&self) -> Result<String, String> {
        Ok(self.affinity.clone())
    }
    fn backing_models(&self, provider: &str) -> Option<Vec<Value>> {
        models(&self.catalog, provider)
    }
    async fn resolve_ambient_auth(
        &self,
        _: &str,
        _: &Value,
        _: &CancellationToken,
    ) -> Result<AmbientResolution, String> {
        // The real resolution occurs in start, against the opaque target from
        // the very same epoch. No source authority enters attempt options.
        Ok(AmbientResolution::Resolved(AmbientAuth::default()))
    }
    async fn start(&self, attempt: Attempt) -> Result<Self::Stream, String> {
        self.host.start_target(attempt).await
    }
    fn response(&self) -> Option<Response> {
        self.host.response()
    }
    async fn emit(&self, event: Value) {
        self.host.emit(event).await;
    }
    async fn failover(&self, info: crate::lift::Failover<'_>) -> Result<bool, String> {
        self.host.failover(info).await
    }
}

fn models(catalog: &Value, id: &str) -> Option<Vec<Value>> {
    catalog["providers"]
        .as_array()?
        .iter()
        .find(|provider| provider["id"].as_str() == Some(id))?["models"]
        .as_array()
        .cloned()
}

impl Shared {
    /// CLI selection precedes session_start. Seed the configured identities in
    /// the registration snapshot; runtime publication still acknowledges and
    /// heals their catalogs before a request can execute.
    pub(super) fn initial_virtuals(&self) -> Result<Vec<(String, Vec<Value>)>, String> {
        let stored = self.context.run(
            &self.stopping,
            self.store_request(json!({"op":"listVirtual"}), self.stopping.clone()),
        )??;
        let configs: Vec<VirtualProviderConfig> =
            serde_json::from_value(stored).map_err(|error| error.to_string())?;
        Ok(configs
            .into_iter()
            .map(|config| {
                let id = config.id.clone();
                let provider = VirtualProvider::new(
                    self.service.clone(),
                    config,
                    Arc::new(Metadata {
                        catalog: json!({"providers":[]}),
                    }),
                    None,
                );
                (id, provider.models())
            })
            .collect())
    }
    /// Publish before acknowledging the config. A failed publication leaves the
    /// previous scheduler/catalog intact, and the next reconcile retries it.
    pub(super) async fn reconcile_virtuals(
        &self,
        client: &Client,
        scope: Option<String>,
        signal: &CancellationToken,
    ) -> Result<(), String> {
        let stored = self
            .store_request(json!({"op":"listVirtual"}), signal.clone())
            .await?;
        let configs: Vec<VirtualProviderConfig> =
            serde_json::from_value(stored).map_err(|error| error.to_string())?;
        let mut catalogs = self.virtuals.lock().await;
        if configs.is_empty()
            && catalogs
                .get(&scope)
                .map_or(true, |catalog| catalog.configs.is_empty())
        {
            return Ok(());
        }
        let catalog = client
            .request(
                json!({"port":"providers","action":"inspect"}),
                signal.clone(),
            )
            .await?;
        let state = catalogs.entry(scope).or_default();
        if let Ok(settings) = self
            .store_request(json!({"op":"getScheduler"}), signal.clone())
            .await
        {
            let _ = state.service.update_settings(&settings);
        }
        let stale: Vec<_> = state
            .configs
            .keys()
            .filter(|id| !configs.iter().any(|config| &config.id == *id))
            .cloned()
            .collect();
        for id in stale {
            if models(&catalog, &id).is_some() {
                client
                    .request(
                        json!({"port":"providers","action":"unregister","providerId":id}),
                        signal.clone(),
                    )
                    .await?;
            }
            if let Some(registrations) = state.registrations.remove(&id) {
                for registration in registrations {
                    state.service.unregister(&registration);
                }
            }
            state.configs.remove(&id);
            state.models.remove(&id);
        }
        for config in configs {
            let healed = heal_virtual_templates(&config, |provider, model| {
                models(&catalog, provider)?
                    .into_iter()
                    .find(|value| value["id"] == model)
                    .map(|value| capture_virtual_model_template(&value))
            });
            if let Some(healed) = &healed {
                let _ = self
                    .store_request(json!({"op":"saveVirtual","input":healed}), signal.clone())
                    .await;
            }
            let config = healed.unwrap_or(config);
            let metadata = Metadata {
                catalog: catalog.clone(),
            };
            let provider = VirtualProvider::new(
                state.service.clone(),
                config.clone(),
                Arc::new(metadata),
                None,
            );
            let advertised = provider.models();
            let present = models(&catalog, &config.id);
            let changed = state.configs.get(&config.id) != Some(&config);
            if changed
                || state.models.get(&config.id) != Some(&advertised)
                || present.as_ref() != Some(&advertised)
            {
                client.request(json!({"port":"providers","action":if present.is_some() {"publish"} else {"register"},
                    "implementationId":IMPLEMENTATION_ID,"providerId":config.id,"models":advertised}), signal.clone()).await?;
                if changed {
                    if let Some(registrations) = state.registrations.remove(&config.id) {
                        for registration in registrations {
                            state.service.unregister(&registration);
                        }
                    }
                    let mut registrations = Vec::new();
                    for integration in
                        create_virtual_integrations(&config, &IntegrationOptions::default())
                    {
                        registrations.push(
                            state
                                .service
                                .register(integration.registration)
                                .map_err(|error| error.to_string())?,
                        );
                    }
                    state.registrations.insert(config.id.clone(), registrations);
                }
                state.models.insert(config.id.clone(), advertised);
                state.configs.insert(config.id.clone(), config);
            }
        }
        Ok(())
    }
}

// Catalog construction has no streaming authority.
struct Metadata {
    catalog: Value,
}
#[async_trait::async_trait]
impl VirtualHost for Metadata {
    type Stream = futures_util::stream::Empty<Result<Value, String>>;
    fn now(&self) -> f64 {
        0.0
    }
    fn affinity_key(&self) -> Result<String, String> {
        Err("multiprovider: session not ready".into())
    }
    fn backing_models(&self, id: &str) -> Option<Vec<Value>> {
        models(&self.catalog, id)
    }
    async fn resolve_ambient_auth(
        &self,
        _: &str,
        _: &Value,
        _: &CancellationToken,
    ) -> Result<AmbientResolution, String> {
        Err("multiprovider: session not ready".into())
    }
    async fn start(&self, _: Attempt) -> Result<Self::Stream, String> {
        Err("multiprovider: session not ready".into())
    }
    fn response(&self) -> Option<Response> {
        None
    }
    async fn emit(&self, _: Value) {}
}

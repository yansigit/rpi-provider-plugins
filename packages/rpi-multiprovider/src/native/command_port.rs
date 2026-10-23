//! Native runner and the narrow ports for the two commands currently supported.
use super::{
    commands::{Dialogs, Runner},
    Shared,
};
use crate::commands::port::{AccountsPort, LogoutPort};
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc};
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
            let port =
                NativePort::new(self.shared.clone(), client, signal, ui.mode().to_owned()).await?;
            match command.as_str() {
                "accounts" => crate::commands::accounts::accounts(&ui, &port).await,
                "multilogout" => crate::commands::logout::multilogout(&ui, &port, &args).await,
                _ => Err(format!("Unknown Multiprovider command \"{command}\"")),
            }
        })
    }
}

//! Registration state follows committed slot transitions; reads never activate a source.
use crate::vault::{runtime::Vault, slot::Slot, source};
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use std::{
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub enum Mode {
    Local,
    Custody,
    Refusing(String),
}
pub struct State {
    vault: OnceLock<Arc<Vault>>,
    mode: Mutex<Mode>,
    serial: tokio::sync::Mutex<()>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            vault: OnceLock::new(),
            mode: Mutex::new(Mode::Refusing(
                "Antigravity has not initialized its account source.".into(),
            )),
            serial: tokio::sync::Mutex::new(()),
        }
    }
}
impl State {
    pub fn initialize(&self, agent_dir: &Path) -> Result<(), String> {
        let vault = Arc::new(Vault::new(&agent_dir.join("antigravity-auth-vault"))?);
        self.vault
            .set(vault)
            .map_err(|_| "Antigravity account source is already initialized".into())
    }
    pub fn vault(&self) -> Result<Arc<Vault>, String> {
        self.vault
            .get()
            .cloned()
            .ok_or_else(|| "Antigravity account source is not initialized".into())
    }
    pub fn mode(&self) -> Mode {
        self.mode.lock().unwrap().clone()
    }
    pub async fn synchronize(
        &self,
        client: &Client,
        signal: CancellationToken,
    ) -> Result<(), String> {
        let _serial = self.serial.lock().await;
        let result = async {
            let vault = self.vault()?;
            vault.authority.synchronize(|mode| client.request(json!({"port":"auth","action":"credentialStore","providerId":"google","input":{"mode":mode["mode"]}}), signal)).await
        }.await;
        *self.mode.lock().unwrap() = match &result {
            Ok(mode) if mode["mode"] == "local" => Mode::Local,
            Ok(_) => Mode::Custody,
            Err(error) => Mode::Refusing(error.clone()),
        };
        result.map(|_| ())
    }
    /// The host already holds its credential-store lock; never re-enter a host auth port here.
    pub fn transaction(&self, input: &Value) -> Result<Value, String> {
        if input["expiryUnit"] != "seconds" {
            return Err("Unsupported legacy credential expiry unit".into());
        }
        let path = Path::new(
            input["authPath"]
                .as_str()
                .ok_or("Missing host credential path")?,
        );
        if !path.is_absolute() {
            return Err("Invalid host credential path".into());
        }
        let vault = self.vault()?;
        let slot = Slot::new(
            path,
            &vault
                .authority
                .mode_file
                .with_file_name("google-login-stash.json"),
            "google",
            source::MARKER,
        )
        .map_err(|e| e.to_string())?;
        match input["input"]["mode"].as_str() {
            Some("custody") => {
                slot.enter().map_err(|e| e.to_string())?;
                Ok(Value::Null)
            }
            Some("local") => slot.exit().map(Value::from).map_err(|e| e.to_string()),
            _ => Err("Invalid Antigravity credential transition".into()),
        }
    }
    pub async fn assert_local(&self) -> Result<(), String> {
        self.vault()?.authority.assert_local().await
    }
    pub async fn close(&self) {
        if let Some(vault) = self.vault.get() {
            vault.close().await;
        }
    }
}

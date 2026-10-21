//! Explicit enrollment controls; model requests never enter this path.
use super::{
    ceremony::{self, Client, Failure, Manager, Store},
    enrollment,
    source::{self, Source},
};
use serde_json::{json, Value};
use std::{future::Future, path::Path, sync::Arc};

#[derive(Debug)]
pub enum Error {
    Refused(&'static str),
    Other(Failure),
}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        Self::Other(value)
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(v) => f.write_str(v),
            Self::Other(v) => v.fmt(f),
        }
    }
}
impl std::error::Error for Error {}

pub trait Port: Send + Sync + 'static {
    type Client: Client + Clone;
    fn connect(&self) -> impl Future<Output = Result<Self::Client, Failure>> + Send;
    fn close(&self, client: Self::Client) -> impl Future<Output = ()> + Send;
    fn host_slot(&self) -> impl Future<Output = Result<String, Failure>> + Send;
    fn changed(&self, mode: Value) -> impl Future<Output = Result<(), Failure>> + Send;
    fn now_ms(&self) -> u64;
    fn mint_secret(&self) -> Result<String, Failure>;
}
pub struct Setup<S: Store, P: Port> {
    pub source: Source<S>,
    pub port: Arc<P>,
}
impl<S: Store + Clone, P: Port> Setup<S, P> {
    pub async fn status(&self) -> Result<Value, Error> {
        let mut source = self.source.status().await;
        source["hostSlot"] = if source["mode"] == "local" {
            self.port.host_slot().await?.into()
        } else {
            "dormant".into()
        };
        Ok(source)
    }
    pub async fn connect_step(&self) -> Result<Value, Error> {
        let client = self.port.connect().await?;
        let now = self.port.clone();
        let mint = self.port.clone();
        let manager = Manager {
            store: self.source.store.clone(),
            client: client.clone(),
            proposed_name: source::PROPOSED_NAME.into(),
            now: Arc::new(move || now.now_ms()),
            mint_secret: Arc::new(move || mint.mint_secret()),
        };
        let result = async {
            let current = manager.status().await?;
            if matches!(current["state"].as_str(), Some("denied" | "blocked")) {
                manager.reset_terminal().await?;
            }
            let status = manager.reconcile().await?;
            if status["state"] == "approved" {
                let mode = source::read_mode(&self.source.mode_file);
                let route = if mode["ok"] == true && mode["record"]["mode"] == "custody" {
                    mode["record"]["routeId"].as_str()
                } else {
                    None
                };
                self.use_vault(route).await?;
            }
            Ok(status)
        }
        .await;
        self.port.close(client).await;
        result
    }
    async fn write(&self, record: Value) -> Result<Value, Error> {
        let path = self.source.mode_file.clone();
        let value = record.clone();
        tokio::task::spawn_blocking(move || write_mode(&path, &value))
            .await
            .map_err(|_| Failure::Storage("Vault filesystem operation failed".into()))??;
        self.port.changed(record.clone()).await?;
        Ok(record)
    }
    pub async fn use_vault(&self, route: Option<&str>) -> Result<Value, Error> {
        if enrollment::read_status(&self.source.store, source::PROPOSED_NAME).await?["state"]
            != "approved"
        {
            return Err(Error::Refused("Pi doesn't have vault access yet. Choose Request vault access and wait for the vault operator to approve it."));
        }
        if route.is_some_and(|v| crate::json::trim(v).is_empty()) {
            return Err(Error::Refused("Choose a vault account to use."));
        }
        let mut record = json!({"version":1,"mode":"custody"});
        if let Some(route) = route {
            record["routeId"] = route.into();
        }
        self.write(record).await
    }
    pub async fn use_local(&self) -> Result<Value, Error> {
        if enrollment::read_authority(&self.source.store, source::PROPOSED_NAME).await != "local" {
            return Err(Error::Refused(
                "Disconnect from the vault before switching to Pi's own login.",
            ));
        }
        self.write(json!({"version":1,"mode":"local"})).await
    }
    pub async fn disconnect(&self) -> Result<(), Error> {
        if ceremony::disconnect(&self.source.store, source::PROPOSED_NAME).await? == "busy" {
            return Err(Error::Refused("Another vault setup step is running, so Pi is still connected to the vault. Try again in a moment."));
        }
        self.write(json!({"version":1,"mode":"local"})).await?;
        Ok(())
    }
}

/// The source writer intentionally has no enrollment lease or credential-store durability policy.
pub fn write_mode(path: &Path, record: &Value) -> Result<(), Failure> {
    use std::{fs, io::Write};
    let mut value = json!({"version":1,"mode":record["mode"]});
    if let Some(route) = record.get("routeId") {
        value["routeId"] = route.clone();
    }
    fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| super::private::io_error(e, "mkdir", path.parent()))?;
    let mut stage = path.as_os_str().to_owned();
    stage.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let stage = std::path::PathBuf::from(stage);
    let result = (|| {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&stage)
            .map_err(|e| super::private::io_error(e, "open", Some(&stage)))?;
        file.write_all(format!("{}\n", crate::json::pretty(&value)).as_bytes())
            .map_err(|e| super::private::io_error(e, "write", Some(&stage)))?;
        drop(file);
        fs::rename(&stage, path).map_err(|e| super::private::rename_error(e, &stage, path))
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}

pub fn classify_host_slot(auth: &Value) -> &'static str {
    if auth["type"] == "oauth" {
        if let Some(refresh) = auth["refresh"].as_str() {
            if refresh == "claustrum-tombstone:v1:google" {
                return "placeholder";
            }
            if !refresh.is_empty() && !refresh.starts_with("claustrum-tombstone:v1:") {
                return "login";
            }
        }
    }
    if auth["type"] == "api" && auth["key"].as_str().is_some_and(|v| !v.is_empty()) {
        return "login";
    }
    "empty"
}

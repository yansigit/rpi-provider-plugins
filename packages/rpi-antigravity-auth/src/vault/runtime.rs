//! Native composition of the Pi request path and enrolled Subc transport.
use super::{
    ceremony::{Slot, Store},
    client::Operation,
    connection::{Enrolled, SubcConnector},
    consumer,
    custody::{self, Failure},
    discovery, enrollment,
    file_store::FileStore,
    path,
    policy::ConsumerError,
    roster::Projection,
    roster_store::RosterStore,
    send,
    source::Source,
};
use futures_util::{future::BoxFuture, FutureExt};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64
}
struct Client {
    connection: Arc<Enrolled<SubcConnector>>,
    files: FileStore,
}
impl custody::Port for Client {
    fn now_ms(&self) -> f64 {
        now()
    }
    async fn token(&self) -> Result<Value, Failure> {
        let value = self.files.read(Slot::Token).await?.ok_or_else(|| {
            Failure::Consumer(ConsumerError::new(
                "not-enrolled",
                "Claustrum enrollment is not configured",
            ))
        })?;
        enrollment::decode_token(&value).map_err(Failure::Consumer)
    }
    async fn call(&self, operation: Operation, input: Value) -> Result<Value, Failure> {
        self.connection
            .call(operation, input)
            .await
            .map_err(Failure::Wire)
    }
    fn delay(&self, milliseconds: u64) -> BoxFuture<'static, ()> {
        tokio::time::sleep(Duration::from_millis(milliseconds)).boxed()
    }
    fn log(&self, _: &'static str, _: &'static str, _: Value) {}
    async fn close(&self) {
        self.connection.close().await;
    }
}
struct Port {
    files: FileStore,
    locale: String,
}
impl consumer::Port for Port {
    type Client = Client;
    async fn connect(&self) -> Result<Arc<Client>, Failure> {
        let env: HashMap<_, _> = std::env::vars().collect();
        let connection_file = discovery::connection_file(&env).map_err(|_| Failure::Other)?;
        let cwd = std::env::current_dir().map_err(|_| Failure::Other)?;
        let canonical = std::fs::canonicalize(&cwd).map_err(|_| Failure::Other)?;
        let canonical = canonical.to_string_lossy();
        #[cfg(windows)]
        let canonical = canonical
            .strip_prefix(r"\\?\UNC\")
            .map(|v| format!(r"\\{v}"))
            .unwrap_or_else(|| canonical.strip_prefix(r"\\?\").unwrap_or(&canonical).into());
        let identity = subc_protocol::BindIdentity::new(
            cwd,
            "opencode",
            format!("store-{:x}", Sha256::digest(canonical.as_bytes())),
        );
        let connection = Enrolled::connect(SubcConnector {
            connection_file,
            identity,
            handshake_timeout: Duration::from_secs(10),
        })
        .await
        .map_err(|_| Failure::Unreachable)?;
        Ok(Arc::new(Client {
            connection,
            files: self.files.clone(),
        }))
    }
    async fn active(&self) -> Result<bool, Failure> {
        Ok(self.files.token.exists())
    }
    fn projection(&self) -> Projection {
        Projection::new(&self.locale, now()).expect("validated roster locale")
    }
    fn on_roster(&self, _: &Value) {}
}
impl send::Port for Port {
    fn log(&self, _: &'static str, _: Value) {}
    fn on_error(&self, _: &send::Error) {}
}

pub struct Vault {
    pub authority: Source,
    source: OnceLock<Arc<send::Source<Port>>>,
    last_list: Mutex<(Vec<Value>, Option<Value>)>,
}
impl Vault {
    pub fn new(directory: &Path) -> Result<Self, String> {
        Ok(Self {
            authority: Source::new(directory).map_err(|e| e.to_string())?,
            source: OnceLock::new(),
            last_list: Mutex::new((vec![], None)),
        })
    }
    fn source(&self) -> Result<Arc<send::Source<Port>>, String> {
        if let Some(source) = self.source.get() {
            return Ok(source.clone());
        }
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
            .unwrap_or_else(|| "en-US".into());
        let locale = locale.split(['.', '@']).next().unwrap().replace('_', "-");
        let locale = if matches!(locale.as_str(), "C" | "POSIX") {
            "en-US".into()
        } else {
            locale
        };
        Projection::new(&locale, now()).map_err(|e| e.to_string())?;
        let port = Arc::new(Port {
            files: self.authority.store.clone(),
            locale,
        });
        let store = RosterStore::new(self.authority.mode_file.with_file_name("pi-roster.json"))
            .map_err(|e| e.to_string())?;
        let source = send::Source::new(port, store);
        Ok(self.source.get_or_init(|| source).clone())
    }
    async fn open(&self) -> Result<(Value, Arc<send::Source<Port>>), String> {
        let read = self.authority.request_mode().await?;
        let record = path::require_custody(&read)?.clone();
        Ok((record, self.source()?))
    }
    pub async fn send<D: send::Dispatch>(
        &self,
        dispatch: &mut D,
        signal: &CancellationToken,
    ) -> Result<D::Response, String> {
        let (record, source) = self.open().await?;
        if signal.is_cancelled() {
            return Err("This operation was aborted".into());
        }
        let mut routes = source.routes();
        if routes.is_empty() {
            source.refresh().await.map_err(|e| e.to_string())?;
            if signal.is_cancelled() {
                return Err("This operation was aborted".into());
            }
            routes = source.routes();
        }
        let reference =
            path::select_route(&routes, record.get("routeId"), source.empty_roster_cause())?;
        source
            .send(reference, dispatch, "model", signal, "direct")
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn list(&self) -> Result<Vec<Value>, String> {
        let (_, source) = self.open().await?;
        self.last_list.lock().unwrap().1 = None;
        let roster = source.refresh().await.map_err(|e| e.to_string())?;
        let reach = source.grant_reach();
        let (references, choices) = path::account_choices(roster.as_ref());
        *self.last_list.lock().unwrap() = (references, reach);
        Ok(choices)
    }
    pub fn grant_reach(&self) -> Option<Value> {
        self.last_list.lock().unwrap().1.clone()
    }
    pub async fn change_route(&self, route: &str, accept: bool) -> Result<(), String> {
        let (_, source) = self.open().await?;
        let reference = self
            .last_list
            .lock()
            .unwrap()
            .0
            .iter()
            .rev()
            .find(|v| v["routeId"] == route)
            .cloned()
            .ok_or("That vault account is no longer listed. Open the Vault section again.")?;
        source
            .change_route(&reference, accept)
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn close(&self) {
        if let Some(source) = self.source.get() {
            source.close().await;
        }
    }
}

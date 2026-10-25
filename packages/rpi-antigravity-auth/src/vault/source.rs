//! Pi's local/custody admission, derived from fresh enrollment reads each time.
use super::{ceremony::Store, enrollment, file_store::FileStore};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const PROPOSED_NAME: &str = "antigravity-auth-pi";
pub const MARKER: &str = "antigravity-vault-custody";
pub const UNREADABLE: &str =
    "Vault enrollment or source settings cannot be read. Local credentials remain unavailable.";
pub const UNAPPROVED: &str =
    "Vault enrollment must be approved before activating vault credentials.";
pub const LOCAL_EXCLUDED: &str =
    "Vault enrollment excludes local login, refresh and request execution.";
pub const SLOT_CONFLICT: &str = "The native Google login changed. It was not overwritten; restore the saved login after resolving the conflict.";
pub const CHANGED: &str = "Vault enrollment changed while the credential slot was being synchronized. The account source was not activated.";

pub fn parse_mode(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "version" | "mode" | "routeId"))
        || value["version"].as_f64() != Some(1.0)
        || !matches!(value["mode"].as_str(), Some("local" | "custody"))
    {
        return None;
    }
    let mut record = json!({"version":1,"mode":value["mode"]});
    if let Some(route) = value.get("routeId") {
        if crate::json::trim(route.as_str()?).is_empty() {
            return None;
        }
        record["routeId"] = route.clone();
    }
    Some(record)
}
pub fn read_mode(path: &Path) -> Value {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return json!({"ok":true,"record":{"version":1,"mode":"local"}})
        }
        Err(_) => return json!({"ok":false,"reason":"its vault settings file could not be read"}),
    };
    let Ok(value) = serde_json::from_str::<Value>(&String::from_utf8_lossy(&bytes)) else {
        return json!({"ok":false,"reason":"its vault settings file is damaged"});
    };
    match parse_mode(&value) {
        Some(record) => json!({"ok":true,"record":record}),
        None => {
            json!({"ok":false,"reason":"its vault settings file holds settings it does not understand"})
        }
    }
}

#[derive(Clone)]
pub struct Source<S = FileStore> {
    pub store: S,
    pub mode_file: PathBuf,
}
impl Source<FileStore> {
    /// Native storage is isolated; the Pi file formats and consumer identity remain intact.
    pub fn new(vault_dir: &Path) -> Result<Self, super::ceremony::Failure> {
        Ok(Self {
            store: FileStore::new(
                vault_dir.join("pi-enrollment-state.json"),
                vault_dir.join("pi-enrollment.json"),
            )?,
            mode_file: vault_dir.join("pi-mode.json"),
        })
    }
}
impl<S: Store> Source<S> {
    async fn guard(&self) -> Result<(), String> {
        // Ordinary IO/decoder failures retain the existing current/request error
        // contracts. A provenance refusal is typed and must survive those
        // diagnostic decoders with the notice the guard produced.
        let origin = |result: Result<Option<Value>, super::ceremony::Failure>| match result {
            Ok(value) => Ok(value),
            Err(super::ceremony::Failure::Refused(notice)) => Err(notice),
            Err(_) => Ok(None),
        };
        let state = origin(self.store.read(super::ceremony::Slot::State).await)?;
        let token = origin(self.store.read(super::ceremony::Slot::Token).await)?;
        let path = self.mode_file.clone();
        let mode = tokio::task::spawn_blocking(move || {
            super::file_store::read_raw_mode(&path).unwrap_or(None)
        })
        .await
        .map_err(|_| UNREADABLE.to_owned())?;
        super::import_guard::guard_storage(&super::import_guard::Files {
            mode,
            enrollment_state: state,
            enrollment_token: token,
            ..Default::default()
        })
    }

    /// The owning runtime serializes these transitions across startup and mode changes.
    pub async fn synchronize<F, Fut>(&self, transition: F) -> Result<Value, String>
    where
        F: FnOnce(Value) -> Fut,
        Fut: std::future::Future<Output = Result<Value, String>>,
    {
        let mode = self.current().await?;
        let result = transition(mode.clone()).await?;
        if mode["mode"] == "local" && result == "conflict" {
            return Err(SLOT_CONFLICT.into());
        }
        let after = self.current().await?;
        if after["mode"] != mode["mode"] {
            return Err(CHANGED.into());
        }
        Ok(after)
    }
    pub async fn status(&self) -> Value {
        let path = self.mode_file.clone();
        let mode = tokio::task::spawn_blocking(move || read_mode(&path))
            .await
            .unwrap_or_else(
                |_| json!({"ok":false,"reason":"its vault settings file could not be read"}),
            );
        let status=enrollment::read_status(&self.store,PROPOSED_NAME).await.unwrap_or_else(|_|json!({"state":"blocked","proposedName":PROPOSED_NAME,"code":"unreadable-state"}));
        let authority = enrollment::read_authority(&self.store, PROPOSED_NAME).await;
        let mut value = if authority != "local" {
            json!({"mode":if authority=="vault" {"custody"} else {"unreadable"}})
        } else if mode["ok"] == true {
            json!({"mode":mode["record"]["mode"]})
        } else {
            json!({"mode":"unreadable","modeProblem":mode["reason"]})
        };
        if mode["ok"] == true {
            if let Some(route) = mode["record"].get("routeId") {
                value["selectedRouteId"] = route.clone();
            }
        }
        value["enrollment"] = status;
        value
    }
    pub async fn current(&self) -> Result<Value, String> {
        self.guard().await?;
        let status = self.status().await;
        let authority = enrollment::read_authority(&self.store, PROPOSED_NAME).await;
        if authority == "unreadable" || status["mode"] == "unreadable" {
            return Err(UNREADABLE.into());
        }
        if authority == "vault" {
            let mut record = json!({"version":1,"mode":"custody"});
            if status["selectedRouteId"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            {
                record["routeId"] = status["selectedRouteId"].clone();
            }
            return Ok(record);
        }
        if status["mode"] != "local" {
            return Err(UNAPPROVED.into());
        }
        Ok(json!({"version":1,"mode":"local"}))
    }
    pub async fn assert_local(&self) -> Result<(), String> {
        if self.current().await?["mode"] != "local" {
            return Err(LOCAL_EXCLUDED.into());
        }
        Ok(())
    }
    pub async fn request_mode(&self) -> Result<Value, String> {
        self.guard().await?;
        let status = self.status().await;
        let authority = enrollment::read_authority(&self.store, PROPOSED_NAME).await;
        if authority == "local" && status["mode"] == "custody" {
            return Err("Vault enrollment is not active. No request was sent.".into());
        }
        if status["mode"] == "unreadable" {
            return Err("Vault enrollment or source settings cannot be read.".into());
        }
        let mut record = json!({"version":1,"mode":status["mode"]});
        if status["selectedRouteId"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
        {
            record["routeId"] = status["selectedRouteId"].clone();
        }
        Ok(json!({"ok":true,"record":record}))
    }
}

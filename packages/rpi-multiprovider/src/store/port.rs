//! Private credential transactions for the host's provider-owned auth resolver.
//!
//! Besides reading, locking, replacing and removing an existing account, the
//! port can create the record a completed login produces (`"add"`). That is the
//! upstream `MultiAuthStore.addAccount` path: the first account of a provider
//! materializes the pool the pool manager had only buffered, applying the
//! buffered policy, affinity, includeUpstream and upstream preferences, and the
//! resulting file is byte-identical to Pi's.
use super::{cancelled, records, schema, Failure, Lock, Store};
use crate::services::Error;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

pub const STORE_ID: &str = "rpi-multiprovider";
struct Transaction {
    _lock: Lock,
    state: Value,
    provider: String,
    account: String,
}

pub struct Port {
    store: Store,
    transactions: Mutex<HashMap<String, Arc<Mutex<Transaction>>>>,
}
fn error(message: impl Into<String>) -> Failure {
    Failure::Public(Error::Message(format!("multiprovider: {}", message.into())))
}
fn field<'a>(value: &'a Value, name: &str) -> Result<&'a str, Failure> {
    value[name]
        .as_str()
        .ok_or_else(|| error(format!("missing {name}")))
}
fn account<'a>(state: &'a Value, provider: &str, id: &str) -> Result<&'a Value, Failure> {
    // A reserved record is never a credential the host can read or replace.
    if super::is_reserved_account_id(id) {
        return Err(error(format!("stored account \"{id}\" is unavailable")));
    }
    schema::entries(&state["providers"])
        .into_iter()
        .find(|(name, _)| name == provider)
        .and_then(|(_, pool)| pool["accounts"].as_array())
        .and_then(|accounts| accounts.iter().find(|account| account["id"] == id))
        .ok_or_else(|| error(format!("stored account \"{id}\" is unavailable")))
}
fn result(kind: &str, payload: Value) -> Value {
    json!({"version":1,"kind":kind,"payload":payload})
}
impl Port {
    pub fn new(store: Store) -> Self {
        Self {
            store,
            transactions: Mutex::new(HashMap::new()),
        }
    }
    pub async fn request(&self, input: Value, signal: CancellationToken) -> Result<Value, Failure> {
        if input["version"] != 1 || input["storeId"] != STORE_ID {
            return Err(error("invalid credential store request"));
        }
        let provider = field(&input, "providerId")?.to_owned();
        schema::safe_key(&provider, "provider id")?;
        let action = field(&input, "action")?;
        // A login-added record has no account identity yet: the store mints one.
        if action == "add" {
            return self.add(provider, input["input"].clone(), signal).await;
        }
        let id = field(&input, "accountId")?.to_owned();
        schema::safe_key(&id, "account id")?;
        match action {
            "read" => {
                let store = self.store.clone();
                blocking(move || {
                    if signal.is_cancelled() {
                        return Err(cancelled());
                    }
                    let state = store.read()?;
                    let account = account(&state, &provider, &id)?;
                    if !records::truthy(&account["enabled"]) {
                        return Err(error(format!("stored account \"{id}\" is unavailable")));
                    }
                    Ok(result("credential", account["credential"].clone()))
                })
                .await
            }
            "lock" => {
                let store = self.store.clone();
                let signal = signal.child_token();
                let _cancel = signal.clone().drop_guard();
                let transaction = blocking(move || {
                    let lock = store.lock(&signal)?;
                    let state = store.read()?;
                    account(&state, &provider, &id)?;
                    Ok(Transaction {
                        _lock: lock,
                        state,
                        provider,
                        account: id,
                    })
                })
                .await?;
                let credential = account(
                    &transaction.state,
                    &transaction.provider,
                    &transaction.account,
                )?["credential"]
                    .clone();
                let token = uuid::Uuid::new_v4().to_string();
                self.transactions
                    .lock()
                    .unwrap()
                    .insert(token.clone(), Arc::new(Mutex::new(transaction)));
                Ok(result(
                    "locked",
                    json!({"lockId":token,"credential":credential}),
                ))
            }
            "replace" | "remove" => {
                let token = field(&input["input"], "lockId")?;
                let transaction = self
                    .transactions
                    .lock()
                    .unwrap()
                    .get(token)
                    .cloned()
                    .ok_or_else(|| error("expired credential transaction"))?;
                let store = self.store.clone();
                blocking(move || {
                    let mut transaction = transaction.lock().unwrap();
                    if transaction.provider != provider || transaction.account != id {
                        return Err(error("credential transaction identity mismatch"));
                    }
                    let request = if input["action"] == "replace" {
                        let credential = &input["input"]["credential"];
                        schema::credential(credential)?;
                        json!({"op":"replace","provider":provider,"account":id,"credential":credential})
                    } else {
                        json!({"op":"remove","provider":provider,"account":id})
                    };
                    let mut next = transaction.state.clone();
                    records::execute(&mut next, &request, &*store.now, &*store.random_id)?;
                    store.write(&next)?;
                    transaction.state = next;
                    Ok(result("success", Value::Null))
                }).await
            }
            _ => Err(error("unknown credential store operation")),
        }
    }
    /// Creates the record a completed login produces. The account is pending
    /// until this write commits: it takes the store lock, reads the current
    /// state, runs the same `add` operation the pool manager uses and persists
    /// atomically. The result is the public account (its credential stays
    /// private) and the file matches Pi's `addAccount` output byte for byte.
    async fn add(
        &self,
        provider: String,
        input: Value,
        signal: CancellationToken,
    ) -> Result<Value, Failure> {
        let mut request = json!({"op":"add","provider":provider,"input":input});
        records::prepare(&mut request)?;
        let store = self.store.clone();
        blocking(move || {
            if signal.is_cancelled() {
                return Err(cancelled());
            }
            let _lock = store.lock(&signal)?;
            let mut state = store.read()?;
            let account = records::execute(&mut state, &request, &*store.now, &*store.random_id)?;
            store.write(&state)?;
            Ok(result("account", account))
        })
        .await
    }
    /// In-progress persistence owns its guard even if the host releases early.
    pub fn release(&self, token: &str) {
        self.transactions.lock().unwrap().remove(token);
    }
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| error("credential store task stopped"))?
}

//! Awaited enrollment transitions. The storage lease fences all setup mutations.
use super::{
    client::Operation,
    connection::{Connector, Enrolled},
    enrollment,
    errors::Error,
    policy::ConsumerError,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{future::Future, sync::Arc};

#[derive(Clone, Debug)]
pub enum Failure {
    Consumer(ConsumerError),
    Wire(Error),
    Storage(String),
    Private(super::private::PrivateError),
    Io(super::private::IoError),
    LockContention {
        target: std::path::PathBuf,
        name: String,
        timeout_ms: u64,
    },
    LockLost {
        target: std::path::PathBuf,
        name: String,
        expected_owner: String,
        observed: Option<Value>,
    },
}
impl From<ConsumerError> for Failure {
    fn from(value: ConsumerError) -> Self {
        Self::Consumer(value)
    }
}
impl From<Error> for Failure {
    fn from(value: Error) -> Self {
        Self::Wire(value)
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Consumer(e) => e.fmt(f),
            Self::Wire(e) => e.fmt(f),
            Self::Storage(e) => f.write_str(e),
            Self::Private(e) => e.fmt(f),
            Self::Io(e) => f.write_str(&e.message),
            Self::LockContention { target, name, .. } => write!(
                f,
                "Timed out acquiring {name} lock for {}",
                target.display()
            ),
            Self::LockLost { target, name, .. } => {
                write!(f, "Lost {name} lock for {}", target.display())
            }
        }
    }
}
impl std::error::Error for Failure {}
impl Failure {
    pub fn io_code(&self) -> Option<&str> {
        match self {
            Self::Io(e) => Some(&e.code),
            Self::Private(e) => Some(&e.code),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    State,
    Token,
}
pub trait Lease: Send {
    fn assert_owned(&self) -> impl Future<Output = Result<(), Failure>> + Send;
    fn release(self) -> impl Future<Output = Result<(), Failure>> + Send;
}
pub trait Store: Send + Sync + 'static {
    type Lease: Lease;
    fn acquire(&self) -> impl Future<Output = Result<Option<Self::Lease>, Failure>> + Send;
    fn read(&self, slot: Slot) -> impl Future<Output = Result<Option<Value>, Failure>> + Send;
    fn write(&self, slot: Slot, value: Value) -> impl Future<Output = Result<(), Failure>> + Send;
    fn remove(&self, slot: Slot) -> impl Future<Output = Result<bool, Failure>> + Send;
}
pub trait Client: Send + Sync + 'static {
    fn invoke(
        &self,
        operation: Operation,
        input: Value,
    ) -> impl Future<Output = Result<Value, Error>> + Send;
}
impl<C: Connector> Client for Arc<Enrolled<C>> {
    fn invoke(
        &self,
        operation: Operation,
        input: Value,
    ) -> impl Future<Output = Result<Value, Error>> + Send {
        self.call(operation, input)
    }
}
pub struct Manager<S: Store, C: Client> {
    pub store: S,
    pub client: C,
    pub proposed_name: String,
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub mint_secret: Arc<dyn Fn() -> Result<String, Failure> + Send + Sync>,
}
fn wrong_consumer() -> Failure {
    ConsumerError::new(
        "wrong-consumer",
        "Claustrum enrollment state belongs to a different consumer",
    )
    .into()
}
impl<S: Store, C: Client> Manager<S, C> {
    pub async fn status(&self) -> Result<Value, Failure> {
        enrollment::read_status(&self.store, &self.proposed_name).await
    }
    async fn block(&self, state: &Value, code: &str) -> Result<Value, Failure> {
        let state = json!({"version":1,"phase":"blocked","proposedName":state["proposedName"],"errorCode":code,"updatedAt":(self.now)()});
        self.store.write(Slot::State, state.clone()).await?;
        Ok(enrollment::status_from_state(&state))
    }
    /// The caller must keep this operation alive through persistence once it starts.
    pub async fn reconcile(&self) -> Result<Value, Failure> {
        let Some(lease) = self.store.acquire().await? else {
            return Ok(json!({"state":"busy"}));
        };
        let result = self.reconcile_locked().await;
        lease.release().await?;
        result
    }
    async fn reconcile_locked(&self) -> Result<Value, Failure> {
        let token = self
            .store
            .read(Slot::Token)
            .await?
            .as_ref()
            .map(enrollment::decode_token)
            .transpose()?;
        let mut state = self
            .store
            .read(Slot::State)
            .await?
            .as_ref()
            .map(enrollment::decode_state)
            .transpose()?;
        if state
            .as_ref()
            .is_some_and(|s| s["proposedName"] != self.proposed_name)
        {
            return Err(wrong_consumer());
        }
        if let Some(token) = token {
            if state.as_ref().map_or(true, |s| s["phase"] == "pending") {
                let approved = json!({"version":1,"phase":"approved","proposedName":state.as_ref().map(|s|s["proposedName"].clone()).unwrap_or(json!(self.proposed_name)),"tokenGeneration":token["token_generation"],"updatedAt":(self.now)()});
                self.store.write(Slot::State, approved.clone()).await?;
                state = Some(approved);
            }
            let state = state.unwrap();
            let mut result = json!({"state":"approved","proposedName":state["proposedName"],"tokenGeneration":token["token_generation"]});
            if state["phase"] == "approved" {
                if let Some(name) = state.get("approvedName") {
                    result["approvedName"] = name.clone();
                }
            }
            return Ok(result);
        }
        if let Some(state) = &state {
            if state["phase"] == "approved" {
                return self.block(state, "missing_token").await;
            }
            if state["phase"] != "pending" {
                return Ok(enrollment::status_from_state(state));
            }
        }
        let mut state = match state {
            Some(state) => state,
            None => {
                let now = (self.now)();
                let secret = (self.mint_secret)()?;
                if !enrollment::valid_token(&json!(secret)) {
                    return Err(ConsumerError::new(
                        "invalid-state",
                        "invalid minted Claustrum enrollment secret",
                    )
                    .into());
                }
                let state = json!({"version":1,"phase":"pending","proposedName":self.proposed_name,"requestSecret":secret,"createdAt":now,"updatedAt":now});
                self.store.write(Slot::State, state.clone()).await?;
                state
            }
        };
        if state.get("requestId").is_none() {
            // The secret is committed before proposing, so the same ceremony can be retried.
            let bytes = secret_bytes(&state["requestSecret"]);
            let hash = format!("{:x}", Sha256::digest(bytes));
            let proposed = self
                .client
                .invoke(
                    Operation::Propose,
                    json!({"name":state["proposedName"],"requestSecretHash":hash}),
                )
                .await;
            match proposed {
                Ok(proposed) => {
                    state["requestId"] = proposed["requestId"].clone();
                    state["updatedAt"] = json!((self.now)());
                    self.store.write(Slot::State, state.clone()).await?;
                }
                Err(error) if enrollment::retryable(&error) => {
                    return Ok(
                        json!({"state":"pending","proposedName":state["proposedName"],"retryCode":error.code}),
                    )
                }
                Err(error) => return self.block(&state, &error.code).await,
            }
        }
        let outcome = self
            .client
            .invoke(
                Operation::Poll,
                json!({"requestId":state["requestId"],"requestSecret":state["requestSecret"]}),
            )
            .await;
        match outcome {
            Err(error) if enrollment::retryable(&error) => Ok(
                json!({"state":"pending","proposedName":state["proposedName"],"requestId":state["requestId"],"retryCode":error.code}),
            ),
            Err(error) => self.block(&state, &error.code).await,
            Ok(outcome) if outcome["status"] == "pending" => {
                Ok(enrollment::status_from_state(&state))
            }
            Ok(outcome) => {
                let next = if outcome["status"] == "denied" {
                    json!({"version":1,"phase":"denied","proposedName":state["proposedName"],"updatedAt":(self.now)()})
                } else {
                    self.store.write(Slot::Token,json!({"token":outcome["token"],"token_generation":outcome["tokenGeneration"]})).await?;
                    json!({"version":1,"phase":"approved","proposedName":state["proposedName"],"approvedName":outcome["name"],"tokenGeneration":outcome["tokenGeneration"],"updatedAt":(self.now)()})
                };
                self.store.write(Slot::State, next.clone()).await?;
                Ok(enrollment::status_from_state(&next))
            }
        }
    }
    pub async fn reset_terminal(&self) -> Result<&'static str, Failure> {
        let Some(lease) = self.store.acquire().await? else {
            return Ok("busy");
        };
        let result = async {
            if self.store.read(Slot::Token).await?.is_some() {
                return Ok("refused-approved");
            }
            let Some(value) = self.store.read(Slot::State).await? else {
                return Ok("idle");
            };
            let state = enrollment::decode_state(&value)?;
            if state["proposedName"] != self.proposed_name {
                return Err(wrong_consumer());
            }
            if state["phase"] == "pending" {
                return Ok("refused-pending");
            }
            lease.assert_owned().await?;
            self.store.remove(Slot::State).await?;
            Ok("reset")
        }
        .await;
        lease.release().await?;
        result
    }
    pub async fn disconnect(&self) -> Result<&'static str, Failure> {
        disconnect(&self.store, &self.proposed_name).await
    }
}
fn secret_bytes(value: &Value) -> Vec<u8> {
    // Retain the original Buffer.from(array) coercion for accepted legacy state files.
    if let Some(values) = value.as_array() {
        return values
            .iter()
            .map(|value| {
                let number = crate::json::string(value)
                    .parse::<f64>()
                    .unwrap_or(f64::NAN);
                if number.is_finite() {
                    number.trunc().rem_euclid(256.0) as u8
                } else {
                    0
                }
            })
            .collect();
    }
    value
        .as_str()
        .unwrap()
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

pub async fn disconnect<S: Store>(store: &S, proposed_name: &str) -> Result<&'static str, Failure> {
    let Some(lease) = store.acquire().await? else {
        return Ok("busy");
    };
    let result = async {
        let state = async {
            store
                .read(Slot::State)
                .await?
                .as_ref()
                .map(enrollment::decode_state)
                .transpose()
                .map_err(Failure::from)
        }
        .await;
        match state {
            Ok(Some(state)) if state["proposedName"] != proposed_name => {
                return Err(wrong_consumer())
            }
            Err(Failure::Consumer(error)) if error.kind == "invalid-state" => {}
            Err(error) => return Err(error),
            _ => {}
        }
        lease.assert_owned().await?;
        let token = store.remove(Slot::Token).await?;
        lease.assert_owned().await?;
        let state = store.remove(Slot::State).await?;
        Ok(if token || state {
            "disconnected"
        } else {
            "idle"
        })
    }
    .await;
    lease.release().await?;
    result
}

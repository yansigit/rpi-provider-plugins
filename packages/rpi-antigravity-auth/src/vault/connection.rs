//! Claustrum's enrolled connection policy over the shared Rust Subc consumer.
use super::{
    client::{self, Operation},
    errors::Error,
};
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde_json::Value;
use std::{
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subc_client_rs::consumer::{
    is_retryable_route_open_code, CallError, CallOptions, ConsumerOptions, OutcomeUnknownCause,
    SubcConsumer,
};
use subc_protocol::{BindIdentity, RouteTarget};

#[derive(Clone, Debug)]
pub enum Failure {
    Subc {
        terminal: bool,
        code: Option<String>,
    },
    Credential(Error),
    Other,
}
impl Failure {
    fn reconnect(&self) -> bool {
        matches!(self, Self::Subc {terminal:true, code} if !matches!(code.as_deref(), Some("missing_identity" | "invalid_control_body" | "unknown_module")))
    }
    fn public(self) -> Error {
        match self {
            Self::Credential(error) => error,
            Self::Subc { code, .. } => Error::transport(
                code.filter(|c| !c.is_empty())
                    .as_deref()
                    .unwrap_or("transport_error"),
            ),
            Self::Other => Error::transport("transport_error"),
        }
    }
}
pub trait Transport: Send + Sync + 'static {
    fn call(&self, request: Value) -> impl Future<Output = Result<Value, Failure>> + Send;
    fn close(&self) -> impl Future<Output = ()> + Send;
}
pub trait Connector: Send + Sync + 'static {
    type Transport: Transport;
    fn now_ms(&self) -> i64;
    fn connect(&self) -> impl Future<Output = Result<Self::Transport, Failure>> + Send;
}

/// Connections are shared; each request keeps the connection it actually used.
pub struct Enrolled<C: Connector> {
    connector: C,
    current: Mutex<Arc<C::Transport>>,
    reconnect: Mutex<Reconnect>,
    closed: AtomicBool,
}
#[derive(Default)]
struct Reconnect {
    next_at: i64,
    pending: Option<Shared<BoxFuture<'static, Result<(), Failure>>>>,
}
impl<C: Connector> Enrolled<C> {
    pub async fn connect(connector: C) -> Result<Arc<Self>, Failure> {
        let current = connector.connect().await?;
        Ok(Arc::new(Self::with_connection(connector, current)))
    }
    pub fn with_connection(connector: C, current: C::Transport) -> Self {
        Self {
            connector,
            current: Mutex::new(Arc::new(current)),
            reconnect: Mutex::new(Reconnect::default()),
            closed: AtomicBool::new(false),
        }
    }
    fn current(&self) -> Arc<C::Transport> {
        self.current.lock().unwrap().clone()
    }
    fn reconnect(self: &Arc<Self>) -> impl Future<Output = Result<(), Failure>> + Send {
        let mut state = self.reconnect.lock().unwrap();
        let pending = if self.closed.load(Ordering::Acquire) {
            futures_util::future::ready(Err(Failure::Other))
                .boxed()
                .shared()
        } else if let Some(pending) = &state.pending {
            pending.clone()
        } else if self.connector.now_ms() < state.next_at {
            futures_util::future::ready(Err(Failure::Other))
                .boxed()
                .shared()
        } else {
            state.next_at = self.connector.now_ms().saturating_add(60_000);
            let client = self.clone();
            let task = tokio::spawn(async move {
                let result = async {
                    let connected = Arc::new(client.connector.connect().await?);
                    let previous = {
                        let mut current = client.current.lock().unwrap();
                        (!client.closed.load(Ordering::Acquire))
                            .then(|| std::mem::replace(&mut *current, connected.clone()))
                    };
                    let Some(previous) = previous else {
                        connected.close().await;
                        return Err(Failure::Other);
                    };
                    previous.close().await;
                    Ok(())
                }
                .await;
                client.reconnect.lock().unwrap().pending = None;
                result
            });
            let pending = async move { task.await.map_err(|_| Failure::Other)? }
                .boxed()
                .shared();
            state.pending = Some(pending.clone());
            pending
        };
        pending
    }
    async fn raw(
        self: &Arc<Self>,
        sent: Arc<C::Transport>,
        request: Value,
    ) -> Result<Value, Error> {
        match sent.call(request.clone()).await {
            Ok(value) => Ok(value),
            Err(error) if !self.closed.load(Ordering::Acquire) && error.reconnect() => {
                if Arc::ptr_eq(&self.current(), &sent) {
                    self.reconnect().await.map_err(Failure::public)?;
                }
                self.current().call(request).await.map_err(Failure::public)
            }
            Err(error) => Err(error.public()),
        }
    }
    /// Keep the operation alive if one caller stops waiting, as the original Promise does.
    pub fn call(
        self: &Arc<Self>,
        operation: Operation,
        input: Value,
    ) -> impl Future<Output = Result<Value, Error>> + Send {
        let client = self.clone();
        let sent = self.current();
        let pending = tokio::spawn(async move {
            let reply = client.raw(sent, client::request(operation, &input)).await?;
            client::decode(operation, &reply)
        });
        async move {
            pending
                .await
                .map_err(|_| Error::transport("transport_error"))?
        }
    }
    pub async fn close(&self) {
        let current = {
            let current = self.current.lock().unwrap();
            self.closed.store(true, Ordering::Release);
            current.clone()
        };
        current.close().await;
    }
}

pub struct SubcConnector {
    pub connection_file: PathBuf,
    pub identity: BindIdentity,
    pub handshake_timeout: Duration,
}
pub struct SubcTransport {
    client: SubcConsumer,
    identity: BindIdentity,
}
impl Connector for SubcConnector {
    type Transport = SubcTransport;
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
    async fn connect(&self) -> Result<SubcTransport, Failure> {
        let client = SubcConsumer::connect(
            &self.connection_file,
            ConsumerOptions {
                inherit_env_identity: false,
                handshake_timeout: self.handshake_timeout,
                ..Default::default()
            },
        )
        .await
        .map_err(|_| Failure::Other)?;
        Ok(SubcTransport {
            client,
            identity: self.identity.clone(),
        })
    }
}
impl Transport for SubcTransport {
    async fn call(&self, request: Value) -> Result<Value, Failure> {
        let reply = self
            .client
            .call_reply(
                RouteTarget::ManagementSurface {
                    module_id: "claustrum".into(),
                },
                self.identity.clone(),
                request.to_string().into_bytes(),
                CallOptions {
                    response_timeout_only: true,
                    retry_module_reloading: false,
                    ..Default::default()
                },
            )
            .await
            .map_err(from_subc)?;
        if reply.flags.is_binary() {
            return Ok(Value::Null);
        }
        // Subc wraps JSON decoding failures as terminal, uncoded call errors.
        serde_json::from_str(&String::from_utf8_lossy(&reply.body)).map_err(|_| Failure::Subc {
            terminal: true,
            code: None,
        })
    }
    async fn close(&self) {
        self.client.close().await;
    }
}
fn from_subc(error: CallError) -> Failure {
    if let Some(body) = error.route_open_refusal() {
        return Failure::Subc {
            terminal: !is_retryable_route_open_code(&body.code),
            code: Some(body.code.clone()),
        };
    }
    let code = error
        .code()
        .map(str::to_owned)
        .or_else(|| match error.outcome_cause() {
            Some(OutcomeUnknownCause::Deadline) => {
                Some("deadline_exceeded_no_drop_observed".into())
            }
            Some(OutcomeUnknownCause::RouteEnded) => Some("route_closed".into()),
            _ => None,
        });
    Failure::Subc {
        terminal: !matches!(
            error,
            CallError::NotSent(_) | CallError::OutcomeUnknown(_) | CallError::StaleRouteHandle(_)
        ),
        code: if matches!(error, CallError::StaleRouteHandle(_)) {
            Some("stale_route_handle".into())
        } else {
            code
        },
    }
}

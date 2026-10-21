//! Pi's reachable common-auth consumer operations. Pi never calls the background start path.
use super::{
    custody::{Custody, Failure, Port as CustodyPort, Receipt},
    policy::ConsumerError,
    roster::Projection,
    roster_store::RosterStore,
};
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

pub trait Port: Send + Sync + 'static {
    type Client: CustodyPort;
    fn connect(&self) -> impl Future<Output = Result<Arc<Self::Client>, Failure>> + Send;
    fn active(&self) -> impl Future<Output = Result<bool, Failure>> + Send;
    fn projection(&self) -> Projection;
    fn on_roster(&self, roster: &Value);
}
type Connecting<P> = Shared<BoxFuture<'static, Result<Arc<Custody<<P as Port>::Client>>, Failure>>>;
type Refresh = Shared<BoxFuture<'static, Result<Option<Value>, Failure>>>;
struct State<P: Port> {
    custody: Option<Arc<Custody<P::Client>>>,
    connecting: Option<Connecting<P>>,
    refreshing: Option<Refresh>,
    roster: Option<Value>,
    grant_reach: Option<(Value, Value)>,
}
pub struct Consumer<P: Port> {
    port: Arc<P>,
    store: RosterStore,
    shutdown: CancellationToken,
    state: Mutex<State<P>>,
}
fn error(kind: &'static str, message: &str) -> Failure {
    Failure::Consumer(ConsumerError::new(kind, message))
}
fn closed() -> Failure {
    error("closed", "Claustrum consumer is closed")
}
fn row<'a>(roster: Option<&'a Value>, route: &str) -> Option<&'a Value> {
    roster?
        .get("rows")?
        .as_array()?
        .iter()
        .find(|v| v["routeId"] == route)
}
impl<P: Port> Consumer<P> {
    pub fn new(port: Arc<P>, store: RosterStore) -> Arc<Self> {
        Arc::new(Self {
            port,
            store,
            shutdown: CancellationToken::new(),
            state: Mutex::new(State {
                custody: None,
                connecting: None,
                refreshing: None,
                roster: None,
                grant_reach: None,
            }),
        })
    }
    fn check(&self, signal: Option<&CancellationToken>) -> Result<(), Failure> {
        if self.shutdown.is_cancelled() {
            Err(closed())
        } else if signal.is_some_and(CancellationToken::is_cancelled) {
            Err(Failure::Cancelled)
        } else {
            Ok(())
        }
    }
    async fn wait<T>(
        &self,
        pending: impl Future<Output = Result<T, Failure>>,
        signal: &CancellationToken,
    ) -> Result<T, Failure> {
        tokio::select! {biased;_=self.shutdown.cancelled()=>Err(closed()),_=signal.cancelled()=>Err(Failure::Cancelled),result=pending=>{match result {Ok(value)=>{self.check(Some(signal))?;Ok(value)},Err(e)=>Err(e)}}}
    }
    pub fn snapshot(&self) -> Option<Value> {
        self.state.lock().unwrap().roster.clone()
    }
    pub fn grant_reach(&self) -> Option<Value> {
        let state = self.state.lock().unwrap();
        state
            .grant_reach
            .as_ref()
            .filter(|(view, _)| {
                state
                    .roster
                    .as_ref()
                    .is_some_and(|v| v.get("view") == Some(view))
            })
            .map(|(_, reach)| reach.clone())
    }
    fn custody(&self) -> Option<Arc<Custody<P::Client>>> {
        self.state.lock().unwrap().custody.clone()
    }
    fn connecting(self: &Arc<Self>) -> Connecting<P> {
        if self.shutdown.is_cancelled() {
            return futures_util::future::ready(Err(closed())).boxed().shared();
        }
        let mut state = self.state.lock().unwrap();
        if let Some(custody) = &state.custody {
            return futures_util::future::ready(Ok(custody.clone()))
                .boxed()
                .shared();
        }
        if let Some(pending) = &state.connecting {
            return pending.clone();
        }
        let consumer = self.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let client = consumer.port.connect().await?;
                let custody = Custody::new(client);
                let accepted = {
                    let mut state = consumer.state.lock().unwrap();
                    if consumer.shutdown.is_cancelled() {
                        false
                    } else {
                        state.custody = Some(custody.clone());
                        true
                    }
                };
                if !accepted {
                    custody.close().await;
                    return Err(closed());
                }
                Ok(custody)
            }
            .await;
            consumer.state.lock().unwrap().connecting = None;
            result
        });
        let pending = async move { task.await.unwrap_or(Err(Failure::Other)) }
            .boxed()
            .shared();
        state.connecting = Some(pending.clone());
        pending
    }
    /// A caller may stop waiting; admitted metadata work still publishes or cleans up.
    pub fn refresh(self: &Arc<Self>) -> Refresh {
        if self.shutdown.is_cancelled() {
            return futures_util::future::ready(Err(closed())).boxed().shared();
        }
        let mut state = self.state.lock().unwrap();
        if let Some(pending) = &state.refreshing {
            return pending.clone();
        }
        let consumer = self.clone();
        let task = tokio::spawn(async move {
            let result = consumer.refresh_inner().await;
            consumer.state.lock().unwrap().refreshing = None;
            result
        });
        let pending = async move { task.await.unwrap_or(Err(Failure::Other)) }
            .boxed()
            .shared();
        state.refreshing = Some(pending.clone());
        pending
    }
    async fn refresh_inner(self: &Arc<Self>) -> Result<Option<Value>, Failure> {
        if !self.port.active().await? {
            let custody = {
                let mut state = self.state.lock().unwrap();
                state.roster = None;
                state.grant_reach = None;
                state.custody.take()
            };
            if let Some(custody) = custody {
                custody.close().await;
            }
            return Ok(None);
        }
        let custody = self
            .wait(self.connecting(), &CancellationToken::new())
            .await?;
        let discovered = Arc::new(Mutex::new(None::<Value>));
        let capture = discovered.clone();
        let discoverer = custody.clone();
        let active = self.port.clone();
        let projection = self.port.clone();
        let roster = self
            .store
            .refresh(
                move |signal| async move {
                    let inventory = discoverer.discover(&signal).await?;
                    *capture.lock().unwrap() = Some(inventory.clone());
                    Ok(inventory)
                },
                self.shutdown.clone(),
                move || {
                    let active = active.clone();
                    async move { active.active().await }
                },
                move || projection.projection(),
            )
            .await?;
        let inventory = discovered.lock().unwrap().take();
        let changed = {
            let mut state = self.state.lock().unwrap();
            self.check(None)?;
            let changed = roster.as_ref().and_then(|v| v.get("view"))
                != state.roster.as_ref().and_then(|v| v.get("view"));
            state.roster = roster.clone();
            custody.retain(
                roster
                    .as_ref()
                    .and_then(|v| v["rows"].as_array())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            );
            if let (Some(inventory), Some(view)) =
                (inventory, roster.as_ref().and_then(|v| v.get("view")))
            {
                state.grant_reach = inventory
                    .get("grantReach")
                    .map(|reach| (view.clone(), reach.clone()));
            }
            changed
        };
        if changed {
            if let Some(roster) = &roster {
                self.port.on_roster(roster);
            }
        }
        Ok(roster)
    }
    pub fn authorize(
        self: &Arc<Self>,
        route: String,
        signal: CancellationToken,
    ) -> impl Future<Output = Result<Arc<Receipt>, Failure>> + Send {
        let consumer = self.clone();
        let task = tokio::spawn(async move { consumer.authorize_inner(route, signal).await });
        async move { task.await.unwrap_or(Err(Failure::Other)) }
    }
    async fn authorize_inner(
        self: &Arc<Self>,
        route: String,
        signal: CancellationToken,
    ) -> Result<Arc<Receipt>, Failure> {
        self.check(Some(&signal))?;
        let roster = match self.snapshot() {
            Some(value) => Some(value),
            None => self.wait(self.refresh(), &signal).await?,
        };
        if roster.is_none() || !self.port.active().await? {
            let custody = self.state.lock().unwrap().custody.take();
            if let Some(custody) = custody {
                custody.close().await;
            }
            return Err(error(
                "not-active",
                "Claustrum scoped custody is not active",
            ));
        }
        let current = self.store.read().await?;
        self.check(Some(&signal))?;
        let observed = row(roster.as_ref(), &route);
        let configured = row(current.as_ref(), &route);
        let refused = if configured.is_some_and(|v| v["enabled"] == false) {
            Some(error("route-declined", "Claustrum route is disabled"))
        } else if observed.zip(configured).map_or(true, |(a, b)| {
            b["state"] != "active"
                || a["credentialId"] != b["credentialId"]
                || a.get("accountIdentity") != b.get("accountIdentity")
                || a["credentialType"] != b["credentialType"]
        }) {
            Some(error(
                "route-unavailable",
                "Claustrum route is disabled, removed or changed",
            ))
        } else {
            None
        };
        if let Some(error) = refused {
            if let (Some(observed), Some(custody)) = (observed, self.custody()) {
                custody.forget(observed["credentialId"].as_str().unwrap());
            }
            return Err(error);
        }
        let configured = configured.unwrap();
        let custody = self.wait(self.connecting(), &signal).await?;
        let combined = self.shutdown.child_token();
        let mut identity = json!({"credentialId":configured["credentialId"],"credentialType":configured["credentialType"]});
        if let Some(account) = configured.get("accountIdentity") {
            identity["accountIdentity"] = account.clone();
        }
        let pending = custody.authorize(&identity, &combined, &route);
        tokio::pin!(pending);
        tokio::select! {biased;_=signal.cancelled()=>{combined.cancel();pending.await},result=&mut pending=>result}
    }
    pub async fn report_failure(
        self: &Arc<Self>,
        attempt: Arc<Receipt>,
        status: u16,
        source: &str,
    ) -> Result<(), Failure> {
        self.check(None)?;
        let custody = self.custody().ok_or_else(|| {
            error(
                "no-receipt",
                "Claustrum dispatch receipt has no active owner",
            )
        })?;
        custody.report_failure(&attempt, status, source).await
    }
    pub fn change_route(
        self: &Arc<Self>,
        route: String,
        accept: bool,
    ) -> impl Future<Output = Result<(), Failure>> + Send {
        let consumer = self.clone();
        let task = tokio::spawn(async move {
            consumer.check(None)?;
            consumer.store.change_route(route, accept).await?;
            let roster = consumer.store.read().await?;
            consumer.state.lock().unwrap().roster = roster.clone();
            if !accept {
                if let Some(custody) = consumer.custody() {
                    custody.retain(
                        roster
                            .as_ref()
                            .and_then(|v| v["rows"].as_array())
                            .map(Vec::as_slice)
                            .unwrap_or(&[]),
                    );
                }
            }
            Ok(())
        });
        async move { task.await.unwrap_or(Err(Failure::Other)) }
    }
    pub async fn close(&self) {
        if self.shutdown.is_cancelled() {
            return;
        }
        self.shutdown.cancel();
        let custody = {
            let mut state = self.state.lock().unwrap();
            state.roster = None;
            state.grant_reach = None;
            state.custody.clone()
        };
        if let Some(custody) = custody {
            custody.close().await;
        }
    }
}

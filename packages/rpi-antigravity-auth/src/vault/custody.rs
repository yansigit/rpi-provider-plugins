//! Per-send scoped receipts and the pinned common-auth outage policy.
use super::{
    client::{self, Operation},
    errors::Error,
    policy::{self, Authorized, ConsumerError},
};
use futures_util::{
    future::{BoxFuture, Shared},
    FutureExt,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, Weak},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum Failure {
    Wire(Error),
    Consumer(ConsumerError),
    Enrollment(super::ceremony::Failure),
    Unreachable,
    Cancelled,
    Other,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wire(e) => e.fmt(f),
            Self::Consumer(e) => e.fmt(f),
            Self::Enrollment(e) => e.fmt(f),
            Self::Unreachable | Self::Other => {
                f.write_str("Claustrum scoped operation unavailable")
            }
            Self::Cancelled => f.write_str("This operation was aborted"),
        }
    }
}
impl std::error::Error for Failure {}
impl From<super::ceremony::Failure> for Failure {
    fn from(value: super::ceremony::Failure) -> Self {
        Self::Enrollment(value)
    }
}
impl Failure {
    fn unreachable(&self) -> bool {
        matches!(self, Self::Unreachable)
            || matches!(self, Self::Wire(e) if policy::unreachable(&e.code))
    }
}
fn consumer(kind: &'static str, message: &str) -> Failure {
    Failure::Consumer(ConsumerError::new(kind, message))
}

/// I/O and time are supplied by the owning runtime; neither bearer nor enrollment enters logs.
pub trait Port: Send + Sync + 'static {
    fn now_ms(&self) -> f64;
    fn token(&self) -> impl Future<Output = Result<Value, Failure>> + Send;
    fn call(
        &self,
        operation: Operation,
        input: Value,
    ) -> impl Future<Output = Result<Value, Failure>> + Send;
    fn delay(&self, milliseconds: u64) -> BoxFuture<'static, ()>;
    fn log(&self, level: &'static str, message: &'static str, fields: Value);
    fn close(&self) -> impl Future<Output = ()> + Send;
}

type Report = Shared<BoxFuture<'static, Result<(), Failure>>>;
/// Opaque provenance prevents another custody instance from reporting an invented receipt.
pub struct Receipt {
    authorized: Authorized,
    owner: Weak<()>,
    enrollment_token: String,
    kept_route: Option<String>,
    report: Mutex<Option<Report>>,
}
impl Receipt {
    pub fn metadata(&self) -> &Value {
        &self.authorized.metadata
    }
    pub fn access_token(&self) -> &str {
        self.authorized.access_token()
    }
}
struct Kept {
    attempt: Arc<Receipt>,
    since: Mutex<Option<f64>>,
}
struct State {
    kept: HashMap<String, Arc<Kept>>,
    outage: bool,
    outage_error: Failure,
    timer: Option<Arc<CancellationToken>>,
    probing: bool,
    last_probe: f64,
    probe_delay: u64,
    closed: bool,
    discovered_view: Option<Value>,
}
pub struct Custody<P: Port> {
    port: Arc<P>,
    owner: Arc<()>,
    state: Mutex<State>,
}
impl<P: Port> Custody<P> {
    pub fn new(port: Arc<P>) -> Arc<Self> {
        Arc::new(Self {
            port,
            owner: Arc::new(()),
            state: Mutex::new(State {
                kept: HashMap::new(),
                outage: false,
                outage_error: Failure::Unreachable,
                timer: None,
                probing: false,
                last_probe: f64::NEG_INFINITY,
                probe_delay: 2000,
                closed: false,
                discovered_view: None,
            }),
        })
    }
    fn check(&self, signal: &CancellationToken) -> Result<(), Failure> {
        if self.state.lock().unwrap().closed {
            return Err(consumer("closed", "Claustrum scoped custody is closed"));
        }
        if signal.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        Ok(())
    }
    async fn token(&self, signal: &CancellationToken) -> Result<String, Failure> {
        self.check(signal)?;
        let value = self.port.token().await?;
        self.check(signal)?;
        if !super::enrollment::valid_token(&value["token"])
            || !client::safe_nonnegative(&value["token_generation"])
            || value["token_generation"].as_f64().unwrap() < 1.0
        {
            return Err(consumer(
                "invalid-token",
                "Invalid Claustrum enrollment token",
            ));
        }
        Ok(crate::json::string(&value["token"]))
    }
    async fn call(
        self: &Arc<Self>,
        operation: Operation,
        input: Value,
        signal: &CancellationToken,
    ) -> Result<Value, Failure> {
        self.check(signal)?;
        // A cancelled caller stops waiting; the original operation still owns its transport.
        let port = self.port.clone();
        let pending =
            super::promise::spawn_started(async move { port.call(operation, input).await });
        let result = tokio::select! {
            biased;
            _ = signal.cancelled() => Err(Failure::Cancelled),
            result = pending => result.unwrap_or(Err(Failure::Other)),
        };
        self.check(signal)?;
        match result {
            Ok(value) => {
                self.reachable();
                Ok(value)
            }
            Err(error) => {
                if error.unreachable() {
                    self.transport_lost(Some(error.clone()));
                } else {
                    self.reachable();
                }
                Err(match error {
                    Failure::Wire(_) => error,
                    error if error.unreachable() => Failure::Unreachable,
                    _ => consumer("unavailable", "Claustrum scoped operation unavailable"),
                })
            }
        }
    }
    pub fn transport_lost(self: &Arc<Self>, error: Option<Failure>) {
        {
            let mut state = self.state.lock().unwrap();
            if state.closed || state.outage {
                return;
            }
            state.outage = true;
            state.outage_error = match error {
                Some(error @ Failure::Wire(_)) => error,
                _ => Failure::Unreachable,
            };
        }
        self.port.log(
            "warn",
            "serving kept vault receipt",
            json!({"reason":"unreachable"}),
        );
        self.schedule_probe();
    }
    pub fn reachable(&self) {
        {
            let mut state = self.state.lock().unwrap();
            if state.closed || !state.outage {
                return;
            }
            state.outage = false;
            if let Some(timer) = state.timer.take() {
                timer.cancel();
            }
            state.probe_delay = 2000;
            for kept in state.kept.values() {
                *kept.since.lock().unwrap() = None;
            }
        }
        self.port.log(
            "info",
            "vault receipt serving resumed",
            json!({"reason":"reachable"}),
        );
    }
    fn schedule_probe(self: &Arc<Self>) {
        let (cancel, delay) = {
            let mut state = self.state.lock().unwrap();
            if state.closed || !state.outage || state.timer.is_some() || state.probing {
                return;
            }
            let cancel = Arc::new(CancellationToken::new());
            state.timer = Some(cancel.clone());
            (cancel, self.port.delay(state.probe_delay))
        };
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            tokio::select! { biased; _ = cancel.cancelled() => return, _ = delay => {} }
            if let Some(custody) = weak.upgrade() {
                {
                    let mut state = custody.state.lock().unwrap();
                    if !state
                        .timer
                        .as_ref()
                        .is_some_and(|timer| Arc::ptr_eq(timer, &cancel))
                    {
                        return;
                    }
                    state.timer = None;
                }
                custody.probe(true);
            }
        });
    }
    fn probe(self: &Arc<Self>, idle: bool) {
        {
            let mut state = self.state.lock().unwrap();
            if state.closed || !state.outage || state.probing {
                return;
            }
            let now = self.port.now_ms();
            if now - state.last_probe < 1000.0 {
                drop(state);
                self.schedule_probe();
                return;
            }
            state.last_probe = now;
            state.probing = true;
        }
        let custody = self.clone();
        tokio::spawn(async move {
            let result = custody.list(&CancellationToken::new()).await;
            {
                let mut state = custody.state.lock().unwrap();
                if result.is_err() && idle && state.outage {
                    state.probe_delay = (state.probe_delay * 2).min(30_000);
                }
                state.probing = false;
            }
            custody.schedule_probe();
        });
    }
    pub fn list_failed(self: &Arc<Self>, error: &Failure) {
        if error.unreachable() {
            self.transport_lost(Some(error.clone()));
        } else {
            self.reachable();
            self.state.lock().unwrap().kept.clear();
        }
    }
    async fn list(self: &Arc<Self>, signal: &CancellationToken) -> Result<Value, Failure> {
        let token = self.token(signal).await.map_err(|error| {
            self.state.lock().unwrap().kept.clear();
            error
        })?;
        let listed = self
            .call(Operation::List, json!({"enrollmentToken":token}), signal)
            .await
            .map_err(|error| {
                self.list_failed(&error);
                error
            })?;
        let inventory = policy::inventory(&listed);
        for skipped in inventory["skipped"].as_array().unwrap() {
            self.port
                .log("warn", "skipped malformed vault record", skipped.clone());
        }
        Ok(inventory)
    }
    pub async fn discover(self: &Arc<Self>, signal: &CancellationToken) -> Result<Value, Failure> {
        let inventory = self.list(signal).await?;
        self.state.lock().unwrap().discovered_view = Some(inventory["view"].clone());
        Ok(inventory)
    }
    pub fn discovered_view(&self) -> Option<Value> {
        self.state.lock().unwrap().discovered_view.clone()
    }
    pub fn forget(&self, id: &str) {
        self.state.lock().unwrap().kept.remove(id);
    }
    pub fn retain(&self, identities: &[Value]) {
        let current: HashMap<_, _> = identities.iter().map(|v| (&v["credentialId"], v)).collect();
        self.state.lock().unwrap().kept.retain(|_, kept| {
            let attempt = kept.attempt.metadata();
            current
                .get(&attempt["credentialId"])
                .is_some_and(|identity| {
                    crate::json::truthy(&identity["enabled"])
                        && identity["state"] == "active"
                        && identity["credentialType"] == attempt["credentialType"]
                        && identity.get("accountIdentity") == attempt.get("expectedAccountIdentity")
                })
        });
    }
    pub fn kept_receipt_state(&self) -> Value {
        let mut since: Option<f64> = None;
        self.state.lock().unwrap().kept.retain(|_, kept| {
            if !kept.attempt.authorized.usable_kept(self.port.now_ms()) {
                return false;
            }
            if let Some(start) = *kept.since.lock().unwrap() {
                since = Some(since.map_or(start, |value| value.min(start)));
            }
            true
        });
        json!({"active":since.is_some(),"since":since.map(crate::json::finite_number)})
    }
    fn serve_kept(
        &self,
        id: &str,
        kept: Option<Arc<Kept>>,
        token: String,
        route: &str,
        error: Option<Failure>,
    ) -> Result<Arc<Receipt>, Failure> {
        let state = self.state.lock().unwrap();
        if let Some(kept) = kept.filter(|kept| {
            state
                .kept
                .get(id)
                .is_some_and(|current| Arc::ptr_eq(current, kept))
                && kept.attempt.authorized.usable_kept(self.port.now_ms())
        }) {
            kept.since
                .lock()
                .unwrap()
                .get_or_insert_with(|| self.port.now_ms());
            let mut authorized = kept.attempt.authorized.clone();
            authorized.metadata["fromKeptReceipt"] = true.into();
            return Ok(Arc::new(Receipt {
                authorized,
                owner: Arc::downgrade(&self.owner),
                enrollment_token: token,
                kept_route: Some(route.into()),
                report: Mutex::new(None),
            }));
        }
        let error = error.unwrap_or_else(|| state.outage_error.clone());
        drop(state);
        self.forget(id);
        Err(error)
    }
    pub async fn authorize(
        self: &Arc<Self>,
        identity: &Value,
        signal: &CancellationToken,
        route: &str,
    ) -> Result<Arc<Receipt>, Failure> {
        self.check(signal)?;
        let id = identity["credentialId"].as_str().unwrap_or_default();
        let mut kept = self.state.lock().unwrap().kept.get(id).cloned();
        if kept.as_ref().is_some_and(|kept| {
            kept.attempt.metadata()["credentialType"] != identity["credentialType"]
                || kept.attempt.metadata().get("expectedAccountIdentity")
                    != identity.get("accountIdentity")
        }) {
            self.forget(id);
            kept = None;
        }
        let token = self.token(signal).await.map_err(|error| {
            self.state.lock().unwrap().kept.clear();
            error
        })?;
        let outage = self.state.lock().unwrap().outage;
        if outage && kept.is_some() {
            self.probe(false);
            return self.serve_kept(id, kept, token, route, None);
        }
        let fresh = async {
            if id.is_empty() {
                return Err(consumer(
                    "route-unavailable",
                    "Claustrum dispatch requires a credential id",
                ));
            }
            if identity.get("accountIdentity").is_none() {
                return Err(consumer(
                    "identity-unasserted",
                    "Claustrum dispatch requires a known account identity",
                ));
            }
            let mut input = json!({"credentialId":id,"enrollmentToken":token});
            if identity["credentialType"] == "oauth" {
                input["minTtlMs"] = 300_000.into();
            }
            let served = self.call(Operation::Get, input, signal).await?;
            let authorized = policy::validate_receipt(identity, &served, self.port.now_ms())
                .map_err(Failure::Consumer)?;
            Ok(Arc::new(Receipt {
                authorized,
                owner: Arc::downgrade(&self.owner),
                enrollment_token: token.clone(),
                kept_route: None,
                report: Mutex::new(None),
            }))
        }
        .await;
        match fresh {
            Ok(attempt) => {
                self.state.lock().unwrap().kept.insert(
                    id.into(),
                    Arc::new(Kept {
                        attempt: attempt.clone(),
                        since: Mutex::new(None),
                    }),
                );
                Ok(attempt)
            }
            Err(error) => {
                self.check(signal)?;
                if !error.unreachable() {
                    self.forget(id);
                    return Err(error);
                }
                self.serve_kept(id, kept, token, route, Some(error))
            }
        }
    }
    pub async fn report_failure(
        self: &Arc<Self>,
        attempt: &Arc<Receipt>,
        status: u16,
        reporter: &str,
    ) -> Result<(), Failure> {
        if status != 401 {
            return Ok(());
        }
        self.check(&CancellationToken::new())?;
        if !Weak::ptr_eq(&attempt.owner, &Arc::downgrade(&self.owner)) {
            return Err(consumer(
                "no-receipt",
                "Claustrum failure report requires an original dispatch receipt",
            ));
        }
        let metadata = attempt.metadata();
        let id = metadata["credentialId"].as_str().unwrap();
        if let Some(route) = &attempt.kept_route {
            self.forget(id);
            self.port.log(
                "debug",
                "scoped 401 not reported",
                json!({"routeId":route,"reason":"kept-receipt"}),
            );
            return Ok(());
        }
        {
            let mut state = self.state.lock().unwrap();
            if state.kept.get(id).is_some_and(|kept| {
                kept.attempt.metadata()["recordVersion"] == metadata["recordVersion"]
            }) {
                state.kept.remove(id);
            }
        }
        let report = {
            let mut report = attempt.report.lock().unwrap();
            if let Some(pending) = report.as_ref() {
                pending.clone()
            } else {
                let custody = self.clone();
                let attempt = attempt.clone();
                let reporter = reporter.to_owned();
                let pending = super::promise::spawn_started(async move {
                    let metadata = attempt.metadata();
                    let result = custody.call(Operation::Report, json!({"credentialId":metadata["credentialId"],"enrollmentToken":attempt.enrollment_token,"providerStatus":401,"recordVersion":metadata["recordVersion"],"reporterSource":reporter}), &CancellationToken::new()).await;
                    // Completion follows initiation, including immediately settled client calls.
                    tokio::task::yield_now().await;
                    if result.is_ok() {
                        custody.port.log("debug", "scoped 401 reported", json!({"credentialId":metadata["credentialId"],"recordVersion":metadata["recordVersion"],"reporterSource":reporter}));
                    } else {
                        attempt.report.lock().unwrap().take();
                    }
                    result.map(|_| ())
                });
                let pending = async move { pending.await.unwrap_or(Err(Failure::Other)) }
                    .boxed()
                    .shared();
                *report = Some(pending.clone());
                pending
            }
        };
        report.await
    }
    pub async fn close(&self) {
        {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return;
            }
            state.closed = true;
            if let Some(timer) = state.timer.take() {
                timer.cancel();
            }
            state.kept.clear();
        }
        self.port.close().await;
    }
}

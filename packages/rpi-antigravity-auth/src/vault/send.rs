//! Antigravity's receipt-bound send policy, above the shared scoped consumer.
use super::{
    client::safe_nonnegative,
    consumer::{self, Consumer},
    custody::{Failure, Receipt},
    policy, roster,
    roster_store::RosterStore,
};
use crate::json::trim;
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Weak,
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum Error {
    Source {
        kind: &'static str,
        message: &'static str,
    },
    Custody(Failure),
    Cancelled,
    Dispatch(String),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source { message, .. } => f.write_str(message),
            Self::Custody(error) => error.fmt(f),
            Self::Cancelled => f.write_str("This operation was aborted"),
            Self::Dispatch(message) => f.write_str(message),
        }
    }
}
impl std::error::Error for Error {}
impl From<Failure> for Error {
    fn from(value: Failure) -> Self {
        if matches!(value, Failure::Cancelled) {
            Self::Cancelled
        } else {
            Self::Custody(value)
        }
    }
}
fn error(kind: &'static str, message: &'static str) -> Error {
    Error::Source { kind, message }
}
fn nonempty(value: &Value) -> bool {
    value.as_str().is_some_and(|v| !trim(v).is_empty())
}
fn serviceable(row: &Value, roster: &Value) -> bool {
    crate::json::truthy(&row["enabled"])
        && row["state"] == "active"
        && row["credentialType"] == "oauth"
        && row["stale"] != true
        && row["unclaimed"] != true
        && nonempty(&row["accountIdentity"])
        && !roster::is_declined(
            roster["declined"].as_array().unwrap(),
            row["credentialId"].as_str().unwrap(),
            row.get("accountIdentity"),
        )
}
pub fn routes(roster: Option<&Value>) -> Vec<Value> {
    roster.map(|roster|roster["rows"].as_array().unwrap().iter().filter(|row|serviceable(row,roster)).map(|row|{
        let mut reference=json!({"routeId":row["routeId"],"credentialId":row["credentialId"],"accountIdentity":row["accountIdentity"],"label":row["label"]});if let Some(email)=row.get("email"){reference["email"]=email.clone();}reference
    }).collect()).unwrap_or_default()
}
/// The bearer is separate from JSON metadata, as in the original non-enumerable property.
pub fn admission_metadata(
    reference: &Value,
    receipt: &Value,
    access_token: &str,
) -> Result<Value, Error> {
    if receipt["credentialType"] != "oauth" {
        return Err(error(
            "api-key-refused",
            "Antigravity vault routes serve OAuth credentials only",
        ));
    }
    if receipt["accountIdentitySource"] != "asserted"
        || receipt.get("assertedCredentialId").is_none()
        || receipt.get("assertedAccountIdentity").is_none()
    {
        return Err(error(
            "identity-unasserted",
            "The vault did not assert the credential and account for this send",
        ));
    }
    if receipt["credentialId"] != reference["credentialId"]
        || receipt["assertedCredentialId"] != reference["credentialId"]
        || receipt["assertedAccountIdentity"] != reference["accountIdentity"]
        || receipt["accountIdentity"] != reference["accountIdentity"]
        || receipt["expectedAccountIdentity"] != reference["accountIdentity"]
    {
        return Err(error(
            "identity-contradicted",
            "The vault served a different credential or account than the selected route",
        ));
    }
    if !nonempty(&receipt["projectId"])
        || receipt["projectId"]
            .as_str()
            .unwrap()
            .chars()
            .any(|c| trim(c.encode_utf8(&mut [0; 4])).is_empty())
    {
        return Err(error(
            "project-missing",
            "The vault served no Antigravity project for this send",
        ));
    }
    if trim(access_token).is_empty() || !safe_nonnegative(&receipt["recordVersion"]) {
        return Err(error(
            "invalid-receipt",
            "The vault receipt carries no usable token or record version",
        ));
    }
    let mut value = json!({"routeId":reference["routeId"],"credentialId":receipt["credentialId"],"accountIdentity":receipt["assertedAccountIdentity"],"recordVersion":receipt["recordVersion"],"projectId":receipt["projectId"]});
    for key in ["expiresAtMs", "fromKeptReceipt"] {
        if let Some(field) = receipt.get(key) {
            value[key] = field.clone();
        }
    }
    Ok(value)
}
pub trait Port: consumer::Port {
    fn log(&self, message: &'static str, fields: Value);
    fn on_error(&self, error: &Error);
}
pub struct Admission {
    metadata: Value,
    receipt: Arc<Receipt>,
    owner: Weak<()>,
    reported: AtomicBool,
}
impl Admission {
    pub fn metadata(&self) -> &Value {
        &self.metadata
    }
    pub fn access_token(&self) -> &str {
        self.receipt.access_token()
    }
}
pub trait Dispatch {
    type Response: Send;
    fn status(response: &Self::Response) -> u16;
    fn send(
        &mut self,
        admission: &Admission,
        signal: CancellationToken,
    ) -> impl Future<Output = Result<Self::Response, Error>> + Send;
    fn cancel(
        &mut self,
        response: Self::Response,
    ) -> impl Future<Output = Result<(), Error>> + Send;
}
pub struct Source<P: Port> {
    consumer: Arc<Consumer<P>>,
    port: Arc<P>,
    closed: AtomicBool,
    owner: Arc<()>,
}
async fn until<T>(
    signal: &CancellationToken,
    work: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    // Starting the operation precedes waiting for abort in the original Promise call.
    tokio::select! {biased;result=work=>if signal.is_cancelled(){Err(Error::Cancelled)}else{result},_=signal.cancelled()=>Err(Error::Cancelled)}
}
impl<P: Port> Source<P> {
    pub fn new(port: Arc<P>, store: RosterStore) -> Arc<Self> {
        Arc::new(Self {
            consumer: Consumer::new(port.clone(), store),
            port,
            closed: AtomicBool::new(false),
            owner: Arc::new(()),
        })
    }
    fn check(&self, signal: Option<&CancellationToken>) -> Result<(), Error> {
        if self.closed.load(Ordering::Acquire) {
            Err(error("closed", "Antigravity vault source is closed"))
        } else if signal.is_some_and(CancellationToken::is_cancelled) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn routes(&self) -> Vec<Value> {
        if self.closed.load(Ordering::Acquire) {
            vec![]
        } else {
            routes(self.consumer.snapshot().as_ref())
        }
    }
    pub fn grant_reach(&self) -> Option<Value> {
        if self.closed.load(Ordering::Acquire) {
            None
        } else {
            self.consumer.grant_reach()
        }
    }
    pub fn empty_roster_cause(&self) -> &'static str {
        if self.closed.load(Ordering::Acquire)
            || !self
                .consumer
                .snapshot()
                .is_some_and(|v| v["rows"].as_array().unwrap().is_empty())
        {
            return "unknown";
        }
        match self.consumer.grant_reach() {
            None => "unknown",
            Some(reach) if reach["grants"].as_f64() == Some(0.0) => "no-grant",
            Some(_) => "grant-reaches-nothing",
        }
    }
    pub async fn refresh(self: &Arc<Self>) -> Result<Option<Value>, Error> {
        self.check(None)?;
        self.consumer.refresh().await.map_err(Error::from)
    }
    async fn serving(&self, signal: &CancellationToken) -> Result<(), Error> {
        self.check(Some(signal))?;
        if !until(signal, async {
            self.port.active().await.map_err(Error::from)
        })
        .await?
        {
            return Err(error(
                "not-active",
                "Antigravity vault custody is not active for this host",
            ));
        }
        self.check(Some(signal))
    }
    fn current(&self, reference: &Value) -> Result<(), Error> {
        if !self.consumer.snapshot().as_ref().is_some_and(|roster| {
            roster["rows"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["routeId"] == reference["routeId"])
                .is_some_and(|row| {
                    row["credentialId"] == reference["credentialId"]
                        && row["accountIdentity"] == reference["accountIdentity"]
                        && serviceable(row, roster)
                })
        }) {
            return Err(error(
                "route-unavailable",
                "The selected vault route is declined, removed or changed",
            ));
        }
        Ok(())
    }
    pub async fn admit(
        self: &Arc<Self>,
        reference: &Value,
        signal: &CancellationToken,
    ) -> Result<Arc<Admission>, Error> {
        self.serving(signal).await?;
        self.current(reference)?;
        let pending = self.consumer.authorize(
            reference["routeId"].as_str().unwrap().into(),
            signal.clone(),
        );
        let receipt = until(signal, async { pending.await.map_err(Error::from) }).await?;
        self.check(Some(signal))?;
        let metadata = admission_metadata(reference, receipt.metadata(), receipt.access_token())?;
        Ok(Arc::new(Admission {
            metadata,
            receipt,
            owner: Arc::downgrade(&self.owner),
            reported: AtomicBool::new(false),
        }))
    }
    fn issued(&self, admission: &Admission) -> Result<(), Error> {
        if Weak::ptr_eq(&admission.owner, &Arc::downgrade(&self.owner)) {
            Ok(())
        } else {
            Err(error(
                "not-issued",
                "This admission was not issued by this vault source",
            ))
        }
    }
    pub async fn report_status(
        self: &Arc<Self>,
        admission: Arc<Admission>,
        status: u16,
        reporter: &str,
    ) -> Result<bool, Error> {
        self.issued(&admission)?;
        if status != 401 || admission.reported.load(Ordering::Acquire) {
            return Ok(false);
        }
        self.check(None)?;
        if admission.reported.swap(true, Ordering::AcqRel) {
            return Ok(false);
        }
        let consumer = self.consumer.clone();
        let receipt = admission.receipt.clone();
        let reporter = reporter.to_owned();
        let pending = super::promise::spawn_started(async move {
            consumer.report_failure(receipt, 401, &reporter).await
        });
        pending.await.unwrap_or(Err(Failure::Other))?;
        Ok(admission.receipt.metadata()["fromKeptReceipt"] != true)
    }
    pub fn retry_after_401(
        &self,
        served: &Admission,
        current: Option<&Admission>,
        site: &str,
    ) -> Result<bool, Error> {
        self.issued(served)?;
        if let Some(current) = current {
            self.issued(current)?;
        }
        let served = served.receipt.metadata();
        let current = current.map(|v| v.receipt.metadata());
        let retry = policy::is_rotation(served, current);
        let reason = match current {
            None => "reauthorize-failed",
            Some(value) if value["credentialId"] != served["credentialId"] => "credential-changed",
            Some(value) if value["accountIdentity"] != served["accountIdentity"] => {
                "account-changed"
            }
            Some(value) if value["recordVersion"] == served["recordVersion"] => "version-unchanged",
            Some(_) => "rotated",
        };
        self.port.log("scoped 401 re-authorized",json!({"site":site,"credentialId":served["credentialId"],"servedVersion":served["recordVersion"],"currentVersion":current.and_then(|v|v.get("recordVersion")),"retry":retry,"reason":reason}));
        Ok(retry)
    }
    pub async fn send<D: Dispatch>(
        self: &Arc<Self>,
        reference: &Value,
        dispatch: &mut D,
        site: &str,
        signal: &CancellationToken,
        reporter: &str,
    ) -> Result<D::Response, Error> {
        let mut admission = self.admit(reference, signal).await?;
        let mut response = dispatch.send(&admission, signal.clone()).await?;
        if D::status(&response) == 401 && admission.metadata["fromKeptReceipt"] == true {
            self.report_status(admission, 401, reporter).await?;
            return Ok(response);
        }
        if D::status(&response) == 401 && !signal.is_cancelled() {
            let current = self.admit(reference, signal).await.ok();
            if let Some(current) = current {
                if self.retry_after_401(&admission, Some(&current), site)? {
                    let _ = until(signal, dispatch.cancel(response)).await;
                    self.check(Some(signal))?;
                    admission = current;
                    response = dispatch.send(&admission, signal.clone()).await?;
                }
            }
        }
        if D::status(&response) == 401 {
            if let Err(error) = until(signal, self.report_status(admission, 401, reporter)).await {
                self.port.on_error(&error);
            }
        }
        Ok(response)
    }
    pub async fn change_route(
        self: &Arc<Self>,
        reference: &Value,
        accept: bool,
    ) -> Result<(), Error> {
        self.check(None)?;
        self.consumer
            .change_route(reference["routeId"].as_str().unwrap().into(), accept)
            .await
            .map_err(Error::from)
    }
    pub async fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.consumer.close().await;
    }
}

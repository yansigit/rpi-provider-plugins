//! The pinned account-attempt state machine, independent of the SDK stream adapter.
use crate::services::Service;
use crate::types::{Account, AcquireOptions, AttemptFailure, LeaseOutcome};
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Full,
    Simple,
}
#[derive(Clone)]
pub struct Request {
    pub provider_id: String,
    pub model_id: String,
    pub affinity_key: Option<String>,
    pub exclude_account_ids: Vec<String>,
    pub max_account_attempts: f64,
    pub disable_provider_retries: bool,
    pub kind: StreamKind,
    pub signal: CancellationToken,
}
#[derive(Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Value,
}
pub struct Failover<'a> {
    pub provider_id: &'a str,
    pub from_account_id: &'a str,
    pub failure: &'a AttemptFailure,
    pub errors_on_account: usize,
}

#[async_trait::async_trait]
pub trait Host: Send + Sync {
    type Auth: Send + Sync;
    type Stream: Stream<Item = Result<Value, String>> + Send + Unpin;
    async fn resolve(&self, account: &Account, request: &Request) -> Result<Self::Auth, String>;
    /// Each physical stream resets its private response receipt before sending.
    async fn start(&self, auth: &Self::Auth, request: &Request) -> Result<Self::Stream, String>;
    fn response(&self) -> Option<Response>;
    async fn emit(&self, event: Value) -> bool;
    async fn pause(&self) {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    async fn failover(&self, _: Failover<'_>) -> Result<bool, String> {
        Ok(false)
    }
}

fn failure(
    error: Option<String>,
    response: Option<Response>,
    output_started: bool,
    message: Option<&Value>,
) -> AttemptFailure {
    AttemptFailure {
        message: message
            .and_then(|message| message["errorMessage"].as_str())
            .map(str::to_owned)
            .or_else(|| error.clone())
            .unwrap_or_else(|| "undefined".into()),
        status: response.as_ref().map(|response| response.status),
        headers: response.map(|response| response.headers),
        assistant_message: message.cloned(),
        cause: error.map(|message| serde_json::json!({"name":"Error","message":message})),
        output_started,
    }
}
struct Terminal {
    start: Option<Value>,
    event: Value,
}
async fn replay(host: &impl Host, terminal: Terminal) {
    if let Some(start) = terminal.start {
        if !host.emit(start).await {
            return;
        }
    }
    host.emit(terminal.event).await;
}
async fn hand_off(service: &Service, request: &Request, excluded: &[String]) {
    let Some(key) = &request.affinity_key else {
        return;
    };
    if !service
        .pool_preference(&request.provider_id)
        .is_ok_and(|pool| pool.affinity)
        || service
            .affinity(&request.provider_id, key)
            .ok()
            .flatten()
            .is_some_and(|pin| pin.explicit)
    {
        return;
    }
    if let Ok(mut lease) = service
        .acquire(AcquireOptions {
            provider_id: request.provider_id.clone(),
            affinity_key: Some(key.clone()),
            exclude_account_ids: excluded.into(),
            ..Default::default()
        })
        .await
    {
        lease.release(LeaseOutcome::Cancelled);
    }
}
enum Flow {
    End,
    Next,
    Surface,
}

pub async fn run(service: &Service, host: &impl Host, request: &Request) -> Result<(), String> {
    let mut attempted = request.exclude_account_ids.clone();
    let errors_before_switch = service.errors_before_switch();
    let mut attempts = 0usize;
    let mut last_rejected = None;
    let mut last_setup_error = None;
    while (attempts as f64) < request.max_account_attempts {
        let mut lease = match service
            .acquire(AcquireOptions {
                provider_id: request.provider_id.clone(),
                model_id: Some(request.model_id.clone()),
                affinity_key: request.affinity_key.clone(),
                exclude_account_ids: attempted.clone(),
            })
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Some(terminal) = last_rejected {
                    replay(host, terminal).await;
                    return Ok(());
                }
                return Err(last_setup_error.unwrap_or_else(|| error.to_string()));
            }
        };
        attempts += 1;
        attempted.push(lease.account.id.clone());
        let mut settled = false;
        let mut output_started = false;
        let mut start = None;
        let outcome: Result<Flow, String> = async {
            let auth = match host.resolve(&lease.account, request).await {
                Ok(auth) => auth,
                Err(error) => {
                    last_setup_error = Some(error.clone());
                    let disposition = lease.release(LeaseOutcome::Failure {
                        error: failure(Some(error.clone()), None, false, None),
                    });
                    settled = true;
                    if disposition.is_some_and(|value| value.retryable)
                        && (attempts as f64) < request.max_account_attempts
                        && !request.signal.is_cancelled()
                    {
                        return Ok(Flow::Next);
                    }
                    return Err(error);
                }
            };
            let mut same_account_errors = 0usize;
            loop {
                start = None;
                let mut inner = host.start(&auth, request).await?;
                let mut retry_same = false;
                while let Some(event) = inner.next().await {
                    let event = event?;
                    match event["type"].as_str() {
                        Some("start") => {
                            start = Some(event);
                            continue;
                        }
                        Some("error") => {
                            if event["reason"] == "aborted" || request.signal.is_cancelled() {
                                lease.release(LeaseOutcome::Cancelled);
                                settled = true;
                            } else {
                                let failure = failure(
                                    None,
                                    host.response(),
                                    output_started,
                                    event.get("error"),
                                );
                                // The pinned implementation retries even fatal pre-output errors here.
                                if !output_started
                                    && ((same_account_errors + 1) as f64) < errors_before_switch
                                {
                                    same_account_errors += 1;
                                    host.pause().await;
                                    if request.signal.is_cancelled() {
                                        lease.release(LeaseOutcome::Cancelled);
                                        settled = true;
                                        return Ok(Flow::End);
                                    }
                                    retry_same = true;
                                    break;
                                }
                                let disposition = lease.release(LeaseOutcome::Failure {
                                    error: failure.clone(),
                                });
                                settled = true;
                                if !output_started
                                    && disposition.is_some_and(|value| value.retryable)
                                    && (attempts as f64) < request.max_account_attempts
                                {
                                    last_rejected = Some(Terminal {
                                        start: start.take(),
                                        event,
                                    });
                                    return Ok(
                                        if host
                                            .failover(Failover {
                                                provider_id: &request.provider_id,
                                                from_account_id: &lease.account.id,
                                                failure: &failure,
                                                errors_on_account: same_account_errors + 1,
                                            })
                                            .await?
                                        {
                                            Flow::Surface
                                        } else {
                                            Flow::Next
                                        },
                                    );
                                }
                            }
                            if !output_started {
                                if let Some(start) = start.take() {
                                    if !host.emit(start).await {
                                        return Ok(Flow::End);
                                    }
                                }
                            }
                            host.emit(event).await;
                            return Ok(Flow::End);
                        }
                        Some("done") => {
                            lease.release(LeaseOutcome::Success);
                            settled = true;
                            if !output_started {
                                if let Some(start) = start.take() {
                                    if !host.emit(start).await {
                                        return Ok(Flow::End);
                                    }
                                }
                            }
                            host.emit(event).await;
                            return Ok(Flow::End);
                        }
                        _ => {
                            if !output_started {
                                output_started = true;
                                if let Some(start) = start.take() {
                                    if !host.emit(start).await {
                                        return Ok(Flow::End);
                                    }
                                }
                            }
                            if !host.emit(event).await {
                                return Ok(Flow::End);
                            }
                        }
                    }
                }
                if !retry_same {
                    break;
                }
            }
            let error = "Provider stream ended without a terminal event".to_string();
            last_setup_error = Some(error.clone());
            let disposition = lease.release(LeaseOutcome::Failure {
                error: failure(Some(error.clone()), host.response(), output_started, None),
            });
            settled = true;
            if !output_started
                && disposition.is_some_and(|value| value.retryable)
                && (attempts as f64) < request.max_account_attempts
            {
                return Ok(Flow::Next);
            }
            Err(error)
        }
        .await;
        match outcome {
            Ok(Flow::End) => return Ok(()),
            Ok(Flow::Next) => continue,
            Ok(Flow::Surface) => {
                hand_off(service, request, &attempted).await;
                break;
            }
            Err(error) => {
                if !settled {
                    let outcome = if request.signal.is_cancelled() {
                        LeaseOutcome::Cancelled
                    } else {
                        LeaseOutcome::Failure {
                            error: failure(
                                Some(error.clone()),
                                host.response(),
                                output_started,
                                None,
                            ),
                        }
                    };
                    let disposition = lease.release(outcome);
                    if !output_started
                        && disposition.is_some_and(|value| value.retryable)
                        && (attempts as f64) < request.max_account_attempts
                    {
                        last_setup_error = Some(error);
                        continue;
                    }
                }
                return Err(error);
            }
        }
    }
    if let Some(terminal) = last_rejected {
        replay(host, terminal).await;
        return Ok(());
    }
    Err(last_setup_error.unwrap_or_else(|| {
        format!(
            "multiprovider: exhausted account attempts for \"{}\"",
            request.provider_id
        )
    }))
}

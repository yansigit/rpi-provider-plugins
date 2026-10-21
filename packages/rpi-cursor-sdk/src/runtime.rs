//! One retained adapter process and its standing host connection.
use crate::{
    native_host::Router,
    worker::{Call, HostHandler, Worker, WorkerConfig},
};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{Notify, OnceCell};
use tokio_util::sync::CancellationToken;

pub struct Session {
    pub manifest: Value,
    worker: Worker,
    standing: Mutex<Option<Call>>,
    router: Arc<Router>,
    configured: OnceCell<()>,
}

/// Admission and draining for callbacks that can reenter the same worker.
pub struct Owner {
    state: Mutex<OwnedState>,
    idle: Notify,
    closed: OnceCell<Result<(), String>>,
    signal: CancellationToken,
}
struct OwnedState {
    session: Option<Arc<Session>>,
    active: usize,
}
pub struct Invocation {
    session: Option<Arc<Session>>,
    pub signal: CancellationToken,
    owner: Arc<Owner>,
}
impl Invocation {
    pub fn session(&self) -> &Session {
        self.session.as_deref().unwrap()
    }
}
impl Drop for Invocation {
    fn drop(&mut self) {
        self.signal.cancel();
        self.session.take();
        let mut state = self.owner.state.lock().unwrap();
        state.active -= 1;
        if state.active == 0 {
            self.owner.idle.notify_waiters();
        }
    }
}
impl Owner {
    pub fn new(session: Session, signal: CancellationToken) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(OwnedState {
                session: Some(Arc::new(session)),
                active: 0,
            }),
            idle: Notify::new(),
            closed: OnceCell::new(),
            signal,
        })
    }
    pub fn enter(self: &Arc<Self>) -> Result<Invocation, String> {
        let mut state = self.state.lock().unwrap();
        if self.signal.is_cancelled() {
            return Err("Cursor runtime is shutting down".into());
        }
        let session = state.session.clone().ok_or("Cursor runtime is closed")?;
        state.active += 1;
        Ok(Invocation {
            session: Some(session),
            signal: self.signal.child_token(),
            owner: self.clone(),
        })
    }
    /// Await to completion: the retained callback/watcher owns cleanup through reaping.
    pub async fn close(&self) -> Result<(), String> {
        self.closed
            .get_or_init(|| async {
                self.signal.cancel();
                let session = self.state.lock().unwrap().session.take();
                loop {
                    let idle = self.idle.notified();
                    tokio::pin!(idle);
                    idle.as_mut().enable();
                    if self.state.lock().unwrap().active == 0 {
                        break;
                    }
                    idle.await;
                }
                match session {
                    Some(session) => {
                        Arc::try_unwrap(session)
                            .map_err(|_| "Cursor session retained after callback drain")?
                            .close()
                            .await
                    }
                    None => Ok(()),
                }
            })
            .await
            .clone()
    }
}
impl Session {
    pub async fn open(
        config: WorkerConfig,
        locations: Value,
        router: Arc<Router>,
    ) -> Result<Self, String> {
        let worker = Worker::spawn(config, router.standing()).await?;
        let mut standing = Some(worker.call("runtime.open", locations).await?);
        let ready =
            match tokio::time::timeout(Duration::from_secs(30), standing.as_mut().unwrap().next())
                .await
            {
                Ok(Some(event)) if event["type"] == "ready" && event["manifest"].is_object() => {
                    Ok(event["manifest"].clone())
                }
                Ok(None) => Err(standing
                    .take()
                    .unwrap()
                    .result()
                    .await
                    .err()
                    .unwrap_or_else(|| "Cursor worker opened without a manifest".into())),
                _ => Err("Cursor worker initialization failed or timed out".into()),
            };
        match ready {
            Ok(manifest) => Ok(Self {
                manifest,
                worker,
                standing: Mutex::new(standing),
                router,
                configured: OnceCell::new(),
            }),
            Err(error) => {
                let _ = router.close().await;
                let _ = worker.shutdown().await;
                Err(error)
            }
        }
    }
    pub async fn call(
        &self,
        method: &str,
        input: Value,
        host: HostHandler,
        signal: &CancellationToken,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Call, String> {
        if cancelled() || signal.is_cancelled() {
            return Err("Cursor operation cancelled".into());
        }
        self.configured
            .get_or_try_init(|| async {
                let call = self
                    .worker
                    .call_with_host("runtime.configure", json!({}), host.clone())
                    .await?;
                drive(call, signal, cancelled, |_| Ok(())).await.map(|_| ())
            })
            .await?;
        if cancelled() || signal.is_cancelled() {
            return Err("Cursor operation cancelled".into());
        }
        self.worker.call_with_host(method, input, host).await
    }
    pub async fn close(self) -> Result<(), String> {
        let standing = self.standing.into_inner().unwrap();
        let mut failure = None;
        if let Some(call) = standing {
            if let Err(error) = call.cancel().await {
                failure = Some(error);
            }
            let completion = tokio::time::timeout(Duration::from_secs(30), call.result()).await;
            match completion {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                }
                Err(_) => {
                    failure.get_or_insert("Cursor worker shutdown timed out; inspect recovery records before further mutations".into());
                }
            }
        }
        if let Err(error) = self.router.close().await {
            failure.get_or_insert(error);
        }
        if let Err(error) = self.worker.shutdown().await {
            failure.get_or_insert(error);
        }
        failure.map_or(Ok(()), Err)
    }
}

/// Run on the blocking foreign callback thread. A slow synchronous emit never
/// occupies the async worker thread that processes IPC replies and cancellation.
pub async fn drive(
    mut call: Call,
    signal: &CancellationToken,
    cancelled: &impl Fn() -> bool,
    mut emit: impl FnMut(Value) -> Result<(), String>,
) -> Result<Value, String> {
    let mut sent_cancel = false;
    let mut failure = None;
    loop {
        if !sent_cancel && (cancelled() || signal.is_cancelled()) {
            call.cancel().await?;
            sent_cancel = true;
        }
        match tokio::time::timeout(Duration::from_millis(10), call.next()).await {
            Ok(Some(event)) => {
                if failure.is_none() {
                    if let Err(error) = emit(event) {
                        failure = Some(error);
                        call.cancel().await?;
                        sent_cancel = true;
                    }
                }
            }
            Ok(None) => break,
            Err(_) => {}
        }
    }
    let result = call.result().await;
    match failure {
        Some(error) => Err(error),
        None => result,
    }
}

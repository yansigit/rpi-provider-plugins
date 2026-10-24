//! Worker callbacks retain their original native context until origin release.
use crate::{
    journal::port::JournalPort,
    worker::{HostCall, HostHandler, HostResult},
};
use rpi_plugin_sdk::provider::native::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{OnceCell, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

type Request = Arc<dyn Fn(Value, CancellationToken) -> HostResult + Send + Sync>;
#[derive(Clone)]
struct Invocation {
    live: Request,
    persistence: Request,
}
fn request(client: Client) -> Request {
    Arc::new(move |input, signal| {
        let client = client.clone();
        Box::pin(async move { client.request(input, signal).await })
    })
}
struct Origin {
    invocation: Invocation,
    session_id: String,
    closed: AtomicBool,
    authority_notice: AtomicBool,
    journals: OnceCell<Arc<JournalPort>>,
    _permit: OwnedSemaphorePermit,
}
impl Origin {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(journals) = self.journals.get() {
            journals.close();
        }
    }
}
#[derive(Default)]
struct State {
    closing: bool,
    origins: HashMap<String, Arc<Origin>>,
}
pub struct Router {
    state: Mutex<State>,
    capacity: Arc<Semaphore>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scoped {
    scope: String,
    method: String,
    input: Value,
}

impl Default for Router {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            capacity: Arc::new(Semaphore::new(128)),
        }
    }
}
impl Router {
    /// Capture on the foreign callback thread, before sending any worker request.
    pub fn handler(self: &Arc<Self>, client: Client) -> Result<HostHandler, String> {
        let client = client.scoped()?;
        let persistence = client.session_persistence()?;
        Ok(self.bind(Some(Invocation {
            live: request(client),
            persistence: request(persistence),
        })))
    }
    /// The standing connection can only reuse a captured origin, never borrow a root context.
    pub fn standing(self: &Arc<Self>) -> HostHandler {
        self.bind(None)
    }
    fn bind(self: &Arc<Self>, invocation: Option<Invocation>) -> HostHandler {
        let router = self.clone();
        Arc::new(move |call| {
            let router = router.clone();
            let invocation = invocation.clone();
            Box::pin(async move { router.dispatch(invocation, call).await })
        })
    }
    async fn dispatch(
        &self,
        invocation: Option<Invocation>,
        call: HostCall,
    ) -> Result<Value, String> {
        if self.state.lock().unwrap().closing {
            return Err("Cursor host router is closed".into());
        }
        match call.method.as_str() {
            "native" => {
                let invocation = invocation.ok_or("Cursor callback has no invocation owner")?;
                if call.input["port"] == "sessions" {
                    if call.input["action"] != "capture" {
                        return Err("Cursor session callbacks require an origin binding".into());
                    }
                    self.capture(invocation, call.input, call.signal).await
                } else {
                    (invocation.live)(call.input, call.signal).await
                }
            }
            "scoped" => {
                let scoped: Scoped = serde_json::from_value(call.input)
                    .map_err(|_| "Invalid Cursor scoped callback")?;
                let origin = self
                    .state
                    .lock()
                    .unwrap()
                    .origins
                    .get(&scoped.scope)
                    .cloned()
                    .ok_or("Expired or foreign Cursor session binding")?;
                match scoped.method.as_str() {
                    "native" if scoped.input["port"] == "sessions" => {
                        if scoped.input["origin"] != scoped.scope
                            || scoped.input["action"] == "capture"
                        {
                            return Err("Cursor callback cannot redirect a session binding".into());
                        }
                        let release = scoped.input["action"] == "release";
                        let view = scoped.input["action"] == "view";
                        let mut result =
                            (origin.invocation.persistence)(scoped.input, CancellationToken::new())
                                .await?;
                        if view {
                            let location = (origin.invocation.persistence)(
                                json!({"port":"sessions","action":"location","origin":scoped.scope}),
                                CancellationToken::new(),
                            ).await?;
                            let path = Path::new(location["path"].as_str().unwrap_or(""));
                            if crate::import_guard::guard_native_view(path, &mut result)
                                && !origin.authority_notice.swap(true, Ordering::AcqRel)
                            {
                                (origin.invocation.live)(
                                    json!({"port":"ui","action":"notify","kind":"warning",
                                    "text":crate::import_guard::FRESH_AGENT_NOTICE}),
                                    CancellationToken::new(),
                                )
                                .await?;
                            }
                        }
                        if release {
                            origin.close();
                            let mut state = self.state.lock().unwrap();
                            if state
                                .origins
                                .get(&scoped.scope)
                                .is_some_and(|known| Arc::ptr_eq(known, &origin))
                            {
                                state.origins.remove(&scoped.scope);
                            }
                        }
                        Ok(result)
                    }
                    "native" => (origin.invocation.live)(scoped.input, call.signal).await,
                    "journals" => Self::journal(&origin, &scoped.scope, scoped.input).await,
                    _ => Err("Unsupported Cursor scoped callback".into()),
                }
            }
            _ => Err("Unsupported Cursor host callback".into()),
        }
    }
    async fn capture(
        &self,
        invocation: Invocation,
        input: Value,
        signal: CancellationToken,
    ) -> Result<Value, String> {
        let permit = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "Cursor session binding limit reached")?;
        let captured = (invocation.live)(input, signal).await?;
        let id = captured["origin"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Invalid native session capture")?
            .to_owned();
        let session_id = captured["snapshot"]["metadata"]["id"]
            .as_str()
            .filter(|s| !s.is_empty());
        let result = if let Some(session_id) = session_id {
            let mut state = self.state.lock().unwrap();
            if state.closing {
                Err("Cursor host router closed during capture")
            } else if state.origins.contains_key(&id) {
                return Err("Duplicate native session binding".into());
            } else {
                state.origins.insert(
                    id.clone(),
                    Arc::new(Origin {
                        invocation: invocation.clone(),
                        session_id: session_id.into(),
                        closed: AtomicBool::new(false),
                        authority_notice: AtomicBool::new(false),
                        journals: OnceCell::new(),
                        _permit: permit,
                    }),
                );
                Ok(())
            }
        } else {
            Err("Invalid captured native session identity")
        };
        if let Err(error) = result {
            (invocation.persistence)(
                json!({"port":"sessions","action":"release","origin":id}),
                CancellationToken::new(),
            )
            .await?;
            return Err(error.into());
        }
        Ok(captured)
    }
    async fn journal(origin: &Origin, id: &str, mut input: Value) -> Result<Value, String> {
        let object = input
            .as_object_mut()
            .ok_or("Invalid Cursor journal callback")?;
        if object.remove("origin").as_ref().and_then(Value::as_str) != Some(id) {
            return Err("Cursor journal origin mismatch".into());
        }
        let journal = origin
            .journals
            .get_or_try_init(|| async {
                let location = (origin.invocation.persistence)(
                    json!({"port":"sessions","action":"location","origin":id}),
                    CancellationToken::new(),
                )
                .await?;
                let path = location["path"]
                    .as_str()
                    .ok_or("Cursor durable journals require persistent session storage")?;
                JournalPort::for_native_session(Path::new(path), &origin.session_id).map(Arc::new)
            })
            .await?
            .clone();
        if origin.closed.load(Ordering::Acquire) {
            journal.close();
            return Err("Cursor journal origin is closed".into());
        }
        // Once admitted, durability completes even if the worker stops waiting.
        let result = tokio::task::spawn_blocking(move || journal.request(input))
            .await
            .map_err(|_| "Cursor journal task stopped; inspect recovery records")?;
        if result.as_ref().err().map(String::as_str)
            == Some(crate::import_guard::FRESH_AGENT_NOTICE)
            && !origin.authority_notice.swap(true, Ordering::AcqRel)
        {
            (origin.invocation.live)(
                json!({"port":"ui","action":"notify","kind":"warning",
                "text":crate::import_guard::FRESH_AGENT_NOTICE}),
                CancellationToken::new(),
            )
            .await?;
        }
        result
    }
    /// Release abandoned readers before draining worker host tasks, then release captured origins.
    pub async fn close(&self) -> Result<(), String> {
        self.capacity.close();
        let origins = {
            let mut state = self.state.lock().unwrap();
            state.closing = true;
            std::mem::take(&mut state.origins)
        };
        for origin in origins.values() {
            origin.close();
        }
        let mut failure = None;
        for (id, origin) in origins {
            if let Err(error) = (origin.invocation.persistence)(
                json!({"port":"sessions","action":"release","origin":id}),
                CancellationToken::new(),
            )
            .await
            {
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod import_authority_tests;
#[cfg(test)]
mod tests;

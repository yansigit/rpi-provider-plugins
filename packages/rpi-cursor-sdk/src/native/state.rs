use crate::{
    native_host::Router,
    runtime::{drive, Owner, Session},
    worker::WorkerConfig,
};
use rpi_plugin_sdk::provider::{native::Client, ProviderContext};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{Notify, OnceCell},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

struct Scoped {
    owner: Arc<Owner>,
    router: Arc<Router>,
    started: OnceCell<Result<(), String>>,
}
type Children = Arc<Mutex<HashMap<String, Arc<OnceCell<Result<Arc<Scoped>, String>>>>>>;

pub(super) struct Runtime {
    pub context: ProviderContext,
    pub client: Client,
    pub stopping: CancellationToken,
    pub manifest: Value,
    config: WorkerConfig,
    locations: Value,
    root: Arc<Scoped>,
    children: Children,
    watchers: Mutex<Vec<JoinHandle<Result<(), String>>>>,
    failure: Mutex<Option<String>>,
    active: Mutex<usize>,
    idle: Notify,
}
struct Admission<'a>(&'a Runtime);
impl Drop for Admission<'_> {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().unwrap();
        *active -= 1;
        if *active == 0 {
            self.0.idle.notify_waiters();
        }
    }
}
impl Runtime {
    pub fn open(client: Client, entry: PathBuf) -> Result<Arc<Self>, String> {
        let context = ProviderContext::new();
        let stopping = CancellationToken::new();
        let opened = context
            .run(&stopping, async {
                client
                    .require_capabilities(
                        &[
                            "assistant.durationMs",
                            "json.exactNumbers",
                            "auth.publicErrors",
                            "auth.configuredValues",
                            "providers.awaitedPublication",
                            "providers.refreshIsolation",
                            "providers.scopedCallbacks",
                            "providers.childCatalogs",
                            "context.bootstrap",
                            "context.snapshot",
                            "context.flags",
                            "context.abort",
                            "context.scopeLifetime",
                            "context.trustProvenance",
                            "events.lifecycleReasons",
                            "events.compactionDetails",
                            "events.selectionDetails",
                            "events.promptContext",
                            "tools.provenance",
                            "tools.metadata",
                            "tools.activation",
                            "tools.exactActivation",
                            "tools.registrationPolicy",
                            "tools.selectionState",
                            "tools.boundedProgress",
                            "tools.hostBindings",
                            "sessions.location",
                            "sessions.view",
                            "sessions.persistenceLease",
                            "commands.owned",
                            "ui.notifications",
                            "ui.status",
                        ],
                        stopping.clone(),
                    )
                    .await?;
                let locations = client
                    .request(
                        json!({"port":"context","action":"bootstrap"}),
                        stopping.clone(),
                    )
                    .await?;
                let config = WorkerConfig {
                    node: std::env::var_os("RPI_CURSOR_NODE")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| "node".into()),
                    cwd: PathBuf::from(
                        locations["cwd"]
                            .as_str()
                            .ok_or("Missing Cursor project directory")?,
                    ),
                    entry,
                };
                let router = Arc::new(Router::default());
                let session =
                    Session::open(config.clone(), locations.clone(), router.clone()).await?;
                let manifest = session.manifest.clone();
                Ok::<_, String>((
                    config,
                    locations,
                    manifest,
                    Arc::new(Scoped {
                        owner: Owner::new(session, stopping.child_token()),
                        router,
                        started: OnceCell::new(),
                    }),
                ))
            })
            .and_then(|value| value);
        match opened {
            Ok((config, locations, manifest, root)) => Ok(Arc::new(Self {
                context,
                client,
                stopping,
                manifest,
                config,
                locations,
                root,
                children: Arc::new(Mutex::new(HashMap::new())),
                watchers: Mutex::new(Vec::new()),
                failure: Mutex::new(None),
                active: Mutex::new(0),
                idle: Notify::new(),
            })),
            Err(error) => {
                context.finish_shutdown();
                Err(error)
            }
        }
    }
    async fn reap(&self, all: bool) {
        let pending = {
            let mut watchers = self.watchers.lock().unwrap();
            let mut pending = Vec::new();
            let mut index = 0;
            while index < watchers.len() {
                if all || watchers[index].is_finished() {
                    pending.push(watchers.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            pending
        };
        for task in pending {
            let result = task
                .await
                .unwrap_or_else(|_| Err("Cursor child cleanup task stopped".into()));
            if let Err(error) = result {
                self.failure.lock().unwrap().get_or_insert(error);
            }
        }
    }
    async fn scoped(
        &self,
        client: &Client,
        signal: &CancellationToken,
        method: &str,
        cancelled: &impl Fn() -> bool,
    ) -> Result<Arc<Scoped>, String> {
        self.reap(false).await;
        if self.stopping.is_cancelled() {
            return Err("Cursor extension is shutting down".into());
        }
        let inspection = client
            .request(json!({"port":"context","action":"inspect"}), signal.clone())
            .await?;
        if inspection["closing"] == true {
            return Err("Cursor child runtime is closing".into());
        }
        let scope = inspection
            .get("scopeId")
            .ok_or("Host omitted the native scope identity")?;
        if scope.is_null() {
            return Ok(self.root.clone());
        }
        let id = scope.as_str().ok_or("Invalid native scope identity")?;
        let id = id.to_owned();
        let cell = {
            let mut children = self.children.lock().unwrap();
            if children.len() >= 64 && !children.contains_key(&id) {
                return Err("Cursor child runtime capacity exhausted".into());
            }
            children.entry(id.clone()).or_default().clone()
        };
        let result = cell
            .get_or_init(|| async {
                let lifetime = client.scope_lifetime()?;
                let router = Arc::new(Router::default());
                let session =
                    Session::open(self.config.clone(), self.locations.clone(), router.clone())
                        .await?;
                let scoped = Arc::new(Scoped {
                    owner: Owner::new(session, self.stopping.child_token()),
                    router,
                    started: OnceCell::new(),
                });
                let owner = scoped.owner.clone();
                let children = self.children.clone();
                let id = id.clone();
                let stopping = self.stopping.clone();
                self.watchers.lock().unwrap().push(tokio::spawn(async move {
                    let watched = lifetime
                        .request(
                            json!({"port":"context","action":"waitClosed"}),
                            stopping.clone(),
                        )
                        .await;
                    let closed = owner.close().await;
                    children.lock().unwrap().remove(&id);
                    drop(lifetime);
                    closed?;
                    if stopping.is_cancelled() {
                        Ok(())
                    } else {
                        watched.map(|_| ())
                    }
                }));
                Ok(scoped)
            })
            .await
            .clone();
        if result.is_err() {
            let mut children = self.children.lock().unwrap();
            if children
                .get(&id)
                .is_some_and(|known| Arc::ptr_eq(known, &cell))
            {
                children.remove(&id);
            }
        }
        let scoped = result?;
        // Startup can reenter auth/catalog callbacks before its session event finishes.
        if !matches!(method, "provider.auth" | "provider.catalog") {
            scoped
                .started
                .get_or_init(|| async {
                    let started = async {
                        let invocation = scoped.owner.enter()?;
                        let host = scoped.router.handler(client.clone())?;
                        let call = invocation
                            .session()
                            .call(
                                "extension.event",
                                json!({"event":{"type":"session_start","reason":"startup"}}),
                                host,
                                &invocation.signal,
                                cancelled,
                            )
                            .await?;
                        drive(call, &invocation.signal, cancelled, |_| Ok(()))
                            .await
                            .map(|_| ())
                    }
                    .await;
                    if let Err(error) = started {
                        let _ = scoped.owner.close().await;
                        return Err(error);
                    }
                    Ok(())
                })
                .await
                .clone()?;
        }
        Ok(scoped)
    }
    pub fn invoke(
        &self,
        client: Client,
        method: &str,
        input: Value,
        cancelled: &impl Fn() -> bool,
        mut emit: impl FnMut(Value, &CancellationToken) -> Result<(), String>,
    ) -> Result<Value, String> {
        let signal = self.stopping.child_token();
        {
            let mut active = self.active.lock().unwrap();
            if cancelled() || signal.is_cancelled() {
                return Err("Cursor operation cancelled".into());
            }
            *active += 1;
        }
        let _admission = Admission(self);
        self.context.run(&signal, async {
            let scoped = self.scoped(&client, &signal, method, cancelled).await?;
            let invocation = scoped.owner.enter()?;
            let host = scoped.router.handler(client)?;
            let call = invocation
                .session()
                .call(method, input, host, &invocation.signal, cancelled)
                .await?;
            drive(call, &invocation.signal, cancelled, |event| {
                emit(event, &invocation.signal)
            })
            .await
        })?
    }
    pub fn close(&self) -> Result<(), String> {
        {
            let _gate = self.active.lock().unwrap();
            self.stopping.cancel();
        }
        self.context.run(&CancellationToken::new(), async {
            loop {
                let idle = self.idle.notified();
                tokio::pin!(idle);
                idle.as_mut().enable();
                if *self.active.lock().unwrap() == 0 {
                    break;
                }
                idle.await;
            }
            self.reap(true).await;
            let children = std::mem::take(&mut *self.children.lock().unwrap());
            for cell in children.into_values() {
                if let Some(Ok(child)) = cell.get() {
                    if let Err(error) = child.owner.close().await {
                        self.failure.lock().unwrap().get_or_insert(error);
                    }
                }
            }
            let root = self.root.owner.close().await;
            if let Some(error) = self.failure.lock().unwrap().clone() {
                return Err(error);
            }
            root
        })?
    }
}

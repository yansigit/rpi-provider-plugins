//! Private local worker transport. Credits are returned only after consumption.
mod wire;

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wire::{Body, CAPACITY, MAX_CALLS, MAX_FRAME, PAYLOAD_BYTES};

pub struct WorkerConfig {
    pub node: PathBuf,
    pub entry: PathBuf,
    pub cwd: PathBuf,
}

pub struct HostCall {
    pub parent: String,
    pub method: String,
    pub input: Value,
    pub signal: CancellationToken,
}

pub type HostResult = Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>;
pub type HostHandler = Arc<dyn Fn(HostCall) -> HostResult + Send + Sync>;

struct Request {
    host: Option<HostHandler>,
    events: Option<mpsc::Sender<(u64, Value)>>,
    completion: Option<oneshot::Sender<Result<Value, String>>>,
    sequence: u64,
    credits: BTreeMap<u64, usize>,
    bytes: usize,
    terminal: Option<(u64, Result<Value, String>)>,
    completed: bool,
    cancelled: bool,
}

struct State {
    generation: String,
    stopped: CancellationToken,
    failure: Mutex<Option<String>>,
    requests: Mutex<HashMap<String, Request>>,
    outgoing: mpsc::Sender<(Vec<u8>, usize)>,
    queued_bytes: AtomicUsize,
    next_id: Mutex<u64>,
    host_tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl State {
    fn send(&self, id: &str, body: Body) -> Result<(), String> {
        let urgent = matches!(&body, Body::Cancel | Body::Credit { .. });
        let bytes = wire::encode(&wire::message(&self.generation, id, body))?;
        let accounted = if urgent { 0 } else { bytes.len() };
        if self.stopped.is_cancelled() {
            return Err(self.error());
        }
        // Credits/cancellation have reserved slots and cannot be crowded out by large RPC bodies.
        if self
            .queued_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |bytes| {
                bytes
                    .checked_add(accounted)
                    .filter(|bytes| *bytes <= (MAX_FRAME + 4) * MAX_CALLS * 2)
            })
            .is_err()
        {
            let error = "Cursor worker control byte capacity exhausted".to_owned();
            self.fail(error.clone());
            return Err(error);
        }
        self.outgoing.try_send((bytes, accounted)).map_err(|_| {
            self.queued_bytes.fetch_sub(accounted, Ordering::AcqRel);
            let error = "Cursor worker control queue unavailable".to_owned();
            self.fail(error.clone());
            error
        })
    }

    fn cancel(&self, id: &str) -> Result<(), String> {
        {
            let mut requests = self.requests.lock().unwrap();
            let Some(request) = requests.get_mut(id) else {
                return Ok(());
            };
            if request.cancelled || request.completed {
                return Ok(());
            }
            request.cancelled = true;
        }
        self.send(id, Body::Cancel)
    }

    fn error(&self) -> String {
        self.failure
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "Cursor worker disconnected".into())
    }

    fn fail(&self, error: String) {
        let mut failure = self.failure.lock().unwrap();
        if failure.is_some() {
            return;
        }
        *failure = Some(error.clone());
        self.stopped.cancel();
        for request in self.requests.lock().unwrap().values_mut() {
            request.events.take();
            if let Some(completion) = request.completion.take() {
                let _ = completion.send(Err(error.clone()));
            }
        }
    }

    fn credit(&self, id: &str, sequence: u64) -> Result<(), String> {
        let frame = wire::encode(&wire::message(
            &self.generation,
            id,
            Body::Credit { sequence },
        ))?;
        let queued = {
            let mut requests = self.requests.lock().unwrap();
            let request = requests.get_mut(id).ok_or("unknown worker credit owner")?;
            let weight = request
                .credits
                .remove(&sequence)
                .ok_or("unknown worker credit")?;
            request.bytes -= weight;
            // Enqueue before freeing admission: a new call must follow this credit on the wire.
            let queued = !self.stopped.is_cancelled() && self.outgoing.try_send((frame, 0)).is_ok();
            if request.completed && request.credits.is_empty() {
                requests.remove(id);
            }
            queued
        };
        if queued {
            Ok(())
        } else {
            if !self.stopped.is_cancelled() {
                self.fail("Cursor worker credit queue unavailable".into());
            }
            Err(self.error())
        }
    }
}

pub struct Worker {
    child: Option<Child>,
    state: Arc<State>,
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

pub struct Call {
    id: String,
    state: Arc<State>,
    events: mpsc::Receiver<(u64, Value)>,
    completion: Option<oneshot::Receiver<Result<Value, String>>>,
}

impl Call {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn next(&mut self) -> Option<Value> {
        let (sequence, event) = self.events.recv().await?;
        // The caller now owns this event. Its memory is outside queued accounting.
        if let Err(error) = self.state.credit(&self.id, sequence) {
            if !self.state.stopped.is_cancelled() {
                self.state.fail(error);
            }
        }
        Some(event)
    }

    pub async fn cancel(&self) -> Result<(), String> {
        self.state.cancel(&self.id)
    }

    /// Discard intermediates while waiting, matching the host's result-only path.
    pub async fn result(mut self) -> Result<Value, String> {
        while self.next().await.is_some() {}
        self.completion
            .take()
            .ok_or("worker result already consumed")?
            .await
            .map_err(|_| self.state.error())?
    }
}

impl Drop for Call {
    fn drop(&mut self) {
        if self.completion.is_none() {
            return;
        }
        self.events.close();
        let queued: Vec<_> = std::iter::from_fn(|| self.events.try_recv().ok())
            .map(|(sequence, _)| sequence)
            .collect();
        let _ = self.state.cancel(&self.id);
        for sequence in queued {
            let _ = self.state.credit(&self.id, sequence);
        }
    }
}

impl Worker {
    pub async fn spawn(config: WorkerConfig, host: HostHandler) -> Result<Self, String> {
        let entry = config
            .entry
            .canonicalize()
            .map_err(|_| "Cursor worker asset is missing; reinstall rpi-cursor-sdk")?;
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "could not bind private Cursor worker connection")?;
        let generation = random_hex::<16>()?;
        let token = random_hex::<32>()?;
        let mut child = Command::new(&config.node)
            .arg(entry)
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not start Cursor Node worker: {error}"))?;
        let bootstrap = json!({"version":1,"generation":generation,"token":token,"port":listener.local_addr().map_err(|_| "worker listener unavailable")?.port()});
        let started = tokio::time::timeout(Duration::from_secs(10), async {
            let mut stdin = child.stdin.take().ok_or("worker stdin unavailable")?;
            stdin
                .write_all(&serde_json::to_vec(&bootstrap).unwrap())
                .await
                .map_err(|_| "worker bootstrap failed")?;
            stdin
                .shutdown()
                .await
                .map_err(|_| "worker bootstrap failed")?;
            drop(stdin);
            authenticate(&listener, &generation, &token).await
        })
        .await
        .map_err(|_| "Cursor worker handshake timed out; Node 24+ is required".to_owned())
        .and_then(|v| v);
        let stream = match started {
            Ok(stream) => stream,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
        };
        stream
            .set_nodelay(true)
            .map_err(|_| "worker socket setup failed")?;
        let (mut reader, mut writer) = stream.into_split();
        // At most 16 * 64 stream credits plus bounded calls/completions/cancellation.
        let (outgoing, mut writes) = mpsc::channel::<(Vec<u8>, usize)>(MAX_CALLS * (CAPACITY + 4));
        let state = Arc::new(State {
            generation,
            stopped: CancellationToken::new(),
            failure: Mutex::new(None),
            requests: Mutex::new(HashMap::new()),
            outgoing,
            queued_bytes: AtomicUsize::new(0),
            next_id: Mutex::new(0),
            host_tasks: Mutex::new(Vec::new()),
        });
        let write_state = Arc::clone(&state);
        let writer = tokio::spawn(async move {
            let result = async {
                while let Some((frame, accounted)) = writes.recv().await {
                    write_state
                        .queued_bytes
                        .fetch_sub(accounted, Ordering::AcqRel);
                    writer
                        .write_all(&frame)
                        .await
                        .map_err(|_| "Cursor worker write failed".to_owned())?;
                }
                Ok::<_, String>(())
            };
            tokio::select! {
                biased;
                _ = write_state.stopped.cancelled() => {},
                result = result => if let Err(error) = result { write_state.fail(error); },
            }
        });
        let read_state = Arc::clone(&state);
        let reader = tokio::spawn(async move {
            let result = read_loop(&mut reader, &read_state, host);
            tokio::select! {
                biased;
                _ = read_state.stopped.cancelled() => {},
                result = result => if let Err(error) = result { read_state.fail(error); },
            }
        });
        Ok(Self {
            child: Some(child),
            state,
            reader: Some(reader),
            writer: Some(writer),
        })
    }

    pub async fn call(&self, method: &str, input: Value) -> Result<Call, String> {
        self.start_call(method, input, None).await
    }

    /// Install the invocation binding before the worker can issue a host callback.
    pub async fn call_with_host(
        &self,
        method: &str,
        input: Value,
        host: HostHandler,
    ) -> Result<Call, String> {
        self.start_call(method, input, Some(host)).await
    }

    async fn start_call(
        &self,
        method: &str,
        input: Value,
        host: Option<HostHandler>,
    ) -> Result<Call, String> {
        if method.is_empty() || method.len() > 128 {
            return Err("invalid Cursor worker method".into());
        }
        // Allocate and enqueue under one lock so concurrent callers cannot reorder IDs.
        let mut next = self.state.next_id.lock().unwrap();
        if *next == 9_007_199_254_740_991 {
            return Err("Cursor worker request IDs exhausted".into());
        }
        let (events, receiver) = mpsc::channel(CAPACITY);
        let (completion, result) = oneshot::channel();
        let id = {
            let mut requests = self.state.requests.lock().unwrap();
            if requests.len() >= MAX_CALLS {
                return Err("Cursor worker call capacity exhausted".into());
            }
            *next += 1;
            let id = format!("h{next}");
            requests.insert(
                id.clone(),
                Request {
                    host,
                    events: Some(events),
                    completion: Some(completion),
                    sequence: 0,
                    credits: BTreeMap::new(),
                    bytes: 0,
                    terminal: None,
                    completed: false,
                    cancelled: false,
                },
            );
            id
        };
        let mut call = Call {
            id,
            state: Arc::clone(&self.state),
            events: receiver,
            completion: Some(result),
        };
        if let Err(error) = self.state.send(
            &call.id,
            Body::Request {
                method: method.into(),
                input,
                parent: None,
            },
        ) {
            self.state.requests.lock().unwrap().remove(&call.id);
            call.completion.take();
            return Err(error);
        }
        Ok(call)
    }

    /// The adapter shutdown request must finish before this final process teardown.
    pub async fn shutdown(mut self) -> Result<(), String> {
        self.state.fail("Cursor worker shut down".into());
        let status = if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            child
                .wait()
                .await
                .map(|_| ())
                .map_err(|_| "could not reap Cursor worker".into())
        } else {
            Ok(())
        };
        if let Some(task) = self.reader.take() {
            let _ = task.await;
        }
        if let Some(task) = self.writer.take() {
            let _ = task.await;
        }
        drain_hosts(&self.state).await;
        status
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.state.fail("Cursor worker owner dropped".into());
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let state = Arc::clone(&self.state);
                let reader = self.reader.take();
                runtime.spawn(async move {
                    let _ = child.wait().await;
                    if let Some(reader) = reader {
                        let _ = reader.await;
                    }
                    drain_hosts(&state).await;
                });
            }
        }
    }
}

async fn drain_hosts(state: &State) {
    let tasks = std::mem::take(&mut *state.host_tasks.lock().unwrap());
    for task in tasks {
        let _ = task.await;
    }
}

fn random_hex<const N: usize>() -> Result<String, String> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes).map_err(|_| "could not generate worker connection secret")?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

async fn authenticate(
    listener: &TcpListener,
    generation: &str,
    token: &str,
) -> Result<TcpStream, String> {
    for _ in 0..8 {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|_| "worker accept failed")?;
        let hello =
            tokio::time::timeout(Duration::from_secs(1), wire::read(&mut stream, 4096)).await;
        let Ok(Ok((hello, _))) = hello else { continue };
        let supplied = hello["token"].as_str().unwrap_or("");
        let matches = supplied.len() == token.len()
            && supplied
                .bytes()
                .zip(token.bytes())
                .fold(0u8, |diff, (a, b)| diff | (a ^ b))
                == 0;
        if !matches
            || hello["version"] != 1
            || hello["generation"] != generation
            || hello.as_object().map(|v| v.len()) != Some(3)
        {
            continue;
        }
        wire::write(
            &mut stream,
            &json!({"version":1,"generation":generation,"ready":true}),
        )
        .await?;
        return Ok(stream);
    }
    Err("Cursor worker authentication failed".into())
}

async fn read_loop(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
    state: &Arc<State>,
    host: HostHandler,
) -> Result<(), String> {
    let mut last_worker_id = 0;
    let host_calls = Arc::new(Mutex::new(HashMap::<String, CancellationToken>::new()));
    loop {
        let (value, bytes) = wire::read(reader, MAX_FRAME).await?;
        if state.stopped.is_cancelled() {
            return Err(state.error());
        }
        let (id, body) = wire::decode(value, &state.generation)?;
        match body {
            Body::Request {
                method,
                input,
                parent,
            } => {
                let number = wire::id_number(&id).ok_or("invalid host call identity")?;
                let parent = parent.ok_or("missing host call scope")?;
                let host = state
                    .requests
                    .lock()
                    .unwrap()
                    .get(&parent)
                    .map(|request| request.host.clone().unwrap_or_else(|| Arc::clone(&host)))
                    .ok_or("unknown worker host call scope")?;
                if !id.starts_with('w')
                    || number <= last_worker_id
                    || method.is_empty()
                    || method.len() > 128
                {
                    return Err("invalid worker host call".into());
                }
                last_worker_id = number;
                let signal = state.stopped.child_token();
                {
                    let mut calls = host_calls.lock().unwrap();
                    if calls.len() >= MAX_CALLS {
                        return Err("worker host call capacity exceeded".into());
                    }
                    calls.insert(id.clone(), signal.clone());
                }
                let calls = Arc::clone(&host_calls);
                let task_state = Arc::clone(state);
                let task = tokio::spawn(async move {
                    let result = tokio::spawn(async move {
                        host(HostCall {
                            parent,
                            method,
                            input,
                            signal,
                        })
                        .await
                    })
                    .await
                    .unwrap_or_else(|_| Err("Cursor host operation stopped".into()));
                    let body = match result {
                        Ok(result) => Body::Reply {
                            result,
                            last_event: 0,
                        },
                        Err(error) => Body::Error {
                            error,
                            last_event: 0,
                        },
                    };
                    let _ = task_state.send(&id, body);
                    calls.lock().unwrap().remove(&id);
                });
                let mut tasks = state.host_tasks.lock().unwrap();
                tasks.retain(|task| !task.is_finished());
                tasks.push(task);
            }
            Body::Cancel => {
                let calls = host_calls.lock().unwrap();
                if !id.starts_with('w')
                    || wire::id_number(&id).map_or(true, |id| id > last_worker_id)
                {
                    return Err("unknown worker host cancellation".into());
                }
                if let Some(signal) = calls.get(&id) {
                    signal.cancel();
                }
            }
            Body::Event { sequence, event } => {
                let discarded = {
                    let mut requests = state.requests.lock().unwrap();
                    let request = requests.get_mut(&id).ok_or("unknown worker event owner")?;
                    let weight = bytes.clamp(1, PAYLOAD_BYTES);
                    if request.completed
                        || sequence != request.sequence + 1
                        || request.credits.len() >= CAPACITY
                        || weight > PAYLOAD_BYTES - request.bytes
                        || request
                            .terminal
                            .as_ref()
                            .is_some_and(|(last, _)| sequence > *last)
                    {
                        return Err("worker exceeded stream credits or sequence".into());
                    }
                    request.sequence = sequence;
                    request.credits.insert(sequence, weight);
                    request.bytes += weight;
                    let discarded = match request
                        .events
                        .as_ref()
                        .ok_or("worker event after completion")?
                        .try_send((sequence, event))
                    {
                        Ok(()) => false,
                        Err(mpsc::error::TrySendError::Closed(_)) => true,
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            return Err("worker event queue overflow".into())
                        }
                    };
                    finish(request)?;
                    discarded
                };
                if discarded {
                    state.credit(&id, sequence)?;
                }
            }
            Body::Reply { result, last_event } => complete(state, &id, last_event, Ok(result))?,
            Body::Error { error, last_event } => complete(state, &id, last_event, Err(error))?,
            Body::Credit { .. } => return Err("unexpected worker credit".into()),
        }
    }
}

fn finish(request: &mut Request) -> Result<(), String> {
    if request
        .terminal
        .as_ref()
        .is_some_and(|(last, _)| *last == request.sequence)
    {
        let (_, result) = request.terminal.take().unwrap();
        request.completed = true;
        request.events.take();
        if let Some(completion) = request.completion.take() {
            let _ = completion.send(result);
        }
    }
    Ok(())
}

fn complete(
    state: &State,
    id: &str,
    last: u64,
    result: Result<Value, String>,
) -> Result<(), String> {
    let mut requests = state.requests.lock().unwrap();
    let request = requests.get_mut(id).ok_or("unknown worker completion")?;
    if request.completed
        || request.terminal.is_some()
        || last < request.sequence
        || last > request.sequence + CAPACITY as u64
    {
        return Err("invalid worker completion sequence".into());
    }
    request.terminal = Some((last, result));
    finish(request)?;
    if request.completed && request.credits.is_empty() {
        requests.remove(id);
    }
    Ok(())
}

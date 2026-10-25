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
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wire::{Body, CAPACITY, MAX_CALLS, MAX_FRAME, PAYLOAD_BYTES};

/// Bound on the captured worker stderr ring; enough for a module-resolution
/// failure or a Node stack tail, small enough to stay in an error string.
const DIAGNOSTIC_BYTES: usize = 4 * 1024;
const DIAGNOSTIC_LINES: usize = 12;
const REDACTED: &str = "[redacted]";

#[derive(Clone)]
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
    diagnostics: Option<JoinHandle<()>>,
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
            // A worker that cannot resolve its packaged dependencies dies while
            // bootstrapping; keep its own diagnostics instead of discarding them.
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not start Cursor Node worker: {error}"))?;
        let diagnostics = Arc::new(Diagnostics::new(vec![generation.clone(), token.clone()]));
        let mut diagnostic_task = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(capture_diagnostics(stderr, Arc::clone(&diagnostics))));
        let stdin = child.stdin.take().ok_or("worker stdin unavailable")?;
        let bootstrap = json!({"version":1,"generation":generation,"token":token,"port":listener.local_addr().map_err(|_| "worker listener unavailable")?.port()});
        let outcome = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::select! {
                biased;
                // The child dying is itself the answer: wait() would otherwise
                // hide it behind the listener timeout below.
                status = child.wait() => Bootstrap::Exited(status),
                outcome = open_bootstrap(
                    stdin,
                    serde_json::to_vec(&bootstrap).unwrap(),
                    &listener,
                    &generation,
                    &token,
                ) => outcome,
            }
        })
        .await;
        let stream =
            match bootstrap_error(outcome, &mut child, &diagnostics, &mut diagnostic_task).await {
                Ok(stream) => stream,
                Err(error) => {
                    if let Some(task) = diagnostic_task.take() {
                        task.abort();
                    }
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
            diagnostics: diagnostic_task,
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
        if let Some(task) = self.diagnostics.take() {
            // The reaped child has closed its stderr; keep a bound anyway.
            let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
        }
        drain_hosts(&self.state).await;
        status
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.state.fail("Cursor worker owner dropped".into());
        if let Some(task) = self.diagnostics.take() {
            task.abort();
        }
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

/// How the private worker connection came up, or why it did not.
enum Bootstrap {
    Stream(TcpStream),
    Exited(std::io::Result<std::process::ExitStatus>),
    StdinClosed,
    Refused(String),
}

/// Deliver the bootstrap frame and authenticate the worker. The stdin half is
/// generic so a closed pipe is exercised deterministically instead of only when
/// the child happens to die before the frame lands.
async fn open_bootstrap<W>(
    mut stdin: W,
    payload: Vec<u8>,
    listener: &TcpListener,
    generation: &str,
    token: &str,
) -> Bootstrap
where
    W: AsyncWrite + Unpin,
{
    let written = match stdin.write_all(&payload).await {
        Ok(()) => stdin.shutdown().await,
        Err(error) => Err(error),
    };
    if written.is_err() {
        // Writing failed, not the handshake: the child closed its read end (or
        // died). The caller reaps it, so its own stderr is the diagnostic.
        return Bootstrap::StdinClosed;
    }
    drop(stdin);
    match authenticate(listener, generation, token).await {
        Ok(stream) => Bootstrap::Stream(stream),
        Err(error) => Bootstrap::Refused(error),
    }
}

/// Turn a bootstrap outcome into either the authenticated stream or the most
/// specific failure the child left behind, so no arm can collapse into a bare
/// "bootstrap failed" message.
async fn bootstrap_error(
    outcome: Result<Bootstrap, tokio::time::error::Elapsed>,
    child: &mut Child,
    diagnostics: &Diagnostics,
    task: &mut Option<JoinHandle<()>>,
) -> Result<TcpStream, String> {
    match outcome {
        Ok(Bootstrap::Stream(stream)) => Ok(stream),
        Ok(Bootstrap::Exited(status)) => {
            flush_diagnostics(task, Duration::from_secs(1)).await;
            status
                .map_err(|error| format!("could not reap Cursor worker: {error}"))
                .and_then(|status| Err(diagnostics.failure(status)))
        }
        // The child stopped reading its bootstrap frame: it is either gone
        // already or on its way out, so spend the same bounded wait the exit arm
        // does and keep whatever it printed.
        Ok(Bootstrap::StdinClosed) => Err(bootstrap_closed(
            child,
            diagnostics,
            task,
            Duration::from_secs(1),
        )
        .await),
        Ok(Bootstrap::Refused(error)) => Err(error),
        Err(_elapsed) => {
            // Only a worker that is still alive without connecting reaches this
            // arm; the earlier arms report the child's own failure instead. Its
            // stderr still explains why it never connected.
            flush_diagnostics(task, Duration::from_millis(250)).await;
            Err(diagnostics.handshake_timeout())
        }
    }
}

/// A worker that closed its bootstrap pipe keeps its own diagnostics.
async fn bootstrap_closed(
    child: &mut Child,
    diagnostics: &Diagnostics,
    task: &mut Option<JoinHandle<()>>,
    limit: Duration,
) -> String {
    reap_bootstrap_failure(child, diagnostics, task, limit).await
}

/// Let the capture task drain what the child already wrote. A worker that is
/// still alive never closes its stderr, so the wait stays bounded.
async fn flush_diagnostics(task: &mut Option<JoinHandle<()>>, limit: Duration) {
    if let Some(task) = task.take() {
        let _ = tokio::time::timeout(limit, task).await;
    }
}

/// A worker that stopped reading its bootstrap frame is normally exiting; wait
/// briefly so its exit status and the tail of its stderr still explain why.
async fn reap_bootstrap_failure(
    child: &mut Child,
    diagnostics: &Diagnostics,
    task: &mut Option<JoinHandle<()>>,
    limit: Duration,
) -> String {
    let status = tokio::time::timeout(limit, child.wait()).await;
    flush_diagnostics(task, limit).await;
    match status {
        Ok(Ok(status)) => diagnostics.failure(status),
        _ => diagnostics.closed_pipe(),
    }
}

/// Last bytes a worker wrote to stderr, kept bounded so a chatty child cannot
/// grow host memory, and scrubbed of bootstrap secrets as they arrive.
struct Diagnostics {
    secrets: Vec<String>,
    ring: Mutex<Vec<u8>>,
}

impl Diagnostics {
    fn new(secrets: Vec<String>) -> Self {
        Self {
            secrets: secrets
                .into_iter()
                .filter(|secret| !secret.is_empty())
                .collect(),
            ring: Mutex::new(Vec::new()),
        }
    }

    /// A panic elsewhere must not turn a bootstrap failure into a poison panic:
    /// the bytes captured so far are still valid evidence.
    fn ring(&self) -> std::sync::MutexGuard<'_, Vec<u8>> {
        self.ring
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push(&self, chunk: &[u8]) {
        let mut ring = self.ring();
        ring.extend_from_slice(chunk);
        // Scrub before trimming: a secret that spans the ring boundary is still
        // whole here, so it is replaced instead of surviving in part.
        for secret in &self.secrets {
            redact_matches(&mut ring, secret);
        }
        if ring.len() > DIAGNOSTIC_BYTES {
            let mut start = ring.len() - DIAGNOSTIC_BYTES;
            // Do not cut a multi-byte character in half and publish U+FFFD.
            while start < ring.len() && ring[start] & 0b1100_0000 == 0b1000_0000 {
                start += 1;
            }
            ring.drain(..start);
        }
    }

    /// The bounded stderr tail with every bootstrap secret replaced.
    fn excerpt(&self) -> Option<String> {
        let mut bytes = self.ring().clone();
        for secret in &self.secrets {
            redact_matches(&mut bytes, secret);
            redact_partial_edges(&mut bytes, secret);
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let tail: Vec<&str> = text.lines().rev().take(DIAGNOSTIC_LINES).collect();
        let excerpt = tail.into_iter().rev().collect::<Vec<&str>>().join("\n");
        let excerpt = excerpt.trim();
        if excerpt.is_empty() {
            return None;
        }
        Some(excerpt.to_owned())
    }

    /// Fail fast with the worker's own words instead of a generic timeout.
    /// Bootstrap secrets are never replayed, even when the child echoes them.
    fn failure(&self, status: std::process::ExitStatus) -> String {
        match self.excerpt() {
            None => format!("Cursor worker exited before handshake ({status}): no stderr output"),
            Some(excerpt) => {
                format!("Cursor worker exited before handshake ({status}): {excerpt}")
            }
        }
    }

    /// A live worker that never connected keeps the sentence the reference
    /// implementation and the native-loading gate assert on, and appends whatever
    /// it did print before giving up.
    fn handshake_timeout(&self) -> String {
        const BASE: &str = "Cursor worker handshake timed out; Node 24+ is required";
        match self.excerpt() {
            None => BASE.to_owned(),
            Some(excerpt) => format!("{BASE} (worker stderr: {excerpt})"),
        }
    }

    fn closed_pipe(&self) -> String {
        match self.excerpt() {
            None => "Cursor worker closed its bootstrap input before handshake".to_owned(),
            Some(excerpt) => {
                format!("Cursor worker closed its bootstrap input before handshake: {excerpt}")
            }
        }
    }
}

/// Bytes a worker may put between two characters of a secret and still be matched.
const SECRET_SEPARATORS: [u8; 5] = [b' ', b'-', b':', b'.', b'_'];
/// Shortest surviving fragment still treated as part of a secret.
const MIN_SECRET_FRAGMENT: usize = 8;

/// Match `secret` at `start`, ignoring ASCII case and tolerating one separator
/// between characters, so an upper-cased or spaced echo is still recognised.
fn match_secret(bytes: &[u8], start: usize, secret: &str) -> Option<usize> {
    let mut cursor = start;
    for expected in secret.bytes() {
        if cursor != start && cursor < bytes.len() && SECRET_SEPARATORS.contains(&bytes[cursor]) {
            cursor += 1;
        }
        if cursor >= bytes.len() || !bytes[cursor].eq_ignore_ascii_case(&expected) {
            return None;
        }
        cursor += 1;
    }
    Some(cursor)
}

/// Replace every occurrence of `secret` in `bytes` with [`REDACTED`].
fn redact_matches(bytes: &mut Vec<u8>, secret: &str) {
    if secret.is_empty() {
        return;
    }
    let mut index = 0;
    while index < bytes.len() {
        match match_secret(bytes, index, secret) {
            Some(end) => {
                bytes.splice(index..end, REDACTED.bytes());
                index += REDACTED.len();
            }
            None => index += 1,
        }
    }
}

/// The ring trim can cut a secret at either edge, so replace a surviving head
/// suffix or tail prefix instead of publishing half of it.
fn redact_partial_edges(bytes: &mut Vec<u8>, secret: &str) {
    if secret.len() <= MIN_SECRET_FRAGMENT {
        return;
    }
    let lowered = secret.to_ascii_lowercase();
    for length in (MIN_SECRET_FRAGMENT..secret.len()).rev() {
        let suffix = &lowered.as_bytes()[secret.len() - length..];
        if bytes.len() >= length && bytes[..length].eq_ignore_ascii_case(suffix) {
            bytes.splice(..length, REDACTED.bytes());
            break;
        }
    }
    for length in (MIN_SECRET_FRAGMENT..secret.len()).rev() {
        if bytes.len() < length {
            continue;
        }
        let start = bytes.len() - length;
        if bytes[start..].eq_ignore_ascii_case(&lowered.as_bytes()[..length]) {
            bytes.splice(start.., REDACTED.bytes());
            break;
        }
    }
}

async fn capture_diagnostics(mut stderr: ChildStderr, diagnostics: Arc<Diagnostics>) {
    let mut chunk = [0; 1024];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(bytes) => diagnostics.push(&chunk[..bytes]),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn diagnostics_of(secrets: &[&str]) -> Diagnostics {
        Diagnostics::new(secrets.iter().map(|secret| (*secret).to_owned()).collect())
    }

    /// Longest run of hex digits in `text`, used to prove no secret survived.
    fn longest_hex_run(text: &str) -> usize {
        let mut longest = 0;
        let mut run = 0;
        for byte in text.bytes() {
            if byte.is_ascii_hexdigit() {
                run += 1;
                longest = longest.max(run);
            } else {
                run = 0;
            }
        }
        longest
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn a_closed_bootstrap_pipe_is_not_a_generic_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (stdin, closed) = tokio::io::duplex(64);
        drop(closed);
        let outcome = open_bootstrap(stdin, b"{}".to_vec(), &listener, "g", "t").await;
        assert!(
            matches!(outcome, Bootstrap::StdinClosed),
            "a failed frame write must reach the reap arm instead of ending the handshake"
        );
    }

    #[test]
    fn secrets_split_across_chunks_are_redacted() {
        let diagnostics = diagnostics_of(&[TOKEN]);
        diagnostics.push(TOKEN[..20].as_bytes());
        diagnostics.push(TOKEN[20..].as_bytes());
        let excerpt = diagnostics.excerpt().unwrap();
        assert_eq!(excerpt, REDACTED);
        assert!(longest_hex_run(&excerpt) < MIN_SECRET_FRAGMENT);
    }

    #[test]
    fn secrets_are_redacted_before_the_ring_is_trimmed() {
        let diagnostics = diagnostics_of(&[TOKEN]);
        // The trim below starts inside the secret; scrubbing after the trim would
        // publish its surviving tail.
        diagnostics.push(&vec![b'y'; DIAGNOSTIC_BYTES - 84]);
        diagnostics.push(TOKEN.as_bytes());
        diagnostics.push(&vec![b'y'; DIAGNOSTIC_BYTES - 46]);
        let excerpt = diagnostics.excerpt().unwrap();
        assert!(excerpt.starts_with('y'), "{excerpt}");
        assert!(!excerpt.contains(TOKEN), "{excerpt}");
        assert!(
            longest_hex_run(&excerpt) < MIN_SECRET_FRAGMENT,
            "{excerpt}"
        );
    }

    #[test]
    fn secrets_that_lose_an_edge_are_still_redacted() {
        let diagnostics = diagnostics_of(&[TOKEN]);
        // Cut the secret itself in half at the ring boundary.
        diagnostics.push(&vec![b'y'; DIAGNOSTIC_BYTES - 40]);
        diagnostics.push(TOKEN.as_bytes());
        let excerpt = diagnostics.excerpt().unwrap();
        assert!(
            longest_hex_run(&excerpt) < MIN_SECRET_FRAGMENT,
            "{excerpt}"
        );
        // A secret that is still arriving must not be half-published either.
        let diagnostics = diagnostics_of(&[TOKEN]);
        diagnostics.push(&vec![b'y'; DIAGNOSTIC_BYTES - 2]);
        diagnostics.push(TOKEN[..40].as_bytes());
        let excerpt = diagnostics.excerpt().unwrap();
        assert!(
            longest_hex_run(&excerpt) < MIN_SECRET_FRAGMENT,
            "{excerpt}"
        );
    }

    #[test]
    fn upper_cased_and_spaced_secrets_are_redacted() {
        let diagnostics = diagnostics_of(&[TOKEN]);
        diagnostics.push(TOKEN.to_ascii_uppercase().as_bytes());
        diagnostics.push(b"\n");
        let spaced: String = TOKEN
            .chars()
            .map(|character| format!("{character} "))
            .collect();
        diagnostics.push(spaced.as_bytes());
        let excerpt = diagnostics.excerpt().unwrap();
        assert!(!excerpt.is_empty());
        assert!(longest_hex_run(&excerpt) < MIN_SECRET_FRAGMENT, "{excerpt}");
    }

    #[test]
    fn the_ring_tail_stays_valid_utf8() {
        let diagnostics = diagnostics_of(&[]);
        diagnostics.push(&vec![b'a'; DIAGNOSTIC_BYTES - 4]);
        // "🦀" is four bytes; the trim below starts inside it.
        diagnostics.push("🦀".as_bytes());
        diagnostics.push(&vec![b'b'; DIAGNOSTIC_BYTES - 2]);
        let excerpt = diagnostics.excerpt().unwrap();
        assert!(
            !excerpt.contains('\u{FFFD}'),
            "trim cut a character: {excerpt}"
        );
        assert!(excerpt.starts_with("bbb"), "{excerpt}");
    }

    #[test]
    fn a_poisoned_ring_still_reports_diagnostics() {
        let diagnostics = Arc::new(diagnostics_of(&[TOKEN]));
        let poison = Arc::clone(&diagnostics);
        let _ = std::thread::spawn(move || {
            let _guard = poison.ring();
            panic!("poison the diagnostics ring");
        })
        .join();
        diagnostics.push(format!("worker said: {TOKEN}\n").as_bytes());
        let excerpt = diagnostics.excerpt().unwrap();
        assert!(excerpt.contains("worker said"), "{excerpt}");
        assert!(!excerpt.contains(TOKEN), "{excerpt}");
        assert!(diagnostics.closed_pipe().contains("worker said"));
    }

    #[cfg(unix)]
    struct FakeWorker {
        child: Child,
        diagnostics: Arc<Diagnostics>,
        task: Option<JoinHandle<()>>,
    }

    #[cfg(unix)]
    async fn fake_worker(script: &str) -> FakeWorker {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let diagnostics = Arc::new(Diagnostics::new(Vec::new()));
        let task = child
            .stderr
            .take()
            .map(|stderr| tokio::spawn(capture_diagnostics(stderr, Arc::clone(&diagnostics))));
        FakeWorker {
            child,
            diagnostics,
            task,
        }
    }

    #[cfg(unix)]
    impl Drop for FakeWorker {
        fn drop(&mut self) {
            let _ = self.child.start_kill();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_closed_bootstrap_pipe_failure_carries_the_worker_stderr() {
        let mut worker = fake_worker(
            "printf 'Error: bootstrap input closed\\n' >&2; exec 0<&-; exec 2>&-; sleep 30",
        )
        .await;
        let error = bootstrap_error(
            Ok(Bootstrap::StdinClosed),
            &mut worker.child,
            &worker.diagnostics,
            &mut worker.task,
        )
        .await
        .err()
        .expect("a closed bootstrap pipe must not spawn");
        assert!(
            error.starts_with("Cursor worker closed its bootstrap input before handshake"),
            "{error}"
        );
        assert!(error.contains("Error: bootstrap input closed"), "{error}");
        let _ = worker.child.kill().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_exited_worker_failure_carries_its_status_and_stderr() {
        let mut worker = fake_worker("printf 'Error: bootstrap exited\\n' >&2; exit 3").await;
        let status = worker.child.wait().await.unwrap();
        let error = bootstrap_error(
            Ok(Bootstrap::Exited(Ok(status))),
            &mut worker.child,
            &worker.diagnostics,
            &mut worker.task,
        )
        .await
        .err()
        .expect("an exited worker must not spawn");
        assert!(
            error.starts_with("Cursor worker exited before handshake ("),
            "{error}"
        );
        assert!(error.contains("Error: bootstrap exited"), "{error}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_handshake_timeout_carries_the_worker_stderr() {
        // The child prints, closes stderr (so the capture task ends on its own),
        // and then stays alive without ever connecting.
        let mut worker = fake_worker("printf 'Error: never connected\\n' >&2; exec 2>&-; sleep 30")
            .await;
        let elapsed =
            tokio::time::timeout(Duration::from_millis(50), std::future::pending::<()>())
                .await
                .unwrap_err();
        let error = bootstrap_error(
            Err(elapsed),
            &mut worker.child,
            &worker.diagnostics,
            &mut worker.task,
        )
        .await
        .err()
        .expect("a silent worker must not spawn");
        assert!(
            error.starts_with("Cursor worker handshake timed out; Node 24+ is required"),
            "{error}"
        );
        assert!(error.contains("(worker stderr: "), "{error}");
        assert!(error.contains("Error: never connected"), "{error}");
        let _ = worker.child.kill().await;
    }
}

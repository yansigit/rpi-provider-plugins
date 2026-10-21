//! Request execution shared by local and vault credential sources.
use crate::{
    metadata::{self, SessionStore},
    sse::DecoderState,
    stream::{EventSink, Flow, State},
};
use serde_json::{json, Value};
use std::{
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub const PRE_RESPONSE_MS: u64 = 600_000;
pub const TRAILING_USAGE_MS: u64 = 1_000;
pub const TIMEOUT_MESSAGE: &str = "Antigravity did not begin a response within the pre-response time limit. Check your connection and try again later.";
const ABORTED: &str = "This operation was aborted";

/// One absolute deadline; accepting headers never replenishes the remaining time.
pub struct Deadline {
    caller: CancellationToken,
    pub signal: CancellationToken,
    expires: Instant,
    active: AtomicBool,
    changed: tokio::sync::Notify,
}
impl Deadline {
    pub fn new(caller: CancellationToken) -> Self {
        Self {
            signal: caller.child_token(),
            caller,
            expires: Instant::now() + Duration::from_millis(PRE_RESPONSE_MS),
            active: AtomicBool::new(true),
            changed: tokio::sync::Notify::new(),
        }
    }
    pub fn check(&self) -> Result<(), String> {
        if self.caller.is_cancelled() {
            return Err(ABORTED.into());
        }
        if self.active.load(Ordering::Relaxed) && Instant::now() >= self.expires {
            self.signal.cancel();
            return Err(TIMEOUT_MESSAGE.into());
        }
        if self.signal.is_cancelled() {
            return Err(ABORTED.into());
        }
        Ok(())
    }
    pub fn accept_headers(&self) -> Result<(), String> {
        self.check()?;
        self.active.store(false, Ordering::Relaxed);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn resume(&self) -> Result<(), String> {
        self.active.store(true, Ordering::Relaxed);
        self.changed.notify_waiters();
        self.check()
    }
    pub async fn wait<T>(
        &self,
        work: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        tokio::pin!(work);
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            self.check()?;
            let result = tokio::select! {
                biased;
                _=self.caller.cancelled()=>Err(ABORTED.into()),
                _=tokio::time::sleep_until(self.expires), if self.active.load(Ordering::Relaxed)=>{
                    self.signal.cancel();
                    Err(TIMEOUT_MESSAGE.into())
                },
                _=self.signal.cancelled()=>Err(ABORTED.into()),
                _=changed=>continue,
                value=&mut work=>value,
            };
            self.check()?;
            return result;
        }
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}

#[derive(Default)]
pub struct Secrets {
    pub credentials: Vec<String>,
    pub projects: Vec<String>,
}
impl Secrets {
    pub fn credential(&mut self, value: &str) {
        if !value.is_empty() && !self.credentials.iter().any(|known| known == value) {
            self.credentials.push(value.into());
        }
    }
    pub fn project(&mut self, value: &str) {
        if !value.is_empty() && !self.projects.iter().any(|known| known == value) {
            self.projects.push(value.into());
        }
    }
}
pub trait Body: Send {
    fn next(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}
pub struct Response<B> {
    pub status: u16,
    pub headers: Vec<(String, Vec<u8>)>,
    pub body: B,
}
/// Sources choose credentials at each physical send; the stream never falls back between sources.
pub trait Source: Sync {
    type Body: Body;
    fn now_ms(&self) -> i64;
    fn next_id(&self) -> String;
    fn sessions(&self) -> &Mutex<SessionStore>;
    fn open(
        &self,
        model: &Value,
        context: &Value,
        options: &Value,
        deadline: &Deadline,
        secrets: &mut Secrets,
    ) -> impl Future<Output = Result<Response<Self::Body>, String>> + Send;
}

pub async fn run(
    source: &impl Source,
    model: &Value,
    context: &Value,
    options: &Value,
    caller: CancellationToken,
    response_metadata: bool,
    sink: &mut impl EventSink,
) -> Value {
    let started = Instant::now();
    let deadline = Deadline::new(caller.clone());
    let mut state = State::new(model, source.now_ms());
    let mut secrets = Secrets::default();
    if let Some(key) = options["apiKey"].as_str() {
        secrets.credential(key);
    }
    // A private response receipt must precede ordinary events at the native host boundary.
    let mut start_sent = false;
    let result = async {
        if !response_metadata {
            state.start(sink).await?;
            start_sent = true;
        }
        // TLS futures are large in debug builds; keep nested deadline/FFI frames off the thread stack.
        let mut response = deadline.wait(Box::pin(source.open(model, context, options, &deadline, &mut secrets))).await?;
        if (200..300).contains(&response.status) {
            deadline.accept_headers()?;
        }
        if response_metadata {
            sink.emit(json!({"type":"response","response":{"status":response.status,"headers":response.headers}})).await?;
            state.start(sink).await?;
            start_sent = true;
        }
        if !(200..300).contains(&response.status) {
            let text = deadline.wait(async {
                let mut bytes = Vec::new();
                while let Some(chunk) = response.body.next().await? { bytes.extend(chunk); }
                Ok(encoding_rs::UTF_8.decode(&bytes).0.into_owned())
            }).await?;
            return Err(format!("Antigravity request failed: HTTP {} {text}", response.status));
        }
        let mut decoder = DecoderState::default();
        let mut eof = false;
        let mut flow = Flow::Continue;
        loop {
            let next = async {
                loop {
                    if let Some(chunk) = decoder.next_chunk(eof) { return Ok::<_, String>(Some(chunk)); }
                    if eof { return Ok(None); }
                    match response.body.next().await? {
                        Some(bytes) => decoder.push(&bytes),
                        None => eof = true,
                    }
                }
            };
            let chunk = if flow == Flow::TrailingUsage {
                match tokio::time::timeout(Duration::from_millis(TRAILING_USAGE_MS), next).await {
                    Ok(value) => value?,
                    Err(_) => { deadline.signal.cancel(); break; }
                }
            } else {
                next.await?
            };
            let Some(chunk) = chunk else { break; };
            flow = state.chunk(&chunk, &mut || source.next_id(), sink).await?;
            if flow == Flow::Complete { break; }
        }
        drop(response);
        state.finish(caller.is_cancelled(), duration_ms(started), sink).await?;
        if matches!(state.output["stopReason"].as_str(), Some("stop" | "length")) {
            source.sessions().lock().unwrap().complete_execution(
                &metadata::session_key(context, options), &mut || source.next_id());
        }
        Ok::<_,String>(())
    }.await;
    if let Err(error) = result {
        deadline.signal.cancel();
        if !start_sent {
            let _ = state.start(sink).await;
        }
        let _ = state
            .fail(
                &error,
                caller.is_cancelled(),
                duration_ms(started),
                &secrets.credentials,
                &secrets.projects,
                sink,
            )
            .await;
    }
    state.output
}
fn duration_ms(start: Instant) -> u64 {
    (start.elapsed().as_secs_f64() * 1000.0).round() as u64
}

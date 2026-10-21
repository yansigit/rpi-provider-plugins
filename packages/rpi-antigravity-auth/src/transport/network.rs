//! One physical HTTP/1.1 send. All spawned work stays on the owning provider runtime.
use super::{
    body::{Framer, Inflater},
    https_proxy, parse_response_head, serialize_request, ResponseHead, IDLE_TIMEOUT_MS,
    RESPONSE_HEADER_TIMEOUT_MS,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use percent_encoding::percent_decode_str;
use std::{
    collections::HashMap,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::{sleep_until, Instant},
};
use tokio_rustls::{
    rustls::{self, pki_types::ServerName},
    TlsConnector,
};
use tokio_util::sync::CancellationToken;
use url::Url;

const ABORTED: &str = "The operation was aborted";
const READ_SIZE: usize = 16 * 1024;
pub type DebugCallback = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct Options {
    pub timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub signal: CancellationToken,
    pub environment: HashMap<String, String>,
    pub on_debug: Option<DebugCallback>,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            timeout_ms: RESPONSE_HEADER_TIMEOUT_MS,
            idle_timeout_ms: IDLE_TIMEOUT_MS,
            signal: CancellationToken::new(),
            environment: std::env::vars().collect(),
            on_debug: None,
        }
    }
}
impl Options {
    fn debug(&self, message: &str) {
        if let Some(callback) = &self.on_debug {
            callback(message);
        }
    }
    async fn stage<T>(
        &self,
        future: impl Future<Output = Result<T, String>>,
        timeout: &str,
        debug: &str,
    ) -> Result<T, String> {
        tokio::select! {
            biased;
            _ = self.signal.cancelled() => Err(ABORTED.into()),
            result = tokio::time::timeout(Duration::from_millis(self.timeout_ms.max(1)), future) => match result {
                Ok(result) => result,
                Err(_) => { self.debug(debug); Err(timeout.into()) }
            }
        }
    }
}

#[derive(Clone)]
pub struct Client {
    tls: Arc<rustls::ClientConfig>,
}
impl Default for Client {
    fn default() -> Self {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        Self::with_tls(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth(),
        )
    }
}
impl Client {
    pub fn with_tls(config: rustls::ClientConfig) -> Self {
        Self {
            tls: Arc::new(config),
        }
    }

    pub async fn fetch(
        &self,
        url: &str,
        method: Option<&str>,
        headers: &[(String, String)],
        body: &[u8],
        options: Options,
    ) -> Result<Response, String> {
        let parsed = Url::parse(url).map_err(|error| error.to_string())?;
        if parsed.scheme() != "https" {
            return Err(format!("agy transport only supports https URLs: {url}"));
        }
        if options.signal.is_cancelled() {
            return Err(ABORTED.into());
        }
        let request = serialize_request(&parsed, method, headers, body)?;
        let timeout = options.timeout_ms;
        options.debug(&format!(
            "agy transport connecting to {} with header timeout {timeout}ms",
            parsed.host_str().unwrap_or_default()
        ));
        let mut socket = self.connect(&parsed, &options).await?;
        let (head, leftover) = options
            .stage(
                async {
                    socket.write_all(&request).await.map_err(io_error)?;
                    options.debug(&format!(
                        "agy transport request dispatched ({} bytes)",
                        request.len()
                    ));
                    read_head(&mut socket).await
                },
                &format!(
                    "Antigravity request timed out waiting for response headers after {timeout}ms"
                ),
                &format!("agy transport response header timeout after {timeout}ms"),
            )
            .await?;
        let head = parse_response_head(&head)?;
        options.debug(&format!(
            "agy transport response headers received: {} {}",
            head.status, head.status_text
        ));
        if !(200..=599).contains(&head.status) {
            return Err("init[\"status\"] must be in the range of 200 to 599, inclusive.".into());
        }
        if matches!(head.status, 204 | 205 | 304) {
            return Err(format!(
                "Response constructor: Invalid response status code {}",
                head.status
            ));
        }
        if head.status_text.contains(['\r', '\n']) {
            return Err("Invalid statusText".into());
        }
        let signal = options.signal.child_token();
        let mut body_options = options;
        body_options.signal = signal.clone();
        let framer = Framer::new(&head);
        let gzip = head.gzip;
        let (sender, receiver) = mpsc::channel(1);
        let (finish, completion) = oneshot::channel();
        tokio::spawn(async move {
            let result = pump(socket, leftover, framer, gzip, sender, body_options).await;
            let _ = finish.send(result);
        });
        Ok(Response {
            head,
            body: Body {
                receiver,
                completion: Some(completion),
                signal,
                finished: false,
            },
        })
    }

    async fn connect(
        &self,
        target: &Url,
        options: &Options,
    ) -> Result<tokio_rustls::client::TlsStream<Prefixed>, String> {
        let host = target.host_str().unwrap_or_default();
        let port = target.port().unwrap_or(443);
        let server_name =
            ServerName::try_from(host.to_owned()).map_err(|error| error.to_string())?;
        let connector = TlsConnector::from(self.tls.clone());
        let timeout = options.timeout_ms;
        if let Some(proxy) = https_proxy(target, &options.environment) {
            let mut socket = options
                .stage(
                    async {
                        TcpStream::connect((
                            proxy.host_str().unwrap_or_default(),
                            proxy.port().unwrap_or(8080),
                        ))
                        .await
                        .map_err(io_error)
                    },
                    &format!(
                        "Antigravity request timed out connecting to HTTPS proxy after {timeout}ms"
                    ),
                    &format!("agy transport proxy connect timeout after {timeout}ms"),
                )
                .await?;
            let request = proxy_connect_request(&proxy, target)?;
            let (head, prefix) = options.stage(async {
                socket.write_all(&request).await.map_err(io_error)?;
                read_head(&mut socket).await
            }, &format!("Antigravity request timed out waiting for response headers after {timeout}ms"),
            &format!("agy transport proxy CONNECT response timeout after {timeout}ms")).await?;
            let first = head.split("\r\n").next().unwrap_or_default();
            let status = first.as_bytes();
            if !(status.starts_with(b"HTTP/1.0 2") || status.starts_with(b"HTTP/1.1 2"))
                || status.len() < 12
                || !status[10..12].iter().all(u8::is_ascii_digit)
                || status
                    .get(12)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
            {
                return Err(format!("Proxy CONNECT failed: {first}"));
            }
            options.stage(async {
                connector.connect(server_name, Prefixed { socket, prefix: io::Cursor::new(prefix) }).await.map_err(io_error)
            }, &format!("Antigravity request timed out during proxy TLS handshake after {timeout}ms"),
            &format!("agy transport proxy TLS handshake timeout after {timeout}ms")).await
        } else {
            options
                .stage(
                    async {
                        let socket = TcpStream::connect((host, port)).await.map_err(io_error)?;
                        connector
                            .connect(
                                server_name,
                                Prefixed {
                                    socket,
                                    prefix: io::Cursor::new(Vec::new()),
                                },
                            )
                            .await
                            .map_err(io_error)
                    },
                    &format!("Antigravity request timed out connecting after {timeout}ms"),
                    &format!("agy transport TLS connect timeout after {timeout}ms"),
                )
                .await
        }
    }
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}

fn gzip_error(_: io::Error) -> String {
    // Node 24 Readable.toWeb exposes zlib failures as TypeErrors with empty messages.
    String::new()
}

pub fn proxy_connect_request(proxy: &Url, target: &Url) -> Result<Vec<u8>, String> {
    let target = format!(
        "{}:{}",
        target.host_str().unwrap_or_default(),
        target.port().unwrap_or(443)
    );
    let mut result = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if !proxy.username().is_empty() {
        let decode = |value: &str| {
            let bytes = value.as_bytes();
            if (0..bytes.len()).any(|i| {
                bytes[i] == b'%'
                    && (i + 2 >= bytes.len()
                        || !bytes[i + 1].is_ascii_hexdigit()
                        || !bytes[i + 2].is_ascii_hexdigit())
            }) {
                return Err("URI malformed".to_owned());
            }
            percent_decode_str(value)
                .decode_utf8()
                .map(|value| value.into_owned())
                .map_err(|_| "URI malformed".to_owned())
        };
        let credential = format!(
            "{}:{}",
            decode(proxy.username())?,
            decode(proxy.password().unwrap_or_default())?
        );
        result.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            STANDARD.encode(credential)
        ));
    }
    result.push_str("\r\n");
    Ok(result.into_bytes())
}

async fn read_head(socket: &mut (impl AsyncRead + Unpin)) -> Result<(String, Vec<u8>), String> {
    let mut buffer = Vec::new();
    let mut bytes = [0; READ_SIZE];
    loop {
        let count = socket.read(&mut bytes).await.map_err(io_error)?;
        if count == 0 {
            return Err("Socket closed before response headers".into());
        }
        buffer.extend_from_slice(&bytes[..count]);
        if let Some(end) = buffer.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let head = buffer[..end].iter().map(|byte| char::from(*byte)).collect();
            return Ok((head, buffer.split_off(end + 4)));
        }
    }
}

struct Prefixed {
    socket: TcpStream,
    prefix: io::Cursor<Vec<u8>>,
}
impl AsyncRead for Prefixed {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let position = self.prefix.position() as usize;
        if position < self.prefix.get_ref().len() {
            let count = buffer
                .remaining()
                .min(self.prefix.get_ref().len() - position);
            buffer.put_slice(&self.prefix.get_ref()[position..position + count]);
            self.prefix.set_position((position + count) as u64);
            Poll::Ready(Ok(()))
        } else {
            Pin::new(&mut self.socket).poll_read(cx, buffer)
        }
    }
}
impl AsyncWrite for Prefixed {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.socket).poll_write(cx, bytes)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.socket).poll_shutdown(cx)
    }
}

pub struct Response {
    pub head: ResponseHead,
    pub body: Body,
}
pub struct Body {
    receiver: mpsc::Receiver<Vec<u8>>,
    completion: Option<oneshot::Receiver<Result<(), String>>>,
    signal: CancellationToken,
    finished: bool,
}
impl Body {
    pub async fn next(&mut self) -> Result<Option<Vec<u8>>, String> {
        if self.finished {
            return Ok(None);
        }
        loop {
            if let Some(completion) = &mut self.completion {
                tokio::select! {
                    biased;
                    result = completion => {
                        self.completion = None;
                        if let Err(error) = result.unwrap_or_else(|_| Err("Antigravity response task stopped".into())) {
                            self.finished = true;
                            self.receiver.close();
                            return Err(error);
                        }
                    },
                    bytes = self.receiver.recv() => {
                        if let Some(bytes) = bytes { return Ok(Some(bytes)); }
                        // Completion has its own reserved channel, including on worker failure.
                    }
                }
            } else {
                let bytes = self.receiver.recv().await;
                self.finished = bytes.is_none();
                return Ok(bytes);
            }
        }
    }
}
impl Drop for Body {
    fn drop(&mut self) {
        self.signal.cancel();
    }
}

struct Output {
    sender: mpsc::Sender<Vec<u8>>,
    options: Options,
    deadline: Option<Instant>,
}
impl Output {
    fn arm(&mut self) {
        self.deadline = (self.options.idle_timeout_ms > 0)
            .then(|| Instant::now() + Duration::from_millis(self.options.idle_timeout_ms));
    }
    async fn wait<T>(&self, future: impl Future<Output = Result<T, String>>) -> Result<T, String> {
        tokio::select! {
            biased;
            _ = self.options.signal.cancelled() => Err(ABORTED.into()),
            _ = async { match self.deadline { Some(deadline) => sleep_until(deadline).await, None => std::future::pending().await } } => {
                let timeout = self.options.idle_timeout_ms;
                self.options.debug(&format!("agy transport idle timeout after {timeout}ms with no body data"));
                Err(format!("Antigravity response stalled: no data for {timeout}ms"))
            },
            result = future => result
        }
    }
    async fn send(&self, bytes: Vec<u8>) -> Result<(), String> {
        for bytes in bytes.chunks(READ_SIZE) {
            self.wait(async {
                self.sender
                    .send(bytes.to_vec())
                    .await
                    .map_err(|_| ABORTED.into())
            })
            .await?;
        }
        Ok(())
    }
}
async fn pump(
    mut socket: impl AsyncRead + Unpin,
    leftover: Vec<u8>,
    mut framer: Framer,
    gzip: bool,
    sender: mpsc::Sender<Vec<u8>>,
    options: Options,
) -> Result<(), String> {
    let mut output = Output {
        sender,
        options,
        deadline: None,
    };
    output.arm();
    let mut decoder = gzip.then(Inflater::default);
    let mut eof = false;
    let mut bytes = [0; READ_SIZE];
    framer.push(&leftover);
    loop {
        while let Some(bytes) = framer.next(eof)? {
            if let Some(decoder) = &mut decoder {
                let mut position = 0;
                while position < bytes.len() {
                    let (consumed, bytes) = decoder.push(&bytes[position..]).map_err(gzip_error)?;
                    position += consumed;
                    output.send(bytes).await?;
                }
                loop {
                    let (done, bytes) = decoder.drain(false).map_err(gzip_error)?;
                    output.send(bytes).await?;
                    if done {
                        break;
                    }
                }
            } else {
                output.send(bytes).await?;
            }
        }
        if framer.is_complete() {
            if let Some(decoder) = &mut decoder {
                loop {
                    let (done, bytes) = decoder.drain(true).map_err(gzip_error)?;
                    output.send(bytes).await?;
                    if done {
                        break;
                    }
                }
            }
            return Ok(());
        }
        if eof {
            // A malformed non-progressing chunk cannot consume CPU after the peer closes.
            return output.wait(std::future::pending()).await;
        }
        let count = output
            .wait(async { socket.read(&mut bytes).await.map_err(io_error) })
            .await?;
        eof = count == 0;
        if !eof {
            output.arm();
        }
        framer.push(&bytes[..count]);
    }
}

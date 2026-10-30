//! Loopback callback server for the browser flow. It listens on
//! [`CALLBACK_HOST`]`:`[`CALLBACK_PORT`] and only accepts `GET /callback` carrying the
//! state this login minted; any other request is answered and the wait continues.
//! Failure to bind is not fatal — the driver falls back to pasted `code#state` input.
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use url::form_urlencoded;

use crate::auth::{CALLBACK_HOST, CALLBACK_PORT, LOCAL_CALLBACK_TIMEOUT};

const POLL_INTERVAL: Duration = Duration::from_millis(25);
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HEADER_LINES: usize = 64;

pub struct CallbackServer {
    listener: TcpListener,
    state: String,
    redirect_uri: String,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl CallbackServer {
    /// Binds the pinned loopback address; an occupied port surfaces as an error so
    /// the caller can fall back to the paste flow.
    pub fn bind(state: &str) -> io::Result<Self> {
        Self::bind_on(
            &format!("{CALLBACK_HOST}:{CALLBACK_PORT}"),
            "localhost",
            state,
            LOCAL_CALLBACK_TIMEOUT,
        )
    }

    /// `advertised_host` is what the browser sees; tests bind port 0 and keep the
    /// redirect URI that the OS assigned.
    pub fn bind_on(
        addr: &str,
        advertised_host: &str,
        state: &str,
        timeout: Duration,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        Ok(Self {
            listener,
            state: state.into(),
            redirect_uri: format!("http://{advertised_host}:{port}/callback"),
            deadline: Instant::now() + timeout,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The redirect URI to register for this login; it is the `localhost` spelling
    /// upstream advertises, not the bound numeric host.
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Stops a blocked [`CallbackServer::wait`], for a driver that received a pasted
    /// code while the browser flow was still open.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Blocks until a valid callback arrives, the timeout expires or the server is
    /// cancelled. A valid callback yields the local redirect URL, ready for
    /// [`crate::auth::Login::complete`].
    pub fn wait(&self) -> Option<String> {
        while Instant::now() < self.deadline && !self.cancelled.load(Ordering::SeqCst) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(input) = self.serve(stream) {
                        return Some(input);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(_) => return None,
            }
        }
        None
    }

    fn serve(&self, mut stream: TcpStream) -> Option<String> {
        stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
        stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;

        let target = read_request_target(&stream).ok().flatten()?;
        let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));

        if path != "/callback" {
            respond(
                &mut stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "Not found",
            )
            .ok()?;
            return None;
        }

        let params: Vec<(String, String)> = form_urlencoded::parse(query.as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        let param = |name: &str| {
            params
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };

        let state = match (param("code"), param("state")) {
            (Some(_code), Some(state)) => state,
            _ => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain; charset=utf-8",
                    "Missing code or state",
                )
                .ok()?;
                return None;
            }
        };

        if state != self.state {
            respond(
                &mut stream,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                "Invalid state",
            )
            .ok()?;
            return None;
        }

        respond(
            &mut stream,
            "200 OK",
            "text/html; charset=utf-8",
            &callback_page(),
        )
        .ok()?;

        let echoed = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(params.iter().map(|(key, value)| (key, value)))
            .finish();
        Some(format!("{}?{echoed}", self.redirect_uri))
    }
}

/// The page the browser lands on after a successful callback, matching upstream.
pub fn callback_page() -> String {
    String::from(
        "<!doctype html>\n\
<html>\n  \
<head><meta charset=\"utf-8\" /><title>Authorization complete</title></head>\n  \
<body>\n    \
<h1>Authorization complete</h1>\n    \
<p>You can close this window and return to Pi.</p>\n  \
</body>\n\
</html>",
    )
}

fn read_request_target(stream: &TcpStream) -> io::Result<Option<String>> {
    let mut reader = BufReader::new(stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    for _ in 0..MAX_HEADER_LINES {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some(_method), Some(target)) => Ok(Some(target.to_owned())),
        _ => Ok(None),
    }
}

fn respond(stream: &mut TcpStream, status: &str, content_type: &str, body: &str) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

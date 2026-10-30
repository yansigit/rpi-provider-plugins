//! Loopback server checks. Each test binds port 0 so a developer's real login on
//! 127.0.0.1:53692 never collides with the suite.
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use rpi_anthropic_auth::auth::callback::{callback_page, CallbackServer};
use url::Url;

const STATE: &str = "the-state";
const TIMEOUT: Duration = Duration::from_secs(10);

fn server() -> CallbackServer {
    CallbackServer::bind_on("127.0.0.1:0", "localhost", STATE, TIMEOUT).unwrap()
}

fn port_of(redirect_uri: &str) -> u16 {
    let url = Url::parse(redirect_uri).unwrap();
    assert_eq!(url.host_str(), Some("localhost"));
    assert_eq!(url.path(), "/callback");
    url.port().unwrap()
}

/// Sends one request straight to the loopback socket and returns the raw response.
fn request(port: u16, target: &str) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
fn valid_callback_answers_the_browser_and_yields_the_redirect_url() {
    let server = server();
    let redirect_uri = server.redirect_uri().to_string();
    assert!(
        redirect_uri.starts_with("http://localhost:"),
        "{redirect_uri}"
    );
    let port = port_of(&redirect_uri);

    let (response, input) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| server.wait());
        let response = request(
            port,
            "/callback?code=the-code&state=the-state&scope=org%3Acreate_api_key",
        );
        (response, waiter.join().unwrap())
    });

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(
        response.contains("Content-Type: text/html; charset=utf-8"),
        "{response}"
    );
    assert!(
        response.contains("<h1>Authorization complete</h1>"),
        "{response}"
    );

    let input = input.expect("a valid callback resolves the wait");
    assert!(input.starts_with(&format!("{redirect_uri}?")), "{input}");
    let parsed = Url::parse(&input).unwrap();
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("code".to_string(), "the-code".to_string()),
            ("state".to_string(), STATE.to_string()),
            ("scope".to_string(), "org:create_api_key".to_string()),
        ]
    );
}

#[test]
fn a_wrong_state_is_rejected_and_the_wait_continues() {
    let server = server();
    let port = port_of(server.redirect_uri());

    let (rejected, accepted) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| server.wait());
        let rejected = request(port, "/callback?code=the-code&state=someone-else");
        let accepted = request(port, "/callback?code=the-code&state=the-state");
        (rejected, (accepted, waiter.join().unwrap()))
    });

    assert!(
        rejected.starts_with("HTTP/1.1 400 Bad Request"),
        "{rejected}"
    );
    assert!(rejected.ends_with("Invalid state"), "{rejected}");
    let (accepted, input) = accepted;
    assert!(accepted.starts_with("HTTP/1.1 200 OK"), "{accepted}");
    assert!(input.is_some());
}

#[test]
fn other_paths_and_incomplete_callbacks_are_rejected() {
    let server = CallbackServer::bind_on(
        "127.0.0.1:0",
        "localhost",
        STATE,
        Duration::from_millis(300),
    )
    .unwrap();
    let port = port_of(server.redirect_uri());

    let (not_found, missing_code, missing_state, input) = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| server.wait());
        let not_found = request(port, "/not-callback?code=c&state=the-state");
        let missing_code = request(port, "/callback?state=the-state");
        let missing_state = request(port, "/callback?code=the-code");
        (
            not_found,
            missing_code,
            missing_state,
            waiter.join().unwrap(),
        )
    });

    assert!(
        not_found.starts_with("HTTP/1.1 404 Not Found"),
        "{not_found}"
    );
    assert!(not_found.ends_with("Not found"), "{not_found}");
    assert!(
        missing_code.starts_with("HTTP/1.1 400 Bad Request"),
        "{missing_code}"
    );
    assert!(
        missing_code.ends_with("Missing code or state"),
        "{missing_code}"
    );
    assert!(
        missing_state.ends_with("Missing code or state"),
        "{missing_state}"
    );
    assert!(input.is_none(), "no callback was valid");
}

#[test]
fn an_idle_server_times_out() {
    let server = CallbackServer::bind_on(
        "127.0.0.1:0",
        "localhost",
        STATE,
        Duration::from_millis(120),
    )
    .unwrap();
    let started = Instant::now();
    assert!(server.wait().is_none());
    let elapsed = started.elapsed();
    assert!(elapsed >= Duration::from_millis(120), "{elapsed:?}");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
}

#[test]
fn cancel_releases_a_blocked_wait() {
    let server = server();
    let input = std::thread::scope(|scope| {
        let waiter = scope.spawn(|| server.wait());
        std::thread::sleep(Duration::from_millis(50));
        server.cancel();
        waiter.join().unwrap()
    });
    assert!(input.is_none());
}

#[test]
fn the_callback_page_matches_the_upstream_html() {
    assert_eq!(
        callback_page(),
        "<!doctype html>\n<html>\n  <head><meta charset=\"utf-8\" /><title>Authorization complete</title></head>\n  <body>\n    <h1>Authorization complete</h1>\n    <p>You can close this window and return to Pi.</p>\n  </body>\n</html>"
    );
}

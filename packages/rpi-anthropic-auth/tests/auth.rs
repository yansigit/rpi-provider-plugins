//! Flow-level checks for the Claude Pro/Max OAuth port. Everything runs against a
//! scripted [`Port`], so the crate's own code never touches the network.
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Mutex;

use rpi_anthropic_auth::auth::{
    self, ClientConfig, Credentials, Login, Port, TokenReply, TokenRequest, AUTHORIZE_URL,
    CLIENT_ID_ENV, CLIENT_SECRET_ENV, COMPILED_CLIENT_ID, REDIRECT_URI, SCOPES, TOKEN_URL,
    UPSTREAM_CLIENT_ID,
};
use url::Url;

const NOW: i64 = 1_700_000_000_000;
const LOCAL: &str = "http://localhost:53692/callback";

#[derive(Default)]
struct Script {
    replies: Mutex<VecDeque<Result<TokenReply, String>>>,
    requests: Mutex<Vec<TokenRequest>>,
    sleeps: Mutex<Vec<u64>>,
}

impl Script {
    fn new(replies: Vec<Result<TokenReply, String>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            ..Self::default()
        }
    }

    fn ok(body: &str) -> Self {
        Self::new(vec![Ok(reply(200, body))])
    }

    fn requests(&self) -> Vec<TokenRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn sleeps(&self) -> Vec<u64> {
        self.sleeps.lock().unwrap().clone()
    }
}

impl Port for Script {
    fn send(
        &self,
        request: TokenRequest,
    ) -> impl Future<Output = Result<TokenReply, String>> + Send {
        self.requests.lock().unwrap().push(request);
        let next = self.replies.lock().unwrap().pop_front();
        async move { next.unwrap_or_else(|| Err("no scripted reply".into())) }
    }

    fn sleep(&self, delay_ms: u64) -> impl Future<Output = ()> + Send {
        self.sleeps.lock().unwrap().push(delay_ms);
        async {}
    }

    fn now_ms(&self) -> i64 {
        NOW
    }
}

fn reply(status: u16, body: &str) -> TokenReply {
    TokenReply {
        status,
        headers: Vec::new(),
        body: body.into(),
    }
}

fn reply_with(status: u16, headers: &[(&str, &str)], body: &str) -> TokenReply {
    TokenReply {
        status,
        headers: headers
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        body: body.into(),
    }
}

fn login() -> Login {
    Login::begin_with_entropy(UPSTREAM_CLIENT_ID, &[7u8; 48])
}

fn callback_url(code: &str, state: &str) -> String {
    format!("{LOCAL}?code={code}&state={state}")
}

fn credentials() -> Credentials {
    Credentials {
        access: "old-access".into(),
        refresh: "old-refresh".into(),
        expires: NOW + 60_000,
    }
}

#[test]
fn upstream_constants_match_the_pinned_client() {
    assert_eq!(UPSTREAM_CLIENT_ID, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    assert_eq!(AUTHORIZE_URL, "https://claude.ai/oauth/authorize");
    assert_eq!(TOKEN_URL, "https://platform.claude.com/v1/oauth/token");
    assert_eq!(
        REDIRECT_URI,
        "https://platform.claude.com/oauth/code/callback"
    );
    assert_eq!(
        SCOPES.join(" "),
        "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload"
    );
}

#[test]
fn pkce_challenge_matches_the_rfc7636_vector() {
    assert_eq!(
        auth::pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );

    let pkce = auth::pkce_from_entropy(&[0u8; 32]);
    assert_eq!(pkce.verifier.len(), 43);
    assert!(!pkce.verifier.contains('='));
    assert_eq!(pkce.challenge, auth::pkce_challenge(&pkce.verifier));
    assert_eq!(pkce.challenge.len(), 43);
    for byte in pkce.verifier.bytes().chain(pkce.challenge.bytes()) {
        assert!(
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_',
            "not base64url: {byte}"
        );
    }
}

#[test]
fn state_is_a_dashed_uuid_without_its_dashes() {
    let state = auth::state_from_entropy(&[0xab; 16]);
    assert_eq!(state.len(), 32);
    assert!(state
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()));
    assert_eq!(state.as_bytes()[12], b'4', "version nibble: {state}");
    assert!(
        matches!(state.as_bytes()[16], b'8' | b'9' | b'a' | b'b'),
        "variant nibble: {state}"
    );
    assert_eq!(state, auth::state_from_entropy(&[0xab; 16]));
    assert_ne!(state, auth::state_from_entropy(&[0xac; 16]));
}

#[test]
fn authorize_url_carries_the_upstream_parameters_in_order() {
    let flow = login().use_local_callback(LOCAL);
    let url = Url::parse(&flow.authorize_url()).unwrap();

    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some("claude.ai"));
    assert_eq!(url.path(), "/oauth/authorize");

    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let keys: Vec<&str> = pairs.iter().map(|(key, _)| key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "code",
            "client_id",
            "response_type",
            "redirect_uri",
            "scope",
            "code_challenge",
            "code_challenge_method",
            "state",
        ]
    );
    let value = |name: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap()
    };
    assert_eq!(value("code"), "true");
    assert_eq!(value("client_id"), UPSTREAM_CLIENT_ID);
    assert_eq!(value("response_type"), "code");
    assert_eq!(value("redirect_uri"), LOCAL);
    assert_eq!(value("scope"), SCOPES.join(" "));
    assert_eq!(
        value("code_challenge"),
        auth::pkce_challenge(flow.verifier())
    );
    assert_eq!(value("code_challenge_method"), "S256");
    assert_eq!(value("state"), flow.state());

    // The paste flow advertises the platform redirect, not the loopback one.
    let paste = login();
    assert_eq!(paste.redirect_uri(), REDIRECT_URI);
    assert!(paste
        .authorize_url()
        .contains("redirect_uri=https%3A%2F%2Fplatform.claude.com%2Foauth%2Fcode%2Fcallback"));
}

#[test]
fn parse_accepts_callback_urls_for_initiated_redirects() {
    let local = format!("{LOCAL}?code=the-code&state=the-state&extra=1");
    let parsed = auth::parse_auth_input(&local, &[REDIRECT_URI.into(), LOCAL.into()]).unwrap();
    assert_eq!(parsed.code, "the-code");
    assert_eq!(parsed.state, "the-state");
    assert_eq!(parsed.redirect_uri.as_deref(), Some(LOCAL));

    let platform = format!("{REDIRECT_URI}?code=c&state=s");
    let parsed = auth::parse_auth_input(&platform, &[REDIRECT_URI.into()]).unwrap();
    assert_eq!(parsed.code, "c");
    assert_eq!(parsed.redirect_uri.as_deref(), Some(REDIRECT_URI));

    let trimmed = auth::parse_auth_input(&format!("  {local}  "), &[LOCAL.into()]).unwrap();
    assert_eq!(trimmed.code, "the-code");
}

#[test]
fn parse_rejects_redirects_this_login_did_not_initiate() {
    let foreign = "https://evil.example/callback?code=c&state=s";
    assert!(auth::parse_auth_input(foreign, &[REDIRECT_URI.into()]).is_none());

    let local = format!("{LOCAL}?code=c&state=s");
    assert!(auth::parse_auth_input(&local, &[REDIRECT_URI.into()]).is_none());
}

#[test]
fn parse_accepts_code_hash_state_and_query_strings() {
    let parsed = auth::parse_auth_input("the-code#the-state", &[]).unwrap();
    assert_eq!(
        parsed,
        auth::ParsedAuthInput {
            code: "the-code".into(),
            state: "the-state".into(),
            redirect_uri: None,
        }
    );

    let parsed = auth::parse_auth_input("code=c&state=s", &[]).unwrap();
    assert_eq!(parsed.code, "c");
    assert_eq!(parsed.state, "s");
    assert_eq!(parsed.redirect_uri, None);
}

#[test]
fn parse_rejects_incomplete_or_malformed_input() {
    for input in [
        "",
        "   ",
        "the-code#",
        "#the-state",
        "the-code",
        "code=c",
        "state=s",
        "a#b#c",
        "https://claude.ai/oauth/authorize?code=c&state=s",
    ] {
        assert!(
            auth::parse_auth_input(input, &[REDIRECT_URI.into()]).is_none(),
            "accepted {input:?}"
        );
    }
}

#[test]
fn complete_checks_state_and_resolves_the_redirect() {
    let login = login().use_local_callback(LOCAL);
    let exchange = login
        .complete(&callback_url("the-code", login.state()))
        .unwrap();
    assert_eq!(exchange.code, "the-code");
    assert_eq!(exchange.state, login.state());
    assert_eq!(exchange.redirect_uri, LOCAL);
    assert_eq!(exchange.verifier, login.verifier());

    // A pasted code#state keeps the redirect the login is currently using.
    let pasted = login
        .complete(&format!("the-code#{}", login.state()))
        .unwrap();
    assert_eq!(pasted.redirect_uri, LOCAL);

    assert_eq!(
        login.complete("the-code#not-the-state").unwrap_err(),
        "OAuth state mismatch."
    );
    assert_eq!(
        login.complete("nonsense").unwrap_err(),
        "Could not parse authorization callback input."
    );
}

#[test]
fn exchange_request_matches_the_upstream_body() {
    let client = ClientConfig::upstream();
    let login = login().use_local_callback(LOCAL);
    let exchange = login
        .complete(&callback_url("the-code", login.state()))
        .unwrap();
    let request = exchange.request(&client);

    assert_eq!(request.url, TOKEN_URL);
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.headers,
        [("Content-Type".to_string(), "application/json".to_string())]
    );
    assert_eq!(
        request.body,
        format!(
            r#"{{"grant_type":"authorization_code","client_id":"{UPSTREAM_CLIENT_ID}","code":"the-code","state":"{}","redirect_uri":"{LOCAL}","code_verifier":"{}"}}"#,
            login.state(),
            login.verifier()
        )
    );
    assert!(!request.body.contains("client_secret"));

    let confidential = ClientConfig {
        id: "deployment-client".into(),
        secret: Some("deployment-secret".into()),
    };
    assert!(exchange
        .request(&confidential)
        .body
        .ends_with(r#","client_secret":"deployment-secret"}"#));
}

#[test]
fn refresh_request_matches_the_upstream_body() {
    let request = auth::refresh_request(&ClientConfig::upstream(), "old-refresh");
    assert_eq!(request.url, TOKEN_URL);
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.body,
        format!(
            r#"{{"grant_type":"refresh_token","client_id":"{UPSTREAM_CLIENT_ID}","refresh_token":"old-refresh"}}"#
        )
    );
}

#[tokio::test]
async fn exchange_returns_credentials_with_the_expiry_skew() {
    let script = Script::ok(r#"{"access_token":"acc","refresh_token":"ref","expires_in":3600}"#);
    let client = ClientConfig::upstream();
    let login = login().use_local_callback(LOCAL);
    let exchange = login
        .complete(&callback_url("the-code", login.state()))
        .unwrap();

    let credentials = auth::exchange(&script, &client, &exchange).await.unwrap();
    assert_eq!(
        credentials,
        Credentials {
            access: "acc".into(),
            refresh: "ref".into(),
            expires: NOW + 3_600_000 - auth::EXPIRY_SKEW_MS,
        }
    );
    assert_eq!(script.requests().len(), 1);

    // A response that omits expires_in is treated as the one-hour default.
    assert_eq!(
        auth::expires_at(NOW, None),
        NOW + 3_600_000 - auth::EXPIRY_SKEW_MS
    );
    assert_eq!(
        auth::expires_at(NOW, Some(f64::NAN)),
        NOW + 3_600_000 - auth::EXPIRY_SKEW_MS
    );
    assert_eq!(auth::expires_at(NOW, Some(60.0)), NOW - 240_000);
}

#[tokio::test]
async fn exchange_rejects_responses_without_an_access_token() {
    for (body, expected) in [
        (
            r#"{"refresh_token":"ref","expires_in":60}"#,
            "Token exchange failed: response carries no access_token",
        ),
        (
            "not json",
            "Token exchange failed: expected ident at line 1 column 2",
        ),
    ] {
        let script = Script::ok(body);
        let login = login().use_local_callback(LOCAL);
        let exchange = login
            .complete(&callback_url("the-code", login.state()))
            .unwrap();
        let error = auth::exchange(&script, &ClientConfig::upstream(), &exchange)
            .await
            .unwrap_err();
        assert_eq!(error, expected);
    }
}

#[tokio::test]
async fn refresh_keeps_the_previous_refresh_token_when_the_response_omits_it() {
    let script = Script::ok(r#"{"access_token":"new-access","expires_in":3600}"#);
    let credentials = credentials();
    let refreshed = auth::refresh(&script, &ClientConfig::upstream(), &credentials)
        .await
        .unwrap();

    assert_eq!(refreshed.access, "new-access");
    assert_eq!(refreshed.refresh, "old-refresh");
    assert_eq!(refreshed.expires, NOW + 3_600_000 - auth::EXPIRY_SKEW_MS);
}

#[tokio::test]
async fn refresh_preserves_a_rotated_refresh_token() {
    let script =
        Script::ok(r#"{"access_token":"new","refresh_token":"rotated","expires_in":3600}"#);
    let refreshed = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(refreshed.refresh, "rotated");
}

#[tokio::test]
async fn transient_refresh_failure_rearms_unexpired_credentials() {
    // A dropped connection is transient, exactly like a failed fetch.
    let script = Script::new(vec![Err("connection reset".into())]);
    let grace = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(
        grace,
        Credentials {
            expires: NOW + auth::REFRESH_GRACE_MS,
            ..credentials()
        }
    );
    assert_eq!(script.requests().len(), 1);
    assert!(script.sleeps().is_empty());

    // A 503 is transient too, so the retry policy runs before the grace applies.
    let script = Script::new(vec![
        Ok(reply(503, "busy")),
        Ok(reply(503, "busy")),
        Ok(reply(503, "busy")),
    ]);
    let grace = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(grace.expires, NOW + auth::REFRESH_GRACE_MS);
    assert_eq!(script.requests().len(), 3);
    assert_eq!(script.sleeps(), [5_000, 10_000]);
}

#[tokio::test]
async fn refresh_does_not_grace_expired_credentials_or_client_errors() {
    // An expired credential is never re-armed, so the retry policy runs to the end
    // and the wrapped upstream message is what the caller sees.
    let expired = Credentials {
        expires: NOW - 1,
        ..credentials()
    };
    let script = Script::new(vec![
        Ok(reply(503, "busy")),
        Ok(reply(503, "busy")),
        Ok(reply(503, "busy")),
    ]);
    let error = auth::refresh(&script, &ClientConfig::upstream(), &expired)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "Token refresh failed: Token refresh failed: 503 busy"
    );
    assert_eq!(script.requests().len(), 3);

    let script = Script::new(vec![Ok(reply(400, "invalid_grant"))]);
    let error = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap_err();
    assert_eq!(
        error,
        "Token refresh failed: Token refresh failed: 400 invalid_grant"
    );
    assert_eq!(script.requests().len(), 1);
}

#[tokio::test]
async fn retry_policy_honours_retry_after_and_gives_up_after_two_retries() {
    let script = Script::new(vec![
        Ok(reply_with(429, &[("Retry-After", "1")], "slow down")),
        Ok(reply(200, r#"{"access_token":"acc","expires_in":3600}"#)),
    ]);
    let refreshed = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(refreshed.access, "acc");
    assert_eq!(script.requests().len(), 2);
    assert_eq!(script.sleeps(), [1_000]);

    // Two retries are the ceiling; the last 5xx surfaces as a transient failure,
    // so unexpired credentials are re-armed instead of failing the request.
    let script = Script::new(vec![
        Ok(reply(500, "boom")),
        Ok(reply(500, "boom")),
        Ok(reply(500, "boom")),
    ]);
    let grace = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(grace.expires, NOW + auth::REFRESH_GRACE_MS);
    assert_eq!(script.requests().len(), 3);
    assert_eq!(script.sleeps(), [5_000, 10_000]);
}

#[tokio::test]
async fn retry_policy_stops_on_should_retry_false_and_client_errors() {
    // `x-should-retry: false` suppresses the retry, not the transient grace.
    let script = Script::new(vec![Ok(reply_with(
        500,
        &[("X-Should-Retry", "false")],
        "stop",
    ))]);
    let grace = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(grace.expires, NOW + auth::REFRESH_GRACE_MS);
    assert_eq!(script.requests().len(), 1);
    assert!(script.sleeps().is_empty());

    let script = Script::new(vec![Ok(reply(400, "bad"))]);
    let error = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap_err();
    assert_eq!(error, "Token refresh failed: Token refresh failed: 400 bad");
    assert_eq!(script.requests().len(), 1);

    // A transport failure rejects immediately instead of retrying.
    let script = Script::new(vec![Err("connection reset".into())]);
    let error = auth::refresh(&script, &ClientConfig::upstream(), &credentials())
        .await
        .unwrap();
    assert_eq!(error.expires, NOW + auth::REFRESH_GRACE_MS);
    assert_eq!(script.requests().len(), 1);
}

#[test]
fn retry_after_accepts_seconds_dates_and_clamps() {
    assert_eq!(auth::parse_retry_after(None, NOW), None);
    assert_eq!(auth::parse_retry_after(Some("   "), NOW), None);
    assert_eq!(auth::parse_retry_after(Some("not-a-date"), NOW), None);
    assert_eq!(auth::parse_retry_after(Some("1"), NOW), Some(1_000));
    assert_eq!(auth::parse_retry_after(Some("1.5"), NOW), Some(1_500));
    assert_eq!(auth::parse_retry_after(Some("-4"), NOW), Some(0));
    assert_eq!(auth::parse_retry_after(Some("120"), NOW), Some(30_000));
    assert_eq!(auth::parse_retry_after(Some("Infinity"), NOW), None);
    // 784111777 is the epoch second of the canonical IMF-fixdate example.
    assert_eq!(
        auth::parse_retry_after(
            Some("Sun, 06 Nov 1994 08:49:37 GMT"),
            784_111_777_000 - 5_000
        ),
        Some(5_000)
    );
    assert_eq!(
        auth::parse_retry_after(
            Some("Sun, 06 Nov 1994 08:49:37 GMT"),
            784_111_777_000 + 5_000
        ),
        Some(0)
    );
    assert_eq!(auth::retry_delay_ms(0), 5_000);
    assert_eq!(auth::retry_delay_ms(1), 10_000);
}

#[test]
fn is_oauth_recognises_claude_code_access_tokens() {
    assert!(auth::is_oauth("sk-ant-oat01-abcdef"));
    assert!(auth::is_oauth("prefix-sk-ant-oat-suffix"));
    assert!(!auth::is_oauth("sk-ant-api03-abcdef"));
    assert!(!auth::is_oauth(""));
}

#[test]
fn client_config_reads_runtime_overrides_over_the_compiled_default() {
    let unset = ClientConfig::from_env(|_| None);
    assert_eq!(unset.id, COMPILED_CLIENT_ID);
    assert_eq!(unset.secret, None);

    let overridden = ClientConfig::from_env(|name| match name {
        CLIENT_ID_ENV => Some("runtime-client".into()),
        CLIENT_SECRET_ENV => Some("runtime-secret".into()),
        _ => None,
    });
    assert_eq!(overridden.id, "runtime-client");
    assert_eq!(overridden.secret.as_deref(), Some("runtime-secret"));
    assert!(!format!("{overridden:?}").contains("runtime-secret"));

    let blank = ClientConfig::from_env(|name| match name {
        CLIENT_ID_ENV => Some("  ".into()),
        _ => Some(String::new()),
    });
    assert_eq!(blank.id, COMPILED_CLIENT_ID);
    assert_eq!(blank.secret, None);
}

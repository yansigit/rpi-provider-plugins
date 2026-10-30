//! Port of the Claude Pro/Max OAuth flow. The pin is
//! `pi-anthropic-oauth/src/auth.ts`; this module keeps its wire behaviour — PKCE,
//! the pinned client, the two redirect URIs, the state check, the retry policy and
//! the refresh grace path — and leaves every socket to the caller through [`Port`].
//! A driver supplies that port (and the loopback [`callback`] server) so this crate
//! never opens a connection on its own.
use std::fmt;
use std::future::Future;
use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::{form_urlencoded, Position, Url};

pub mod callback;

/// The client id upstream ships. It is a public PKCE client: the crate holds no
/// secret, and the OAuth endpoints never receive one unless a runtime override sets it.
pub const UPSTREAM_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const CLIENT_ID_ENV: &str = "RPI_ANTHROPIC_OAUTH_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "RPI_ANTHROPIC_OAUTH_CLIENT_SECRET";
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Redirect URI used by the paste flow; the browser never reaches this one directly.
pub const REDIRECT_URI: &str = "https://platform.claude.com/oauth/code/callback";
pub const CALLBACK_HOST: &str = "127.0.0.1";
pub const CALLBACK_PORT: u16 = 53692;
/// Advertised to the browser while the loopback server listens on [`CALLBACK_HOST`].
pub const LOCAL_REDIRECT_URI: &str = "http://localhost:53692/callback";
pub const SCOPES: [&str; 6] = [
    "org:create_api_key",
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];

/// How long the loopback server waits for the browser before the driver falls back
/// to pasted input.
pub const LOCAL_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Budget the driver gives a pasted code to arrive after the loopback wait ends.
pub const MANUAL_SETTLE_GRACE: Duration = Duration::from_secs(10);
pub const MAX_TOKEN_RETRIES: u32 = 2;
pub const INITIAL_RETRY_DELAY_MS: u64 = 5_000;
pub const MAX_RETRY_DELAY_MS: u64 = 30_000;
/// Credentials are reported as expiring this long before the server says they do.
pub const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;
/// On a transient refresh failure the unexpired credentials are re-armed this long.
pub const REFRESH_GRACE_MS: i64 = 30_000;

/// Client id compiled into this crate; a build-time variable can replace upstream's.
pub const COMPILED_CLIENT_ID: &str = match option_env!("RPI_ANTHROPIC_OAUTH_CLIENT_ID") {
    Some(id) => id,
    None => UPSTREAM_CLIENT_ID,
};

/// The OAuth client for one login. Runtime variables override the compiled client id,
/// which is why `upstream()` and `environment()` usually resolve the same value.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientConfig {
    pub id: String,
    /// Only set when a deployment overrides the public client with a confidential one.
    pub secret: Option<String>,
}

impl ClientConfig {
    pub fn upstream() -> Self {
        Self {
            id: COMPILED_CLIENT_ID.into(),
            secret: None,
        }
    }

    /// Reads both client variables; blank values count as unset.
    pub fn environment() -> Self {
        Self::from_env(|name| std::env::var(name).ok())
    }

    pub fn from_env(env: impl Fn(&str) -> Option<String>) -> Self {
        let pick = |name: &str| env(name).filter(|value| !value.trim().is_empty());
        Self {
            id: pick(CLIENT_ID_ENV).unwrap_or_else(|| COMPILED_CLIENT_ID.into()),
            secret: pick(CLIENT_SECRET_ENV),
        }
    }
}

/// The secret never reaches a log through formatting.
impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("id", &self.id)
            .field("secret", &self.secret.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

/// `sk-ant-oat` marks a Claude Code OAuth access token; API keys never carry it.
pub fn is_oauth(api_key: &str) -> bool {
    api_key.contains("sk-ant-oat")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

pub fn generate_pkce() -> Result<Pkce, String> {
    let mut entropy = [0u8; 32];
    getrandom::getrandom(&mut entropy).map_err(|error| error.to_string())?;
    Ok(pkce_from_entropy(&entropy))
}

/// Entropy is injected only at the generation boundary; production uses the OS CSPRNG.
pub fn pkce_from_entropy(entropy: &[u8; 32]) -> Pkce {
    let verifier = URL_SAFE_NO_PAD.encode(entropy);
    let challenge = pkce_challenge(&verifier);
    Pkce {
        verifier,
        challenge,
    }
}

/// S256, as RFC 7636 defines it.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn generate_state() -> Result<String, String> {
    let mut entropy = [0u8; 16];
    getrandom::getrandom(&mut entropy).map_err(|error| error.to_string())?;
    Ok(state_from_entropy(&entropy))
}

/// Upstream hashes `crypto.randomUUID()` without its dashes; the same 32 hex
/// characters come from a version-4 UUID built here.
pub fn state_from_entropy(entropy: &[u8; 16]) -> String {
    let mut bytes = *entropy;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut state = String::with_capacity(32);
    for byte in bytes {
        state.push_str(&format!("{byte:02x}"));
    }
    state
}

pub fn authorize_url(client_id: &str, challenge: &str, state: &str, redirect_uri: &str) -> String {
    let scope = SCOPES.join(" ");
    let mut url = Url::parse(AUTHORIZE_URL).expect("the pinned authorize URL is valid");
    url.query_pairs_mut().extend_pairs([
        ("code", "true"),
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", scope.as_str()),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ]);
    url.into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAuthInput {
    pub code: String,
    pub state: String,
    /// Present only when the input was a full callback URL.
    pub redirect_uri: Option<String>,
}

/// Accepts a full callback URL, a `code#state` pair or a query string. A URL is
/// accepted only for a redirect this login actually initiated, so a callback copied
/// from another client cannot be exchanged here.
pub fn parse_auth_input(
    input: &str,
    initiated_redirect_uris: &[String],
) -> Option<ParsedAuthInput> {
    let text = input.trim();

    if let Ok(url) = Url::parse(text) {
        let param = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .filter(|value| !value.is_empty())
        };
        if let (Some(code), Some(state)) = (param("code"), param("state")) {
            let redirect_uri = url[..Position::AfterPath].to_owned();
            if !initiated_redirect_uris
                .iter()
                .any(|initiated| initiated == &redirect_uri)
            {
                return None;
            }
            return Some(ParsedAuthInput {
                code,
                state,
                redirect_uri: Some(redirect_uri),
            });
        }
    }

    let split: Vec<&str> = text.split('#').collect();
    if split.len() == 2 && !split[0].is_empty() && !split[1].is_empty() {
        return Some(ParsedAuthInput {
            code: split[0].into(),
            state: split[1].into(),
            redirect_uri: None,
        });
    }

    let param = |name: &str| {
        form_urlencoded::parse(text.as_bytes())
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .filter(|value| !value.is_empty())
    };
    match (param("code"), param("state")) {
        (Some(code), Some(state)) => Some(ParsedAuthInput {
            code,
            state,
            redirect_uri: None,
        }),
        _ => None,
    }
}

/// One browser authorization attempt. The driver chooses the redirect (loopback or
/// paste), shows [`Login::authorize_url`], then feeds the result to [`Login::complete`].
#[derive(Debug, Clone)]
pub struct Login {
    client_id: String,
    pkce: Pkce,
    state: String,
    redirect_uri: String,
    initiated: Vec<String>,
}

impl Login {
    pub fn begin(client_id: impl Into<String>) -> Result<Self, String> {
        Ok(Self {
            client_id: client_id.into(),
            pkce: generate_pkce()?,
            state: generate_state()?,
            redirect_uri: REDIRECT_URI.into(),
            initiated: vec![REDIRECT_URI.into()],
        })
    }

    /// Entropy is injected only at the generation boundary: 32 bytes for PKCE, then
    /// 16 for the state.
    pub fn begin_with_entropy(client_id: impl Into<String>, entropy: &[u8; 48]) -> Self {
        let mut pkce_entropy = [0u8; 32];
        pkce_entropy.copy_from_slice(&entropy[..32]);
        let mut state_entropy = [0u8; 16];
        state_entropy.copy_from_slice(&entropy[32..]);
        Self {
            client_id: client_id.into(),
            pkce: pkce_from_entropy(&pkce_entropy),
            state: state_from_entropy(&state_entropy),
            redirect_uri: REDIRECT_URI.into(),
            initiated: vec![REDIRECT_URI.into()],
        }
    }

    /// Switches to the loopback callback the driver bound. Both redirect URIs stay
    /// initiated, because a browser may hand back either after a manual fallback.
    pub fn use_local_callback(mut self, redirect_uri: impl Into<String>) -> Self {
        let redirect_uri = redirect_uri.into();
        if !self
            .initiated
            .iter()
            .any(|initiated| initiated == &redirect_uri)
        {
            self.initiated.push(redirect_uri.clone());
        }
        self.redirect_uri = redirect_uri;
        self
    }

    pub fn authorize_url(&self) -> String {
        authorize_url(
            &self.client_id,
            &self.pkce.challenge,
            &self.state,
            &self.redirect_uri,
        )
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    pub fn verifier(&self) -> &str {
        &self.pkce.verifier
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Parses the callback, enforces the state check and resolves the redirect URI to
    /// present at exchange time. A parsed URL wins over the redirect in use here.
    pub fn complete(&self, input: &str) -> Result<TokenExchange, String> {
        let parsed = parse_auth_input(input, &self.initiated)
            .ok_or_else(|| "Could not parse authorization callback input.".to_string())?;
        if parsed.state != self.state {
            return Err("OAuth state mismatch.".into());
        }
        Ok(TokenExchange {
            code: parsed.code,
            state: parsed.state,
            redirect_uri: parsed
                .redirect_uri
                .unwrap_or_else(|| self.redirect_uri.clone()),
            verifier: self.pkce.verifier.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenExchange {
    pub code: String,
    pub state: String,
    pub redirect_uri: String,
    pub verifier: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl TokenRequest {
    fn json(url: &str, body: String) -> Self {
        Self {
            url: url.into(),
            method: "POST".into(),
            headers: vec![("Content-Type".into(), "application/json".into())],
            body,
        }
    }
}

#[derive(Serialize)]
struct ExchangeBody<'a> {
    grant_type: &'static str,
    client_id: &'a str,
    code: &'a str,
    state: &'a str,
    redirect_uri: &'a str,
    code_verifier: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<&'a str>,
}

#[derive(Serialize)]
struct RefreshBody<'a> {
    grant_type: &'static str,
    client_id: &'a str,
    refresh_token: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_secret: Option<&'a str>,
}

impl TokenExchange {
    pub fn request(&self, client: &ClientConfig) -> TokenRequest {
        let body = serde_json::to_string(&ExchangeBody {
            grant_type: "authorization_code",
            client_id: &client.id,
            code: &self.code,
            state: &self.state,
            redirect_uri: &self.redirect_uri,
            code_verifier: &self.verifier,
            client_secret: client.secret.as_deref(),
        })
        .expect("the exchange body serializes");
        TokenRequest::json(TOKEN_URL, body)
    }
}

pub fn refresh_request(client: &ClientConfig, refresh_token: &str) -> TokenRequest {
    let body = serde_json::to_string(&RefreshBody {
        grant_type: "refresh_token",
        client_id: &client.id,
        refresh_token,
        client_secret: client.secret.as_deref(),
    })
    .expect("the refresh body serializes");
    TokenRequest::json(TOKEN_URL, body)
}

/// What Pi stores for an OAuth provider, with `expires` already skewed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Credentials {
    pub access: String,
    pub refresh: String,
    pub expires: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenReply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl TokenReply {
    /// Header names are case-insensitive on the wire.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Outbound token HTTP, supplied by the driver. `now_ms` and `sleep` are injected
/// with it so expiry, retry delays and the refresh grace share one clock.
pub trait Port: Sync {
    fn send(
        &self,
        request: TokenRequest,
    ) -> impl Future<Output = Result<TokenReply, String>> + Send;
    fn sleep(&self, delay_ms: u64) -> impl Future<Output = ()> + Send;
    fn now_ms(&self) -> i64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenFailure {
    message: String,
    status: Option<u16>,
    transport: bool,
}

impl TokenFailure {
    fn transport(message: String) -> Self {
        Self {
            message,
            status: None,
            transport: true,
        }
    }

    fn http(status: u16, message: String) -> Self {
        Self {
            message,
            status: Some(status),
            transport: false,
        }
    }

    /// A dropped connection is transient, as is 429 and any 5xx.
    fn is_transient(&self) -> bool {
        self.transport || self.is_retryable_status()
    }

    fn is_retryable_status(&self) -> bool {
        self.status
            .is_some_and(|status| status == 429 || (500..600).contains(&status))
    }
}

async fn fetch_with_retry<P: Port + ?Sized>(
    port: &P,
    request: &TokenRequest,
    label: &str,
) -> Result<TokenReply, TokenFailure> {
    let mut last_error = None;

    for attempt in 0..=MAX_TOKEN_RETRIES {
        // A transport failure rejects at once, exactly like a failed fetch.
        let reply = port
            .send(request.clone())
            .await
            .map_err(|error| TokenFailure::transport(format!("{label} failed: {error}")))?;

        if (200..300).contains(&reply.status) {
            return Ok(reply);
        }

        let failure = TokenFailure::http(
            reply.status,
            format!("{label} failed: {} {}", reply.status, reply.body),
        );

        if reply.header("x-should-retry") == Some("false") {
            return Err(failure);
        }

        if attempt < MAX_TOKEN_RETRIES && failure.is_retryable_status() {
            let delay = parse_retry_after(reply.header("retry-after"), port.now_ms())
                .unwrap_or_else(|| retry_delay_ms(attempt));
            port.sleep(delay).await;
            last_error = Some(failure);
            continue;
        }

        return Err(failure);
    }

    Err(last_error
        .unwrap_or_else(|| TokenFailure::transport(format!("{label} failed after retries"))))
}

/// Exponential backoff base used when the server sends no usable `Retry-After`.
pub fn retry_delay_ms(attempt: u32) -> u64 {
    INITIAL_RETRY_DELAY_MS * 2u64.pow(attempt)
}

/// `Retry-After` is delta-seconds or an HTTP date; both are clamped to
/// [`MAX_RETRY_DELAY_MS`] and floored at zero.
pub fn parse_retry_after(value: Option<&str>, now_ms: i64) -> Option<u64> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }

    if let Ok(seconds) = value.parse::<f64>() {
        if seconds.is_finite() {
            return Some(((seconds.max(0.0) * 1000.0) as u64).min(MAX_RETRY_DELAY_MS));
        }
    }

    let at = parse_imf_fixdate(value)?;
    Some(((at - now_ms).max(0) as u64).min(MAX_RETRY_DELAY_MS))
}

/// Parses `Sun, 06 Nov 1994 08:49:37 GMT`, the only date form HTTP servers send.
fn parse_imf_fixdate(value: &str) -> Option<i64> {
    let rest = value.strip_suffix(" GMT")?;
    let mut fields = rest.split(' ');
    let _weekday = fields.next()?;
    let day: u32 = fields.next()?.parse().ok()?;
    let month = match fields.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i32 = fields.next()?.parse().ok()?;
    let mut clock = fields.next()?.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if fields.next().is_some()
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(seconds * 1000)
}

/// Days since the Unix epoch for a proleptic Gregorian date.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year } as i64;
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = ((month + 9) % 12) as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `expires` is the server's lifetime minus the skew Pi reserves; a response that
/// omits `expires_in` is treated as the upstream one-hour default.
pub fn expires_at(now_ms: i64, expires_in: Option<f64>) -> i64 {
    let seconds = match expires_in {
        Some(seconds) if seconds.is_finite() => seconds,
        _ => 3600.0,
    };
    now_ms + (seconds * 1000.0) as i64 - EXPIRY_SKEW_MS
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    access_token: String,
    #[serde(default)]
    refresh_token: String,
    #[serde(default)]
    expires_in: Option<f64>,
}

fn credentials_from(
    label: &str,
    body: &str,
    now_ms: i64,
    previous_refresh: Option<&str>,
) -> Result<Credentials, String> {
    let tokens: TokenResponse =
        serde_json::from_str(body).map_err(|error| format!("{label} failed: {error}"))?;
    if tokens.access_token.is_empty() {
        return Err(format!("{label} failed: response carries no access_token"));
    }
    let refresh = if tokens.refresh_token.is_empty() {
        previous_refresh.unwrap_or_default().to_owned()
    } else {
        tokens.refresh_token
    };
    Ok(Credentials {
        access: tokens.access_token,
        refresh,
        expires: expires_at(now_ms, tokens.expires_in),
    })
}

pub async fn exchange<P: Port + ?Sized>(
    port: &P,
    client: &ClientConfig,
    exchange: &TokenExchange,
) -> Result<Credentials, String> {
    let reply = fetch_with_retry(port, &exchange.request(client), "Token exchange")
        .await
        .map_err(|failure| failure.message)?;
    credentials_from("Token exchange", &reply.body, port.now_ms(), None)
}

pub async fn refresh<P: Port + ?Sized>(
    port: &P,
    client: &ClientConfig,
    credentials: &Credentials,
) -> Result<Credentials, String> {
    let reply = match fetch_with_retry(
        port,
        &refresh_request(client, &credentials.refresh),
        "Token refresh",
    )
    .await
    {
        Ok(reply) => reply,
        Err(failure) => {
            // An unexpired credential survives a transient outage on a short leash.
            if failure.is_transient() && credentials.expires > port.now_ms() {
                return Ok(Credentials {
                    access: credentials.access.clone(),
                    refresh: credentials.refresh.clone(),
                    expires: port.now_ms() + REFRESH_GRACE_MS,
                });
            }
            // Upstream wraps the retry message, so the label repeats here.
            return Err(format!("Token refresh failed: {}", failure.message));
        }
    };
    credentials_from(
        "Token refresh",
        &reply.body,
        port.now_ms(),
        Some(&credentials.refresh),
    )
    .map_err(|error| format!("Token refresh failed: {error}"))
}

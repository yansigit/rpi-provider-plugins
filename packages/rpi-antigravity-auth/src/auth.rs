//! Pi OAuth and packed credentials. The caller owns persistence and custody admission.
use crate::json::{describe, finite_number, string, trim, truthy};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::future::Future;
use url::Url;

pub mod json_error;
pub mod network;

/// Build-time and runtime variable names for the Antigravity OAuth client. The package
/// ships no client identity: builds and runtimes must supply both or OAuth fails closed.
pub const CLIENT_ID_ENV: &str = "RPI_ANTIGRAVITY_OAUTH_CLIENT_ID";
pub const CLIENT_SECRET_ENV: &str = "RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET";
pub const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v1/userinfo?alt=json";
pub const LOAD_ENDPOINTS: [&str; 2] = [
    "https://daily-cloudcode-pa.googleapis.com",
    "https://cloudcode-pa.googleapis.com",
];
pub const CUSTODY_REFRESH_PREFIX: &str = "claustrum-tombstone:v1:";
pub const CUSTODY_KEY_MARKER: &str = "antigravity-vault-custody";
pub const EXCHANGE_REFUSED: &str = "Antigravity sign-in is no longer authorized.";
const SCOPES: [&str; 5] = [
    "https://www.googleapis.com/auth/cloud-platform",
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
    "https://www.googleapis.com/auth/cclog",
    "https://www.googleapis.com/auth/experimentsandconfigs",
];

/// Client values captured by `option_env!` when this crate is compiled.
#[derive(Clone, Copy)]
pub struct BuildClient {
    pub id: Option<&'static str>,
    pub secret: Option<&'static str>,
}
impl BuildClient {
    pub const COMPILED: BuildClient = BuildClient {
        id: option_env!("RPI_ANTIGRAVITY_OAUTH_CLIENT_ID"),
        secret: option_env!("RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET"),
    };
    pub const NONE: BuildClient = BuildClient {
        id: None,
        secret: None,
    };
}

/// Where the OAuth client comes from. Production uses `environment()`; tests inject
/// explicit values so their results never depend on how the crate was built.
#[derive(Clone, Copy)]
pub struct ClientSource {
    pub build: BuildClient,
    pub runtime: fn(&str) -> Option<String>,
}
impl ClientSource {
    pub fn environment() -> Self {
        Self {
            build: BuildClient::COMPILED,
            runtime: runtime_variable,
        }
    }
    pub const fn configured(id: &'static str, secret: &'static str) -> Self {
        Self {
            build: BuildClient {
                id: Some(id),
                secret: Some(secret),
            },
            runtime: no_runtime_variable,
        }
    }
    /// Build-time values win over runtime variables; empty or blank values count as unset.
    pub fn resolve(self) -> Result<OAuthClient, String> {
        let pick = |build: Option<&str>, name: &str| {
            build
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .or_else(|| (self.runtime)(name).filter(|value| !value.trim().is_empty()))
        };
        match (
            pick(self.build.id, CLIENT_ID_ENV),
            pick(self.build.secret, CLIENT_SECRET_ENV),
        ) {
            (Some(id), Some(secret)) => Ok(OAuthClient { id, secret }),
            _ => Err(format!(
                "Antigravity OAuth is not configured: set {CLIENT_ID_ENV} and {CLIENT_SECRET_ENV} when building rpi-antigravity-auth, or export both in the runtime environment."
            )),
        }
    }
}
fn runtime_variable(name: &str) -> Option<String> {
    std::env::var(name).ok()
}
fn no_runtime_variable(_name: &str) -> Option<String> {
    None
}

/// A resolved OAuth client. Only `ClientSource::resolve` constructs one, and it is
/// deliberately not `Debug`, so the secret cannot reach a log through formatting.
pub struct OAuthClient {
    id: String,
    secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RefreshParts {
    pub refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_project_id: Option<String>,
}
pub fn parse_refresh(value: &str) -> RefreshParts {
    let mut parts = value.split('|');
    RefreshParts {
        refresh_token: parts.next().unwrap_or_default().into(),
        project_id: parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        managed_project_id: parts
            .next()
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    }
}
pub fn format_refresh(parts: &RefreshParts) -> String {
    let base = format!(
        "{}|{}",
        parts.refresh_token,
        parts.project_id.as_deref().unwrap_or_default()
    );
    match parts
        .managed_project_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some(managed) => format!("{base}|{managed}"),
        None => base,
    }
}
pub fn expires_at(request_ms: f64, seconds: &Value) -> f64 {
    let seconds = seconds.as_f64().unwrap_or(3600.0);
    if seconds.is_nan() || seconds <= 0.0 {
        request_ms
    } else {
        request_ms + seconds * 1000.0
    }
}
pub fn access_expired(auth: &Value, now_ms: f64) -> bool {
    !truthy(&auth["access"])
        || auth["expires"]
            .as_f64()
            .map_or(true, |expires| expires <= now_ms + 60_000.0)
}

#[derive(Default)]
pub struct CredentialCache(IndexMap<String, String>);
impl CredentialCache {
    pub fn remember(&mut self, access: &str, refresh: &str) {
        if access.is_empty() {
            return;
        }
        if self.0.shift_remove(access).is_none() && self.0.len() >= 4 {
            self.0.shift_remove_index(0);
        }
        self.0.insert(access.into(), refresh.into());
    }
    pub fn get(&self, access: &str) -> Option<&str> {
        self.0.get(access).map(String::as_str)
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Authorization {
    pub url: String,
    pub verifier: String,
    pub project_id: String,
}
pub fn authorize(project: &str, source: ClientSource) -> Result<Authorization, String> {
    let mut entropy = [0; 32];
    getrandom::getrandom(&mut entropy).map_err(|error| error.to_string())?;
    authorize_with_entropy(project, &entropy, source)
}
/// Entropy is injected only at the generation boundary; production uses the OS CSPRNG.
/// Fails before building any URL when the client is not configured.
pub fn authorize_with_entropy(
    project: &str,
    entropy: &[u8; 32],
    source: ClientSource,
) -> Result<Authorization, String> {
    let client = source.resolve()?;
    let verifier = URL_SAFE_NO_PAD.encode(entropy);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(describe(&json!({"verifier":verifier,"projectId":project})));
    let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", client.id.as_str()),
        ("response_type", "code"),
        ("redirect_uri", REDIRECT_URI),
        ("scope", &SCOPES.join(" ")),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("state", &state),
        ("access_type", "offline"),
        ("prompt", "consent"),
    ]);
    Ok(Authorization {
        url: url.into(),
        verifier,
        project_id: project.into(),
    })
}
pub fn callback_input(input: &str, authorization_url: &str) -> (String, String) {
    let mut code = trim(input).to_owned();
    let mut state = Url::parse(authorization_url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find(|(key, _)| key == "state")
                .map(|(_, value)| value.into_owned())
        })
        .unwrap_or_default();
    if let Ok(url) = Url::parse(&code) {
        let get = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .filter(|value| !value.is_empty())
        };
        if let Some(value) = get("code") {
            code = value;
        }
        if let Some(value) = get("state") {
            state = value;
        }
    }
    (code, state)
}

fn decode_state(state: &str) -> Result<(String, String), String> {
    // Buffer.from(base64) ignores non-alphabet bytes and an incomplete final sextet.
    let mut sextets =
        state
            .bytes()
            .take_while(|byte| *byte != b'=')
            .filter_map(|byte| match byte {
                b'A'..=b'Z' => Some(byte - b'A'),
                b'a'..=b'z' => Some(byte - b'a' + 26),
                b'0'..=b'9' => Some(byte - b'0' + 52),
                b'+' | b'-' => Some(62),
                b'/' | b'_' => Some(63),
                _ => None,
            });
    let mut bytes = Vec::new();
    while let (Some(a), Some(b)) = (sextets.next(), sextets.next()) {
        bytes.push(a << 2 | b >> 4);
        let Some(c) = sextets.next() else {
            break;
        };
        bytes.push(b << 4 | c >> 2);
        let Some(d) = sextets.next() else {
            break;
        };
        bytes.push(c << 6 | d);
    }
    let json = String::from_utf8_lossy(&bytes);
    let parsed: Value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
    if parsed.is_null() {
        return Err("Cannot read properties of null (reading 'verifier')".into());
    }
    let verifier = parsed["verifier"]
        .as_str()
        .ok_or("Missing PKCE verifier in state")?;
    Ok((
        verifier.into(),
        parsed["projectId"].as_str().unwrap_or_default().into(),
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub agy: bool,
    pub timeout_ms: u64,
}
pub trait Body: Send {
    fn text(self) -> impl Future<Output = Result<String, String>> + Send;
}
pub struct Reply<B> {
    pub status: u16,
    pub status_text: String,
    pub body: B,
}
impl<B> Reply<B> {
    pub(crate) fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}
pub trait Port: Sync {
    type Body: Body;
    fn send(
        &self,
        request: Request,
    ) -> impl Future<Output = Result<Reply<Self::Body>, String>> + Send;
    fn now_ms(&self) -> f64;
    fn user_agent(&self) -> String;
}
pub trait Admission: Sync {
    fn check(&self) -> impl Future<Output = Result<(), String>> + Send;
    fn cancelled(&self) -> bool;
}
async fn admit(gate: &impl Admission) -> Result<(), String> {
    if gate.cancelled() {
        return Err(EXCHANGE_REFUSED.into());
    }
    gate.check()
        .await
        .map_err(|_| EXCHANGE_REFUSED.to_owned())?;
    if gate.cancelled() {
        return Err(EXCHANGE_REFUSED.into());
    }
    Ok(())
}
fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs.iter().copied())
        .finish()
}
fn prop(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map(string)
        .unwrap_or_else(|| "undefined".into())
}
pub(crate) async fn json_body(body: impl Body) -> Result<Value, String> {
    let text = body.text().await?;
    serde_json::from_str(&text)
        .map_err(|error| json_error::parse_error(&text).unwrap_or_else(|| error.to_string()))
}
pub fn bootstrap_headers(access: &str, user_agent: String) -> Vec<(String, String)> {
    vec![
        ("User-Agent".into(), user_agent),
        ("Authorization".into(), format!("Bearer {access}")),
        ("Content-Type".into(), "application/json".into()),
        ("Accept-Encoding".into(), "gzip".into()),
    ]
}

pub async fn refresh(
    port: &impl Port,
    source: ClientSource,
    packed: &str,
) -> Result<Value, String> {
    if packed.starts_with(CUSTODY_REFRESH_PREFIX) {
        return Err("Pi holds the Antigravity vault placeholder, not a login. Run /login, or serve Antigravity from the vault.".into());
    }
    let client = source.resolve()?;
    let (refresh, suffix) = packed
        .find('|')
        .map_or((packed, ""), |index| (&packed[..index], &packed[index..]));
    let started = port.now_ms();
    let response = port
        .send(Request {
            url: TOKEN_URL.into(),
            method: "POST".into(),
            agy: false,
            timeout_ms: 15_000,
            headers: vec![(
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            )],
            body: Some(form(&[
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
                ("client_id", client.id.as_str()),
                ("client_secret", client.secret.as_str()),
            ])),
        })
        .await?;
    if !response.ok() {
        let text = response.body.text().await.unwrap_or_default();
        return Err(format!(
            "Antigravity token refresh failed ({} {}){}",
            response.status,
            response.status_text,
            if text.is_empty() {
                String::new()
            } else {
                format!(" - {text}")
            }
        ));
    }
    let payload = json_body(response.body).await?;
    let rotated = payload
        .get("refresh_token")
        .filter(|value| !value.is_null())
        .map(string)
        .unwrap_or_else(|| refresh.into());
    let mut result = json!({"refresh":format!("{rotated}{suffix}")});
    if let Some(access) = payload.get("access_token") {
        result["access"] = access.clone();
    }
    result["expires"] = finite_number(expires_at(started, &payload["expires_in"]));
    Ok(result)
}

pub async fn exchange(
    port: &impl Port,
    gate: &impl Admission,
    source: ClientSource,
    code: &str,
    state: &str,
) -> Value {
    match exchange_inner(port, gate, source, code, state).await {
        Ok(value) => value,
        Err(error) => {
            json!({"type":"failed","error":if gate.cancelled() { EXCHANGE_REFUSED.to_owned() } else { error }})
        }
    }
}
async fn exchange_inner(
    port: &impl Port,
    gate: &impl Admission,
    source: ClientSource,
    code: &str,
    state: &str,
) -> Result<Value, String> {
    let client = source.resolve()?;
    let (verifier, project) = decode_state(state)?;
    admit(gate).await?;
    let started = port.now_ms();
    let response = port
        .send(Request {
            url: TOKEN_URL.into(),
            method: "POST".into(),
            agy: false,
            timeout_ms: 15_000,
            headers: vec![
                (
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded;charset=UTF-8".into(),
                ),
                ("Accept".into(), "*/*".into()),
                ("Accept-Encoding".into(), "gzip, deflate, br".into()),
                ("User-Agent".into(), port.user_agent()),
            ],
            body: Some(form(&[
                ("client_id", client.id.as_str()),
                ("client_secret", client.secret.as_str()),
                ("code", code),
                ("grant_type", "authorization_code"),
                ("redirect_uri", REDIRECT_URI),
                ("code_verifier", &verifier),
            ])),
        })
        .await?;
    admit(gate).await?;
    if !response.ok() {
        let error = response.body.text().await?;
        admit(gate).await?;
        return Ok(json!({"type":"failed","error":error}));
    }
    let tokens = json_body(response.body).await?;
    admit(gate).await?;
    let response = port
        .send(Request {
            url: USERINFO_URL.into(),
            method: "GET".into(),
            agy: false,
            timeout_ms: 15_000,
            headers: vec![
                (
                    "Authorization".into(),
                    format!("Bearer {}", prop(&tokens, "access_token")),
                ),
                ("User-Agent".into(), port.user_agent()),
            ],
            body: None,
        })
        .await?;
    admit(gate).await?;
    let user = if response.ok() {
        json_body(response.body).await?
    } else {
        json!({})
    };
    admit(gate).await?;
    if !truthy(&tokens["refresh_token"]) {
        return Ok(json!({"type":"failed","error":"Missing refresh token in response"}));
    }
    let project = if project.is_empty() {
        let project = discover_login_project(port, gate, &prop(&tokens, "access_token")).await?;
        admit(gate).await?;
        project
    } else {
        project
    };
    let mut result =
        json!({"type":"success","refresh":format!("{}|{project}",prop(&tokens,"refresh_token"))});
    if let Some(access) = tokens.get("access_token") {
        result["access"] = access.clone();
    }
    result["expires"] = finite_number(expires_at(started, &tokens["expires_in"]));
    if let Some(email) = user.get("email") {
        result["email"] = email.clone();
    }
    if let Some(name) = user.get("name").filter(|value| !value.is_null()) {
        let name = trim(
            name.as_str()
                .ok_or("userInfo.name?.trim is not a function")?,
        );
        if !name.is_empty() {
            result["label"] = name.into();
        }
    }
    result["projectId"] = project.into();
    Ok(result)
}
async fn discover_login_project(
    port: &impl Port,
    gate: &impl Admission,
    access: &str,
) -> Result<String, String> {
    for endpoint in LOAD_ENDPOINTS {
        admit(gate).await?;
        let attempt = async {
            let response = port
                .send(Request {
                    url: format!("{endpoint}/v1internal:loadCodeAssist"),
                    method: "POST".into(),
                    agy: true,
                    timeout_ms: 10_000,
                    headers: bootstrap_headers(access, port.user_agent()),
                    body: Some("{\"metadata\":{\"ideType\":\"ANTIGRAVITY\"}}".into()),
                })
                .await?;
            admit(gate).await?;
            if !response.ok() {
                let _ = response.body.text().await;
                admit(gate).await?;
                return Ok(None);
            }
            let payload = json_body(response.body).await?;
            admit(gate).await?;
            Ok::<_, String>(
                payload["cloudaicompanionProject"]
                    .as_str()
                    .or_else(|| payload["cloudaicompanionProject"]["id"].as_str())
                    .filter(|project| !project.is_empty())
                    .map(str::to_owned),
            )
        }
        .await;
        match attempt {
            Ok(Some(project)) => return Ok(project),
            Ok(None) => {}
            Err(error) if error == EXCHANGE_REFUSED => return Err(error),
            Err(_) => {
                admit(gate).await?;
            }
        }
    }
    Ok(String::new())
}

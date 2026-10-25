//! `/multilogin`'s provider login through the host's `auth` port.
//!
//! Upstream calls the provider's login and receives the credential, then
//! stores it with `addAccount` / `replaceAccountCredential`. The host's login
//! instead persists the credential into a store account before it reports
//! success, and a new pool account has no record yet. The login therefore
//! targets a private pending account of this plugin's credential store
//! ([`super::credentials::PendingLogin`]); the credential captured there is the
//! login's result, which the command flow commits exactly as upstream does.
use super::{credentials::CREDENTIAL_STORE_ID, Shared};
use crate::commands::port::{AuthMethod, LoginInteraction, CANCELLED};
use rpi_plugin_sdk::provider::native::Client;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// One host-reported provider authentication method (`auth` `methods`).
#[derive(Clone, Debug, PartialEq)]
pub struct HostMethod {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub login_label: Option<String>,
    pub interactive_login: Option<bool>,
}

/// Parses the host's method list. Unknown kinds are skipped, malformed entries
/// refuse the whole list.
pub fn parse_methods(value: &Value) -> Result<Vec<HostMethod>, String> {
    let entries = value
        .as_array()
        .ok_or("invalid provider authentication methods")?;
    let mut methods = Vec::new();
    for entry in entries {
        let text = |key: &str| entry[key].as_str().map(str::to_owned);
        let (Some(id), Some(kind), Some(name)) = (text("id"), text("kind"), text("name")) else {
            return Err("invalid provider authentication methods".into());
        };
        if kind != "oauth" && kind != "api_key" {
            continue;
        }
        methods.push(HostMethod {
            id,
            kind,
            name,
            login_label: text("loginLabel").filter(|label| !label.trim().is_empty()),
            interactive_login: entry["interactiveLogin"].as_bool(),
        });
    }
    Ok(methods)
}

/// `provider.auth.oauth` / `provider.auth.apiKey` as the flows read them. An
/// OAuth method always has a login; an API-key method has one unless the host
/// reports `interactiveLogin: false` (Pi's `api_key_paste`). Older hosts omit
/// the flag, which keeps the provider's own login.
pub fn presentation(methods: &[HostMethod]) -> (Option<AuthMethod>, Option<AuthMethod>) {
    let pick = |kind: &str| methods.iter().find(|method| method.kind == kind);
    let oauth = pick("oauth").map(|method| AuthMethod {
        name: method.name.clone(),
        login_label: method.login_label.clone(),
        login: true,
    });
    let api_key = pick("api_key").map(|method| AuthMethod {
        name: method.name.clone(),
        login_label: method.login_label.clone(),
        login: method.interactive_login != Some(false),
    });
    (oauth, api_key)
}

/// The method id the host logs in with for `auth_type` (`oauth`/`api_key`).
pub fn method_id(methods: &[HostMethod], auth_type: &str) -> Option<String> {
    methods
        .iter()
        .find(|method| method.kind == auth_type)
        .map(|method| method.id.clone())
}

pub async fn methods(
    client: &Client,
    signal: &CancellationToken,
    provider: &str,
) -> Result<Vec<HostMethod>, String> {
    let value = client
        .request(
            json!({"port":"auth","action":"methods","providerId":provider}),
            signal.clone(),
        )
        .await?;
    parse_methods(&value)
}

/// Drops the reservation on every exit, so a failed or cancelled login never
/// leaves a pending account behind.
struct Reservation<'a> {
    shared: &'a Shared,
    id: String,
}
impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.shared.take_pending_login(&self.id);
    }
}

/// Runs the host login for `method_id` into a private pending account and
/// returns the credential it persisted. A cancelled presentation cancels the
/// host login and reports [`CANCELLED`].
pub async fn login<I: LoginInteraction>(
    shared: &Shared,
    client: &Client,
    signal: &CancellationToken,
    provider: &str,
    method_id: &str,
    interaction: &I,
) -> Result<Value, String> {
    let reservation = Reservation {
        id: shared.reserve_pending_login(provider)?,
        shared,
    };
    let mut response = client
        .request(
            json!({"port":"auth","action":"login","providerId":provider,"methodId":method_id,
                "storeId":CREDENTIAL_STORE_ID,"accountId":reservation.id}),
            signal.clone(),
        )
        .await?;
    let mut waiting: Option<String> = None;
    loop {
        match response["kind"].as_str() {
            Some("success") => {
                return shared
                    .take_pending_login(&reservation.id)
                    .ok_or_else(|| "multiprovider: login completed without a credential".into());
            }
            Some("cancelled") => return Err(CANCELLED.into()),
            Some("interaction") => {
                let login_id = response["loginId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or("multiprovider: invalid login response")?
                    .to_owned();
                let value = match present(
                    &response["interaction"],
                    interaction,
                    signal,
                    &mut waiting,
                )
                .await
                {
                    Ok(value) => value,
                    Err(error) => {
                        // The host keeps an unanswered login until it is
                        // cancelled; the command's own token may already be.
                        let _ = client
                            .request(
                                json!({"port":"auth","action":"cancelLogin",
                                        "providerId":provider,"loginId":login_id}),
                                CancellationToken::new(),
                            )
                            .await;
                        return Err(error);
                    }
                };
                response = client
                    .request(
                        json!({"port":"auth","action":"continue","providerId":provider,
                            "loginId":login_id,"value":value}),
                        signal.clone(),
                    )
                    .await?;
            }
            _ => return Err("multiprovider: invalid login response".into()),
        }
    }
}

/// One host interaction through Pi's login dialog events and prompts.
async fn present<I: LoginInteraction>(
    interaction: &Value,
    dialog: &I,
    signal: &CancellationToken,
    waiting: &mut Option<String>,
) -> Result<Value, String> {
    let title = interaction["title"].as_str().unwrap_or("");
    let message = interaction["message"]
        .as_str()
        .filter(|text| !text.trim().is_empty());
    match interaction["kind"].as_str() {
        Some("input") => {
            match interaction["url"].as_str().filter(|url| !url.is_empty()) {
                Some(url) => dialog
                    .notify(
                        json!({"type":"auth_url","url":url,"instructions":message.unwrap_or("")}),
                    )
                    .await?,
                None => {
                    if let Some(message) = message {
                        dialog
                            .notify(json!({"type":"info","message":message}))
                            .await?;
                    }
                }
            }
            let kind = if interaction["secret"] == true {
                "secret"
            } else {
                "text"
            };
            let allow_empty = interaction["allowEmpty"] == true;
            loop {
                let value = dialog.prompt(json!({"type":kind,"message":title})).await?;
                if allow_empty || !value.is_empty() {
                    return Ok(json!(value));
                }
                dialog
                    .notify(json!({"type":"info","message":"A value is required."}))
                    .await?;
            }
        }
        Some("select") => {
            let options: Vec<Value> = interaction["choices"]
                .as_array()
                .ok_or("multiprovider: invalid login choices")?
                .iter()
                .map(|choice| json!({"id":choice["id"],"label":choice["label"]}))
                .collect();
            let chosen = dialog
                .prompt(json!({"type":"select","message":title,"options":options}))
                .await?;
            if chosen.is_empty() {
                return Err(CANCELLED.into());
            }
            Ok(json!(chosen))
        }
        Some("wait") => {
            if let Some(message) = message {
                if waiting.as_deref() != Some(message) {
                    dialog
                        .notify(json!({"type":"info","message":message}))
                        .await?;
                    *waiting = Some(message.to_owned());
                }
            }
            let delay = interaction["pollAfterMs"].as_u64().unwrap_or(1000);
            tokio::select! {
                biased;
                _ = signal.cancelled() => Err(CANCELLED.into()),
                _ = tokio::time::sleep(std::time::Duration::from_millis(delay)) => Ok(Value::Null),
            }
        }
        _ => Err("multiprovider: unsupported login interaction".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_methods_become_pi_auth_presentation() {
        let methods = parse_methods(&json!([
            {"id":"device","kind":"oauth","name":"Sign in","loginLabel":"Sign in with Acme"},
            {"id":"ambient","kind":"api_key","name":"Acme key","interactiveLogin":false},
            {"id":"other","kind":"session","name":"ignored"}
        ]))
        .unwrap();
        assert_eq!(methods.len(), 2);
        let (oauth, api_key) = presentation(&methods);
        let oauth = oauth.unwrap();
        assert!(oauth.login);
        assert_eq!(oauth.login_label.as_deref(), Some("Sign in with Acme"));
        let api_key = api_key.unwrap();
        assert!(
            !api_key.login,
            "interactiveLogin:false is Pi's api_key_paste"
        );
        assert_eq!(api_key.name, "Acme key");
        assert_eq!(method_id(&methods, "oauth").as_deref(), Some("device"));
        assert_eq!(method_id(&methods, "api_key").as_deref(), Some("ambient"));
    }

    #[test]
    fn older_hosts_without_presentation_keep_the_provider_login() {
        let methods = parse_methods(
            &json!([{"id":"key","kind":"api_key","name":"API key","loginLabel":"  "}]),
        )
        .unwrap();
        let (oauth, api_key) = presentation(&methods);
        assert!(oauth.is_none());
        let api_key = api_key.unwrap();
        assert!(api_key.login);
        assert_eq!(api_key.login_label, None, "a blank label is absent");
    }

    #[test]
    fn malformed_method_lists_are_refused() {
        assert!(parse_methods(&json!({"id":"key"})).is_err());
        assert!(parse_methods(&json!([{"kind":"oauth","name":"x"}])).is_err());
    }
}

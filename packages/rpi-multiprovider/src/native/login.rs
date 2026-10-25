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

/// One host login request, reporting [`CANCELLED`] as soon as the command's
/// token fires. The request carries that token, so it is aborted on the host
/// side at the same moment; the caller cancels the host login itself.
async fn request(
    client: &Client,
    signal: &CancellationToken,
    body: Value,
) -> Result<Value, String> {
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(CANCELLED.into()),
        response = client.request(body, signal.clone()) => response,
    }
}

/// Cancels one host login on a fresh token: the command's own token may be the
/// reason the login is ending, and a cancelled token would abort this request
/// before it reaches the host.
async fn cancel_login(client: &Client, provider: &str, login_id: &str) {
    let _ = client
        .request(
            json!({"port":"auth","action":"cancelLogin","providerId":provider,
                "loginId":login_id}),
            CancellationToken::new(),
        )
        .await;
}

/// Runs the host login for `method_id` into a private pending account and
/// returns the credential it persisted. A cancelled presentation, or a command
/// token that fires while the host holds the login, cancels the host login and
/// reports [`CANCELLED`].
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
    let mut response = match request(
        client,
        signal,
        json!({"port":"auth","action":"login","providerId":provider,"methodId":method_id,
            "storeId":CREDENTIAL_STORE_ID,"accountId":reservation.id}),
    )
    .await
    {
        Ok(value) => value,
        Err(_) if signal.is_cancelled() => return Err(CANCELLED.into()),
        Err(error) => return Err(error),
    };
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
                        // cancelled.
                        cancel_login(client, provider, &login_id).await;
                        return Err(error);
                    }
                };
                response = match request(
                    client,
                    signal,
                    json!({"port":"auth","action":"continue","providerId":provider,
                        "loginId":login_id,"value":value}),
                )
                .await
                {
                    Ok(value) => value,
                    Err(_) if signal.is_cancelled() => {
                        cancel_login(client, provider, &login_id).await;
                        return Err(CANCELLED.into());
                    }
                    Err(error) => {
                        // The host keeps the ticket of a continuation it
                        // refused, so it is cancelled before the error shows.
                        cancel_login(client, provider, &login_id).await;
                        return Err(error);
                    }
                };
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
                .map(
                    |choice| match (choice["id"].as_str(), choice["label"].as_str()) {
                        (Some(id), Some(label)) if !id.is_empty() => {
                            Ok(json!({"id":id,"label":label}))
                        }
                        _ => Err("multiprovider: invalid login choices"),
                    },
                )
                .collect::<Result<Vec<Value>, _>>()?;
            let chosen = dialog
                .prompt(json!({"type":"select","message":title,"options":options}))
                .await?;
            // A dialog cancel already arrives as [`CANCELLED`] from the prompt;
            // an empty answer to a resolved list means the provider's choices
            // had no id, which the host reports as a resolve error.
            if chosen.is_empty() {
                return Err("multiprovider: invalid login choices".into());
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

    /// Answers prompts from a fixed script and records what the dialog saw.
    struct Scripted {
        replies: std::sync::Mutex<std::collections::VecDeque<Result<String, String>>>,
        events: std::sync::Mutex<Vec<Value>>,
        prompts: std::sync::Mutex<Vec<Value>>,
    }
    impl Scripted {
        fn replying(replies: Vec<Result<String, String>>) -> Self {
            Self {
                replies: std::sync::Mutex::new(replies.into()),
                events: std::sync::Mutex::new(Vec::new()),
                prompts: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    impl LoginInteraction for Scripted {
        fn notify(
            &self,
            event: Value,
        ) -> impl std::future::Future<Output = Result<(), String>> + Send {
            async move {
                self.events.lock().unwrap().push(event);
                Ok(())
            }
        }
        fn prompt(
            &self,
            prompt: Value,
        ) -> impl std::future::Future<Output = Result<String, String>> + Send {
            async move {
                self.prompts.lock().unwrap().push(prompt);
                self.replies
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(Err(CANCELLED.into()))
            }
        }
    }

    fn blocked_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn select_interactions_need_choice_ids() {
        let dialog = Scripted::replying(Vec::new());
        let mut waiting = None;
        let error = blocked_on(present(
            &json!({"kind":"select","title":"Pick","choices":[{"label":"Alpha"}]}),
            &dialog,
            &CancellationToken::new(),
            &mut waiting,
        ))
        .unwrap_err();
        assert_eq!(error, "multiprovider: invalid login choices");
        assert!(
            dialog.prompts.lock().unwrap().is_empty(),
            "an unresolvable choice list never prompts"
        );
    }

    #[test]
    fn unresolved_select_answers_are_errors_not_cancellations() {
        let dialog = Scripted::replying(vec![Ok(String::new())]);
        let mut waiting = None;
        let error = blocked_on(present(
            &json!({"kind":"select","title":"Pick","choices":[
                {"id":"ws-a","label":"Alpha"},{"id":"ws-b","label":"Beta"}]}),
            &dialog,
            &CancellationToken::new(),
            &mut waiting,
        ))
        .unwrap_err();
        assert_ne!(error, CANCELLED, "the host reports a resolve error");
        assert_eq!(error, "multiprovider: invalid login choices");
        let prompt = dialog.prompts.lock().unwrap()[0].clone();
        assert_eq!(prompt["type"], "select");
        assert_eq!(prompt["options"][1]["id"], "ws-b");
    }

    #[test]
    fn wait_interactions_report_once_and_stop_on_cancellation() {
        let dialog = Scripted::replying(Vec::new());
        let signal = CancellationToken::new();
        let mut waiting = None;
        let wait = json!({"kind":"wait","message":"Approve the request","pollAfterMs":5});
        for _ in 0..2 {
            let value = blocked_on(present(&wait, &dialog, &signal, &mut waiting)).unwrap();
            assert_eq!(value, Value::Null);
        }
        assert_eq!(
            dialog.events.lock().unwrap().clone(),
            vec![json!({"type":"info","message":"Approve the request"})],
            "a repeated wait message is reported once"
        );
        signal.cancel();
        let error = blocked_on(present(&wait, &dialog, &signal, &mut waiting)).unwrap_err();
        assert_eq!(error, CANCELLED);
    }
}

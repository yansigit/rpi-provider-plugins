//! Multiprovider command flow: `/multilogin` (upstream `extensions/multiprovider.ts`
//! `multilogin` handler, `src/multilogin.ts` selector, login dialog and API-key
//! prompt). The pool manager screen is reached through `Port::pool_manager`.
use super::port::{
    input, notify, select_control, AuthStatus, LoginInteraction, PoolAction, PoolBuffer,
    PoolMethod, Port, ProviderInfo, CANCELLED,
};
use super::Ui;
use crate::types::trim;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::future::Future;

/// `LoginDialogResult`: `None` when the user cancels, otherwise the credential
/// or the error message.
pub type LoginResult = Option<Result<Value, String>>;

#[derive(Clone)]
struct LoginOption {
    id: String,
    name: String,
    auth_type: &'static str,
    status: Option<(&'static str, Option<String>)>,
}

pub struct Selection {
    pub provider: ProviderInfo,
    pub auth_type: &'static str,
}

fn collate(left: &str, right: &str) -> Ordering {
    match rpi_node_collation::collator(&rpi_node_collation::default_locale()) {
        Ok(collator) => collator.compare(left, right),
        Err(_) => left.cmp(right),
    }
}

fn format_type(auth_type: &str) -> &'static str {
    if auth_type == "api_key" {
        "API key"
    } else {
        "subscription"
    }
}

/// `^[A-Z][A-Z0-9_]*(?:, [A-Z][A-Z0-9_]*)*$`
fn is_env_list(source: &str) -> bool {
    source.split(", ").all(|part| {
        let mut chars = part.chars();
        matches!(chars.next(), Some('A'..='Z'))
            && chars.all(|c| matches!(c, 'A'..='Z' | '0'..='9' | '_'))
    })
}

fn status_text(option: &LoginOption) -> String {
    match &option.status {
        None => " • not configured".into(),
        Some((status_type, _)) if *status_type != option.auth_type => {
            format!(" • {} configured", format_type(status_type))
        }
        Some((_, None)) => " ✓ configured".into(),
        Some((_, Some(source)))
            if source.is_empty() || source == "OAuth" || source == "stored credential" =>
        {
            " ✓ configured".into()
        }
        Some((_, Some(source))) if is_env_list(source) => format!(" ✓ env: {source}"),
        Some((_, Some(source))) => format!(" ✓ {source}"),
    }
}

fn option_label(option: &LoginOption, show_type: bool) -> String {
    let kind = if show_type {
        format!(" [{}]", format_type(option.auth_type))
    } else {
        String::new()
    };
    format!("{}{}{}", option.name, kind, status_text(option))
}

fn login_options(
    providers: &[ProviderInfo],
    status_for: impl Fn(&str) -> Option<(&'static str, Option<String>)>,
) -> Vec<LoginOption> {
    let mut options = Vec::new();
    for provider in providers {
        let status = status_for(&provider.id);
        for (auth_type, method) in [("oauth", &provider.oauth), ("api_key", &provider.api_key)] {
            if method.is_some() {
                options.push(LoginOption {
                    id: provider.id.clone(),
                    name: provider.name.clone(),
                    auth_type,
                    status: status.clone(),
                });
            }
        }
    }
    options.sort_by(|left, right| collate(&left.name, &right.name));
    options
}

/// `selectLogin`: the provider and auth type to log in with.
pub async fn select_login<U: Ui, P: Port>(
    ui: &U,
    port: &P,
    providers: Vec<ProviderInfo>,
    provider_ref: Option<&str>,
) -> Result<Option<Selection>, String> {
    let normalized = provider_ref.map(|value| trim(value).to_lowercase());
    let runtime = port.runtime();
    let all = match &runtime {
        Some(runtime) => runtime.providers.clone(),
        None => providers,
    };
    let scoped: Vec<ProviderInfo> = match normalized.as_deref() {
        None | Some("") => all.clone(),
        Some(wanted) => all
            .iter()
            .filter(|provider| {
                provider.id.to_lowercase() == wanted || provider.name.to_lowercase() == wanted
            })
            .cloned()
            .collect(),
    };
    let options = login_options(&scoped, |id| {
        let runtime = runtime.as_ref()?;
        let status: AuthStatus = runtime.status(id);
        if !status.configured {
            return None;
        }
        let source = status.label.or(status.source);
        let status_type = if runtime.is_using_oauth(id) {
            "oauth"
        } else {
            "api_key"
        };
        Some((status_type, source))
    });
    if options.is_empty() {
        let text = match provider_ref {
            None => "No providers with API-key or OAuth authentication are available.".to_owned(),
            Some(reference) => format!(
                "No API-key or OAuth authentication method is available for \"{}\".",
                reference.trim()
            ),
        };
        notify(ui, &text, "warning").await?;
        return Ok(None);
    }
    if options.len() == 1 {
        let option = &options[0];
        let provider = scoped
            .iter()
            .find(|provider| provider.id == option.id)
            .cloned();
        return Ok(provider.map(|provider| Selection {
            provider,
            auth_type: option.auth_type,
        }));
    }

    let show_type = options
        .iter()
        .any(|option| option.auth_type != options[0].auth_type);
    let choices: Vec<Value> = options
        .iter()
        .enumerate()
        .map(|(index, option)| json!({"label": option_label(option, show_type), "value": index.to_string()}))
        .collect();
    let mut control = json!({
        "kind": "select",
        "title": "Select provider to configure:",
        "searchable": true,
        "choices": choices,
    });
    if let Some(query) = normalized.as_deref().filter(|query| !query.is_empty()) {
        control["query"] = json!(query);
    }
    let Some(index) = select_control(ui, control, options.len()).await? else {
        return Ok(None);
    };
    let option = &options[index];
    Ok(all
        .iter()
        .find(|provider| provider.id == option.id)
        .cloned()
        .map(|provider| Selection {
            provider,
            auth_type: option.auth_type,
        }))
}

/// Pi's login dialog as native controls. Notifications carry the dialog title
/// and ` · ` separators; each prompt is one control titled `<title> · <message>`.
pub struct LoginDialog<'a, U: Ui> {
    ui: &'a U,
    title: String,
}

fn event_texts(title: &str, event: &Value) -> Vec<String> {
    match event["type"].as_str() {
        Some("auth_url") => {
            let instructions = event["instructions"]
                .as_str()
                .filter(|text| !text.is_empty());
            let mut parts = vec![
                title.to_owned(),
                format!("Open {}", event["url"].as_str().unwrap_or("")),
            ];
            parts.extend(instructions.map(str::to_owned));
            vec![parts.join(" · ")]
        }
        Some("device_code") => vec![
            format!(
                "{title} · Open {} and enter code {}",
                event["verificationUri"].as_str().unwrap_or(""),
                event["userCode"].as_str().unwrap_or("")
            ),
            format!("{title} · Waiting for authentication..."),
        ],
        Some("info") => {
            let mut parts = vec![
                title.to_owned(),
                event["message"].as_str().unwrap_or("").to_owned(),
            ];
            for link in event["links"].as_array().into_iter().flatten() {
                let url = link["url"].as_str().unwrap_or("");
                parts.push(
                    match link["label"].as_str().filter(|label| !label.is_empty()) {
                        Some(label) => format!("{label}: {url}"),
                        None => url.to_owned(),
                    },
                );
            }
            vec![parts.join(" · ")]
        }
        _ => vec![format!(
            "{title} · {}",
            event["message"].as_str().unwrap_or("")
        )],
    }
}

impl<'a, U: Ui> LoginDialog<'a, U> {
    pub fn new(ui: &'a U, title: &str) -> Self {
        Self {
            ui,
            title: title.to_owned(),
        }
    }
}

impl<U: Ui> LoginInteraction for LoginDialog<'_, U> {
    fn notify(&self, event: Value) -> impl Future<Output = Result<(), String>> + Send {
        async move {
            for text in event_texts(&self.title, &event) {
                notify(self.ui, &text, "info").await?;
            }
            Ok(())
        }
    }

    fn prompt(&self, prompt: Value) -> impl Future<Output = Result<String, String>> + Send {
        async move {
            let message = prompt["message"].as_str().unwrap_or("");
            let title = format!("{} · {message}", self.title);
            if prompt["type"] == "select" {
                let options = prompt["options"].as_array().cloned().unwrap_or_default();
                let labels: Vec<String> = options
                    .iter()
                    .map(|option| option["label"].as_str().unwrap_or("").to_owned())
                    .collect();
                let choices: Vec<Value> = labels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| json!({"label": label, "value": index.to_string()}))
                    .collect();
                let control = json!({"kind":"select","title":title,"choices":choices});
                let Some(index) = select_control(self.ui, control, labels.len()).await? else {
                    return Err(CANCELLED.into());
                };
                // The dialog resolves the first option whose label matches the choice.
                let chosen = options
                    .iter()
                    .find(|option| option["label"].as_str() == Some(labels[index].as_str()))
                    .and_then(|option| option["id"].as_str())
                    .unwrap_or("");
                return Ok(chosen.to_owned());
            }
            let (placeholder, masked) = match prompt["type"].as_str() {
                Some("manual_code") => (String::new(), false),
                _ => (
                    prompt["placeholder"].as_str().unwrap_or("").to_owned(),
                    prompt["type"] == "secret",
                ),
            };
            match input(self.ui, &title, &placeholder, masked).await? {
                Some(value) => Ok(value),
                None => Err(CANCELLED.into()),
            }
        }
    }
}

/// `loginCredential` through the dialog host (`showLoginDialog`).
pub async fn show_login_dialog<U: Ui, P: Port>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    auth_type: &str,
    title: Option<&str>,
) -> Result<LoginResult, String> {
    let method = if auth_type == "oauth" {
        &provider.oauth
    } else {
        &provider.api_key
    };
    if !method.as_ref().is_some_and(|method| method.login) {
        return Ok(Some(Err(format!(
            "No {auth_type} login method for {}",
            provider.name
        ))));
    }
    let dialog = LoginDialog {
        ui,
        title: title
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Add {} account", provider.name)),
    };
    match port.login(provider, auth_type, &dialog).await {
        Ok(credential) => Ok(Some(Ok(credential))),
        Err(message) if message == CANCELLED => Ok(None),
        Err(message) => Ok(Some(Err(message))),
    }
}

/// `promptApiKeyCredential`: the pasted key for a provider without a login.
pub async fn prompt_api_key_credential<U: Ui>(
    ui: &U,
    provider: &ProviderInfo,
) -> Result<LoginResult, String> {
    let Some(method) = &provider.api_key else {
        return Ok(Some(Err(format!(
            "No API-key authentication for {}",
            provider.name
        ))));
    };
    let Some(value) = input(ui, &format!("Enter {}:", method.name), "", false).await? else {
        return Ok(None);
    };
    let key = trim(&value);
    if key.is_empty() {
        return Ok(Some(Err("API key is required".into())));
    }
    Ok(Some(Ok(json!({"type":"api_key","key":key}))))
}

async fn run_login<U: Ui, P: Port>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    method: &str,
    title: Option<&str>,
) -> Result<LoginResult, String> {
    if method == "api_key_paste" {
        prompt_api_key_credential(ui, provider).await
    } else {
        show_login_dialog(ui, port, provider, method, title).await
    }
}

fn pool_methods(provider: &ProviderInfo) -> Vec<PoolMethod> {
    let mut methods = Vec::new();
    if let Some(oauth) = provider.oauth.as_ref().filter(|method| method.login) {
        methods.push(PoolMethod {
            label: oauth
                .login_label
                .clone()
                .unwrap_or_else(|| oauth.name.clone()),
            value: "oauth".into(),
        });
    }
    if let Some(api_key) = &provider.api_key {
        let base = if api_key.name == "API key" {
            "API key".to_owned()
        } else {
            format!("API key · {}", api_key.name)
        };
        let interactive = api_key.login;
        methods.push(PoolMethod {
            label: if interactive {
                base
            } else {
                format!("{base} (paste)")
            },
            value: if interactive {
                "api_key"
            } else {
                "api_key_paste"
            }
            .into(),
        });
    }
    methods
}

async fn pool_of<P: Port>(port: &P, provider_id: &str) -> Result<Value, String> {
    port.store(json!({"op":"pool","provider":provider_id}))
        .await
}

fn accounts_of(pool: &Value) -> Vec<Value> {
    pool["accounts"].as_array().cloned().unwrap_or_default()
}

async fn reauthenticate<U: Ui, P: Port>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    account_id: &str,
    method: &str,
) -> Result<(), String> {
    let pool = pool_of(port, &provider.id).await?;
    let Some(account) = accounts_of(&pool)
        .into_iter()
        .find(|account| account["id"] == account_id)
    else {
        return notify(ui, "That account is no longer stored.", "warning").await;
    };
    let label = account["label"].as_str().unwrap_or("").to_owned();
    let login = run_login(
        ui,
        port,
        provider,
        method,
        Some(&format!("Reauthenticate {label}")),
    )
    .await?;
    match login {
        None => Ok(()),
        Some(Err(message)) => {
            notify(
                ui,
                &format!("Failed to reauthenticate {label}: {message}"),
                "error",
            )
            .await
        }
        Some(Ok(credential)) => {
            let saved = async {
                port.store(json!({"op":"replace","provider":provider.id,"account":account_id,"credential":credential}))
                    .await?;
                if port.service_has_provider(&provider.id) {
                    port.reset_health(&provider.id, account_id)?;
                }
                port.reconcile().await
            }
            .await;
            match saved {
                Ok(()) => {
                    notify(
                        ui,
                        &format!(
                            "Reauthenticated {label} for {}. Credentials saved to {}",
                            provider.name,
                            port.store_path()
                        ),
                        "info",
                    )
                    .await
                }
                Err(message) => {
                    notify(
                        ui,
                        &format!("Could not replace account credentials: {message}"),
                        "error",
                    )
                    .await
                }
            }
        }
    }
}

async fn add_account<U: Ui, P: Port>(
    ui: &U,
    port: &P,
    provider: &ProviderInfo,
    method: &str,
    buffer: &PoolBuffer,
) -> Result<(), String> {
    let existing = pool_of(port, &provider.id).await?;
    let default_label = format!("{} {}", provider.name, accounts_of(&existing).len() + 1);
    let Some(entered) = input(ui, "Account label:", &default_label, false).await? else {
        return Ok(());
    };
    let trimmed = trim(&entered);
    let label = if trimmed.is_empty() {
        default_label
    } else {
        trimmed.to_owned()
    };
    let login = run_login(ui, port, provider, method, None).await?;
    match login {
        None => Ok(()),
        Some(Err(message)) => {
            notify(
                ui,
                &format!("Failed to authenticate {}: {message}", provider.name),
                "error",
            )
            .await
        }
        Some(Ok(credential)) => {
            let saved = async {
                let mut request = json!({"label": label, "credential": credential});
                if pool_of(port, &provider.id).await?.is_null() {
                    request["pool"] = json!({
                        "policy": buffer.policy,
                        "affinity": buffer.affinity,
                        "includeUpstream": buffer.include_upstream,
                        "upstream": buffer.upstream,
                    });
                }
                port.store(json!({"op":"add","provider":provider.id,"input":request}))
                    .await?;
                port.reconcile().await
            }
            .await;
            match saved {
                Ok(()) => {
                    notify(
                        ui,
                        &format!(
                            "Added {label} to {}. Credentials saved to {}",
                            provider.name,
                            port.store_path()
                        ),
                        "info",
                    )
                    .await
                }
                Err(message) => {
                    notify(ui, &format!("Could not save account: {message}"), "error").await
                }
            }
        }
    }
}

pub async fn multilogin<U: Ui, P: Port>(ui: &U, port: &P, args: &str) -> Result<(), String> {
    if !ui.has_ui() || port.mode() != "tui" {
        return notify(ui, "/multilogin requires Pi interactive mode.", "warning").await;
    }
    port.reconcile().await?;
    let mut providers = port.registered_providers();
    providers.retain(|provider| !port.is_virtual_provider(&provider.id));
    providers.sort_by(|left, right| collate(&left.name, &right.name));
    let reference = trim(args);
    let selection = select_login(
        ui,
        port,
        providers,
        (!reference.is_empty()).then_some(reference),
    )
    .await?;
    let Some(selection) = selection else {
        return Ok(());
    };
    let provider = selection.provider;
    let initial = pool_of(port, &provider.id).await?;
    let mut buffer = PoolBuffer {
        policy: initial["policy"]
            .as_str()
            .unwrap_or("round-robin")
            .to_owned(),
        affinity: initial
            .get("affinity")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        include_upstream: initial
            .get("includeUpstream")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        upstream: match initial.get("upstream") {
            Some(upstream) if upstream.is_object() => upstream.clone(),
            _ => json!({}),
        },
    };
    let methods = pool_methods(&provider);
    let mut action = port.pool_manager(&provider, &methods, &mut buffer).await?;
    loop {
        match action {
            PoolAction::Reauth { account_id, method } => {
                reauthenticate(ui, port, &provider, &account_id, &method).await?;
            }
            PoolAction::Add { method } => {
                add_account(ui, port, &provider, &method, &buffer).await?
            }
            PoolAction::Closed => break,
        }
        action = port.pool_manager(&provider, &methods, &mut buffer).await?;
    }
    Ok(())
}

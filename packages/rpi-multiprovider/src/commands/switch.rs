//! Multiprovider command flow: `/switch-account` (upstream `extensions/multiprovider.ts`
//! `switch-account` handler and its label helpers).
use super::port::{locale_time, notify, select, Port, SelectedModel};
use super::Ui;
use crate::types::trim;
use serde_json::{json, Value};

/// `PI_UPSTREAM_ACCOUNT_ID`: the account that stands for Pi's own credential.
const UPSTREAM_ACCOUNT_ID: &str = "pi:default";
const SESSION_PIN_ENTRY: &str = "pi-multiprovider:switch-account";

fn switch_label(account: &Value, current: bool) -> String {
    let id = account["id"].as_str().unwrap_or("");
    let kind = if id == UPSTREAM_ACCOUNT_ID {
        "upstream".to_owned()
    } else {
        account["authKind"].as_str().unwrap_or("").to_owned()
    };
    let status = match account.get("cooldownUntil").and_then(Value::as_f64) {
        Some(until) if account["status"] == "cooldown" => {
            format!("cooldown until {}", locale_time(until))
        }
        _ => account["status"].as_str().unwrap_or("").to_owned(),
    };
    let mut parts = vec![
        format!("{} ({kind})", account["label"].as_str().unwrap_or("")),
        status,
        format!(
            "w{} · p{}",
            super::port::js_number(&account["weight"]),
            super::port::js_number(&account["priority"])
        ),
    ];
    if current {
        parts.push("current".into());
    }
    parts.join(" · ")
}

/// Labels for the picker; repeated labels gain the first eight characters of
/// the account id so each choice is distinguishable.
fn switch_labels(accounts: &[Value], current: Option<&str>) -> Vec<String> {
    let labels: Vec<String> = accounts
        .iter()
        .map(|account| switch_label(account, current.is_some_and(|id| account["id"] == id)))
        .collect();
    labels
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let repeats = labels
                .iter()
                .filter(|candidate| *candidate == label)
                .count();
            if repeats > 1 {
                let id: String = accounts[index]["id"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(8)
                    .collect();
                format!("{label} · {id}")
            } else {
                label.clone()
            }
        })
        .collect()
}

pub async fn switch_account<U: Ui, P: Port>(ui: &U, port: &P, args: &str) -> Result<(), String> {
    if !ui.has_ui() {
        return notify(
            ui,
            "/switch-account requires Pi interactive mode.",
            "warning",
        )
        .await;
    }
    port.reconcile().await?;
    let Some(model) = port.model() else {
        return notify(ui, "No model is selected.", "info").await;
    };
    let SelectedModel {
        provider: provider_id,
        id: model_id,
        name: model_name,
    } = model;
    let virtual_pool = port.is_virtual_pool(&provider_id, &model_id);
    let pool_id = if virtual_pool {
        format!("{provider_id}::{model_id}")
    } else {
        provider_id.clone()
    };
    let provider_name = if virtual_pool {
        format!(
            "{} · {model_name}",
            port.virtual_provider_name(&provider_id)
                .unwrap_or_else(|| provider_id.clone())
        )
    } else {
        port.provider_name(&provider_id)
            .unwrap_or_else(|| provider_id.clone())
    };
    let integrated = virtual_pool || port.has_integration(&provider_id);
    let pool = if integrated {
        let snapshot = port.snapshot().await?;
        snapshot["providers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|candidate| candidate["id"] == pool_id.as_str())
            .cloned()
    } else {
        None
    };
    let accounts: Vec<Value> = pool
        .as_ref()
        .and_then(|pool| pool["accounts"].as_array().cloned())
        .unwrap_or_default();
    let Some(pool) = pool.filter(|_| integrated && !accounts.is_empty()) else {
        return notify(
            ui,
            &format!("{provider_name} has no pooled accounts. Use /multilogin to add one."),
            "info",
        )
        .await;
    };

    // Pi has no credential for its own account until one is configured, so
    // pinning it would never apply.
    let upstream_configured = port
        .runtime()
        .map(|runtime| runtime.status(&provider_id).configured)
        .unwrap_or(true);
    let switchable: Vec<Value> = accounts
        .into_iter()
        .filter(|account| account["id"] != UPSTREAM_ACCOUNT_ID || upstream_configured)
        .collect();
    if switchable.is_empty() {
        return notify(
            ui,
            &format!("No switchable accounts for {provider_name}. Use /multilogin to add one."),
            "info",
        )
        .await;
    }

    let affinity_key = port.session_id();
    let pin = port.affinity(&pool_id, &affinity_key);
    let pool_affinity = pool["affinity"] == true;
    let current_id = pin
        .as_ref()
        .filter(|(_, explicit)| pool_affinity || *explicit)
        .map(|(account_id, _)| account_id.clone());

    let reference = trim(args);
    let mut automatic = false;
    let mut chosen: Option<Value> = None;
    if !reference.is_empty() {
        let normalized = reference.to_lowercase();
        let mut matches: Vec<&Value> = switchable
            .iter()
            .filter(|account| account["label"].as_str().unwrap_or("").to_lowercase() == normalized)
            .collect();
        if matches.is_empty() {
            matches = switchable
                .iter()
                .filter(|account| {
                    account["label"]
                        .as_str()
                        .unwrap_or("")
                        .to_lowercase()
                        .starts_with(&normalized)
                })
                .collect();
        }
        if matches.len() == 1 {
            chosen = Some(matches[0].clone());
        } else if matches.is_empty() && (normalized == "auto" || normalized == "automatic") {
            automatic = true;
        } else if matches.len() > 1 {
            notify(
                ui,
                &format!("Multiple accounts match \"{reference}\". Pick one below."),
                "warning",
            )
            .await?;
        } else {
            notify(
                ui,
                &format!("No pooled account for {provider_name} matches \"{reference}\". Pick one below."),
                "warning",
            )
            .await?;
        }
    }

    if !automatic && chosen.is_none() {
        let mut labels = vec![format!(
            "Automatic · let the {} strategy pick the next account",
            pool["policy"].as_str().unwrap_or("")
        )];
        labels.extend(switch_labels(&switchable, current_id.as_deref()));
        let title = format!("Switch {provider_name} account:");
        let Some(index) = select(ui, &title, &labels).await? else {
            return Ok(());
        };
        if index == 0 {
            automatic = true;
        } else {
            chosen = Some(switchable[index - 1].clone());
        }
    }

    if automatic {
        port.clear_affinity(&pool_id, &affinity_key);
        port.append_entry(
            SESSION_PIN_ENTRY,
            json!({"pool": pool_id, "key": affinity_key}),
        )
        .await?;
        port.announce_active_account(&pool_id).await?;
        let text = if pool_affinity {
            "Cleared this session's pinned account. The next request re-selects using the pool strategy."
        } else {
            "Selection for this session is already automatic."
        };
        return notify(ui, text, "info").await;
    }

    let Some(account) = chosen else {
        return Ok(());
    };
    let label = account["label"].as_str().unwrap_or("").to_owned();
    if account["enabled"] != true {
        return notify(
            ui,
            &format!("Account \"{label}\" is disabled. Enable it in /multilogin first."),
            "error",
        )
        .await;
    }
    let account_id = account["id"].as_str().unwrap_or("").to_owned();
    if let Err(message) = port.pin_account(&pool_id, &affinity_key, &account_id).await {
        return notify(ui, &format!("Could not switch account: {message}"), "error").await;
    }
    port.append_entry(
        SESSION_PIN_ENTRY,
        json!({"pool": pool_id, "key": affinity_key, "accountId": account_id, "label": label}),
    )
    .await?;
    port.announce_active_account(&pool_id).await?;
    let cooldown = match account.get("cooldownUntil").and_then(Value::as_f64) {
        Some(until) => format!(
            " It cools down until {}; other accounts serve until it recovers.",
            locale_time(until)
        ),
        None => String::new(),
    };
    notify(
        ui,
        &format!(
            "Switched to {label} for this session. Pool settings are unchanged; new requests from this session use it.{cooldown}"
        ),
        "info",
    )
    .await
}

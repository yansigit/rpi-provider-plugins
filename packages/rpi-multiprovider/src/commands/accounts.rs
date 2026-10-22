//! Multiprovider command flow: `/accounts` (upstream `extensions/multiprovider.ts`
//! `accounts` handler and `statusLines`).
use super::port::{js_number, locale_time, notify, select, Port};
use super::Ui;
use serde_json::Value;

pub async fn accounts<U: Ui, P: Port>(ui: &U, port: &P) -> Result<(), String> {
    port.reconcile().await?;
    let snapshot = port.snapshot().await?;
    let providers = snapshot["providers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if providers.is_empty() {
        return notify(
            ui,
            "No account pools are configured. Use /multilogin to add one.",
            "info",
        )
        .await;
    }
    let lines = status_lines(&providers);
    select(ui, "Provider Accounts", &lines).await?;
    Ok(())
}

/// The status lines shown by `/accounts`, one header per provider followed by
/// its accounts (or `no accounts`).
pub fn status_lines(providers: &[Value]) -> Vec<String> {
    let mut lines = Vec::new();
    for provider in providers {
        let main_first = if provider["firstAccountBias"] == true {
            " · main-first"
        } else {
            ""
        };
        let affinity = if provider["affinity"] == true {
            "on"
        } else {
            "off"
        };
        lines.push(format!(
            "{} ({}) · {}{} · affinity {affinity}",
            text(&provider["label"]),
            text(&provider["id"]),
            text(&provider["policy"]),
            main_first
        ));
        let accounts = provider["accounts"].as_array().cloned().unwrap_or_default();
        if accounts.is_empty() {
            lines.push("  no accounts".into());
            continue;
        }
        for account in &accounts {
            let cooldown = match account.get("cooldownUntil").and_then(Value::as_f64) {
                Some(until) => format!(" · cooldown until {}", locale_time(until)),
                None => String::new(),
            };
            let model_cooldowns: String = account["modelCooldowns"]
                .as_object()
                .into_iter()
                .flatten()
                .map(|(model, until)| {
                    format!(
                        " · {model} cooldown until {}",
                        locale_time(until.as_f64().unwrap_or(f64::NAN))
                    )
                })
                .collect();
            lines.push(format!(
                "  {} ({}) · {} · w{} · p{} · {} in flight · {} failures{cooldown}{model_cooldowns}",
                text(&account["label"]),
                text(&account["authKind"]),
                text(&account["status"]),
                js_number(&account["weight"]),
                js_number(&account["priority"]),
                js_number(&account["inFlight"]),
                js_number(&account["consecutiveFailures"]),
            ));
        }
    }
    lines
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or("").to_owned()
}

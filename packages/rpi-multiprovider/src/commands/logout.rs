//! Multiprovider command flow: `/multilogout` (upstream `extensions/multiprovider.ts`
//! `multilogout` handler).
use super::port::{confirm, notify, select, LogoutPort};
use super::Ui;
use crate::types::trim;
use serde_json::{json, Value};

pub async fn multilogout<U: Ui, P: LogoutPort>(ui: &U, port: &P, args: &str) -> Result<(), String> {
    if !ui.has_ui() || port.mode() != "tui" {
        return notify(ui, "/multilogout requires Pi interactive mode.", "warning").await;
    }
    let mut pools = Vec::new();
    let ids = port.store(json!({"op":"list"})).await?;
    for id in ids
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let pool = port.store(json!({"op":"pool","provider":id})).await?;
        if !pool.is_null() {
            pools.push(pool);
        }
    }
    if pools.is_empty() {
        return notify(ui, "No multilogin accounts are stored.", "info").await;
    }
    let reference = trim(args).to_lowercase();
    let mut pool = if reference.is_empty() {
        None
    } else {
        pools
            .iter()
            .find(|candidate| {
                candidate["providerId"]
                    .as_str()
                    .is_some_and(|id| id.to_lowercase() == reference)
            })
            .cloned()
    };
    if pool.is_none() {
        let mut labels = Vec::with_capacity(pools.len());
        for candidate in &pools {
            let id = candidate["providerId"].as_str().unwrap_or("");
            let name = port
                .installed_provider_name(id)
                .await?
                .unwrap_or_else(|| id.to_owned());
            labels.push(format!("{name} ({})", accounts(candidate).len()));
        }
        let Some(index) = select(ui, "Select provider to remove an account from:", &labels).await?
        else {
            return Ok(());
        };
        pool = pools.get(index).cloned();
    }
    let Some(pool) = pool else {
        return Ok(());
    };
    let provider_id = pool["providerId"].as_str().unwrap_or("").to_owned();
    let stored = accounts(&pool);
    let labels: Vec<String> = stored
        .iter()
        .map(|account| {
            format!(
                "{} · {}",
                account["label"].as_str().unwrap_or(""),
                account["authKind"].as_str().unwrap_or("")
            )
        })
        .collect();
    let Some(index) = select(ui, "Select account to remove:", &labels).await? else {
        return Ok(());
    };
    let Some(account) = stored.get(index) else {
        return Ok(());
    };
    let label = account["label"].as_str().unwrap_or("").to_owned();
    let confirmed = confirm(
        ui,
        "Remove pooled account?",
        &format!("Remove {label} from {provider_id}? Pi's normal /login credential is unchanged."),
    )
    .await?;
    if !confirmed {
        return Ok(());
    }
    let account_id = account["id"].as_str().unwrap_or("").to_owned();
    port.store(json!({"op":"remove","provider":provider_id,"account":account_id}))
        .await?;
    port.reconcile().await?;
    notify(ui, &format!("Removed {label} from {provider_id}."), "info").await
}

fn accounts(pool: &Value) -> Vec<Value> {
    pool["accounts"].as_array().cloned().unwrap_or_default()
}

//! Native presentation of the pinned Pi menu walk; only sealed payloads reach the UI.
use super::{Menu, Port};
use serde_json::{json, Value};
use std::{collections::HashSet, future::Future};
pub trait Ui {
    fn select(
        &self,
        title: String,
        labels: Vec<String>,
    ) -> impl Future<Output = Result<Option<usize>, String>> + Send;
    fn confirm(
        &self,
        title: String,
        message: String,
    ) -> impl Future<Output = Result<bool, String>> + Send;
    fn notify(&self, text: &str, kind: &str) -> impl Future<Output = Result<(), String>> + Send;
}
fn title(heading: &str, lines: Vec<String>) -> String {
    if lines.is_empty() {
        heading.into()
    } else {
        format!("{heading}\n{}", lines.join("\n"))
    }
}
fn row(item: &Value) -> String {
    let label = item["label"].as_str().unwrap();
    match item["status"].as_str().filter(|v| !v.is_empty()) {
        Some(status) => format!("{label} · {status}"),
        None => label.into(),
    }
}
async fn pick<U: Ui>(ui: &U, title: String, labels: Vec<String>) -> Result<Option<usize>, String> {
    let mut seen = HashSet::new();
    let count = labels.len();
    let labels = labels
        .into_iter()
        .map(|label| {
            let mut unique = label.clone();
            let mut n = 2;
            while seen.contains(&unique) {
                unique = format!("{label} ({n})");
                n += 1;
            }
            seen.insert(unique.clone());
            unique
        })
        .collect();
    match ui.select(title, labels).await? {
        Some(index) if index >= count => Err("Invalid menu selection".into()),
        selected => Ok(selected),
    }
}
pub async fn run<P: Port, U: Ui>(menu: &Menu<P>, ui: &U) -> Result<(), String> {
    let mut model = menu.open().await["menu"].clone();
    loop {
        let sections = model["sections"].as_array().unwrap();
        let labels = sections
            .iter()
            .map(|section| {
                let heading = section["title"].as_str().unwrap();
                match section["lines"][0].as_str().filter(|v| !v.is_empty()) {
                    Some(line) => format!("{heading}: {line}"),
                    None => heading.into(),
                }
            })
            .collect();
        let Some(selected) = pick(ui, model["title"].as_str().unwrap().into(), labels).await?
        else {
            return Ok(());
        };
        let selected = sections[selected]["id"].clone();
        loop {
            let Some(section) = model["sections"]
                .as_array()
                .unwrap()
                .iter()
                .find(|s| s["id"] == selected)
            else {
                break;
            };
            let mut entries: Vec<(String, Option<&Value>, Option<&Value>)> = section["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| !item["actions"].as_array().unwrap().is_empty())
                .map(|item| (row(item), Some(item), None))
                .collect();
            entries.extend(
                section["actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|action| (action["label"].as_str().unwrap().into(), None, Some(action))),
            );
            let mut text: Vec<String> = section["lines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().into())
                .collect();
            for item in section["items"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["actions"].as_array().unwrap().is_empty())
            {
                text.push(row(item));
            }
            if entries.is_empty() && text.is_empty() {
                ui.notify("Nothing to do here.", "info").await?;
                break;
            }
            let mut labels: Vec<_> = entries.iter().map(|e| e.0.clone()).collect();
            labels.push("Back".into());
            let Some(choice) =
                pick(ui, title(section["title"].as_str().unwrap(), text), labels).await?
            else {
                break;
            };
            if choice == entries.len() {
                break;
            }
            let (_, item, action) = &entries[choice];
            let action = if let Some(item) = item {
                let actions = item["actions"].as_array().unwrap();
                let detail = item["detail"]
                    .as_str()
                    .filter(|v| !v.is_empty())
                    .map(|v| vec![v.into()])
                    .unwrap_or_default();
                let Some(choice) = pick(
                    ui,
                    title(&row(item), detail),
                    actions
                        .iter()
                        .map(|a| a["label"].as_str().unwrap().into())
                        .collect(),
                )
                .await?
                else {
                    continue;
                };
                &actions[choice]
            } else {
                action.unwrap()
            };
            if let Some(confirm) = action.get("confirm") {
                if !ui
                    .confirm(
                        action["label"].as_str().unwrap().into(),
                        confirm["message"].as_str().unwrap().into(),
                    )
                    .await?
                {
                    ui.notify("Cancelled.", "info").await?;
                    continue;
                }
            }
            let mut request = json!({"command":model["command"],"sectionId":section["id"],"actionId":action["id"],"values":{}});
            if let Some(item) = item {
                request["itemId"] = item["id"].clone();
            }
            if action.get("confirm").is_some() {
                request["confirmed"] = true.into();
            }
            let result = menu.apply(&request).await;
            ui.notify(
                result["text"].as_str().unwrap(),
                if result["ok"] == true {
                    "info"
                } else {
                    "error"
                },
            )
            .await?;
            model = result["menu"].clone();
        }
    }
}

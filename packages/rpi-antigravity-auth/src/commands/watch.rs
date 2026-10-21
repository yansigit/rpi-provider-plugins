//! Approval watching is setup-only and finishes an admitted enrollment transition before stopping.
use super::{seam, Menu, Port};
use crate::vault::setup::Error;
use serde_json::Value;
use std::future::Future;
use tokio_util::sync::CancellationToken;

pub trait Clock: Port {
    fn now_ms(&self) -> f64;
    fn sleep(&self, milliseconds: u64) -> impl Future<Output = ()> + Send;
    fn source_status(&self) -> impl Future<Output = Result<Value, Error>> + Send;
}

pub async fn approval<P: Clock>(
    port: &P,
    signal: &CancellationToken,
) -> Result<Option<Value>, Error> {
    if signal.is_cancelled() {
        return Ok(None);
    }
    let deadline = port.now_ms() + 600_000.0;
    let mut status = port.connect_step().await?;
    while matches!(status["state"].as_str(), Some("pending" | "busy")) {
        if signal.is_cancelled() {
            return Ok(None);
        }
        if port.now_ms() >= deadline {
            return Ok(Some(status));
        }
        tokio::select! {biased; _=signal.cancelled()=>return Ok(None), _=port.sleep(2000)=>{}}
        if signal.is_cancelled() || port.source_status().await?["enrollment"]["state"] == "idle" {
            return Ok(None);
        }
        if signal.is_cancelled() {
            return Ok(None);
        }
        status = port.connect_step().await?;
    }
    Ok(Some(status))
}

pub async fn run<P: Clock>(
    menu: &Menu<P>,
    signal: &CancellationToken,
) -> Option<(String, &'static str)> {
    let (text, kind) = match approval(menu.port.as_ref(), signal).await {
        Ok(None) => return None,
        Ok(Some(status)) => (
            menu.step_text(&status, "").await,
            if status["state"] == "approved" {
                "info"
            } else {
                "warning"
            },
        ),
        Err(error) => (
            seam::folder_refusal(&error.to_string())
                .unwrap_or_else(|| format!("Requesting vault access failed: {error}")),
            "error",
        ),
    };
    Some((seam::text(&text), kind))
}

//! Command flows from `extensions/multiprovider.ts`, `pool-manager.ts` and
//! `settings-ui.ts`, written against [`Ui`] so the same flow runs through the
//! native dialogs port and through scripted differential fixtures.
//!
//! Dialog controls are the host's native dialog objects (`kind`: `input`,
//! `select`, `confirm`, `settings`; see pi-rust `docs/provider-parity/native-dialogs.md`).
//! Replies are the host's validated answers, or `None` when the user cancels.
//! Pi's custom TUI components are mapped to these controls as recorded in
//! `docs/presentation-map.md`.
use serde_json::Value;
use std::future::Future;

pub mod accounts;
pub mod login;
pub mod logout;
pub mod pool_manager;
pub mod port;
pub mod settings;
pub mod switch;
pub mod vprovider;

pub trait Ui: Send + Sync {
    /// Whether an interactive UI is attached. Commands reject non-TUI use
    /// before showing anything, as upstream does.
    fn has_ui(&self) -> bool;
    /// Shows one native dialog control and awaits its reply.
    fn show(&self, control: Value) -> impl Future<Output = Result<Option<Value>, String>> + Send;
    /// Emits a notification with Pi's `info`, `warning` or `error` kind.
    fn notify(&self, text: &str, kind: &str) -> impl Future<Output = Result<(), String>> + Send;
}

//! Claude Pro/Max OAuth for the Anthropic provider, as a native rpi plugin.
//!
//! `auth`, `convert`, `prompt`, `billing` and `version` are pure state machines:
//! they take JSON in and give JSON back. [`transport`] builds the Claude Code
//! Messages request from those pieces, [`stream`] turns Anthropic's SSE events
//! into the assistant-message events the host renders, and [`native`] is the
//! C-ABI surface that wires both into rpi.
pub mod auth;
pub mod billing;
pub mod commands;
pub mod convert;
mod native;
pub mod prompt;
pub mod stream;
pub mod transport;
pub mod usage;
pub mod version;

rpi_plugin_sdk::export_plugin!(native::register);

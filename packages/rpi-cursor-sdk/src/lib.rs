//! Retained Cursor adapter integration. Package parity is tracked in README.md.
pub mod import_guard;
pub mod installed;
pub mod journal;
mod native;
pub mod native_host;
pub mod runtime;
pub mod worker;

rpi_plugin_sdk::export_plugin!(native::register);

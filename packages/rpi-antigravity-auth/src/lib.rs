//! Native port of the pinned Antigravity Pi integration. See PARITY.md.
pub mod auth;
pub mod catalog;
pub mod convert;
pub mod driver;
pub mod local;
pub mod metadata;
pub mod project;
pub mod redaction;
pub mod request;
pub mod sse;
pub mod stream;
pub mod transport;

mod json;
pub mod schema;

mod native;
rpi_plugin_sdk::export_plugin!(native::register);

//! Vault enrollment and per-send custody. No local credential fallback.
pub mod client;
pub mod connection;
pub mod discovery;
pub mod errors;

pub mod policy;
pub mod roster;
pub mod roster_store;

pub mod enrollment;

pub mod ceremony;

pub mod consumer;
pub mod custody;
pub mod file_lease;
pub mod file_store;
pub mod path;
pub mod private;
mod promise;
pub mod runtime;
pub mod send;
pub mod slot;
pub mod source;
pub mod stream;

//! Vault enrollment and per-send custody. No local credential fallback.
pub mod client;
pub mod connection;
pub mod errors;

pub mod policy;

pub mod enrollment;

pub mod ceremony;

pub mod file_lease;
pub mod file_store;
pub mod private;
pub mod slot;

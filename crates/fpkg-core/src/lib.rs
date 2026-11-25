//! Flux package management: specs, versions, archives, the lockfile, resolution,
//! registries, install, verify and tasks. Prints nothing and never prompts (the Lua of a
//! task prints through the runtime's console).

pub mod archive;
pub mod auth;
pub mod config;
pub mod files;
mod id;
pub mod install;
pub mod lockfile;
pub mod project;
pub mod registry;
pub mod resolve;
pub mod spec;
pub mod tasks;
pub mod version;

pub use id::{IdError, PackageId, nearest_id};
pub use version::{Constraint, Version, VersionError};

/// The version of fpkg itself, sent as the user agent.
pub const FPKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

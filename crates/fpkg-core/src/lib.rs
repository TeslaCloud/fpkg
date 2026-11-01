//! Flux package management: specs, versions, archives, the lockfile, resolution,
//! registries, install and verify. Prints nothing and never prompts.

pub mod archive;
pub mod auth;
pub mod config;
mod id;
pub mod install;
pub mod lockfile;
pub mod project;
pub mod registry;
pub mod resolve;
pub mod spec;
pub mod version;

pub use id::{IdError, PackageId};
pub use version::{Constraint, Version, VersionError};

/// The version of fpkg itself, sent as the user agent.
pub const FPKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

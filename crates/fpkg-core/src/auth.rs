//! Credentials: `~/.config/fpkg/credentials.toml` (mode 0600), one entry per
//! registry URL. Tokens go only over HTTPS, or plain HTTP to loopback.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::lockfile::normalize_registry;
use crate::registry::Token;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("{path}: {message}")]
    File { path: String, message: String },
    #[error(
        "refusing to send a token to {0} over plain http (use https; http is allowed only for localhost and 127.0.0.1)"
    )]
    Insecure(String),
    #[error("invalid registry URL '{0}'")]
    BadUrl(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(default)]
    pub registries: BTreeMap<String, Entry>,
}

impl Credentials {
    pub fn load(path: &Path) -> Result<Credentials, AuthError> {
        match std::fs::read_to_string(path) {
            Ok(s) => toml::from_str(&s).map_err(|e| AuthError::File { path: path.display().to_string(), message: e.to_string() }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Credentials::default()),
            Err(e) => Err(AuthError::File { path: path.display().to_string(), message: e.to_string() }),
        }
    }

    /// Writes the file with mode 0600 (created so, never widened).
    pub fn save(&self, path: &Path) -> Result<(), AuthError> {
        let err = |e: std::io::Error| AuthError::File { path: path.display().to_string(), message: e.to_string() };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(err)?;
        }
        let text = toml::to_string(self).map_err(|e| AuthError::File { path: path.display().to_string(), message: e.to_string() })?;
        let dir = path.parent().unwrap_or(Path::new("."));
        let mut tmp = tempfile::Builder::new().prefix(".credentials").tempfile_in(dir).map_err(err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600)).map_err(err)?;
        }
        std::io::Write::write_all(&mut tmp, text.as_bytes()).map_err(err)?;
        tmp.persist(path).map_err(|e| err(e.error))?;
        Ok(())
    }

    pub fn get(&self, registry: &str) -> Option<&Entry> {
        self.registries.get(&normalize_registry(registry))
    }

    pub fn set(&mut self, registry: &str, token: &Token, user: Option<String>) {
        self.registries.insert(normalize_registry(registry), Entry { token: token.secret().to_string(), user });
    }

    /// Removes the entry; whether there was one.
    pub fn remove(&mut self, registry: &str) -> bool {
        self.registries.remove(&normalize_registry(registry)).is_some()
    }
}

/// The token for a registry: `FPKG_TOKEN` when `env_allowed` (the registry
/// is one the user or CI named, see `Config::env_token_allowed`), else the
/// credentials file's entry for exactly that registry.
pub fn token_for(registry: &str, creds: &Credentials, env_allowed: bool) -> Option<Token> {
    if env_allowed
        && let Ok(t) = std::env::var("FPKG_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(Token::new(t));
    }
    creds.get(registry).map(|e| Token::new(e.token.clone()))
}

/// Whether a token may be sent to `registry`: HTTPS, or HTTP to loopback.
/// `file://` registries take no token, so anything goes.
pub fn check_transport(registry: &str) -> Result<(), AuthError> {
    let u = url::Url::parse(registry).map_err(|_| AuthError::BadUrl(registry.to_string()))?;
    match u.scheme() {
        "https" | "file" => Ok(()),
        "http" if crate::registry::is_loopback_url(&u) => Ok(()),
        _ => Err(AuthError::Insecure(registry.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_rules() {
        assert!(check_transport("https://fluxpkg.dev/").is_ok());
        assert!(check_transport("http://localhost:8787/").is_ok());
        assert!(check_transport("http://127.0.0.1:8787/").is_ok());
        assert!(check_transport("http://[::1]:8787/").is_ok());
        assert!(check_transport("http://fluxpkg.dev/").is_err());
        assert!(check_transport("http://localhost.example.com/").is_err());
    }

    #[test]
    fn file_round_trip_and_mode() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("sub/credentials.toml");
        let mut c = Credentials::default();
        c.set("https://fluxpkg.dev", &Token::new("fpk_secret"), Some("luna".into()));
        c.save(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("[registries.\"https://fluxpkg.dev/\"]"), "{text}");
        let back = Credentials::load(&p).unwrap();
        assert_eq!(back.get("https://fluxpkg.dev/").unwrap().user.as_deref(), Some("luna"));
        let mut back = back;
        assert!(back.remove("https://fluxpkg.dev/"));
        assert!(!back.remove("https://fluxpkg.dev/"));
        assert_eq!(format!("{:?}", Token::new("fpk_x")), "Token(***)");
    }
}

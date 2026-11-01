//! Configuration: user config, project config, `FPKG_<SECTION>_<KEY>` variables
//! (plus `FPKG_REGISTRY`, `FPKG_OFFLINE`), then flags, each overriding the last.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::registry::{DEFAULT_REGISTRY, NetOptions};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {message}")]
    File { path: PathBuf, message: String },
    #[error("environment variable {name}: {message}")]
    Env { name: String, message: String },
    #[error("{0}")]
    Invalid(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// `[registry] default`
    pub registry: String,
    /// `[net] offline`
    pub offline: bool,
    /// `[net] retries`
    pub retries: u32,
    /// `[net] timeout_seconds`
    pub timeout_seconds: u64,
    /// `[pack] compression`: the deflate level.
    pub compression: i64,
    pub cache_dir: PathBuf,
    pub config_dir: PathBuf,
    /// The registries `FPKG_TOKEN` may be sent to: the built-in or user-config
    /// default, `FPKG_REGISTRY` and `--registry`, so a project config cannot
    /// collect CI tokens.
    pub env_token_registries: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            registry: DEFAULT_REGISTRY.to_string(),
            offline: false,
            retries: 3,
            timeout_seconds: 30,
            compression: 6,
            cache_dir: cache_dir(),
            config_dir: config_dir(),
            env_token_registries: Vec::new(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    registry: Option<RegistrySection>,
    net: Option<NetSection>,
    pack: Option<PackSection>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RegistrySection {
    default: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct NetSection {
    offline: Option<bool>,
    retries: Option<u32>,
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PackSection {
    compression: Option<i64>,
}

/// Values given as flags; `None` keeps the configured value.
#[derive(Clone, Debug, Default)]
pub struct Flags {
    pub registry: Option<String>,
    pub offline: Option<bool>,
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// `~/.config/fpkg` (or `$XDG_CONFIG_HOME/fpkg`, or `$FPKG_CONFIG_DIR`), macOS included.
pub fn config_dir() -> PathBuf {
    env_dir("FPKG_CONFIG_DIR")
        .or_else(|| env_dir("XDG_CONFIG_HOME").map(|d| d.join("fpkg")))
        .unwrap_or_else(|| home().join(".config").join("fpkg"))
}

/// `~/.cache/fpkg` (or `$XDG_CACHE_HOME/fpkg`, or `$FPKG_CACHE_DIR`).
pub fn cache_dir() -> PathBuf {
    env_dir("FPKG_CACHE_DIR")
        .or_else(|| env_dir("XDG_CACHE_HOME").map(|d| d.join("fpkg")))
        .unwrap_or_else(|| home().join(".cache").join("fpkg"))
}

fn parse_bool(name: &str, v: &str) -> Result<bool, ConfigError> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" | "" => Ok(false),
        other => Err(ConfigError::Env { name: name.into(), message: format!("'{other}' is not a boolean") }),
    }
}

fn env(names: &[&str]) -> Option<(String, String)> {
    names.iter().find_map(|n| std::env::var(n).ok().map(|v| (n.to_string(), v)))
}

impl Config {
    /// Loads every layer. `project_root` adds the project config.
    pub fn load(project_root: Option<&Path>, flags: &Flags) -> Result<Config, ConfigError> {
        let mut c = Config::default();
        c.apply_file(&c.config_dir.join("config.toml"))?;
        let mut trusted = vec![c.registry.clone()];
        if let Some(root) = project_root {
            c.apply_file(&root.join(".fpkg").join("config.toml"))?;
        }
        c.apply_env()?;
        if let Some((_, v)) = env(&["FPKG_REGISTRY_DEFAULT", "FPKG_REGISTRY"]).filter(|(_, v)| !v.is_empty()) {
            trusted.push(v);
        }
        if let Some(r) = &flags.registry {
            c.registry = r.clone();
            trusted.push(r.clone());
        }
        c.env_token_registries = trusted.iter().map(|u| registry_key(u)).collect();
        if let Some(o) = flags.offline {
            c.offline = c.offline || o;
        }
        c.validate()?;
        Ok(c)
    }

    fn apply_file(&mut self, path: &Path) -> Result<(), ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(ConfigError::File { path: path.into(), message: e.to_string() }),
        };
        let f: FileConfig =
            toml::from_str(&text).map_err(|e| ConfigError::File { path: path.into(), message: e.to_string() })?;
        if let Some(r) = f.registry.and_then(|r| r.default) {
            self.registry = r;
        }
        if let Some(n) = f.net {
            if let Some(v) = n.offline {
                self.offline = v;
            }
            if let Some(v) = n.retries {
                self.retries = v;
            }
            if let Some(v) = n.timeout_seconds {
                self.timeout_seconds = v;
            }
        }
        if let Some(v) = f.pack.and_then(|p| p.compression) {
            self.compression = v;
        }
        Ok(())
    }

    fn apply_env(&mut self) -> Result<(), ConfigError> {
        if let Some((_, v)) = env(&["FPKG_REGISTRY_DEFAULT", "FPKG_REGISTRY"]).filter(|(_, v)| !v.is_empty()) {
            self.registry = v;
        }
        if let Some((n, v)) = env(&["FPKG_NET_OFFLINE", "FPKG_OFFLINE"]) {
            self.offline = parse_bool(&n, &v)?;
        }
        let num = |names: &[&str]| -> Result<Option<i64>, ConfigError> {
            match env(names) {
                None => Ok(None),
                Some((n, v)) => v
                    .trim()
                    .parse::<i64>()
                    .map(Some)
                    .map_err(|_| ConfigError::Env { name: n, message: format!("'{v}' is not a number") }),
            }
        };
        if let Some(v) = num(&["FPKG_NET_RETRIES"])? {
            self.retries = v.clamp(0, 100) as u32;
        }
        if let Some(v) = num(&["FPKG_NET_TIMEOUT_SECONDS"])? {
            self.timeout_seconds = v.max(1) as u64;
        }
        if let Some(v) = num(&["FPKG_PACK_COMPRESSION"])? {
            self.compression = v;
        }
        Ok(())
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !(0..=9).contains(&self.compression) {
            return Err(ConfigError::Invalid(format!("[pack] compression is {}; deflate levels are 0 to 9", self.compression)));
        }
        if self.timeout_seconds == 0 {
            return Err(ConfigError::Invalid("[net] timeout_seconds must be at least 1".into()));
        }
        Ok(())
    }

    /// Whether `FPKG_TOKEN` may be sent to `registry` (see
    /// `env_token_registries`).
    pub fn env_token_allowed(&self, registry: &str) -> bool {
        let key = registry_key(registry);
        self.env_token_registries.iter().any(|k| k == &key)
    }

    pub fn net(&self) -> NetOptions {
        NetOptions { retries: self.retries, timeout: Duration::from_secs(self.timeout_seconds) }
    }

    pub fn credentials_path(&self) -> PathBuf {
        self.config_dir.join("credentials.toml")
    }
}

/// A registry URL as compared for token scoping.
fn registry_key(url: &str) -> String {
    crate::registry::normalize_url(url).unwrap_or_else(|_| crate::lockfile::normalize_registry(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_token_is_scoped() {
        let t = tempfile::tempdir().unwrap();
        let project = t.path().join("project");
        std::fs::create_dir_all(project.join(".fpkg")).unwrap();
        std::fs::write(project.join(".fpkg/config.toml"), "[registry]\ndefault = \"https://collector.example/\"\n").unwrap();
        // Without the environment's own FPKG_REGISTRY, the user default is
        // https://fluxpkg.dev/ unless the user config says otherwise.
        let mut c = Config { config_dir: t.path().join("none"), ..Config::default() };
        c.env_token_registries = vec![registry_key(DEFAULT_REGISTRY)];
        c.apply_file(&project.join(".fpkg/config.toml")).unwrap();
        assert_eq!(c.registry, "https://collector.example/");
        assert!(!c.env_token_allowed(&c.registry), "a project-config registry does not get FPKG_TOKEN");
        assert!(c.env_token_allowed("https://fluxpkg.dev"));
        assert!(c.env_token_allowed("https://fluxpkg.dev/api/v1/"));
        // Named by a flag, it does.
        let flags = Flags { registry: Some("https://collector.example/".into()), offline: None };
        let loaded = Config::load(Some(&project), &flags).unwrap();
        assert!(loaded.env_token_allowed("https://collector.example/"));
    }

    #[test]
    fn layers() {
        let t = tempfile::tempdir().unwrap();
        let user = t.path().join("user");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("config.toml"), "[registry]\ndefault = \"https://a.example/\"\n[net]\nretries = 5\n[pack]\ncompression = 9\n").unwrap();
        let project = t.path().join("project");
        std::fs::create_dir_all(project.join(".fpkg")).unwrap();
        std::fs::write(project.join(".fpkg/config.toml"), "[registry]\ndefault = \"https://b.example/\"\n").unwrap();
        let mut c = Config { config_dir: user.clone(), ..Config::default() };
        c.apply_file(&user.join("config.toml")).unwrap();
        assert_eq!((c.registry.as_str(), c.retries, c.compression), ("https://a.example/", 5, 9));
        c.apply_file(&project.join(".fpkg/config.toml")).unwrap();
        assert_eq!(c.registry, "https://b.example/");
        assert_eq!(c.retries, 5);
        std::fs::write(project.join(".fpkg/config.toml"), "[net]\nretry = 1\n").unwrap();
        assert!(c.apply_file(&project.join(".fpkg/config.toml")).is_err());
        c.compression = 12;
        assert!(c.validate().is_err());
    }
}

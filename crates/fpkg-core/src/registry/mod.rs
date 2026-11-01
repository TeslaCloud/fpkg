//! Registries: the `Registry` trait, the API's JSON types, `FileRegistry`,
//! `HttpRegistry` and `CachedRegistry`.

mod cache;
mod file;
mod http;
pub mod literals;

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use cache::{CachedRegistry, cache_key};
pub use file::FileRegistry;
pub use http::{HttpRegistry, is_loopback as is_loopback_url};

use crate::PackageId;
use crate::spec::{DepKind, Manifest};
use crate::version::Version;

pub type Result<T, E = RegistryError> = std::result::Result<T, E>;

/// The API version this client speaks; every response carries `X-Fpkg-Api`.
pub const API_VERSION: &str = "1";
pub const DEFAULT_REGISTRY: &str = "https://fluxpkg.dev/";
pub const PER_PAGE: u32 = 20;

/// An API token. Its `Debug` form never shows the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    pub fn new(s: impl Into<String>) -> Token {
        Token(s.into().trim().to_string())
    }
    /// What a `file://` registry is given: it does no authentication.
    pub fn none() -> Token {
        Token(String::new())
    }
    pub fn secret(&self) -> &str {
        &self.0
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepMeta {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    /// A constraint; `*` for any version.
    #[serde(default = "any_constraint")]
    pub constraint: String,
}

/// `null` reads as the default, so that a server writing `"globals": null`
/// or `"yanked": null` is understood.
fn nullable<'de, D, T>(d: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Owners as logins, or as objects with a `login`.
fn owner_logins<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Owner {
        Login(String),
        Object { login: String },
    }
    let v: Option<Vec<Owner>> = Option::deserialize(d)?;
    Ok(v.unwrap_or_default()
        .into_iter()
        .map(|o| match o {
            Owner::Login(l) => l,
            Owner::Object { login } => login,
        })
        .collect())
}

/// A package id, checked against the id grammar as it is read: registry
/// JSON names paths (`fpkg mirror`, the cache), so nothing unchecked may pass.
fn de_id<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let s = String::deserialize(d)?;
    match PackageId::problem(&s) {
        None => Ok(s),
        Some(reason) => Err(serde::de::Error::custom(format!("invalid package id '{s}': {reason}"))),
    }
}

/// A version, checked against the version grammar as it is read.
fn de_version<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let s = String::deserialize(d)?;
    Version::parse(&s).map_err(serde::de::Error::custom)?;
    Ok(s)
}

fn de_opt_version<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<String>, D::Error> {
    let s = Option::<String>::deserialize(d)?;
    if let Some(v) = &s {
        Version::parse(v).map_err(serde::de::Error::custom)?;
    }
    Ok(s)
}

/// The versions map of a package document: every key a version.
fn de_versions<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<BTreeMap<String, VersionMeta>, D::Error> {
    let m = Option::<BTreeMap<String, VersionMeta>>::deserialize(d)?.unwrap_or_default();
    for k in m.keys() {
        Version::parse(k).map_err(serde::de::Error::custom)?;
    }
    Ok(m)
}

fn any_constraint() -> String {
    "*".into()
}

fn package_kind() -> String {
    "package".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dist {
    pub sha256: String,
    pub size: u64,
    pub url: String,
}

/// Version metadata: in the package document, the version endpoint, the
/// publish request (without `dist`) and the publish response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionMeta {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    pub name: String,
    #[serde(deserialize_with = "de_version")]
    pub version: String,
    #[serde(default = "package_kind")]
    pub kind: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub authors: Vec<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub globals: Vec<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub serverside: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub clientside: bool,
    #[serde(default, deserialize_with = "nullable")]
    pub dependencies: Vec<DepMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dist: Option<Dist>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub yanked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_by: Option<String>,
    /// Publish requests only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_missing_dependencies: bool,
}

impl VersionMeta {
    /// The publish metadata of a manifest (no `dist`; the server adds it).
    pub fn from_manifest(m: &Manifest) -> VersionMeta {
        VersionMeta {
            id: m.id.clone(),
            name: m.name.clone(),
            version: m.version.to_string(),
            kind: package_kind(),
            summary: m.summary.clone(),
            description: m.description.clone(),
            authors: m.authors.clone(),
            license: m.license.clone(),
            website: m.website.clone(),
            globals: m.globals.clone(),
            serverside: m.serverside,
            clientside: m.clientside,
            dependencies: m
                .dependencies
                .iter()
                .filter(|d| d.kind == DepKind::Package)
                .map(|d| DepMeta { id: d.id.clone(), constraint: d.constraint.to_string() })
                .collect(),
            dist: None,
            docs: None,
            yanked: false,
            published_at: None,
            published_by: None,
            allow_missing_dependencies: false,
        }
    }

    pub fn parsed_version(&self) -> Result<Version> {
        Version::parse(&self.version).map_err(|e| RegistryError::Invalid(format!("{}: {e}", self.id)))
    }

    pub fn package_id(&self) -> Result<PackageId> {
        PackageId::new(self.id.clone()).map_err(|e| RegistryError::Invalid(e.to_string()))
    }

    /// The metadata as sent in a publish request: without the fields the
    /// server sets.
    pub fn for_publish(&self) -> VersionMeta {
        VersionMeta { dist: None, docs: None, yanked: false, published_at: None, published_by: None, ..self.clone() }
    }
}

/// The package document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageDoc {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub revision: u64,
    #[serde(default, deserialize_with = "owner_logins")]
    pub owners: Vec<String>,
    /// The newest non-yanked version.
    #[serde(default, deserialize_with = "de_opt_version")]
    pub latest: Option<String>,
    #[serde(default, deserialize_with = "de_versions")]
    pub versions: BTreeMap<String, VersionMeta>,
}

impl PackageDoc {
    /// Versions newest first, parsed. Unparsable entries, and entries whose `id`
    /// or `version` disagree with the document, are left out.
    pub fn sorted_versions(&self) -> Vec<(Version, &VersionMeta)> {
        let mut v: Vec<(Version, &VersionMeta)> = self
            .versions
            .iter()
            .filter_map(|(k, m)| self.consistent(k, m).map(|v| (v, m)))
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v
    }

    fn consistent(&self, key: &str, m: &VersionMeta) -> Option<Version> {
        let kv = Version::parse(key).ok()?;
        let mv = Version::parse(&m.version).ok()?;
        (m.id == self.id && kv == mv).then_some(kv)
    }

    /// The metadata of `v`, if the document has a consistent entry for it.
    pub fn get(&self, v: &Version) -> Option<&VersionMeta> {
        self.versions.iter().find(|(k, m)| self.consistent(k, m).as_ref() == Some(v)).map(|(_, m)| m)
    }

    /// The newest non-yanked version, computed (rather than trusting `latest`).
    pub fn newest(&self) -> Option<Version> {
        let all = self.sorted_versions();
        let pick = |pre: bool| all.iter().find(|(v, m)| !m.yanked && (pre || !v.is_prerelease())).map(|(v, _)| v.clone());
        pick(false).or_else(|| pick(true))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    #[serde(deserialize_with = "de_id")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    /// The newest version; a server may call it `latest` instead.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default, skip_serializing)]
    pub latest: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    #[serde(alias = "results", alias = "items", deserialize_with = "nullable")]
    pub packages: Vec<SearchHit>,
    #[serde(default = "first_page")]
    pub page: u32,
    #[serde(default = "per_page")]
    pub per_page: u32,
    #[serde(default)]
    pub total: u64,
}

impl SearchPage {
    /// Fills `version` from `latest` where only that was given.
    pub fn normalized(mut self) -> SearchPage {
        for h in &mut self.packages {
            if h.version.is_none() {
                h.version = h.latest.take();
            }
        }
        self
    }
}

fn first_page() -> u32 {
    1
}
fn per_page() -> u32 {
    PER_PAGE
}

/// `GET /me`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub login: String,
    #[serde(default)]
    pub token_name: Option<String>,
}

/// What an HTTP error status means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiErrorKind {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    Validation,
    RateLimited,
    Server,
    Other,
}

impl ApiErrorKind {
    pub fn from_status(status: u16) -> Self {
        match status {
            400 => ApiErrorKind::BadRequest,
            401 => ApiErrorKind::Unauthorized,
            403 => ApiErrorKind::Forbidden,
            404 => ApiErrorKind::NotFound,
            409 => ApiErrorKind::Conflict,
            413 => ApiErrorKind::TooLarge,
            422 => ApiErrorKind::Validation,
            429 => ApiErrorKind::RateLimited,
            500..=599 => ApiErrorKind::Server,
            _ => ApiErrorKind::Other,
        }
    }
}

/// An error response: `{ "error": { "code", "message", "details"? } }` with
/// its status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub kind: ApiErrorKind,
    pub code: String,
    pub message: String,
    /// The `details[]` of a `422`, as text.
    pub details: Vec<String>,
    /// Seconds, from `Retry-After`.
    pub retry_after: Option<u64>,
}

impl ApiError {
    pub fn new(status: u16, code: &str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            kind: ApiErrorKind::from_status(status),
            code: code.to_string(),
            message: message.into(),
            details: Vec::new(),
            retry_after: None,
        }
    }

    pub fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({} {})", self.message, self.status, self.code)?;
        for d in &self.details {
            write!(f, "\n  - {d}")?;
        }
        if let Some(s) = self.retry_after {
            write!(f, "\n  retry after {s} s")?;
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("{0}")]
    Api(ApiError),
    #[error("{url} does not look like an fpkg registry: {message}")]
    BadApi { url: String, message: String },
    #[error("{url}: {message}")]
    Transport { url: String, message: String },
    #[error("offline: {0} is not in the cache")]
    Offline(String),
    #[error("checksum mismatch for {what}: expected sha256 {expected}, got {actual}")]
    Checksum { what: String, expected: String, actual: String },
    #[error(
        "refusing to talk to {0} over plain http: use https (http is allowed only for localhost and 127.0.0.1)"
    )]
    Insecure(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{path}")]
    Io { path: PathBuf, source: std::io::Error },
}

impl RegistryError {
    pub fn api(status: u16, code: &str, message: impl Into<String>) -> Self {
        RegistryError::Api(ApiError::new(status, code, message))
    }
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::api(404, "not_found", message)
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            RegistryError::Api(e) => Some(e.status),
            _ => None,
        }
    }
    pub fn kind(&self) -> Option<ApiErrorKind> {
        match self {
            RegistryError::Api(e) => Some(e.kind),
            _ => None,
        }
    }
    pub fn code(&self) -> Option<&str> {
        match self {
            RegistryError::Api(e) => Some(&e.code),
            _ => None,
        }
    }
    pub fn details(&self) -> &[String] {
        match self {
            RegistryError::Api(e) => &e.details,
            _ => &[],
        }
    }
    pub fn is_not_found(&self) -> bool {
        self.kind() == Some(ApiErrorKind::NotFound)
    }
    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        RegistryError::Io { path: path.to_path_buf(), source }
    }
}

/// The result of a conditional fetch of a package document.
#[derive(Clone, Debug)]
pub enum Conditional {
    /// `304`: the cached document is current.
    NotModified,
    Modified { doc: PackageDoc, etag: Option<String> },
    NotFound,
}

/// The registry API. `CachedRegistry` revalidates through `package_conditional`.
pub trait Registry {
    fn package(&self, id: &PackageId) -> Result<Option<PackageDoc>>;
    fn version(&self, id: &PackageId, v: &Version) -> Result<Option<VersionMeta>>;
    fn archive(&self, id: &PackageId, v: &Version) -> Result<Vec<u8>>;
    fn publish(&self, meta: &VersionMeta, archive: &[u8], auth: &Token) -> Result<VersionMeta>;
    fn yank(&self, id: &PackageId, v: &Version, yanked: bool, auth: &Token) -> Result<()>;
    fn owners(&self, id: &PackageId) -> Result<Vec<String>>;
    fn set_owner(&self, id: &PackageId, login: &str, add: bool, auth: &Token) -> Result<()>;
    fn search(&self, query: &str, page: u32) -> Result<SearchPage>;
    fn me(&self, auth: &Token) -> Result<Identity>;

    /// `GET /packages/{id}` with `If-None-Match: <etag>`.
    fn package_conditional(&self, id: &PackageId, etag: Option<&str>) -> Result<Conditional> {
        let _ = etag;
        Ok(match self.package(id)? {
            Some(doc) => Conditional::Modified { doc, etag: None },
            None => Conditional::NotFound,
        })
    }
}

macro_rules! forward_registry {
    ($t:ty) => {
        fn package(&self, id: &PackageId) -> Result<Option<PackageDoc>> {
            <$t>::package(self, id)
        }
        fn version(&self, id: &PackageId, v: &Version) -> Result<Option<VersionMeta>> {
            <$t>::version(self, id, v)
        }
        fn archive(&self, id: &PackageId, v: &Version) -> Result<Vec<u8>> {
            <$t>::archive(self, id, v)
        }
        fn publish(&self, meta: &VersionMeta, archive: &[u8], auth: &Token) -> Result<VersionMeta> {
            <$t>::publish(self, meta, archive, auth)
        }
        fn yank(&self, id: &PackageId, v: &Version, yanked: bool, auth: &Token) -> Result<()> {
            <$t>::yank(self, id, v, yanked, auth)
        }
        fn owners(&self, id: &PackageId) -> Result<Vec<String>> {
            <$t>::owners(self, id)
        }
        fn set_owner(&self, id: &PackageId, login: &str, add: bool, auth: &Token) -> Result<()> {
            <$t>::set_owner(self, id, login, add, auth)
        }
        fn search(&self, query: &str, page: u32) -> Result<SearchPage> {
            <$t>::search(self, query, page)
        }
        fn me(&self, auth: &Token) -> Result<Identity> {
            <$t>::me(self, auth)
        }
        fn package_conditional(&self, id: &PackageId, etag: Option<&str>) -> Result<Conditional> {
            <$t>::package_conditional(self, id, etag)
        }
    };
}

impl<R: Registry + ?Sized> Registry for Box<R> {
    forward_registry!(R);
}

impl<R: Registry + ?Sized> Registry for &R {
    forward_registry!(R);
}

/// Network settings of `[net]`.
#[derive(Clone, Debug)]
pub struct NetOptions {
    pub retries: u32,
    pub timeout: Duration,
}

impl Default for NetOptions {
    fn default() -> Self {
        NetOptions { retries: 3, timeout: Duration::from_secs(30) }
    }
}

/// Opens the registry at `url`: `file://` or `http(s)://`.
pub fn open(url: &str, net: &NetOptions) -> Result<Box<dyn Registry>> {
    if url.starts_with("file:") {
        Ok(Box::new(FileRegistry::from_url(url)?))
    } else if url.starts_with("http://") || url.starts_with("https://") {
        Ok(Box::new(HttpRegistry::new(url, net)?))
    } else {
        Err(RegistryError::Invalid(format!("unsupported registry URL '{url}' (https://, http://localhost or file://)")))
    }
}

/// The registry's normalised URL (trailing slash), as lockfiles record it.
pub fn normalize_url(url: &str) -> Result<String> {
    if url.starts_with("file:") {
        return Ok(FileRegistry::from_url(url)?.url().to_string());
    }
    let u = url::Url::parse(url).map_err(|e| RegistryError::Invalid(format!("invalid registry URL '{url}': {e}")))?;
    Ok(HttpRegistry::root_of(&u).to_string())
}

/// The browser URL of a package on a registry (`https://fluxpkg.dev/packages/<id>`).
pub fn package_page(registry_url: &str, id: &str) -> String {
    if registry_url.starts_with("file:") {
        format!("{}packages/{id}.json", crate::lockfile::normalize_registry(registry_url))
    } else {
        format!("{}packages/{id}", crate::lockfile::normalize_registry(registry_url))
    }
}

/// The docs URL of a version when the registry does not say
/// (`https://docs.fluxpkg.dev/<id>/<version>/`).
pub fn docs_url(registry_url: &str, id: &str, version: &str) -> Option<String> {
    let u = url::Url::parse(registry_url).ok()?;
    let host = u.host_str()?;
    if u.scheme() == "file" {
        return None;
    }
    let mut d = u.clone();
    if host != "localhost" && host.parse::<std::net::IpAddr>().is_err() {
        d.set_host(Some(&format!("docs.{host}"))).ok()?;
        d.set_path(&format!("/{id}/{version}/"));
    } else {
        // A local server dispatches on Host; there is no docs subdomain.
        d.set_path(&format!("/docs/{id}/{version}/"));
    }
    Some(d.to_string())
}

/// The registries a command talks to, each opened once and wrapped in the
/// cache: the configured default, and whatever a lockfile names.
pub struct Registries {
    cache_root: PathBuf,
    net: NetOptions,
    offline: bool,
    default_url: String,
    /// Where archives come from instead of the registry a lock entry names
    /// (`fpkg install --registry <mirror>`); safe as the lock's checksum is verified.
    mirror: Option<String>,
    opened: std::cell::RefCell<std::collections::HashMap<String, SharedRegistry>>,
}

/// A registry opened by `Registries`: cached, behind a trait object.
pub type SharedRegistry = std::rc::Rc<CachedRegistry<Box<dyn Registry>>>;

impl Registries {
    pub fn new(default_url: &str, cache_root: PathBuf, net: NetOptions, offline: bool) -> Result<Registries> {
        Ok(Registries {
            cache_root,
            net,
            offline,
            default_url: normalize_url(default_url)?,
            mirror: None,
            opened: Default::default(),
        })
    }

    /// Fetch every archive from `url` (a mirror) whatever registry the lock names.
    pub fn with_mirror(mut self, url: &str) -> Result<Registries> {
        self.mirror = Some(normalize_url(url)?);
        Ok(self)
    }

    /// The registry to download `source`'s archives from.
    pub fn archive_source(&self, source: &str) -> Result<SharedRegistry> {
        match &self.mirror {
            Some(m) => {
                let m = m.clone();
                self.get(&m)
            }
            None => self.get(source),
        }
    }

    pub fn default_url(&self) -> &str {
        &self.default_url
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    pub fn cache_root(&self) -> &std::path::Path {
        &self.cache_root
    }

    pub fn get(&self, url: &str) -> Result<SharedRegistry> {
        let url = normalize_url(url)?;
        if let Some(r) = self.opened.borrow().get(&url) {
            return Ok(r.clone());
        }
        let inner = open(&url, &self.net)?;
        let r = std::rc::Rc::new(CachedRegistry::new(inner, &self.cache_root, &url, self.offline));
        self.opened.borrow_mut().insert(url, r.clone());
        Ok(r)
    }

    pub fn default_registry(&self) -> Result<SharedRegistry> {
        let url = self.default_url.clone();
        self.get(&url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_json_shape() {
        let json = r#"{
          "id": "cable", "name": "Cable", "version": "1.0.0", "kind": "package",
          "summary": "A net library wrapper.", "description": "x", "authors": ["Luna Fox"],
          "license": "MIT", "website": "https://teslacloud.net", "globals": [],
          "serverside": false, "clientside": false,
          "dependencies": [ { "id": "sfs", "constraint": "~> 7.0" } ],
          "dist": { "sha256": "9a02", "size": 18231, "url": "https://fluxpkg.dev/api/v1/packages/cable/versions/1.0.0/archive" },
          "docs": "https://docs.fluxpkg.dev/cable/1.0.0/", "yanked": false,
          "published_at": "2026-11-01T18:04:11Z", "published_by": "luna", "extra": 1
        }"#;
        let m: VersionMeta = serde_json::from_str(json).unwrap();
        assert_eq!(m.dependencies[0].constraint, "~> 7.0");
        assert_eq!(m.dist.as_ref().unwrap().size, 18231);
        let publish = serde_json::to_value(m.for_publish()).unwrap();
        assert!(publish.get("dist").is_none());
        assert!(publish.get("allow_missing_dependencies").is_none());
        assert_eq!(publish["summary"], "A net library wrapper.");
        let doc: PackageDoc = serde_json::from_str(&format!(
            r#"{{ "id": "cable", "revision": 7, "owners": ["luna"], "latest": "1.0.0", "versions": {{ "1.0.0": {json} }} }}"#
        ))
        .unwrap();
        assert_eq!(doc.newest().unwrap().to_string(), "1.0.0");
        let lenient: PackageDoc = serde_json::from_str(
            r#"{ "id": "x", "revision": 1, "owners": [{"login": "luna"}], "versions": { "1.0.0": { "id": "x", "name": "X", "version": "1.0.0", "globals": null, "authors": null, "yanked": null } } }"#,
        )
        .unwrap();
        assert_eq!(lenient.owners, vec!["luna"]);
        let page: SearchPage = serde_json::from_str(r#"{ "results": [ { "id": "x", "name": "X", "latest": "1.0.0", "version": "1.0.0" }, { "id": "y", "name": "Y", "latest": "2.0.0" } ] }"#).unwrap();
        let page = page.normalized();
        assert_eq!(page.packages[1].version.as_deref(), Some("2.0.0"));
    }

    #[test]
    fn hostile_ids_and_versions_are_refused_when_read() {
        let meta = |id: &str, v: &str| format!(r#"{{ "id": "{id}", "name": "X", "version": "{v}" }}"#);
        assert!(serde_json::from_str::<VersionMeta>(&meta("../../../escape_id", "1.0.0")).is_err());
        assert!(serde_json::from_str::<VersionMeta>(&meta("ok", "../1.0.0")).is_err());
        assert!(serde_json::from_str::<VersionMeta>(&meta("ok", "1.0.0/../../x")).is_err());
        assert!(serde_json::from_str::<VersionMeta>(&meta("ok", "1.0.0")).is_ok());
        let doc = |id: &str, key: &str, inner: &str| {
            format!(r#"{{ "id": "{id}", "versions": {{ "{key}": {inner} }} }}"#)
        };
        assert!(serde_json::from_str::<PackageDoc>(&doc("../x", "1.0.0", &meta("x", "1.0.0"))).is_err());
        assert!(serde_json::from_str::<PackageDoc>(&doc("x", "../../1", &meta("x", "1.0.0"))).is_err());
        assert!(serde_json::from_str::<PackageDoc>(r#"{ "id": "x", "versions": {}, "latest": "../x" }"#).is_err());
        let dep = r#"{ "id": "x", "name": "X", "version": "1.0.0", "dependencies": [ { "id": "../y", "constraint": "*" } ] }"#;
        assert!(serde_json::from_str::<VersionMeta>(dep).is_err());
        // An entry that names another package, or another version, is not served.
        let d: PackageDoc = serde_json::from_str(&doc("x", "1.0.0", &meta("y", "1.0.0"))).unwrap();
        assert!(d.get(&Version::new(1, 0, 0)).is_none());
        assert!(d.sorted_versions().is_empty());
        let d: PackageDoc = serde_json::from_str(&doc("x", "1.0.0", &meta("x", "2.0.0"))).unwrap();
        assert!(d.get(&Version::new(1, 0, 0)).is_none() && d.get(&Version::new(2, 0, 0)).is_none());
        let d: PackageDoc = serde_json::from_str(&doc("x", "1.0", &meta("x", "1.0.0"))).unwrap();
        assert!(d.get(&Version::new(1, 0, 0)).is_some());
    }

    #[test]
    fn docs_urls() {
        assert_eq!(docs_url("https://fluxpkg.dev/", "cable", "1.0.0").unwrap(), "https://docs.fluxpkg.dev/cable/1.0.0/");
        assert_eq!(docs_url("file:///tmp/r/", "cable", "1.0.0"), None);
        assert_eq!(package_page("https://fluxpkg.dev", "cable"), "https://fluxpkg.dev/packages/cable");
    }
}

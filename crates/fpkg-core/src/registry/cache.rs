//! The client cache (`<registry>/index/<id>.json`, `<registry>/archives/<id>-<v>.zip`)
//! and offline mode. Documents are revalidated on `lock`/`update`, else trusted for an hour.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Conditional, Identity, PackageDoc, Registry, RegistryError, Result, SearchPage, Token, VersionMeta};
use crate::PackageId;
use crate::lockfile::write_atomic;
use crate::version::Version;

const TRUST_SECONDS: i64 = 3600;

#[derive(Serialize, Deserialize)]
struct Entry {
    etag: Option<String>,
    fetched_at: i64,
    doc: PackageDoc,
}

/// The directory name of a registry in the cache: scheme, host, port and
/// path, in a safe encoding, so that two registries never share a directory
/// (`https_fluxpkg.dev_443`, `http_127.0.0.1_8787_mirror_<hash>`).
pub fn cache_key(registry_url: &str) -> String {
    let safe = |s: &str| -> String { s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect() };
    let hash = |s: &str| crate::sha256_hex(s.as_bytes())[..12].to_string();
    match url::Url::parse(registry_url) {
        Ok(u) if u.scheme() == "file" => format!("file_{}", hash(u.path())),
        Ok(u) => {
            let host = u.host_str().unwrap_or("unknown");
            let port = u.port_or_known_default().map(|p| p.to_string()).unwrap_or_default();
            let mut key = format!("{}_{}_{port}", safe(u.scheme()), safe(host));
            let path = u.path().trim_matches('/');
            if !path.is_empty() {
                // The readable part may collide (a/b and a_b); the hash does not.
                key += &format!("_{}_{}", safe(path), hash(u.as_str()));
            }
            key
        }
        Err(_) => format!("other_{}", hash(registry_url)),
    }
}

pub struct CachedRegistry<R> {
    inner: R,
    dir: PathBuf,
    offline: bool,
    revalidate: Cell<bool>,
    memo: RefCell<HashMap<String, Option<PackageDoc>>>,
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

impl<R: Registry> CachedRegistry<R> {
    /// `cache_root` is `~/.cache/fpkg`; the registry's own directory is
    /// derived from `registry_url`.
    pub fn new(inner: R, cache_root: &Path, registry_url: &str, offline: bool) -> Self {
        CachedRegistry {
            inner,
            dir: cache_root.join(cache_key(registry_url)),
            offline,
            revalidate: Cell::new(false),
            memo: RefCell::new(HashMap::new()),
        }
    }

    /// Revalidate every package document once (what `lock` and `update` do).
    pub fn set_revalidate(&self, on: bool) {
        self.revalidate.set(on);
    }

    pub fn is_offline(&self) -> bool {
        self.offline
    }

    pub fn inner(&self) -> &R {
        &self.inner
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn index_path(&self, id: &str) -> PathBuf {
        self.dir.join("index").join(format!("{id}.json"))
    }

    fn archive_path(&self, id: &str, v: &Version) -> PathBuf {
        self.dir.join("archives").join(format!("{id}-{v}.zip"))
    }

    fn read_entry(&self, id: &str) -> Option<Entry> {
        let bytes = std::fs::read(self.index_path(id)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn write_entry(&self, id: &str, entry: &Entry) -> Result<()> {
        let path = self.index_path(id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::io(parent, e))?;
        }
        let bytes = serde_json::to_vec(entry).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        write_atomic(&path, &bytes).map_err(|e| RegistryError::io(&path, e))
    }

    fn forget(&self, id: &str) {
        self.memo.borrow_mut().remove(id);
        let _ = std::fs::remove_file(self.index_path(id));
    }

    fn needs_network(&self, what: &str) -> Result<()> {
        if self.offline { Err(RegistryError::Invalid(format!("--offline: {what} needs the network"))) } else { Ok(()) }
    }

    fn fetch_doc(&self, id: &PackageId) -> Result<Option<PackageDoc>> {
        let cached = self.read_entry(id.as_str());
        if self.offline {
            return match cached {
                Some(e) => Ok(Some(e.doc)),
                None => Err(RegistryError::Offline(id.to_string())),
            };
        }
        if let Some(e) = &cached
            && !self.revalidate.get()
            && now() - e.fetched_at < TRUST_SECONDS
        {
            return Ok(Some(e.doc.clone()));
        }
        let etag = cached.as_ref().and_then(|e| e.etag.clone());
        match self.inner.package_conditional(id, etag.as_deref())? {
            Conditional::NotModified => match cached {
                Some(mut e) => {
                    e.fetched_at = now();
                    self.write_entry(id.as_str(), &e)?;
                    Ok(Some(e.doc))
                }
                // A 304 to a request without our ETag: fetch plainly.
                None => self.inner.package(id),
            },
            Conditional::Modified { doc, etag } => {
                self.write_entry(id.as_str(), &Entry { etag, fetched_at: now(), doc: doc.clone() })?;
                Ok(Some(doc))
            }
            Conditional::NotFound => {
                self.forget(id.as_str());
                Ok(None)
            }
        }
    }

    /// The archive of `id` `v`, which must hash to `sha256`: from the cache if
    /// a matching file is there, otherwise downloaded, checked and cached.
    pub fn archive_verified(&self, id: &PackageId, v: &Version, sha256: &str) -> Result<Vec<u8>> {
        let path = self.archive_path(id.as_str(), v);
        if let Ok(bytes) = std::fs::read(&path) {
            if crate::sha256_hex(&bytes).eq_ignore_ascii_case(sha256) {
                return Ok(bytes);
            }
            let _ = std::fs::remove_file(&path);
        }
        if self.offline {
            return Err(RegistryError::Offline(format!("the archive of {id} {v}")));
        }
        let bytes = self.inner.archive(id, v)?;
        let actual = crate::sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(sha256) {
            return Err(RegistryError::Checksum { what: format!("{id} {v}"), expected: sha256.to_string(), actual });
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::io(parent, e))?;
        }
        write_atomic(&path, &bytes).map_err(|e| RegistryError::io(&path, e))?;
        Ok(bytes)
    }
}

impl<R: Registry> Registry for CachedRegistry<R> {
    fn package(&self, id: &PackageId) -> Result<Option<PackageDoc>> {
        if let Some(doc) = self.memo.borrow().get(id.as_str()) {
            return Ok(doc.clone());
        }
        let doc = self.fetch_doc(id)?;
        self.memo.borrow_mut().insert(id.to_string(), doc.clone());
        Ok(doc)
    }

    fn version(&self, id: &PackageId, v: &Version) -> Result<Option<VersionMeta>> {
        if let Some(doc) = self.package(id)?
            && let Some(m) = doc.get(v)
        {
            return Ok(Some(m.clone()));
        }
        if self.offline {
            return Err(RegistryError::Offline(format!("{id} {v}")));
        }
        self.inner.version(id, v)
    }

    fn archive(&self, id: &PackageId, v: &Version) -> Result<Vec<u8>> {
        let sha = self.version(id, v)?.and_then(|m| m.dist).map(|d| d.sha256);
        match sha {
            Some(sha) => self.archive_verified(id, v, &sha),
            None => Err(RegistryError::not_found(format!("{id} {v} does not exist"))),
        }
    }

    fn publish(&self, meta: &VersionMeta, archive: &[u8], auth: &Token) -> Result<VersionMeta> {
        self.needs_network("publish")?;
        let r = self.inner.publish(meta, archive, auth);
        self.forget(&meta.id);
        r
    }

    fn yank(&self, id: &PackageId, v: &Version, yanked: bool, auth: &Token) -> Result<()> {
        self.needs_network("yank")?;
        let r = self.inner.yank(id, v, yanked, auth);
        self.forget(id.as_str());
        r
    }

    fn owners(&self, id: &PackageId) -> Result<Vec<String>> {
        self.needs_network("owner list")?;
        self.inner.owners(id)
    }

    fn set_owner(&self, id: &PackageId, login: &str, add: bool, auth: &Token) -> Result<()> {
        self.needs_network("owner changes")?;
        let r = self.inner.set_owner(id, login, add, auth);
        self.forget(id.as_str());
        r
    }

    fn search(&self, query: &str, page: u32) -> Result<SearchPage> {
        self.needs_network("search")?;
        self.inner.search(query, page)
    }

    fn me(&self, auth: &Token) -> Result<Identity> {
        self.needs_network("login")?;
        self.inner.me(auth)
    }

    fn package_conditional(&self, id: &PackageId, etag: Option<&str>) -> Result<Conditional> {
        self.needs_network("revalidation")?;
        self.inner.package_conditional(id, etag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::FileRegistry;

    #[test]
    fn keys() {
        assert_eq!(cache_key("https://fluxpkg.dev/"), "https_fluxpkg.dev_443");
        assert_eq!(cache_key("http://127.0.0.1:8787/"), "http_127.0.0.1_8787");
        assert_ne!(cache_key("http://fluxpkg.dev/"), cache_key("https://fluxpkg.dev/"));
        assert_ne!(cache_key("https://h/a/b/"), cache_key("https://h/a_b/"));
        assert!(cache_key("https://h/mirror/").starts_with("https_h_443_mirror_"));
        assert!(cache_key("file:///tmp/reg/").starts_with("file_"));
        assert_ne!(cache_key("file:///tmp/a/"), cache_key("file:///tmp/b/"));
        assert!(!cache_key("https://h/../../x/").contains('/'));
    }

    #[test]
    fn offline_without_cache_names_the_package() {
        let reg = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let fr = FileRegistry::new(reg.path());
        let url = fr.url().to_string();
        let c = CachedRegistry::new(fr, cache.path(), &url, true);
        let id = PackageId::new("sfs").unwrap();
        assert!(matches!(c.package(&id), Err(RegistryError::Offline(s)) if s == "sfs"));
        assert!(c.search("x", 1).is_err());
    }
}

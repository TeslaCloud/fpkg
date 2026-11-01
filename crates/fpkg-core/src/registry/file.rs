//! The `file://` registry, laid out as the URL space (`packages/<id>.json`,
//! `packages/<id>/versions/<v>.json`, `.../<v>/archive.zip`). No auth; otherwise
//! it refuses what the server refuses.

use std::path::{Path, PathBuf};

use super::{
    ApiError, Conditional, DepMeta, Dist, Identity, PER_PAGE, PackageDoc, Registry, RegistryError, Result, SearchHit,
    SearchPage, Token, VersionMeta, literals,
};
use crate::PackageId;
use crate::lockfile::write_atomic;
use crate::version::{Constraint, Version};

#[derive(Clone, Debug)]
pub struct FileRegistry {
    root: PathBuf,
    url: String,
    login: String,
}

impl FileRegistry {
    pub fn new(root: impl Into<PathBuf>) -> FileRegistry {
        let root: PathBuf = root.into();
        let abs = std::path::absolute(&root).unwrap_or_else(|_| root.clone());
        let url = url::Url::from_directory_path(&abs).map(|u| u.to_string()).unwrap_or_else(|_| format!("file://{}/", abs.display()));
        let login = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_else(|_| "local".into());
        FileRegistry { root: abs, url, login }
    }

    pub fn from_url(url: &str) -> Result<FileRegistry> {
        let u = url::Url::parse(url).map_err(|e| RegistryError::Invalid(format!("invalid registry URL '{url}': {e}")))?;
        let path = u.to_file_path().map_err(|_| RegistryError::Invalid(format!("'{url}' is not a local file URL")))?;
        Ok(FileRegistry::new(path))
    }

    /// The login that publishes and owns packages here (there is no auth).
    pub fn with_login(mut self, login: &str) -> FileRegistry {
        self.login = login.to_string();
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn doc_path(&self, id: &str) -> PathBuf {
        self.root.join("packages").join(format!("{id}.json"))
    }
    fn version_path(&self, id: &str, v: &str) -> PathBuf {
        self.root.join("packages").join(id).join("versions").join(format!("{v}.json"))
    }
    pub fn archive_path(&self, id: &str, v: &str) -> PathBuf {
        self.root.join("packages").join(id).join("versions").join(v).join("archive.zip")
    }

    fn read_json<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<Option<T>> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b)
                .map(Some)
                .map_err(|e| RegistryError::Invalid(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(RegistryError::io(path, e)),
        }
    }

    fn write_json<T: serde::Serialize>(&self, path: &Path, value: &T) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::io(parent, e))?;
        }
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| RegistryError::Invalid(e.to_string()))?;
        bytes.push(b'\n');
        write_atomic(path, &bytes).map_err(|e| RegistryError::io(path, e))
    }

    fn write_doc(&self, doc: &mut PackageDoc) -> Result<()> {
        doc.latest = doc.newest().map(|v| v.to_string());
        self.write_json(&self.doc_path(&doc.id), doc)
    }

    fn archive_url(&self, id: &str, v: &str) -> String {
        url::Url::from_file_path(self.archive_path(id, v))
            .map(|u| u.to_string())
            .unwrap_or_else(|_| self.archive_path(id, v).display().to_string())
    }

    /// Writes a version as given, without validation (yank state included):
    /// what `fpkg mirror` does. The archive must match `meta.dist.sha256`.
    pub fn import(&self, meta: &VersionMeta, archive: &[u8], owners: &[String]) -> Result<()> {
        // Paths below are built from the id and version: both must be in
        // their grammars, and the version is written canonically.
        let id = meta.package_id()?;
        let version = meta.parsed_version()?.to_string();
        let meta = &VersionMeta { id: id.to_string(), version, ..meta.clone() };
        let sha = crate::sha256_hex(archive);
        if let Some(d) = &meta.dist
            && !d.sha256.eq_ignore_ascii_case(&sha)
        {
            return Err(RegistryError::Checksum { what: format!("{} {}", meta.id, meta.version), expected: d.sha256.clone(), actual: sha });
        }
        let ap = self.archive_path(&meta.id, &meta.version);
        if let Some(parent) = ap.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::io(parent, e))?;
        }
        write_atomic(&ap, archive).map_err(|e| RegistryError::io(&ap, e))?;
        let mut m = meta.clone();
        m.dist = Some(Dist { sha256: sha, size: archive.len() as u64, url: self.archive_url(&m.id, &m.version) });
        self.write_json(&self.version_path(&m.id, &m.version), &m)?;
        let mut doc = self.read_json::<PackageDoc>(&self.doc_path(&m.id))?.unwrap_or_else(|| PackageDoc {
            id: m.id.clone(),
            revision: 0,
            owners: owners.to_vec(),
            latest: None,
            versions: Default::default(),
        });
        doc.versions.insert(m.version.clone(), m);
        doc.revision += 1;
        self.write_doc(&mut doc)
    }

    fn etag(doc: &PackageDoc) -> String {
        format!("\"{}-{}\"", doc.id, doc.revision)
    }

    /// The server's publish checks (metadata, archive, spec literals, dependencies), as `422 details`.
    fn validate_publish(&self, meta: &VersionMeta, archive: &[u8]) -> Result<Version> {
        let mut details = Vec::new();
        if let Some(reason) = PackageId::problem(&meta.id) {
            details.push(format!("id: '{}' {reason}", meta.id));
        }
        let version = match Version::parse_strict(&meta.version) {
            Ok(v) if v.to_string() == meta.version => Some(v),
            Ok(v) => {
                details.push(format!("version: '{}' is not canonical ('{v}')", meta.version));
                None
            }
            Err(e) => {
                details.push(format!("version: {e}"));
                None
            }
        };
        if meta.kind != "package" {
            details.push(format!("kind: '{}' is not 'package'", meta.kind));
        }
        if meta.name.trim().is_empty() {
            details.push("name: it is empty".into());
        }
        let mut deps_ok = Vec::new();
        for DepMeta { id, constraint } in &meta.dependencies {
            match (PackageId::problem(id), Constraint::parse(constraint)) {
                (Some(reason), _) => details.push(format!("dependencies: '{id}' {reason}")),
                (None, Err(e)) => details.push(format!("dependencies: {id}: {e}")),
                (None, Ok(c)) => deps_ok.push((id.clone(), c)),
            }
        }
        if details.is_empty() {
            match crate::archive::validate(archive, &meta.id) {
                Err(e) => details.push(format!("archive: {e}")),
                Ok(_) => {
                    let spec = read_member(archive, &format!("{}/packagespec.lua", meta.id)).unwrap_or_default();
                    let versions = literals::assigned_literals(&spec, "version");
                    if versions.is_empty() {
                        details.push("archive: packagespec.lua assigns no version literal".into());
                    }
                    for lit in &versions {
                        match (Version::parse(lit), &version) {
                            (Ok(v), Some(want)) if &v == want => {}
                            _ => details.push(format!(
                                "archive: packagespec.lua says version '{lit}', the request says '{}'",
                                meta.version
                            )),
                        }
                    }
                    for lit in literals::assigned_literals(&spec, "name") {
                        if lit != meta.name {
                            details.push(format!("archive: packagespec.lua says name '{lit}', the metadata says '{}'", meta.name));
                        }
                    }
                }
            }
        }
        if details.is_empty() && !meta.allow_missing_dependencies {
            for (id, c) in &deps_ok {
                let ok = self.read_json::<PackageDoc>(&self.doc_path(id))?.is_some_and(|doc| {
                    doc.sorted_versions().iter().any(|(v, m)| !m.yanked && c.matches(v))
                });
                if !ok {
                    details.push(format!("dependencies: no non-yanked version of '{id}' satisfies '{c}' on this registry"));
                }
            }
        }
        match (details.is_empty(), version) {
            (true, Some(v)) => Ok(v),
            _ => Err(RegistryError::Api(
                ApiError::new(422, "validation_failed", format!("{} {} was refused", meta.id, meta.version)).with_details(details),
            )),
        }
    }

    fn owned_doc(&self, id: &PackageId) -> Result<PackageDoc> {
        self.read_json::<PackageDoc>(&self.doc_path(id.as_str()))?
            .ok_or_else(|| RegistryError::not_found(format!("package '{id}' does not exist")))
    }
}

fn read_member(archive: &[u8], name: &str) -> Option<String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(archive)).ok()?;
    let mut f = z.by_name(name).ok()?;
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).ok()?;
    Some(s)
}

fn now() -> String {
    let t = time::OffsetDateTime::now_utc().replace_nanosecond(0).unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

impl Registry for FileRegistry {
    fn package(&self, id: &PackageId) -> Result<Option<PackageDoc>> {
        self.read_json(&self.doc_path(id.as_str()))
    }

    fn version(&self, id: &PackageId, v: &Version) -> Result<Option<VersionMeta>> {
        self.read_json(&self.version_path(id.as_str(), &v.to_string()))
    }

    fn archive(&self, id: &PackageId, v: &Version) -> Result<Vec<u8>> {
        let p = self.archive_path(id.as_str(), &v.to_string());
        match std::fs::read(&p) {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(RegistryError::not_found(format!("{id} {v} does not exist")))
            }
            Err(e) => Err(RegistryError::io(&p, e)),
        }
    }

    fn publish(&self, meta: &VersionMeta, archive: &[u8], _auth: &Token) -> Result<VersionMeta> {
        if archive.len() as u64 > crate::archive::MAX_COMPRESSED {
            return Err(RegistryError::api(413, "too_large", "the archive is larger than 64 MiB"));
        }
        if PackageId::is_valid(&meta.id) && self.version_path(&meta.id, &meta.version).exists() {
            return Err(RegistryError::api(409, "conflict", format!("{} {} is already published", meta.id, meta.version)));
        }
        let version = self.validate_publish(meta, archive)?;
        let existing = self.read_json::<PackageDoc>(&self.doc_path(&meta.id))?;
        if let Some(doc) = &existing
            && !doc.owners.iter().any(|o| o == &self.login)
        {
            return Err(RegistryError::api(403, "forbidden", format!("{} is not an owner of {}", self.login, meta.id)));
        }
        let sha256 = crate::sha256_hex(archive);
        let ap = self.archive_path(&meta.id, &meta.version);
        if let Some(parent) = ap.parent() {
            std::fs::create_dir_all(parent).map_err(|e| RegistryError::io(parent, e))?;
        }
        write_atomic(&ap, archive).map_err(|e| RegistryError::io(&ap, e))?;
        let mut m = meta.for_publish();
        m.allow_missing_dependencies = false;
        m.version = version.to_string();
        m.dist = Some(Dist { sha256, size: archive.len() as u64, url: self.archive_url(&m.id, &m.version) });
        m.published_at = Some(now());
        m.published_by = Some(self.login.clone());
        self.write_json(&self.version_path(&m.id, &m.version), &m)?;
        let mut doc = existing.unwrap_or_else(|| PackageDoc {
            id: m.id.clone(),
            revision: 0,
            owners: vec![self.login.clone()],
            latest: None,
            versions: Default::default(),
        });
        doc.versions.insert(m.version.clone(), m.clone());
        doc.revision += 1;
        self.write_doc(&mut doc)?;
        Ok(m)
    }

    fn yank(&self, id: &PackageId, v: &Version, yanked: bool, _auth: &Token) -> Result<()> {
        let mut doc = self.owned_doc(id)?;
        let key = v.to_string();
        let Some(m) = doc.versions.get_mut(&key) else {
            return Err(RegistryError::not_found(format!("{id} {v} does not exist")));
        };
        m.yanked = yanked;
        let m = m.clone();
        self.write_json(&self.version_path(id.as_str(), &key), &m)?;
        doc.revision += 1;
        self.write_doc(&mut doc)
    }

    fn owners(&self, id: &PackageId) -> Result<Vec<String>> {
        Ok(self.owned_doc(id)?.owners)
    }

    fn set_owner(&self, id: &PackageId, login: &str, add: bool, _auth: &Token) -> Result<()> {
        let mut doc = self.owned_doc(id)?;
        if add {
            if !doc.owners.iter().any(|o| o == login) {
                doc.owners.push(login.to_string());
            }
        } else {
            if !doc.owners.iter().any(|o| o == login) {
                return Err(RegistryError::not_found(format!("{login} is not an owner of {id}")));
            }
            if doc.owners.len() == 1 {
                return Err(RegistryError::api(409, "last_owner", format!("{login} is the last owner of {id}")));
            }
            doc.owners.retain(|o| o != login);
        }
        doc.revision += 1;
        self.write_doc(&mut doc)
    }

    fn search(&self, query: &str, page: u32) -> Result<SearchPage> {
        let q = query.trim().to_lowercase();
        let dir = self.root.join("packages");
        let mut hits: Vec<(u8, SearchHit)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("json") {
                    continue;
                }
                let Some(doc) = self.read_json::<PackageDoc>(&path)? else { continue };
                let Some(v) = doc.newest() else { continue };
                let Some(m) = doc.get(&v) else { continue };
                let fields = [doc.id.to_lowercase(), m.name.to_lowercase(), m.summary.clone().unwrap_or_default().to_lowercase()];
                // Rank: exact id, id prefix, any prefix, substring.
                let rank = if q.is_empty() {
                    3
                } else if fields[0] == q {
                    0
                } else if fields[0].starts_with(&q) {
                    1
                } else if fields.iter().any(|f| f.starts_with(&q)) {
                    2
                } else if fields.iter().any(|f| f.contains(&q)) {
                    3
                } else {
                    continue;
                };
                hits.push((rank, SearchHit { id: doc.id.clone(), name: m.name.clone(), version: Some(v.to_string()), latest: None, summary: m.summary.clone() }));
            }
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
        let total = hits.len() as u64;
        let page = page.max(1);
        let packages = hits.into_iter().skip(((page - 1) * PER_PAGE) as usize).take(PER_PAGE as usize).map(|h| h.1).collect();
        Ok(SearchPage { packages, page, per_page: PER_PAGE, total })
    }

    fn me(&self, _auth: &Token) -> Result<Identity> {
        Ok(Identity { login: self.login.clone(), token_name: None })
    }

    fn package_conditional(&self, id: &PackageId, etag: Option<&str>) -> Result<Conditional> {
        Ok(match self.package(id)? {
            None => Conditional::NotFound,
            Some(doc) => {
                let tag = Self::etag(&doc);
                if etag == Some(tag.as_str()) {
                    Conditional::NotModified
                } else {
                    Conditional::Modified { doc, etag: Some(tag) }
                }
            }
        })
    }
}

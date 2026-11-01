//! Project layout and discovery, and the `.fpkg.toml` install marker.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::archive::MARKER;
use crate::spec::{SPEC_FILE, SpecLocation};
use crate::version::Version;

pub const PACKAGES_DIR: &str = "packages";
pub const LOCK_FILE: &str = "fpkg.lock";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error(
        "no project found from {0}: looked for a directory with {SPEC_FILE} and {PACKAGES_DIR}/, or an {LOCK_FILE} (use --project <dir>, or fpkg init --project)"
    )]
    NotFound(PathBuf),
    #[error("{0} is not a project: it needs {SPEC_FILE} and {PACKAGES_DIR}/, or an {LOCK_FILE}")]
    NotAProject(PathBuf),
    #[error("{path}: {message}")]
    Marker { path: PathBuf, message: String },
    #[error("{path}")]
    Io { path: PathBuf, source: std::io::Error },
}

/// A project: a directory with a `packagespec.lua` and `packages/`.
#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    /// The gamemode folder name, which `engine.ActiveGamemode()` returns.
    pub id: String,
    /// The package whose directory discovery started in, if any.
    pub current_package: Option<String>,
}

fn is_project_dir(dir: &Path) -> bool {
    (dir.join(SPEC_FILE).is_file() && dir.join(PACKAGES_DIR).is_dir()) || dir.join(LOCK_FILE).is_file()
}

impl Project {
    /// Walks up from `start` to the first directory that has both
    /// `packagespec.lua` and `packages/`, or an `fpkg.lock`.
    pub fn discover(start: &Path) -> Result<Project, ProjectError> {
        let start = start.canonicalize().map_err(|e| ProjectError::Io { path: start.to_path_buf(), source: e })?;
        let mut dir = start.as_path();
        loop {
            if is_project_dir(dir) {
                let mut p = Project::new(dir.to_path_buf());
                p.current_package = current_package(&p.root, &start);
                return Ok(p);
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => return Err(ProjectError::NotFound(start)),
            }
        }
    }

    /// The project at exactly `root` (`--project`).
    pub fn at(root: &Path) -> Result<Project, ProjectError> {
        let root = root.canonicalize().map_err(|e| ProjectError::Io { path: root.to_path_buf(), source: e })?;
        if !is_project_dir(&root) {
            return Err(ProjectError::NotAProject(root));
        }
        let cwd = std::env::current_dir().ok().and_then(|c| c.canonicalize().ok());
        let mut p = Project::new(root);
        p.current_package = cwd.and_then(|c| current_package(&p.root, &c));
        Ok(p)
    }

    /// A project rooted at `root`, without checking the layout.
    pub fn new(root: PathBuf) -> Project {
        let id = project_id(&root);
        Project { root, id, current_package: None }
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(SPEC_FILE)
    }

    pub fn packages_dir(&self) -> PathBuf {
        self.root.join(PACKAGES_DIR)
    }

    pub fn lock_path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    pub fn package_dir(&self, id: &str) -> PathBuf {
        self.packages_dir().join(id)
    }

    pub fn root_location(&self) -> SpecLocation {
        SpecLocation {
            project_root: self.root.clone(),
            dir: self.root.clone(),
            id: self.id.clone(),
            project_id: self.id.clone(),
            is_project_manifest: true,
            // Flux includes the project manifest as `Package:include '<gamemode>'`.
            lua_path: format!("{}/", self.id),
        }
    }

    /// A package in `packages/`, workspace or installed; Flux finds it at
    /// `<gamemode>/packages/<id>/`.
    pub fn package_location(&self, id: &str) -> SpecLocation {
        self.location_of(self.package_dir(id), id, format!("{}/{PACKAGES_DIR}/{id}/", self.id))
    }

    /// A package directory anywhere (a path dependency), with its `LUA` path.
    pub fn location_of(&self, dir: PathBuf, id: &str, lua_path: String) -> SpecLocation {
        SpecLocation {
            project_root: self.root.clone(),
            dir,
            id: id.to_string(),
            project_id: self.id.clone(),
            is_project_manifest: false,
            lua_path,
        }
    }

    /// The `LUA` path of a path dependency, `<gamemode>/<dir>/`.
    pub fn path_dep_lua_path(&self, rel: &str) -> String {
        format!("{}/{}/", self.id, rel.trim_end_matches('/'))
    }

    /// The package directories in `packages/` (those with a spec), sorted by
    /// id. Hidden directories (`.fpkg-tmp-*`) are skipped.
    pub fn packages(&self) -> Result<Vec<LocalPackage>, ProjectError> {
        let dir = self.packages_dir();
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(ProjectError::Io { path: dir, source: e }),
        };
        for entry in rd {
            let entry = entry.map_err(|e| ProjectError::Io { path: dir.clone(), source: e })?;
            let Ok(id) = entry.file_name().into_string() else { continue };
            let path = entry.path();
            if id.starts_with('.') || !path.is_dir() || !path.join(SPEC_FILE).is_file() {
                continue;
            }
            let marker = Marker::read(&path)?;
            out.push(LocalPackage { id, dir: path, marker });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Directories in `packages/` that have no spec but a marker, or neither;
    /// `install` needs the first kind to clean up.
    pub fn marked_dirs(&self) -> Result<Vec<(String, Marker)>, ProjectError> {
        let dir = self.packages_dir();
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(out) };
        for entry in rd.flatten() {
            let Ok(id) = entry.file_name().into_string() else { continue };
            if id.starts_with('.') || !entry.path().is_dir() {
                continue;
            }
            if let Some(m) = Marker::read(&entry.path())? {
                out.push((id, m));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

/// A directory in `packages/`.
#[derive(Clone, Debug)]
pub struct LocalPackage {
    pub id: String,
    pub dir: PathBuf,
    /// Present for registry packages that `fpkg install` put there.
    pub marker: Option<Marker>,
}

impl LocalPackage {
    pub fn is_workspace(&self) -> bool {
        self.marker.is_none()
    }
}

/// `packages/<id>/.fpkg.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub id: String,
    pub version: Version,
    pub registry: String,
    pub sha256: String,
    /// `archive::tree_sha256` of what was extracted: every regular file but
    /// the marker. `verify` and `install` compare the directory against it.
    pub tree_sha256: String,
    pub installed_at: String,
}

impl Marker {
    pub fn read(dir: &Path) -> Result<Option<Marker>, ProjectError> {
        let path = dir.join(MARKER);
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).map(Some).map_err(|e| ProjectError::Marker { path, message: e.to_string() }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ProjectError::Io { path, source: e }),
        }
    }

    /// The marker's text, keys in a fixed order.
    pub fn to_toml(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut s = String::from("# Installed by fpkg from a registry. Do not edit.\n");
        s += &format!("id = {}\n", q(&self.id));
        s += &format!("version = {}\n", q(&self.version.to_string()));
        s += &format!("registry = {}\n", q(&self.registry));
        s += &format!("sha256 = {}\n", q(&self.sha256));
        s += &format!("tree_sha256 = {}\n", q(&self.tree_sha256));
        s += &format!("installed_at = {}\n", q(&self.installed_at));
        s
    }
}

fn current_package(root: &Path, start: &Path) -> Option<String> {
    let rel = start.strip_prefix(root.join(PACKAGES_DIR)).ok()?;
    let first = rel.components().next()?;
    let id = first.as_os_str().to_str()?.to_string();
    root.join(PACKAGES_DIR).join(&id).join(SPEC_FILE).is_file().then_some(id)
}

/// The gamemode's folder name: the quoted first token of `<name>.txt` (a checkout
/// may live under another name, as `flux-ce` holds `flux.txt`), else the directory
/// name made into an id.
pub fn project_id(root: &Path) -> String {
    if let Ok(rd) = std::fs::read_dir(root) {
        let mut found = Vec::new();
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".txt") else { continue };
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            let first = text.trim_start().trim_start_matches('\u{feff}');
            let quoted = [format!("\"{stem}\""), format!("'{stem}'")];
            if quoted.iter().any(|q| first.starts_with(q.as_str())) {
                found.push(stem.to_string());
            }
        }
        if found.len() == 1 {
            return found.remove(0);
        }
    }
    let name = root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut id: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id.insert(0, 'p');
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/flux-ce")
    }

    #[test]
    fn discovers_from_a_package_directory() {
        let p = Project::discover(&fixture().join("packages/cable/lib")).unwrap();
        assert_eq!(p.root, fixture().canonicalize().unwrap());
        assert_eq!(p.current_package.as_deref(), Some("cable"));
        assert_eq!(p.id, "flux");
        let ids: Vec<_> = p.packages().unwrap().into_iter().map(|x| x.id).collect();
        assert_eq!(ids.len(), 16);
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn marker_round_trip() {
        let m = Marker {
            id: "sfs".into(),
            version: Version::parse("7.0.9").unwrap(),
            registry: "https://fluxpkg.dev/".into(),
            sha256: "ab".repeat(32),
            tree_sha256: "cd".repeat(32),
            installed_at: "2026-11-01T18:04:11Z".into(),
        };
        let back: Marker = toml::from_str(&m.to_toml()).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn project_ids() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("My-Schema");
        std::fs::create_dir(&d).unwrap();
        assert_eq!(project_id(&d), "my_schema");
        std::fs::write(d.join("myschema.txt"), "\"myschema\"\n{\n}\n").unwrap();
        assert_eq!(project_id(&d), "myschema");
    }
}

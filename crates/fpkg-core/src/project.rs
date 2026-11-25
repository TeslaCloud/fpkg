//! Project layout and discovery, the packages in `packages/` (top-level, linked and
//! nested), Garry's Mod server trees, and the `.fpkg.toml` install marker.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::PackageId;
use crate::archive::MARKER;
use crate::spec::{Dependency, SPEC_FILE, SpecLocation};
use crate::version::Version;

pub const PACKAGES_DIR: &str = "packages";
pub const LOCK_FILE: &str = "fpkg.lock";
/// The folder of a Garry's Mod server that holds `gamemodes/`, `addons/` and `lua/`.
pub const GARRYSMOD_DIR: &str = "garrysmod";
pub const GAMEMODES_DIR: &str = "gamemodes";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error(
        "not inside an fpkg project (looked from {0} up for a {SPEC_FILE}). Create one with `fpkg new schema <name>` or `fpkg init`, or pass --project <dir>"
    )]
    NotFound(PathBuf),
    #[error(
        "{0} is not a project: it needs {SPEC_FILE} and {PACKAGES_DIR}/, {SPEC_FILE} and a gamemode <name>.txt, or an {LOCK_FILE}"
    )]
    NotAProject(PathBuf),
    #[error(
        "{0} is not a Garry's Mod server folder: it has no garrysmod/ directory and is not one"
    )]
    NotAServer(PathBuf),
    #[error("{path}: {message}")]
    Marker { path: PathBuf, message: String },
    #[error(
        "path dependency '{dep}': its directory name '{id}' is not a package id ({reason}); rename the directory"
    )]
    PathDepId {
        dep: String,
        id: String,
        reason: &'static str,
    },
    #[error("{path}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// A project: a directory with a `packagespec.lua` and `packages/`, a lock, or a
/// gamemode `<name>.txt`.
#[derive(Clone, Debug)]
pub struct Project {
    /// The canonical directory.
    pub root: PathBuf,
    /// The directory as it was reached (absolute, symlinks not resolved), which
    /// tells where a gamemode folder sits in a server tree.
    pub given_root: PathBuf,
    /// The gamemode folder name, which `engine.ActiveGamemode()` returns, or the
    /// package id of a project that is not a gamemode.
    pub id: String,
    /// Whether the root has a gamemode `<id>.txt`.
    pub gamemode: bool,
    /// The server root (the directory that holds `garrysmod/`), when known.
    pub server: Option<PathBuf>,
    /// The package whose directory discovery started in, if any.
    pub current_package: Option<String>,
}

/// Whether `dir` marks a project root by itself: a spec and `packages/`, a
/// lock, or a spec and a gamemode `<name>.txt` (a fresh clone of a schema has
/// neither of the others).
fn marks_project(dir: &Path) -> bool {
    let spec = dir.join(SPEC_FILE).is_file();
    (spec && dir.join(PACKAGES_DIR).is_dir())
        || dir.join(LOCK_FILE).is_file()
        || (spec && gamemode_name(dir).is_some())
}

/// Whether `dir` is a project root: one that `marks_project`, or a directory
/// with a spec and nothing else (a standalone package project, whose root is
/// the package).
pub fn is_project_dir(dir: &Path) -> bool {
    marks_project(dir) || dir.join(SPEC_FILE).is_file()
}

/// Whether `dir` is `<project>/packages/<dir>` of an enclosing project, where a
/// bare spec is a package of that project rather than a project of its own.
fn in_enclosing_packages(dir: &Path) -> bool {
    let Some(parent) = dir.parent() else {
        return false;
    };
    let Some(grand) = parent.parent() else {
        return false;
    };
    parent.file_name().is_some_and(|n| n == PACKAGES_DIR)
        && (grand.join(SPEC_FILE).is_file() || grand.join(LOCK_FILE).is_file())
}

impl Project {
    /// Walks up from `start` (as given: symlinks are not resolved, so that a
    /// gamemode folder linked into a server keeps its place) to the first
    /// project directory. A directory with only a spec is a standalone package
    /// project, unless it sits in the `packages/` of an enclosing project.
    pub fn discover(start: &Path) -> Result<Project, ProjectError> {
        let start = absolute(start)?;
        let real = start.canonicalize().map_err(|e| ProjectError::Io {
            path: start.clone(),
            source: e,
        })?;
        let mut dir = start.as_path();
        loop {
            if marks_project(dir) || (dir.join(SPEC_FILE).is_file() && !in_enclosing_packages(dir))
            {
                let mut p = Project::at_unchecked(dir)?;
                p.current_package = current_package(&p, &start, &real);
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
        let given = absolute(root)?;
        let canonical = given.canonicalize().map_err(|e| ProjectError::Io {
            path: given.clone(),
            source: e,
        })?;
        if !is_project_dir(&canonical) {
            return Err(ProjectError::NotAProject(canonical));
        }
        let mut p = Project::at_unchecked(&given)?;
        let cwd = logical_cwd();
        if let Some(cwd) = cwd {
            let real = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
            p.current_package = current_package(&p, &cwd, &real);
        }
        Ok(p)
    }

    fn at_unchecked(given: &Path) -> Result<Project, ProjectError> {
        let root = given.canonicalize().map_err(|e| ProjectError::Io {
            path: given.to_path_buf(),
            source: e,
        })?;
        let mut p = Project::new(root);
        p.server = server_of_gamemode_dir(given).or(p.server);
        p.given_root = given.to_path_buf();
        Ok(p)
    }

    /// A project rooted at `root`, without checking the layout.
    pub fn new(root: PathBuf) -> Project {
        let gamemode = gamemode_name(&root);
        let id = gamemode.clone().unwrap_or_else(|| project_id(&root));
        let server = server_of_gamemode_dir(&root);
        Project {
            given_root: root.clone(),
            root,
            id,
            gamemode: gamemode.is_some(),
            server,
            current_package: None,
        }
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

    /// A top-level package in `packages/`, workspace or installed; Flux finds it
    /// at `<gamemode>/packages/<id>/`.
    pub fn package_location(&self, id: &str) -> SpecLocation {
        self.location_of(
            self.package_dir(id),
            id,
            format!("{}/{PACKAGES_DIR}/{id}/", self.id),
        )
    }

    /// The location of a package directory found by `scan`, top-level or nested.
    pub fn location(&self, p: &LocalPackage) -> SpecLocation {
        self.location_of(p.dir.clone(), &p.id, format!("{}/{}/", self.id, p.rel))
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

    /// A path dependency of the project manifest (`s.depends '../shared/foo'`):
    /// its directory as written, without a trailing slash, and its spec
    /// location. Its id is the directory name, which has to be a package id.
    pub fn path_dep(&self, dep: &Dependency) -> Result<(String, SpecLocation), ProjectError> {
        let rel = dep.id.trim_end_matches('/');
        let id = dep.package_id().unwrap_or_default();
        if let Some(reason) = PackageId::problem(id) {
            return Err(ProjectError::PathDepId {
                dep: dep.id.clone(),
                id: id.to_string(),
                reason,
            });
        }
        let dir = self.root.join(rel);
        let dir = dir.canonicalize().map_err(|e| ProjectError::Io {
            path: dir.clone(),
            source: e,
        })?;
        Ok((
            rel.to_string(),
            self.location_of(dir, id, self.path_dep_lua_path(rel)),
        ))
    }

    /// The top-level package directories in `packages/` (those with a spec),
    /// sorted by id. Hidden directories (`.fpkg-tmp-*`) are skipped. A symlink
    /// is always a workspace package: its marker, if any, is not read.
    pub fn packages(&self) -> Result<Vec<LocalPackage>, ProjectError> {
        let dir = self.packages_dir();
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => {
                return Err(ProjectError::Io {
                    path: dir,
                    source: e,
                });
            }
        };
        for entry in rd {
            let entry = entry.map_err(|e| ProjectError::Io {
                path: dir.clone(),
                source: e,
            })?;
            let Ok(id) = entry.file_name().into_string() else {
                continue;
            };
            let path = entry.path();
            if id.starts_with('.') || !path.is_dir() || !path.join(SPEC_FILE).is_file() {
                continue;
            }
            let link = read_link(&path);
            let marker = if link.is_some() {
                None
            } else {
                Marker::read(&path)?
            };
            let rel = format!("{PACKAGES_DIR}/{id}");
            out.push(LocalPackage {
                id,
                dir: path,
                rel,
                marker,
                link,
                outer: None,
                outer_is_link: false,
            });
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Every package directory of the project: the top-level ones and the
    /// unmarked packages nested in unmarked top-level directories that have a
    /// `packages/` of their own (a monorepo checkout such as flux-ce linked at
    /// `packages/flux`). An unmarked top-level directory shadows a nested one of
    /// the same id; of two nested ones, the first outer in id order wins. A
    /// marked (installed) top-level directory does not shadow: the nested
    /// workspace package is what the project uses, and `install` removes the
    /// installed copy.
    pub fn scan(&self) -> Result<Scan, ProjectError> {
        let top = self.packages()?;
        let unmarked: BTreeMap<&str, &LocalPackage> = top
            .iter()
            .filter(|p| p.marker.is_none())
            .map(|p| (p.id.as_str(), p))
            .collect();
        let mut nested: Vec<LocalPackage> = Vec::new();
        let mut shadowed = Vec::new();
        for outer in top.iter().filter(|p| p.marker.is_none()) {
            let dir = outer.dir.join(PACKAGES_DIR);
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut found = Vec::new();
            for entry in rd.flatten() {
                let Ok(id) = entry.file_name().into_string() else {
                    continue;
                };
                let path = entry.path();
                if id.starts_with('.') || !path.is_dir() || !path.join(SPEC_FILE).is_file() {
                    continue;
                }
                // What the outer project installed from a registry is not a
                // workspace package: this project gets its own from the lock.
                if path.join(MARKER).is_file() {
                    continue;
                }
                found.push((id, path));
            }
            found.sort();
            for (id, path) in found {
                let rel = format!("{}/{PACKAGES_DIR}/{id}", outer.rel);
                if let Some(winner) = unmarked.get(id.as_str()) {
                    shadowed.push(Shadowed {
                        id,
                        rel,
                        by: winner.rel.clone(),
                        by_top_level: true,
                    });
                    continue;
                }
                if let Some(winner) = nested.iter().find(|n| n.id == id) {
                    shadowed.push(Shadowed {
                        id,
                        rel,
                        by: winner.rel.clone(),
                        by_top_level: false,
                    });
                    continue;
                }
                nested.push(LocalPackage {
                    id,
                    dir: path,
                    rel,
                    marker: None,
                    link: None,
                    outer: Some(outer.id.clone()),
                    outer_is_link: outer.link.is_some(),
                });
            }
        }
        nested.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(Scan {
            top,
            nested,
            shadowed,
        })
    }

    /// The workspace package directory (unmarked top-level, or nested) for `id`.
    pub fn workspace_package(&self, id: &str) -> Result<Option<LocalPackage>, ProjectError> {
        let scan = self.scan()?;
        Ok(scan.workspace().find(|p| p.id == id).cloned())
    }

    /// Directories in `packages/` that have no spec but a marker, or neither;
    /// `install` needs the first kind to clean up. Symlinks are never listed.
    pub fn marked_dirs(&self) -> Result<Vec<(String, Marker)>, ProjectError> {
        let dir = self.packages_dir();
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return Ok(out);
        };
        for entry in rd.flatten() {
            let Ok(id) = entry.file_name().into_string() else {
                continue;
            };
            let path = entry.path();
            if id.starts_with('.') || read_link(&path).is_some() || !path.is_dir() {
                continue;
            }
            if let Some(m) = Marker::read(&path)? {
                out.push((id, m));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }

    /// The symlinks in `packages/`, sorted by name, with their targets as written.
    pub fn links(&self) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.packages_dir()) else {
            return out;
        };
        for entry in rd.flatten() {
            let Ok(id) = entry.file_name().into_string() else {
                continue;
            };
            if id.starts_with('.') {
                continue;
            }
            if let Some(target) = read_link(&entry.path()) {
                out.push((id, target));
            }
        }
        out.sort();
        out
    }
}

/// What `Project::scan` found.
#[derive(Clone, Debug, Default)]
pub struct Scan {
    /// Top-level directories with a spec, sorted by id.
    pub top: Vec<LocalPackage>,
    /// Nested package directories the project uses, sorted by id.
    pub nested: Vec<LocalPackage>,
    /// Nested package directories another one hides.
    pub shadowed: Vec<Shadowed>,
}

impl Scan {
    /// The workspace packages: unmarked top-level directories, then nested ones.
    pub fn workspace(&self) -> impl Iterator<Item = &LocalPackage> {
        self.top
            .iter()
            .filter(|p| p.marker.is_none())
            .chain(self.nested.iter())
    }
}

/// A nested package directory that another directory of the same id hides.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shadowed {
    pub id: String,
    /// Project-relative, `packages/<outer>/packages/<id>`.
    pub rel: String,
    /// The directory that wins, project-relative.
    pub by: String,
    /// Whether the winner is a top-level directory (a warning) rather than
    /// another nested one (an error).
    pub by_top_level: bool,
}

/// A package directory of the project.
#[derive(Clone, Debug)]
pub struct LocalPackage {
    pub id: String,
    /// The directory as the project reaches it (`<root>/packages/<id>` or
    /// `<root>/packages/<outer>/packages/<id>`), not resolved through symlinks.
    pub dir: PathBuf,
    /// Project-relative, forward slashes: `packages/<id>` or
    /// `packages/<outer>/packages/<id>`.
    pub rel: String,
    /// Present for registry packages that `fpkg install` put there.
    pub marker: Option<Marker>,
    /// The target of a top-level symlink, as written.
    pub link: Option<PathBuf>,
    /// For a nested package, the id of the top-level directory it is in.
    pub outer: Option<String>,
    /// Whether that top-level directory is a symlink.
    pub outer_is_link: bool,
}

impl LocalPackage {
    pub fn is_workspace(&self) -> bool {
        self.marker.is_none()
    }

    /// Whether the package is the top-level directory of this project and not
    /// a link: the only kind that `pack` and `publish` take.
    pub fn is_own(&self) -> bool {
        self.marker.is_none() && self.link.is_none() && self.outer.is_none()
    }

    /// The SOURCE column of `fpkg list` for a workspace package.
    pub fn source_label(&self) -> String {
        match (&self.link, &self.outer) {
            (Some(_), _) => "link".into(),
            (None, Some(outer)) if self.outer_is_link => format!("link ({outer})"),
            _ => "workspace".into(),
        }
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
            Ok(s) => toml::from_str(&s)
                .map(Some)
                .map_err(|e| ProjectError::Marker {
                    path,
                    message: e.to_string(),
                }),
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

/// The target of `path` if it is a symlink.
pub fn read_link(path: &Path) -> Option<PathBuf> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        std::fs::read_link(path).ok()
    } else {
        None
    }
}

fn absolute(p: &Path) -> Result<PathBuf, ProjectError> {
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    let cwd = logical_cwd()
        .or_else(|| std::env::current_dir().ok())
        .ok_or_else(|| ProjectError::Io {
            path: p.to_path_buf(),
            source: std::io::Error::other("no current directory"),
        })?;
    Ok(normalize(&cwd.join(p)))
}

/// `.` and `..` removed lexically, as a shell treats a logical path.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The current directory as the shell reached it (`$PWD`, when it names the
/// same directory as the real one), so that a symlinked gamemode folder keeps
/// its place in the server tree.
pub fn logical_cwd() -> Option<PathBuf> {
    let real = std::env::current_dir().ok()?;
    if let Some(pwd) = std::env::var_os("PWD").map(PathBuf::from)
        && pwd.is_absolute()
        && let (Ok(a), Ok(b)) = (pwd.canonicalize(), real.canonicalize())
        && a == b
    {
        return Some(normalize(&pwd));
    }
    Some(real)
}

fn current_package(p: &Project, start: &Path, real: &Path) -> Option<String> {
    let rel = start
        .strip_prefix(p.given_root.join(PACKAGES_DIR))
        .ok()
        .or_else(|| real.strip_prefix(p.root.join(PACKAGES_DIR)).ok())?;
    let mut comps = rel.components().filter_map(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let outer = comps.next()?.to_string();
    let outer_dir = p.packages_dir().join(&outer);
    if !outer_dir.join(SPEC_FILE).is_file() {
        return None;
    }
    if comps.next() == Some(PACKAGES_DIR)
        && let Some(inner) = comps.next()
        && outer_dir
            .join(PACKAGES_DIR)
            .join(inner)
            .join(SPEC_FILE)
            .is_file()
    {
        return Some(inner.to_string());
    }
    Some(outer)
}

/// Whether the first token of a KeyValues text, past a BOM, blank space and
/// `//` comments, is `name` quoted (in any case, as the game reads it).
fn first_key_is(text: &str, name: &str) -> bool {
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        match rest.strip_prefix("//") {
            Some(comment) => rest = comment.split_once('\n').map_or("", |(_, after)| after),
            None => break,
        }
    }
    let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
        return false;
    };
    rest[1..]
        .split_once(quote)
        .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
}

/// The gamemode name of a directory: the stem of its one `<name>.txt` whose
/// first token is that name quoted (a checkout may live under another folder
/// name, as `hl2rp-dev` holds `hl2rp.txt`).
pub fn gamemode_name(root: &Path) -> Option<String> {
    let rd = std::fs::read_dir(root).ok()?;
    let mut found = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".txt") else {
            continue;
        };
        if !entry.path().is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        if first_key_is(&text, stem) {
            found.push(stem.to_string());
        }
    }
    if found.len() == 1 { found.pop() } else { None }
}

/// The `s.id = '<id>'` assignment of a spec, read from the text without
/// evaluating it: an identifier, `.id`, `=` and a quoted string at the start
/// of a line. `fpkg check` compares it with the evaluated value.
pub fn scan_spec_id(text: &str) -> Option<String> {
    for line in text.lines() {
        let l = line.trim_start();
        let ident_len = l
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(l.len());
        if ident_len == 0 || l.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        let Some(rest) = l[ident_len..].strip_prefix(".id") else {
            continue;
        };
        if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        if rest.starts_with('=') {
            continue;
        }
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        let body = &rest[1..];
        let Some(end) = body.find(quote) else {
            continue;
        };
        let value = &body[..end];
        if value.contains('\\') {
            continue;
        }
        return Some(value.to_string());
    }
    None
}

/// The `s.id` of the spec in `dir`, read textually.
pub fn spec_id_of(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(SPEC_FILE))
        .ok()
        .and_then(|t| scan_spec_id(&t))
}

/// The project id of a root directory: the gamemode name of its `<name>.txt`,
/// else the spec's `s.id` (read from the text), else the directory name made
/// into an id.
pub fn project_id(root: &Path) -> String {
    if let Some(name) = gamemode_name(root) {
        return name;
    }
    if let Some(id) = spec_id_of(root).filter(|id| PackageId::is_valid(id)) {
        return id;
    }
    dir_id(root)
}

/// The directory name of `dir` made into an id.
pub fn dir_id(dir: &Path) -> String {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut id: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id.insert(0, 'p');
    }
    id
}

/// The server root of a gamemode folder at `<server>/garrysmod/gamemodes/<name>`.
pub fn server_of_gamemode_dir(dir: &Path) -> Option<PathBuf> {
    let gamemodes = dir.parent()?;
    let garrysmod = gamemodes.parent()?;
    if gamemodes.file_name()? != GAMEMODES_DIR || garrysmod.file_name()? != GARRYSMOD_DIR {
        return None;
    }
    garrysmod.parent().map(Path::to_path_buf)
}

/// Walks up from `start` to a Garry's Mod server tree: a directory named
/// `garrysmod` with `gamemodes/`, or one that has `garrysmod/gamemodes/`.
/// Returns the server root.
pub fn find_server_tree(start: &Path) -> Option<PathBuf> {
    let start = absolute(start).ok()?;
    let mut dir = start.as_path();
    loop {
        if dir.file_name().is_some_and(|n| n == GARRYSMOD_DIR) && dir.join(GAMEMODES_DIR).is_dir() {
            return dir.parent().map(Path::to_path_buf);
        }
        if dir.join(GARRYSMOD_DIR).join(GAMEMODES_DIR).is_dir() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// `--server <dir>`: the server root, or its `garrysmod` directory.
pub fn server_root(dir: &Path) -> Result<PathBuf, ProjectError> {
    let dir = absolute(dir)?;
    if !dir.is_dir() {
        return Err(ProjectError::NotAServer(dir));
    }
    if dir.file_name().is_some_and(|n| n == GARRYSMOD_DIR)
        && let Some(parent) = dir.parent()
    {
        return Ok(parent.to_path_buf());
    }
    if dir.join(GARRYSMOD_DIR).is_dir() {
        return Ok(dir);
    }
    Err(ProjectError::NotAServer(dir))
}

/// The projects among the direct children of `<server>/garrysmod/gamemodes/`, in
/// name order, each with the server root set.
pub fn server_projects(server: &Path) -> Result<Vec<Project>, ProjectError> {
    let dir = server.join(GARRYSMOD_DIR).join(GAMEMODES_DIR);
    let rd = std::fs::read_dir(&dir).map_err(|e| ProjectError::Io {
        path: dir.clone(),
        source: e,
    })?;
    let mut names: BTreeSet<String> = BTreeSet::new();
    for entry in rd.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if !name.starts_with('.') && entry.path().is_dir() && is_project_dir(&entry.path()) {
            names.insert(name);
        }
    }
    let mut out = Vec::new();
    for name in names {
        let mut p = Project::at_unchecked(&dir.join(name))?;
        p.server = Some(server.to_path_buf());
        out.push(p);
    }
    Ok(out)
}

/// Creates `link` as a directory symlink to `target`.
pub fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).map_err(|e| {
            if e.raw_os_error() == Some(1314) {
                std::io::Error::new(
                    e.kind(),
                    "creating a directory symlink needs the symlink privilege: turn on Developer Mode in the Windows settings, or run fpkg from an elevated prompt",
                )
            } else {
                e
            }
        })
    }
}

/// Removes a directory symlink (not what it points to).
pub fn remove_symlink_dir(link: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))
    }
    #[cfg(not(windows))]
    {
        std::fs::remove_file(link)
    }
}

/// The path from directory `from` to `to`, both absolute and normalized, or
/// `None` when they share no root (another drive).
pub fn relative_path(from: &Path, to: &Path) -> Option<PathBuf> {
    let a: Vec<Component> = from.components().collect();
    let b: Vec<Component> = to.components().collect();
    if a.first() != b.first() {
        return None;
    }
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut out = PathBuf::new();
    for _ in common..a.len() {
        out.push("..");
    }
    for c in &b[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    Some(out)
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
        assert_eq!(p.id, "flux", "from s.id");
        assert!(!p.gamemode);
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
        std::fs::write(
            d.join("packagespec.lua"),
            "Package:describe(function(s)\n  s.id      = 'spec_id' -- the package\nend)\n",
        )
        .unwrap();
        assert_eq!(
            project_id(&d),
            "spec_id",
            "s.id comes before the directory name"
        );
        std::fs::write(d.join("myschema.txt"), "\"myschema\"\n{\n}\n").unwrap();
        assert_eq!(project_id(&d), "myschema", "the gamemode name comes first");
        let p = Project::new(d.clone());
        assert!(p.gamemode);
        assert_eq!(p.id, "myschema");
        std::fs::write(
            d.join("packagespec.lua"),
            "Package:describe(function(s) s.id = 'Bad' end)\n",
        )
        .unwrap();
        std::fs::remove_file(d.join("myschema.txt")).unwrap();
        assert_eq!(project_id(&d), "my_schema", "an invalid s.id is ignored");
    }

    #[test]
    fn spec_id_scan() {
        assert_eq!(scan_spec_id("  s.id = 'flux'\n").as_deref(), Some("flux"));
        assert_eq!(scan_spec_id("s.id=\"a_b\" -- x\n").as_deref(), Some("a_b"));
        assert_eq!(scan_spec_id("  spec.id     = 'x'\n").as_deref(), Some("x"));
        assert_eq!(
            scan_spec_id("  s.idx = 'x'\n  s.id == 'y'\n  -- s.id = 'z'\n"),
            None
        );
        assert_eq!(scan_spec_id("  s.id = name\n"), None);
    }

    #[test]
    fn gamemode_projects_are_detected() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("hl2rp-dev");
        std::fs::create_dir(&d).unwrap();
        std::fs::write(
            d.join("packagespec.lua"),
            "Package:describe(function(s) end)\n",
        )
        .unwrap();
        assert!(
            is_project_dir(&d),
            "a spec alone is a standalone package project"
        );
        let p = Project::discover(&d).unwrap();
        assert_eq!((p.id.as_str(), p.gamemode), ("hl2rp_dev", false));
        std::fs::write(d.join("hl2rp.txt"), "\"hl2rp\"\n{\n}\n").unwrap();
        assert!(is_project_dir(&d), "a spec and a gamemode txt are");
        std::fs::write(d.join("readme.txt"), "read me\n").unwrap();
        assert!(is_project_dir(&d), "other .txt files do not count");
        let p = Project::discover(&d).unwrap();
        assert_eq!(p.id, "hl2rp");
        assert!(p.gamemode);
        assert!(p.server.is_none());
    }

    #[test]
    fn server_trees() {
        let t = tempfile::tempdir().unwrap();
        let server = t.path().canonicalize().unwrap().join("srv");
        let gm = server.join("garrysmod/gamemodes/myschema");
        std::fs::create_dir_all(&gm).unwrap();
        std::fs::create_dir_all(server.join("garrysmod/gamemodes/sandbox")).unwrap();
        std::fs::create_dir_all(server.join("garrysmod/addons/x")).unwrap();
        std::fs::write(
            gm.join("packagespec.lua"),
            "Package:describe(function(s) end)\n",
        )
        .unwrap();
        std::fs::write(gm.join("myschema.txt"), "\"myschema\"\n{\n}\n").unwrap();
        std::fs::write(
            server.join("garrysmod/gamemodes/sandbox/sandbox.txt"),
            "\"sandbox\"\n{\n}\n",
        )
        .unwrap();
        assert_eq!(find_server_tree(&server).as_deref(), Some(server.as_path()));
        assert_eq!(
            find_server_tree(&server.join("garrysmod/addons/x")).as_deref(),
            Some(server.as_path())
        );
        assert_eq!(
            find_server_tree(&server.join("garrysmod/gamemodes")).as_deref(),
            Some(server.as_path())
        );
        assert_eq!(server_root(&server.join("garrysmod")).unwrap(), server);
        assert_eq!(server_root(&server).unwrap(), server);
        assert!(server_root(&gm).is_err());
        let ps = server_projects(&server).unwrap();
        assert_eq!(ps.len(), 1, "sandbox has no spec");
        assert_eq!(ps[0].id, "myschema");
        assert_eq!(ps[0].server.as_deref(), Some(server.as_path()));
        assert_eq!(
            Project::at(&gm).unwrap().server.as_deref(),
            Some(server.as_path())
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_gamemode_folder_keeps_its_server() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let checkout = base.join("code/myschema");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join("packagespec.lua"),
            "Package:describe(function(s) end)\n",
        )
        .unwrap();
        std::fs::write(checkout.join("myschema.txt"), "\"myschema\"\n{\n}\n").unwrap();
        let gamemodes = base.join("srv/garrysmod/gamemodes");
        std::fs::create_dir_all(&gamemodes).unwrap();
        std::os::unix::fs::symlink(&checkout, gamemodes.join("myschema")).unwrap();
        let p = Project::at(&gamemodes.join("myschema")).unwrap();
        assert_eq!(p.root, checkout);
        assert_eq!(p.server.as_deref(), Some(base.join("srv").as_path()));
        assert!(Project::at(&checkout).unwrap().server.is_none());
        let p = Project::discover(&gamemodes.join("myschema")).unwrap();
        assert_eq!(p.server.as_deref(), Some(base.join("srv").as_path()));
    }

    #[cfg(unix)]
    #[test]
    fn nested_packages_and_shadowing() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let spec = "Package:describe(function(s) s.version = '1.0.0' end)\n";
        let mono = base.join("mono");
        for d in [
            "",
            "packages/a",
            "packages/b",
            "packages/c",
            "packages/installed",
        ] {
            std::fs::create_dir_all(mono.join(d)).unwrap();
            std::fs::write(mono.join(d).join("packagespec.lua"), spec).unwrap();
        }
        // What mono installed from a registry is not a workspace package here.
        std::fs::write(mono.join("packages/installed").join(MARKER), "").unwrap();
        let other = base.join("other");
        for d in ["", "packages/c"] {
            std::fs::create_dir_all(other.join(d)).unwrap();
            std::fs::write(other.join(d).join("packagespec.lua"), spec).unwrap();
        }
        let proj = base.join("proj");
        std::fs::create_dir_all(proj.join("packages/b")).unwrap();
        std::fs::write(proj.join("packagespec.lua"), spec).unwrap();
        std::fs::write(proj.join("packages/b/packagespec.lua"), spec).unwrap();
        std::os::unix::fs::symlink("../../mono", proj.join("packages/mono")).unwrap();
        std::os::unix::fs::symlink(&other, proj.join("packages/zother")).unwrap();
        let p = Project::at(&proj).unwrap();
        let scan = p.scan().unwrap();
        let top: Vec<_> = scan
            .top
            .iter()
            .map(|x| (x.id.as_str(), x.link.is_some()))
            .collect();
        assert_eq!(top, vec![("b", false), ("mono", true), ("zother", true)]);
        let nested: Vec<_> = scan
            .nested
            .iter()
            .map(|x| (x.id.as_str(), x.rel.as_str()))
            .collect();
        assert_eq!(
            nested,
            vec![
                ("a", "packages/mono/packages/a"),
                ("c", "packages/mono/packages/c")
            ]
        );
        assert_eq!(scan.nested[0].source_label(), "link (mono)");
        assert_eq!(
            p.location(&scan.nested[0]).lua_path,
            "proj/packages/mono/packages/a/"
        );
        assert_eq!(
            scan.shadowed,
            vec![
                Shadowed {
                    id: "b".into(),
                    rel: "packages/mono/packages/b".into(),
                    by: "packages/b".into(),
                    by_top_level: true
                },
                Shadowed {
                    id: "c".into(),
                    rel: "packages/zother/packages/c".into(),
                    by: "packages/mono/packages/c".into(),
                    by_top_level: false
                },
            ]
        );
        assert_eq!(
            p.links(),
            vec![
                ("mono".to_string(), PathBuf::from("../../mono")),
                ("zother".to_string(), other.clone())
            ]
        );
        assert!(p.workspace_package("a").unwrap().is_some());
        assert!(p.marked_dirs().unwrap().is_empty());
        let start = proj.join("packages/mono/packages/a");
        assert_eq!(Project::at(&proj).map(|_| ()).ok(), Some(()));
        let found = Project::discover(&start).unwrap();
        assert_eq!(
            found.root, mono,
            "a linked monorepo is its own project from inside"
        );
    }

    #[test]
    fn gamemode_txt_is_read_as_the_game_reads_it() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        std::fs::write(dir.join("readme.txt"), "\"hl2rp\" is a schema\n").unwrap();
        for text in [
            "\"hl2rp\"\n{\n}\n",
            "\u{feff}// The gamemode.\r\n  // Two comments.\n\"HL2RP\"\n{\n}\n",
            "'hl2rp' {}",
        ] {
            std::fs::write(dir.join("hl2rp.txt"), text).unwrap();
            assert_eq!(gamemode_name(dir).as_deref(), Some("hl2rp"), "{text:?}");
        }
        for text in ["", "// \"hl2rp\"\n", "\"hl2rp_base\"\n{\n}\n", "hl2rp\n"] {
            std::fs::write(dir.join("hl2rp.txt"), text).unwrap();
            assert_eq!(gamemode_name(dir), None, "{text:?}");
        }
        std::fs::write(dir.join("hl2rp.txt"), "\"hl2rp\" {}").unwrap();
        std::fs::write(dir.join("other.txt"), "\"other\" {}").unwrap();
        assert_eq!(gamemode_name(dir), None, "two candidates");
    }

    #[test]
    fn standalone_package_projects() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let repo = base.join("gmsv_file");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(
            repo.join("packagespec.lua"),
            "Package:describe(function(s)\n  s.id = 'gmsv_file'\nend)\n",
        )
        .unwrap();
        let p = Project::discover(&repo.join("src")).unwrap();
        assert_eq!(p.root, repo);
        assert_eq!(p.id, "gmsv_file");
        assert!(!p.gamemode);
        assert!(p.current_package.is_none());
        let p = Project::discover(&fixture().join("packages/cable")).unwrap();
        assert_eq!(
            p.root,
            fixture().canonicalize().unwrap(),
            "a package of an enclosing project is not a project of its own"
        );
        assert_eq!(p.current_package.as_deref(), Some("cable"));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_path(Path::new("/x/schema/packages"), Path::new("/x/flux-ce")).unwrap(),
            PathBuf::from("../../flux-ce")
        );
        assert_eq!(
            relative_path(Path::new("/a"), Path::new("/a/b")).unwrap(),
            PathBuf::from("b")
        );
        assert_eq!(
            normalize(Path::new("/a/b/../c/./d")),
            PathBuf::from("/a/c/d")
        );
    }
}

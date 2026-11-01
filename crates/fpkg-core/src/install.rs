//! Loading a project's state, locking, `install` and `verify`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::PackageId;
use crate::archive::ArchiveError;
use crate::lockfile::{Current, LockError, LockedPackage, Lockfile, Source, Stale, staleness};
use crate::project::{LocalPackage, Marker, Project, ProjectError};
use crate::registry::{Registries, RegistryError};
use crate::resolve::{Release, ResolveError, ResolveInput, resolve};
use crate::spec::{self, DepKind, Env, Manifest, SpecError, SpecLocation};
use crate::version::Version;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Spec(#[from] SpecError),
    #[error(transparent)]
    Project(#[from] ProjectError),
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// The message is the id; the archive error is the source.
    #[error("{id}")]
    Archive {
        id: String,
        #[source]
        source: ArchiveError,
    },
    #[error("fpkg.lock is stale:\n{}", fmt_stale(.0))]
    Stale(Vec<Stale>),
    #[error("there is no fpkg.lock (run fpkg lock, or fpkg install without --locked)")]
    NoLock,
    #[error("{}", conflict_message(.0))]
    Conflict(Vec<String>),
    #[error("{}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn fmt_stale(v: &[Stale]) -> String {
    v.iter().map(|s| format!("  - {s}")).collect::<Vec<_>>().join("\n")
}

fn conflict_message(ids: &[String]) -> String {
    let dirs = ids.iter().map(|id| format!("packages/{id}")).collect::<Vec<_>>().join(", ");
    let (verb, pron, are) = if ids.len() == 1 { ("exists", "it", "is") } else { ("exist", "them", "are") };
    format!(
        "{dirs} {verb} without an .fpkg.toml marker but {are} locked as registry {}; someone copied {pron} in by hand.\nRemove {pron}, run `fpkg lock` to use {pron} as workspace {}, or `fpkg install --force` to replace {pron}",
        if ids.len() == 1 { "package" } else { "packages" },
        if ids.len() == 1 { "package" } else { "packages" },
    )
}

/// Everything a command needs to know about a project.
#[derive(Clone)]
pub struct Loaded {
    pub project: Project,
    pub root: Manifest,
    /// Unmarked package directories, by id.
    pub workspace: BTreeMap<String, Manifest>,
    /// Marked (installed) package directories, by id.
    pub installed: BTreeMap<String, LocalPackage>,
    /// Path dependencies, by id: project-relative directory and manifest.
    pub paths: BTreeMap<String, (String, Manifest)>,
    /// Unmarked directories that the lock records as registry packages.
    pub conflicts: Vec<String>,
}

impl Loaded {
    /// The project manifest's package and path dependencies.
    pub fn root_deps(&self) -> Vec<crate::spec::Dependency> {
        self.root.package_deps().cloned().collect()
    }

    pub fn staleness(&self, lock: &Lockfile) -> Vec<Stale> {
        let deps = self.root_deps();
        staleness(lock, &Current { root_deps: &deps, workspace: &self.workspace, paths: &self.paths })
    }

    /// Resolves against the default registry and returns the lock.
    pub fn resolve(&self, lock: Option<&Lockfile>, release: Release, registries: &Registries) -> Result<Lockfile, InstallError> {
        let deps = self.root_deps();
        let reg = registries.default_registry()?;
        let url = registries.default_url().to_string();
        let r: &dyn crate::registry::Registry = &*reg;
        Ok(resolve(&ResolveInput {
            project: (&self.project.id, &self.root.version),
            root_deps: &deps,
            workspace: &self.workspace,
            paths: &self.paths,
            lock,
            release,
            registry: Some((r, &url)),
        })?)
    }
}

/// Reads the project, evaluating specs in parallel. With `lock`, unmarked
/// directories it has as registry packages are `conflicts`; otherwise they are
/// workspace packages, which shadow the registry.
pub fn load(project: &Project, env: Env, lock: Option<&Lockfile>) -> Result<Loaded, InstallError> {
    let mut installed = BTreeMap::new();
    let mut conflicts = Vec::new();
    let mut locs: Vec<SpecLocation> = vec![project.root_location()];
    for p in project.packages()? {
        if p.marker.is_some() {
            installed.insert(p.id.clone(), p);
        } else if lock.and_then(|l| l.get(&p.id)).is_some_and(|l| matches!(l.source, Source::Registry(_))) {
            conflicts.push(p.id.clone());
        } else {
            locs.push(project.package_location(&p.id));
        }
    }
    let mut results = spec::load_many(&locs, env).into_iter();
    let root = results.next().unwrap_or(Err(SpecError::NoEvaluator))?;
    let mut workspace = BTreeMap::new();
    for (loc, r) in locs.iter().skip(1).zip(results) {
        workspace.insert(loc.id.clone(), r?);
    }
    let mut path_locs = Vec::new();
    for d in root.dependencies.iter().filter(|d| d.kind == DepKind::Path) {
        let rel = d.id.trim_end_matches('/').to_string();
        let Some(id) = d.package_id().map(str::to_string) else { continue };
        let dir = project.root.join(&rel);
        let dir = dir.canonicalize().map_err(|e| InstallError::Io { path: dir.clone(), source: e })?;
        path_locs.push((rel.clone(), project.location_of(dir, &id, project.path_dep_lua_path(&rel))));
    }
    let locs: Vec<SpecLocation> = path_locs.iter().map(|(_, l)| l.clone()).collect();
    let mut paths = BTreeMap::new();
    for ((rel, loc), r) in path_locs.into_iter().zip(spec::load_many(&locs, env)) {
        paths.insert(loc.id.clone(), (rel, r?));
    }
    Ok(Loaded { project: project.clone(), root, workspace, installed, paths, conflicts })
}

/// What `install` reports as it goes.
#[derive(Clone, Debug)]
pub enum Event {
    Resolving,
    LockWritten,
    Fetching { id: String, version: Version },
    Repairing { id: String, version: Version },
    Removing { id: String, version: Version },
    Replacing { id: String },
    Restored { id: String },
}

#[derive(Clone, Debug, Default)]
pub struct InstallReport {
    pub added: Vec<(String, Version)>,
    pub updated: Vec<(String, Version, Version)>,
    /// Installed packages that did not match their marker's tree hash, reinstalled.
    pub repaired: Vec<(String, Version)>,
    pub removed: Vec<(String, Version)>,
    pub unchanged: Vec<(String, Version)>,
    /// Why the lock was re-resolved, if it was.
    pub stale: Vec<Stale>,
    pub lock_written: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InstallOptions {
    pub locked: bool,
    pub force: bool,
    pub env: Env,
}

fn now_rfc3339() -> String {
    let t = time::OffsetDateTime::now_utc();
    let t = t.replace_nanosecond(0).unwrap_or(t);
    t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
}

/// Puts back packages whose replacement was interrupted between its two
/// renames: `.fpkg-old-<id>` exists and `<id>` does not.
pub fn recover_interrupted(project: &Project) -> Result<Vec<String>, InstallError> {
    let dir = project.packages_dir();
    let mut restored = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else { return Ok(restored) };
    for entry in rd.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix(".fpkg-old-") else { continue };
        let dest = dir.join(id);
        if !dest.exists() {
            std::fs::rename(entry.path(), &dest).map_err(|e| InstallError::Io { path: dest.clone(), source: e })?;
            restored.push(id.to_string());
        }
    }
    Ok(restored)
}

/// `fpkg install`.
pub fn install(
    project: &Project,
    opts: InstallOptions,
    registries: &Registries,
    progress: &mut dyn FnMut(Event),
) -> Result<InstallReport, InstallError> {
    let mut report = InstallReport::default();
    for id in recover_interrupted(project)? {
        progress(Event::Restored { id });
    }
    let existing = Lockfile::read(&project.lock_path())?;
    let loaded = load(project, opts.env, existing.as_ref())?;
    if !loaded.conflicts.is_empty() && !opts.force {
        return Err(InstallError::Conflict(loaded.conflicts.clone()));
    }

    // 1. The lock: resolve if missing or stale, unless --locked.
    let lock = match existing {
        Some(l) => {
            let stale = loaded.staleness(&l);
            if stale.is_empty() {
                l
            } else if opts.locked {
                return Err(InstallError::Stale(stale));
            } else {
                progress(Event::Resolving);
                let new = loaded.resolve(Some(&l), Release::None, registries)?;
                report.stale = stale;
                if new != l {
                    new.write(&project.lock_path())?;
                    report.lock_written = true;
                    progress(Event::LockWritten);
                }
                new
            }
        }
        None if opts.locked => return Err(InstallError::NoLock),
        None => {
            progress(Event::Resolving);
            let new = loaded.resolve(None, Release::None, registries)?;
            new.write(&project.lock_path())?;
            report.lock_written = true;
            progress(Event::LockWritten);
            new
        }
    };

    // 2. Fetch what is missing, different, or does not match its tree hash.
    let packages_dir = project.packages_dir();
    std::fs::create_dir_all(&packages_dir).map_err(|e| InstallError::Io { path: packages_dir.clone(), source: e })?;
    for entry in &lock.packages {
        let (Source::Registry(url), Some(sha)) = (&entry.source, &entry.sha256) else { continue };
        let dir = project.package_dir(&entry.id);
        let marker = if dir.is_dir() { Marker::read(&dir)? } else { None };
        let unmarked = dir.exists() && marker.is_none();
        if unmarked && !opts.force {
            return Err(InstallError::Conflict(vec![entry.id.clone()]));
        }
        let mut repair = false;
        if let Some(m) = &marker
            && m.version == entry.version
            && m.sha256.eq_ignore_ascii_case(sha)
        {
            if tree_matches(&dir, m) {
                report.unchanged.push((entry.id.clone(), entry.version.clone()));
                continue;
            }
            repair = true;
        }
        if unmarked {
            progress(Event::Replacing { id: entry.id.clone() });
        }
        if repair {
            progress(Event::Repairing { id: entry.id.clone(), version: entry.version.clone() });
        } else {
            progress(Event::Fetching { id: entry.id.clone(), version: entry.version.clone() });
        }
        fetch_and_extract(project, entry, url, sha, registries, opts.env)?;
        match marker {
            _ if repair => report.repaired.push((entry.id.clone(), entry.version.clone())),
            Some(m) => report.updated.push((entry.id.clone(), m.version, entry.version.clone())),
            None => report.added.push((entry.id.clone(), entry.version.clone())),
        }
    }

    // 3. Remove marked directories that the lock does not have.
    let wanted: BTreeSet<&str> =
        lock.packages.iter().filter(|p| matches!(p.source, Source::Registry(_))).map(|p| p.id.as_str()).collect();
    for (id, marker) in project.marked_dirs()? {
        if wanted.contains(id.as_str()) {
            continue;
        }
        progress(Event::Removing { id: id.clone(), version: marker.version.clone() });
        let dir = project.package_dir(&id);
        std::fs::remove_dir_all(&dir).map_err(|e| InstallError::Io { path: dir, source: e })?;
        report.removed.push((id, marker.version));
    }
    Ok(report)
}

/// Whether an installed directory still hashes to its marker's tree hash.
fn tree_matches(dir: &Path, marker: &Marker) -> bool {
    crate::archive::tree_sha256(dir).is_ok_and(|t| t.eq_ignore_ascii_case(&marker.tree_sha256))
}

fn fetch_and_extract(
    project: &Project,
    entry: &LockedPackage,
    url: &str,
    sha: &str,
    registries: &Registries,
    env: Env,
) -> Result<(), InstallError> {
    let reg = registries.archive_source(url)?;
    let id = PackageId::new(entry.id.clone()).map_err(|e| RegistryError::Invalid(e.to_string()))?;
    // The lockfile's checksum, not the registry's, is what is verified.
    let bytes = reg.archive_verified(&id, &entry.version, sha)?;
    // The staged spec must evaluate (with its final PACKAGE.__path__) to the
    // locked version; the marker records its tree hash.
    let check = |staged: &Path| -> Result<(), String> {
        let loc = SpecLocation { dir: staged.to_path_buf(), ..project.package_location(&entry.id) };
        let m = spec::load(&loc, env).map_err(|e| format!("its packagespec.lua does not evaluate: {e}"))?;
        if m.version != entry.version {
            return Err(format!("its packagespec.lua says version {}, the lock says {}", m.version, entry.version));
        }
        let tree_sha256 = crate::archive::tree_sha256(staged).map_err(|e| e.to_string())?;
        let marker = Marker {
            id: entry.id.clone(),
            version: entry.version.clone(),
            registry: url.to_string(),
            sha256: sha.to_ascii_lowercase(),
            tree_sha256,
            installed_at: now_rfc3339(),
        };
        let path = staged.join(crate::archive::MARKER);
        std::fs::write(&path, marker.to_toml()).map_err(|e| format!("{}: {e}", path.display()))
    };
    crate::archive::unpack_checked(&bytes, &entry.id, sha, &project.packages_dir(), None, &check)
        .map_err(|source| InstallError::Archive { id: entry.id.clone(), source })
}

/// One difference `verify` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub id: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct VerifyReport {
    pub ok: Vec<(String, Version)>,
    pub problems: Vec<Problem>,
}

/// `fpkg verify`: installed registry packages must match the lock and their
/// marker's tree hash; workspace and path packages, their locked version.
pub fn verify(project: &Project, env: Env) -> Result<VerifyReport, InstallError> {
    let lock = Lockfile::read(&project.lock_path())?.ok_or(InstallError::NoLock)?;
    let mut report = VerifyReport::default();
    let mut problems = Vec::new();
    let mut problem = |id: &str, message: String| problems.push(Problem { id: id.to_string(), message });
    // Specs to evaluate, checked against the lock afterwards.
    let mut to_eval: Vec<(SpecLocation, Version)> = Vec::new();
    for entry in &lock.packages {
        let id = entry.id.as_str();
        match &entry.source {
            Source::Registry(_) => {
                let dir = project.package_dir(id);
                if !dir.is_dir() {
                    problem(id, "is not installed".into());
                    continue;
                }
                let marker = match Marker::read(&dir) {
                    Ok(Some(m)) => m,
                    Ok(None) => {
                        problem(id, "has no .fpkg.toml marker (copied in by hand?)".into());
                        continue;
                    }
                    Err(e) => {
                        problem(id, format!("has an unreadable marker: {e}"));
                        continue;
                    }
                };
                let sha = entry.sha256.as_deref().unwrap_or_default();
                if marker.version != entry.version || !marker.sha256.eq_ignore_ascii_case(sha) {
                    problem(id, format!("is installed at {} ({}), the lock has {} ({})", marker.version, short(&marker.sha256), entry.version, short(sha)));
                    continue;
                }
                match crate::archive::tree_sha256(&dir) {
                    Ok(t) if t.eq_ignore_ascii_case(&marker.tree_sha256) => {}
                    Ok(t) => {
                        problem(id, format!("was modified after install: its files hash to {}, the marker has {}", short(&t), short(&marker.tree_sha256)));
                        continue;
                    }
                    Err(e) => {
                        problem(id, format!("cannot be hashed: {e}"));
                        continue;
                    }
                }
                to_eval.push((project.package_location(id), entry.version.clone()));
            }
            Source::Workspace | Source::Path(_) => {
                let (dir, loc) = match &entry.source {
                    Source::Path(rel) => {
                        let dir = project.root.join(rel);
                        (dir.clone(), project.location_of(dir, id, project.path_dep_lua_path(rel)))
                    }
                    _ => (project.package_dir(id), project.package_location(id)),
                };
                if !dir.join(spec::SPEC_FILE).is_file() {
                    problem(id, format!("is locked as {} but {} has no spec", entry.source, dir.display()));
                    continue;
                }
                if matches!(entry.source, Source::Workspace) && Marker::read(&dir)?.is_some() {
                    problem(id, "is locked as a workspace package but is installed from a registry".into());
                    continue;
                }
                to_eval.push((loc, entry.version.clone()));
            }
        }
    }
    let locs: Vec<SpecLocation> = to_eval.iter().map(|(l, _)| l.clone()).collect();
    for ((loc, want), r) in to_eval.iter().zip(spec::load_many(&locs, env)) {
        match r {
            Ok(m) if &m.version == want => report.ok.push((loc.id.clone(), want.clone())),
            Ok(m) => problem(&loc.id, format!("its spec says {}, the lock has {want}", m.version)),
            Err(e) => problem(&loc.id, format!("its spec does not evaluate: {e}")),
        }
    }
    for (id, marker) in project.marked_dirs()? {
        if lock.get(&id).is_none() {
            problem(&id, format!("is installed ({}) but not in the lock", marker.version));
        }
    }
    report.problems = problems;
    Ok(report)
}

fn short(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

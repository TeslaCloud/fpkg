//! Materialized files: the `content/` of every package merged into a gamemode
//! project's `content/`, and binary modules copied to the server's
//! `garrysmod/lua/bin/`, recorded in `packages/.fpkg-files.toml`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::install::{Event, InstallError, InstallReport};
use crate::project::{GARRYSMOD_DIR, Project};

/// The state file, in `packages/` (hidden, so package listing skips it).
pub const STATE_FILE: &str = ".fpkg-files.toml";
/// The state schema this fpkg writes and reads.
pub const STATE_VERSION: u32 = 1;
/// The package folder whose files are merged into the gamemode's `content/`.
pub const CONTENT_DIR: &str = "content";
/// The prefix of recorded binary modules, which live in `garrysmod/lua/bin/`.
pub const BIN_PREFIX: &str = "bin/";

/// A package whose files are materialized.
#[derive(Clone, Debug)]
pub struct Source {
    pub id: String,
    /// The package directory (through a symlink, for a linked package).
    pub dir: PathBuf,
    /// Its binaries folder, relative (`s.binaries`, `bin` by default).
    pub binaries: String,
}

/// One materialized file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// `content/...` (relative to the project) or `bin/<name>` (in `lua/bin/`).
    pub path: String,
    /// The package that provides it.
    pub package: String,
    /// The SHA-256 of what was written.
    pub sha256: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawState {
    version: u32,
    #[serde(default)]
    file: Vec<FileEntry>,
}

/// The state file's entries, by path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub files: BTreeMap<String, FileEntry>,
}

impl State {
    pub fn path(project: &Project) -> PathBuf {
        project.packages_dir().join(STATE_FILE)
    }

    pub fn read(project: &Project) -> Result<State, InstallError> {
        let path = State::path(project);
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(e) => return Err(InstallError::Io { path, source: e }),
        };
        let bad = |m: String| InstallError::Io {
            path: path.clone(),
            source: std::io::Error::other(m),
        };
        let raw: RawState = toml::from_str(&text).map_err(|e| bad(e.to_string()))?;
        if raw.version > STATE_VERSION {
            return Err(bad(format!(
                "schema version {} is newer than this fpkg understands ({STATE_VERSION}); upgrade fpkg",
                raw.version
            )));
        }
        // The file can come with a checkout: a path that leaves `content/` or
        // `lua/bin/` would make install remove files elsewhere.
        if let Some(f) = raw.file.iter().find(|f| !is_recordable(&f.path)) {
            return Err(bad(format!(
                "'{}' is not a path fpkg records (content/<path> or bin/<name>); delete the file and run fpkg install",
                f.path
            )));
        }
        Ok(State {
            files: raw.file.into_iter().map(|f| (f.path.clone(), f)).collect(),
        })
    }

    /// The file's text: entries sorted by path, keys in a fixed order.
    pub fn to_toml(&self) -> String {
        let q = |s: &str| toml::Value::String(s.to_string()).to_string();
        let mut out =
            String::from("# Files fpkg install copied out of the packages. Do not edit.\n");
        out += &format!("version = {STATE_VERSION}\n");
        for f in self.files.values() {
            out += "\n[[file]]\n";
            out += &format!("path = {}\n", q(&f.path));
            out += &format!("package = {}\n", q(&f.package));
            out += &format!("sha256 = {}\n", q(&f.sha256));
        }
        out
    }

    /// Writes the state, or removes the file when there is nothing to record.
    fn write(&self, project: &Project) -> Result<(), InstallError> {
        let path = State::path(project);
        if self.files.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(InstallError::Io { path, source: e })
                }
                _ => Ok(()),
            };
        }
        crate::lockfile::write_atomic(&path, self.to_toml().as_bytes())
            .map_err(|e| InstallError::Io { path, source: e })
    }
}

/// Whether `path` is one fpkg records: `content/<rel>` with `rel` a valid
/// archive member path, or `bin/<name>` with a single file name.
fn is_recordable(path: &str) -> bool {
    if let Some(name) = path.strip_prefix(BIN_PREFIX) {
        return !name.contains('/') && crate::archive::check_member_path(name).is_ok();
    }
    path.strip_prefix(&format!("{CONTENT_DIR}/"))
        .is_some_and(|rel| crate::archive::check_member_path(rel).is_ok())
}

/// Where a recorded path lives on disk, if it can be known.
pub fn destination(project: &Project, path: &str) -> Option<PathBuf> {
    if let Some(name) = path.strip_prefix(BIN_PREFIX) {
        return project
            .server
            .as_ref()
            .map(|s| s.join(GARRYSMOD_DIR).join("lua").join("bin").join(name));
    }
    path.starts_with(&format!("{CONTENT_DIR}/"))
        .then(|| path.split('/').fold(project.root.clone(), |p, c| p.join(c)))
}

/// The SHA-256 of a file, read in chunks: content files can be large.
fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hex::encode(hash.finalize()));
        }
        hash.update(&buf[..n]);
    }
}

fn is_symlink(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// The server binaries that tell which modules a server loads, and the
/// platform suffix of those modules (`gmsv_<name>_<platform>.dll`).
const SERVER_BINARIES: &[(&str, &str)] = &[
    ("srcds_run", "linux"),
    ("srcds_linux", "linux"),
    ("bin/linux32/srcds", "linux"),
    ("srcds_run_x64", "linux64"),
    ("srcds_linux64", "linux64"),
    ("bin/linux64/srcds", "linux64"),
    ("srcds.exe", "win32"),
    ("srcds_win64.exe", "win64"),
    ("bin/win64/srcds.exe", "win64"),
];

/// The module platforms a server folder can run (`linux`, `linux64`,
/// `win32`, `win64`), from the binaries in it; `None` when it has none of
/// them, and every module is copied.
pub fn server_platforms(server: &Path) -> Option<Vec<&'static str>> {
    let mut out: Vec<&'static str> = SERVER_BINARIES
        .iter()
        .filter(|(file, _)| server.join(file).is_file())
        .map(|(_, platform)| *platform)
        .collect();
    out.sort_unstable();
    out.dedup();
    (!out.is_empty()).then_some(out)
}

/// The platform a module's name ends in (`gmsv_pg_linux64.dll` is `linux64`),
/// if it is one Garry's Mod knows.
fn module_platform(name: &str) -> Option<&'static str> {
    let stem = name.get(..name.len().checked_sub(4)?)?;
    let suffix = stem.rsplit('_').next()?;
    ["linux", "linux64", "win32", "win64", "osx", "osx64"]
        .into_iter()
        .find(|p| suffix.eq_ignore_ascii_case(p))
}

/// A file a package wants in place.
struct Wanted {
    package: String,
    src: PathBuf,
    sha256: String,
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |e| InstallError::Io {
        path: path.to_path_buf(),
        source: e,
    }
}

/// What the packages provide: `content/**` in a gamemode project and the
/// modules in each binaries folder. Symlinks are skipped with an event.
fn wanted(
    project: &Project,
    sources: &[Source],
    report: &mut InstallReport,
    progress: &mut dyn FnMut(Event),
) -> Result<BTreeMap<String, Wanted>, InstallError> {
    let platforms = project.server.as_deref().and_then(server_platforms);
    let mut out: BTreeMap<String, Wanted> = BTreeMap::new();
    let mut add = |path: String, package: &str, src: PathBuf| -> Result<(), InstallError> {
        let sha256 = sha256_file(&src).map_err(io(&src))?;
        if let Some(prev) = out.get(&path) {
            if prev.sha256 != sha256 {
                return Err(InstallError::FileClash {
                    path,
                    first: prev.package.clone(),
                    second: package.to_string(),
                });
            }
            return Ok(());
        }
        out.insert(
            path,
            Wanted {
                package: package.to_string(),
                src,
                sha256,
            },
        );
        Ok(())
    };
    for s in sources {
        if project.gamemode {
            let content = s.dir.join(CONTENT_DIR);
            if is_symlink(&content) {
                progress(Event::SkippedSymlink { path: content });
            } else if content.is_dir() {
                let mut files = Vec::new();
                for entry in walkdir::WalkDir::new(&content)
                    .follow_links(false)
                    .min_depth(1)
                {
                    let entry = entry.map_err(|e| {
                        let path = e
                            .path()
                            .map(Path::to_path_buf)
                            .unwrap_or_else(|| content.clone());
                        InstallError::Io {
                            path,
                            source: e.into(),
                        }
                    })?;
                    let ft = entry.file_type();
                    if ft.is_symlink() {
                        progress(Event::SkippedSymlink {
                            path: entry.path().to_path_buf(),
                        });
                        continue;
                    }
                    if !ft.is_file() {
                        continue;
                    }
                    let rel = entry.path().strip_prefix(&content).unwrap_or(entry.path());
                    let Some(rel) = rel.to_str() else { continue };
                    files.push((
                        format!(
                            "{CONTENT_DIR}/{}",
                            rel.replace(std::path::MAIN_SEPARATOR, "/")
                        ),
                        entry.path().to_path_buf(),
                    ));
                }
                files.sort();
                for (path, src) in files {
                    add(path, &s.id, src)?;
                }
            }
        }
        // The spec of an installed package is not trusted to stay inside it.
        let Some(folder) = crate::spec::normal_rel(&s.binaries) else {
            return Err(InstallError::BadBinaries {
                package: s.id.clone(),
                folder: s.binaries.clone(),
            });
        };
        let bin = folder.split('/').fold(s.dir.clone(), |p, c| p.join(c));
        if is_symlink(&bin) {
            progress(Event::SkippedSymlink { path: bin });
            continue;
        }
        let Ok(rd) = std::fs::read_dir(&bin) else {
            continue;
        };
        let mut names = Vec::new();
        for entry in rd.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if !crate::spec::is_module_name(&name) {
                continue;
            }
            match entry.file_type() {
                Ok(t) if t.is_symlink() => progress(Event::SkippedSymlink { path: entry.path() }),
                Ok(t) if t.is_file() => names.push((name, entry.path())),
                _ => {}
            }
        }
        names.sort();
        for (name, src) in names {
            if project.server.is_none() {
                report.binaries_skipped.push((s.id.clone(), name));
                continue;
            }
            if let (Some(server), Some(platform)) = (&platforms, module_platform(&name))
                && !server.contains(&platform)
            {
                progress(Event::OtherPlatform {
                    name,
                    package: s.id.clone(),
                });
                continue;
            }
            add(format!("{BIN_PREFIX}{name}"), &s.id, src)?;
        }
    }
    Ok(out)
}

/// Copies `src` to `dest` through a temporary file beside it.
fn write_file(dest: &Path, src: &Path) -> Result<(), InstallError> {
    let parent = dest.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io(parent))?;
    let mut from = std::fs::File::open(src).map_err(io(src))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(io(parent))?;
    std::io::copy(&mut from, &mut tmp).map_err(io(dest))?;
    tmp.persist(dest).map_err(|e| InstallError::Io {
        path: dest.to_path_buf(),
        source: e.error,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

/// Removes the empty directories between `file`'s parent and `base` (not
/// `base`). `file` has to lie below `base` by normal components only, and
/// only real directories are removed, never a symlink.
fn prune(file: &Path, base: &Path) {
    let Ok(rel) = file.strip_prefix(base) else {
        return;
    };
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return;
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == base || !d.starts_with(base) || is_symlink(d) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// The directory below which a recorded path is written: the project root
/// for `content/...` (a checkout may hold a symlinked `content/`), the
/// server's `lua/bin/` for a module.
fn top_of(project: &Project, path: &str) -> Option<PathBuf> {
    if path.starts_with(BIN_PREFIX) {
        base_of(project, path)
    } else {
        Some(project.root.clone())
    }
}

/// Refuses a destination that a symlink between `top` and the file would
/// take somewhere else.
fn check_real_dirs(project: &Project, path: &str, dest: &Path) -> Result<(), InstallError> {
    let Some(top) = top_of(project, path) else {
        return Ok(());
    };
    let parent = dest.parent().unwrap_or(dest);
    let symlinked = |link: PathBuf| InstallError::SymlinkedDir {
        path: path.to_string(),
        link,
    };
    let rel = parent
        .strip_prefix(&top)
        .map_err(|_| symlinked(parent.to_path_buf()))?;
    let mut dir = top;
    for c in rel.components() {
        dir.push(c);
        if is_symlink(&dir) {
            return Err(symlinked(dir));
        }
    }
    Ok(())
}

fn base_of(project: &Project, path: &str) -> Option<PathBuf> {
    if path.starts_with(BIN_PREFIX) {
        project
            .server
            .as_ref()
            .map(|s| s.join(GARRYSMOD_DIR).join("lua").join("bin"))
    } else {
        Some(project.root.join(CONTENT_DIR))
    }
}

/// The other gamemode project of the server that records `path` (a module
/// in the shared `garrysmod/lua/bin/`), and the package it records it for.
fn recorded_elsewhere(project: &Project, path: &str) -> Option<(String, String)> {
    let server = project.server.as_ref()?;
    let others = crate::project::server_projects(server).ok()?;
    others
        .iter()
        .filter(|p| p.root != project.root)
        .find_map(|p| {
            let state = State::read(p).ok()?;
            state
                .files
                .get(path)
                .map(|f| (p.id.clone(), f.package.clone()))
        })
}

/// A recorded path as the user finds it: `content/...` in the project,
/// `garrysmod/lua/bin/<name>` in the server.
pub fn shown_path(path: &str) -> String {
    match path.strip_prefix(BIN_PREFIX) {
        Some(name) => format!("{GARRYSMOD_DIR}/lua/bin/{name}"),
        None => path.to_string(),
    }
}

/// A file `install` would write over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// The recorded path (`content/...` or `bin/<name>`).
    pub path: String,
    /// The other gamemode project of the server that installed it, and the
    /// package it came from; `None` for a file fpkg did not write.
    pub installed_by: Option<(String, String)>,
}

/// `install`'s last step: writes missing or changed files, removes recorded
/// files no package wants any more (when they still hold what was written),
/// and records the result. A file that is not recorded and differs is a
/// conflict unless `force`; one with the same content is adopted.
pub fn materialize(
    project: &Project,
    sources: &[Source],
    force: bool,
    report: &mut InstallReport,
    progress: &mut dyn FnMut(Event),
) -> Result<(), InstallError> {
    let old = State::read(project)?;
    let want = wanted(project, sources, report, progress)?;

    let mut plan = Vec::new();
    let mut conflicts = Vec::new();
    for (path, w) in &want {
        let Some(dest) = destination(project, path) else {
            continue;
        };
        check_real_dirs(project, path, &dest)?;
        let current = if dest.is_file() {
            Some(sha256_file(&dest).map_err(io(&dest))?)
        } else {
            None
        };
        let recorded = old.files.contains_key(path);
        let conflict = || Conflict {
            path: path.clone(),
            installed_by: path
                .starts_with(BIN_PREFIX)
                .then(|| recorded_elsewhere(project, path))
                .flatten(),
        };
        match current {
            Some(c) if c == w.sha256 => {}
            Some(_) if !recorded && !force => conflicts.push(conflict()),
            _ if dest.exists() && !dest.is_file() && !force => conflicts.push(conflict()),
            _ => plan.push((path.clone(), dest)),
        }
    }
    if !conflicts.is_empty() {
        return Err(InstallError::FileConflict(conflicts));
    }

    let mut new = State::default();
    for (path, w) in &want {
        new.files.insert(
            path.clone(),
            FileEntry {
                path: path.clone(),
                package: w.package.clone(),
                sha256: w.sha256.clone(),
            },
        );
    }
    for (path, dest) in plan {
        let w = &want[&path];
        progress(Event::Writing {
            path: path.clone(),
            package: w.package.clone(),
        });
        if dest.is_dir() {
            std::fs::remove_dir_all(&dest).map_err(io(&dest))?;
        }
        write_file(&dest, &w.src)?;
        report.files_written.push(path);
    }
    for (path, entry) in &old.files {
        if want.contains_key(path) {
            continue;
        }
        let Some(dest) = destination(project, path) else {
            // A binary module of a server this run does not know: keep the record.
            new.files.insert(path.clone(), entry.clone());
            continue;
        };
        check_real_dirs(project, path, &dest)?;
        if !dest.is_file() {
            continue;
        }
        let same = sha256_file(&dest).is_ok_and(|h| h.eq_ignore_ascii_case(&entry.sha256));
        if !same {
            progress(Event::KeptModified { path: path.clone() });
            report.files_kept.push(path.clone());
            continue;
        }
        // lua/bin/ belongs to the whole server: a module another gamemode
        // project still records stays, and only this project's record goes.
        if path.starts_with(BIN_PREFIX) && recorded_elsewhere(project, path).is_some() {
            continue;
        }
        progress(Event::RemovingFile { path: path.clone() });
        std::fs::remove_file(&dest).map_err(io(&dest))?;
        if let Some(base) = base_of(project, path) {
            prune(&dest, &base);
        }
        report.files_removed.push(path.clone());
    }
    if new != old {
        std::fs::create_dir_all(project.packages_dir()).map_err(io(&project.packages_dir()))?;
        new.write(project)?;
    }
    Ok(())
}

/// What `verify` found about the recorded files.
#[derive(Debug, Default)]
pub struct FilesVerify {
    pub ok: usize,
    /// (package, message).
    pub problems: Vec<(String, String)>,
    /// Recorded `bin/` paths not checked because the server folder is not known.
    pub bin_unchecked: Vec<String>,
}

/// Checks every recorded file against the hash that was written.
pub fn verify(project: &Project) -> Result<FilesVerify, InstallError> {
    let state = State::read(project)?;
    let mut out = FilesVerify::default();
    for (path, entry) in &state.files {
        let Some(dest) = destination(project, path) else {
            out.bin_unchecked.push(path.clone());
            continue;
        };
        if !dest.is_file() {
            out.problems.push((
                entry.package.clone(),
                format!("provides {path}, which is missing ({})", dest.display()),
            ));
            continue;
        }
        match sha256_file(&dest) {
            Ok(h) if h.eq_ignore_ascii_case(&entry.sha256) => out.ok += 1,
            Ok(_) => out.problems.push((
                entry.package.clone(),
                format!("provides {path}, which was changed after install"),
            )),
            Err(e) => out.problems.push((
                entry.package.clone(),
                format!("provides {path}, which cannot be read: {e}"),
            )),
        }
    }
    Ok(out)
}

/// The files the packages provide that the record lacks, as (package,
/// path): what an install that stopped short, at a conflict, did not copy.
pub fn unrecorded(
    project: &Project,
    sources: &[Source],
) -> Result<Vec<(String, String)>, InstallError> {
    let state = State::read(project)?;
    let want = wanted(project, sources, &mut InstallReport::default(), &mut |_| {})?;
    Ok(want
        .into_iter()
        .filter(|(path, _)| !state.files.contains_key(path))
        .map(|(path, w)| (w.package, path))
        .collect())
}

/// The message of `InstallError::FileConflict`.
pub fn conflict_message(conflicts: &[Conflict]) -> String {
    let (shared, foreign): (Vec<&Conflict>, Vec<&Conflict>) =
        conflicts.iter().partition(|c| c.installed_by.is_some());
    let mut parts = Vec::new();
    if !foreign.is_empty() {
        let list = foreign
            .iter()
            .map(|c| format!("  - {}", shown_path(&c.path)))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!(
            "these files exist but fpkg did not write them, and the packages provide different content:\n{list}\nMove them away, or run `fpkg install --force` to overwrite them"
        ));
    }
    if !shared.is_empty() {
        let list = shared
            .iter()
            .filter_map(|c| {
                let (gamemode, package) = c.installed_by.as_ref()?;
                Some(format!(
                    "  - {} (installed by the gamemode {gamemode}, from {package})",
                    shown_path(&c.path)
                ))
            })
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(format!(
            "another gamemode of this server installed these modules with different content:\n{list}\nThe gamemodes of a server share garrysmod/lua/bin/: give them the same version of the package, or run `fpkg install --force` to overwrite the module (fpkg verify of the other gamemode then fails)"
        ));
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trip() {
        let mut s = State::default();
        for (p, k) in [("content/b.txt", "x"), ("bin/gmsv_a_linux64.dll", "y")] {
            s.files.insert(
                p.into(),
                FileEntry {
                    path: p.into(),
                    package: k.into(),
                    sha256: "ab".repeat(32),
                },
            );
        }
        let text = s.to_toml();
        assert!(
            text.find("bin/gmsv_a").unwrap() < text.find("content/b").unwrap(),
            "{text}"
        );
        let raw: RawState = toml::from_str(&text).unwrap();
        assert_eq!(raw.version, STATE_VERSION);
        assert_eq!(raw.file.len(), 2);
    }

    #[test]
    fn a_binaries_folder_outside_the_package_is_refused() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let gm = base.join("srv/garrysmod/gamemodes/gm");
        let pkg = gm.join("packages/evil");
        std::fs::create_dir_all(&pkg).unwrap();
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("host.dll"), "not the package's").unwrap();
        std::fs::write(pkg.join("root.dll"), "not in a binaries folder").unwrap();
        let project = Project::new(gm);
        assert_eq!(project.server.as_deref(), Some(base.join("srv").as_path()));
        for folder in [
            "../../../../../../outside",
            outside.to_str().unwrap(),
            "",
            ".",
        ] {
            let source = Source {
                id: "evil".into(),
                dir: pkg.clone(),
                binaries: folder.into(),
            };
            let e = materialize(
                &project,
                &[source],
                true,
                &mut InstallReport::default(),
                &mut |_| {},
            )
            .unwrap_err();
            assert!(
                matches!(e, InstallError::BadBinaries { .. }),
                "{folder:?}: {e}"
            );
        }
        assert!(!base.join("srv/garrysmod/lua").exists());
    }

    /// A gamemode project at `<base>/srv/garrysmod/gamemodes/gm`, and a file
    /// at `<base>/victim.txt` whose content its state file could name.
    fn project_and_victim(base: &Path) -> (Project, PathBuf, String) {
        let gm = base.join("srv/garrysmod/gamemodes/gm");
        std::fs::create_dir_all(gm.join("packages")).unwrap();
        std::fs::write(gm.join("gm.txt"), "\"gm\"\n{\n}\n").unwrap();
        let victim = base.join("victim.txt");
        std::fs::write(&victim, "precious").unwrap();
        let sha = sha256_file(&victim).unwrap();
        (Project::new(gm), victim, sha)
    }

    fn write_state(project: &Project, path: &str, sha: &str) {
        let text = format!(
            "version = 1\n\n[[file]]\npath = {}\npackage = \"evil\"\nsha256 = \"{sha}\"\n",
            toml::Value::String(path.to_string())
        );
        std::fs::write(State::path(project), text).unwrap();
    }

    #[test]
    fn a_state_file_cannot_name_paths_outside() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let (project, victim, sha) = project_and_victim(&base);
        std::fs::create_dir_all(project.root.join("content")).unwrap();
        let abs = victim.display().to_string();
        for path in [
            "content/../../../../../victim.txt".to_string(),
            "content/a/../../../../../../victim.txt".into(),
            "bin/../../../../victim.txt".into(),
            format!("content/{abs}"),
            abs.clone(),
            format!("bin/{abs}"),
            "bin/sub/x.dll".into(),
            "content/".into(),
            "bin/".into(),
            "content\\..\\..\\x".into(),
            "content/C:/x".into(),
            "other/x".into(),
        ] {
            write_state(&project, &path, &sha);
            assert!(State::read(&project).is_err(), "{path} was accepted");
            assert!(
                materialize(
                    &project,
                    &[],
                    true,
                    &mut InstallReport::default(),
                    &mut |_| {}
                )
                .is_err(),
                "{path}"
            );
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "precious",
                "{path}"
            );
        }
        write_state(&project, "content/materials/x.vmt", &sha);
        assert_eq!(State::read(&project).unwrap().files.len(), 1);
        write_state(&project, "bin/gmsv_x_linux64.dll", &sha);
        assert_eq!(State::read(&project).unwrap().files.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_removed_or_written_through_a_symlinked_directory() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let (project, victim, sha) = project_and_victim(&base);
        let content = project.root.join("content");
        // A committed content/sub that points outside, then content/ itself.
        std::fs::create_dir_all(&content).unwrap();
        std::os::unix::fs::symlink(&base, content.join("sub")).unwrap();
        for (link, path) in [
            (content.join("sub"), "content/sub/victim.txt"),
            (content.clone(), "content/victim.txt"),
        ] {
            if link == content {
                std::fs::remove_dir_all(&content).unwrap();
                std::os::unix::fs::symlink(&base, &content).unwrap();
            }
            write_state(&project, path, &sha);
            let e = materialize(
                &project,
                &[],
                true,
                &mut InstallReport::default(),
                &mut |_| {},
            )
            .unwrap_err();
            assert!(
                matches!(&e, InstallError::SymlinkedDir { link: l, .. } if *l == link),
                "{path}: {e}"
            );
            assert_eq!(
                std::fs::read_to_string(&victim).unwrap(),
                "precious",
                "{path}"
            );
        }
        // A package's file is not written there either.
        std::fs::remove_file(State::path(&project)).unwrap();
        let pkg = project.root.join("packages/evil");
        std::fs::create_dir_all(pkg.join("content")).unwrap();
        std::fs::write(pkg.join("content/planted.txt"), "planted").unwrap();
        let source = Source {
            id: "evil".into(),
            dir: pkg,
            binaries: "bin".into(),
        };
        let e = materialize(
            &project,
            &[source],
            true,
            &mut InstallReport::default(),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(e, InstallError::SymlinkedDir { .. }), "{e}");
        assert!(!base.join("planted.txt").exists());
    }

    /// A gamemode project `<server>/garrysmod/gamemodes/<id>` and a package
    /// in it whose `bin/` holds `modules` (name, content).
    fn gamemode_with_modules(
        server: &Path,
        id: &str,
        modules: &[(&str, &str)],
    ) -> (Project, Source) {
        let gm = server.join("garrysmod/gamemodes").join(id);
        std::fs::create_dir_all(gm.join("packages/gmsv_x/bin")).unwrap();
        std::fs::write(gm.join(format!("{id}.txt")), format!("\"{id}\"\n{{\n}}\n")).unwrap();
        std::fs::write(
            gm.join("packagespec.lua"),
            "Package:describe(function(s) end)\n",
        )
        .unwrap();
        for (name, text) in modules {
            std::fs::write(gm.join("packages/gmsv_x/bin").join(name), text).unwrap();
        }
        let source = Source {
            id: "gmsv_x".into(),
            dir: gm.join("packages/gmsv_x"),
            binaries: "bin".into(),
        };
        (Project::new(gm), source)
    }

    #[test]
    fn only_the_modules_of_the_server_binaries_are_copied() {
        let t = tempfile::tempdir().unwrap();
        let server = t.path().canonicalize().unwrap().join("srv");
        let modules = [
            ("gmsv_x_linux64.dll", "linux64"),
            ("gmsv_x_win64.dll", "win64"),
            ("gmsv_x.dll", "no platform"),
        ];
        let (project, source) = gamemode_with_modules(&server, "gm", &modules);
        let bin = server.join("garrysmod/lua/bin");
        assert_eq!(server_platforms(&server), None);
        // The x86-64 branch on Linux: both launchers, and the engines.
        for f in [
            "srcds_run",
            "srcds_run_x64",
            "bin/linux32/srcds",
            "bin/linux64/srcds",
        ] {
            std::fs::create_dir_all(server.join(f).parent().unwrap()).unwrap();
            std::fs::write(server.join(f), "").unwrap();
        }
        assert_eq!(server_platforms(&server), Some(vec!["linux", "linux64"]));
        // A Windows module someone copied in by hand is no conflict.
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("gmsv_x_win64.dll"), "another build").unwrap();
        let mut report = InstallReport::default();
        let mut skipped = Vec::new();
        materialize(&project, &[source], false, &mut report, &mut |e| {
            if let Event::OtherPlatform { name, .. } = e {
                skipped.push(name);
            }
        })
        .unwrap();
        assert_eq!(
            report.files_written,
            vec!["bin/gmsv_x.dll", "bin/gmsv_x_linux64.dll"]
        );
        assert_eq!(skipped, vec!["gmsv_x_win64.dll"]);
        assert_eq!(
            std::fs::read_to_string(bin.join("gmsv_x_win64.dll")).unwrap(),
            "another build"
        );
        assert_eq!(module_platform("gmsv_pg_LINUX64.dll"), Some("linux64"));
        assert_eq!(module_platform("gmsv_pg_linux64_extra.dll"), None);
    }

    #[test]
    fn a_module_of_another_gamemode_is_named_in_the_conflict() {
        let t = tempfile::tempdir().unwrap();
        let server = t.path().canonicalize().unwrap().join("srv");
        let (alpha, a) = gamemode_with_modules(&server, "alpha", &[("gmsv_x_linux64.dll", "1.0")]);
        let (beta, b) = gamemode_with_modules(&server, "beta", &[("gmsv_x_linux64.dll", "2.0")]);
        materialize(
            &alpha,
            &[a],
            false,
            &mut InstallReport::default(),
            &mut |_| {},
        )
        .unwrap();
        let e = materialize(
            &beta,
            &[b],
            false,
            &mut InstallReport::default(),
            &mut |_| {},
        )
        .unwrap_err();
        let InstallError::FileConflict(conflicts) = &e else {
            panic!("{e}");
        };
        assert_eq!(
            conflicts,
            &vec![Conflict {
                path: "bin/gmsv_x_linux64.dll".into(),
                installed_by: Some(("alpha".into(), "gmsv_x".into())),
            }]
        );
        let text = e.to_string();
        assert!(
            text.contains("garrysmod/lua/bin/gmsv_x_linux64.dll (installed by the gamemode alpha, from gmsv_x)")
                && !text.contains("fpkg did not write them"),
            "{text}"
        );
    }

    #[test]
    fn prune_stays_below_its_base() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        let content = base.join("gm/content");
        std::fs::create_dir_all(content.join("a/b")).unwrap();
        std::fs::create_dir_all(base.join("outside")).unwrap();
        prune(&content.join("a/b/../../../../outside/f"), &content);
        assert!(base.join("outside").is_dir());
        prune(&content.join("a/b/f"), &content);
        assert!(!content.join("a").exists() && content.is_dir());
    }

    #[test]
    fn destinations() {
        let t = tempfile::tempdir().unwrap();
        let mut p = Project::new(t.path().to_path_buf());
        assert_eq!(
            destination(&p, "content/a/b.ttf"),
            Some(t.path().join("content/a/b.ttf"))
        );
        assert_eq!(destination(&p, "bin/x.dll"), None);
        p.server = Some(PathBuf::from("/srv"));
        assert_eq!(
            destination(&p, "bin/x.dll"),
            Some(PathBuf::from("/srv/garrysmod/lua/bin/x.dll"))
        );
        assert_eq!(destination(&p, "other/x"), None);
    }
}

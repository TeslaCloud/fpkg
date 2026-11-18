//! Materialized files: the `content/` of every package merged into a gamemode
//! project's `content/`, and binary modules copied to the server's
//! `garrysmod/lua/bin/`, recorded in `packages/.fpkg-files.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

/// Removes the empty directories between `file`'s parent and `base` (not `base`).
fn prune(file: &Path, base: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d == base || !d.starts_with(base) || std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
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

/// Whether another gamemode project of the server records `path` (a module in
/// the shared `garrysmod/lua/bin/`).
fn recorded_elsewhere(project: &Project, path: &str) -> bool {
    let Some(server) = &project.server else {
        return false;
    };
    let Ok(others) = crate::project::server_projects(server) else {
        return false;
    };
    others
        .iter()
        .filter(|p| p.root != project.root)
        .any(|p| State::read(p).is_ok_and(|s| s.files.contains_key(path)))
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
        let current = if dest.is_file() {
            Some(sha256_file(&dest).map_err(io(&dest))?)
        } else {
            None
        };
        let recorded = old.files.contains_key(path);
        match current {
            Some(c) if c == w.sha256 => {}
            Some(_) if !recorded && !force => conflicts.push(path.clone()),
            _ if dest.exists() && !dest.is_file() && !force => conflicts.push(path.clone()),
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
        if path.starts_with(BIN_PREFIX) && recorded_elsewhere(project, path) {
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

/// The message of `InstallError::FileConflict`.
pub fn conflict_message(paths: &[String]) -> String {
    let list = paths
        .iter()
        .map(|p| format!("  - {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "these files exist but fpkg did not write them, and the packages provide different content:\n{list}\nMove them away, or run `fpkg install --force` to overwrite them"
    )
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

//! The virtual tree: the game's file system mapped onto disk.
//!
//! Paths the game names are relative to a search path (`GAME`, `LUA`, `DATA`, ...). Every one
//! of them is a list of folders relative to `garrysmod/`, and a folder relative to
//! `garrysmod/` is a real directory: `gamemodes/<gamemode>/` is the project, everything else
//! is under the tree's `garrysmod` directory. For a project inside a server that is the
//! server's own `garrysmod/`; for a stand-alone checkout it is `<project>/.fpkg/garrysmod/`,
//! created (with `data/`, `lua/` and a `.gitignore` of `*`) the first time something is
//! written there, never by reading. Other folders can be mounted at a game path, such as a
//! path package next to a stand-alone checkout. A stand-alone checkout's own `.fpkg/` is not
//! part of the game's tree. A path component that is not a plain name (`C:`, anything with
//! a `:`) leaves the tree, as `..` beyond the start does.
//!
//! Names are found as the game finds them, ignoring ASCII case in every search path: the
//! path as it is written first, then each component in its folder's listing, preferring the
//! entry of that exact name, then the lower-case one. Listings are kept for the life of the
//! tree and dropped when the runtime writes (`changed`). Paths are bytes, as Lua has them;
//! case is folded for ASCII letters only. `game_path` is exact, for writes and for `File.*`.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::UNIX_EPOCH;

/// The folder of a stand-alone checkout that holds what the game keeps outside the gamemode.
pub const STANDALONE_DIR: &str = ".fpkg";

/// How a project is placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Layout {
    /// `<server>/garrysmod/gamemodes/<gamemode>`, or a checkout pointed at a server tree.
    Server,
    /// A checkout on its own; the rest of `garrysmod/` is `<project>/.fpkg/garrysmod/`.
    Standalone,
}

/// A component of a game path, as bytes.
type Part = Vec<u8>;

/// Names of files or folders, as bytes.
pub type Names = Vec<Vec<u8>>;

/// The names in a folder, sorted byte by byte, by folder.
type Listings = Rc<RefCell<HashMap<PathBuf, Rc<Vec<OsString>>>>>;

/// The game's paths for one project.
#[derive(Clone, Debug)]
pub struct Tree {
    garrysmod: PathBuf,
    gamemode: String,
    layout: Layout,
    /// Folders at game paths, the gamemode first: (components relative to `garrysmod/`,
    /// directory).
    mounts: Vec<(Vec<Part>, PathBuf)>,
    listings: Listings,
}

/// The file sorts of `file.Find`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    NameAsc,
    NameDesc,
    DateAsc,
    DateDesc,
}

impl Sort {
    /// The game's names; anything else sorts by name, ascending.
    pub fn parse(s: Option<&str>) -> Sort {
        match s.map(str::to_ascii_lowercase).as_deref() {
            Some("namedesc") => Sort::NameDesc,
            Some("dateasc") => Sort::DateAsc,
            Some("datedesc") => Sort::DateDesc,
            _ => Sort::NameAsc,
        }
    }
}

/// A folder a search path covers: relative to `garrysmod/` (empty for `garrysmod/` itself),
/// or a real directory outside it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Root {
    Game(Vec<Part>),
    Real(PathBuf),
}

/// A file or folder found through a search path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// Where it is on disk.
    pub real: PathBuf,
    /// Its path relative to `garrysmod/`, as the game names files (`gamemodes/x/init.lua`,
    /// `lua/includes/init.lua`); `None` outside of `garrysmod/`.
    pub game: Option<String>,
}

/// A place in a search path before it is looked up: the directory it starts from, the
/// components below it and its game path.
struct Place<'a> {
    base: PathBuf,
    rest: Cow<'a, [Part]>,
    game: Option<Vec<Part>>,
}

#[cfg(unix)]
fn os(part: &[u8]) -> Cow<'_, OsStr> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(OsStr::from_bytes(part))
}

#[cfg(not(unix))]
fn os(part: &[u8]) -> Cow<'_, OsStr> {
    Cow::Owned(OsString::from(String::from_utf8_lossy(part).into_owned()))
}

#[cfg(unix)]
fn bytes_of(name: &OsStr) -> Cow<'_, [u8]> {
    use std::os::unix::ffi::OsStrExt;
    Cow::Borrowed(name.as_bytes())
}

#[cfg(not(unix))]
fn bytes_of(name: &OsStr) -> Cow<'_, [u8]> {
    Cow::Owned(name.to_string_lossy().into_owned().into_bytes())
}

fn join(base: &Path, parts: &[Part]) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in parts {
        p.push(os(part));
    }
    p
}

fn text(parts: &[Part]) -> String {
    String::from_utf8_lossy(&parts.join(&b'/')).into_owned()
}

/// Whether `parts` begins with `prefix`, ignoring ASCII case or not.
fn begins_with(parts: &[Part], prefix: &[Part], fold: bool) -> bool {
    parts.len() >= prefix.len()
        && parts.iter().zip(prefix).all(|(a, b)| {
            if fold {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        })
}

impl Tree {
    /// A project at `gamemode_dir` inside (or pointed at) the server whose root holds
    /// `garrysmod/`.
    pub fn server(server_root: &Path, gamemode: &str, gamemode_dir: &Path) -> Tree {
        Tree::new(
            server_root.join("garrysmod"),
            gamemode,
            gamemode_dir,
            Layout::Server,
        )
    }

    /// A stand-alone checkout at `project_root`.
    pub fn standalone(project_root: &Path, gamemode: &str) -> Tree {
        Tree::new(
            project_root.join(STANDALONE_DIR).join("garrysmod"),
            gamemode,
            project_root,
            Layout::Standalone,
        )
    }

    fn new(garrysmod: PathBuf, gamemode: &str, gamemode_dir: &Path, layout: Layout) -> Tree {
        Tree {
            garrysmod,
            gamemode: gamemode.to_string(),
            layout,
            mounts: vec![(
                vec![b"gamemodes".to_vec(), gamemode.as_bytes().to_vec()],
                gamemode_dir.to_path_buf(),
            )],
            listings: Listings::default(),
        }
    }

    /// Makes `dir` the folder at `game_path` (relative to `garrysmod/`, such as
    /// `gamemodes/shared`); false when the path leaves `garrysmod/` or is the gamemode's.
    pub fn mount(&mut self, game_path: &str, dir: &Path) -> bool {
        let Some(parts) = normalize_bytes(game_path.as_bytes()) else {
            return false;
        };
        if parts.is_empty() || self.mounts.iter().any(|(m, _)| *m == parts) {
            return false;
        }
        self.mounts.push((parts, dir.to_path_buf()));
        true
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The directory that stands for `garrysmod/`.
    pub fn garrysmod(&self) -> &Path {
        &self.garrysmod
    }

    /// The gamemode folder name, `engine.ActiveGamemode()`.
    pub fn gamemode(&self) -> &str {
        &self.gamemode
    }

    pub fn gamemode_dir(&self) -> &Path {
        &self.mounts[0].1
    }

    /// The game's SQLite database, `garrysmod/sv.db`.
    pub fn database(&self) -> PathBuf {
        self.garrysmod.join("sv.db")
    }

    /// Forgets the listings of folders: the runtime wrote something.
    pub fn changed(&self) {
        self.listings.borrow_mut().clear();
    }

    /// Creates the stand-alone `garrysmod/`, with its `data/` and `lua/` folders and a
    /// `.gitignore` that leaves all of it out of git, unless it is there; a server tree has
    /// its own.
    pub fn prepare(&self) -> std::io::Result<()> {
        if self.layout != Layout::Standalone {
            return Ok(());
        }
        let ignore = self.garrysmod.join(".gitignore");
        if ignore.exists() && self.garrysmod.join("data").is_dir() {
            return Ok(());
        }
        for dir in ["data", "lua"] {
            std::fs::create_dir_all(self.garrysmod.join(dir))?;
        }
        if !ignore.exists() {
            std::fs::write(ignore, "*\n")?;
        }
        self.changed();
        Ok(())
    }

    /// `prepare`, when `real` is a path in the stand-alone `garrysmod/` about to be written.
    pub fn prepare_for(&self, real: &Path) -> std::io::Result<()> {
        if self.layout == Layout::Standalone && real.starts_with(&self.garrysmod) {
            self.prepare()?;
        }
        Ok(())
    }

    /// The real path of a path relative to `garrysmod/`, exactly as written, or `None` when
    /// it leaves it or is not part of the game's tree.
    pub fn game_path(&self, rel: impl AsRef<[u8]>) -> Option<PathBuf> {
        let parts = normalize_bytes(rel.as_ref())?;
        let (base, rest) = self.mounted(&parts, false)?;
        Some(join(&base, rest))
    }

    /// The directory a game path starts from (a mount, else `garrysmod/`) and the components
    /// below it; `None` for a path inside the stand-alone `.fpkg/`.
    fn mounted<'p>(&self, parts: &'p [Part], fold: bool) -> Option<(PathBuf, &'p [Part])> {
        let mounted = self
            .mounts
            .iter()
            .filter(|(m, _)| begins_with(parts, m, fold))
            .max_by_key(|(m, _)| m.len());
        let (base, rest) = match mounted {
            Some((m, dir)) => (dir.clone(), &parts[m.len()..]),
            None => (self.garrysmod.clone(), parts),
        };
        if self.layout == Layout::Standalone
            && mounted.is_some_and(|(m, _)| *m == self.mounts[0].0)
            && rest
                .first()
                .is_some_and(|p| p.eq_ignore_ascii_case(STANDALONE_DIR.as_bytes()))
        {
            return None;
        }
        Some((base, rest))
    }

    /// The names in a folder (none when it is not one), from the listings kept.
    fn listing(&self, dir: &Path) -> Rc<Vec<OsString>> {
        if let Some(l) = self.listings.borrow().get(dir) {
            return l.clone();
        }
        let mut names: Vec<OsString> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        names.sort_by(|a, b| bytes_of(a).cmp(&bytes_of(b)));
        let names = Rc::new(names);
        self.listings
            .borrow_mut()
            .insert(dir.to_path_buf(), names.clone());
        names
    }

    /// The real path of `rest` below `base`, ignoring ASCII case: the path as written when it
    /// is there, else each component as its folder has it (the entry of that exact name, then
    /// the lower-case one, then the first other one).
    fn lookup(&self, base: &Path, rest: &[Part]) -> Option<PathBuf> {
        let exact = join(base, rest);
        if exact.exists() {
            return Some(exact);
        }
        let mut cur = base.to_path_buf();
        for part in rest {
            let names = self.listing(&cur);
            let lower = part.to_ascii_lowercase();
            let pick = names
                .iter()
                .find(|n| *bytes_of(n) == **part)
                .or_else(|| names.iter().find(|n| *bytes_of(n) == *lower))
                .or_else(|| {
                    names
                        .iter()
                        .find(|n| bytes_of(n).eq_ignore_ascii_case(part))
                })?;
            cur.push(pick);
        }
        cur.exists().then_some(cur)
    }

    /// The addons of a server tree, by folder name.
    fn addons(&self) -> Vec<Part> {
        if self.layout != Layout::Server {
            return Vec::new();
        }
        let dir = self.garrysmod.join("addons");
        self.listing(&dir)
            .iter()
            .map(|n| bytes_of(n).into_owned())
            .filter(|n| !n.starts_with(b".") && join(&dir, std::slice::from_ref(n)).is_dir())
            .collect()
    }

    /// The folders of a search path, in the order the game searches them.
    fn roots(&self, path_id: &str) -> Vec<Root> {
        let game = |s: &str| Root::Game(normalize_bytes(s.as_bytes()).unwrap_or_default());
        let addons = |sub: &str| -> Vec<Root> {
            self.addons()
                .into_iter()
                .map(|a| {
                    let mut parts = vec![b"addons".to_vec(), a];
                    parts.extend(normalize_bytes(sub.as_bytes()).unwrap_or_default());
                    Root::Game(parts)
                })
                .collect()
        };
        let content = Root::Game(vec![
            b"gamemodes".to_vec(),
            self.gamemode.as_bytes().to_vec(),
            b"content".to_vec(),
        ]);
        match path_id.to_ascii_uppercase().as_str() {
            "GAME" => {
                let mut v = vec![game(""), content];
                v.extend(addons(""));
                v
            }
            "MOD" | "GARRYSMOD" | "MOD_WRITE" | "GAME_WRITE" | "DEFAULT_WRITE_PATH" => {
                vec![game("")]
            }
            "LUA" | "LSV" => {
                let mut v = vec![game("lua"), game("gamemodes")];
                v.extend(addons("gamemodes"));
                v.extend(addons("lua"));
                v
            }
            "DATA" => vec![game("data")],
            "DOWNLOAD" => vec![game("download")],
            "THIRDPARTY" => {
                let mut v = vec![content];
                v.extend(addons(""));
                v
            }
            "BASE_PATH" => match self.garrysmod.parent() {
                Some(p) => vec![Root::Real(p.to_path_buf())],
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    /// Where `parts` are under a root of a search path, before they are looked up; `None`
    /// when that is not part of the game's tree.
    fn place<'a>(&self, root: &Root, parts: &'a [Part]) -> Option<Place<'a>> {
        match root {
            Root::Game(prefix) => {
                let mut all = prefix.clone();
                all.extend(parts.iter().cloned());
                let (base, rest) = self.mounted(&all, true)?;
                let rest = rest.to_vec();
                Some(Place {
                    base,
                    rest: Cow::Owned(rest),
                    game: Some(all),
                })
            }
            Root::Real(dir) => Some(Place {
                base: dir.clone(),
                rest: Cow::Borrowed(parts),
                game: None,
            }),
        }
    }

    /// The places of a path in a search path that are on disk, in the order the game
    /// searches them.
    fn found(&self, path: &[u8], path_id: &str) -> Option<Vec<Found>> {
        let parts = normalize_bytes(path)?;
        Some(
            self.roots(path_id)
                .iter()
                .filter_map(|r| self.place(r, &parts))
                .filter_map(|p| {
                    Some(Found {
                        real: self.lookup(&p.base, &p.rest)?,
                        game: p.game.as_deref().map(text),
                    })
                })
                .collect(),
        )
    }

    /// The first file (not folder) of that name in the search path.
    pub fn find_file(&self, path: impl AsRef<[u8]>, path_id: &str) -> Option<Found> {
        let path = path.as_ref();
        if normalize_bytes(path)?.is_empty() {
            return None;
        }
        self.found(path, path_id)?
            .into_iter()
            .find(|f| f.real.is_file())
    }

    /// The first file or folder of that name in the search path. A folder above a mount
    /// (`gamemodes/` above the gamemode) is there even when it is not on disk.
    pub fn find_any(&self, path: impl AsRef<[u8]>, path_id: &str) -> Option<Found> {
        let path = path.as_ref();
        if let Some(f) = self.found(path, path_id)?.into_iter().next() {
            return Some(f);
        }
        let parts = normalize_bytes(path)?;
        self.roots(path_id).iter().find_map(|r| {
            let Root::Game(prefix) = r else {
                return None;
            };
            let mut all = prefix.clone();
            all.extend(parts.iter().cloned());
            self.above_mount(&all).then(|| Found {
                real: join(&self.garrysmod, &all),
                game: Some(text(&all)),
            })
        })
    }

    /// Whether a game path is a folder that holds a mount without being one.
    fn above_mount(&self, all: &[Part]) -> bool {
        self.mounts
            .iter()
            .any(|(m, dir)| m.len() > all.len() && begins_with(m, all, true) && dir.is_dir())
    }

    pub fn exists(&self, path: impl AsRef<[u8]>, path_id: &str) -> bool {
        self.find_any(path, path_id).is_some()
    }

    pub fn is_dir(&self, path: impl AsRef<[u8]>, path_id: &str) -> bool {
        let path = path.as_ref();
        let Some(found) = self.found(path, path_id) else {
            return false;
        };
        found.iter().any(|f| f.real.is_dir())
            || self
                .find_any(path, path_id)
                .is_some_and(|f| !f.real.exists())
    }

    /// `file.Find`: the names (not paths, as they are on disk) of the files and the folders
    /// that match the last component of `pattern` (`*` matches any run of bytes and `?` one,
    /// ignoring ASCII case), in the folder before it, found as any name is found, over every
    /// folder of the search path, each name once. `None` when the pattern leaves
    /// `garrysmod/`.
    pub fn find(
        &self,
        pattern: impl AsRef<[u8]>,
        path_id: &str,
        sort: Sort,
    ) -> Option<(Names, Names)> {
        let pattern: Vec<u8> = pattern
            .as_ref()
            .iter()
            .map(|b| if *b == b'\\' { b'/' } else { *b })
            .collect();
        let (dir, name) = match pattern.iter().rposition(|b| *b == b'/') {
            Some(i) => (&pattern[..i], &pattern[i + 1..]),
            None => (&b""[..], &pattern[..]),
        };
        let parts = normalize_bytes(dir)?;
        let mut files: Vec<(Vec<u8>, u64)> = Vec::new();
        let mut dirs: Vec<(Vec<u8>, u64)> = Vec::new();
        for root in self.roots(path_id) {
            let Some(place) = self.place(&root, &parts) else {
                continue;
            };
            if let Some(listed) = &place.game {
                for (mount, dir) in &self.mounts {
                    if let Some((last, parent)) = mount.split_last()
                        && parent.len() == listed.len()
                        && begins_with(parent, listed, true)
                        && wildcard(name, last)
                        && let Ok(meta) = std::fs::metadata(dir)
                        && meta.is_dir()
                        && !dirs.iter().any(|(x, _)| x == last)
                    {
                        dirs.push((last.clone(), modified(&meta)));
                    }
                }
            }
            let Some(base) = self.lookup(&place.base, &place.rest) else {
                continue;
            };
            let Ok(rd) = std::fs::read_dir(&base) else {
                continue;
            };
            for entry in rd.flatten() {
                let n = bytes_of(&entry.file_name()).into_owned();
                if n == b"." || n == b".." || !wildcard(name, &n) {
                    continue;
                }
                if let Some(listed) = &place.game {
                    let mut child = listed.clone();
                    child.push(n.clone());
                    if self.mounted(&child, true).is_none() {
                        continue;
                    }
                }
                let Ok(meta) = std::fs::metadata(entry.path()) else {
                    continue;
                };
                let time = modified(&meta);
                let list = if meta.is_dir() { &mut dirs } else { &mut files };
                if !list.iter().any(|(x, _)| *x == n) {
                    list.push((n, time));
                }
            }
        }
        Some((sorted(files, sort), sorted(dirs, sort)))
    }

    /// `file.Size`: bytes, or -1.
    pub fn size(&self, path: impl AsRef<[u8]>, path_id: &str) -> i64 {
        self.find_file(path, path_id)
            .and_then(|f| std::fs::metadata(f.real).ok())
            .map_or(-1, |m| m.len() as i64)
    }

    /// `file.Time`: the Unix time of the last change, or 0.
    pub fn time(&self, path: impl AsRef<[u8]>, path_id: &str) -> u64 {
        self.find_any(path, path_id)
            .and_then(|f| std::fs::metadata(f.real).ok())
            .map_or(0, |m| modified(&m))
    }
}

fn modified(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn sorted(mut v: Vec<(Vec<u8>, u64)>, sort: Sort) -> Vec<Vec<u8>> {
    v.sort_by(|a, b| a.0.cmp(&b.0));
    match sort {
        Sort::NameAsc => {}
        Sort::NameDesc => v.reverse(),
        Sort::DateAsc => v.sort_by_key(|x| x.1),
        Sort::DateDesc => v.sort_by_key(|a| std::cmp::Reverse(a.1)),
    }
    v.into_iter().map(|(n, _)| n).collect()
}

/// The components of a game path, as bytes: backslashes are slashes, empty and `.`
/// components are dropped and `x/..` is removed. `None` when `..` would leave the start, or a
/// component is not a plain name on this system (a drive such as `C:`, anything with a `:`).
pub fn normalize_bytes(path: &[u8]) -> Option<Vec<Part>> {
    let mut out: Vec<Part> = Vec::new();
    for part in path.split(|b| *b == b'/' || *b == b'\\') {
        match part {
            b"" | b"." => {}
            b".." => {
                out.pop()?;
            }
            p => {
                let name = os(p);
                let mut c = Path::new(&*name).components();
                if p.contains(&b':')
                    || !matches!((c.next(), c.next()), (Some(Component::Normal(_)), None))
                {
                    return None;
                }
                out.push(p.to_vec());
            }
        }
    }
    Some(out)
}

/// `normalize_bytes` of a path that is text.
pub fn normalize(path: &str) -> Option<Vec<String>> {
    normalize_bytes(path.as_bytes()).map(|parts| {
        parts
            .into_iter()
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect()
    })
}

/// `*` matches any run of bytes and `?` one, ignoring ASCII case.
pub fn wildcard(pattern: &[u8], name: &[u8]) -> bool {
    let (p, n) = (pattern, name);
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi].eq_ignore_ascii_case(&n[ni])) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Tree::find` with the names as text.
    fn find(
        tree: &Tree,
        pattern: &str,
        path_id: &str,
        sort: Sort,
    ) -> Option<(Vec<String>, Vec<String>)> {
        let text = |v: Vec<Vec<u8>>| -> Vec<String> {
            v.into_iter()
                .map(|n| String::from_utf8(n).unwrap())
                .collect()
        };
        tree.find(pattern, path_id, sort)
            .map(|(f, d)| (text(f), text(d)))
    }

    fn touch(p: &Path, contents: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, contents).unwrap();
    }

    #[test]
    fn normalizing_paths() {
        assert_eq!(normalize("a\\b//./c/").unwrap(), ["a", "b", "c"]);
        assert_eq!(normalize("a/../b").unwrap(), ["b"]);
        assert!(normalize("../x").is_none());
        assert!(normalize("a/../../x").is_none());
        assert!(normalize("").unwrap().is_empty());
        for outside in ["C:/Windows/win.ini", "C:", "c:\\x", "a/b:c/d", "x/C:/y"] {
            assert!(normalize(outside).is_none(), "{outside}");
        }
    }

    #[test]
    fn a_drive_never_replaces_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::standalone(dir.path(), "gm");
        assert!(tree.game_path("C:/Windows/win.ini").is_none());
        assert!(tree.find_file("C:/Windows/win.ini", "GAME").is_none());
        assert!(find(&tree, "C:/*", "GAME", Sort::NameAsc).is_none());
        assert!(tree.find_file("C:/x.lua", "BASE_PATH").is_none());
    }

    #[test]
    fn mounts_and_the_hidden_stand_alone_folder() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("gm");
        let shared = dir.path().join("pathpkg");
        touch(&shared.join("lib/x.lua"), "x");
        touch(&project.join(".fpkg/garrysmod/data/a.txt"), "a");
        touch(&project.join("init.lua"), "i");
        let mut tree = Tree::standalone(&project, "gm");
        assert!(tree.mount("gamemodes/pathpkg", &shared));
        assert!(!tree.mount("gamemodes/gm", &shared));
        assert!(!tree.mount("../x", &shared));
        let found = tree.find_file("gm/../pathpkg/lib/x.lua", "LUA").unwrap();
        assert_eq!(found.real, shared.join("lib/x.lua"));
        assert_eq!(found.game.as_deref(), Some("gamemodes/pathpkg/lib/x.lua"));
        let (_, dirs) = find(&tree, "*", "LUA", Sort::NameAsc).unwrap();
        assert_eq!(dirs, ["gm", "pathpkg"]);
        assert!(
            tree.game_path("gamemodes/gm/.fpkg/garrysmod/data/a.txt")
                .is_none()
        );
        assert!(!tree.exists("gamemodes/gm/.fpkg", "GAME"));
        assert!(tree.exists("data/a.txt", "GAME"));
        let (files, dirs) = find(&tree, "gm/*", "LUA", Sort::NameAsc).unwrap();
        assert_eq!((files, dirs), (vec!["init.lua".to_string()], vec![]));
        assert!(tree.is_dir("gamemodes", "GAME"));
        assert!(!dir.path().join("gm/.fpkg/garrysmod/lua").exists());
    }

    #[test]
    fn lua_searches_addon_gamemodes_after_gamemodes() {
        let dir = tempfile::tempdir().unwrap();
        let gm = dir.path().join("garrysmod/gamemodes/gm");
        touch(&gm.join("gm.txt"), "x");
        touch(
            &dir.path().join("garrysmod/addons/a/gamemodes/other/x.lua"),
            "addon",
        );
        touch(
            &dir.path().join("garrysmod/addons/a/lua/other/x.lua"),
            "addon lua",
        );
        let tree = Tree::server(dir.path(), "gm", &gm);
        let found = tree.find_file("other/x.lua", "LUA").unwrap();
        assert_eq!(
            found.game.as_deref(),
            Some("addons/a/gamemodes/other/x.lua")
        );
        touch(&dir.path().join("garrysmod/gamemodes/other/x.lua"), "own");
        let found = tree.find_file("other/x.lua", "LUA").unwrap();
        assert_eq!(found.game.as_deref(), Some("gamemodes/other/x.lua"));
    }

    #[test]
    fn standalone_maps_the_gamemode_to_the_project() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::standalone(dir.path(), "hl2rp");
        assert_eq!(
            tree.game_path("gamemodes/hl2rp/db/schema.lua").unwrap(),
            dir.path().join("db/schema.lua")
        );
        assert_eq!(
            tree.game_path("lua/_flux/environment.lua").unwrap(),
            dir.path().join(".fpkg/garrysmod/lua/_flux/environment.lua")
        );
        assert_eq!(tree.database(), dir.path().join(".fpkg/garrysmod/sv.db"));
        assert!(tree.game_path("../x").is_none());
        tree.prepare().unwrap();
        assert!(tree.is_dir("gamemodes", "GAME"));
        let (_, dirs) = find(&tree, "gamemodes/*", "GAME", Sort::NameAsc).unwrap();
        assert_eq!(dirs, ["hl2rp"]);
        let (_, dirs) = find(&tree, "*", "LUA", Sort::NameAsc).unwrap();
        assert!(dirs.contains(&"hl2rp".to_string()), "{dirs:?}");
    }

    #[test]
    fn lua_searches_lua_then_gamemodes() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::standalone(dir.path(), "gm");
        touch(&dir.path().join("lib/a.lua"), "x");
        touch(&dir.path().join(".fpkg/garrysmod/lua/gm/lib/a.lua"), "y");
        let found = tree.find_file("gm/lib/a.lua", "LUA").unwrap();
        assert_eq!(found.game.as_deref(), Some("lua/gm/lib/a.lua"));
        std::fs::remove_file(dir.path().join(".fpkg/garrysmod/lua/gm/lib/a.lua")).unwrap();
        let found = tree.find_file("gm/lib/a.lua", "LUA").unwrap();
        assert_eq!(found.game.as_deref(), Some("gamemodes/gm/lib/a.lua"));
        assert_eq!(found.real, dir.path().join("lib/a.lua"));
        assert!(tree.exists("gamemodes/gm/lib", "GAME"));
        assert!(tree.is_dir("gm/lib", "LUA"));
        assert!(!tree.is_dir("gm/lib/a.lua", "LUA"));
        assert!(tree.find_file("gm/lib/a.lua", "DATA").is_none());
        assert!(tree.find_file("gm/lib/a.lua", "BSP").is_none());
    }

    #[test]
    fn find_lists_names_sorted_by_each_flag() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::standalone(dir.path(), "gm");
        let data = dir.path().join(".fpkg/garrysmod/data");
        for (name, secs) in [("b.txt", 300), ("a.txt", 200), ("C.txt", 100)] {
            let p = data.join(name);
            touch(&p, "x");
            let t = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + secs);
            std::fs::File::options()
                .write(true)
                .open(&p)
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        std::fs::create_dir_all(data.join("sub.txt")).unwrap();
        std::fs::create_dir_all(data.join("folder")).unwrap();
        let (files, dirs) = find(&tree, "*.txt", "DATA", Sort::NameAsc).unwrap();
        assert_eq!(files, ["C.txt", "a.txt", "b.txt"]);
        assert_eq!(dirs, ["sub.txt"]);
        let (files, _) = find(&tree, "*.txt", "DATA", Sort::NameDesc).unwrap();
        assert_eq!(files, ["b.txt", "a.txt", "C.txt"]);
        let (files, _) = find(&tree, "*.txt", "DATA", Sort::DateAsc).unwrap();
        assert_eq!(files, ["C.txt", "a.txt", "b.txt"]);
        let (files, _) = find(&tree, "*.txt", "DATA", Sort::DateDesc).unwrap();
        assert_eq!(files, ["b.txt", "a.txt", "C.txt"]);
        let (files, dirs) = find(&tree, "*", "DATA", Sort::NameAsc).unwrap();
        assert_eq!(files.len(), 3);
        assert_eq!(dirs, ["folder", "sub.txt"]);
        let (files, dirs) = find(&tree, "A.*", "DATA", Sort::NameAsc).unwrap();
        assert_eq!((files, dirs), (vec!["a.txt".to_string()], vec![]));
        assert_eq!(
            find(&tree, "missing/*", "DATA", Sort::NameAsc).unwrap(),
            (vec![], vec![])
        );
        assert!(find(&tree, "../*", "DATA", Sort::NameAsc).is_none());
        assert_eq!(tree.size("a.txt", "DATA"), 1);
        assert_eq!(tree.size("nope.txt", "DATA"), -1);
        assert_eq!(tree.time("a.txt", "DATA"), 1_700_000_200);
        assert_eq!(tree.time("nope.txt", "DATA"), 0);
    }

    #[test]
    fn find_merges_the_folders_of_a_search_path() {
        let dir = tempfile::tempdir().unwrap();
        let tree = Tree::standalone(dir.path(), "gm");
        touch(&dir.path().join("x/one.lua"), "1");
        touch(&dir.path().join(".fpkg/garrysmod/lua/gm/x/two.lua"), "2");
        touch(&dir.path().join(".fpkg/garrysmod/lua/gm/x/one.lua"), "1");
        let (files, _) = find(&tree, "gm/x/*.lua", "LUA", Sort::NameAsc).unwrap();
        assert_eq!(files, ["one.lua", "two.lua"]);
    }

    #[test]
    fn server_layout_includes_addons() {
        let dir = tempfile::tempdir().unwrap();
        let gm = dir.path().join("garrysmod/gamemodes/gm");
        touch(&gm.join("gm.txt"), "x");
        touch(
            &dir.path().join("garrysmod/addons/zz/lua/autorun/a.lua"),
            "a",
        );
        let tree = Tree::server(dir.path(), "gm", &gm);
        let found = tree.find_file("autorun/a.lua", "LUA").unwrap();
        assert_eq!(found.game.as_deref(), Some("addons/zz/lua/autorun/a.lua"));
        assert!(tree.exists("gamemodes/gm/gm.txt", "GAME"));
        assert!(!tree.exists("gm.txt", "BASE_PATH"));
        assert!(tree.exists("garrysmod", "BASE_PATH"));
    }
}

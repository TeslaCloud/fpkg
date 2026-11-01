//! Deterministic packing.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use super::{
    ArchiveError, IGNORE_FILE, MARKER, MAX_COMPRESSED, MAX_MEMBERS, MAX_UNCOMPRESSED, STORE_BELOW, check_member_path,
    excluded_dir, excluded_file,
};
use crate::spec::SPEC_FILE;

#[derive(Clone, Debug)]
pub struct PackOptions {
    /// Timestamp of every member, seconds since the Unix epoch
    /// (`SOURCE_DATE_EPOCH`); `None` is zip's epoch, 1980-01-01 00:00:00.
    pub mtime: Option<i64>,
    /// Deflate level, 6 by default (`[pack] compression`).
    pub level: i64,
    /// The project directory, whose `.fpkgignore` applies to packages under
    /// it. `None` packs with the package's own ignore files only, which is
    /// how `fpkg verify` re-packs an installed package.
    pub project_root: Option<PathBuf>,
}

impl Default for PackOptions {
    fn default() -> Self {
        PackOptions { mtime: None, level: 6, project_root: None }
    }
}

impl PackOptions {
    /// Defaults with `SOURCE_DATE_EPOCH` from the environment.
    pub fn from_env(level: i64, project_root: Option<PathBuf>) -> Self {
        let mtime = std::env::var("SOURCE_DATE_EPOCH").ok().and_then(|s| s.trim().parse::<i64>().ok());
        PackOptions { mtime, level, project_root }
    }
}

/// A packed archive.
#[derive(Clone, Debug)]
pub struct Packed {
    pub bytes: Vec<u8>,
    pub sha256: String,
    /// Members in archive order.
    pub members: Vec<Member>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Member {
    pub path: String,
    pub size: u64,
}

/// A file chosen for the archive.
#[derive(Clone, Debug)]
pub struct Selected {
    /// Path relative to the package directory, with forward slashes.
    pub rel: String,
    pub abs: PathBuf,
}

fn build(root: &Path, file: &Path) -> Result<Option<Gitignore>, ArchiveError> {
    if !file.is_file() {
        return Ok(None);
    }
    let mut b = GitignoreBuilder::new(root);
    if let Some(e) = b.add(file) {
        return Err(ArchiveError::Ignore { path: file.to_path_buf(), message: e.to_string() });
    }
    b.build().map(Some).map_err(|e| ArchiveError::Ignore { path: file.to_path_buf(), message: e.to_string() })
}

/// The files under `dir` that go into the archive, in archive order. Excludes the
/// fixed exclusions, the root `.fpkg.toml`, and whatever the package's
/// `.gitignore`/`.fpkgignore` files or the project's `.fpkgignore` ignore.
pub fn select(dir: &Path, project_root: Option<&Path>) -> Result<Vec<Selected>, ArchiveError> {
    let project_ignore = match project_root {
        Some(root) if dir.starts_with(root) => build(root, &root.join(IGNORE_FILE))?,
        _ => None,
    };
    let mut wb = ignore::WalkBuilder::new(dir);
    wb.standard_filters(false)
        .hidden(false)
        .parents(false)
        .ignore(false)
        .git_global(false)
        .git_exclude(false)
        .git_ignore(true)
        .require_git(false)
        .follow_links(false)
        .add_custom_ignore_filename(IGNORE_FILE);
    let root = dir.to_path_buf();
    wb.filter_entry(move |e| {
        if e.depth() == 0 {
            return true;
        }
        let name = e.file_name().to_string_lossy();
        let is_dir = e.file_type().is_some_and(|t| t.is_dir());
        let fixed = if is_dir {
            excluded_dir(&name)
        } else {
            excluded_file(&name) || e.path().strip_prefix(&root).is_ok_and(|r| r == Path::new(MARKER))
        };
        if fixed {
            return false;
        }
        match &project_ignore {
            Some(gi) => !gi.matched(e.path(), is_dir).is_ignore(),
            None => true,
        }
    });
    let mut out = Vec::new();
    for entry in wb.build() {
        let entry = entry.map_err(|e| ArchiveError::Ignore { path: dir.to_path_buf(), message: e.to_string() })?;
        if entry.depth() == 0 {
            continue;
        }
        let path = entry.path();
        let Some(ft) = entry.file_type() else {
            return Err(ArchiveError::NotRegular(path.to_path_buf()));
        };
        if ft.is_dir() {
            continue;
        }
        if ft.is_symlink() {
            return Err(ArchiveError::Symlink(path.to_path_buf()));
        }
        if !ft.is_file() {
            return Err(ArchiveError::NotRegular(path.to_path_buf()));
        }
        let rel_path = path.strip_prefix(dir).unwrap_or(path);
        let Some(rel) = rel_path.to_str() else {
            return Err(ArchiveError::BadPath {
                path: rel_path.to_string_lossy().into_owned(),
                reason: "it is not UTF-8".into(),
            });
        };
        let rel = rel.replace(std::path::MAIN_SEPARATOR, "/");
        check_member_path(&rel)?;
        out.push(Selected { rel, abs: path.to_path_buf() });
    }
    sort_members(&mut out, |s| &s.rel);
    Ok(out)
}

/// `packagespec.lua` first, then bytewise.
pub(crate) fn sort_members<T>(v: &mut [T], key: impl Fn(&T) -> &str) {
    v.sort_by(|a, b| {
        let (a, b) = (key(a), key(b));
        (a != SPEC_FILE).cmp(&(b != SPEC_FILE)).then_with(|| a.as_bytes().cmp(b.as_bytes()))
    });
}

fn zip_time(mtime: Option<i64>) -> DateTime {
    let fallback = DateTime::default();
    let Some(secs) = mtime else { return fallback };
    let Ok(t) = time::OffsetDateTime::from_unix_timestamp(secs) else { return fallback };
    // Zip times cover 1980 to 2107; clamp rather than fail.
    if t.year() < 1980 {
        return fallback;
    }
    if t.year() > 2107 {
        return DateTime::from_date_and_time(2107, 12, 31, 23, 59, 58).unwrap_or(fallback);
    }
    DateTime::from_date_and_time(t.year() as u16, u8::from(t.month()), t.day(), t.hour(), t.minute(), t.second())
        .unwrap_or(fallback)
}

/// Packs the package directory `dir` as `<id>/...`.
pub fn pack(dir: &Path, id: &str, opts: &PackOptions) -> Result<Packed, ArchiveError> {
    let files = select(dir, opts.project_root.as_deref())?;
    if !files.iter().any(|f| f.rel == SPEC_FILE) {
        return Err(ArchiveError::MissingSpec(format!("{id}/{SPEC_FILE}")));
    }
    if files.len() > MAX_MEMBERS {
        return Err(ArchiveError::TooManyMembers(files.len()));
    }
    let time = zip_time(opts.mtime);
    let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
    let mut members = Vec::with_capacity(files.len());
    let mut total = 0u64;
    for f in &files {
        let data = std::fs::read(&f.abs).map_err(|e| ArchiveError::io(&f.abs, e))?;
        let size = data.len() as u64;
        total += size;
        if total > MAX_UNCOMPRESSED {
            return Err(ArchiveError::TooLarge { what: "the package uncompressed".into(), size: total, limit: MAX_UNCOMPRESSED });
        }
        let path = format!("{id}/{}", f.rel);
        check_member_path(&path)?;
        let options = if size < STORE_BELOW {
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
        } else {
            SimpleFileOptions::default().compression_method(CompressionMethod::Deflated).compression_level(Some(opts.level))
        }
        .last_modified_time(time)
        .unix_permissions(0o644);
        zw.start_file(path.as_str(), options)?;
        zw.write_all(&data).map_err(|e| ArchiveError::io(&f.abs, e))?;
        members.push(Member { path, size });
    }
    let bytes = zw.finish()?.into_inner();
    if bytes.len() as u64 > MAX_COMPRESSED {
        return Err(ArchiveError::TooLarge { what: "the archive".into(), size: bytes.len() as u64, limit: MAX_COMPRESSED });
    }
    // The same checks as extraction, ratios included, on what was produced.
    super::validate(&bytes, id)?;
    let sha256 = crate::sha256_hex(&bytes);
    Ok(Packed { bytes, sha256, members })
}

/// SHA-256 over sorted `<sha256>  <path>\n` lines for every file under `dir`
/// except the root `.fpkg.toml`, ignoring nothing. Non-regular files are an error.
pub fn tree_sha256(dir: &Path) -> Result<String, ArchiveError> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for entry in walkdir::WalkDir::new(dir).follow_links(false).min_depth(1) {
        let entry = entry.map_err(|e| {
            let path = e.path().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf());
            ArchiveError::Io { path, source: e.into() }
        })?;
        let ft = entry.file_type();
        if ft.is_dir() {
            continue;
        }
        let rel = entry.path().strip_prefix(dir).unwrap_or(entry.path());
        if rel == Path::new(MARKER) {
            continue;
        }
        if ft.is_symlink() {
            return Err(ArchiveError::Symlink(entry.path().to_path_buf()));
        }
        if !ft.is_file() {
            return Err(ArchiveError::NotRegular(entry.path().to_path_buf()));
        }
        let rel = rel.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/");
        files.push((rel, entry.path().to_path_buf()));
    }
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut h = Sha256::new();
    for (rel, abs) in &files {
        let data = std::fs::read(abs).map_err(|e| ArchiveError::io(abs, e))?;
        h.update(hex::encode(Sha256::digest(&data)).as_bytes());
        h.update(b"  ");
        h.update(rel.as_bytes());
        h.update(b"\n");
    }
    Ok(hex::encode(h.finalize()))
}

//! Safe extraction: everything is checked before anything is written, then a
//! temporary sibling directory is renamed over the destination.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::Path;

use zip::{CompressionMethod, ZipArchive};

use super::{
    ArchiveError, MARKER, MAX_COMPRESSED, MAX_MEMBERS, MAX_RATIO, MAX_UNCOMPRESSED, check_member_path,
};
use crate::spec::SPEC_FILE;

/// One validated member.
#[derive(Clone, Debug)]
pub struct EntryInfo {
    pub index: usize,
    /// The full member path, `<id>/...`.
    pub path: String,
    pub size: u64,
    pub compressed: u64,
}

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
}

/// The member names as the central directory lists them, read directly:
/// the `zip` crate keys members by name, so two entries with one name would
/// silently become one.
fn central_directory_names(bytes: &[u8]) -> Result<Vec<Vec<u8>>, ArchiveError> {
    let bad = |m: &str| ArchiveError::Zip(m.to_string());
    const EOCD: u32 = 0x0605_4b50;
    const CDH: u32 = 0x0201_4b50;
    if bytes.len() < 22 {
        return Err(bad("too short for an end of central directory record"));
    }
    let lowest = bytes.len().saturating_sub(22 + 0xFFFF);
    let eocd = (lowest..=bytes.len() - 22)
        .rev()
        .find(|&i| u32_at(bytes, i) == Some(EOCD))
        .ok_or_else(|| bad("no end of central directory record"))?;
    let count = u16_at(bytes, eocd + 10).ok_or_else(|| bad("truncated end record"))?;
    let offset = u32_at(bytes, eocd + 16).ok_or_else(|| bad("truncated end record"))?;
    if count == 0xFFFF || offset == 0xFFFF_FFFF {
        return Err(bad("zip64 archives are not allowed"));
    }
    let mut names = Vec::with_capacity(count as usize);
    let mut pos = offset as usize;
    for _ in 0..count {
        if u32_at(bytes, pos) != Some(CDH) {
            return Err(bad("a central directory entry is malformed"));
        }
        let n = u16_at(bytes, pos + 28).ok_or_else(|| bad("truncated central directory"))? as usize;
        let e = u16_at(bytes, pos + 30).ok_or_else(|| bad("truncated central directory"))? as usize;
        let c = u16_at(bytes, pos + 32).ok_or_else(|| bad("truncated central directory"))? as usize;
        let name = bytes.get(pos + 46..pos + 46 + n).ok_or_else(|| bad("truncated central directory"))?;
        names.push(name.to_vec());
        pos += 46 + n + e + c;
    }
    Ok(names)
}

/// Checks an archive completely without writing anything: limits, path rules,
/// the `<id>/` prefix, regular files only, ratios, and `<id>/packagespec.lua`.
pub fn validate(bytes: &[u8], id: &str) -> Result<Vec<EntryInfo>, ArchiveError> {
    if bytes.len() as u64 > MAX_COMPRESSED {
        return Err(ArchiveError::TooLarge { what: "the archive".into(), size: bytes.len() as u64, limit: MAX_COMPRESSED });
    }
    let raw = central_directory_names(bytes)?;
    if raw.len() > MAX_MEMBERS {
        return Err(ArchiveError::TooManyMembers(raw.len()));
    }
    let mut lower = HashSet::new();
    for name in &raw {
        if !lower.insert(String::from_utf8_lossy(name).to_lowercase()) {
            return Err(ArchiveError::Duplicate(String::from_utf8_lossy(name).into_owned()));
        }
    }
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    if zip.len() != raw.len() {
        return Err(ArchiveError::Zip(format!("the central directory lists {} members, the archive holds {}", raw.len(), zip.len())));
    }
    let prefix = format!("{id}/");
    let spec = format!("{id}/{SPEC_FILE}");
    let mut seen = HashSet::new();
    let mut entries = Vec::with_capacity(zip.len());
    let mut total = 0u64;
    let mut has_spec = false;
    for index in 0..zip.len() {
        let f = zip.by_index_raw(index)?;
        let path = std::str::from_utf8(f.name_raw())
            .map_err(|_| ArchiveError::BadPath {
                path: String::from_utf8_lossy(f.name_raw()).into_owned(),
                reason: "it is not UTF-8".into(),
            })?
            .to_string();
        if path.ends_with('/') || f.is_dir() {
            return Err(ArchiveError::NonRegularEntry { path, reason: "a directory entry".into() });
        }
        check_member_path(&path)?;
        if !path.starts_with(&prefix) {
            return Err(ArchiveError::WrongPrefix { path, prefix });
        }
        if f.is_symlink() {
            return Err(ArchiveError::NonRegularEntry { path, reason: "a symlink".into() });
        }
        if let Some(mode) = f.unix_mode()
            && mode & S_IFMT != 0
            && mode & S_IFMT != S_IFREG
        {
            return Err(ArchiveError::NonRegularEntry { path, reason: format!("of file type {:o}", mode & S_IFMT) });
        }
        if f.encrypted() {
            return Err(ArchiveError::NonRegularEntry { path, reason: "encrypted".into() });
        }
        if !matches!(f.compression(), CompressionMethod::Stored | CompressionMethod::Deflated) {
            return Err(ArchiveError::Zip(format!("member '{path}' uses an unsupported compression method")));
        }
        if !seen.insert(path.to_lowercase()) {
            return Err(ArchiveError::Duplicate(path));
        }
        let size = f.size();
        let compressed = f.compressed_size();
        if size >= MAX_RATIO * compressed.max(1) && size > 0 {
            return Err(ArchiveError::Ratio { path, ratio: size / compressed.max(1) });
        }
        total = total.saturating_add(size);
        if total > MAX_UNCOMPRESSED {
            return Err(ArchiveError::TooLarge { what: "the archive uncompressed".into(), size: total, limit: MAX_UNCOMPRESSED });
        }
        if path == spec {
            has_spec = true;
        }
        entries.push(EntryInfo { index, path, size, compressed });
    }
    if !has_spec {
        return Err(ArchiveError::MissingSpec(spec));
    }
    Ok(entries)
}

/// Decompresses every member into memory, enforcing declared sizes.
fn read_all(bytes: &[u8], entries: &[EntryInfo]) -> Result<Vec<(String, Vec<u8>)>, ArchiveError> {
    let mut zip = ZipArchive::new(Cursor::new(bytes))?;
    let mut out = Vec::with_capacity(entries.len());
    for e in entries {
        let f = zip.by_index(e.index)?;
        let mut data = Vec::with_capacity(e.size as usize);
        // One byte more than declared, to notice a member that lies.
        f.take(e.size + 1)
            .read_to_end(&mut data)
            .map_err(|err| ArchiveError::Zip(format!("member '{}': {err}", e.path)))?;
        if data.len() as u64 != e.size {
            return Err(ArchiveError::Zip(format!("member '{}' does not have its declared size", e.path)));
        }
        out.push((e.path.clone(), data));
    }
    Ok(out)
}

/// Extracts a validated archive into `dir` (which must not exist), so that it
/// holds `<id>/...`. Used for the temporary directory of `unpack` and by tests.
pub fn extract_to(bytes: &[u8], id: &str, dir: &Path) -> Result<(), ArchiveError> {
    let entries = validate(bytes, id)?;
    let files = read_all(bytes, &entries)?;
    std::fs::create_dir_all(dir).map_err(|e| ArchiveError::io(dir, e))?;
    for (path, data) in files {
        // Validated: relative, no '.'/'..', forward slashes.
        let dest = path.split('/').fold(dir.to_path_buf(), |p, c| p.join(c));
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ArchiveError::io(parent, e))?;
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&dest)
            .map_err(|e| ArchiveError::io(&dest, e))?;
        std::io::Write::write_all(&mut f, &data).map_err(|e| ArchiveError::io(&dest, e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644));
        }
    }
    Ok(())
}

/// Installs an archive as `packages_dir/<id>` with `marker` as its `.fpkg.toml`,
/// checking the SHA-256 first. A failure leaves the destination as it was.
pub fn unpack(
    bytes: &[u8],
    id: &str,
    expected_sha256: &str,
    packages_dir: &Path,
    marker: Option<&str>,
) -> Result<(), ArchiveError> {
    unpack_checked(bytes, id, expected_sha256, packages_dir, marker, &|_| Ok(()))
}

/// `unpack` with a last check of the staged directory (`<tmp>/<id>`) before
/// it replaces the destination; an `Err` aborts the install.
pub fn unpack_checked(
    bytes: &[u8],
    id: &str,
    expected_sha256: &str,
    packages_dir: &Path,
    marker: Option<&str>,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), ArchiveError> {
    let actual = crate::sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(ArchiveError::Checksum { expected: expected_sha256.to_string(), actual });
    }
    validate(bytes, id)?;
    let tmp = packages_dir.join(format!(".fpkg-tmp-{id}"));
    let old = packages_dir.join(format!(".fpkg-old-{id}"));
    let dest = packages_dir.join(id);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(|e| ArchiveError::io(&tmp, e))?;
    }
    if old.exists() {
        // A replacement interrupted between its two renames: the old tree is
        // the only copy, so it goes back before anything else happens.
        if !dest.exists() {
            std::fs::rename(&old, &dest).map_err(|e| ArchiveError::io(&old, e))?;
        } else {
            std::fs::remove_dir_all(&old).map_err(|e| ArchiveError::io(&old, e))?;
        }
    }
    let result = (|| {
        extract_to(bytes, id, &tmp)?;
        let staged = tmp.join(id);
        check(&staged).map_err(ArchiveError::Rejected)?;
        if let Some(m) = marker {
            let p = staged.join(MARKER);
            std::fs::write(&p, m).map_err(|e| ArchiveError::io(&p, e))?;
        }
        if dest.exists() {
            std::fs::rename(&dest, &old).map_err(|e| ArchiveError::io(&dest, e))?;
        }
        if let Err(e) = std::fs::rename(&staged, &dest) {
            if old.exists() {
                let _ = std::fs::rename(&old, &dest);
            }
            return Err(ArchiveError::io(&dest, e));
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    result
}

//! The archive: deterministic packing, safe extraction, limits.

mod pack;
mod unpack;

use std::path::{Path, PathBuf};

pub use pack::{PackOptions, Packed, pack, tree_sha256};
pub use unpack::{EntryInfo, extract_to, unpack, unpack_checked, validate};

/// Limits enforced on pack, on extract and by the server.
pub const MAX_MEMBERS: usize = 10_000;
pub const MAX_COMPRESSED: u64 = 64 * 1024 * 1024;
pub const MAX_UNCOMPRESSED: u64 = 256 * 1024 * 1024;
/// The compression ratio of every member must stay under 100:1.
pub const MAX_RATIO: u64 = 100;
pub const MAX_COMPONENT_BYTES: usize = 255;
/// Files under this many bytes are stored, not deflated.
pub const STORE_BELOW: u64 = 64;
/// The install marker, never packed.
pub const MARKER: &str = ".fpkg.toml";
pub const IGNORE_FILE: &str = ".fpkgignore";

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("{0} is a symlink; packages cannot contain symlinks")]
    Symlink(PathBuf),
    #[error("{0} is not a regular file")]
    NotRegular(PathBuf),
    #[error("invalid member path '{path}': {reason}")]
    BadPath { path: String, reason: String },
    #[error("the archive has {0} members; the limit is {MAX_MEMBERS}")]
    TooManyMembers(usize),
    #[error("{what} is {size} bytes; the limit is {limit}")]
    TooLarge { what: String, size: u64, limit: u64 },
    #[error("member '{path}' compresses {ratio}:1; the limit is under {MAX_RATIO}:1")]
    Ratio { path: String, ratio: u64 },
    #[error("checksum mismatch: expected sha256 {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("the archive has no {0}")]
    MissingSpec(String),
    #[error("member '{path}' is not under '{prefix}'")]
    WrongPrefix { path: String, prefix: String },
    #[error("member '{0}' appears twice (names are compared without case)")]
    Duplicate(String),
    #[error("member '{path}' is {reason}; only regular files are allowed")]
    NonRegularEntry { path: String, reason: String },
    #[error("{0}")]
    Rejected(String),
    #[error("invalid zip: {0}")]
    Zip(String),
    #[error("ignore file {path}: {message}")]
    Ignore { path: PathBuf, message: String },
    #[error("{path}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl ArchiveError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        ArchiveError::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl From<zip::result::ZipError> for ArchiveError {
    fn from(e: zip::result::ZipError) -> Self {
        ArchiveError::Zip(e.to_string())
    }
}

/// The archive path rules for one member path (`<id>/lib/x.lua`): forward
/// slashes, no `.` or `..`, not absolute, no drive letters, components of at
/// most 255 bytes, and none that Windows cannot create (a device name such as
/// `CON` or `nul.lua`, or a name that ends in a dot or a space). UTF-8 is
/// checked by the caller.
pub fn check_member_path(path: &str) -> Result<(), ArchiveError> {
    let bad = |reason: &str| {
        Err(ArchiveError::BadPath {
            path: path.to_string(),
            reason: reason.to_string(),
        })
    };
    if path.is_empty() {
        return bad("it is empty");
    }
    if path.starts_with('/') {
        return bad("it is absolute");
    }
    if path.contains('\\') {
        return bad("it contains a backslash");
    }
    // ':' would name a drive (C:) or an alternate data stream on Windows.
    if path.contains(':') {
        return bad("it contains ':'");
    }
    if path.chars().any(|c| c.is_control()) {
        return bad("it contains a control character");
    }
    for comp in path.split('/') {
        match comp {
            "" => return bad("it has an empty component"),
            "." | ".." => return bad("it has a '.' or '..' component"),
            c if c.len() > MAX_COMPONENT_BYTES => {
                return bad("a component is longer than 255 bytes");
            }
            c if c.ends_with('.') || c.ends_with(' ') => {
                return bad("a component ends in a dot or a space, which Windows drops");
            }
            c if is_windows_device(c) => {
                return bad(
                    "a component is a device name on Windows (CON, PRN, AUX, NUL, COM1, LPT1, ...)",
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Whether Windows takes a file name for a device: `CON`, `PRN`, `AUX`, `NUL`,
/// `COM0` to `COM9` or `LPT0` to `LPT9`, in any case, also with an extension.
fn is_windows_device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end()
        .to_ascii_uppercase();
    match stem.as_str() {
        "CON" | "PRN" | "AUX" | "NUL" => true,
        s => {
            (s.starts_with("COM") || s.starts_with("LPT"))
                && s.len() == 4
                && s.as_bytes()[3].is_ascii_digit()
        }
    }
}

/// The fixed exclusions for a directory name.
pub fn excluded_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | ".svn" | ".vscode" | ".idea" | "node_modules" | "target"
    )
}

/// The fixed exclusions for a file name.
pub fn excluded_file(name: &str) -> bool {
    // A `.git` or `.svn` file (a submodule's gitlink) is litter too.
    matches!(name, ".DS_Store" | ".git" | ".svn")
        || name.starts_with("._")
        || [
            ".swp",
            ".local.yml",
            ".local.lua",
            ".generated.lua",
            ".db",
            ".old",
            ".cache",
            ".env",
        ]
        .iter()
        .any(|ext| name.ends_with(ext))
}

#[cfg(test)]
mod tests;

/// The package-relative paths `pack` would include, for `fpkg check`.
pub fn pack_selection(dir: &Path, opts: &PackOptions) -> Result<Vec<String>, ArchiveError> {
    Ok(pack::select(dir, opts)?
        .into_iter()
        .map(|s| s.rel)
        .collect())
}

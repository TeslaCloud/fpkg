//! `fpkg check`.

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::path::{Component, Path};

use serde::Serialize;

use super::{DepKind, Manifest, SPEC_FILE, SpecLocation};
use crate::PackageId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
}

impl Diagnostic {
    fn error(message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Error, message: message.into() }
    }
    fn warning(message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Warning, message: message.into() }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        write!(f, "{s}: {}", self.message)
    }
}

/// Known SPDX license ids; not the whole list (`'file'` with a LICENSE covers the rest).
const SPDX: &[&str] = &[
    "0BSD", "AFL-3.0", "AGPL-3.0", "AGPL-3.0-only", "AGPL-3.0-or-later", "Apache-1.1", "Apache-2.0",
    "Artistic-2.0", "BSD-1-Clause", "BSD-2-Clause", "BSD-3-Clause", "BSD-3-Clause-Clear", "BSD-4-Clause",
    "BSL-1.0", "CC-BY-3.0", "CC-BY-4.0", "CC-BY-NC-4.0", "CC-BY-NC-SA-4.0", "CC-BY-ND-4.0", "CC-BY-SA-3.0",
    "CC-BY-SA-4.0", "CC0-1.0", "CDDL-1.0", "ECL-2.0", "EPL-1.0", "EPL-2.0", "EUPL-1.1", "EUPL-1.2",
    "GPL-1.0-only", "GPL-1.0-or-later", "GPL-2.0", "GPL-2.0-only", "GPL-2.0-or-later", "GPL-3.0",
    "GPL-3.0-only", "GPL-3.0-or-later", "ISC", "LGPL-2.0-only", "LGPL-2.0-or-later", "LGPL-2.1",
    "LGPL-2.1-only", "LGPL-2.1-or-later", "LGPL-3.0", "LGPL-3.0-only", "LGPL-3.0-or-later", "LPPL-1.3c",
    "MIT", "MIT-0", "MPL-1.1", "MPL-2.0", "MS-PL", "MS-RL", "MulanPSL-2.0", "NCSA", "ODbL-1.0", "OFL-1.1",
    "OSL-3.0", "PostgreSQL", "Unicode-DFS-2016", "Unlicense", "UPL-1.0", "Vim", "W3C", "WTFPL", "X11",
    "Zlib", "zlib-acknowledgement",
];

/// Whether `license` is a known SPDX id or an expression of them
/// (`MIT OR Apache-2.0`, `GPL-2.0-only WITH Classpath-exception-2.0`).
pub fn is_known_license(license: &str) -> bool {
    let spaced = license.replace('(', " ( ").replace(')', " ) ");
    let mut tokens = spaced.split_whitespace().peekable();
    let mut any = false;
    while let Some(t) = tokens.next() {
        match t {
            "(" | ")" | "AND" | "OR" => {}
            "WITH" => {
                // Exception ids are not checked.
                tokens.next();
            }
            id => {
                let id = id.strip_suffix('+').unwrap_or(id);
                if !SPDX.iter().any(|k| k.eq_ignore_ascii_case(id)) {
                    return false;
                }
                any = true;
            }
        }
    }
    any
}

fn has_license_file(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_ascii_uppercase();
                (n.starts_with("LICENSE") || n.starts_with("LICENCE")) && e.path().is_file()
            })
        })
        .unwrap_or(false)
}

/// A relative path that stays inside its base, normalised to forward slashes.
fn normal_rel(path: &str) -> Option<String> {
    let mut parts = Vec::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(s) => parts.push(s.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.is_empty() { None } else { Some(parts.join("/")) }
}

/// Validates a manifest against its directory. The result is sorted, errors
/// first.
pub fn check(m: &Manifest, loc: &SpecLocation) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let dir = &loc.dir;

    if !loc.is_project_manifest {
        let dir_name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if m.id != dir_name {
            out.push(Diagnostic::error(format!("id '{}' does not match the directory name '{dir_name}'", m.id)));
        }
        if let Some(reason) = PackageId::problem(&m.id) {
            out.push(Diagnostic::error(format!(
                "'{}' is not a valid package id: {reason} (^[a-z][a-z0-9_]{{0,63}}$)",
                m.id
            )));
        }
    }
    for p in &m.problems {
        out.push(Diagnostic::error(p.clone()));
    }
    if m.name.trim().is_empty() {
        out.push(Diagnostic::error("s.name is missing"));
    }
    if m.version_components < 3 {
        out.push(Diagnostic::warning(format!(
            "version '{}' should be written with three components, '{}' (fpkg publish requires it)",
            m.version_written, m.version
        )));
    }

    // Listed files exist, stay inside the package and are not excluded from
    // the archive. The project manifest is never packed.
    let packed: Option<HashSet<String>> = if loc.is_project_manifest {
        None
    } else {
        match crate::archive::pack_selection(dir, Some(&loc.project_root)) {
            Ok(sel) => Some(sel.into_iter().collect()),
            Err(e) => {
                out.push(Diagnostic::error(format!("cannot select the package's files: {e}")));
                None
            }
        }
    };
    for (field, list) in [("file", &m.files), ("cl_file", &m.cl_files), ("sv_file", &m.sv_files)] {
        for f in list.iter() {
            check_listed(&mut out, dir, &format!("s.{field}"), f, packed.as_ref());
        }
    }

    let mut seen = BTreeSet::new();
    for d in &m.dependencies {
        match d.kind {
            DepKind::Package => {
                if let Some(reason) = PackageId::problem(&d.id) {
                    out.push(Diagnostic::error(format!("dependency '{}' is not a valid package id: {reason}", d.id)));
                }
                if !loc.is_project_manifest && d.id == m.id {
                    out.push(Diagnostic::error(format!("'{}' depends on itself", m.id)));
                }
            }
            DepKind::File => check_listed(&mut out, dir, "s.depends", &d.id, packed.as_ref()),
            DepKind::Path => {
                if !loc.is_project_manifest {
                    out.push(Diagnostic::error(format!(
                        "path dependency '{}' is only allowed in the project manifest",
                        d.id
                    )));
                } else if !dir.join(&d.id).join(SPEC_FILE).is_file() {
                    out.push(Diagnostic::error(format!("path dependency '{}' has no {SPEC_FILE}", d.id)));
                } else if let Some(id) = d.package_id()
                    && let Some(reason) = PackageId::problem(id)
                {
                    out.push(Diagnostic::error(format!("path dependency '{}': '{id}' {reason}", d.id)));
                }
            }
        }
        if d.kind != DepKind::File
            && let Some(id) = d.package_id()
            && !seen.insert(id.to_string())
        {
            out.push(Diagnostic::warning(format!("'{id}' is listed twice in s.depends")));
        }
    }

    match m.license.as_deref() {
        None => out.push(Diagnostic::warning("s.license is missing")),
        Some("file") => {
            if !has_license_file(dir) {
                out.push(Diagnostic::error("s.license is 'file' but there is no LICENSE* file in the package"));
            }
        }
        Some(l) if !is_known_license(l) => {
            out.push(Diagnostic::error(format!("s.license '{l}' is not a known SPDX id (or 'file' with a LICENSE)")))
        }
        Some(_) => {}
    }

    if m.serverside && m.clientside {
        out.push(Diagnostic::error("s.serverside and s.clientside are both true"));
    }

    for f in &m.unknown_fields {
        let hint = match f.as_str() {
            "dependency" | "dep" | "depend" => "; declare dependencies with s.depends 'id'",
            _ => "",
        };
        out.push(Diagnostic::warning(format!("unknown field s.{f}, which Flux ignores{hint}")));
    }

    out.sort_by_key(|d| std::cmp::Reverse(d.severity));
    out
}

fn check_listed(out: &mut Vec<Diagnostic>, dir: &Path, what: &str, path: &str, packed: Option<&HashSet<String>>) {
    let Some(rel) = normal_rel(path) else {
        out.push(Diagnostic::error(format!("{what} '{path}' is not a relative path inside the package")));
        return;
    };
    let full = dir.join(&rel);
    if !full.is_file() {
        out.push(Diagnostic::error(format!("{what} '{path}' does not exist")));
        return;
    }
    if let (Ok(d), Ok(f)) = (dir.canonicalize(), full.canonicalize())
        && !f.starts_with(d)
    {
        out.push(Diagnostic::error(format!("{what} '{path}' is outside the package")));
        return;
    }
    if let Some(p) = packed
        && !p.contains(&rel)
    {
        out.push(Diagnostic::error(format!("{what} '{path}' is excluded from the archive")));
    }
}

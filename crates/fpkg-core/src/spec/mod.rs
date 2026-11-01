//! `packagespec.lua`: evaluation on the nl VM, the derived `Manifest` and `check`.

mod check;
pub mod eval;
mod process;

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use check::{Diagnostic, Severity, check, is_known_license};
pub use eval::{EvalInput, Evaluation, LuaData, evaluate_here, sandbox_names};
pub use process::{Evaluator, child_main, evaluator, set_evaluator};

use crate::version::{Constraint, Version};

pub const SPEC_FILE: &str = "packagespec.lua";

/// `--env`: what `IS_DEVELOPMENT`, `IS_TEST` and `IS_PRODUCTION` say.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    #[default]
    Production,
    Development,
    Test,
}

impl Env {
    pub fn as_str(self) -> &'static str {
        match self {
            Env::Production => "production",
            Env::Development => "development",
            Env::Test => "test",
        }
    }
}

impl FromStr for Env {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "production" => Ok(Env::Production),
            "development" => Ok(Env::Development),
            "test" => Ok(Env::Test),
            other => Err(format!("unknown environment '{other}' (production, development or test)")),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum SpecError {
    #[error("{file}: {message}")]
    Eval { file: String, message: String },
    #[error("{file}: the evaluator {status}{}", if stderr.is_empty() { String::new() } else { format!("\n{stderr}") })]
    Crashed { file: String, status: String, stderr: String },
    #[error("no spec evaluator: set FPKG_EVALUATOR to fpkg-eval-spec, or call spec::set_evaluator")]
    NoEvaluator,
    #[error("{file}: Package:describe(function(s) ... end) was never called")]
    NotDescribed { file: String },
    #[error("{file}: {message}")]
    Invalid { file: String, message: String },
    #[error("cannot read {file}: {message}")]
    Io { file: String, message: String },
}

/// What a dependency names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DepKind {
    /// A package id, resolved from the workspace or a registry.
    Package,
    /// A `.lua` file to include, relative to the spec's directory.
    File,
    /// A package directory outside `packages/` (`'../shared/foo'`), allowed in
    /// the project manifest; its id is the last path component.
    Path,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    /// The package id, the `.lua` path, or the directory path, as written.
    pub id: String,
    pub constraint: Constraint,
    pub kind: DepKind,
}

impl Dependency {
    /// The package id a `Package` or `Path` dependency resolves to.
    pub fn package_id(&self) -> Option<&str> {
        match self.kind {
            DepKind::Package => Some(&self.id),
            DepKind::Path => self.id.trim_end_matches('/').rsplit('/').next(),
            DepKind::File => None,
        }
    }
}

/// The manifest derived from a spec.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    /// The directory name (the project id for the project manifest).
    pub id: String,
    pub name: String,
    pub version: Version,
    /// How many numeric components the spec wrote (1 to 3).
    pub version_components: u8,
    /// The version as the spec wrote it.
    pub version_written: String,
    #[serde(with = "date_serde")]
    pub date: Option<time::Date>,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub email: Option<String>,
    pub website: Option<String>,
    pub license: Option<String>,
    pub authors: Vec<String>,
    pub globals: Vec<String>,
    pub files: Vec<String>,
    pub cl_files: Vec<String>,
    pub sv_files: Vec<String>,
    pub serverside: bool,
    pub clientside: bool,
    pub reload: bool,
    pub dependencies: Vec<Dependency>,
    pub resources: Vec<String>,
    pub client_env: Vec<(String, String)>,
    /// Calls the spec made that fpkg recorded and did not run.
    pub includes: Vec<String>,
    pub cs_files: Vec<String>,
    /// Fields that Flux does not know.
    pub unknown_fields: Vec<String>,
    /// Problems found while building the manifest; `check` reports them.
    pub problems: Vec<String>,
    /// What the spec printed.
    pub output: Vec<String>,
}

mod date_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<time::Date>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.collect_str(&super::format_date(*d)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<time::Date>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(None),
            Some(s) => super::parse_date(&s).map(Some).ok_or_else(|| serde::de::Error::custom(format!("invalid date '{s}'"))),
        }
    }
}

pub fn format_date(d: time::Date) -> String {
    format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
}

/// The field names Flux reads (singular, as stored).
pub const KNOWN_FIELDS: &[&str] = &[
    "name",
    "version",
    "date",
    "summary",
    "description",
    "author",
    "email",
    "file",
    "cl_file",
    "sv_file",
    "global",
    "website",
    "license",
    "serverside",
    "clientside",
    "reload",
];

impl Manifest {
    /// Builds the manifest from an evaluation. Fails when there is no
    /// `describe` call, no usable version, or an invalid constraint; other
    /// problems are kept in `problems` for `check`.
    pub fn from_evaluation(id: &str, file: &str, ev: Evaluation) -> Result<Manifest, SpecError> {
        let invalid = |message: String| SpecError::Invalid { file: file.to_string(), message };
        if !ev.described {
            return Err(SpecError::NotDescribed { file: file.to_string() });
        }
        let mut problems = Vec::new();
        let f = &ev.fields;
        let version_written = match f.get("version") {
            Some(LuaData::String(s)) => s.clone(),
            None | Some(LuaData::Nil) => return Err(invalid("s.version is missing".into())),
            Some(other) => return Err(invalid(format!("s.version must be a string, not a {}", other.type_name()))),
        };
        let (version, version_components) =
            Version::parse_with_components(&version_written).map_err(|e| invalid(e.to_string()))?;

        let string = |key: &str, problems: &mut Vec<String>| -> Option<String> {
            match f.get(key) {
                None | Some(LuaData::Nil) => None,
                Some(LuaData::String(s)) => Some(s.clone()),
                Some(other) => {
                    problems.push(format!("s.{key} must be a string, not a {}", other.type_name()));
                    None
                }
            }
        };
        // Flux accepts a string or a list of strings for these fields.
        let list = |key: &str, problems: &mut Vec<String>| -> Vec<String> {
            match f.get(key) {
                None | Some(LuaData::Nil) => Vec::new(),
                Some(LuaData::String(s)) => vec![s.clone()],
                Some(LuaData::List(items)) => items
                    .iter()
                    .filter_map(|x| match x {
                        LuaData::String(s) => Some(s.clone()),
                        other => {
                            problems.push(format!("s.{key} must hold strings, not a {}", other.type_name()));
                            None
                        }
                    })
                    .collect(),
                Some(LuaData::Map(m)) if m.is_empty() => Vec::new(),
                Some(other) => {
                    problems.push(format!("s.{key} must be a string or a list of strings, not a {}", other.type_name()));
                    Vec::new()
                }
            }
        };
        let boolean = |key: &str, default: bool, problems: &mut Vec<String>| -> bool {
            match f.get(key) {
                None | Some(LuaData::Nil) => default,
                Some(LuaData::Bool(b)) => *b,
                Some(other) => {
                    problems.push(format!("s.{key} must be true or false, not a {}", other.type_name()));
                    default
                }
            }
        };

        let name = string("name", &mut problems).unwrap_or_default();
        let date = string("date", &mut problems).and_then(|d| match parse_date(&d) {
            Some(date) => Some(date),
            None => {
                problems.push(format!("s.date '{d}' is not a YYYY-MM-DD date"));
                None
            }
        });

        let mut dependencies = Vec::new();
        for (dep, constraint) in &ev.depends {
            let kind = if dep.ends_with(".lua") {
                DepKind::File
            } else if dep.contains('/') {
                DepKind::Path
            } else {
                DepKind::Package
            };
            let constraint = match constraint {
                Some(c) => Constraint::parse(c).map_err(|e| invalid(format!("s.depends '{dep}': {e}")))?,
                None => Constraint::any(),
            };
            dependencies.push(Dependency { id: dep.clone(), constraint, kind });
        }

        let unknown_fields = f.keys().filter(|k| !KNOWN_FIELDS.contains(&k.as_str())).cloned().collect();

        Ok(Manifest {
            id: id.to_string(),
            name,
            version,
            version_components,
            version_written,
            date,
            summary: string("summary", &mut problems),
            description: string("description", &mut problems),
            email: string("email", &mut problems),
            website: string("website", &mut problems),
            license: string("license", &mut problems),
            authors: list("author", &mut problems),
            globals: list("global", &mut problems),
            files: list("file", &mut problems),
            cl_files: list("cl_file", &mut problems),
            sv_files: list("sv_file", &mut problems),
            serverside: boolean("serverside", false, &mut problems),
            clientside: boolean("clientside", false, &mut problems),
            // PackageInstance's default.
            reload: boolean("reload", true, &mut problems),
            dependencies,
            resources: ev.resources,
            client_env: ev.client_env,
            includes: ev.includes,
            cs_files: ev.cs_files,
            unknown_fields,
            problems,
            output: ev.output,
        })
    }

    /// The package dependencies (workspace or registry), without `.lua` files.
    pub fn package_deps(&self) -> impl Iterator<Item = &Dependency> {
        self.dependencies.iter().filter(|d| d.kind != DepKind::File)
    }
}

fn parse_date(s: &str) -> Option<time::Date> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(s.trim(), &fmt).ok()
}

/// Where a spec lives in a project.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SpecLocation {
    pub project_root: PathBuf,
    /// The package directory, or the project root for the project manifest.
    pub dir: PathBuf,
    /// The id: directory name, or the project id.
    pub id: String,
    pub project_id: String,
    pub is_project_manifest: bool,
    /// `PACKAGE.__path__` as Flux sets it: the path in the `LUA` search path,
    /// `<gamemode>/packages/<id>/` (`<gamemode>/` for the project manifest).
    pub lua_path: String,
}

impl SpecLocation {
    pub fn spec_path(&self) -> PathBuf {
        self.dir.join(SPEC_FILE)
    }

    /// The project-relative directory with a trailing slash (empty for the
    /// project manifest), as error messages name the spec.
    pub fn rel_dir(&self) -> String {
        match self.dir.strip_prefix(&self.project_root) {
            Ok(p) if p.as_os_str().is_empty() => String::new(),
            Ok(p) => format!("{}/", p.to_string_lossy().replace('\\', "/")),
            // A path dependency outside the project: show it as given.
            Err(_) => format!("{}/", self.dir.to_string_lossy()),
        }
    }
}

/// Reads, evaluates and derives the manifest of the spec at `loc`, with the
/// evaluator (normally a child process).
pub fn load(loc: &SpecLocation, env: Env) -> Result<Manifest, SpecError> {
    load_file(loc, env, &loc.spec_path())
}

/// `load`, reading the spec text from `file` instead of `loc`'s spec.
pub fn load_file(loc: &SpecLocation, env: Env, file: &Path) -> Result<Manifest, SpecError> {
    load_with(evaluator()?, loc, env, file)
}

/// `load_file` with a given evaluator.
pub fn load_with(evaluator: &Evaluator, loc: &SpecLocation, env: Env, file: &Path) -> Result<Manifest, SpecError> {
    match evaluator {
        Evaluator::InProcess => load_here(loc, env, file),
        Evaluator::Process { program, args } => process::run(program, args, loc, env, file),
    }
}

/// Many specs at once, a few child processes in parallel; results in order.
pub fn load_many(locs: &[SpecLocation], env: Env) -> Vec<Result<Manifest, SpecError>> {
    process::run_many(locs, env)
}

/// Evaluates in this process (the child's half, and trusted tests).
pub fn load_here(loc: &SpecLocation, env: Env, file: &Path) -> Result<Manifest, SpecError> {
    let rel = format!("{}{}", loc.rel_dir(), SPEC_FILE);
    let source = std::fs::read_to_string(file).map_err(|e| SpecError::Io { file: rel.clone(), message: e.to_string() })?;
    let ev = evaluate_here(&EvalInput {
        source,
        chunk_name: rel.clone(),
        project_root: loc.project_root.clone(),
        package_path: loc.lua_path.clone(),
        project_id: loc.project_id.clone(),
        env,
    })?;
    Manifest::from_evaluation(&loc.id, &rel, ev)
}

/// Makes this process evaluate specs itself; for fpkg's own unit tests,
/// whose specs are trusted fixtures.
#[cfg(test)]
pub(crate) fn test_in_process() {
    let _ = set_evaluator(Evaluator::InProcess);
}

impl fmt::Display for DepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            DepKind::Package => "package",
            DepKind::File => "file",
            DepKind::Path => "path",
        })
    }
}

/// Field values for display (`fpkg info`), in a stable order.
pub fn display_fields(m: &Manifest) -> BTreeMap<&'static str, String> {
    let mut out = BTreeMap::new();
    out.insert("name", m.name.clone());
    out.insert("version", m.version.to_string());
    if let Some(d) = m.date {
        out.insert("date", format_date(d));
    }
    for (k, v) in [
        ("summary", &m.summary),
        ("description", &m.description),
        ("email", &m.email),
        ("website", &m.website),
        ("license", &m.license),
    ] {
        if let Some(v) = v {
            out.insert(k, v.clone());
        }
    }
    out
}

/// Is this path the spec file of a package directory?
pub fn has_spec(dir: &Path) -> bool {
    dir.join(SPEC_FILE).is_file()
}

#[cfg(test)]
mod tests;

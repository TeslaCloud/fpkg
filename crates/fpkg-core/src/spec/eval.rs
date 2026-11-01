//! Evaluating `packagespec.lua` in this process (the child half of `spec::process`):
//! the nl sandbox with `prelude.lua`, the GLua lexer, no JIT, one million instructions.

use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};

use nl::mlua;
use nl::prelude::*;
use serde::{Deserialize, Serialize};

use super::{Env, SpecError};

/// Instructions between two calls of the count hook.
const HOOK_EVERY: u32 = 1000;
/// Hook calls before the spec is stopped: 1000 * 1000 = one million instructions.
const HOOK_BUDGET: u32 = 1000;
/// Nesting depth at which converting Lua tables stops.
const MAX_DEPTH: usize = 32;
/// Values converted out of the state, all fields together.
const MAX_NODES: usize = 100_000;

/// A Lua value copied out of the state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LuaData {
    Nil,
    Bool(bool),
    Number(f64),
    String(String),
    /// A table with the keys 1..n and nothing else.
    List(Vec<LuaData>),
    /// Any other table; keys are shown as strings.
    Map(BTreeMap<String, LuaData>),
    /// A function, userdata or thread, by type name.
    Other(String),
}

impl LuaData {
    pub fn type_name(&self) -> &'static str {
        match self {
            LuaData::Nil => "nil",
            LuaData::Bool(_) => "boolean",
            LuaData::Number(_) => "number",
            LuaData::String(_) => "string",
            LuaData::List(_) | LuaData::Map(_) => "table",
            LuaData::Other(_) => "value",
        }
    }
}

/// Everything a spec told fpkg, before it is turned into a `Manifest`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evaluation {
    /// The fields assigned on `s`, under their singular names.
    pub fields: BTreeMap<String, LuaData>,
    /// `s.depends` calls in order: name and optional constraint.
    pub depends: Vec<(String, Option<String>)>,
    /// `resource.AddSingleFile` / `resource.AddFile` paths.
    pub resources: Vec<String>,
    /// `add_client_env(name, value)` calls.
    pub client_env: Vec<(String, String)>,
    /// `AddCSLuaFile` calls.
    pub cs_files: Vec<String>,
    /// `include` and `require_relative` calls (recorded, never run).
    pub includes: Vec<String>,
    /// What the spec printed.
    pub output: Vec<String>,
    /// Whether `Package:describe` was called with a function.
    pub described: bool,
}

/// What the sandbox needs to know about where the spec lives.
#[derive(Debug, Clone)]
pub struct EvalInput {
    pub source: String,
    /// Shown in error messages, normally the project-relative path of the spec.
    pub chunk_name: String,
    /// The project directory; `file.Find` never leaves it.
    pub project_root: PathBuf,
    /// `PACKAGE.__path__`: the path Flux gives the package in the `LUA`
    /// search path, `<gamemode>/packages/<id>/`.
    pub package_path: String,
    pub project_id: String,
    pub env: Env,
}

/// Builds the sandbox environment of a fresh state, without running a spec.
fn prepare(lua: &Lua, input: &EvalInput) -> mlua::Result<(Table, Table)> {
    let env = lua.sandbox_env()?;
    let host = lua.create_table()?;
    host.set("path", input.package_path.as_str())?;
    host.set("project_id", input.project_id.as_str())?;
    host.set("env", input.env.as_str())?;
    let root = input.project_root.clone();
    let gamemode = input.project_id.clone();
    let find = lua.create_function(move |lua, (pattern, place): (String, Option<String>)| {
        let (files, dirs) = file_find(&root, &gamemode, &pattern, place.as_deref().unwrap_or("GAME"));
        Ok((lua.create_sequence_from(files)?, lua.create_sequence_from(dirs)?))
    })?;
    host.set("find", find)?;
    let root = input.project_root.clone();
    let gamemode = input.project_id.clone();
    let exists = lua.create_function(move |_, (path, place): (String, Option<String>)| {
        Ok(file_exists(&root, &gamemode, &path, place.as_deref().unwrap_or("GAME")))
    })?;
    host.set("exists", exists)?;
    let prelude: Function = lua
        .load(include_str!("prelude.lua"))
        .set_name("=fpkg prelude")
        .set_mode(LuaChunkMode::Text)
        .into_function()?;
    let state: Table = prelude.call((env.clone(), host))?;
    lua.set_option(NlOption::Glua(true))?;
    lua.set_option(NlOption::Sandbox(true))?;
    Ok((env, state))
}

/// The names in the sandbox environment, sorted (for the allow-list test).
pub fn sandbox_names() -> Result<Vec<String>, SpecError> {
    let err = |e: mlua::Error| SpecError::Eval { file: "sandbox".into(), message: clean_error(&e) };
    let lua = nl::new().map_err(err)?;
    let input = EvalInput {
        source: String::new(),
        chunk_name: "sandbox".into(),
        project_root: std::env::temp_dir(),
        package_path: String::new(),
        project_id: "p".into(),
        env: Env::Production,
    };
    let (env, _) = prepare(&lua, &input).map_err(err)?;
    let mut names: Vec<String> =
        env.pairs::<Value, Value>().flatten().filter_map(|(k, _)| k.as_string().map(|s| s.to_string_lossy().to_string())).collect();
    names.sort();
    Ok(names)
}

/// Evaluates a spec in this process and thread.
pub fn evaluate_here(input: &EvalInput) -> Result<Evaluation, SpecError> {
    let file = input.chunk_name.clone();
    let lua_err = |e: mlua::Error| SpecError::Eval { file: file.clone(), message: clean_error(&e) };
    let lua = nl::new().map_err(lua_err)?;
    let (env, state) = prepare(&lua, input).map_err(lua_err)?;

    // A global hook, so coroutines count too (mlua's per-thread hook skips them).
    // Once spent, it re-arms at every instruction so pcall cannot outrun it.
    let calls = std::cell::Cell::new(0u32);
    lua.set_global_hook(mlua::HookTriggers::new().every_nth_instruction(HOOK_EVERY), move |lua, _| {
        let n = calls.get() + 1;
        calls.set(n);
        if n <= HOOK_BUDGET {
            return Ok(mlua::VmState::Continue);
        }
        lua.set_global_hook(mlua::HookTriggers::new().every_nth_instruction(1), |_, _| {
            Err(mlua::Error::runtime("instruction budget exceeded (one million instructions)"))
        })?;
        Err(mlua::Error::runtime("instruction budget exceeded (one million instructions)"))
    })
    .map_err(lua_err)?;

    let chunk: Function = lua
        .load(input.source.as_str())
        .set_name(format!("={}", input.chunk_name))
        .set_environment(env)
        .set_mode(LuaChunkMode::Text)
        .into_function()
        .map_err(lua_err)?;
    let result = chunk.call::<()>(());
    lua.remove_global_hook();
    result.map_err(lua_err)?;
    read_state(&state).map_err(|message| SpecError::Invalid { file: file.clone(), message })
}

fn read_state(state: &Table) -> Result<Evaluation, String> {
    let e = |e: mlua::Error| clean_error(&e);
    let mut ev = Evaluation { described: state.get("described").map_err(e)?, ..Default::default() };
    let mut conv = Conv { nodes: 0 };
    let meta: Table = state.get("meta").map_err(e)?;
    for pair in meta.pairs::<Value, Value>() {
        let (k, v) = pair.map_err(e)?;
        let key = match k {
            Value::String(s) => s.to_string_lossy().to_string(),
            other => other.type_name().to_string(),
        };
        let data = conv.data(&v, 0, &mut HashSet::new()).map_err(|m| format!("s.{key}: {m}"))?;
        ev.fields.insert(key, data);
    }
    let deps: Table = state.get("deps").map_err(e)?;
    for entry in deps.sequence_values::<Table>() {
        let entry = entry.map_err(e)?;
        ev.depends.push((entry.get(1).map_err(e)?, entry.get(2).map_err(e)?));
    }
    let rec: Table = state.get("rec").map_err(e)?;
    let strings = |name: &str| -> Result<Vec<String>, String> {
        let t: Table = rec.get(name).map_err(e)?;
        t.sequence_values::<String>().collect::<mlua::Result<Vec<_>>>().map_err(e)
    };
    ev.resources = strings("resources")?;
    ev.cs_files = strings("cs_files")?;
    ev.includes = strings("includes")?;
    ev.output = strings("output")?;
    let env: Table = rec.get("client_env").map_err(e)?;
    for entry in env.sequence_values::<Table>() {
        let entry = entry.map_err(e)?;
        ev.client_env.push((entry.get(1).map_err(e)?, entry.get(2).map_err(e)?));
    }
    let n = ev.resources.len() + ev.cs_files.len() + ev.includes.len() + ev.output.len() + ev.client_env.len() + ev.depends.len();
    if conv.nodes + n > MAX_NODES {
        return Err(format!("the spec recorded more than {MAX_NODES} values"));
    }
    Ok(ev)
}

/// Copies values out of the state, visiting every table once: a table seen
/// again (shared or cyclic) is an error, and so is more than `MAX_NODES`
/// values in all, so the copy is linear in what the spec built.
struct Conv {
    nodes: usize,
}

impl Conv {
    fn data(&mut self, v: &Value, depth: usize, seen: &mut HashSet<usize>) -> Result<LuaData, String> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(format!("more than {MAX_NODES} values"));
        }
        Ok(match v {
            Value::Nil => LuaData::Nil,
            Value::Boolean(b) => LuaData::Bool(*b),
            Value::Integer(i) => LuaData::Number(*i as f64),
            Value::Number(n) => LuaData::Number(*n),
            Value::String(s) => LuaData::String(s.to_string_lossy().to_string()),
            Value::Table(t) => {
                if depth >= MAX_DEPTH {
                    return Err(format!("tables nested more than {MAX_DEPTH} deep"));
                }
                if !seen.insert(t.to_pointer() as usize) {
                    return Err("the same table appears twice (shared or cyclic)".into());
                }
                let len = t.raw_len();
                let mut map = BTreeMap::new();
                let mut count = 0usize;
                for pair in t.pairs::<Value, Value>() {
                    let (k, val) = pair.map_err(|e| clean_error(&e))?;
                    count += 1;
                    let key = match &k {
                        Value::String(s) => s.to_string_lossy().to_string(),
                        Value::Integer(i) => i.to_string(),
                        Value::Number(n) => n.to_string(),
                        other => other.type_name().to_string(),
                    };
                    map.insert(key, self.data(&val, depth + 1, seen)?);
                }
                if count == len && count > 0 {
                    // A sequence: the converted values in key order 1..n.
                    let mut list = Vec::with_capacity(len);
                    for i in 1..=len {
                        list.push(map.remove(&i.to_string()).unwrap_or(LuaData::Nil));
                    }
                    LuaData::List(list)
                } else if map.is_empty() {
                    LuaData::List(Vec::new())
                } else {
                    LuaData::Map(map)
                }
            }
            other => LuaData::Other(other.type_name().to_string()),
        })
    }
}

/// mlua's text without its prefixes and the traceback.
pub(crate) fn clean_error(e: &mlua::Error) -> String {
    let text = match e {
        mlua::Error::SyntaxError { message, .. } => message.clone(),
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::CallbackError { cause, .. } => return clean_error(cause),
        mlua::Error::WithContext { cause, .. } => return clean_error(cause),
        other => other.to_string(),
    };
    let text = text.split("\nstack traceback:").next().unwrap_or(&text);
    text.trim_start_matches("runtime error: ").trim_start_matches("syntax error: ").trim().to_string()
}

/// A project-relative path from a spec, refused if it could leave the project.
fn safe_rel(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

/// The directories a gmod search root maps to inside the project: `GAME` is
/// `content/` then the project root; `LUA` is the project root, where paths
/// that start with the gamemode's own folder name (`flux/packages/...`) have it
/// removed. Other roots find nothing.
fn roots(project_root: &Path, gamemode: &str, place: &str, rel: &str) -> Vec<(PathBuf, String)> {
    match place.to_ascii_uppercase().as_str() {
        "GAME" | "MOD" => vec![(project_root.join("content"), rel.to_string()), (project_root.to_path_buf(), rel.to_string())],
        "LUA" | "LSV" | "LCL" => {
            let mut v = vec![(project_root.to_path_buf(), rel.to_string())];
            if let Some(rest) = rel.strip_prefix(&format!("{gamemode}/")) {
                v.insert(0, (project_root.to_path_buf(), rest.to_string()));
            }
            v
        }
        _ => Vec::new(),
    }
}

/// Is `p` (after resolving symlinks) inside `root`?
fn inside(root: &Path, p: &Path) -> bool {
    match (root.canonicalize(), p.canonicalize()) {
        (Ok(r), Ok(c)) => c.starts_with(r),
        _ => false,
    }
}

/// gmod's `file.Find`: the wildcard may appear in the last component only.
/// Returns sorted file names and directory names (not paths), like gmod.
pub fn file_find(project_root: &Path, gamemode: &str, pattern: &str, place: &str) -> (Vec<String>, Vec<String>) {
    let mut files = std::collections::BTreeSet::new();
    let mut dirs = std::collections::BTreeSet::new();
    for (base, rel) in roots(project_root, gamemode, place, pattern) {
        let (dir_part, name_pat) = match rel.rsplit_once('/') {
            Some((d, n)) => (d.to_string(), n.to_string()),
            None => (String::new(), rel.clone()),
        };
        let Some(dir_rel) = safe_rel(&dir_part) else { continue };
        let dir = base.join(dir_rel);
        if !inside(project_root, &dir) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else { continue };
            if !wildcard(&name_pat, &name) {
                continue;
            }
            let path = entry.path();
            if !inside(project_root, &path) {
                continue;
            }
            if path.is_dir() {
                dirs.insert(name);
            } else if path.is_file() {
                files.insert(name);
            }
        }
    }
    (files.into_iter().collect(), dirs.into_iter().collect())
}

pub fn file_exists(project_root: &Path, gamemode: &str, path: &str, place: &str) -> bool {
    roots(project_root, gamemode, place, path).into_iter().any(|(base, rel)| match safe_rel(&rel) {
        Some(r) => {
            let p = base.join(r);
            p.exists() && inside(project_root, &p)
        }
        None => false,
    })
}

/// `*` matches any run of characters, `?` one; case-insensitive like gmod's
/// file system on the platforms Flux servers run.
fn wildcard(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
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
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards() {
        assert!(wildcard("fa-*.ttf", "fa-solid-900.ttf"));
        assert!(wildcard("*", "x"));
        assert!(wildcard("*.lua", "a.lua"));
        assert!(!wildcard("fa-*.ttf", "fa-solid-900.otf"));
        assert!(wildcard("a?c", "abc"));
        assert!(!wildcard("a?c", "ac"));
    }

    #[test]
    fn safe_paths() {
        assert!(safe_rel("../x").is_none());
        assert!(safe_rel("/etc").is_none());
        assert_eq!(safe_rel("./a/b").unwrap(), PathBuf::from("a/b"));
    }
}

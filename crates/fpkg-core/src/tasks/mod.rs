//! Tasks: the `taskspec.lua` files of a project's packages, run in fpkg's GLua runtime
//! (`fpkg-glua`) without the game.
//!
//! Every package of the project may have a `taskspec.lua` next to its `packagespec.lua`.
//! The files are run in the order Flux loads the packages (dependencies first, the project's
//! own last), so a later file can add prerequisites or actions to an earlier file's task.
//! They are trusted code of the project. While a file runs, `TASKSPEC` is `{ id, path }` of
//! its package; host information for tasks is in the global `fpkg`. The vocabulary
//! (`namespace`, `desc`, `task`, `Tasks.invoke`, `Tasks.exists`) is in `tasks.lua`.
//!
//! Loading the task files only reads: the stand-alone `.fpkg/garrysmod/` is created by the
//! first write of a task, never by listing or loading. A task fails when it raises an error,
//! and also when the runtime reported an uncaught Lua error while it ran or while the loop
//! delivered what was due afterwards (an error in a timer or hook, a file `include` could
//! not run); `ErrorNoHalt` messages do not count.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fpkg_glua::{Config, Failure, Runtime};
pub use fpkg_glua::{Output, Tree};
use nl::mlua::{self, Function, Table, Value};

use crate::install::ProjectPackage;
use crate::project::Project;
use crate::spec::Env;

/// The task file of a package.
pub const TASK_FILE: &str = "taskspec.lua";

const TASKS_LUA: &str = include_str!("tasks.lua");

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error(transparent)]
    Runtime(#[from] fpkg_glua::Error),
    /// A task file that does not compile or raises an error.
    #[error("{}", if message.starts_with(file.as_str()) { message.clone() } else { format!("{file}: {message}") })]
    Load {
        /// The package whose file it is.
        package: String,
        file: String,
        message: String,
        traceback: String,
    },
    #[error("unknown task '{0}'")]
    Unknown(String),
    /// A task raised an error.
    #[error("{message}")]
    Failed { message: String, traceback: String },
    /// A task returned, but uncaught Lua errors were reported while it ran.
    #[error("the task finished, but {0} Lua error{s} {were} reported", s = if *.0 == 1 { "" } else { "s" }, were = if *.0 == 1 { "was" } else { "were" })]
    Reported(usize),
    #[error("{0}")]
    Lua(String),
}

impl TaskError {
    /// The Lua stack under the error, as the game prints it (empty when there is none).
    pub fn traceback(&self) -> &str {
        match self {
            TaskError::Load { traceback, .. } | TaskError::Failed { traceback, .. } => traceback,
            _ => "",
        }
    }
}

fn lua_err(e: mlua::Error) -> TaskError {
    TaskError::Lua(e.to_string())
}

/// A task as the task files defined it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TaskInfo {
    pub name: String,
    pub description: Option<String>,
    pub prerequisites: Vec<String>,
    /// The package whose task file defined it first.
    pub package: String,
    /// How many actions it has.
    pub actions: usize,
}

/// The game's paths for a project: the server's `garrysmod/` when the project is in a
/// server tree (or `--server` names one), else `<project>/.fpkg/garrysmod/`. A package of
/// `packages` whose `LUA` path is outside the gamemode (a path package, `../shared/x`) is
/// mounted where that path points, unless the project is inside a server, whose own
/// `gamemodes/` the game reads.
pub fn tree_for(project: &Project, packages: &[ProjectPackage]) -> Tree {
    let (mut tree, inside) = match &project.server {
        Some(server) => (
            Tree::server(server, &project.id, &project.given_root),
            project.given_root == server.join("garrysmod").join("gamemodes").join(&project.id),
        ),
        None => (Tree::standalone(&project.given_root, &project.id), false),
    };
    if !inside {
        for p in packages {
            let Some(parts) = fpkg_glua::tree::normalize(&p.lua_path) else {
                continue;
            };
            if parts.first() != Some(&project.id) {
                tree.mount(&format!("gamemodes/{}", parts.join("/")), &p.dir);
            }
        }
    }
    tree
}

/// The task files of the packages, in their order: (package, file).
pub fn task_files(packages: &[ProjectPackage]) -> Vec<(&ProjectPackage, PathBuf)> {
    packages
        .iter()
        .map(|p| (p, p.dir.join(TASK_FILE)))
        .filter(|(_, f)| f.is_file())
        .collect()
}

/// The syntax error of the task file in `dir`, if it has one that does not compile; `shown`
/// names the file in the message.
pub fn check_task_file(dir: &Path, shown: &str) -> Option<String> {
    let file = dir.join(TASK_FILE);
    let bytes = std::fs::read(&file).ok()?;
    fpkg_glua::check_syntax(&bytes, shown)
        .err()
        .map(|e| format!("{TASK_FILE} does not compile: {e}"))
}

/// The `KEY=VALUE` words (keys upper-cased) and the other words, in order. A key is
/// letters, digits and underscores.
pub fn parse_words(words: &[String]) -> (BTreeMap<String, String>, Vec<String>) {
    let mut options = BTreeMap::new();
    let mut rest = Vec::new();
    for w in words {
        match w.split_once('=') {
            Some((k, v))
                if !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
            {
                options.insert(k.to_ascii_uppercase(), v.to_string());
            }
            _ => rest.push(w.clone()),
        }
    }
    (options, rest)
}

/// What the task files need to know about the project.
pub struct Context<'a> {
    pub project: &'a Project,
    /// The packages in load order (`install::load_order`).
    pub packages: &'a [ProjectPackage],
    /// The `--env` value, when the flag was given.
    pub env: Option<Env>,
    pub tree: Tree,
    /// Where the console goes while the files load, and after until `set_output`.
    pub output: Output,
    /// Whether `MsgC` writes colors.
    pub color: bool,
}

/// The tasks of a project, loaded into a runtime.
pub struct TaskSet {
    runtime: Runtime,
    host: Table,
    tasks: Vec<TaskInfo>,
}

impl TaskSet {
    /// Starts a runtime, defines the vocabulary and `fpkg`, and runs every task file.
    pub fn load(cx: Context) -> Result<TaskSet, TaskError> {
        let runtime = Runtime::new(Config {
            tree: cx.tree.clone(),
            output: cx.output.clone(),
            color: cx.color,
        })?;
        let lua = runtime.lua();
        let host: Table = lua
            .load(TASKS_LUA)
            .set_name("=fpkg/tasks.lua")
            .call(lua.current_thread())
            .map_err(lua_err)?;
        install_fpkg(&runtime, &cx).map_err(lua_err)?;
        let begin: Function = host.get("begin_file").map_err(lua_err)?;
        let end: Function = host.get("end_file").map_err(lua_err)?;
        let g = lua.globals();
        for (pkg, file) in task_files(cx.packages) {
            let lua_file = format!("{}{TASK_FILE}", pkg.lua_path);
            let chunk_name = format!("gamemodes/{lua_file}");
            let spec = lua.create_table().map_err(lua_err)?;
            spec.set("id", pkg.id.as_str()).map_err(lua_err)?;
            spec.set("path", pkg.lua_path.as_str()).map_err(lua_err)?;
            g.set("TASKSPEC", spec).map_err(lua_err)?;
            begin.call::<()>(pkg.id.as_str()).map_err(lua_err)?;
            let r = load_file(&runtime, &file, &chunk_name)
                .and_then(|f| runtime.call_in(&lua_file, &f, ()).map(|_| ()));
            end.call::<()>(()).map_err(lua_err)?;
            if let Err(e) = r {
                return Err(TaskError::Load {
                    package: pkg.id.clone(),
                    file: chunk_name,
                    message: e.message.clone(),
                    traceback: e.traceback(),
                });
            }
        }
        g.set("TASKSPEC", Value::Nil).map_err(lua_err)?;
        let tasks = read_tasks(&host).map_err(lua_err)?;
        Ok(TaskSet {
            runtime,
            host,
            tasks,
        })
    }

    /// The tasks in the order they were first defined.
    pub fn tasks(&self) -> &[TaskInfo] {
        &self.tasks
    }

    pub fn get(&self, name: &str) -> Option<&TaskInfo> {
        self.tasks.iter().find(|t| t.name == name)
    }

    pub fn runtime(&self) -> &Runtime {
        &self.runtime
    }

    /// Sends the console elsewhere from now on.
    pub fn set_output(&self, output: Output) {
        self.runtime.set_output(output);
    }

    /// The prerequisites that name no task: (task, prerequisite), in the tasks' order.
    pub fn missing_prerequisites(&self) -> Vec<(&TaskInfo, &str)> {
        self.tasks
            .iter()
            .flat_map(|t| {
                t.prerequisites
                    .iter()
                    .filter(|p| self.get(p).is_none())
                    .map(move |p| (t, p.as_str()))
            })
            .collect()
    }

    /// Runs a task with the words after its name, then delivers what the loop has due.
    pub fn run(&self, name: &str, words: &[String]) -> Result<(), TaskError> {
        if self.get(name).is_none() {
            return Err(TaskError::Unknown(name.to_string()));
        }
        let lua = self.runtime.lua();
        let (options, words) = parse_words(words);
        let options = lua.create_table_from(options).map_err(lua_err)?;
        let words = lua.create_sequence_from(words).map_err(lua_err)?;
        let invoke: Function = self.host.get("invoke").map_err(lua_err)?;
        let reported = self.runtime.reported_errors();
        let r = self.runtime.call(&invoke, (name, options, words));
        self.runtime.settle();
        self.runtime.flush();
        r.map_err(|e: Failure| TaskError::Failed {
            traceback: e.traceback(),
            message: e.message,
        })?;
        match self.runtime.reported_errors() - reported {
            0 => Ok(()),
            n => Err(TaskError::Reported(n)),
        }
    }
}

fn load_file(runtime: &Runtime, file: &Path, chunk_name: &str) -> Result<Function, Failure> {
    let bytes = std::fs::read(file).map_err(|e| Failure {
        message: format!("cannot read {}: {e}", file.display()),
        stack: Vec::new(),
    })?;
    runtime.load(&bytes, chunk_name)
}

fn read_tasks(host: &Table) -> mlua::Result<Vec<TaskInfo>> {
    let list: Function = host.get("list")?;
    let t: Table = list.call(())?;
    let mut out = Vec::new();
    for entry in t.sequence_values::<Table>() {
        let entry = entry?;
        let prerequisites: Table = entry.get("prerequisites")?;
        out.push(TaskInfo {
            name: entry.get("name")?,
            description: entry.get("description")?,
            prerequisites: prerequisites
                .sequence_values::<String>()
                .collect::<mlua::Result<_>>()?,
            package: entry.get::<Option<String>>("package")?.unwrap_or_default(),
            actions: entry.get("actions")?,
        });
    }
    Ok(out)
}

/// The global `fpkg`: `version`, `project`, `packages`, `env`, `server`, `wait_until` and
/// `exit`.
fn install_fpkg(runtime: &Runtime, cx: &Context) -> mlua::Result<()> {
    let lua = runtime.lua();
    let t = lua.create_table()?;
    t.set("version", crate::FPKG_VERSION)?;
    let project = lua.create_table()?;
    project.set("id", cx.project.id.as_str())?;
    project.set("gamemode", cx.project.gamemode)?;
    project.set("path", format!("{}/", cx.project.id))?;
    t.set("project", project)?;
    let packages = lua.create_table()?;
    for (i, p) in cx.packages.iter().enumerate() {
        let e = lua.create_table()?;
        e.set("id", p.id.as_str())?;
        e.set("path", p.lua_path.as_str())?;
        e.set("version", p.manifest.version.to_string())?;
        packages.set(i + 1, e)?;
    }
    t.set("packages", packages)?;
    if let Some(env) = cx.env {
        t.set("env", env.as_str())?;
    }
    t.set("server", cx.project.server.is_some())?;
    t.set("wait_until", runtime.wait_until_function()?)?;
    t.set(
        "exit",
        runtime.create_function(|_, code: Value| -> mlua::Result<()> {
            let code = exit_code(&code)
                .ok_or_else(|| mlua::Error::RuntimeError("fpkg.exit: code must be 0-255".into()))?;
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let _ = std::io::stderr().flush();
            std::process::exit(code)
        })?,
    )?;
    lua.globals().set("fpkg", t)
}

/// The exit code `fpkg.exit` takes: nothing (0) or a whole number from 0 to 255.
fn exit_code(v: &Value) -> Option<i32> {
    let n = match v {
        Value::Nil => return Some(0),
        Value::Integer(i) => *i as f64,
        Value::Number(n) => *n,
        _ => return None,
    };
    (n == n.trunc() && (0.0..=255.0).contains(&n)).then_some(n as i32)
}

#[cfg(test)]
mod tests;

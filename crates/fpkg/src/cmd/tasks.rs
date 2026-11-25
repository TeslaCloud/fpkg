//! `fpkg tasks`, `fpkg run <task>` and `fpkg <task>`: the tasks of the project's
//! `taskspec.lua` files, run in fpkg's Lua runtime.

use anyhow::Result;
use clap::CommandFactory;
use clap::builder::{Resettable, ValueParser};
use clap::error::{ContextKind, ContextValue};
use fpkg_core::install::{self, InstallError, LoadOrder, Loaded};
use fpkg_core::lockfile::Lockfile;
use fpkg_core::project::{Project, ProjectError};
use fpkg_core::spec::Severity;
use fpkg_core::tasks::{Context, Output, TaskError, TaskSet, tree_for};
use serde_json::json;

use super::{Ctx, exit, exit_silent};
use crate::output::table;
use crate::{Cli, EXIT_FAILURE, EXIT_HUMAN};

/// The stack of the thread tasks run on: Lua that recurses through Rust can go deep before
/// Lua's own limits stop it.
const STACK: usize = 64 << 20;

/// Runs `f` with `ctx` on a thread with a large stack.
pub fn on_large_stack(ctx: Ctx, f: impl FnOnce(&Ctx) -> Result<()> + Send + 'static) -> Result<()> {
    std::thread::Builder::new()
        .name("tasks".into())
        .stack_size(STACK)
        .spawn(move || f(&ctx))?
        .join()
        .unwrap_or_else(|p| std::panic::resume_unwind(p))
}

/// What the task files are loaded from: the project and its packages in Flux's order.
struct Prepared {
    project: Project,
    loaded: Loaded,
    order: LoadOrder,
}

fn prepare(ctx: &Ctx) -> Result<Prepared> {
    let project = ctx.project()?;
    let lock = Lockfile::read(&project.lock_path())?;
    let loaded = install::load_used(&project, ctx.env, lock.as_ref())?;
    let order = install::load_order(&loaded, ctx.env)?;
    Ok(Prepared {
        project,
        loaded,
        order,
    })
}

/// The warnings about packages the server would not find, and under `-v` the load order.
fn warn(ctx: &Ctx, p: &Prepared) {
    for (id, by) in &p.order.missing {
        if p.loaded.conflicts.contains(id) {
            ctx.out.warn(&format!(
                "{}; until then the tasks of {id}, a dependency of {by}, are missing",
                InstallError::Conflict(vec![id.clone()])
            ));
        } else {
            ctx.out.warn(&format!(
                "{id}, a dependency of {by}, is not in packages/: its tasks are missing (run fpkg install)"
            ));
        }
    }
    for (id, by, outer) in &p.order.too_early {
        ctx.out.warn(&format!(
            "{id}, a dependency of {by}, is in packages/{outer}, which the server has not included at that point, so it does not find {id}: its tasks are missing (depend on {outer} before {id})"
        ));
    }
    for pkg in &p.order.packages {
        ctx.out
            .verbose(&format!("package {} at {}", pkg.id, pkg.dir.display()));
    }
}

/// Loads the project's task files, with their console going to `output`.
fn load(ctx: &Ctx, p: &Prepared, output: Output) -> Result<TaskSet, TaskError> {
    TaskSet::load(Context {
        project: &p.project,
        packages: &p.order.packages,
        env: ctx.env_flag,
        tree: tree_for(&p.project, &p.order.packages),
        output,
        color: ctx.out.color_stdout,
    })
}

/// The console of a task: standard output and standard error.
fn stdio(ctx: &Ctx) -> Output {
    Output::Stdio {
        color_out: ctx.out.color_stdout,
        color_err: ctx.out.color,
    }
}

/// A task error as an exit: 2 for an unknown task, 1 for any other. The message of an error
/// with a Lua stack is followed by the stack under `-v`; without `-v`, an error that looks
/// unexpected (its message starts with the `file:line:` where it was raised) is followed by
/// how to see the stack: `again` is what to run with `-v` (`tasks`, or the task).
fn task_error(ctx: &Ctx, e: TaskError, again: &str) -> anyhow::Error {
    let code = match e {
        TaskError::Unknown(_) => EXIT_HUMAN,
        _ => EXIT_FAILURE,
    };
    let tb = e.traceback();
    if tb.is_empty() {
        return exit(code, e.to_string());
    }
    let message = e.to_string();
    ctx.out.error(&message);
    if ctx.out.verbose {
        eprint!("{tb}");
    } else if has_position(&message) {
        let again = if again != "tasks" && command_names().iter().any(|c| c == again) {
            format!("run {again}")
        } else {
            again.to_string()
        };
        ctx.out
            .note(&format!("run fpkg -v {again} for the Lua stack"));
    }
    exit_silent(code)
}

/// Whether a Lua error message starts with the `file:line:` of where it was raised.
fn has_position(message: &str) -> bool {
    let Some((head, _)) = message.split_once(": ") else {
        return false;
    };
    let Some((file, line)) = head.rsplit_once(':') else {
        return false;
    };
    !file.is_empty() && !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit())
}

/// The names of fpkg's own commands.
fn command_names() -> Vec<String> {
    Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect()
}

/// The warnings about prerequisites that name no task.
fn warn_missing_prerequisites(ctx: &Ctx, set: &TaskSet) {
    for (t, p) in set.missing_prerequisites() {
        ctx.out.warn(&format!(
            "task '{}' needs '{p}', which no task file defines",
            t.name
        ));
    }
}

/// What `fpkg check` reports about the task files of the whole project, by package id: a
/// file that fails to load is an error, a prerequisite that names no task a warning. Nothing
/// when the packages cannot be put in order (`check` reports why) or no file fails to
/// compile is left to load.
pub fn check_diagnostics(ctx: &Ctx) -> Vec<(String, Severity, String)> {
    let Ok(prepared) = prepare(ctx) else {
        return Vec::new();
    };
    let files = fpkg_core::tasks::task_files(&prepared.order.packages);
    if files.is_empty() {
        return Vec::new();
    }
    let (held, _) = Output::capture();
    match load(ctx, &prepared, held) {
        Ok(set) => set
            .missing_prerequisites()
            .into_iter()
            .map(|(t, p)| {
                (
                    t.package.clone(),
                    Severity::Warning,
                    format!("task '{}' needs '{p}', which no task file defines", t.name),
                )
            })
            .collect(),
        Err(e) => {
            let package = match &e {
                TaskError::Load { package, .. } => package.clone(),
                _ => prepared.project.id.clone(),
            };
            vec![(
                package,
                Severity::Error,
                format!("its task files do not load: {e}"),
            )]
        }
    }
}

/// `fpkg tasks [--all] [PATTERN]`. What the task files print while they load goes to
/// standard error, so that standard output is only the listing.
pub fn list(ctx: &Ctx, all: bool, pattern: Option<&str>) -> Result<()> {
    let prepared = prepare(ctx)?;
    warn(ctx, &prepared);
    let set = load(
        ctx,
        &prepared,
        Output::Stderr {
            color: ctx.out.color,
        },
    )
    .map_err(|e| task_error(ctx, e, "tasks"))?;
    warn_missing_prerequisites(ctx, &set);
    let exact = pattern.and_then(|p| set.get(p));
    let matching: Vec<_> = set
        .tasks()
        .iter()
        .filter(|t| pattern.is_none_or(|p| t.name.contains(p)))
        .collect();
    let mut tasks: Vec<_> = matching
        .iter()
        .copied()
        .filter(|t| all || t.description.is_some() || exact.is_some_and(|e| e.name == t.name))
        .collect();
    tasks.sort_by(|a, b| a.name.cmp(&b.name));
    if ctx.out.json {
        ctx.emit_json(json!({ "tasks": tasks }));
        return Ok(());
    }
    if tasks.is_empty() {
        let hidden = matching.iter().filter(|t| t.description.is_none()).count();
        let note = if set.tasks().is_empty() {
            "no tasks: no package of the project has a taskspec.lua".to_string()
        } else if hidden > 0 && !all {
            format!(
                "no tasks with a description match; {hidden} without one {} listed by fpkg tasks --all",
                if hidden == 1 { "is" } else { "are" }
            )
        } else {
            "no tasks match".to_string()
        };
        ctx.out.note(&note);
        return Ok(());
    }
    let rows: Vec<Vec<String>> = tasks
        .iter()
        .map(|t| vec![t.name.clone(), t.description.clone().unwrap_or_default()])
        .collect();
    let width = tasks
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0);
    for (line, t) in table(&rows).lines().zip(&tasks) {
        println!("{line}");
        if exact.is_some_and(|e| e.name == t.name) {
            let needs = if t.prerequisites.is_empty() {
                "needs no other task".to_string()
            } else {
                format!("needs: {}", t.prerequisites.join(", "))
            };
            println!("{:width$}  {needs}", "");
        }
    }
    Ok(())
}

/// The nearest task name to `word`.
fn suggestion<'a>(word: &str, tasks: &'a [String]) -> Option<&'a str> {
    fpkg_core::nearest_id(word, tasks.iter().map(String::as_str))
}

/// clap's own error for the command line, whose first word is not one of fpkg's commands,
/// with its suggestion among the commands, the nearest task added, then `note`.
fn unrecognized(word: &str, tasks: &[String], note: Option<String>) -> anyhow::Error {
    let parsed = Cli::command()
        .allow_external_subcommands(false)
        .external_subcommand_value_parser(Resettable::<ValueParser>::Reset)
        .try_get_matches_from(std::env::args_os());
    let mut e = match parsed {
        Err(e) => e,
        Ok(_) => Cli::command().error(
            clap::error::ErrorKind::InvalidSubcommand,
            format!("unrecognized subcommand '{word}'"),
        ),
    };
    if let Some(task) = suggestion(word, tasks) {
        let mut similar = match e.get(ContextKind::SuggestedSubcommand) {
            Some(ContextValue::String(s)) => vec![s.clone()],
            Some(ContextValue::Strings(v)) => v.clone(),
            _ => Vec::new(),
        };
        if !similar.iter().any(|s| s == task) {
            similar.push(task.to_string());
        }
        e.insert(
            ContextKind::SuggestedSubcommand,
            ContextValue::Strings(similar),
        );
    }
    let _ = e.print();
    if let Some(note) = note {
        eprintln!("note: {note}");
    }
    exit_silent(EXIT_HUMAN)
}

/// Runs a loaded task, with its errors as exits.
fn run_task(ctx: &Ctx, set: &TaskSet, task: &str, words: &[String]) -> Result<()> {
    ctx.out.verbose(&format!("running {task}"));
    set.run(task, words).map_err(|e| task_error(ctx, e, task))
}

/// `fpkg run <task> [words...]`: `words` holds the task and the words after it.
pub fn run(ctx: &Ctx, words: &[String]) -> Result<()> {
    let Some((task, words)) = words.split_first() else {
        return Err(exit(EXIT_HUMAN, "no task given"));
    };
    let prepared = prepare(ctx)?;
    warn(ctx, &prepared);
    let set = load(ctx, &prepared, stdio(ctx)).map_err(|e| task_error(ctx, e, task))?;
    if set.get(task).is_none() {
        let names: Vec<String> = set.tasks().iter().map(|t| t.name.clone()).collect();
        let mut message = format!("unknown task '{task}'");
        match suggestion(task, &names) {
            Some(s) => message += &format!("; did you mean '{s}'?"),
            None if names.is_empty() => message += ": no package of the project has a taskspec.lua",
            None => message += " (fpkg tasks --all lists them)",
        }
        return Err(exit(EXIT_HUMAN, message));
    }
    run_task(ctx, &set, task, words)
}

/// The first line of an error.
fn first_line(e: &dyn std::fmt::Display) -> String {
    e.to_string().lines().next().unwrap_or_default().to_string()
}

/// `fpkg <word> [words...]`: a word that is not one of fpkg's commands is run as a task when
/// the project has one of that name. Trying costs nothing visible: the task files load with
/// their console held back and without warnings, and when they do not load or have no such
/// task, the result is clap's usage error for an unknown command.
pub fn shorthand(ctx: &Ctx, words: &[String]) -> Result<()> {
    let Some((task, words)) = words.split_first() else {
        return Err(exit(EXIT_HUMAN, "no command given"));
    };
    let prepared = match prepare(ctx) {
        Ok(p) => p,
        Err(e) => {
            let outside = e.chain().any(|c| {
                c.downcast_ref::<ProjectError>()
                    .is_some_and(|p| matches!(p, ProjectError::NotFound(_)))
            });
            let note = if outside {
                "tasks need a project, and there is none here (fpkg --project <dir> names one)"
                    .to_string()
            } else {
                format!(
                    "the project's task files could not be loaded: {}; see fpkg tasks",
                    first_line(&format!("{e:#}"))
                )
            };
            return Err(unrecognized(task, &[], Some(note)));
        }
    };
    let (held, console) = Output::capture_with(true);
    let set = match load(ctx, &prepared, held) {
        Ok(set) => set,
        Err(e) => {
            let note = format!(
                "the project's task files could not be loaded: {}; see fpkg tasks",
                first_line(&e)
            );
            return Err(unrecognized(task, &[], Some(note)));
        }
    };
    if set.get(task).is_none() {
        let names: Vec<String> = set.tasks().iter().map(|t| t.name.clone()).collect();
        return Err(unrecognized(task, &names, None));
    }
    warn(ctx, &prepared);
    console.borrow().replay(&stdio(ctx));
    set.set_output(stdio(ctx));
    run_task(ctx, &set, task, words)
}

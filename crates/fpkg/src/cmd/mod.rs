//! Command implementations and what they share.

mod deps;
mod link;
mod local;
mod new;
mod remote;

use std::cell::RefCell;
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use fpkg_core::config::{Config, Flags};
use fpkg_core::install::InstallError;
use fpkg_core::project::{self, Project, ProjectError};
use fpkg_core::registry::{Registries, Token};
use fpkg_core::spec::Env;

use crate::output::Out;
use crate::{Cli, Cmd, EXIT_FAILURE, EXIT_HUMAN, EXIT_STALE};

/// An error with a specific exit code. `message` is printed as the error;
/// `None` means the command already said what went wrong.
#[derive(Debug)]
pub struct Exit {
    pub code: u8,
    pub message: Option<String>,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message.as_deref().unwrap_or("failed"))
    }
}

impl std::error::Error for Exit {}

pub fn exit(code: u8, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Exit {
        code,
        message: Some(message.into()),
    })
}

pub fn exit_silent(code: u8) -> anyhow::Error {
    anyhow::Error::new(Exit {
        code,
        message: None,
    })
}

pub fn exit_code(e: &anyhow::Error) -> u8 {
    if let Some(x) = e.downcast_ref::<Exit>() {
        return x.code;
    }
    for cause in e.chain() {
        if let Some(ie) = cause.downcast_ref::<InstallError>() {
            return match ie {
                InstallError::Stale(_) | InstallError::NoLock => EXIT_STALE,
                InstallError::Conflict(_)
                | InstallError::FileConflict(_)
                | InstallError::SymlinkedDir { .. } => EXIT_HUMAN,
                _ => EXIT_FAILURE,
            };
        }
        // Where to run a command is a usage question.
        if let Some(
            ProjectError::NotFound(_) | ProjectError::NotAProject(_) | ProjectError::NotAServer(_),
        ) = cause.downcast_ref::<ProjectError>()
        {
            return EXIT_HUMAN;
        }
    }
    EXIT_FAILURE
}

/// Prints a command's error as `main` does.
pub fn print_error(out: &Out, e: &anyhow::Error) {
    if let Some(exit) = e.downcast_ref::<Exit>() {
        if let Some(m) = &exit.message {
            out.error(m);
        }
    } else {
        out.error(&format!("{e:#}"));
    }
}

/// What every command gets.
pub struct Ctx {
    pub out: Out,
    pub env: Env,
    pub project_flag: Option<PathBuf>,
    pub server_flag: Option<PathBuf>,
    pub registry_flag: Option<String>,
    pub offline: bool,
    /// While several projects are processed, the JSON each one emits.
    json_sink: RefCell<Option<Vec<serde_json::Value>>>,
}

/// The projects a command runs on.
enum Targets {
    One(Project),
    /// The gamemode projects of a server tree.
    Server(PathBuf, Vec<Project>),
}

impl Ctx {
    /// The project: `--project <dir>` or discovered from the current directory,
    /// with `--server` as its server folder.
    pub fn project(&self) -> Result<Project> {
        let mut p = match &self.project_flag {
            Some(dir) => Project::at(existing("--project", dir)?)?,
            None => Project::discover(&cwd()?)?,
        };
        if let Some(dir) = &self.server_flag {
            p.server = Some(project::server_root(existing("--server", dir)?)?);
        }
        Ok(p)
    }

    /// One project as `project` finds it, or, when there is none from the
    /// current directory, every gamemode project of the server tree that
    /// `--server` names or that is found around the current directory.
    fn targets(&self) -> Result<Targets> {
        if self.project_flag.is_some() {
            return Ok(Targets::One(self.project()?));
        }
        let cwd = cwd()?;
        match self.project() {
            Ok(p) => Ok(Targets::One(p)),
            Err(e)
                if e.downcast_ref::<ProjectError>()
                    .is_some_and(|pe| matches!(pe, ProjectError::NotFound(_))) =>
            {
                let server = match &self.server_flag {
                    Some(dir) => Some(project::server_root(existing("--server", dir)?)?),
                    None => project::find_server_tree(&cwd),
                };
                match server {
                    Some(s) => {
                        let projects = project::server_projects(&s)?;
                        Ok(Targets::Server(s, projects))
                    }
                    None => Err(exit(
                        EXIT_HUMAN,
                        format!(
                            "{e}. For the gamemodes of a server, run it in the server folder (the one with garrysmod/gamemodes/), or pass --server <dir>"
                        ),
                    )),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Runs `f` on the project, or on each project of a server tree under a
    /// heading (on stdout when `heading_on_stdout`, as for tables). One failing
    /// project does not stop the others; the exit code is the highest one.
    pub fn for_each_project(
        &self,
        heading_on_stdout: bool,
        f: &mut dyn FnMut(&Project) -> Result<()>,
    ) -> Result<()> {
        let (server, projects) = match self.targets()? {
            Targets::One(p) => return f(&p),
            Targets::Server(s, ps) => (s, ps),
        };
        if projects.is_empty() {
            return Err(anyhow!(
                "{} has no projects in garrysmod/gamemodes/ (a gamemode folder with a packagespec.lua)",
                server.display()
            ));
        }
        let mut worst = 0u8;
        let mut entries = Vec::new();
        for p in &projects {
            let label = p
                .given_root
                .strip_prefix(&server)
                .unwrap_or(&p.given_root)
                .display()
                .to_string();
            if !self.out.json {
                let heading = format!("==> {} ({label})", p.id);
                if heading_on_stdout {
                    println!("{heading}");
                } else {
                    self.out.note(&heading);
                }
            }
            *self.json_sink.borrow_mut() = Some(Vec::new());
            let r = f(p);
            let values = self.json_sink.borrow_mut().take().unwrap_or_default();
            let (code, error) = match &r {
                Ok(()) => (0, None),
                Err(e) => {
                    print_error(&self.out, e);
                    (
                        exit_code(e),
                        e.downcast_ref::<Exit>()
                            .map_or_else(|| Some(format!("{e:#}")), |x| x.message.clone()),
                    )
                }
            };
            worst = worst.max(code);
            entries.push(serde_json::json!({
                "project": p.given_root, "id": p.id, "exit_code": code, "error": error,
                "result": values.into_iter().next(),
            }));
            if heading_on_stdout && !self.out.json {
                println!();
            }
        }
        if self.out.json {
            self.out
                .json(&serde_json::json!({ "server": server, "projects": entries }));
        }
        if worst != 0 {
            Err(exit_silent(worst))
        } else {
            Ok(())
        }
    }

    /// Prints a JSON result, or keeps it for `for_each_project`.
    pub fn emit_json(&self, v: serde_json::Value) {
        match self.json_sink.borrow_mut().as_mut() {
            Some(sink) => sink.push(v),
            None => self.out.json(&v),
        }
    }

    pub fn config(&self, project: Option<&Project>) -> Result<Config> {
        let flags = Flags {
            registry: self.registry_flag.clone(),
            offline: self.offline.then_some(true),
        };
        Ok(Config::load(project.map(|p| p.root.as_path()), &flags)?)
    }

    pub fn registries(&self, config: &Config) -> Result<Registries> {
        Ok(Registries::new(
            &config.registry,
            config.cache_dir.clone(),
            config.net(),
            config.offline,
        )?)
    }

    /// The token for `url`: none for `file://`, else `FPKG_TOKEN` or the
    /// credentials file, refused over plain HTTP to anything but loopback.
    pub fn token(&self, config: &Config, url: &str) -> Result<Token> {
        if url.starts_with("file:") {
            return Ok(Token::none());
        }
        fpkg_core::auth::check_transport(url)?;
        let creds = fpkg_core::auth::Credentials::load(&config.credentials_path())?;
        fpkg_core::auth::token_for(url, &creds, config.env_token_allowed(url)).ok_or_else(|| {
            anyhow!("no token for {url}: run `fpkg login --registry {url}`, or set FPKG_TOKEN (sent only to a registry named by --registry, FPKG_REGISTRY or your user config)")
        })
    }

    /// Asks a yes/no question on the terminal. Without a terminal it refuses,
    /// so that CI must say `--yes`.
    pub fn confirm(&self, question: &str) -> Result<bool> {
        if !std::io::stdin().is_terminal() {
            return Err(exit(
                EXIT_HUMAN,
                format!("{question}: no terminal to ask on; pass --yes"),
            ));
        }
        eprint!("{question} [y/N] ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(matches!(
            line.trim().to_ascii_lowercase().as_str(),
            "y" | "yes"
        ))
    }
}

/// The directory a flag names, which has to exist.
fn existing<'a>(flag: &str, dir: &'a std::path::Path) -> Result<&'a std::path::Path> {
    if dir.is_dir() {
        Ok(dir)
    } else if dir.exists() {
        Err(exit(
            EXIT_HUMAN,
            format!("{flag} {}: not a directory", dir.display()),
        ))
    } else {
        Err(exit(
            EXIT_HUMAN,
            format!("{flag} {}: the directory does not exist", dir.display()),
        ))
    }
}

/// The current directory as the shell reached it.
pub fn cwd() -> Result<PathBuf> {
    project::logical_cwd()
        .or_else(|| std::env::current_dir().ok())
        .context("cannot read the current directory")
}

/// A package id given on the command line; an invalid one is a usage error.
pub fn usage_id(id: &str) -> Result<fpkg_core::PackageId> {
    fpkg_core::PackageId::new(id).map_err(|e| exit(EXIT_HUMAN, e.to_string()))
}

/// `id@rest` -> (`id`, Some(`rest`)).
pub fn split_at(spec: &str) -> (&str, Option<&str>) {
    match spec.split_once('@') {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (spec.trim(), None),
    }
}

/// Opens a URL in the browser; failure is only a warning.
pub fn open_url(out: &Out, url: &str) {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    match std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => {}
        _ => out.warn(&format!("could not open {url} with {program}")),
    }
}

pub fn run(cli: Cli, out: Out) -> Result<()> {
    let ctx = Ctx {
        out,
        env: cli.env,
        project_flag: cli.project,
        server_flag: cli.server,
        registry_flag: cli.registry,
        offline: cli.offline,
        json_sink: RefCell::new(None),
    };
    match cli.cmd {
        Cmd::Init { dir } => local::init(&ctx, dir),
        Cmd::New {
            kind,
            name,
            minimal,
            title,
            flux,
            skip_docker,
        } => new::new(
            &ctx,
            new::NewOpts {
                kind,
                name,
                minimal,
                title,
                flux,
                skip_docker,
            },
        ),
        Cmd::Link { dir, as_id, force } => match dir {
            Some(dir) => link::link(&ctx, &dir, as_id, force),
            None if as_id.is_some() || force => Err(exit(
                EXIT_HUMAN,
                "give the directory to link: fpkg link <dir> [--as <id>]",
            )),
            None => link::list(&ctx),
        },
        Cmd::Unlink { id } => link::unlink(&ctx, &id),
        Cmd::List => local::list(&ctx),
        Cmd::Tree { id } => local::tree(&ctx, id),
        Cmd::Info { spec } => local::info(&ctx, &spec),
        Cmd::Check { ids, all } => local::check(&ctx, &ids, all),
        Cmd::Pack { id, out, list } => local::pack(&ctx, id, out, list),
        Cmd::Lock => deps::lock(&ctx),
        Cmd::Update { ids } => deps::update(&ctx, &ids),
        Cmd::Add { spec } => deps::add(&ctx, &spec),
        Cmd::Remove { id } => deps::remove(&ctx, &id),
        Cmd::Install { locked, force } => deps::install(&ctx, locked, force),
        Cmd::Verify => deps::verify(&ctx),
        Cmd::Search { query, page } => remote::search(&ctx, &query, page),
        Cmd::Publish {
            id,
            dry_run,
            all,
            yes,
            allow_missing_dependencies,
        } => remote::publish(
            &ctx,
            id,
            remote::PublishOpts {
                dry_run,
                all,
                yes,
                allow_missing: allow_missing_dependencies,
            },
        ),
        Cmd::Yank { spec, undo } => remote::yank(&ctx, &spec, undo),
        Cmd::Owner { action } => remote::owner(&ctx, action),
        Cmd::Login => remote::login(&ctx),
        Cmd::Logout => remote::logout(&ctx),
        Cmd::Docs { spec } => remote::docs(&ctx, &spec),
        Cmd::Cache { action } => remote::cache(&ctx, action),
        Cmd::Mirror { registry_url, dir } => remote::mirror(&ctx, &registry_url, &dir),
        Cmd::Completions { shell } => {
            let mut command = <Cli as clap::CommandFactory>::command();
            let mut script = Vec::new();
            clap_complete::generate(shell, &mut command, "fpkg", &mut script);
            std::io::stdout().write_all(&script)?;
            Ok(())
        }
    }
}

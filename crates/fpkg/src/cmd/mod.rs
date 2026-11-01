//! Command implementations and what they share.

mod deps;
mod local;
mod remote;

use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use fpkg_core::config::{Config, Flags};
use fpkg_core::install::InstallError;
use fpkg_core::project::Project;
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
    anyhow::Error::new(Exit { code, message: Some(message.into()) })
}

pub fn exit_silent(code: u8) -> anyhow::Error {
    anyhow::Error::new(Exit { code, message: None })
}

pub fn exit_code(e: &anyhow::Error) -> u8 {
    if let Some(x) = e.downcast_ref::<Exit>() {
        return x.code;
    }
    for cause in e.chain() {
        if let Some(ie) = cause.downcast_ref::<InstallError>() {
            return match ie {
                InstallError::Stale(_) | InstallError::NoLock => EXIT_STALE,
                InstallError::Conflict(_) => EXIT_HUMAN,
                _ => EXIT_FAILURE,
            };
        }
    }
    EXIT_FAILURE
}

/// What every command gets.
pub struct Ctx {
    pub out: Out,
    pub env: Env,
    pub project_flag: Option<Option<PathBuf>>,
    pub registry_flag: Option<String>,
    pub offline: bool,
}

impl Ctx {
    /// The project: `--project <dir>` or discovered from the current directory.
    pub fn project(&self) -> Result<Project> {
        match &self.project_flag {
            Some(Some(dir)) => Ok(Project::at(dir)?),
            Some(None) => Err(exit(EXIT_HUMAN, "--project needs a directory here (only `fpkg init --project` takes none)")),
            None => {
                let cwd = std::env::current_dir().context("cannot read the current directory")?;
                Ok(Project::discover(&cwd)?)
            }
        }
    }

    pub fn config(&self, project: Option<&Project>) -> Result<Config> {
        let flags = Flags { registry: self.registry_flag.clone(), offline: self.offline.then_some(true) };
        Ok(Config::load(project.map(|p| p.root.as_path()), &flags)?)
    }

    pub fn registries(&self, config: &Config) -> Result<Registries> {
        Ok(Registries::new(&config.registry, config.cache_dir.clone(), config.net(), config.offline)?)
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
            return Err(exit(EXIT_HUMAN, format!("{question}: no terminal to ask on; pass --yes")));
        }
        eprint!("{question} [y/N] ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        Ok(matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
    }
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
    let program = if cfg!(target_os = "macos") { "open" } else if cfg!(windows) { "explorer" } else { "xdg-open" };
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
    let ctx = Ctx { out, env: cli.env, project_flag: cli.project, registry_flag: cli.registry, offline: cli.offline };
    match cli.cmd {
        Cmd::Init { id } => local::init(&ctx, id),
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
        Cmd::Publish { id, dry_run, all, yes, allow_missing_dependencies } => {
            remote::publish(&ctx, id, remote::PublishOpts { dry_run, all, yes, allow_missing: allow_missing_dependencies })
        }
        Cmd::Yank { spec, undo } => remote::yank(&ctx, &spec, undo),
        Cmd::Owner { action } => remote::owner(&ctx, action),
        Cmd::Login => remote::login(&ctx),
        Cmd::Logout => remote::logout(&ctx),
        Cmd::Docs { spec } => remote::docs(&ctx, &spec),
        Cmd::Cache { action } => remote::cache(&ctx, action),
        Cmd::Mirror { registry_url, dir } => remote::mirror(&ctx, &registry_url, &dir),
    }
}

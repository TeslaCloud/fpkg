//! `fpkg`: the package manager for Flux packages.

mod cmd;
mod edit;
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use fpkg_core::spec::Env;

use output::{ColorChoice, Out};

/// Exit codes.
pub const EXIT_FAILURE: u8 = 1;
pub const EXIT_HUMAN: u8 = 2;
pub const EXIT_STALE: u8 = 3;
pub const EXIT_VERIFY: u8 = 4;

#[derive(Parser, Debug)]
#[command(
    name = "fpkg",
    version,
    about = "Package manager for Flux packages",
    propagate_version = true
)]
pub struct Cli {
    /// The project directory (default: found by walking up from the current
    /// directory). `fpkg init --project` without a value creates a project.
    #[arg(long, global = true, value_name = "DIR", num_args = 0..=1)]
    pub project: Option<Option<PathBuf>>,
    /// The Garry's Mod server folder (its root, or its garrysmod directory):
    /// where `install` copies binary modules, and, when no project is found
    /// from the current directory, the server whose gamemode projects
    /// `install`, `verify` and `list` process
    #[arg(long, global = true, value_name = "DIR")]
    pub server: Option<PathBuf>,
    /// The registry (default: [registry] default, https://fluxpkg.dev/)
    #[arg(long, global = true, value_name = "URL")]
    pub registry: Option<String>,
    /// What IS_DEVELOPMENT, IS_TEST and IS_PRODUCTION say in the spec sandbox
    #[arg(long, global = true, value_name = "ENV", default_value = "production", value_parser = parse_env)]
    pub env: Env,
    /// Use the cache only; fail with the ids it lacks
    #[arg(long, global = true)]
    pub offline: bool,
    /// Machine-readable output
    #[arg(long, global = true)]
    pub json: bool,
    /// Print only errors and results
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,
    /// Print more detail
    #[arg(short, long, global = true)]
    pub verbose: bool,
    /// Color in messages
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN")]
    pub color: ColorChoice,
    #[command(subcommand)]
    pub cmd: Cmd,
}

fn parse_env(s: &str) -> Result<Env, String> {
    s.parse()
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// With --project, turn a directory into a project (a root packagespec.lua
    /// and packages/); a schema with a gamemode <name>.txt is then a gamemode
    /// project. `fpkg new` scaffolds a schema, package, plugin or addon
    Init,
    /// Scaffold a schema (./<name>/), a package (packages/<name>/ of the
    /// project, or ./<name>/ outside one), a Flux plugin (plugins/ of the
    /// project, or ./ outside one) or a Garry's Mod addon (./<name>/)
    New {
        #[arg(value_enum)]
        kind: NewKind,
        name: String,
        /// Write only the files it needs to load
        #[arg(long)]
        minimal: bool,
        /// The display name (default: the name, capitalized)
        #[arg(long)]
        title: Option<String>,
        /// The constraint on flux, for a schema (default: ~> 0.9)
        #[arg(long, value_name = "CONSTRAINT")]
        flux: Option<String>,
        /// For a schema: leave out the Dockerfile, docker-compose.yml and
        /// .dockerignore, and the Docker section of the README
        #[arg(long)]
        skip_docker: bool,
    },
    /// Use a local checkout of a package: packages/<id> becomes a symlink to
    /// <dir>. Without <dir>, list the links
    Link {
        dir: Option<PathBuf>,
        /// The package id (default: the checkout's project id)
        #[arg(long = "as", value_name = "ID")]
        as_id: Option<String>,
        /// Replace an existing link, or a package installed from a registry
        #[arg(long)]
        force: bool,
    },
    /// Remove the link packages/<id>
    Unlink { id: String },
    /// Packages in the project: id, version, source, path
    List,
    /// Dependency tree with resolved versions
    Tree { id: Option<String> },
    /// Show a manifest (local, or the registry's with @version)
    Info {
        #[arg(value_name = "ID[@VERSION]")]
        spec: String,
    },
    /// Validate specs (default: the project manifest, workspace and path
    /// packages; --all adds installed registry packages)
    Check {
        ids: Vec<String>,
        /// Also check packages installed from a registry
        #[arg(long)]
        all: bool,
    },
    /// Build the archive and print its path and sha256
    Pack {
        id: Option<String>,
        /// Output directory (default: <project>/target/fpkg)
        #[arg(short = 'o', long = "out", value_name = "DIR")]
        out: Option<PathBuf>,
        /// Print the member list
        #[arg(long)]
        list: bool,
    },
    /// Resolve and write fpkg.lock, keeping locked versions
    Lock,
    /// Re-resolve the given ids (or all) to the newest matching versions
    Update { ids: Vec<String> },
    /// Add s.depends to the project manifest, lock, install
    Add {
        #[arg(value_name = "ID[@CONSTRAINT]")]
        spec: String,
    },
    /// Remove a dependency from the project manifest, lock, install
    Remove { id: String },
    /// Install the locked registry packages into packages/, then copy the
    /// packages' content/ into a gamemode project's content/ and their binary
    /// modules into the server's garrysmod/lua/bin/
    Install {
        /// Fail (exit 3) instead of rewriting a missing or stale lock
        #[arg(long)]
        locked: bool,
        /// Replace unmarked directories that the lock has as registry packages,
        /// and overwrite files in content/ or lua/bin/ that fpkg did not write
        #[arg(long)]
        force: bool,
    },
    /// Check that installed packages and the files install copied match
    Verify,
    /// Search the registry
    Search {
        query: String,
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// Check, pack and upload a package
    Publish {
        id: Option<String>,
        /// Do everything but upload
        #[arg(long)]
        dry_run: bool,
        /// Every workspace package whose version is not on the registry, in
        /// dependency order
        #[arg(long)]
        all: bool,
        /// Do not ask for confirmation
        #[arg(long, short = 'y')]
        yes: bool,
        /// Publish even if a dependency is not on the registry (bootstrapping)
        #[arg(long)]
        allow_missing_dependencies: bool,
    },
    /// Yank a version (or --undo a yank)
    Yank {
        #[arg(value_name = "ID@VERSION")]
        spec: String,
        #[arg(long)]
        undo: bool,
    },
    /// Manage a package's owners
    Owner {
        #[command(subcommand)]
        action: OwnerCmd,
    },
    /// Store a registry token
    Login,
    /// Delete the stored token
    Logout,
    /// Print and open the docs URL of a package
    Docs {
        #[arg(value_name = "ID[@VERSION]")]
        spec: String,
    },
    /// The download cache
    Cache {
        #[command(subcommand)]
        action: CacheCmd,
    },
    /// Copy the packages the lockfile needs from a registry into a file:// directory
    Mirror { registry_url: String, dir: PathBuf },
}

/// What `fpkg new` scaffolds.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum NewKind {
    /// A gamemode project that depends on flux
    Schema,
    /// A Flux package
    Package,
    /// A Flux plugin
    Plugin,
    /// A Garry's Mod addon that runs without Flux
    Addon,
}

#[derive(Subcommand, Debug)]
pub enum OwnerCmd {
    Add { id: String, login: String },
    Remove { id: String, login: String },
    List { id: String },
}

#[derive(Subcommand, Debug)]
pub enum CacheCmd {
    /// Delete the cache
    Clean,
    /// Print the cache directory
    Dir,
}

/// The hidden subcommand that evaluates one spec in a child process
/// (fpkg_core::spec's process evaluator). Handled before clap.
const EVAL_SPEC: &str = "__eval-spec";

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let _ = args.next();
    if args.next().is_some_and(|a| a == EVAL_SPEC) {
        let request = args.next().and_then(|a| a.into_string().ok());
        return ExitCode::from(fpkg_core::spec::child_main(request.as_deref()).clamp(0, 255) as u8);
    }
    if let Ok(exe) = std::env::current_exe() {
        fpkg_core::spec::set_evaluator(fpkg_core::spec::Evaluator::Process {
            program: exe,
            args: vec![EVAL_SPEC.into()],
        });
    }
    let cli = Cli::parse();
    let out = Out::new(cli.color, cli.quiet, cli.verbose, cli.json);
    match cmd::run(cli, out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let code = cmd::exit_code(&e);
            cmd::print_error(&out, &e);
            ExitCode::from(code)
        }
    }
}

//! `fpkg`: the package manager for Flux packages.

mod cmd;
mod edit;
mod output;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{CommandFactory, Parser, Subcommand};
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
    /// directory)
    #[arg(long, global = true, value_name = "DIR", help_heading = GLOBAL)]
    pub project: Option<PathBuf>,
    /// The Garry's Mod server folder (its root or its garrysmod/): where
    /// install copies binary modules; outside a project, the server whose
    /// gamemodes install, verify and list process
    #[arg(long, global = true, value_name = "DIR", help_heading = GLOBAL)]
    pub server: Option<PathBuf>,
    /// The registry (default: [registry] default, https://fluxpkg.dev/)
    #[arg(long, global = true, value_name = "URL", help_heading = GLOBAL)]
    pub registry: Option<String>,
    /// What IS_DEVELOPMENT, IS_TEST and IS_PRODUCTION say in the spec sandbox
    #[arg(
        long,
        global = true,
        value_name = "ENV",
        default_value = "production",
        value_parser = PossibleValuesParser::new(["production", "development", "test"])
            .try_map(|s| parse_env(&s)),
        help_heading = GLOBAL
    )]
    pub env: Env,
    /// Use the cache only (a file:// registry is read as well); fail with the
    /// ids it lacks
    #[arg(long, global = true, help_heading = GLOBAL)]
    pub offline: bool,
    /// Machine-readable output
    #[arg(long, global = true, help_heading = GLOBAL)]
    pub json: bool,
    /// Print only errors and results
    #[arg(short, long, global = true, conflicts_with = "verbose", help_heading = GLOBAL)]
    pub quiet: bool,
    /// Print more detail
    #[arg(short, long, global = true, help_heading = GLOBAL)]
    pub verbose: bool,
    /// Color in messages
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto, value_name = "WHEN", help_heading = GLOBAL)]
    pub color: ColorChoice,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// The heading of the options every command takes, after its own.
const GLOBAL: &str = "Global options";

fn parse_env(s: &str) -> Result<Env, String> {
    s.parse()
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Turn a directory into a project (a root packagespec.lua and packages/);
    /// a schema with a gamemode <name>.txt is then a gamemode project. `fpkg
    /// new` scaffolds a schema, package, plugin or addon
    Init {
        /// The directory (default: --project, else the current directory)
        dir: Option<PathBuf>,
    },
    /// Scaffold a schema (./<name>/), a package (packages/<name>/ of the
    /// project, or ./<name>/ outside one), a Flux plugin (plugins/ of the
    /// project, or ./ outside one) or a Garry's Mod addon (./<name>/)
    New {
        /// What to scaffold
        #[arg(value_enum)]
        kind: NewKind,
        /// Its id, which also names its folder: lowercase letters, digits and
        /// underscores, starting with a letter
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
        /// The checkout, a directory with a packagespec.lua (a relative path
        /// gives a relative link)
        dir: Option<PathBuf>,
        /// The package id (default: the checkout's project id)
        #[arg(long = "as", value_name = "ID")]
        as_id: Option<String>,
        /// Replace an existing link, or a package installed from a registry
        #[arg(long)]
        force: bool,
    },
    /// Remove the link packages/<id>
    Unlink {
        /// The id of the linked package
        id: String,
    },
    /// Packages in the project: id, version, source, path
    List,
    /// Dependency tree with resolved versions
    Tree {
        /// Show only the tree below this package
        id: Option<String>,
    },
    /// Show a manifest: the project's package of that id, else the registry's
    /// newest version (with @version, always the registry's)
    Info {
        /// The package id, with @<version> for a version on the registry
        #[arg(value_name = "ID[@VERSION]")]
        spec: String,
    },
    /// Validate specs (default: the project manifest and every workspace,
    /// linked, nested and path package; --all adds installed registry
    /// packages). Lists the packages with problems, and every one with -v
    Check {
        /// Check only these packages of the project
        ids: Vec<String>,
        /// Also check packages installed from a registry
        #[arg(long)]
        all: bool,
    },
    /// Build the archive and print its path and sha256
    Pack {
        /// The package (default: the one the current directory is in, else
        /// the project root of a project that is not a gamemode)
        id: Option<String>,
        /// Output directory (default: <project>/target/fpkg)
        #[arg(short = 'o', long = "out", value_name = "DIR")]
        out: Option<PathBuf>,
        /// Print the members and the sha256 instead of writing the archive
        #[arg(long)]
        list: bool,
    },
    /// Resolve and write fpkg.lock, keeping locked versions
    Lock,
    /// Re-resolve the given ids (or all) to the newest matching versions
    Update {
        /// The locked packages to update (default: all of them)
        ids: Vec<String>,
    },
    /// Add s.depends to the project manifest (or change its constraint),
    /// lock, install
    Add {
        /// The package id; without a constraint, `~> X.Y` of its newest
        /// version is written
        #[arg(value_name = "ID[@CONSTRAINT]")]
        spec: String,
    },
    /// Remove a dependency from the project manifest, lock, install
    Remove {
        /// The id of the dependency
        id: String,
    },
    /// Write fpkg.lock if it is missing or stale, install the locked registry
    /// packages into packages/, then copy the content/ of every package into a
    /// gamemode project's content/ and their binary modules into the server's
    /// garrysmod/lua/bin/
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
        /// Words to look for in package ids, names and summaries
        query: String,
        /// The page of the results
        #[arg(long, default_value_t = 1)]
        page: u32,
    },
    /// Check, pack and upload a package
    Publish {
        /// The package (default: the one the current directory is in, else
        /// the project root of a project that is not a gamemode)
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
        /// The version to yank
        #[arg(value_name = "ID@VERSION")]
        spec: String,
        /// Take the yank back
        #[arg(long)]
        undo: bool,
    },
    /// Manage a package's owners
    Owner {
        #[command(subcommand)]
        action: OwnerCmd,
    },
    /// Store a token of the registry, created on its tokens page (which this
    /// opens) and pasted here, or piped to stdin
    Login,
    /// Delete the stored token
    Logout,
    /// Print and open the docs URL of a package
    Docs {
        /// The package id, with @<version> for another than the newest
        #[arg(value_name = "ID[@VERSION]")]
        spec: String,
    },
    /// The download cache
    Cache {
        #[command(subcommand)]
        action: CacheCmd,
    },
    /// Copy the registry packages of this project's fpkg.lock from a registry
    /// into a file:// registry directory
    Mirror {
        /// The registry to copy from
        registry_url: String,
        /// The directory of the file:// registry, created if needed
        dir: PathBuf,
    },
    /// Print the completion script of a shell (bash, zsh, fish, elvish,
    /// powershell) to stdout
    Completions {
        /// The shell
        shell: clap_complete::Shell,
    },
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
    /// Add an owner (who can publish and yank)
    Add {
        /// The package id
        id: String,
        /// The owner's login on the registry
        login: String,
    },
    /// Remove an owner
    Remove {
        /// The package id
        id: String,
        /// The owner's login on the registry
        login: String,
    },
    /// List the owners
    List {
        /// The package id
        id: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheCmd {
    /// Delete the cache
    Clean,
    /// Print the cache directory
    Dir,
}

/// The subcommand that `--project` took as its value (`fpkg --project install`).
fn subcommand_taken_by_project() -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    let command = Cli::command();
    args.windows(2)
        .find(|w| w[0] == "--project" && command.find_subcommand(&w[1]).is_some())
        .map(|w| w[1].clone())
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
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            if e.kind() == clap::error::ErrorKind::MissingSubcommand
                && let Some(name) = subcommand_taken_by_project()
            {
                eprintln!(
                    "error: --project took '{name}' as its directory; give the directory first: fpkg --project <dir> {name}"
                );
                return ExitCode::from(EXIT_HUMAN);
            }
            e.exit()
        }
    };
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

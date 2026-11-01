# fpkg

`fpkg` is the package manager for [Flux](https://github.com/TeslaCloud/flux-ce) packages.
It reads `packagespec.lua` on the same VM family the game uses (the `nl` fork of
LuaJIT, in its sandbox), validates packages, packs them into byte-for-byte reproducible
zip archives, resolves and locks dependencies with version constraints, installs
packages from a registry into `packages/`, verifies them against the lockfile, and
publishes them to the registry at `fluxpkg.dev`.

| Path | What it is |
| --- | --- |
| `crates/fpkg-core` | The library: spec evaluation, versions, archives, lockfile, resolver, registries, install/verify |
| `crates/fpkg` | The `fpkg` command line tool |
| `tests/fixtures/flux-ce` | The 17 `packagespec.lua` files of flux-ce with stand-in files, used by every test |
| `tests/conformance` | The registry conformance suite |
| `tests/sandbox` | Hostile specs against the process evaluator |
| `tests/e2e` | End-to-end tests of the CLI |
| `server/` | The registry, a Cloudflare Worker (its own README) |

## Building

You need Rust 1.99 and a checkout of the `nl` repository next to this one (`../nl`).
mlua builds LuaJIT from the `luajit-src` crate, and the workspace patches that crate
with the nl fork, prepared once into `vendor/luajit-src/` (git-ignored):

```sh
scripts/prepare-nl.sh            # uses ../nl; NL_DIR=/path/to/nl to point elsewhere
cargo build --release            # target/release/fpkg
```

A build without the prepared crate fails at link time on `nl_version_0_1`; that is
intended.

## Using it

A project is a directory with a root `packagespec.lua` (the project manifest, whose
`s.depends` entries are the direct dependencies) and a `packages/` directory. `flux-ce`
is one; a schema becomes one with `fpkg init --project`. fpkg finds the project by
walking up from the current directory, or takes `--project <dir>`.

Dependencies may carry a constraint. Because a Lua call statement cannot be followed by
another expression, the constraint form needs parentheses:

```lua
Package:describe(function(s)
  s.name    = 'My Schema'
  s.version = '0.1.0'

  s.depends 'cable'                 -- any version
  s.depends('sfs', '~> 7.0')        -- >= 7.0.0, < 8.0.0
end)
```

Constraints are `1.2.3` / `== 1.2.3`, `>= 1.2`, `~> 1.2` (`< 2.0.0`), `~> 1.2.3`
(`< 1.3.0`), `<`, `<=`, `>`, `*`, combined with commas. A pre-release
(`2.0.0-beta`) satisfies a constraint only when the constraint names a pre-release of
the same X.Y.Z (`>= 2.0.0-alpha`); `*` picks one only when the package has no release.

Specs are evaluated in a child process each (`fpkg __eval-spec`), in the nl sandbox,
with a budget of one million instructions, 2 s of CPU, 512 MiB of memory, 5 s of wall
clock and 1 MiB of output, so a spec that loops, recurses or allocates without bound
fails on its own without taking fpkg down. Inside the spec, `PACKAGE.__path__` is
what Flux sets: `<gamemode>/packages/<id>/`, where the gamemode is the name in the
project's `<name>.txt` (`flux` for flux-ce).

### Commands

```
fpkg init <id>                   scaffold packages/<id>/packagespec.lua and lib/
fpkg init --project              add a root packagespec.lua and packages/ to a schema
fpkg list                        packages in the project: id, version, source, path
fpkg tree [id]                   dependency tree with resolved versions
fpkg info <id>[@version]         show a manifest (local, or the registry's)
fpkg check [id...] [--all]       validate specs; default: the project manifest, workspace and path packages; --all adds installed ones
fpkg pack [id] [-o dir] [--list] build the archive (default into target/fpkg/), print path and sha256
fpkg lock                        resolve and write fpkg.lock, keeping locked versions
fpkg update [id...]              re-resolve the given ids (or all) to the newest matching versions
fpkg add <id>[@constraint]       add s.depends to the root packagespec.lua, lock, install
fpkg remove <id>                 the reverse
fpkg install [--locked] [--offline] [--force]
fpkg verify                      installed packages match the lock
fpkg search <query> [--page n]
fpkg publish [id] [--dry-run] [--all] [--registry url] [--yes] [--allow-missing-dependencies]
fpkg yank <id>@<version> [--undo]
fpkg owner add|remove|list <id> [login]
fpkg login [--registry url] / fpkg logout
fpkg docs <id>[@version]         print and open the docs URL
fpkg cache clean|dir
fpkg mirror <registry-url> <dir> copy the packages fpkg.lock needs into a file:// registry
```

Global flags: `--project <dir>`, `--registry <url>`, `--env production|development|test`
(what `IS_DEVELOPMENT`, `IS_TEST` and `IS_PRODUCTION` say in the spec sandbox),
`--offline`, `--json` (machine-readable output), `-q`/`-v`, `--color auto|always|never`.

Exit codes: `0` success, `1` failure, `2` usage or a case that needs a human (an edit
`fpkg add`/`remove` cannot make safely, an unmarked package directory that the lock has
as a registry package), `3` the lock is missing or stale under `--locked`, `4` `verify`
found a difference.

Typical flows:

```sh
# In flux-ce or a schema
fpkg check
fpkg lock && git add fpkg.lock
fpkg add sfs                 # writes s.depends('sfs', '~> 7.0'), locks, installs

# CI and deployment
fpkg install --locked        # fails with exit 3 instead of rewriting a stale lock
fpkg verify                  # exit 4 if an installed package differs from the lock

# Publishing (versions must be written X.Y.Z)
fpkg login
fpkg publish cable
fpkg publish --all --yes     # every workspace package not yet on the registry, in dependency order

# Air-gapped servers
fpkg mirror https://fluxpkg.dev/ /srv/fpkg-mirror
fpkg install --locked --registry file:///srv/fpkg-mirror/
```

With an explicit `--registry`, `install` fetches every locked archive from that
registry (a mirror); the lockfile's checksums are still what is verified.

Each installed package's `.fpkg.toml` records a hash of every file it was installed
with. `fpkg verify` fails (exit 4) on any changed, extra or missing file, and
`fpkg install` reinstalls such a package and reports it as repaired.

### Configuration

Layers, each overriding the one before: `~/.config/fpkg/config.toml`,
`<project>/.fpkg/config.toml`, environment variables, then flags.

```toml
[registry]
default = "https://fluxpkg.dev/"

[net]
offline = false
retries = 3
timeout_seconds = 30

[pack]
compression = 6
```

Environment variables are `FPKG_<SECTION>_<KEY>` (`FPKG_NET_RETRIES`), with the short
forms `FPKG_REGISTRY` and `FPKG_OFFLINE`. `FPKG_TOKEN` overrides the stored token (for
CI), but is sent only to the registry named by `--registry`, `FPKG_REGISTRY` or the
user config (or the built-in default): a registry that only a project's
`.fpkg/config.toml` names never receives it. `FPKG_CONFIG_DIR` and `FPKG_CACHE_DIR`
move `~/.config/fpkg` and `~/.cache/fpkg`. `SOURCE_DATE_EPOCH` sets the timestamp of
archive members (zip's 1980 epoch otherwise). `FPKG_EVALUATOR` names a spec evaluator
binary for programs that embed `fpkg-core` (the crate builds one, `fpkg-eval-spec`).

Network timeouts (`[net] timeout_seconds`) apply to connecting and to each read or
write, not to a whole download.

Tokens live in `~/.config/fpkg/credentials.toml` (mode `0600`) and are only sent over
HTTPS, or plain HTTP to `localhost`/`127.0.0.1`.

## Testing

```sh
cargo test                                  # unit tests, conformance against a file:// registry, e2e
cargo clippy --all-targets
```

The conformance suite always runs against a `FileRegistry` and against a small
reference HTTP server (`tests/support/mock_registry.rs`). `cargo test -p fpkg-core
--test sandbox` runs hostile specs (C recursion, coroutine and `pcall` loops,
exponential tables, memory bombs) against the process evaluator. Against a real server, such as
the Worker under `wrangler dev`, set both variables:

```sh
FPKG_TEST_REGISTRY=http://127.0.0.1:8787/ FPKG_TOKEN=fpk_... \
  cargo test -p fpkg-core --test conformance -- --nocapture
```

`server/scripts/conformance.sh` does this for the Worker.

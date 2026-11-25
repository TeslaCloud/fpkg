# fpkg

`fpkg` is the package manager for [Flux](https://github.com/TeslaCloud/flux-ce) packages.
It reads `packagespec.lua` on the same VM family the game uses (the `nl` fork of
LuaJIT, in its sandbox), validates packages, packs them into byte-for-byte reproducible
zip archives, resolves and locks dependencies with version constraints, installs
packages from a registry into `packages/`, verifies them against the lockfile, and
publishes them to the registry at `fluxpkg.dev`. It also links local checkouts in place
of registry packages and copies the packages' game content and binary modules into a
Garry's Mod server.

| Path | What it is |
| --- | --- |
| `crates/fpkg-core` | The library: spec evaluation, versions, archives, lockfile, resolver, registries, install/verify |
| `crates/fpkg` | The `fpkg` command line tool |
| `crates/fpkg/templates` | The files `fpkg new` writes, embedded into the binary, with `{{id}}`-style placeholders |
| `tests/fixtures/flux-ce` | The 17 `packagespec.lua` files of flux-ce (the root one is the package `flux`) with stand-in files, used by every test |
| `tests/conformance` | The registry conformance suite |
| `tests/sandbox` | Hostile specs against the process evaluator |
| `tests/e2e` | End-to-end tests of the CLI |
| `server/` | The registry, a Cloudflare Worker (its own README) |

## Installing fpkg

On Linux (x86_64 or aarch64), the install script downloads the latest release, checks it against
the release's `SHA256SUMS` and puts `fpkg` into `~/.local/bin`, or into the directory you give it.
It says what to add to `PATH` when that directory is not in it:

```sh
curl -fsSL https://raw.githubusercontent.com/TeslaCloud/fpkg/master/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/TeslaCloud/fpkg/master/install.sh | sh -s -- /usr/local/bin
```

`FPKG_VERSION=X.Y.Z` in its environment installs that version instead.

By hand, download the archive of your system from the
[releases](https://github.com/TeslaCloud/fpkg/releases): `fpkg-X.Y.Z-linux-x86_64.tar.gz` or
`fpkg-X.Y.Z-linux-aarch64.tar.gz` (a static binary that runs on any Linux) or
`fpkg-X.Y.Z-windows-x86_64.zip`. Each is also there without the version in its name, so
`https://github.com/TeslaCloud/fpkg/releases/latest/download/fpkg-linux-x86_64.tar.gz` is always
the latest. Check it against `SHA256SUMS`, unpack it and put `fpkg` (`fpkg.exe`) into a folder in
your `PATH`:

```sh
sha256sum -c --ignore-missing SHA256SUMS
tar -xzf fpkg-X.Y.Z-linux-x86_64.tar.gz
install -m 755 fpkg-X.Y.Z-linux-x86_64/fpkg ~/.local/bin/
```

With Docker alone, run the image in the project or the server folder:

```sh
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD":/work ghcr.io/teslacloud/fpkg install
```

`-u` makes the files it writes yours. A user that the image knows only by its ID has no home
folder in the container, so `fpkg login` cannot store a token there: give the token to `publish`,
`yank` and `owner` as `-e FPKG_TOKEN=fpk_...` instead.

`fpkg completions <shell>` prints the completion script of `bash`, `zsh`, `fish`, `elvish` or
`powershell`, for example
`fpkg completions bash > ~/.local/share/bash-completion/completions/fpkg`.

fpkg is not on crates.io, since it builds against a checkout of `nl`: to build it yourself, see
[Building from source](#building-from-source).

## Building from source

You need Rust 1.100 (the `rust-version` of the workspace, which the nl crates need) and a checkout
of the `nl` repository next to this one (`../nl`).
mlua builds LuaJIT from the `luajit-src` crate, and the workspace patches that crate
with the nl fork, prepared once into `vendor/luajit-src/` (git-ignored):

```sh
scripts/prepare-nl.sh            # uses ../nl; NL_DIR=/path/to/nl to point elsewhere
cargo build --release            # target/release/fpkg
```

The code is formatted with `cargo fmt` (the default style), which CI checks. The server's
TypeScript is linted with oxlint (`npm run lint` in `server/`), which CI checks as well.

A build without the prepared crate fails at link time on `nl_version_0_1`; that is
intended.

### Container image

`ghcr.io/teslacloud/fpkg` has the prebuilt binary for `linux/amd64` and `linux/arm64`,
statically linked, on Alpine. `.github/workflows/image.yml` publishes it once CI has passed:
`latest` and `sha-<commit>` from master, `X.Y.Z` and `X.Y` from a tag `vX.Y.Z` (which has
to be the version in `Cargo.toml`).

```sh
# In a project or a server folder; FPKG_TOKEN for publish, yank and owner
docker run --rm -u "$(id -u):$(id -g)" -v "$PWD":/work ghcr.io/teslacloud/fpkg install
```

```dockerfile
# In an image of your own: the binary runs on any Linux
COPY --from=ghcr.io/teslacloud/fpkg /usr/local/bin/fpkg /usr/local/bin/fpkg
```

The image keeps its package cache in `/var/cache/fpkg` (mount a volume there to keep it
between runs) and takes `FPKG_TOKEN` and the other `FPKG_*` variables from the environment.
To build it locally, with the `nl` checkout as the build context `nl`:

```sh
scripts/prepare-nl.sh
docker buildx build --build-context nl=../nl -t fpkg .
```

### Releases

The `Release` workflow (`.github/workflows/release.yml`) is run by hand, from the Actions
page or with `gh workflow run release.yml`. It runs CI, builds the binaries and drafts the
GitHub release `vX.Y.Z` of the version in `Cargo.toml` with these files:

| File | What it is |
| --- | --- |
| `fpkg-X.Y.Z-linux-x86_64.tar.gz`, `fpkg-X.Y.Z-linux-aarch64.tar.gz` | The binary of the image, statically linked: it runs on any Linux |
| `fpkg-X.Y.Z-windows-x86_64.zip` | `fpkg.exe` for 64-bit Windows, which needs no DLLs but the system's |
| `fpkg-linux-x86_64.tar.gz`, `fpkg-linux-aarch64.tar.gz`, `fpkg-windows-x86_64.zip` | The same archives without the version in the name, for `releases/latest/download/...` links |
| `SHA256SUMS` | The SHA-256 of each archive |

`install.sh` at the root of the repository installs the Linux archive of the latest published
release (it reads the version from the GitHub API, so a draft is not found).

Running it again replaces the draft; a published release is never touched, so raise the
version first. Publishing the draft creates the tag `vX.Y.Z`, for which the image `X.Y.Z` is
published.

The Windows binary is cross-compiled on Linux with MinGW-w64 (`gcc-mingw-w64-x86-64` on
Debian and Ubuntu, and `rustup target add x86_64-pc-windows-gnu`):

```sh
scripts/prepare-nl.sh
scripts/build-windows.sh         # target/x86_64-pc-windows-gnu/release/fpkg.exe
```

The workflow then runs it on Windows, where it has to pack the archive of a fixture byte for
byte as the Linux binary does. On Windows the spec evaluator has its instruction, wall clock
and output budgets, but not the process limits on CPU time and memory that Unix has.

## Using it

A project is a directory with a root `packagespec.lua`, the project manifest, whose
`s.depends` entries are the direct dependencies. Packages live in its `packages/`. fpkg
finds the project by walking up from the current directory, or takes `--project <dir>`.
There are three kinds:

- A **gamemode project** has a gamemode `<name>.txt` whose first token is `"<name>"` (in
  any case, after `//` comments); the name is the project id. A schema such as `hl2rp` is one, and is the only folder a
  server needs in `garrysmod/gamemodes/`: Flux is the package `flux`, installed with its
  dependencies into `packages/`. The root of a gamemode project is never packed or
  published. A fresh clone (no `packages/`, no lock) is a project already.
- A **monorepo** such as flux-ce has `packages/` and no gamemode `.txt`. Its root manifest
  is a package too, the project id being `s.id` (`s.id = 'flux'`), else the directory
  name. Its workspace packages are the directories in `packages/`.
- A **standalone package project** is a directory with a `packagespec.lua` and nothing
  else, such as the checkout of a binary module (`gmsv_file`). Its root is the package.
  A directory like that inside the `packages/` of an enclosing project is that
  project's package instead.

Dependencies may carry a constraint. Because a Lua call statement cannot be followed by
another expression, the constraint form needs parentheses:

```lua
Package:describe(function(s)
  s.name    = 'My Schema'
  s.version = '0.1.0'

  s.depends('flux', '~> 0.9')       -- >= 0.9.0, < 1.0.0
  s.depends 'cable'                 -- any version
end)
```

Constraints are `1.2.3` / `== 1.2.3`, `>= 1.2`, `~> 1.2` (`< 2.0.0`), `~> 1.2.3`
(`< 1.3.0`), `<`, `<=`, `>`, `*`, combined with commas. A pre-release
(`2.0.0-beta`) satisfies a constraint only when the constraint names a pre-release of
the same X.Y.Z (`>= 2.0.0-alpha`); `*` picks one only when the package has no release.
A dependency that names a directory (`s.depends '../shared/foo'`) is a path package.

Specs are evaluated in a child process each (`fpkg __eval-spec`), in the nl sandbox,
with a budget of one million instructions, 2 s of CPU, 512 MiB of memory, 5 s of wall
clock and 1 MiB of output, so a spec that loops, recurses or allocates without bound
fails on its own without taking fpkg down. Inside the spec, `PACKAGE.__path__` is
what Flux sets: `<gamemode>/packages/<id>/` (for a nested package
`<gamemode>/packages/<outer>/packages/<id>/`), where the gamemode is the project id.
`file.Find` with `GAME` sees the project's `content/` and the package's own.

### Scaffolding

`fpkg new <kind> <name>` scaffolds one of four kinds; `<name>` is an id
(`^[a-z][a-z0-9_]{0,63}$`) and `--title` the display name (the name in title case by
default):

- `schema`: a gamemode project in `./<name>/` that depends on `flux` (`--flux` sets the
  constraint, `~> 0.9` by default; it is accepted for this kind only): `<name>.txt`, a
  `packagespec.lua` with a commented-out `-- s.depends 'gmsv_pg'` for servers that use
  PostgreSQL (`fpkg add gmsv_pg` uncomments it and drops the comment above it that says so),
  `gamemode/init.lua` and `cl_init.lua`, which include Flux's `boot.lua` (`init.lua` is
  written in the syntax of the stock `lua_shared` and first says in the server console when
  Flux is not installed or the server runs the stock `lua_shared`, which cannot run Flux),
  `<name>.yml` (the `roleplay` profile, the development plugins, two config values and
  commented-out `depends` and `exclude` lists), a README, a `.gitignore` that leaves out the
  installed packages, the local config and `target/`, an empty `config/` that Git keeps with
  its `.gitkeep`, a `.proofreader.yml` with the LuaJIT 3.0 style readers of Flux, a
  working `schema/` folder (the main file, a client and a server hook, a citizen faction,
  the English phrases and a theme), and the container setup: a `Dockerfile` that bakes
  the checkout with its installed packages into a dedicated server image, a
  `docker-compose.yml` that runs it with the checkout mounted (SQLite, or PostgreSQL and
  MariaDB under profiles), a `.dockerignore` that keeps local config out of the image,
  and the Docker section of the README. `--skip-docker` leaves the container setup out.
- `package`: a Flux package, in `packages/<name>/` of the project the current directory
  (or `--project`) is in, else a standalone package project in `./<name>/` (its spec has
  `s.id`, and it gets a README and a `.gitignore`). Its spec names the main file
  `lib/sh_<name>.lua` and the global `s.global`, the name in PascalCase. In a project
  whose `.gitignore` has the line `/packages/*`, `!/packages/<name>/` is appended so that
  git tracks it. Flux loads it once the project depends on it: `fpkg add <name>`.
- `plugin`: a Flux plugin folder (`plugin.yml`, the main file, a client and a server hook,
  a config key and its phrases) in `plugins/` of the project, else in the current
  directory. A schema loads every plugin in its `plugins/`.
- `addon`: a Garry's Mod addon in `./<name>/` that runs on any gamemode without Flux
  (`addon.json`, `lua/autorun/<name>.lua` and a shared, a client and a server file), and
  is a standalone package project too, so that `fpkg check` and `fpkg pack` work on it.
  fpkg does not bundle dependencies into addons yet.

`--minimal` writes only the files the kind needs to load, never the container setup: for a
schema `<name>.txt`, `packagespec.lua`, the two gamemode files and `schema/sh_schema.lua`
(which Flux includes unconditionally; without `<name>.yml` the schema loads no plugins),
for a package its spec and main file, for a plugin a single-file `sh_<name>.lua`, for an
addon its spec and the autorun file. `--minimal --skip-docker` is the same.

`fpkg init [dir]` turns an existing directory (`dir`, else `--project`, else the current one)
into a project (a root `packagespec.lua` and `packages/`), without gamemode files. It warns
when the directory is inside another project, which then no longer finds it as its own.

### Workspace, links and nested packages

Every directory in `packages/` without an `.fpkg.toml` marker is a workspace package and
shadows the registry. One that has a `packages/` of its own also provides the packages in
it that have no marker either, one level deep: with flux-ce at `packages/flux`, its `packages/cable/` is the package
`cable` at `packages/flux/packages/cable`. A top-level directory of the same id wins (a
warning in `check`); two nested ones of the same id are an error, and the first outer
directory in id order wins. Nested packages are never replaced or removed by `install`, and
are locked and installed only when something depends on them (a top-level workspace package
always is).

`fpkg link <dir>` points the project at a local checkout: `packages/<id>` becomes a
symlink to `<dir>`, which must have a `packagespec.lua`. The id is `--as`, else the
checkout's project id, so `fpkg link ../flux-ce` links `packages/flux`. A relative `<dir>`
gives a relative link (`packages/flux -> ../../flux-ce`), an absolute one an absolute
link. A symlink is always a workspace package: with flux-ce linked, a schema that depends
on `flux` resolves, locks and installs with no registry at all, and a lock that has `flux`
from a registry is merely stale. `link` refuses to replace an installed package unless
`--force` (which also replaces a link), and never deletes a directory that has no marker.
`fpkg unlink <id>` removes the link; `fpkg install` then fetches the registry version (or takes
the package out of the lock when nothing depends on it). A lock written while a package was
linked has it as a local package; `fpkg install --locked` says so once the link is gone, and
`fpkg lock` locks the registry version instead.
`fpkg link` lists the links. Pack and publish a linked package in its own project. On
Windows a directory symlink needs Developer Mode or an elevated prompt.

### Content and binary modules

After the packages are in place, `install` copies, for every package of the lock:

- its `content/**` into the project's `content/`, in a gamemode project (the game mounts
  `gamemodes/<name>/content/`);
- its binary modules into `<server>/garrysmod/lua/bin/`: the files directly in its
  binaries folder whose name ends in `.dll` (Garry's Mod modules are `.dll` on every
  platform). The folder is `s.binaries = '<folder>'`, relative to the package (`bin` by
  default) and has to stay inside it; other files in it, such as `LICENSES.txt`, stay in
  the package. `pack` includes every file directly in the folder `s.binaries` names even
  when `.gitignore` or `.fpkgignore` drops it (build output usually is ignored), and
  refuses an explicit `s.binaries` that holds no `.dll` yet: build the binaries first.
  Of an ignored default `bin/`, it includes the `.dll` files alone.

The server folder is known from `--server <dir>` (the server root or its `garrysmod`),
from running inside the server folder, or because the project was reached as
`<server>/garrysmod/gamemodes/<name>` (also through a symlinked gamemode folder). Without
it, `install` still succeeds and names the packages whose modules it could not place
(`-v` names the files). When the server folder has its binaries, only the modules of their
platforms are copied: `srcds_run_x64` or `bin/linux64/srcds` take `*_linux64.dll`,
`srcds_run` or `bin/linux32/srcds` `*_linux.dll`, `srcds_win64.exe` `*_win64.dll` and
`srcds.exe` `*_win32.dll` (a module whose name ends in no platform is always copied); in a
folder without them, every module is. In a server folder, `install` of a project that locks
`flux` also creates `garrysmod/lua/_flux/`, where Flux writes generated files from its first
boot on.

What was copied is recorded in `packages/.fpkg-files.toml` with its package and
SHA-256; fpkg refuses a record whose paths are not `content/<path>` or `bin/<name>`, and
never writes or removes a file through a symlinked directory (exit 2). Each install writes
missing and changed files, removes recorded files that no package provides any more (unless
they were changed since, which it leaves in place with a warning) and prunes the directories
that became empty. A file that is there but was not written by fpkg and differs is a conflict
(exit 2) unless `--force`; one with the same content is taken over. Two packages that provide
one path with different content are an error. `fpkg verify` checks the recorded files too,
and fails on a file a package provides that was never copied (an install that stopped at a
conflict). `garrysmod/lua/bin/` is shared by the gamemodes of a server: a module that another
project there still records is left in place when this one stops providing it, and a module
another gamemode installed with different content is a conflict that names that gamemode.
`fpkg.lock` and the record are written with mode 0644.

### Server folders

When no project is found from the current directory, `install`, `verify` and `list`
look for a Garry's Mod server tree around it (a `garrysmod` directory with `gamemodes/`,
or a directory with `garrysmod/gamemodes/`), or take `--server <dir>`, and process every
project in `garrysmod/gamemodes/` in name order, each under a heading. One that fails
does not stop the others; the exit code is the highest. A server owner drops the schema
folder into `gamemodes/` and runs `fpkg install` anywhere in the server folder.

### Root packages

In a project that is not a gamemode, the root manifest is a package with the project id.
`fpkg pack` and `fpkg publish` without an id at the root take it (as does naming it).
`publish` needs the root's spec to state the id (`s.id = '<id>'`): the name of a directory
is not an id to publish under, and `publish --all` leaves such a root out. The
archive is `<id>/...` with the root directory minus `packages/`, `fpkg.lock`, `.fpkg/`,
`target/` and the fixed exclusions, honoring the root `.fpkgignore`. `publish --all`
publishes it last, after the workspace packages.

### Commands

```
fpkg new schema <name> [--minimal] [--skip-docker] [--title t] [--flux c]  a gamemode project in ./<name>/
fpkg new package <name> [--minimal] [--title t]  packages/<name>/ of the project, else ./<name>/
fpkg new plugin <name> [--minimal] [--title t]   a Flux plugin in plugins/ of the project, else ./
fpkg new addon <name> [--minimal] [--title t]    a Garry's Mod addon in ./<name>/
fpkg init [dir]                  add a root packagespec.lua and packages/ to a directory
fpkg link <dir> [--as id] [--force]     use a local checkout as packages/<id>
fpkg link                        list the links: id, target, resolved path, version
fpkg unlink <id>                 remove the link packages/<id>
fpkg list                        packages in the project: id, version, source (workspace, link, link (<outer>), registry+, path+), path
fpkg tree [id]                   dependency tree with resolved versions
fpkg info <id>[@version]         show a manifest (the project's package of that id, else the registry's newest; the registry's with @version)
fpkg check [id...] [--all]       validate specs; default: the project manifest, workspace, linked, nested and path packages; --all adds installed ones
fpkg pack [id] [-o dir]          build the archive (default into target/fpkg/), print path and sha256
fpkg pack [id] --list            print the members and the sha256, write nothing
fpkg lock                        resolve and write fpkg.lock, keeping locked versions
fpkg update [id...]              re-resolve the given ids (or all) to the newest matching versions
fpkg add <id>[@constraint]       add s.depends to the root packagespec.lua (or change its constraint), lock, install
fpkg remove <id>                 the reverse
fpkg install [--locked] [--offline] [--force]  write a missing or stale lock, install, copy content and modules
fpkg verify                      installed packages and copied files match the lock and the record
fpkg search <query> [--page n]
fpkg publish [id] [--dry-run] [--all] [--registry url] [--yes] [--allow-missing-dependencies]
fpkg yank <id>@<version> [--undo]
fpkg owner add|remove|list <id> [login]
fpkg login [--registry url] / fpkg logout
fpkg docs <id>[@version]         print and open the docs URL
fpkg cache clean|dir
fpkg mirror <registry-url> <dir> copy the registry packages of this project's fpkg.lock into a file:// registry
fpkg completions <shell>         print the completion script of bash, zsh, fish, elvish or powershell
```

`fpkg <command> --help` lists the options of a command first, then the global ones.

`check` lists the packages that have problems, each with its errors and warnings, then a
summary; `-v` lists every package, `-q` leaves out the warnings. It also warns about a package
dependency that is not a package of the project, not in `fpkg.lock` and not on the registry
(which it asks only about such an id, and never with `--offline`), with the nearest id it
knows (`did you mean flux?`). A resolution that fails names the nearest id of one that does
not exist, and the versions there are of one that a constraint finds none of.

`add <id>` without a constraint writes `~> X.Y` of the newest version (of a pre-release,
`>= X.Y.Z-pre, < (X+1).0.0`, since only a constraint that names a pre-release allows it). It
resolves what the edited manifest evaluates to before it writes anything, and writes the
manifest in one step. A locked registry package keeps its checksum: when the registry has
another one for the same version, `lock`, `install` and `add` fail, and only `fpkg update <id>`
takes it.

Global flags: `--project <dir>`, `--server <dir>`, `--registry <url>`, `--env
production|development|test` (what `IS_DEVELOPMENT`, `IS_TEST` and `IS_PRODUCTION` say
in the spec sandbox), `--offline` (the cache only; a `file://` registry, such as a mirror,
is still read), `--json` (machine-readable output; for several projects of a server, one
object with a `projects` list), `-q`/`-v`, `--color auto|always|never` (stderr and stdout
are colored when each is a terminal).

Spec fields fpkg knows beyond Flux's: `s.id` (the package id; it must match the
directory name of a package, and is the project id of a root without a gamemode `.txt`,
written as a plain `s.id = '<id>'` line, which fpkg reads before evaluating the spec) and
`s.binaries` (above).

Exit codes: `0` success, `1` failure, `2` usage (a bad argument or package id, no project
where the command runs, a `--project` or `--server` that does not exist, a package id that
is not one of the project's) or a case that needs a human (an edit `fpkg add`/`remove`
cannot make safely, an unmarked package directory that the lock has as a registry package,
a file in `content/` or `lua/bin/` that fpkg did not write, a symlinked directory there), `3`
the lock is missing or stale under `--locked`, `4` `verify` found a difference.

Typical flows:

```sh
# A new schema
fpkg new schema hl2rp --title "HL2 RP" && cd hl2rp
fpkg install                 # flux and its packages into packages/, fpkg.lock (commit it)
fpkg add gmsv_pg             # the PostgreSQL module into packages/; copied to garrysmod/lua/bin
                             # by an install that knows the server folder

# Framework development: a flux-ce checkout instead of the registry version
fpkg link ../flux-ce && fpkg install

# A server
cp -r hl2rp /srv/gmod/garrysmod/gamemodes/
cd /srv/gmod && fpkg install --locked
fpkg verify                  # exit 4 if an installed package or a copied file differs

# Publishing (versions must be written X.Y.Z). A package new to the registry is held
# for a registry admin's approval: fpkg says "Submitted", and until it is approved only
# its owners see it and nothing can depend on it. Later versions publish directly.
fpkg login
fpkg publish cable
fpkg publish --all --yes     # in flux-ce: every package not yet on the registry, in dependency order, flux last
cd ../gmsv_file && fpkg pack --list && fpkg publish

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

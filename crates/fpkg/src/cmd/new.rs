//! `fpkg new <kind> <name>`: scaffolds a schema, a package, a Flux plugin or a
//! Garry's Mod addon from the templates in `crates/fpkg/templates/`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use fpkg_core::PackageId;
use fpkg_core::project::{PACKAGES_DIR, Project, ProjectError};
use fpkg_core::spec::{self, SPEC_FILE};
use serde_json::json;

use super::{Ctx, exit};
use crate::{EXIT_HUMAN, NewKind};

/// The constraint on flux of a new schema without `--flux`.
pub const DEFAULT_FLUX: &str = "~> 0.9";

macro_rules! tmpl {
    ($path:literal) => {
        include_str!(concat!("../../templates/", $path, ".tmpl"))
    };
}

/// A template: the destination path (relative, may hold placeholders) and the text.
type Template = (&'static str, &'static str);

const SCHEMA: &[Template] = &[
    ("{{id}}.txt", tmpl!("schema/gamemode.txt")),
    ("packagespec.lua", tmpl!("schema/packagespec.lua")),
    ("gamemode/init.lua", tmpl!("schema/gamemode/init.lua")),
    ("gamemode/cl_init.lua", tmpl!("schema/gamemode/cl_init.lua")),
    ("{{id}}.yml", tmpl!("schema/gamemode.yml")),
    (".gitignore", tmpl!("schema/gitignore")),
    ("config/.gitkeep", tmpl!("schema/config/gitkeep")),
    (".proofreader.yml", tmpl!("schema/proofreader.yml")),
    ("README.md", tmpl!("schema/README.md")),
    ("schema/sh_schema.lua", tmpl!("schema/schema/sh_schema.lua")),
    ("schema/cl_hooks.lua", tmpl!("schema/schema/cl_hooks.lua")),
    ("schema/sv_hooks.lua", tmpl!("schema/schema/sv_hooks.lua")),
    (
        "schema/factions/sh_citizen.lua",
        tmpl!("schema/schema/factions/sh_citizen.lua"),
    ),
    (
        "schema/languages/en.yml",
        tmpl!("schema/schema/languages/en.yml"),
    ),
    (
        "schema/themes/cl_{{id}}.lua",
        tmpl!("schema/schema/themes/cl_theme.lua"),
    ),
];

/// The container setup of a default schema, unless `--skip-docker`. The README
/// gets the section of `README_DOCKER` and `.gitignore` the override file.
const SCHEMA_DOCKER: &[Template] = &[
    ("Dockerfile", tmpl!("schema/Dockerfile")),
    ("docker-compose.yml", tmpl!("schema/docker-compose.yml")),
    (".dockerignore", tmpl!("schema/dockerignore")),
];

const README_DOCKER: &str = tmpl!("schema/README.docker.md");

/// What a schema needs to boot: Flux includes `schema/sh_schema.lua`
/// unconditionally. Without `<id>.yml` the schema loads no plugins.
const SCHEMA_MINIMAL: &[Template] = &[
    ("{{id}}.txt", tmpl!("schema/gamemode.txt")),
    ("packagespec.lua", tmpl!("schema/packagespec.lua")),
    ("gamemode/init.lua", tmpl!("schema/gamemode/init.lua")),
    ("gamemode/cl_init.lua", tmpl!("schema/gamemode/cl_init.lua")),
    (
        "schema/sh_schema.lua",
        tmpl!("schema/schema/sh_schema.minimal.lua"),
    ),
];

const PACKAGE: &[Template] = &[
    ("packagespec.lua", tmpl!("package/packagespec.lua")),
    ("lib/sh_{{id}}.lua", tmpl!("package/lib/sh_package.lua")),
];

const PACKAGE_STANDALONE: &[Template] = &[
    ("README.md", tmpl!("package/README.md")),
    (".gitignore", tmpl!("package/gitignore")),
];

const PACKAGE_MINIMAL: &[Template] = &[
    ("packagespec.lua", tmpl!("package/packagespec.minimal.lua")),
    (
        "lib/sh_{{id}}.lua",
        tmpl!("package/lib/sh_package.minimal.lua"),
    ),
];

const PLUGIN: &[Template] = &[
    ("{{id}}/plugin.yml", tmpl!("plugin/plugin.yml")),
    (
        "{{id}}/plugin/sh_plugin.lua",
        tmpl!("plugin/plugin/sh_plugin.lua"),
    ),
    (
        "{{id}}/plugin/cl_hooks.lua",
        tmpl!("plugin/plugin/cl_hooks.lua"),
    ),
    (
        "{{id}}/plugin/sv_hooks.lua",
        tmpl!("plugin/plugin/sv_hooks.lua"),
    ),
    (
        "{{id}}/plugin/config/config.yml",
        tmpl!("plugin/plugin/config/config.yml"),
    ),
    (
        "{{id}}/plugin/languages/en.yml",
        tmpl!("plugin/plugin/languages/en.yml"),
    ),
];

const PLUGIN_MINIMAL: &[Template] = &[("sh_{{id}}.lua", tmpl!("plugin/sh_plugin.minimal.lua"))];

const ADDON: &[Template] = &[
    ("packagespec.lua", tmpl!("addon/packagespec.lua")),
    ("addon.json", tmpl!("addon/addon.json")),
    (
        "lua/autorun/{{id}}.lua",
        tmpl!("addon/lua/autorun/autorun.lua"),
    ),
    (
        "lua/{{id}}/sh_{{id}}.lua",
        tmpl!("addon/lua/addon/sh_addon.lua"),
    ),
    (
        "lua/{{id}}/cl_{{id}}.lua",
        tmpl!("addon/lua/addon/cl_addon.lua"),
    ),
    (
        "lua/{{id}}/sv_{{id}}.lua",
        tmpl!("addon/lua/addon/sv_addon.lua"),
    ),
    ("README.md", tmpl!("addon/README.md")),
    (".gitignore", tmpl!("addon/gitignore")),
];

const ADDON_MINIMAL: &[Template] = &[
    ("packagespec.lua", tmpl!("addon/packagespec.minimal.lua")),
    (
        "lua/autorun/{{id}}.lua",
        tmpl!("addon/lua/autorun/autorun.minimal.lua"),
    ),
];

/// The values the placeholders of the templates stand for.
#[derive(Clone, Debug)]
pub struct Vars {
    pub id: String,
    pub title: String,
    pub author: String,
    pub date: String,
    pub flux: String,
    /// Whether a schema gets the container setup: `{{docker_readme}}` and
    /// `{{docker_gitignore}}` are empty without it.
    pub docker: bool,
}

impl Vars {
    /// The text of a placeholder: `{{id}}`, `{{title}}`, `{{author}}`, `{{date}}`,
    /// `{{flux}}` as they are, `{{global}}` (the id in PascalCase), and the
    /// variants quoted for a format: `_lua` (a Lua string literal), `_txt` (the
    /// inside of a KeyValues string), `_yaml` (a YAML scalar), `_json`.
    fn get(&self, key: &str) -> Option<String> {
        Some(match key {
            "id" => self.id.clone(),
            "global" => pascal_case(&self.id),
            "title" => self.title.clone(),
            "title_lua" => lua_str(&self.title),
            "title_txt" => txt_str(&self.title),
            "title_yaml" => yaml_str(&self.title),
            "title_json" => serde_json::to_string(&self.title).ok()?,
            "author" => self.author.clone(),
            "author_lua" => lua_str(&self.author),
            "author_txt" => txt_str(&self.author),
            "author_yaml" => yaml_str(&self.author),
            "date" => self.date.clone(),
            "flux" => self.flux.clone(),
            "flux_lua" => lua_str(&self.flux),
            "docker_readme" if self.docker => render(README_DOCKER, self),
            "docker_gitignore" if self.docker => "/docker-compose.override.yml\n".into(),
            "docker_readme" | "docker_gitignore" => String::new(),
            _ => return None,
        })
    }
}

/// Replaces every `{{key}}` of `template` in one pass, so that a value that
/// holds braces is never expanded again.
fn render(template: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let key = &after[..end];
        match vars.get(key) {
            Some(value) => out.push_str(&value),
            None => panic!("unknown template placeholder {{{{{key}}}}}"),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

fn render_all(templates: &[Template], vars: &Vars) -> Vec<(String, String)> {
    templates
        .iter()
        .map(|(path, text)| (render(path, vars), render(text, vars)))
        .collect()
}

pub fn today() -> String {
    spec::format_date(time::OffsetDateTime::now_utc().date())
}

pub fn author() -> String {
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "Unknown".into())
}

/// A single-quoted Lua string literal.
pub fn lua_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// The inside of a KeyValues string: KeyValues has no escape for `"`.
fn txt_str(s: &str) -> String {
    s.replace('"', "'")
}

/// A quoted YAML scalar that reads the same in YAML and in the YAML parser of
/// Flux, which knows no escapes: double quotes unless the text has `"` or `\`,
/// else single quotes unless it has `'`, else double quotes with `"` written
/// as `'` and `\` as `/`.
fn yaml_str(s: &str) -> String {
    if !s.contains(['"', '\\']) {
        format!("\"{s}\"")
    } else if !s.contains('\'') {
        format!("'{s}'")
    } else {
        format!("\"{}\"", s.replace('"', "'").replace('\\', "/"))
    }
}

/// `my_plugin` -> `My Plugin`.
pub fn title_case(id: &str) -> String {
    words(id).collect::<Vec<_>>().join(" ")
}

/// `my_plugin` -> `MyPlugin`, the name of a global.
pub fn pascal_case(id: &str) -> String {
    words(id).collect()
}

fn words(id: &str) -> impl Iterator<Item = String> + '_ {
    id.split('_').filter(|w| !w.is_empty()).map(|w| {
        let mut c = w.chars();
        match c.next() {
            Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
            None => String::new(),
        }
    })
}

/// The files of a new schema, by path relative to its directory. A minimal
/// schema never has the container setup.
pub fn schema_files(vars: &Vars, minimal: bool, docker: bool) -> Vec<(String, String)> {
    let vars = Vars {
        docker: docker && !minimal,
        ..vars.clone()
    };
    let mut files = render_all(if minimal { SCHEMA_MINIMAL } else { SCHEMA }, &vars);
    if vars.docker {
        files.extend(render_all(SCHEMA_DOCKER, &vars));
    }
    files
}

/// The files of a new package, by path relative to its directory. A
/// standalone package project declares its id and, unless minimal, has a
/// README and a `.gitignore`.
pub fn package_files(vars: &Vars, minimal: bool, standalone: bool) -> Vec<(String, String)> {
    let mut files = render_all(if minimal { PACKAGE_MINIMAL } else { PACKAGE }, vars);
    if standalone {
        if let Some((_, spec)) = files.iter_mut().find(|(p, _)| p == SPEC_FILE) {
            *spec = spec.replacen(
                "(function(s)\n",
                &format!("(function(s)\n  s.id          = '{}'\n", vars.id),
                1,
            );
        }
        if !minimal {
            files.extend(render_all(PACKAGE_STANDALONE, vars));
        }
    }
    files
}

/// The files of a new plugin, by path relative to the folder that holds the
/// plugins: a plugin folder, or with `minimal` a single-file plugin.
pub fn plugin_files(vars: &Vars, minimal: bool) -> Vec<(String, String)> {
    render_all(if minimal { PLUGIN_MINIMAL } else { PLUGIN }, vars)
}

/// The files of a new addon, by path relative to its directory.
pub fn addon_files(vars: &Vars, minimal: bool) -> Vec<(String, String)> {
    render_all(if minimal { ADDON_MINIMAL } else { ADDON }, vars)
}

/// The options of `fpkg new`.
pub struct NewOpts {
    pub kind: NewKind,
    pub name: String,
    pub minimal: bool,
    pub title: Option<String>,
    pub flux: Option<String>,
    pub skip_docker: bool,
}

fn kind_name(kind: NewKind) -> &'static str {
    match kind {
        NewKind::Schema => "schema",
        NewKind::Package => "package",
        NewKind::Plugin => "plugin",
        NewKind::Addon => "addon",
    }
}

/// `fpkg new <kind> <name>`.
pub fn new(ctx: &Ctx, opts: NewOpts) -> Result<()> {
    let NewOpts {
        kind,
        name,
        minimal,
        title,
        flux,
        skip_docker,
    } = opts;
    if let Some(reason) = PackageId::problem(&name) {
        let what = match kind {
            NewKind::Schema => "a schema (gamemode) name",
            NewKind::Package => "a package id",
            NewKind::Plugin => "a plugin id",
            NewKind::Addon => "an addon name",
        };
        return Err(exit(
            EXIT_HUMAN,
            format!("'{name}' cannot be {what}: {reason} (^[a-z][a-z0-9_]{{0,63}}$)"),
        ));
    }
    if flux.is_some() && kind != NewKind::Schema {
        return Err(exit(
            EXIT_HUMAN,
            format!(
                "--flux is for fpkg new schema; a new {} does not depend on flux",
                kind_name(kind)
            ),
        ));
    }
    if skip_docker && kind != NewKind::Schema {
        return Err(exit(
            EXIT_HUMAN,
            format!(
                "--skip-docker is for fpkg new schema; a new {} has no container setup",
                kind_name(kind)
            ),
        ));
    }
    let flux = flux.unwrap_or_else(|| DEFAULT_FLUX.to_string());
    fpkg_core::Constraint::parse(&flux)
        .map_err(|e| exit(EXIT_HUMAN, format!("--flux '{flux}': {e}")))?;
    let title = match title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        Some(t) if t.chars().any(char::is_control) => {
            return Err(exit(
                EXIT_HUMAN,
                "--title cannot hold line breaks or other control characters",
            ));
        }
        Some(t) => t,
        None => title_case(&name),
    };
    let vars = Vars {
        id: name.clone(),
        title,
        author: author(),
        date: today(),
        flux,
        docker: false,
    };
    match kind {
        NewKind::Schema => new_schema(ctx, &vars, minimal, !skip_docker),
        NewKind::Package => new_package(ctx, &vars, minimal),
        NewKind::Plugin => new_plugin(ctx, &vars, minimal),
        NewKind::Addon => new_addon(ctx, &vars, minimal),
    }
}

/// `./<name>/`, which must not exist or be empty.
fn fresh_dir(name: &str) -> Result<PathBuf> {
    let dir = super::cwd()?.join(name);
    if dir.exists() && (!dir.is_dir() || std::fs::read_dir(&dir)?.next().is_some()) {
        return Err(exit(
            EXIT_HUMAN,
            format!("{} already exists and is not empty", dir.display()),
        ));
    }
    Ok(dir)
}

/// The project the current directory (or `--project`) is in; `None` outside
/// any project.
fn enclosing_project(ctx: &Ctx) -> Result<Option<Project>> {
    match ctx.project() {
        Ok(p) => Ok(Some(p)),
        Err(e)
            if ctx.project_flag.is_none()
                && e.downcast_ref::<ProjectError>()
                    .is_some_and(|pe| matches!(pe, ProjectError::NotFound(_))) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

fn write_files(base: &Path, files: &[(String, String)]) -> Result<()> {
    for (path, text) in files {
        let dest = path.split('/').fold(base.to_path_buf(), |p, c| p.join(c));
        if let Some(parent) = dest.parent() {
            fs_err::create_dir_all(parent)?;
        }
        fs_err::write(&dest, text)?;
    }
    Ok(())
}

/// Writes the files, prints the status line and the JSON result. Returns
/// whether the human-readable note should follow.
fn finish(
    ctx: &Ctx,
    kind: NewKind,
    vars: &Vars,
    path: &Path,
    base: &Path,
    files: &[(String, String)],
) -> Result<bool> {
    write_files(base, files)?;
    let kind = kind_name(kind);
    ctx.out.status(
        "Created",
        &format!("{kind} {} ({}) in {}", vars.id, vars.title, path.display()),
    );
    if ctx.out.json {
        let rel: Vec<String> = files
            .iter()
            .map(|(p, _)| match base.join(p).strip_prefix(path) {
                Ok(r) if !r.as_os_str().is_empty() => r.display().to_string(),
                _ => p.clone(),
            })
            .collect();
        ctx.out.json(&json!({ "kind": kind, "id": vars.id, "title": vars.title, "path": path, "files": rel }));
        return Ok(false);
    }
    Ok(true)
}

fn new_schema(ctx: &Ctx, vars: &Vars, minimal: bool, docker: bool) -> Result<()> {
    let dir = fresh_dir(&vars.id)?;
    if finish(
        ctx,
        NewKind::Schema,
        vars,
        &dir,
        &dir,
        &schema_files(vars, minimal, docker),
    )? {
        let compose = if docker && !minimal {
            "  docker compose up                    starts a server with it (README.md)\nor copy"
        } else {
            "Then copy"
        };
        ctx.out.note(&format!(
            "\nNext:\n  cd {}\n  fpkg install                         installs flux and its packages into packages/\n{compose} the folder into garrysmod/gamemodes/ of a server and run fpkg install there.\n\nTo develop the framework itself, use a checkout of flux-ce instead of the registry:\n  fpkg link <path to a flux-ce checkout>\n  fpkg install",
            vars.id
        ));
    }
    Ok(())
}

fn new_package(ctx: &Ctx, vars: &Vars, minimal: bool) -> Result<()> {
    let id = &vars.id;
    let Some(project) = enclosing_project(ctx)? else {
        let dir = fresh_dir(id)?;
        if finish(
            ctx,
            NewKind::Package,
            vars,
            &dir,
            &dir,
            &package_files(vars, minimal, true),
        )? {
            ctx.out.note(&format!(
                "\nNext:\n  cd {id}\n  fpkg check\n\nTo use it from a schema, run in the schema:\n  fpkg link {}\n  {:<37}declares s.depends('{id}', ...) in its {SPEC_FILE}, locks and installs",
                dir.display(),
                format!("fpkg add {id}")
            ));
        }
        return Ok(());
    };
    let dir = project.package_dir(id);
    if dir.exists() || dir.is_symlink() {
        return Err(exit(
            EXIT_HUMAN,
            format!("{} already exists", dir.display()),
        ));
    }
    let human = finish(
        ctx,
        NewKind::Package,
        vars,
        &dir,
        &dir,
        &package_files(vars, minimal, false),
    )?;
    match gitignore_packages(&project.root, id)? {
        Gitignore::Appended => ctx.out.note(&format!("Added !/{PACKAGES_DIR}/{id}/ to .gitignore, which ignores the installed packages")),
        Gitignore::Ignored(line) => ctx.out.note(&format!(
            "note: the line {line} of .gitignore ignores {PACKAGES_DIR}/{id}/; write it as /{PACKAGES_DIR}/* and add !/{PACKAGES_DIR}/{id}/ to track the package"
        )),
        Gitignore::Untouched => {}
    }
    if human {
        ctx.out.note(&format!(
            "\nNext: Flux loads a package that the project depends on. Declare it in {}:\n  {:<37}adds s.depends('{id}', ...), locks and installs\nor add the line s.depends '{id}' to it yourself.",
            project.manifest_path().display(),
            format!("fpkg add {id}")
        ));
    }
    Ok(())
}

/// What `gitignore_packages` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Gitignore {
    /// `!/packages/<id>/` was appended after `/packages/*`.
    Appended,
    /// A line that ignores the whole folder, which cannot be undone for one entry.
    Ignored(String),
    Untouched,
}

/// Lets git track a new workspace package in a project whose `.gitignore`
/// ignores the installed ones with `/packages/*`.
pub fn gitignore_packages(root: &Path, id: &str) -> Result<Gitignore> {
    let path = root.join(".gitignore");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(Gitignore::Untouched);
    };
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let keep = format!("!/{PACKAGES_DIR}/{id}/");
    if lines.contains(&format!("/{PACKAGES_DIR}/*").as_str()) {
        if lines.contains(&keep.as_str()) {
            return Ok(Gitignore::Untouched);
        }
        let sep = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs_err::write(&path, format!("{text}{sep}{keep}\n"))?;
        return Ok(Gitignore::Appended);
    }
    for line in [format!("/{PACKAGES_DIR}/"), format!("{PACKAGES_DIR}/")] {
        if lines.contains(&line.as_str()) {
            return Ok(Gitignore::Ignored(line));
        }
    }
    Ok(Gitignore::Untouched)
}

fn new_plugin(ctx: &Ctx, vars: &Vars, minimal: bool) -> Result<()> {
    let id = &vars.id;
    let project = enclosing_project(ctx)?;
    let base = match &project {
        Some(p) => p.root.join("plugins"),
        None => super::cwd()?,
    };
    let taken = std::iter::once(id.clone())
        .chain(["cl", "sh", "sv"].map(|realm| format!("{realm}_{id}.lua")));
    for name in taken {
        let p = base.join(&name);
        if p.exists() || p.is_symlink() {
            return Err(exit(
                EXIT_HUMAN,
                format!(
                    "{} already exists, and a plugin is known by its id '{id}'",
                    p.display()
                ),
            ));
        }
    }
    let path = if minimal {
        base.join(format!("sh_{id}.lua"))
    } else {
        base.join(id)
    };
    if !finish(
        ctx,
        NewKind::Plugin,
        vars,
        &path,
        &base,
        &plugin_files(vars, minimal),
    )? {
        return Ok(());
    }
    let note = match &project {
        Some(p) if p.gamemode => format!("Next: the schema loads every plugin in {}, so {id} loads with it.", base.display()),
        Some(p) if p.id == "flux" => format!(
            "Next: Flux loads one of its own plugins only when a schema depends on it: add {id} to depends in the schema's <schema>.yml, or to a profile in config/profiles.yml."
        ),
        _ => "Next: move the plugin into the plugins/ folder of a schema, which loads every plugin in it.".to_string(),
    };
    ctx.out.note(&format!("\n{note}"));
    Ok(())
}

fn new_addon(ctx: &Ctx, vars: &Vars, minimal: bool) -> Result<()> {
    let dir = fresh_dir(&vars.id)?;
    if finish(
        ctx,
        NewKind::Addon,
        vars,
        &dir,
        &dir,
        &addon_files(vars, minimal),
    )? {
        ctx.out.note(&format!(
            "\nNext: copy the folder into garrysmod/addons/ of a server, or link it there:\n  ln -s {} <server>/garrysmod/addons/{}\nfpkg check and fpkg pack work in it, as in any standalone package project.",
            dir.display(),
            vars.id
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;

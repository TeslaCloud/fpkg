//! Commands that read the project: init, list, tree, info, check, pack.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use fpkg_core::archive::{self, PackOptions};
use fpkg_core::install::{Loaded, load};
use fpkg_core::lockfile::{Lockfile, Source};
use fpkg_core::project::{PACKAGES_DIR, Project};
use fpkg_core::registry::{Registry, VersionMeta};
use fpkg_core::resolve::Release;
use fpkg_core::spec::{self, DepKind, Manifest, SPEC_FILE, Severity, SpecLocation};
use fpkg_core::{PackageId, Version};
use serde_json::json;

use super::{Ctx, exit, exit_silent, split_at};
use crate::output::{Style, table};
use crate::{EXIT_FAILURE, EXIT_HUMAN};

fn today() -> String {
    spec::format_date(time::OffsetDateTime::now_utc().date())
}

fn author() -> String {
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

fn lua_str(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn title_case(id: &str) -> String {
    id.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn init(ctx: &Ctx, id: Option<String>) -> Result<()> {
    match (id, &ctx.project_flag) {
        (None, Some(dir)) => init_project(ctx, dir.clone()),
        (None, None) => Err(exit(EXIT_HUMAN, "give a package id (fpkg init <id>) or --project")),
        (Some(_), Some(None)) => Err(exit(EXIT_HUMAN, "fpkg init <id> creates a package; fpkg init --project creates a project, not both")),
        (Some(id), _) => init_package(ctx, &id),
    }
}

fn init_project(ctx: &Ctx, dir: Option<PathBuf>) -> Result<()> {
    let dir = match dir {
        Some(d) => d,
        None => std::env::current_dir()?,
    };
    fs_err::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    let spec_path = dir.join(SPEC_FILE);
    if spec_path.exists() {
        bail!("{} already exists", spec_path.display());
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    let text = format!(
        "Package:describe(function(s)\n  s.name        = {}\n  s.version     = '0.1.0'\n  s.date        = '{}'\n  s.summary     = ''\n  s.authors     = {{ {} }}\nend)\n",
        lua_str(&name),
        today(),
        lua_str(&author())
    );
    fs_err::write(&spec_path, text)?;
    fs_err::create_dir_all(dir.join(PACKAGES_DIR))?;
    ctx.out.status("Created", &format!("project in {} ({SPEC_FILE}, {PACKAGES_DIR}/)", dir.display()));
    if ctx.out.json {
        ctx.out.json(&json!({ "project": dir }));
    }
    Ok(())
}

fn init_package(ctx: &Ctx, id: &str) -> Result<()> {
    PackageId::new(id)?;
    let project = ctx.project()?;
    let dir = project.package_dir(id);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    fs_err::create_dir_all(dir.join("lib"))?;
    let text = format!(
        "Package:describe(function(s)\n  s.name        = {}\n  s.version     = '0.1.0'\n  s.date        = '{}'\n  s.summary     = ''\n  s.authors     = {{ {} }}\n  s.file        = 'lib/{id}.lua'\n  s.license     = 'MIT'\nend)\n",
        lua_str(&title_case(id)),
        today(),
        lua_str(&author())
    );
    fs_err::write(dir.join(SPEC_FILE), text)?;
    fs_err::write(dir.join("lib").join(format!("{id}.lua")), format!("-- {id}\n"))?;
    ctx.out.status("Created", &format!("package {id} in {}", dir.display()));
    if ctx.out.json {
        ctx.out.json(&json!({ "id": id, "path": dir }));
    }
    Ok(())
}

fn rel(project: &Project, p: &std::path::Path) -> String {
    p.strip_prefix(&project.root).map(|r| r.display().to_string()).unwrap_or_else(|_| p.display().to_string())
}

pub fn list(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    let loaded = load(&project, ctx.env, None)?;
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for (id, m) in &loaded.workspace {
        rows.push((id.clone(), m.version.to_string(), "workspace".into(), rel(&project, &project.package_dir(id))));
    }
    for (id, p) in &loaded.installed {
        let m = p.marker.as_ref().map(|m| (m.version.to_string(), format!("registry+{}", m.registry)));
        let (v, s) = m.unwrap_or_default();
        rows.push((id.clone(), v, s, rel(&project, &p.dir)));
    }
    for (id, (dir, m)) in &loaded.paths {
        rows.push((id.clone(), m.version.to_string(), format!("path+{dir}"), dir.clone()));
    }
    rows.sort();
    if ctx.out.json {
        let v: Vec<_> = rows.iter().map(|(i, v, s, p)| json!({ "id": i, "version": v, "source": s, "path": p })).collect();
        ctx.out.json(&json!(v));
        return Ok(());
    }
    let mut t = vec![vec!["ID".into(), "VERSION".into(), "SOURCE".into(), "PATH".into()]];
    t.extend(rows.into_iter().map(|(a, b, c, d)| vec![a, b, c, d]));
    print!("{}", table(&t));
    Ok(())
}

/// The lock if it is current, otherwise a resolution in memory.
fn current_graph(ctx: &Ctx, project: &Project) -> Result<(Loaded, Lockfile)> {
    let lock = Lockfile::read(&project.lock_path())?;
    let loaded = load(project, ctx.env, lock.as_ref())?;
    if let Some(l) = &lock
        && loaded.staleness(l).is_empty()
    {
        return Ok((loaded, l.clone()));
    }
    ctx.out.note(if lock.is_some() { "fpkg.lock is stale; showing a fresh resolution" } else { "no fpkg.lock; showing a fresh resolution" });
    let config = ctx.config(Some(project))?;
    let registries = ctx.registries(&config)?;
    let resolved = loaded.resolve(lock.as_ref(), Release::None, &registries)?;
    Ok((loaded, resolved))
}

fn source_note(s: &Source, verbose: bool) -> String {
    match s {
        Source::Workspace => String::new(),
        Source::Registry(u) if verbose => format!(" (registry+{u})"),
        Source::Registry(_) => " (registry)".into(),
        Source::Path(p) => format!(" (path {p})"),
    }
}

fn tree_json(lock: &Lockfile, id: &str) -> serde_json::Value {
    let Some(p) = lock.get(id) else { return json!({ "id": id }) };
    let deps: Vec<_> = p.dependencies.iter().map(|d| tree_json(lock, d)).collect();
    json!({ "id": id, "version": p.version.to_string(), "source": p.source.to_string(), "dependencies": deps })
}

fn print_tree(lock: &Lockfile, id: &str, prefix: &str, last: bool, seen: &mut BTreeSet<String>, verbose: bool, top: bool) {
    let branch = if top { "" } else if last { "└── " } else { "├── " };
    let Some(p) = lock.get(id) else {
        println!("{prefix}{branch}{id} (not in the lock)");
        return;
    };
    let repeat = !p.dependencies.is_empty() && seen.contains(id);
    println!("{prefix}{branch}{id} {}{}{}", p.version, source_note(&p.source, verbose), if repeat { " (*)" } else { "" });
    if repeat {
        return;
    }
    seen.insert(id.to_string());
    let child_prefix = if top { prefix.to_string() } else { format!("{prefix}{}", if last { "    " } else { "│   " }) };
    for (i, d) in p.dependencies.iter().enumerate() {
        print_tree(lock, d, &child_prefix, i + 1 == p.dependencies.len(), seen, verbose, false);
    }
}

pub fn tree(ctx: &Ctx, id: Option<String>) -> Result<()> {
    let project = ctx.project()?;
    let (loaded, lock) = current_graph(ctx, &project)?;
    let direct: Vec<String> = {
        let mut seen = BTreeSet::new();
        loaded.root.package_deps().filter_map(|d| d.package_id().map(str::to_string)).filter(|d| seen.insert(d.clone())).collect()
    };
    if let Some(id) = id {
        if lock.get(&id).is_none() {
            bail!("{id} is not in the dependency graph");
        }
        if ctx.out.json {
            ctx.out.json(&tree_json(&lock, &id));
        } else {
            print_tree(&lock, &id, "", true, &mut BTreeSet::new(), ctx.out.verbose, true);
        }
        return Ok(());
    }
    // Local packages nothing depends on still belong to the project.
    let mut reachable = BTreeSet::new();
    let mut stack = direct.clone();
    while let Some(x) = stack.pop() {
        if reachable.insert(x.clone())
            && let Some(p) = lock.get(&x)
        {
            stack.extend(p.dependencies.iter().cloned());
        }
    }
    let others: Vec<String> = lock.packages.iter().filter(|p| !reachable.contains(&p.id)).map(|p| p.id.clone()).collect();
    if ctx.out.json {
        let deps: Vec<_> = direct.iter().map(|d| tree_json(&lock, d)).collect();
        let other: Vec<_> = others.iter().map(|d| tree_json(&lock, d)).collect();
        ctx.out.json(&json!({
            "id": project.id, "version": loaded.root.version.to_string(),
            "dependencies": deps, "unreferenced": other,
        }));
        return Ok(());
    }
    println!("{} {}", project.id, loaded.root.version);
    let mut seen = BTreeSet::new();
    for (i, d) in direct.iter().enumerate() {
        print_tree(&lock, d, "", i + 1 == direct.len(), &mut seen, ctx.out.verbose, false);
    }
    if !others.is_empty() {
        println!();
        println!("{}", ctx.out.paint(Style::Dim, "not depended on by the project manifest:"));
        for o in &others {
            print_tree(&lock, o, "", true, &mut seen, ctx.out.verbose, true);
        }
    }
    Ok(())
}

fn manifest_rows(m: &Manifest) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut add = |k: &str, v: String| {
        if !v.is_empty() {
            rows.push(vec![k.to_string(), v]);
        }
    };
    add("name", m.name.clone());
    add("version", m.version.to_string());
    add("date", m.date.map(spec::format_date).unwrap_or_default());
    add("summary", m.summary.clone().unwrap_or_default());
    add("description", m.description.clone().unwrap_or_default());
    add("authors", m.authors.join(", "));
    add("email", m.email.clone().unwrap_or_default());
    add("website", m.website.clone().unwrap_or_default());
    add("license", m.license.clone().unwrap_or_default());
    add("globals", m.globals.join(", "));
    add("files", m.files.join(", "));
    add("cl_files", m.cl_files.join(", "));
    add("sv_files", m.sv_files.join(", "));
    let realm = match (m.serverside, m.clientside) {
        (true, false) => "serverside",
        (false, true) => "clientside",
        (true, true) => "serverside and clientside (invalid)",
        (false, false) => "shared",
    };
    add("realm", realm.to_string());
    add("reload", m.reload.to_string());
    let deps: Vec<String> = m
        .dependencies
        .iter()
        .map(|d| match d.kind {
            DepKind::Package if d.constraint.is_any() => d.id.clone(),
            DepKind::Package => format!("{} {}", d.id, d.constraint),
            DepKind::File => format!("{} (file)", d.id),
            DepKind::Path => format!("{} (path)", d.id),
        })
        .collect();
    add("dependencies", deps.join(", "));
    add("resources", m.resources.join(", "));
    add("client_env", m.client_env.iter().map(|(k, v)| format!("{k} = {v}")).collect::<Vec<_>>().join(", "));
    rows
}

fn meta_rows(m: &VersionMeta, owners: &[String]) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut add = |k: &str, v: String| {
        if !v.is_empty() {
            rows.push(vec![k.to_string(), v]);
        }
    };
    add("name", m.name.clone());
    add("version", format!("{}{}", m.version, if m.yanked { " (yanked)" } else { "" }));
    add("summary", m.summary.clone().unwrap_or_default());
    add("description", m.description.clone().unwrap_or_default());
    add("authors", m.authors.join(", "));
    add("license", m.license.clone().unwrap_or_default());
    add("website", m.website.clone().unwrap_or_default());
    add("globals", m.globals.join(", "));
    add("realm", if m.serverside { "serverside" } else if m.clientside { "clientside" } else { "shared" }.into());
    add("dependencies", m.dependencies.iter().map(|d| format!("{} {}", d.id, d.constraint)).collect::<Vec<_>>().join(", "));
    add("owners", owners.join(", "));
    if let Some(d) = &m.dist {
        add("sha256", d.sha256.clone());
        add("size", d.size.to_string());
    }
    add("published", format!("{} {}", m.published_at.clone().unwrap_or_default(), m.published_by.as_deref().map(|b| format!("by {b}")).unwrap_or_default()).trim().to_string());
    add("docs", m.docs.clone().unwrap_or_default());
    rows
}

/// A local spec location for `id`: the project manifest, a package
/// directory, or a path dependency.
fn local_location(project: &Project, id: &str) -> Option<SpecLocation> {
    if id == project.id {
        return Some(project.root_location());
    }
    if spec::has_spec(&project.package_dir(id)) {
        return Some(project.package_location(id));
    }
    None
}

pub fn info(ctx: &Ctx, spec_arg: &str) -> Result<()> {
    let (id, version) = split_at(spec_arg);
    let project = ctx.project().ok();
    // Local first, unless a version is asked for; a registry package is never
    // evaluated: its local copy is what is on disk.
    if version.is_none()
        && let Some(project) = &project
        && let Some(loc) = local_location(project, id)
    {
        let m = spec::load(&loc, ctx.env)?;
        let marker = fpkg_core::project::Marker::read(&loc.dir)?;
        let source = match (&marker, loc.is_project_manifest) {
            (_, true) => "project".to_string(),
            (Some(mk), _) => format!("registry+{}", mk.registry),
            (None, _) => "workspace".to_string(),
        };
        if ctx.out.json {
            let mut v = serde_json::to_value(&m)?;
            v["source"] = json!(source);
            v["path"] = json!(loc.dir);
            ctx.out.json(&v);
            return Ok(());
        }
        println!("{} {} ({source}, {})", m.id, m.version, rel(project, &loc.dir));
        print!("{}", table(&manifest_rows(&m)));
        for line in &m.output {
            ctx.out.verbose(&format!("print: {line}"));
        }
        return Ok(());
    }
    let pid = PackageId::new(id)?;
    let config = ctx.config(project.as_ref())?;
    let registries = ctx.registries(&config)?;
    let reg = registries.default_registry()?;
    let doc = reg.package(&pid)?.ok_or_else(|| anyhow!("{id} is not a local package and not on {}", registries.default_url()))?;
    let v = match version {
        Some(v) => Version::parse(v)?,
        None => doc.newest().ok_or_else(|| anyhow!("{id} has no versions that are not yanked"))?,
    };
    let meta = doc.get(&v).ok_or_else(|| anyhow!("{id} {v} is not on {}", registries.default_url()))?;
    if ctx.out.json {
        let mut j = serde_json::to_value(meta)?;
        j["owners"] = json!(doc.owners);
        ctx.out.json(&j);
        return Ok(());
    }
    println!("{} {} ({})", meta.id, meta.version, registries.default_url());
    print!("{}", table(&meta_rows(meta, &doc.owners)));
    Ok(())
}

pub fn check(ctx: &Ctx, ids: &[String], all: bool) -> Result<()> {
    let project = ctx.project()?;
    let mut targets: Vec<SpecLocation> = Vec::new();
    if ids.is_empty() {
        // The project manifest, every workspace package, every path package;
        // installed registry packages with --all.
        targets.push(project.root_location());
        for p in project.packages()? {
            if p.is_workspace() || all {
                targets.push(project.package_location(&p.id));
            }
        }
        let root = spec::load(&project.root_location(), ctx.env);
        if let Ok(root) = &root {
            for d in root.dependencies.iter().filter(|d| d.kind == DepKind::Path) {
                let rel = d.id.trim_end_matches('/');
                if let (Some(id), Ok(dir)) = (d.package_id(), project.root.join(rel).canonicalize()) {
                    targets.push(project.location_of(dir, id, project.path_dep_lua_path(rel)));
                }
            }
        }
    } else {
        for id in ids {
            match local_location(&project, id) {
                Some(loc) => targets.push(loc),
                None => bail!("{id} is not a package of this project (no packages/{id}/{SPEC_FILE})"),
            }
        }
    }
    let mut results = Vec::new();
    let mut failed = 0;
    for (loc, loaded) in targets.iter().zip(spec::load_many(&targets, ctx.env)) {
        let label = if loc.is_project_manifest { format!("{} (project manifest)", loc.id) } else { loc.id.clone() };
        let (version, diags) = match loaded {
            Ok(m) => {
                for line in &m.output {
                    ctx.out.verbose(&format!("{}: print: {line}", loc.id));
                }
                (Some(m.version.to_string()), spec::check(&m, loc))
            }
            Err(e) => (None, vec![spec::Diagnostic { severity: Severity::Error, message: e.to_string() }]),
        };
        let errors = diags.iter().filter(|d| d.severity == Severity::Error).count();
        if errors > 0 {
            failed += 1;
        }
        if !ctx.out.json {
            let tag = if errors > 0 { ctx.out.paint(Style::Red, "error") } else { ctx.out.paint(Style::Green, "   ok") };
            println!("{tag}  {label} {}", version.as_deref().unwrap_or(""));
            for d in &diags {
                if d.severity == Severity::Warning && ctx.out.quiet {
                    continue;
                }
                let s = match d.severity {
                    Severity::Error => ctx.out.paint(Style::Red, "error:"),
                    Severity::Warning => ctx.out.paint(Style::Yellow, "warning:"),
                };
                println!("         {s} {}", d.message);
            }
        }
        results.push(json!({ "id": loc.id, "version": version, "ok": errors == 0, "diagnostics": diags }));
    }
    if ctx.out.json {
        ctx.out.json(&json!(results));
    } else {
        ctx.out.note(&format!("{} checked, {} with errors", targets.len(), failed));
    }
    if failed > 0 { Err(exit_silent(EXIT_FAILURE)) } else { Ok(()) }
}

/// Evaluates and checks a package for `pack` and `publish`; prints the
/// diagnostics and fails on errors.
pub fn checked_manifest(ctx: &Ctx, project: &Project, id: &str) -> Result<(Manifest, SpecLocation)> {
    let loc = project.package_location(id);
    if !spec::has_spec(&loc.dir) {
        bail!("{id} is not a package of this project (no packages/{id}/{SPEC_FILE})");
    }
    let m = spec::load(&loc, ctx.env)?;
    let diags = spec::check(&m, &loc);
    let mut errors = false;
    for d in &diags {
        match d.severity {
            Severity::Error => {
                errors = true;
                ctx.out.error(&format!("{id}: {}", d.message));
            }
            Severity::Warning => ctx.out.warn(&format!("{id}: {}", d.message)),
        }
    }
    if errors {
        return Err(exit(EXIT_FAILURE, format!("{id} does not pass fpkg check")));
    }
    Ok((m, loc))
}

pub fn pack_options(ctx: &Ctx, project: &Project) -> Result<PackOptions> {
    let config = ctx.config(Some(project))?;
    Ok(PackOptions::from_env(config.compression, Some(project.root.clone())))
}

pub fn pack(ctx: &Ctx, id: Option<String>, out: Option<PathBuf>, list: bool) -> Result<()> {
    let project = ctx.project()?;
    let id = id
        .or_else(|| project.current_package.clone())
        .ok_or_else(|| exit(EXIT_HUMAN, "which package? give an id, or run fpkg pack inside packages/<id>/"))?;
    let (m, loc) = checked_manifest(ctx, &project, &id)?;
    let packed = archive::pack(&loc.dir, &id, &pack_options(ctx, &project)?)?;
    let dir = out.unwrap_or_else(|| project.root.join("target").join("fpkg"));
    fs_err::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}-{}.zip", m.version));
    fs_err::write(&path, &packed.bytes)?;
    if ctx.out.json {
        ctx.out.json(&json!({
            "id": id, "version": m.version.to_string(), "path": path, "sha256": packed.sha256,
            "size": packed.bytes.len(), "members": packed.members,
        }));
        return Ok(());
    }
    println!("{}", path.display());
    println!("sha256 {}", packed.sha256);
    if list {
        let rows: Vec<Vec<String>> = packed.members.iter().map(|m| vec![format!("  {}", m.path), m.size.to_string()]).collect();
        print!("{}", table(&rows));
        let total: u64 = packed.members.iter().map(|m| m.size).sum();
        println!("{} members, {total} bytes, {} bytes compressed", packed.members.len(), packed.bytes.len());
    }
    Ok(())
}

/// For `publish --all`: workspace packages and their workspace dependencies.
pub fn workspace_graph(loaded: &Loaded) -> BTreeMap<String, Vec<String>> {
    loaded
        .workspace
        .iter()
        .map(|(id, m)| {
            let deps =
                m.package_deps().filter_map(|d| d.package_id()).filter(|d| loaded.workspace.contains_key(*d)).map(str::to_string).collect();
            (id.clone(), deps)
        })
        .collect()
}

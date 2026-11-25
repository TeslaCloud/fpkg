//! Commands that read the project: init, list, tree, info, check, pack.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use fpkg_core::archive::{self, PackOptions};
use fpkg_core::install::{Loaded, load};
use fpkg_core::lockfile::{Lockfile, Source};
use fpkg_core::project::{LocalPackage, PACKAGES_DIR, Project};
use fpkg_core::registry::{Registry, VersionMeta};
use fpkg_core::resolve::Release;
use fpkg_core::spec::{self, DepKind, Manifest, SPEC_FILE, Severity, SpecLocation};
use fpkg_core::{PackageId, Version};
use serde_json::json;

use super::new::{author, lua_str, today};
use super::{Ctx, exit, exit_silent, split_at};
use crate::output::{Style, table};
use crate::{EXIT_FAILURE, EXIT_HUMAN};

/// `fpkg init [dir]`: the directory, else `--project`, else the current one.
pub fn init(ctx: &Ctx, dir: Option<PathBuf>) -> Result<()> {
    let dir = match dir.or_else(|| ctx.project_flag.clone()) {
        Some(d) => d,
        None => super::cwd()?,
    };
    fs_err::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    let spec_path = dir.join(SPEC_FILE);
    if spec_path.exists() {
        return Err(exit(
            EXIT_HUMAN,
            format!("{} already exists", spec_path.display()),
        ));
    }
    let enclosing = dir
        .parent()
        .and_then(|parent| Project::discover(parent).ok());
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Project".into());
    let text = format!(
        "Package:describe(function(s)\n  s.name        = {}\n  s.version     = '0.1.0'\n  s.date        = '{}'\n  s.summary     = ''\n  s.authors     = {{ {} }}\n  s.license     = 'MIT'\nend)\n",
        lua_str(&name),
        today(),
        lua_str(&author())
    );
    fs_err::write(&spec_path, text)?;
    fs_err::create_dir_all(dir.join(PACKAGES_DIR))?;
    ctx.out.status(
        "Created",
        &format!(
            "project in {} ({SPEC_FILE}, {PACKAGES_DIR}/)",
            dir.display()
        ),
    );
    if let Some(outer) = enclosing {
        ctx.out.warn(&format!(
            "{} is inside the project {} ({}), and is now a project of its own that fpkg finds first from inside it; for a package of {}, delete {SPEC_FILE} and {PACKAGES_DIR}/ again and run fpkg new package <name> there",
            dir.display(),
            outer.id,
            outer.root.display(),
            outer.id
        ));
    }
    if fpkg_core::project::gamemode_name(&dir).is_some() {
        ctx.out.note("it has a gamemode <name>.txt, so it is a gamemode project: add s.depends('flux', '~> 0.9') and run fpkg install");
    } else {
        ctx.out.note("without a gamemode <name>.txt its root is a package; a schema needs one (fpkg new schema <name> scaffolds a whole gamemode project)");
    }
    if ctx.out.json {
        ctx.out.json(&json!({ "project": dir }));
    }
    Ok(())
}

fn rel(project: &Project, p: &std::path::Path) -> String {
    p.strip_prefix(&project.root)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| p.display().to_string())
}

pub fn list(ctx: &Ctx) -> Result<()> {
    ctx.for_each_project(true, &mut |p| list_one(ctx, p))
}

fn list_one(ctx: &Ctx, project: &Project) -> Result<()> {
    let loaded = load(project, ctx.env, None)?;
    let mut rows: Vec<(String, String, String, String)> = Vec::new();
    for (id, m) in &loaded.workspace {
        let (source, path) = match loaded.dirs.get(id) {
            Some(d) => (d.source_label(), d.rel.clone()),
            None => (
                "workspace".to_string(),
                rel(project, &project.package_dir(id)),
            ),
        };
        rows.push((id.clone(), m.version.to_string(), source, path));
    }
    for (id, p) in &loaded.installed {
        let m = p
            .marker
            .as_ref()
            .map(|m| (m.version.to_string(), format!("registry+{}", m.registry)));
        let (v, s) = m.unwrap_or_default();
        rows.push((id.clone(), v, s, p.rel.clone()));
    }
    for (id, (dir, m)) in &loaded.paths {
        rows.push((
            id.clone(),
            m.version.to_string(),
            format!("path+{dir}"),
            dir.clone(),
        ));
    }
    rows.sort();
    if ctx.out.json {
        let v: Vec<_> = rows
            .iter()
            .map(|(i, v, s, p)| json!({ "id": i, "version": v, "source": s, "path": p }))
            .collect();
        ctx.emit_json(json!(v));
        return Ok(());
    }
    if rows.is_empty() {
        ctx.out.note(&format!(
            "nothing installed in {PACKAGES_DIR}/; run fpkg install"
        ));
        return Ok(());
    }
    let mut t = vec![vec![
        "ID".into(),
        "VERSION".into(),
        "SOURCE".into(),
        "PATH".into(),
    ]];
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
    ctx.out.note(if lock.is_some() {
        "fpkg.lock is stale; showing a fresh resolution"
    } else {
        "no fpkg.lock; showing a fresh resolution"
    });
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

/// The tree below `id`; `above` holds the ids on the way to it, so that a
/// cycle ends in `"cycle": true` instead of recursing.
fn tree_json(lock: &Lockfile, id: &str, above: &mut Vec<String>) -> serde_json::Value {
    let Some(p) = lock.get(id) else {
        return json!({ "id": id });
    };
    if above.iter().any(|a| a == id) {
        return json!({ "id": id, "version": p.version.to_string(), "cycle": true });
    }
    above.push(id.to_string());
    let deps: Vec<_> = p
        .dependencies
        .iter()
        .map(|d| tree_json(lock, d, above))
        .collect();
    above.pop();
    json!({ "id": id, "version": p.version.to_string(), "source": p.source.to_string(), "dependencies": deps })
}

fn print_tree(
    lock: &Lockfile,
    id: &str,
    prefix: &str,
    last: bool,
    seen: &mut BTreeSet<String>,
    verbose: bool,
    top: bool,
) {
    let branch = if top {
        ""
    } else if last {
        "└── "
    } else {
        "├── "
    };
    let Some(p) = lock.get(id) else {
        println!("{prefix}{branch}{id} (not in the lock)");
        return;
    };
    let repeat = !p.dependencies.is_empty() && seen.contains(id);
    println!(
        "{prefix}{branch}{id} {}{}{}",
        p.version,
        source_note(&p.source, verbose),
        if repeat { " (*)" } else { "" }
    );
    if repeat {
        return;
    }
    seen.insert(id.to_string());
    let child_prefix = if top {
        prefix.to_string()
    } else {
        format!("{prefix}{}", if last { "    " } else { "│   " })
    };
    for (i, d) in p.dependencies.iter().enumerate() {
        print_tree(
            lock,
            d,
            &child_prefix,
            i + 1 == p.dependencies.len(),
            seen,
            verbose,
            false,
        );
    }
}

pub fn tree(ctx: &Ctx, id: Option<String>) -> Result<()> {
    let project = ctx.project()?;
    let (loaded, lock) = current_graph(ctx, &project)?;
    let direct: Vec<String> = {
        let mut seen = BTreeSet::new();
        loaded
            .root
            .package_deps()
            .filter_map(|d| d.package_id().map(str::to_string))
            .filter(|d| seen.insert(d.clone()))
            .collect()
    };
    if let Some(id) = id {
        if lock.get(&id).is_none() {
            bail!("{id} is not in the dependency graph");
        }
        if ctx.out.json {
            ctx.out.json(&tree_json(&lock, &id, &mut Vec::new()));
        } else {
            print_tree(
                &lock,
                &id,
                "",
                true,
                &mut BTreeSet::new(),
                ctx.out.verbose,
                true,
            );
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
    let others: Vec<String> = lock
        .packages
        .iter()
        .filter(|p| !reachable.contains(&p.id))
        .map(|p| p.id.clone())
        .collect();
    if ctx.out.json {
        let deps: Vec<_> = direct
            .iter()
            .map(|d| tree_json(&lock, d, &mut Vec::new()))
            .collect();
        let other: Vec<_> = others
            .iter()
            .map(|d| tree_json(&lock, d, &mut Vec::new()))
            .collect();
        ctx.out.json(&json!({
            "id": project.id, "version": loaded.root.version.to_string(),
            "dependencies": deps, "unreferenced": other,
        }));
        return Ok(());
    }
    println!("{} {}", project.id, loaded.root.version);
    let mut seen = BTreeSet::new();
    for (i, d) in direct.iter().enumerate() {
        print_tree(
            &lock,
            d,
            "",
            i + 1 == direct.len(),
            &mut seen,
            ctx.out.verbose,
            false,
        );
    }
    if !others.is_empty() {
        println!();
        println!(
            "{}",
            ctx.out
                .paint_stdout(Style::Dim, "not depended on by the project manifest:")
        );
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
    add("id", m.declared_id.clone().unwrap_or_default());
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
    add("binaries", m.binaries.clone().unwrap_or_default());
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
    add(
        "client_env",
        m.client_env
            .iter()
            .map(|(k, v)| format!("{k} = {v}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
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
    add(
        "version",
        format!("{}{}", m.version, if m.yanked { " (yanked)" } else { "" }),
    );
    add("summary", m.summary.clone().unwrap_or_default());
    add("description", m.description.clone().unwrap_or_default());
    add("authors", m.authors.join(", "));
    add("license", m.license.clone().unwrap_or_default());
    add("website", m.website.clone().unwrap_or_default());
    add("globals", m.globals.join(", "));
    add(
        "realm",
        if m.serverside {
            "serverside"
        } else if m.clientside {
            "clientside"
        } else {
            "shared"
        }
        .into(),
    );
    add(
        "dependencies",
        m.dependencies
            .iter()
            .map(|d| format!("{} {}", d.id, d.constraint))
            .collect::<Vec<_>>()
            .join(", "),
    );
    add("owners", owners.join(", "));
    if let Some(d) = &m.dist {
        add("sha256", d.sha256.clone());
        add("size", d.size.to_string());
    }
    add(
        "published",
        format!(
            "{} {}",
            m.published_at.clone().unwrap_or_default(),
            m.published_by
                .as_deref()
                .map(|b| format!("by {b}"))
                .unwrap_or_default()
        )
        .trim()
        .to_string(),
    );
    add("docs", m.docs.clone().unwrap_or_default());
    rows
}

/// A local spec location for `id`: the project manifest, a top-level package
/// directory (workspace, linked or installed), a nested one, or a path
/// dependency.
fn local_location(
    project: &Project,
    id: &str,
) -> Result<Option<(SpecLocation, Option<LocalPackage>)>> {
    if id == project.id {
        return Ok(Some((project.root_location(), None)));
    }
    let scan = project.scan()?;
    let found = scan
        .nested
        .iter()
        .find(|p| p.id == id && !scan.top.iter().any(|t| t.id == id && t.marker.is_none()));
    let found = found.or_else(|| scan.top.iter().find(|p| p.id == id));
    Ok(found.map(|p| (project.location(p), Some(p.clone()))))
}

pub fn info(ctx: &Ctx, spec_arg: &str) -> Result<()> {
    let (id, version) = split_at(spec_arg);
    let project = ctx.project().ok();
    // Local first, unless a version is asked for; a registry package is never
    // evaluated: its local copy is what is on disk.
    if version.is_none()
        && let Some(project) = &project
        && let Some((loc, dir)) = local_location(project, id)?
    {
        let m = spec::load(&loc, ctx.env)?;
        let source = match &dir {
            None if project.gamemode => "project (gamemode)".to_string(),
            None => "project".to_string(),
            Some(LocalPackage {
                marker: Some(mk), ..
            }) => format!("registry+{}", mk.registry),
            Some(d) => d.source_label(),
        };
        if ctx.out.json {
            let mut v = serde_json::to_value(&m)?;
            v["source"] = json!(source);
            v["path"] = json!(loc.dir);
            ctx.out.json(&v);
            return Ok(());
        }
        println!(
            "{} {} ({source}, {})",
            m.id,
            m.version,
            rel(project, &loc.dir)
        );
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
    let doc = reg.package(&pid)?.ok_or_else(|| {
        anyhow!(
            "{id} is not a local package and not on {}",
            registries.default_url()
        )
    })?;
    let v = match version {
        Some(v) => Version::parse(v)?,
        None => doc
            .newest()
            .ok_or_else(|| anyhow!("{id} has no versions that are not yanked"))?,
    };
    let meta = doc
        .get(&v)
        .ok_or_else(|| anyhow!("{id} {v} is not on {}", registries.default_url()))?;
    if ctx.out.json {
        let mut j = serde_json::to_value(meta)?;
        j["owners"] = json!(doc.owners);
        ctx.out.json(&j);
        return Ok(());
    }
    println!(
        "{} {} ({})",
        meta.id,
        meta.version,
        registries.default_url()
    );
    print!("{}", table(&meta_rows(meta, &doc.owners)));
    Ok(())
}

pub fn check(ctx: &Ctx, ids: &[String], all: bool) -> Result<()> {
    let project = ctx.project()?;
    let mut targets: Vec<SpecLocation> = Vec::new();
    let scan = project.scan()?;
    // Shadowed nested packages: reported with the project manifest.
    let mut layout: Vec<spec::Diagnostic> = Vec::new();
    for sh in &scan.shadowed {
        let (severity, message) = if sh.by_top_level {
            (
                Severity::Warning,
                format!("{} is hidden by {}, which has the same id", sh.rel, sh.by),
            )
        } else {
            (
                Severity::Error,
                format!(
                    "{} and {} both provide '{}'; {} is used",
                    sh.by, sh.rel, sh.id, sh.by
                ),
            )
        };
        layout.push(spec::Diagnostic { severity, message });
    }
    // The root spec is evaluated once: its path dependencies are targets too.
    let mut root = None;
    if ids.is_empty() {
        // The project manifest, every workspace package (linked and nested
        // too), every path package; installed registry packages with --all.
        targets.push(project.root_location());
        for p in &scan.top {
            if p.is_workspace() || all {
                targets.push(project.location(p));
            }
        }
        for p in &scan.nested {
            targets.push(project.location(p));
        }
        let loaded = spec::load(&project.root_location(), ctx.env);
        if let Ok(m) = &loaded {
            // One that cannot be loaded is an error of the project manifest.
            for d in m.dependencies.iter().filter(|d| d.kind == DepKind::Path) {
                if let Ok((_, loc)) = project.path_dep(d) {
                    targets.push(loc);
                }
            }
        }
        root = Some(loaded);
    } else {
        for id in ids {
            match local_location(&project, id)? {
                Some((loc, _)) => targets.push(loc),
                None => {
                    return Err(exit(
                        EXIT_HUMAN,
                        format!(
                            "{id} is not a package of this project (no packages/{id}/{SPEC_FILE})"
                        ),
                    ));
                }
            }
        }
    }
    let loaded: Vec<_> = match root {
        Some(r) => std::iter::once(r)
            .chain(spec::load_many(&targets[1..], ctx.env))
            .collect(),
        None => spec::load_many(&targets, ctx.env),
    };
    let mut unknown = UnknownDeps::new(ctx, &project, &scan);
    let compiles = targets.iter().all(|loc| {
        let shown = format!("{}{}", loc.rel_dir(), fpkg_core::tasks::TASK_FILE);
        fpkg_core::tasks::check_task_file(&loc.dir, &shown).is_none()
    });
    let mut task_diags: BTreeMap<String, Vec<spec::Diagnostic>> = BTreeMap::new();
    if ids.is_empty() && compiles {
        for (id, severity, message) in super::tasks::check_diagnostics(ctx) {
            let id = if targets.iter().any(|l| l.id == id) {
                id
            } else {
                project.id.clone()
            };
            task_diags
                .entry(id)
                .or_default()
                .push(spec::Diagnostic { severity, message });
        }
    }
    let mut results = Vec::new();
    let mut failed = 0;
    let mut warned = 0;
    for (loc, loaded) in targets.iter().zip(loaded) {
        let label = if loc.is_project_manifest {
            format!("{} (project manifest)", loc.id)
        } else {
            loc.id.clone()
        };
        let (version, diags) = match loaded {
            Ok(m) => {
                for line in &m.output {
                    ctx.out.verbose(&format!("{}: print: {line}", loc.id));
                }
                let mut diags = spec::check(&m, loc);
                diags.extend(unknown.check(&m));
                (Some(m.version.to_string()), diags)
            }
            Err(e) => (
                None,
                vec![spec::Diagnostic {
                    severity: Severity::Error,
                    message: e.to_string(),
                }],
            ),
        };
        let mut diags = diags;
        let task_file = format!("{}{}", loc.rel_dir(), fpkg_core::tasks::TASK_FILE);
        if let Some(message) = fpkg_core::tasks::check_task_file(&loc.dir, &task_file) {
            diags.push(spec::Diagnostic {
                severity: Severity::Error,
                message,
            });
        }
        if let Some(mut more) = task_diags.remove(&loc.id) {
            diags.append(&mut more);
            diags.sort_by_key(|d| std::cmp::Reverse(d.severity));
        }
        if loc.is_project_manifest && !layout.is_empty() {
            diags.append(&mut layout);
            diags.sort_by_key(|d| std::cmp::Reverse(d.severity));
        }
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = diags.len() - errors;
        if errors > 0 {
            failed += 1;
        } else if warnings > 0 {
            warned += 1;
        }
        // A package without problems is listed with -v only.
        let shown = errors > 0 || (warnings > 0 && !ctx.out.quiet) || ctx.out.verbose;
        if !ctx.out.json && shown {
            let tag = if errors > 0 {
                ctx.out.paint_stdout(Style::Red, "error")
            } else {
                ctx.out.paint_stdout(Style::Green, "   ok")
            };
            println!("{tag}  {label} {}", version.as_deref().unwrap_or(""));
            for d in &diags {
                if d.severity == Severity::Warning && ctx.out.quiet {
                    continue;
                }
                let s = match d.severity {
                    Severity::Error => ctx.out.paint_stdout(Style::Red, "error:"),
                    Severity::Warning => ctx.out.paint_stdout(Style::Yellow, "warning:"),
                };
                println!("         {s} {}", d.message);
            }
        }
        results.push(
            json!({ "id": loc.id, "version": version, "ok": errors == 0, "diagnostics": diags }),
        );
    }
    if ctx.out.json {
        ctx.out.json(&json!(results));
    } else {
        let warnings = if warned > 0 && !ctx.out.quiet {
            format!(", {warned} with warnings")
        } else {
            String::new()
        };
        ctx.out.note(&format!(
            "{} checked, {} with errors{warnings}",
            targets.len(),
            failed
        ));
    }
    if failed > 0 {
        Err(exit_silent(EXIT_FAILURE))
    } else {
        Ok(())
    }
}

/// Finds the package dependencies that nothing provides: not a package of
/// the project, not in its lock, and not on the registry (asked only for
/// such an id, and never offline; a registry that cannot be reached is no
/// finding).
struct UnknownDeps {
    known: BTreeSet<String>,
    registries: Option<fpkg_core::registry::Registries>,
    url: String,
    asked: BTreeMap<String, bool>,
}

impl UnknownDeps {
    fn new(ctx: &Ctx, project: &Project, scan: &fpkg_core::project::Scan) -> UnknownDeps {
        let mut known: BTreeSet<String> = scan
            .top
            .iter()
            .chain(&scan.nested)
            .map(|p| p.id.clone())
            .collect();
        known.insert(project.id.clone());
        if let Ok(Some(lock)) = Lockfile::read(&project.lock_path()) {
            known.extend(lock.packages.into_iter().map(|p| p.id));
        }
        let registries = (!ctx.offline)
            .then(|| ctx.config(Some(project)).ok())
            .flatten()
            .filter(|c| !c.offline)
            .and_then(|c| ctx.registries(&c).ok());
        let url = registries
            .as_ref()
            .map(|r| r.default_url().to_string())
            .unwrap_or_default();
        UnknownDeps {
            known,
            registries,
            url,
            asked: BTreeMap::new(),
        }
    }

    fn on_registry(&mut self, id: &str) -> bool {
        if let Some(found) = self.asked.get(id) {
            return *found;
        }
        let found = match (&self.registries, PackageId::new(id)) {
            (Some(r), Ok(pid)) => !matches!(
                r.default_registry().and_then(|reg| reg.package(&pid)),
                Ok(None)
            ),
            _ => true,
        };
        self.asked.insert(id.to_string(), found);
        found
    }

    fn check(&mut self, m: &Manifest) -> Vec<spec::Diagnostic> {
        let mut out = Vec::new();
        for d in m.dependencies.iter().filter(|d| d.kind == DepKind::Package) {
            if self.known.contains(&d.id) || !PackageId::is_valid(&d.id) || self.on_registry(&d.id)
            {
                continue;
            }
            let candidates = self.known.iter().map(String::as_str).chain(["flux"]);
            let hint = fpkg_core::nearest_id(&d.id, candidates)
                .map(|n| format!("; did you mean {n}?"))
                .unwrap_or_default();
            out.push(spec::Diagnostic {
                severity: Severity::Warning,
                message: format!(
                    "dependency '{}' is not a package of the project, not in fpkg.lock and not on {}{hint}",
                    d.id, self.url
                ),
            });
        }
        out
    }
}

/// What `pack` and `publish` take: the project root, when the project is not a
/// gamemode, or a top-level workspace directory of the project (not a link).
pub struct Packable {
    pub manifest: Manifest,
    pub location: SpecLocation,
    /// The project root, packed without `packages/`, `fpkg.lock` and `.fpkg/`.
    pub root: bool,
}

/// The id `pack` or `publish` acts on without one given: the package the
/// current directory is in, else the root of a project that is not a gamemode.
pub fn default_package(project: &Project, what: &str) -> Result<String> {
    if let Some(id) = &project.current_package {
        return Ok(id.clone());
    }
    if !project.gamemode {
        return Ok(project.id.clone());
    }
    Err(exit(
        EXIT_HUMAN,
        format!(
            "which package? {} is a gamemode project, not a package; give an id, or run fpkg {what} inside packages/<id>/",
            project.id
        ),
    ))
}

/// Evaluates and checks a package for `pack` and `publish`; prints the
/// diagnostics and fails on errors.
pub fn checked_manifest(ctx: &Ctx, project: &Project, id: &str) -> Result<Packable> {
    let (loc, root) = if id == project.id {
        if project.gamemode {
            return Err(exit(
                EXIT_HUMAN,
                format!(
                    "{id} is a gamemode project, not a package: it is never packed or published (its packages are, from packages/<id>/)"
                ),
            ));
        }
        (project.root_location(), true)
    } else {
        let scan = project.scan()?;
        if let Some(n) = scan.nested.iter().find(|p| p.id == id)
            && !scan.top.iter().any(|t| t.id == id && t.marker.is_none())
        {
            let outer = n.outer.clone().unwrap_or_default();
            return Err(exit(
                EXIT_HUMAN,
                format!(
                    "{id} is nested in packages/{outer} ({}); pack and publish it in that checkout's own project",
                    n.rel
                ),
            ));
        }
        match scan.top.iter().find(|p| p.id == id) {
            None => bail!("{id} is not a package of this project (no packages/{id}/{SPEC_FILE})"),
            Some(p) if p.link.is_some() => {
                let target = p.dir.canonicalize().unwrap_or_else(|_| p.dir.clone());
                return Err(exit(
                    EXIT_HUMAN,
                    format!(
                        "packages/{id} is a link to {}; pack and publish it in its own project (fpkg --project {} ...)",
                        target.display(),
                        target.display()
                    ),
                ));
            }
            Some(p) => (project.location(p), false),
        }
    };
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
    if let Some(folder) = &m.binaries
        && spec::binary_modules(&loc.dir, folder).is_empty()
    {
        return Err(exit(
            EXIT_FAILURE,
            format!(
                "{id}: s.binaries is '{folder}', but it holds no .dll files; build the binaries first"
            ),
        ));
    }
    Ok(Packable {
        manifest: m,
        location: loc,
        root,
    })
}

/// The pack options for a package `checked_manifest` returned.
pub fn pack_options(ctx: &Ctx, project: &Project, p: &Packable) -> Result<PackOptions> {
    let config = ctx.config(Some(project))?;
    let mut o = PackOptions::from_env(config.compression, Some(project.root.clone()));
    o.root_package = p.root;
    o.binaries = p.manifest.binaries.clone();
    Ok(o)
}

pub fn pack(ctx: &Ctx, id: Option<String>, out: Option<PathBuf>, list: bool) -> Result<()> {
    let project = ctx.project()?;
    let id = match id {
        Some(id) => id,
        None => default_package(&project, "pack")?,
    };
    let p = checked_manifest(ctx, &project, &id)?;
    let packed = archive::pack(&p.location.dir, &id, &pack_options(ctx, &project, &p)?)?;
    let m = &p.manifest;
    // --list shows what the archive would hold, and writes nothing.
    let path = if list {
        None
    } else {
        let dir = out.unwrap_or_else(|| project.root.join("target").join("fpkg"));
        fs_err::create_dir_all(&dir)?;
        let path = dir.join(format!("{id}-{}.zip", m.version));
        fs_err::write(&path, &packed.bytes)?;
        Some(path)
    };
    if ctx.out.json {
        ctx.out.json(&json!({
            "id": id, "version": m.version.to_string(), "path": path, "sha256": packed.sha256,
            "size": packed.bytes.len(), "members": packed.members,
        }));
        return Ok(());
    }
    if let Some(path) = &path {
        println!("{}", path.display());
    }
    println!("sha256 {}", packed.sha256);
    if list {
        let rows: Vec<Vec<String>> = packed
            .members
            .iter()
            .map(|m| vec![format!("  {}", m.path), m.size.to_string()])
            .collect();
        print!("{}", table(&rows));
        let total: u64 = packed.members.iter().map(|m| m.size).sum();
        println!(
            "{} members, {total} bytes, {} bytes compressed",
            packed.members.len(),
            packed.bytes.len()
        );
    }
    Ok(())
}

/// For `publish --all`: the project's own workspace packages (top-level, not
/// links) and their dependencies among them.
pub fn workspace_graph(loaded: &Loaded) -> BTreeMap<String, Vec<String>> {
    let own = |id: &str| loaded.dirs.get(id).is_some_and(|d| d.is_own());
    loaded
        .workspace
        .iter()
        .filter(|(id, _)| own(id))
        .map(|(id, m)| {
            let deps = m
                .package_deps()
                .filter_map(|d| d.package_id())
                .filter(|d| own(d))
                .map(str::to_string)
                .collect();
            (id.clone(), deps)
        })
        .collect()
}

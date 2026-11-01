//! Commands that talk to a registry: search, publish, yank, owner, login,
//! logout, docs, cache, mirror.

use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use fpkg_core::archive::{self, Packed};
use fpkg_core::auth::Credentials;
use fpkg_core::install::load;
use fpkg_core::lockfile::{Lockfile, Source};
use fpkg_core::registry::{self, FileRegistry, Registry, Token, VersionMeta};
use fpkg_core::resolve::topological;
use fpkg_core::spec::{DepKind, Manifest};
use fpkg_core::{PackageId, Version};
use serde_json::json;

use super::local::{checked_manifest, pack_options, workspace_graph};
use super::{Ctx, exit, open_url, split_at};
use crate::output::table;
use crate::{CacheCmd, EXIT_FAILURE, EXIT_HUMAN, OwnerCmd};

pub fn search(ctx: &Ctx, query: &str, page: u32) -> Result<()> {
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let registries = ctx.registries(&config)?;
    let reg = registries.default_registry()?;
    let result = reg.search(query, page)?;
    if ctx.out.json {
        ctx.out.json(&serde_json::to_value(&result)?);
        return Ok(());
    }
    if result.packages.is_empty() {
        ctx.out.note(&format!("no packages match '{query}'"));
        return Ok(());
    }
    let rows: Vec<Vec<String>> = result
        .packages
        .iter()
        .map(|h| vec![h.id.clone(), h.version.clone().unwrap_or_default(), h.summary.clone().unwrap_or_default()])
        .collect();
    print!("{}", table(&rows));
    let shown = (result.page.max(1) as u64 - 1) * result.per_page as u64 + result.packages.len() as u64;
    if result.total > shown {
        ctx.out.note(&format!("{} of {} shown; --page {} for more", shown, result.total, result.page + 1));
    }
    Ok(())
}

pub struct PublishOpts {
    pub dry_run: bool,
    pub all: bool,
    pub yes: bool,
    pub allow_missing: bool,
}

struct Plan {
    manifest: Manifest,
    packed: Packed,
    meta: VersionMeta,
}

pub fn publish(ctx: &Ctx, id: Option<String>, o: PublishOpts) -> Result<()> {
    let project = ctx.project()?;
    let config = ctx.config(Some(&project))?;
    let registries = ctx.registries(&config)?;
    let url = registries.default_url().to_string();
    let reg = registries.default_registry()?;
    reg.set_revalidate(true);
    let token = match ctx.token(&config, &url) {
        Ok(t) => t,
        Err(_) if o.dry_run => Token::none(),
        Err(e) => return Err(e),
    };
    let loaded = load(&project, ctx.env, None)?;
    let ids: Vec<String> = if o.all {
        if id.is_some() {
            return Err(exit(EXIT_HUMAN, "give a package id or --all, not both"));
        }
        topological(&workspace_graph(&loaded))?
    } else {
        vec![id.or_else(|| project.current_package.clone()).ok_or_else(|| {
            exit(EXIT_HUMAN, "which package? give an id, run fpkg publish inside packages/<id>/, or use --all")
        })?]
    };
    let options = pack_options(ctx, &project)?;
    let mut plans: Vec<Plan> = Vec::new();
    let mut planned: BTreeMap<String, Version> = BTreeMap::new();
    for id in &ids {
        if loaded.installed.contains_key(id) {
            bail!("{id} was installed from a registry (it has .fpkg.toml); publish it from its own source");
        }
        let (m, loc) = checked_manifest(ctx, &project, id)?;
        if m.version_components != 3 {
            return Err(exit(
                EXIT_FAILURE,
                format!("{id}: fpkg publish requires a three-component version; write s.version = '{}' instead of '{}'", m.version, m.version_written),
            ));
        }
        let pid = PackageId::new(id.clone())?;
        if let Some(doc) = reg.package(&pid)?
            && doc.get(&m.version).is_some()
        {
            if o.all {
                ctx.out.status("Skipping", &format!("{id} {} (already on {url})", m.version));
                continue;
            }
            bail!("{id} {} is already on {url}; versions are immutable, publish a new version", m.version);
        }
        // Every dependency must exist at a satisfying version: on the
        // registry, or earlier in this run.
        for d in m.dependencies.iter().filter(|d| d.kind != DepKind::File) {
            if d.kind == DepKind::Path {
                bail!("{id}: path dependency {} cannot be published", d.id);
            }
            if planned.get(&d.id).is_some_and(|v| d.constraint.matches(v)) {
                continue;
            }
            let ok = match reg.package(&PackageId::new(d.id.clone())?)? {
                Some(doc) => doc.sorted_versions().iter().any(|(v, meta)| !meta.yanked && d.constraint.matches(v)),
                None => false,
            };
            if !ok {
                let msg = format!("{id} depends on {} {}, which is not on {url}", d.id, d.constraint);
                if o.allow_missing {
                    ctx.out.warn(&format!("{msg} (--allow-missing-dependencies)"));
                } else {
                    return Err(exit(EXIT_FAILURE, format!("{msg}; publish it first, or use --all")));
                }
            }
        }
        let packed = archive::pack(&loc.dir, id, &options)?;
        let mut meta = VersionMeta::from_manifest(&m);
        meta.allow_missing_dependencies = o.allow_missing;
        planned.insert(id.clone(), m.version.clone());
        plans.push(Plan { manifest: m, packed, meta });
    }
    if plans.is_empty() {
        ctx.out.note(&format!("nothing to publish: every version is already on {url}"));
        if ctx.out.json {
            ctx.out.json(&json!({ "published": [], "dry_run": o.dry_run }));
        }
        return Ok(());
    }

    if !ctx.out.json && !ctx.out.quiet {
        for p in &plans {
            eprintln!("\n{} {} -> {url}", p.manifest.id, p.manifest.version);
            eprintln!("  sha256 {}  ({} bytes, {} members)", p.packed.sha256, p.packed.bytes.len(), p.packed.members.len());
            for mem in &p.packed.members {
                eprintln!("    {}  {}", mem.path, mem.size);
            }
            let meta = serde_json::to_string_pretty(&p.meta.for_publish()).unwrap_or_default();
            for line in meta.lines() {
                eprintln!("  {line}");
            }
        }
        eprintln!();
    }
    if o.dry_run {
        ctx.out.note(&format!("dry run: {} package(s) not uploaded", plans.len()));
        if ctx.out.json {
            let v: Vec<_> = plans.iter().map(|p| json!({ "id": p.meta.id, "version": p.meta.version, "sha256": p.packed.sha256, "size": p.packed.bytes.len() })).collect();
            ctx.out.json(&json!({ "published": [], "dry_run": true, "would_publish": v }));
        }
        return Ok(());
    }
    if !o.yes {
        let what = if plans.len() == 1 {
            format!("{} {}", plans[0].meta.id, plans[0].meta.version)
        } else {
            format!("{} packages", plans.len())
        };
        if !ctx.confirm(&format!("Publish {what} to {url}?"))? {
            return Err(exit(EXIT_FAILURE, "publish cancelled"));
        }
    }
    let mut done = Vec::new();
    for p in &plans {
        ctx.out.status("Uploading", &format!("{} {}", p.meta.id, p.meta.version));
        let published = reg.publish(&p.meta, &p.packed.bytes, &token).with_context(|| format!("publishing {} {}", p.meta.id, p.meta.version))?;
        let page = registry::package_page(&url, &published.id);
        let docs = published.docs.clone().or_else(|| registry::docs_url(&url, &published.id, &published.version));
        ctx.out.status("Published", &format!("{} {}", published.id, published.version));
        if !ctx.out.json {
            println!("{page}");
            if let Some(d) = &docs {
                println!("{d}");
            }
        }
        done.push(json!({ "id": published.id, "version": published.version, "url": page, "docs": docs, "sha256": p.packed.sha256 }));
    }
    if ctx.out.json {
        ctx.out.json(&json!({ "published": done, "dry_run": false }));
    }
    Ok(())
}

fn id_version(spec: &str) -> Result<(PackageId, Version)> {
    match split_at(spec) {
        (id, Some(v)) => Ok((PackageId::new(id)?, Version::parse(v)?)),
        _ => Err(exit(EXIT_HUMAN, format!("expected <id>@<version>, got '{spec}'"))),
    }
}

pub fn yank(ctx: &Ctx, spec: &str, undo: bool) -> Result<()> {
    let (id, v) = id_version(spec)?;
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let registries = ctx.registries(&config)?;
    let url = registries.default_url().to_string();
    let token = ctx.token(&config, &url)?;
    registries.default_registry()?.yank(&id, &v, !undo, &token)?;
    ctx.out.status(if undo { "Unyanked" } else { "Yanked" }, &format!("{id} {v} on {url}"));
    if ctx.out.json {
        ctx.out.json(&json!({ "id": id, "version": v.to_string(), "yanked": !undo }));
    }
    Ok(())
}

pub fn owner(ctx: &Ctx, action: OwnerCmd) -> Result<()> {
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let registries = ctx.registries(&config)?;
    let url = registries.default_url().to_string();
    let reg = registries.default_registry()?;
    match action {
        OwnerCmd::List { id } => {
            let owners = reg.owners(&PackageId::new(id)?)?;
            if ctx.out.json {
                ctx.out.json(&json!(owners));
            } else {
                for o in owners {
                    println!("{o}");
                }
            }
        }
        OwnerCmd::Add { id, login } | OwnerCmd::Remove { id, login } if login.trim().is_empty() => {
            return Err(exit(EXIT_HUMAN, format!("give a login to change the owners of {id}")));
        }
        OwnerCmd::Add { id, login } => {
            let token = ctx.token(&config, &url)?;
            reg.set_owner(&PackageId::new(id.clone())?, &login, true, &token)?;
            ctx.out.status("Added", &format!("{login} as an owner of {id}"));
        }
        OwnerCmd::Remove { id, login } => {
            let token = ctx.token(&config, &url)?;
            reg.set_owner(&PackageId::new(id.clone())?, &login, false, &token)?;
            ctx.out.status("Removed", &format!("{login} as an owner of {id}"));
        }
    }
    Ok(())
}

/// Reads a line from stdin, without echo on a terminal.
fn read_secret() -> Result<String> {
    let tty = std::io::stdin().is_terminal();
    let stty = |arg: &str| {
        let _ = std::process::Command::new("stty").arg(arg).stdin(std::process::Stdio::inherit()).status();
    };
    if tty {
        stty("-echo");
    }
    let mut line = String::new();
    let r = std::io::stdin().lock().read_line(&mut line);
    if tty {
        stty("echo");
        eprintln!();
    }
    r?;
    Ok(line.trim().to_string())
}

pub fn login(ctx: &Ctx) -> Result<()> {
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let url = registry::normalize_url(&config.registry)?;
    if url.starts_with("file:") {
        ctx.out.note("file:// registries need no login");
        return Ok(());
    }
    fpkg_core::auth::check_transport(&url)?;
    let tokens = format!("{url}tokens");
    if std::io::stdin().is_terminal() {
        eprintln!("Create a token at {tokens} and paste it here:");
        if !ctx.out.json {
            open_url(&ctx.out, &tokens);
        }
    }
    let token = Token::new(read_secret()?);
    if token.is_empty() {
        return Err(exit(EXIT_HUMAN, "no token given"));
    }
    let reg = registry::HttpRegistry::new(&url, &config.net())?;
    let me = reg.me(&token).map_err(|e| anyhow!("{url} did not accept the token: {e}"))?;
    let path = config.credentials_path();
    let mut creds = Credentials::load(&path)?;
    creds.set(&url, &token, Some(me.login.clone()));
    creds.save(&path)?;
    let name = me.token_name.as_deref().map(|n| format!(" (token '{n}')")).unwrap_or_default();
    ctx.out.status("Logged in", &format!("to {url} as {}{name}", me.login));
    if ctx.out.json {
        ctx.out.json(&json!({ "registry": url, "login": me.login, "token_name": me.token_name }));
    }
    Ok(())
}

pub fn logout(ctx: &Ctx) -> Result<()> {
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let url = registry::normalize_url(&config.registry)?;
    let path = config.credentials_path();
    let mut creds = Credentials::load(&path)?;
    if creds.remove(&url) {
        creds.save(&path)?;
        ctx.out.status("Logged out", &format!("of {url}"));
    } else {
        ctx.out.note(&format!("no token stored for {url}"));
    }
    if std::env::var_os("FPKG_TOKEN").is_some() {
        ctx.out.warn("FPKG_TOKEN is set in the environment and still applies");
    }
    Ok(())
}

pub fn docs(ctx: &Ctx, spec: &str) -> Result<()> {
    let (id, version) = split_at(spec);
    let pid = PackageId::new(id)?;
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let registries = ctx.registries(&config)?;
    let url = registries.default_url().to_string();
    let reg = registries.default_registry()?;
    let doc = reg.package(&pid)?.ok_or_else(|| anyhow!("{id} is not on {url}"))?;
    let v = match version {
        Some(v) => Version::parse(v)?,
        None => doc.newest().ok_or_else(|| anyhow!("{id} has no versions that are not yanked"))?,
    };
    let meta = doc.get(&v).ok_or_else(|| anyhow!("{id} {v} is not on {url}"))?;
    let docs = meta
        .docs
        .clone()
        .or_else(|| registry::docs_url(&url, id, &meta.version))
        .ok_or_else(|| anyhow!("{url} has no documentation host"))?;
    if ctx.out.json {
        ctx.out.json(&json!({ "id": id, "version": meta.version, "url": docs }));
    } else {
        println!("{docs}");
        open_url(&ctx.out, &docs);
    }
    Ok(())
}

pub fn cache(ctx: &Ctx, action: CacheCmd) -> Result<()> {
    let project = ctx.project().ok();
    let config = ctx.config(project.as_ref())?;
    let dir = &config.cache_dir;
    match action {
        CacheCmd::Dir => {
            if ctx.out.json {
                ctx.out.json(&json!({ "dir": dir }));
            } else {
                println!("{}", dir.display());
            }
        }
        CacheCmd::Clean => {
            if dir.exists() {
                fs_err::remove_dir_all(dir)?;
                ctx.out.status("Removed", &dir.display().to_string());
            } else {
                ctx.out.note(&format!("{} does not exist", dir.display()));
            }
        }
    }
    Ok(())
}

pub fn mirror(ctx: &Ctx, registry_url: &str, dir: &Path) -> Result<()> {
    let project = ctx.project()?;
    let lock = Lockfile::read(&project.lock_path())?.ok_or_else(|| anyhow!("there is no fpkg.lock to mirror (run fpkg lock)"))?;
    let config = ctx.config(Some(&project))?;
    let registries = registry::Registries::new(registry_url, config.cache_dir.clone(), config.net(), config.offline)?;
    let source = registries.default_registry()?;
    fs_err::create_dir_all(dir)?;
    let dest = FileRegistry::new(dir);
    let mut n = 0;
    for p in &lock.packages {
        let (Source::Registry(_), Some(sha)) = (&p.source, &p.sha256) else { continue };
        let pid = PackageId::new(p.id.clone())?;
        let doc = source.package(&pid)?.ok_or_else(|| anyhow!("{} is not on {registry_url}", p.id))?;
        let meta = doc.get(&p.version).ok_or_else(|| anyhow!("{} {} is not on {registry_url}", p.id, p.version))?;
        let bytes = source.archive_verified(&pid, &p.version, sha)?;
        dest.import(meta, &bytes, &doc.owners)?;
        ctx.out.status("Mirrored", &format!("{} {}", p.id, p.version));
        n += 1;
    }
    ctx.out.note(&format!("{n} packages in {}; install from it with fpkg install --registry {}", dir.display(), dest.url()));
    if ctx.out.json {
        ctx.out.json(&json!({ "dir": dir, "url": dest.url(), "packages": n }));
    }
    Ok(())
}

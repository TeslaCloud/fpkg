//! lock, update, add, remove, install, verify.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use fpkg_core::install::{self, Event, InstallOptions, load};
use fpkg_core::lockfile::Lockfile;
use fpkg_core::project::Project;
use fpkg_core::registry::{Registries, Registry};
use fpkg_core::resolve::Release;
use fpkg_core::spec::{self, DepKind, Dependency, SPEC_FILE};
use fpkg_core::{Constraint, PackageId};
use serde_json::json;

use super::{Ctx, exit, exit_silent, split_at};
use crate::edit::{self, EditError};
use crate::{EXIT_HUMAN, EXIT_VERIFY};

fn registries_for(ctx: &Ctx, project: &Project) -> Result<Registries> {
    let config = ctx.config(Some(project))?;
    let r = ctx.registries(&config)?;
    r.default_registry()?.set_revalidate(true);
    Ok(r)
}

fn print_changes(ctx: &Ctx, old: Option<&Lockfile>, new: &Lockfile) {
    for p in &new.packages {
        match old.and_then(|o| o.get(&p.id)) {
            None => ctx.out.status("Locking", &format!("{} {}", p.id, p.version)),
            Some(o) if o.version != p.version => ctx.out.status("Updating", &format!("{} {} -> {}", p.id, o.version, p.version)),
            Some(o) if o.source != p.source => ctx.out.status("Updating", &format!("{} {} ({} -> {})", p.id, p.version, o.source, p.source)),
            Some(_) => {}
        }
    }
    if let Some(old) = old {
        for p in &old.packages {
            if new.get(&p.id).is_none() {
                ctx.out.status("Removing", &format!("{} {}", p.id, p.version));
            }
        }
    }
}

fn lock_json(lock: &Lockfile) -> serde_json::Value {
    let v: Vec<_> = lock
        .packages
        .iter()
        .map(|p| json!({ "id": p.id, "version": p.version.to_string(), "source": p.source.to_string(), "sha256": p.sha256, "dependencies": p.dependencies }))
        .collect();
    json!({ "version": fpkg_core::lockfile::LOCK_VERSION, "packages": v })
}

/// Resolves (keeping what `release` does not release) and writes the lock.
/// Unmarked directories count as workspace packages: locking is how a hand
/// copy becomes a deliberate shadow of a registry package.
fn relock(ctx: &Ctx, project: &Project, registries: &Registries, release: Release) -> Result<Lockfile> {
    let old = Lockfile::read(&project.lock_path())?;
    let loaded = load(project, ctx.env, None)?;
    let new = loaded.resolve(old.as_ref(), release, registries)?;
    print_changes(ctx, old.as_ref(), &new);
    if old.as_ref() != Some(&new) || !project.lock_path().exists() {
        new.write(&project.lock_path())?;
    }
    Ok(new)
}

pub fn lock(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    let registries = registries_for(ctx, &project)?;
    let new = relock(ctx, &project, &registries, Release::None)?;
    if ctx.out.json {
        ctx.out.json(&lock_json(&new));
    } else {
        ctx.out.note(&format!("fpkg.lock: {} packages", new.packages.len()));
    }
    Ok(())
}

pub fn update(ctx: &Ctx, ids: &[String]) -> Result<()> {
    let project = ctx.project()?;
    let registries = registries_for(ctx, &project)?;
    let release = if ids.is_empty() {
        Release::All
    } else {
        let old = Lockfile::read(&project.lock_path())?;
        for id in ids {
            if old.as_ref().and_then(|l| l.get(id)).is_none() {
                bail!("{id} is not in fpkg.lock");
            }
        }
        Release::Ids(ids.iter().cloned().collect::<BTreeSet<_>>())
    };
    let new = relock(ctx, &project, &registries, release)?;
    if ctx.out.json {
        ctx.out.json(&lock_json(&new));
    }
    Ok(())
}

fn install_with(ctx: &Ctx, project: &Project, registries: &Registries, opts: InstallOptions) -> Result<install::InstallReport> {
    let out = ctx.out;
    let mut progress = |e: Event| match e {
        Event::Resolving => out.status("Resolving", "dependencies"),
        Event::LockWritten => out.status("Locking", "fpkg.lock"),
        Event::Fetching { id, version } => out.status("Installing", &format!("{id} {version}")),
        Event::Removing { id, version } => out.status("Removing", &format!("{id} {version} (no longer in fpkg.lock)")),
        Event::Replacing { id } => out.warn(&format!("replacing packages/{id}, which has no .fpkg.toml marker (--force)")),
        Event::Repairing { id, version } => {
            out.status("Repairing", &format!("{id} {version} (its files no longer match the install)"))
        }
        Event::Restored { id } => out.status("Restored", &format!("packages/{id} from an interrupted install")),
    };
    let report = install::install(project, opts, registries, &mut progress)?;
    for s in &report.stale {
        out.verbose(&format!("fpkg.lock was stale: {s}"));
    }
    Ok(report)
}

fn print_report(ctx: &Ctx, r: &install::InstallReport) {
    if ctx.out.json {
        let pairs = |v: &[(String, fpkg_core::Version)]| v.iter().map(|(i, v)| json!({ "id": i, "version": v.to_string() })).collect::<Vec<_>>();
        let updated: Vec<_> =
            r.updated.iter().map(|(i, a, b)| json!({ "id": i, "from": a.to_string(), "to": b.to_string() })).collect();
        ctx.out.json(&json!({
            "added": pairs(&r.added), "updated": updated, "repaired": pairs(&r.repaired),
            "removed": pairs(&r.removed), "unchanged": pairs(&r.unchanged), "lock_written": r.lock_written,
        }));
        return;
    }
    let repaired = if r.repaired.is_empty() { String::new() } else { format!("{} repaired, ", r.repaired.len()) };
    ctx.out.note(&format!(
        "{} added, {} updated, {repaired}{} removed, {} unchanged",
        r.added.len(),
        r.updated.len(),
        r.removed.len(),
        r.unchanged.len()
    ));
}

pub fn install(ctx: &Ctx, locked: bool, force: bool) -> Result<()> {
    let project = ctx.project()?;
    let config = ctx.config(Some(&project))?;
    let mut registries = ctx.registries(&config)?;
    // An explicit --registry also serves the archives of every lock entry: a
    // mirror for an air-gapped server. The lock's checksums still decide.
    if let Some(url) = &ctx.registry_flag {
        registries = registries.with_mirror(url)?;
    }
    let report = install_with(ctx, &project, &registries, InstallOptions { locked, force, env: ctx.env })?;
    print_report(ctx, &report);
    Ok(())
}

pub fn verify(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    let report = install::verify(&project, ctx.env)?;
    if ctx.out.json {
        let ok: Vec<_> = report.ok.iter().map(|(i, v)| json!({ "id": i, "version": v.to_string() })).collect();
        let problems: Vec<_> = report.problems.iter().map(|p| json!({ "id": p.id, "message": p.message })).collect();
        ctx.out.json(&json!({ "ok": ok, "problems": problems }));
    } else {
        for (id, v) in &report.ok {
            ctx.out.verbose(&format!("ok {id} {v}"));
        }
        for p in &report.problems {
            ctx.out.error(&format!("{} {}", p.id, p.message));
        }
        if report.problems.is_empty() {
            ctx.out.note(&format!("{} packages match fpkg.lock", report.ok.len()));
        }
    }
    if report.problems.is_empty() {
        Ok(())
    } else {
        Err(exit(EXIT_VERIFY, format!("{} of {} packages do not match fpkg.lock", report.problems.len(), report.problems.len() + report.ok.len())))
    }
}

fn manual_edit(ctx: &Ctx, project: &Project, e: &EditError, what: &str, line: &str) -> anyhow::Error {
    ctx.out.error(&format!("cannot edit {} automatically: {e}", project.manifest_path().display()));
    eprintln!("{what} inside Package:describe(function(s) ... end):\n\n    {line}\n");
    exit_silent(EXIT_HUMAN)
}

/// Writes the edited manifest if it evaluates.
fn write_manifest(ctx: &Ctx, project: &Project, new: &str) -> Result<()> {
    let path = project.manifest_path();
    // Evaluate the edited text first, from a temporary file beside it.
    let tmp = tempfile::NamedTempFile::new_in(&project.root)?;
    fs_err::write(tmp.path(), new)?;
    if let Err(e) = spec::load_file(&project.root_location(), ctx.env, tmp.path()) {
        bail!("the edited {SPEC_FILE} would not evaluate, so it was left as it was: {e}");
    }
    fs_err::write(&path, new)?;
    Ok(())
}

pub fn add(ctx: &Ctx, spec_arg: &str) -> Result<()> {
    let (id, constraint) = split_at(spec_arg);
    PackageId::new(id)?;
    let project = ctx.project()?;
    let loaded = load(&project, ctx.env, None)?;
    if loaded.root.dependencies.iter().any(|d| d.kind == DepKind::Package && d.id == id) {
        bail!("the project already depends on {id}");
    }
    let registries = registries_for(ctx, &project)?;
    let constraint = match constraint {
        Some(c) => Constraint::parse(c)?,
        None => {
            let version = match loaded.workspace.get(id) {
                Some(m) => m.version.clone(),
                None => {
                    let reg = registries.default_registry()?;
                    let doc = reg
                        .package(&PackageId::new(id)?)?
                        .ok_or_else(|| anyhow!("{id} is not a workspace package and not on {}", registries.default_url()))?;
                    doc.newest().ok_or_else(|| anyhow!("{id} has no versions that are not yanked"))?
                }
            };
            Constraint::default_for(&version)
        }
    };
    let c = constraint.to_string();
    let src = fs_err::read_to_string(project.manifest_path())?;
    let line = edit::depends_line("s", "", '\'', id, Some(&c));
    let new = match edit::add(&src, id, Some(&c)) {
        Ok(n) => n,
        Err(e @ EditError::AlreadyPresent(_)) => bail!("{e}"),
        Err(e) => return Err(manual_edit(ctx, &project, &e, "Add this line", &line)),
    };
    // Resolve with the dependency added in memory; the manifest is written
    // only once that succeeded.
    let old = Lockfile::read(&project.lock_path())?;
    let mut with = loaded.clone();
    with.root.dependencies.push(Dependency { id: id.to_string(), constraint: constraint.clone(), kind: DepKind::Package });
    let new_lock = with.resolve(old.as_ref(), Release::None, &registries)?;
    write_manifest(ctx, &project, &new)?;
    ctx.out.status("Adding", &format!("{id} {c} to {SPEC_FILE}"));
    print_changes(ctx, old.as_ref(), &new_lock);
    new_lock.write(&project.lock_path())?;
    let report = install_with(ctx, &project, &registries, InstallOptions { locked: false, force: false, env: ctx.env })?;
    print_report(ctx, &report);
    Ok(())
}

pub fn remove(ctx: &Ctx, id: &str) -> Result<()> {
    PackageId::new(id)?;
    let project = ctx.project()?;
    let src = fs_err::read_to_string(project.manifest_path())?;
    let new = match edit::remove(&src, id) {
        Ok(n) => n,
        Err(e @ EditError::NotPresent(_)) => bail!("{e}"),
        Err(e) => return Err(manual_edit(ctx, &project, &e, "Remove the s.depends line for", id)),
    };
    // As for add: resolve without the dependency first, write afterwards.
    let loaded = load(&project, ctx.env, None)?;
    let registries = registries_for(ctx, &project)?;
    let old = Lockfile::read(&project.lock_path())?;
    let mut without = loaded.clone();
    without.root.dependencies.retain(|d| !(d.kind != DepKind::File && d.package_id() == Some(id)));
    let new_lock = without.resolve(old.as_ref(), Release::None, &registries)?;
    write_manifest(ctx, &project, &new)?;
    ctx.out.status("Removing", &format!("{id} from {SPEC_FILE}"));
    print_changes(ctx, old.as_ref(), &new_lock);
    new_lock.write(&project.lock_path())?;
    let report = install_with(ctx, &project, &registries, InstallOptions { locked: false, force: false, env: ctx.env })?;
    print_report(ctx, &report);
    Ok(())
}

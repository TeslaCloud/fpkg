//! lock, update, add, remove, install, verify.

use std::collections::BTreeSet;

use anyhow::{Context, Result, anyhow, bail};
use fpkg_core::install::{self, Event, InstallOptions, Loaded, load};
use fpkg_core::lockfile::Lockfile;
use fpkg_core::project::Project;
use fpkg_core::registry::{Registries, Registry};
use fpkg_core::resolve::Release;
use fpkg_core::spec::{self, DepKind, Manifest, SPEC_FILE};
use fpkg_core::{Constraint, PackageId};
use serde_json::json;

use super::{Ctx, exit, exit_silent, split_at, usage_id};
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
            None => ctx
                .out
                .status("Locking", &format!("{} {}", p.id, p.version)),
            Some(o) if o.version != p.version => ctx.out.status(
                "Updating",
                &format!("{} {} -> {}", p.id, o.version, p.version),
            ),
            Some(o) if o.source != p.source => ctx.out.status(
                "Updating",
                &format!("{} {} ({} -> {})", p.id, p.version, o.source, p.source),
            ),
            Some(o) if o.sha256 != p.sha256 => ctx.out.status(
                "Updating",
                &format!(
                    "{} {} (checksum {} -> {})",
                    p.id,
                    p.version,
                    o.sha256.as_deref().unwrap_or("none"),
                    p.sha256.as_deref().unwrap_or("none")
                ),
            ),
            Some(_) => {}
        }
    }
    if let Some(old) = old {
        for p in &old.packages {
            if new.get(&p.id).is_none() {
                ctx.out
                    .status("Removing", &format!("{} {}", p.id, p.version));
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
/// Returns the lock as it was and the new one.
fn relock(
    ctx: &Ctx,
    project: &Project,
    registries: &Registries,
    release: Release,
) -> Result<(Option<Lockfile>, Lockfile)> {
    let old = Lockfile::read(&project.lock_path())?;
    let loaded = load(project, ctx.env, None)?;
    let new = loaded.resolve(old.as_ref(), release, registries)?;
    print_changes(ctx, old.as_ref(), &new);
    if old.as_ref() != Some(&new) || !project.lock_path().exists() {
        new.write(&project.lock_path())?;
    }
    Ok((old, new))
}

pub fn lock(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    let registries = registries_for(ctx, &project)?;
    let (_, new) = relock(ctx, &project, &registries, Release::None)?;
    if ctx.out.json {
        ctx.out.json(&lock_json(&new));
    } else {
        ctx.out
            .note(&format!("fpkg.lock: {} packages", new.packages.len()));
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
                return Err(exit(EXIT_HUMAN, format!("{id} is not in fpkg.lock")));
            }
        }
        Release::Ids(ids.iter().cloned().collect::<BTreeSet<_>>())
    };
    let (old, new) = relock(ctx, &project, &registries, release)?;
    if ctx.out.json {
        ctx.out.json(&lock_json(&new));
    } else if old.as_ref() == Some(&new) {
        ctx.out.note(
            "fpkg.lock is up to date: the locked versions are the newest the constraints allow",
        );
    } else {
        ctx.out.note("next: fpkg install");
    }
    Ok(())
}

fn install_with(
    ctx: &Ctx,
    project: &Project,
    registries: &Registries,
    opts: InstallOptions,
) -> Result<install::InstallReport> {
    let out = ctx.out;
    let mut progress = |e: Event| {
        match e {
        Event::Resolving => out.status("Resolving", "dependencies"),
        Event::LockWritten => out.status("Locking", "fpkg.lock"),
        Event::Fetching { id, version } => out.status("Installing", &format!("{id} {version}")),
        Event::Removing { id, version, replaced_by: Some(dir) } => out.status("Removing", &format!("{id} {version} (replaced by {dir})")),
        Event::Removing { id, version, replaced_by: None } => out.status("Removing", &format!("{id} {version} (no longer in fpkg.lock)")),
        Event::Replacing { id } => out.warn(&format!("replacing packages/{id}, which has no .fpkg.toml marker (--force)")),
        Event::Repairing { id, version } => {
            out.status("Repairing", &format!("{id} {version} (its files no longer match the install)"))
        }
        Event::Restored { id } => out.status("Restored", &format!("packages/{id} from an interrupted install")),
        Event::Writing { path, package } => out.verbose(&format!("copying {path} ({package})")),
        Event::RemovingFile { path } => out.verbose(&format!("removing {path} (no package provides it any more)")),
        Event::KeptModified { path } => {
            out.warn(&format!("{path} is no longer provided by any package but was changed after install; left in place"))
        }
        Event::SkippedSymlink { path } => out.warn(&format!("{} is a symlink; not copied", path.display())),
        Event::OtherPlatform { name, package } => out.verbose(&format!("not copying {name} ({package}): the server's binaries are for another platform")),
        Event::CreatedDir { path } => out.status("Created", &format!("{}, where Flux writes its generated files", path.display())),
    }
    };
    let report = install::install(project, opts, registries, &mut progress)?;
    for s in &report.stale {
        out.verbose(&format!("fpkg.lock was stale: {s}"));
    }
    if !report.binaries_skipped.is_empty() {
        let mut per_package: std::collections::BTreeMap<&str, usize> = Default::default();
        for (package, _) in &report.binaries_skipped {
            *per_package.entry(package).or_default() += 1;
        }
        let list = per_package
            .iter()
            .map(|(p, n)| format!("{p} ({n} {})", if *n == 1 { "module" } else { "modules" }))
            .collect::<Vec<_>>()
            .join(", ");
        out.note(&format!(
            "note: the server folder is not known, so the binary modules of {list} were not copied to garrysmod/lua/bin/\n      (fpkg install -v names them); run fpkg install in the server folder, or pass --server <dir>"
        ));
        for (package, name) in &report.binaries_skipped {
            out.verbose(&format!("not copied: {name} ({package})"));
        }
    }
    Ok(report)
}

fn print_report(ctx: &Ctx, r: &install::InstallReport) {
    if ctx.out.json {
        let pairs = |v: &[(String, fpkg_core::Version)]| {
            v.iter()
                .map(|(i, v)| json!({ "id": i, "version": v.to_string() }))
                .collect::<Vec<_>>()
        };
        let updated: Vec<_> = r
            .updated
            .iter()
            .map(|(i, a, b)| json!({ "id": i, "from": a.to_string(), "to": b.to_string() }))
            .collect();
        let skipped: Vec<_> = r
            .binaries_skipped
            .iter()
            .map(|(p, n)| json!({ "package": p, "file": n }))
            .collect();
        ctx.emit_json(json!({
            "added": pairs(&r.added), "updated": updated, "repaired": pairs(&r.repaired),
            "removed": pairs(&r.removed), "unchanged": pairs(&r.unchanged), "local": pairs(&r.local), "lock_written": r.lock_written,
            "files_written": r.files_written, "files_removed": r.files_removed, "files_kept": r.files_kept,
            "binaries_skipped": skipped,
        }));
        return;
    }
    let repaired = if r.repaired.is_empty() {
        String::new()
    } else {
        format!("{} repaired, ", r.repaired.len())
    };
    let local = if r.local.is_empty() {
        String::new()
    } else {
        format!(", {} local", r.local.len())
    };
    ctx.out.note(&format!(
        "{} added, {} updated, {repaired}{} removed, {} unchanged{local}",
        r.added.len(),
        r.updated.len(),
        r.removed.len(),
        r.unchanged.len()
    ));
    if !r.files_written.is_empty() || !r.files_removed.is_empty() {
        ctx.out.note(&format!(
            "{} files copied, {} removed",
            r.files_written.len(),
            r.files_removed.len()
        ));
    }
}

pub fn install(ctx: &Ctx, locked: bool, force: bool) -> Result<()> {
    ctx.for_each_project(false, &mut |p| install_one(ctx, p, locked, force))
}

fn install_one(ctx: &Ctx, project: &Project, locked: bool, force: bool) -> Result<()> {
    let project = project.clone();
    let config = ctx.config(Some(&project))?;
    let mut registries = ctx.registries(&config)?;
    // An explicit --registry also serves the archives of every lock entry: a
    // mirror for an air-gapped server. The lock's checksums still decide.
    if let Some(url) = &ctx.registry_flag {
        registries = registries.with_mirror(url)?;
    }
    let report = install_with(
        ctx,
        &project,
        &registries,
        InstallOptions {
            locked,
            force,
            env: ctx.env,
        },
    )?;
    print_report(ctx, &report);
    Ok(())
}

pub fn verify(ctx: &Ctx) -> Result<()> {
    ctx.for_each_project(false, &mut |p| verify_one(ctx, p))
}

fn verify_one(ctx: &Ctx, project: &Project) -> Result<()> {
    let report = install::verify(project, ctx.env)?;
    if !report.bin_unchecked.is_empty() {
        ctx.out.note(&format!(
            "{} recorded binary modules not checked: the server folder is not known (run fpkg verify inside it, or pass --server <dir>)",
            report.bin_unchecked.len()
        ));
    }
    if ctx.out.json {
        let ok: Vec<_> = report
            .ok
            .iter()
            .map(|(i, v)| json!({ "id": i, "version": v.to_string() }))
            .collect();
        let problems: Vec<_> = report
            .problems
            .iter()
            .map(|p| json!({ "id": p.id, "message": p.message }))
            .collect();
        ctx.emit_json(json!({ "ok": ok, "problems": problems, "files_ok": report.files_ok, "bin_unchecked": report.bin_unchecked }));
    } else {
        for (id, v) in &report.ok {
            ctx.out.verbose(&format!("ok {id} {v}"));
        }
        for p in &report.problems {
            ctx.out.error(&format!("{} {}", p.id, p.message));
        }
        if report.problems.is_empty() {
            let files = if report.files_ok > 0 {
                format!(", {} copied files match", report.files_ok)
            } else {
                String::new()
            };
            ctx.out.note(&format!(
                "{} packages match fpkg.lock{files}",
                report.ok.len()
            ));
        }
    }
    if report.problems.is_empty() {
        Ok(())
    } else {
        Err(exit(
            EXIT_VERIFY,
            format!(
                "{} of {} packages do not match fpkg.lock",
                report.problems.len(),
                report.problems.len() + report.ok.len()
            ),
        ))
    }
}

fn manual_edit(
    ctx: &Ctx,
    project: &Project,
    e: &dyn std::fmt::Display,
    what: &str,
    line: &str,
) -> anyhow::Error {
    ctx.out.error(&format!(
        "cannot edit {} automatically: {e}",
        project.manifest_path().display()
    ));
    eprintln!("{what} inside Package:describe(function(s) ... end):\n\n    {line}\n");
    exit_silent(EXIT_HUMAN)
}

/// What the edited manifest text evaluates to, from a temporary file beside
/// the manifest; the manifest itself is not touched.
fn evaluate_edit(ctx: &Ctx, project: &Project, new: &str) -> Result<Manifest> {
    let tmp = tempfile::NamedTempFile::new_in(&project.root)?;
    fs_err::write(tmp.path(), new)?;
    spec::load_file(&project.root_location(), ctx.env, tmp.path()).map_err(|e| {
        anyhow!("the edited {SPEC_FILE} would not evaluate, so it was left as it was: {e}")
    })
}

/// Replaces the manifest atomically, keeping its permissions.
fn write_manifest(project: &Project, new: &str) -> Result<()> {
    let path = project.manifest_path();
    let permissions = fs_err::metadata(&path)?.permissions();
    fpkg_core::lockfile::write_atomic(&path, new.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    fs_err::set_permissions(&path, permissions)?;
    Ok(())
}

/// The end of `add` and `remove`: the manifest, then the lock, then install.
fn write_and_install(
    ctx: &Ctx,
    project: &Project,
    registries: &Registries,
    manifest: &str,
    old: Option<&Lockfile>,
    lock: &Lockfile,
) -> Result<()> {
    write_manifest(project, manifest)?;
    print_changes(ctx, old, lock);
    lock.write(&project.lock_path())?;
    let report = install_with(
        ctx,
        project,
        registries,
        InstallOptions {
            locked: false,
            force: false,
            env: ctx.env,
        },
    )?;
    print_report(ctx, &report);
    Ok(())
}

pub fn add(ctx: &Ctx, spec_arg: &str) -> Result<()> {
    let (id, constraint) = split_at(spec_arg);
    usage_id(id)?;
    let project = ctx.project()?;
    let loaded = load(&project, ctx.env, None)?;
    let existing = loaded
        .root
        .dependencies
        .iter()
        .find(|d| d.kind == DepKind::Package && d.id == id)
        .map(|d| d.constraint.clone());
    let registries = registries_for(ctx, &project)?;
    let constraint = match (constraint, &existing) {
        (None, Some(c)) => {
            return Err(exit(
                EXIT_HUMAN,
                format!(
                    "the project already depends on {id} {c}; fpkg add {id}@<constraint> changes the constraint"
                ),
            ));
        }
        (Some(c), _) => Constraint::parse(c).map_err(|e| exit(EXIT_HUMAN, e.to_string()))?,
        (None, None) => {
            let version = match loaded.workspace.get(id) {
                Some(m) => m.version.clone(),
                None => {
                    let reg = registries.default_registry()?;
                    let doc = reg.package(&PackageId::new(id)?)?.ok_or_else(|| {
                        anyhow!(
                            "{id} is not a workspace package and not on {}",
                            registries.default_url()
                        )
                    })?;
                    doc.newest()
                        .ok_or_else(|| anyhow!("{id} has no versions that are not yanked"))?
                }
            };
            Constraint::default_for(&version)
        }
    };
    let c = constraint.to_string();
    if existing.as_ref() == Some(&constraint) {
        ctx.out
            .note(&format!("the project already depends on {id} {c}"));
        return Ok(());
    }
    let src = fs_err::read_to_string(project.manifest_path())?;
    let line = edit::depends_line("s", "", '\'', id, Some(&c));
    let edited = match &existing {
        Some(_) => edit::set_constraint(&src, id, &c),
        None => edit::add(&src, id, Some(&c)),
    };
    let new = match edited {
        Ok(n) => n,
        Err(e @ EditError::AlreadyPresent(_)) => bail!("{e}"),
        Err(e) => return Err(manual_edit(ctx, &project, &e, "Add this line", &line)),
    };
    // Resolve what the edited text evaluates to; the manifest is written
    // only once that succeeded.
    let root = evaluate_edit(ctx, &project, &new)?;
    if !root
        .dependencies
        .iter()
        .any(|d| d.kind == DepKind::Package && d.id == id && d.constraint == constraint)
    {
        let why = format!(
            "the edited file does not depend on {id} {c} when it is evaluated (is the line inside a block comment?)"
        );
        return Err(manual_edit(ctx, &project, &why, "Add this line", &line));
    }
    let old = Lockfile::read(&project.lock_path())?;
    let with = Loaded { root, ..loaded };
    let new_lock = with.resolve(old.as_ref(), Release::None, &registries)?;
    match &existing {
        Some(before) => ctx
            .out
            .status("Changing", &format!("{id} {before} -> {c} in {SPEC_FILE}")),
        None => ctx
            .out
            .status("Adding", &format!("{id} {c} to {SPEC_FILE}")),
    }
    write_and_install(ctx, &project, &registries, &new, old.as_ref(), &new_lock)
}

pub fn remove(ctx: &Ctx, id: &str) -> Result<()> {
    usage_id(id)?;
    let project = ctx.project()?;
    let src = fs_err::read_to_string(project.manifest_path())?;
    let new = match edit::remove(&src, id) {
        Ok(n) => n,
        Err(e @ EditError::NotPresent(_)) => return Err(exit(EXIT_HUMAN, e.to_string())),
        Err(e) => {
            return Err(manual_edit(
                ctx,
                &project,
                &e,
                "Remove the s.depends line for",
                id,
            ));
        }
    };
    // As for add: resolve what the edited text evaluates to, write afterwards.
    let loaded = load(&project, ctx.env, None)?;
    let root = evaluate_edit(ctx, &project, &new)?;
    if root
        .dependencies
        .iter()
        .any(|d| d.kind != DepKind::File && d.package_id() == Some(id))
    {
        let why = format!("the edited file still depends on {id} when it is evaluated");
        return Err(manual_edit(
            ctx,
            &project,
            &why,
            "Remove the s.depends line for",
            id,
        ));
    }
    let registries = registries_for(ctx, &project)?;
    let old = Lockfile::read(&project.lock_path())?;
    let without = Loaded { root, ..loaded };
    let new_lock = without.resolve(old.as_ref(), Release::None, &registries)?;
    ctx.out
        .status("Removing", &format!("{id} from {SPEC_FILE}"));
    write_and_install(ctx, &project, &registries, &new, old.as_ref(), &new_lock)
}

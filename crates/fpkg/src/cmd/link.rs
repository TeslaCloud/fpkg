//! link, unlink: local checkouts in place of registry packages.

use std::path::Path;

use anyhow::Result;
use fpkg_core::PackageId;
use fpkg_core::project::{self, Marker, PACKAGES_DIR};
use fpkg_core::spec::{self, SPEC_FILE};
use serde_json::json;

use super::{Ctx, exit, usage_id};
use crate::EXIT_HUMAN;
use crate::output::table;

/// `fpkg link <dir> [--as <id>] [--force]`.
pub fn link(ctx: &Ctx, dir: &Path, as_id: Option<String>, force: bool) -> Result<()> {
    let project = ctx.project()?;
    let given = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        project::normalize(&super::cwd()?.join(dir))
    };
    let target = given
        .canonicalize()
        .map_err(|e| exit(EXIT_HUMAN, format!("cannot link {}: {e}", given.display())))?;
    if !target.is_dir() || !target.join(SPEC_FILE).is_file() {
        return Err(exit(
            EXIT_HUMAN,
            format!(
                "{} has no {SPEC_FILE}; link a package checkout",
                target.display()
            ),
        ));
    }
    if target == project.root {
        return Err(exit(EXIT_HUMAN, "that is this project's own directory"));
    }
    let id = match as_id {
        Some(id) => id,
        None => project::project_id(&target),
    };
    PackageId::new(id.clone())
        .map_err(|e| exit(EXIT_HUMAN, format!("{e}; name the package with --as <id>")))?;
    if id == project.id {
        return Err(exit(
            EXIT_HUMAN,
            format!("'{id}' is the id of this project itself; name the package with --as <id>"),
        ));
    }
    let link = project.package_dir(&id);
    let shown = format!("{PACKAGES_DIR}/{id}");
    enum Replace {
        Nothing,
        Link,
        Installed,
    }
    let replace = match std::fs::symlink_metadata(&link) {
        Err(_) => Replace::Nothing,
        Ok(meta) if meta.file_type().is_symlink() => {
            let current = std::fs::read_link(&link).unwrap_or_default();
            if link.canonicalize().is_ok_and(|c| c == target) && !force {
                ctx.out
                    .note(&format!("{shown} already links to {}", current.display()));
                return Ok(());
            }
            if !force {
                return Err(exit(
                    EXIT_HUMAN,
                    format!(
                        "{shown} already links to {}; fpkg link --force replaces the link",
                        current.display()
                    ),
                ));
            }
            Replace::Link
        }
        Ok(meta) if meta.is_dir() => match Marker::read(&link)? {
            Some(m) if force => {
                ctx.out.verbose(&format!(
                    "removing {id} {} installed from {}",
                    m.version, m.registry
                ));
                Replace::Installed
            }
            Some(m) => {
                return Err(exit(
                    EXIT_HUMAN,
                    format!(
                        "{shown} is {id} {} installed from {}; fpkg link --force removes it and links the checkout instead",
                        m.version, m.registry
                    ),
                ));
            }
            None => {
                return Err(exit(
                    EXIT_HUMAN,
                    format!(
                        "{shown} is a workspace package directory, not a link; fpkg never deletes one: move it away first"
                    ),
                ));
            }
        },
        Ok(_) => {
            return Err(exit(
                EXIT_HUMAN,
                format!("{shown} exists and is not a directory; move it away first"),
            ));
        }
    };

    // The checkout's spec must evaluate where it will be.
    let loc = project.location_of(
        target.clone(),
        &id,
        format!("{}/{PACKAGES_DIR}/{id}/", project.id),
    );
    let m = spec::load(&loc, ctx.env)?;
    if let Some(declared) = &m.declared_id
        && declared != &id
    {
        ctx.out.warn(&format!(
            "its spec says s.id = '{declared}', so fpkg check reports a mismatch with '{id}'"
        ));
    }

    let written = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        project::relative_path(&project.root.join(PACKAGES_DIR), &target)
            .unwrap_or_else(|| target.clone())
    };
    fs_err::create_dir_all(project.packages_dir())?;
    // The link is made under a hidden name first and renamed into place, so
    // that a symlink that cannot be created (Windows without the privilege)
    // costs nothing: what it replaces is removed only once the new link exists.
    let staged = project.packages_dir().join(format!(".fpkg-tmp-link-{id}"));
    let _ = project::remove_symlink_dir(&staged);
    project::symlink_dir(&written, &staged)
        .map_err(|e| anyhow::anyhow!("creating the symlink {}: {e}", link.display()))?;
    let placed = match replace {
        Replace::Nothing => Ok(()),
        Replace::Link => project::remove_symlink_dir(&link)
            .map_err(|e| anyhow::anyhow!("removing {}: {e}", link.display())),
        Replace::Installed => fs_err::remove_dir_all(&link).map_err(anyhow::Error::from),
    }
    .and_then(|()| fs_err::rename(&staged, &link).map_err(anyhow::Error::from));
    if let Err(e) = placed {
        let _ = project::remove_symlink_dir(&staged);
        return Err(e);
    }
    ctx.out.status(
        "Linked",
        &format!("{shown} -> {} ({id} {})", written.display(), m.version),
    );
    let nested = target.join(PACKAGES_DIR).is_dir();
    if ctx.out.json {
        ctx.out.json(&json!({ "id": id, "target": written, "path": target, "version": m.version.to_string() }));
    } else {
        if nested {
            ctx.out.note(&format!(
                "the packages in {}/{PACKAGES_DIR}/ are used from the checkout too",
                target.display()
            ));
        }
        ctx.out.note("next: fpkg install");
    }
    Ok(())
}

/// `fpkg unlink <id>`.
pub fn unlink(ctx: &Ctx, id: &str) -> Result<()> {
    usage_id(id)?;
    let project = ctx.project()?;
    let link = project.package_dir(id);
    let shown = format!("{PACKAGES_DIR}/{id}");
    match std::fs::symlink_metadata(&link) {
        Err(_) => {
            return Err(exit(
                EXIT_HUMAN,
                format!("{shown} does not exist (fpkg link lists the links)"),
            ));
        }
        Ok(meta) if !meta.file_type().is_symlink() => {
            return Err(exit(
                EXIT_HUMAN,
                format!("{shown} is not a link; fpkg unlink removes only links"),
            ));
        }
        Ok(_) => {}
    }
    let target = std::fs::read_link(&link).unwrap_or_default();
    project::remove_symlink_dir(&link)
        .map_err(|e| anyhow::anyhow!("removing {}: {e}", link.display()))?;
    ctx.out
        .status("Unlinked", &format!("{shown} (was {})", target.display()));
    if ctx.out.json {
        ctx.out.json(&json!({ "id": id, "target": target }));
    } else if needed(ctx, &project, id) {
        ctx.out
            .note("next: fpkg install, which fetches the registry version");
    } else {
        ctx.out.note(&format!(
            "next: fpkg install; nothing depends on {id}, so it leaves fpkg.lock"
        ));
    }
    Ok(())
}

/// Whether the project manifest, or a package of the lock, depends on `id`.
fn needed(ctx: &Ctx, project: &project::Project, id: &str) -> bool {
    let direct = spec::load(&project.root_location(), ctx.env)
        .is_ok_and(|m| m.package_deps().any(|d| d.package_id() == Some(id)));
    let locked = fpkg_core::lockfile::Lockfile::read(&project.lock_path())
        .ok()
        .flatten()
        .is_some_and(|l| {
            l.packages
                .iter()
                .any(|p| p.id != id && p.dependencies.iter().any(|d| d == id))
        });
    direct || locked
}

/// `fpkg link` without arguments: the links in `packages/`.
pub fn list(ctx: &Ctx) -> Result<()> {
    let project = ctx.project()?;
    let links = project.links();
    let mut rows = Vec::new();
    for (id, target) in &links {
        let path = project.package_dir(id);
        let resolved = path.canonicalize().ok();
        let version = match &resolved {
            Some(r) if r.join(SPEC_FILE).is_file() => {
                spec::load(&project.package_location(id), ctx.env)
                    .ok()
                    .map(|m| m.version.to_string())
            }
            _ => None,
        };
        rows.push((id.clone(), target.clone(), resolved, version));
    }
    if ctx.out.json {
        let v: Vec<_> = rows
            .iter()
            .map(|(id, t, r, v)| json!({ "id": id, "target": t, "path": r, "version": v }))
            .collect();
        ctx.out.json(&json!(v));
        return Ok(());
    }
    if rows.is_empty() {
        ctx.out
            .note("no links in packages/ (fpkg link <dir> makes one)");
        return Ok(());
    }
    let mut t = vec![vec![
        "ID".into(),
        "TARGET".into(),
        "PATH".into(),
        "VERSION".into(),
    ]];
    for (id, target, resolved, version) in rows {
        t.push(vec![
            id,
            target.display().to_string(),
            resolved
                .map(|r| r.display().to_string())
                .unwrap_or_else(|| "(missing)".into()),
            version.unwrap_or_else(|| "?".into()),
        ]);
    }
    print!("{}", table(&t));
    Ok(())
}

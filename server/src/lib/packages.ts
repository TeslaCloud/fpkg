// Package mutations shared by the API and the admin pages: yank, owners, approval and rejection,
// each with the cache updates the change needs.

import type { Ctx, Env } from "../env";
import { CACHE_CONTROL, ETAG, purge, store } from "./cache";
import { getVersionRow, ownerLogins, urls } from "./db";
import { ApiError, jsonResponse, now } from "./http";
import { versionMetadata, type VersionRow } from "./metadata";
import { enqueueDocs } from "../docs/generate";

/**
 * API URLs under the origin the request arrived at, which is what the cache is keyed by
 * (`wrangler dev` rewrites every request to the first route's host, so SITE_URL can differ).
 */
export function apiPackageUrl(requestUrl: string, id: string): string {
  return new URL(`/api/v1/packages/${id}`, requestUrl).toString();
}

export function apiVersionUrl(requestUrl: string, id: string, version: string): string {
  return new URL(`/api/v1/packages/${id}/versions/${version}`, requestUrl).toString();
}

/** The public version metadata response, with its validators. */
export function versionResponse(env: Env, row: VersionRow): Response {
  return jsonResponse(versionMetadata(row, urls(env)), 200, {
    ETag: ETAG.version(row.sha256, row.yanked !== 0),
    "Cache-Control": CACHE_CONTROL.version,
  });
}

function purgePackage(ctx: Ctx, requestUrl: string, id: string): void {
  ctx.waitUntil(purge(apiPackageUrl(requestUrl, id)).catch((e) => console.error("cache purge failed", e)));
}

/** Records what an admin did; shown on the admin dashboard. */
export async function logAdminAction(db: D1Database, actorId: number, action: string, packageId: string, version: string | null = null, detail: string | null = null): Promise<void> {
  await db.prepare("INSERT INTO admin_actions (actor_id, action, package_id, version, detail, at) VALUES (?, ?, ?, ?, ?, ?)")
    .bind(actorId, action, packageId, version, detail, now())
    .run();
}

/**
 * Sets a version's yanked flag (a no-op without a revision bump when it already is), and gives
 * this colo the new version body at once; the package document entry is dropped.
 */
export async function setYanked(env: Env, ctx: Ctx, requestUrl: string, row: VersionRow, yanked: boolean): Promise<VersionRow> {
  if ((row.yanked !== 0) === yanked) return row;
  const { package_id: id, version } = row;
  const at = now();
  await env.DB.batch([
    env.DB.prepare("UPDATE versions SET yanked = ? WHERE package_id = ? AND version = ?").bind(yanked ? 1 : 0, id, version),
    env.DB.prepare("UPDATE packages SET revision = revision + 1, updated_at = ? WHERE id = ?").bind(at, id),
  ]);
  const fresh = (await getVersionRow(env.DB, id, version))!;
  // Pending packages are never in the shared cache; approved ones get the new body in this colo.
  if (fresh.package_approved_at !== null) {
    ctx.waitUntil(store(apiVersionUrl(requestUrl, id, version), versionResponse(env, fresh)).catch((e) => console.error("cache update failed", e)));
  }
  purgePackage(ctx, requestUrl, id);
  return fresh;
}

/** Adds an owner by login (case-insensitively); idempotent. 404 `user_not_found` for a login that never logged in. */
export async function addOwner(env: Env, ctx: Ctx, requestUrl: string, id: string, login: string): Promise<{ added: boolean; owners: string[] }> {
  const target = await env.DB.prepare("SELECT id, login FROM users WHERE lower(login) = lower(?) ORDER BY id DESC LIMIT 1")
    .bind(login)
    .first<{ id: number; login: string }>();
  if (!target) throw new ApiError(404, "user_not_found", `no user \`${login}\`: they need to log in to the registry once first`);
  const at = now();
  const [inserted] = await env.DB.batch([
    env.DB.prepare("INSERT OR IGNORE INTO owners (package_id, user_id, added_at) VALUES (?, ?, ?)").bind(id, target.id, at),
    env.DB.prepare("UPDATE packages SET revision = revision + 1, updated_at = ? WHERE id = ? AND (SELECT changes()) > 0").bind(at, id),
  ]);
  const added = (inserted!.meta.changes ?? 0) > 0;
  if (added) purgePackage(ctx, requestUrl, id);
  return { added, owners: await ownerLogins(env.DB, id) };
}

/** Removes an owner by login; 404 when they are not one, 409 `last_owner` for the last one. */
export async function removeOwner(env: Env, ctx: Ctx, requestUrl: string, id: string, login: string): Promise<string[]> {
  const target = await env.DB.prepare("SELECT u.id FROM owners o JOIN users u ON u.id = o.user_id WHERE o.package_id = ? AND lower(u.login) = lower(?)")
    .bind(id, login)
    .first<{ id: number }>();
  if (!target) throw new ApiError(404, "not_found", `\`${login}\` is not an owner of \`${id}\``);
  const at = now();
  // The count guard keeps the last owner even under concurrent removals.
  const [deleted] = await env.DB.batch([
    env.DB.prepare("DELETE FROM owners WHERE package_id = ?1 AND user_id = ?2 AND (SELECT COUNT(*) FROM owners WHERE package_id = ?1) > 1").bind(id, target.id),
    env.DB.prepare("UPDATE packages SET revision = revision + 1, updated_at = ? WHERE id = ? AND (SELECT changes()) > 0").bind(at, id),
  ]);
  if ((deleted!.meta.changes ?? 0) === 0) throw new ApiError(409, "last_owner", `\`${login}\` is the last owner of \`${id}\` and cannot be removed`);
  purgePackage(ctx, requestUrl, id);
  return ownerLogins(env.DB, id);
}

/**
 * Makes a pending package public and queues the docs of its versions. Returns false when it was
 * not pending (already approved, or gone).
 */
export async function approvePackage(env: Env, ctx: Ctx, requestUrl: string, id: string, adminId: number): Promise<boolean> {
  const at = now();
  const r = await env.DB.prepare("UPDATE packages SET approved_at = ?1, approved_by = ?2, revision = revision + 1, updated_at = ?1 WHERE id = ?3 AND approved_at IS NULL")
    .bind(at, adminId, id)
    .run();
  if ((r.meta.changes ?? 0) === 0) return false;
  await logAdminAction(env.DB, adminId, "approve", id);
  purgePackage(ctx, requestUrl, id);
  const versions = (await env.DB.prepare("SELECT version FROM versions WHERE package_id = ?").bind(id).all<{ version: string }>()).results;
  for (const v of versions) {
    try {
      await enqueueDocs(env, id, v.version);
    } catch (e) {
      // Docs are generated on first request anyway.
      console.error(`could not enqueue docs for ${id} ${v.version}`, e);
    }
  }
  return true;
}

/**
 * Removes a pending package: its versions, owners, download counts and archives. Returns false
 * when it is not pending. Approved packages are never deleted (others may depend on them).
 */
export async function rejectPackage(env: Env, id: string, adminId: number, reason: string | null): Promise<boolean> {
  const versions = (await env.DB.prepare("SELECT version, sha256 FROM versions WHERE package_id = ?").bind(id).all<{ version: string; sha256: string }>()).results;
  // Children first (D1 enforces foreign keys), each guarded by the package still being pending.
  const pending = "EXISTS (SELECT 1 FROM packages WHERE id = ?1 AND approved_at IS NULL)";
  const [, , , deleted] = await env.DB.batch([
    env.DB.prepare(`DELETE FROM versions WHERE package_id = ?1 AND ${pending}`).bind(id),
    env.DB.prepare(`DELETE FROM owners WHERE package_id = ?1 AND ${pending}`).bind(id),
    env.DB.prepare(`DELETE FROM downloads WHERE package_id = ?1 AND ${pending}`).bind(id),
    env.DB.prepare("DELETE FROM packages WHERE id = ?1 AND approved_at IS NULL").bind(id),
  ]);
  if ((deleted!.meta.changes ?? 0) === 0) return false;
  await logAdminAction(env.DB, adminId, "reject", id, null, reason);
  // Only the deleted rows' bytes: a publish racing the rejection may have put a new version's
  // archive under the same key. A leftover object is replaced by a later publish once it is
  // old enough (see putArchive).
  for (const v of versions) {
    const key = `archives/${id}/${v.version}.zip`;
    try {
      const head = await env.ARCHIVES.head(key);
      if (head && head.customMetadata?.sha256 === v.sha256) await env.ARCHIVES.delete(key);
    } catch (e) {
      console.error(`deleting the archive of rejected ${id} ${v.version} failed`, e);
    }
  }
  return true;
}

// Publish: PUT /api/v1/packages/{id}/versions/{version}, multipart `metadata` + `archive`.

import type { Context } from "hono";
import type { AppEnv } from "../env";
import { requireUser, sha256Hex } from "../lib/auth";
import { CACHE_CONTROL, purge } from "../lib/cache";
import { getPackageState, getVersionRow, isOwner, urls } from "../lib/db";
import { ApiError, badRequest, forbidden, invalid, jsonResponse, now } from "../lib/http";
import { type StoredMetadata, validateMetadata, versionMetadata } from "../lib/metadata";
import { apiPackageUrl, apiVersionUrl } from "../lib/packages";
import { scanSpec } from "../lib/specscan";
import { canonicalize, isValidId, parseConstraint, parseVersion, satisfies, sortKey, formatVersion, ID_RE } from "../lib/version";
import { LIMITS, ZipError, validateArchive } from "../lib/zip";
import { enqueueDocs } from "../docs/generate";
import { MultipartError, type Part, boundaryOf, parseMultipart } from "../lib/multipart";

/** Room for the metadata part and the multipart framing on top of the archive. */
const MAX_BODY = LIMITS.compressed + 2 * 1024 * 1024;
const MAX_METADATA = 1024 * 1024;

/** Bound parameters per dependency query (D1's limit is 100). */
const BIND_CHUNK = 90;

/** Publishes per user per hour before `429`. */
export const PUBLISH_RATE = { count: 120, windowSeconds: 3600 };

/** New packages a non-admin may have created and still awaiting approval before `403 pending_limit`. */
export const MAX_PENDING_PER_USER = 10;

/** An R2 object older than this without a versions row is a leftover of a failed publish. */
const ORPHAN_AGE_MS = 10 * 60 * 1000;

const tooLarge = (message: string) => new ApiError(413, "payload_too_large", message);

type C = Context<AppEnv>;

export async function publish(c: C): Promise<Response> {
  const env = c.env;
  const db = env.DB;
  const user = await requireUser(env, c.executionCtx, c.req.raw);
  const id = c.req.param("id")!;
  const version = c.req.param("version")!;

  const grammar: string[] = [];
  if (!isValidId(id)) grammar.push(`id: \`${id}\` does not match ${ID_RE.source}`);
  const parsed = parseVersion(version);
  if (!parsed) {
    grammar.push(`version: \`${version}\` is not a version (X.Y.Z[-suffix])`);
  } else if (formatVersion(parsed) !== version) {
    grammar.push(`version: \`${version}\` must be written canonically, as \`${formatVersion(parsed)}\` (three components)`);
  }
  if (grammar.length > 0) throw invalid("the package id or version is invalid", grammar);

  // Rate limit, then existence (409), ownership, and the cap on a non-admin's pending packages.
  await checkRate(c, user.userId);
  if (await getVersionRow(db, id, version)) throw new ApiError(409, "version_exists", `${id} ${version} is already published; versions are immutable`);
  const existing = await getPackageState(db, id);
  if (existing && !(await isOwner(db, id, user.userId))) throw forbidden(`\`${id}\` belongs to other users; \`${user.login}\` is not an owner`);
  if (!existing && !user.admin) await checkPendingLimit(db, user.userId);

  // The body is read into memory once; parts are views into it.
  const length = Number(c.req.header("Content-Length") ?? "0");
  if (length > MAX_BODY) throw tooLarge(`the request body is larger than ${MAX_BODY} bytes (archives are limited to ${LIMITS.compressed})`);
  const boundary = boundaryOf(c.req.header("Content-Type"));
  if (!boundary) throw badRequest("the body must be multipart/form-data with `metadata` and `archive` parts");
  const body = await readBody(c.req.raw, MAX_BODY);
  let parts: Part[];
  try {
    parts = parseMultipart(body, boundary);
  } catch (e) {
    if (e instanceof MultipartError) throw badRequest(`the multipart body does not parse: ${e.message}`);
    throw e;
  }
  const metaPart = parts.find((p) => p.name === "metadata");
  const archivePart = parts.find((p) => p.name === "archive");
  const missing: string[] = [];
  if (!metaPart) missing.push("the `metadata` part is missing");
  if (!archivePart) missing.push("the `archive` part is missing");
  if (!metaPart || !archivePart) throw invalid("the publish request is incomplete", missing);
  const archive = archivePart.data;
  if (archive.length > LIMITS.compressed) throw tooLarge(`the archive is larger than ${LIMITS.compressed} bytes`);
  if (metaPart.data.length > MAX_METADATA) throw tooLarge(`the metadata is larger than ${MAX_METADATA} bytes`);
  const metaText = new TextDecoder().decode(metaPart.data);
  const sha256 = await sha256Hex(archive);

  // Metadata, archive structure and spec literals; all problems are reported together.
  const details: string[] = [];
  let raw: unknown;
  try {
    raw = JSON.parse(metaText);
  } catch (e) {
    details.push(`metadata: not JSON (${e instanceof Error ? e.message : String(e)})`);
  }
  const meta = raw === undefined ? null : validateMetadata(raw, id, version);
  if (meta) details.push(...meta.details);

  let spec: string | null = null;
  try {
    spec = validateArchive(archive, id).spec;
  } catch (e) {
    if (!(e instanceof ZipError)) throw e;
    details.push(...e.details.map((d) => `archive: ${d}`));
  }
  // The name check runs even when other metadata members are invalid.
  const rawName = typeof raw === "object" && raw !== null && typeof (raw as { name?: unknown }).name === "string" ? (raw as { name: string }).name : null;
  if (spec !== null) details.push(...checkSpec(spec, id, version, rawName));
  if (details.length > 0 || !meta?.metadata) throw invalid(`${id} ${version} cannot be published`, details);
  const metadata = meta.metadata;

  // Each dependency needs a satisfying, non-yanked version.
  if (!meta.allowMissingDependencies && metadata.dependencies.length > 0) {
    const problems = await checkDependencies(db, metadata);
    if (problems.length > 0) {
      throw invalid("dependencies are missing from the registry (set `allow_missing_dependencies` to publish anyway)", problems);
    }
  }

  // R2, then one D1 batch; the R2 object is removed if D1 fails.
  const key = `archives/${id}/${version}.zip`;
  const putOptions = archiveOptions(id, version, sha256);
  const wrote = await putArchive(env.ARCHIVES, key, archive, putOptions, id, version);
  let committed: boolean;
  try {
    committed = await commitVersion(db, {
      id,
      version,
      sortKey: sortKey(parsed!),
      metadata: JSON.stringify(metadata),
      sha256,
      size: archive.length,
      userId: user.userId,
      at: now(),
      // An admin's new package is public at once; anyone else's waits for an admin.
      approvedBy: user.admin ? user.userId : null,
    });
  } catch (e) {
    // A concurrent publish won the insert; its row describes the R2 object, so it stays.
    if (/UNIQUE|PRIMARY KEY|constraint/i.test(String(e))) {
      throw new ApiError(409, "version_exists", `${id} ${version} is already published; versions are immutable`);
    }
    if (wrote) await rollbackArchive(env.ARCHIVES, db, key, id, version, archive, putOptions).catch((d) => console.error("R2 rollback failed", d));
    throw e;
  }
  if (!committed) {
    if (wrote) await rollbackArchive(env.ARCHIVES, db, key, id, version, archive, putOptions).catch((d) => console.error("R2 rollback failed", d));
    throw forbidden(`\`${id}\` belongs to other users; \`${user.login}\` is not an owner`);
  }
  // A concurrent rollback may have deleted the identical object; put it back.
  await ensureArchive(env.ARCHIVES, key, archive, putOptions);

  const row = (await getVersionRow(db, id, version))!;
  const pending = row.package_approved_at === null;
  if (!pending) {
    // A pending package's docs are queued when it is approved.
    try {
      await enqueueDocs(env, id, version);
    } catch (e) {
      // Docs are generated on first request anyway.
      console.error(`could not enqueue docs for ${id} ${version}`, e);
    }
  }
  c.executionCtx.waitUntil(purge(apiPackageUrl(c.req.url, id)).catch(() => {}));
  // 202: stored, but not public until an admin approves the package.
  return jsonResponse(versionMetadata(row, urls(env)), pending ? 202 : 201, { Location: apiVersionUrl(c.req.url, id, version) });
}

/** The spec's `version` literal must canonicalise to the URL's version; a `name` literal must equal `metadataName` (null skips). */
export function checkSpec(spec: string, id: string, version: string, metadataName: string | null): string[] {
  const out: string[] = [];
  const where = `archive: ${id}/packagespec.lua`;
  const scan = scanSpec(spec);
  if (scan.error) return [`${where}: ${scan.error}`];
  const versions = [...new Set(scan.version.literals)];
  if (scan.version.computed.length > 0) {
    out.push(`${where}: \`version\` is assigned a computed value on line ${scan.version.computed.join(", ")}; the registry needs a string literal`);
  } else if (versions.length === 0) {
    out.push(`${where}: no \`s.version = '...'\` string literal`);
  } else if (new Set(versions.map((v) => canonicalize(v) ?? v)).size > 1) {
    out.push(`${where}: \`version\` is assigned different values (${versions.map((v) => `'${v}'`).join(", ")})`);
  } else {
    const v = versions[0]!;
    const canonical = canonicalize(v);
    if (canonical === null) out.push(`${where}: \`version = '${v}'\` is not a version`);
    else if (canonical !== version) out.push(`${where}: \`version = '${v}'\` is not the published version ${version}`);
  }
  const names = [...new Set(scan.name.literals)];
  if (scan.name.computed.length > 0) {
    out.push(`${where}: \`name\` is assigned a computed value on line ${scan.name.computed.join(", ")}; the registry needs a string literal`);
  } else if (names.length > 1) {
    out.push(`${where}: \`name\` is assigned different values (${names.map((n) => `'${n}'`).join(", ")})`);
  } else if (names.length === 1 && metadataName !== null && names[0] !== metadataName) {
    out.push(`${where}: \`name = '${names[0]}'\` disagrees with the metadata's name \`${metadataName}\``);
  }
  return out;
}

/** Only approved packages' versions satisfy a dependency, so nothing can depend on what the public cannot fetch. */
async function checkDependencies(db: D1Database, metadata: StoredMetadata): Promise<string[]> {
  const ids = metadata.dependencies.map((d) => d.id);
  // D1 binds at most 100 parameters per statement.
  const statements: D1PreparedStatement[] = [];
  for (let i = 0; i < ids.length; i += BIND_CHUNK) {
    const chunk = ids.slice(i, i + BIND_CHUNK);
    statements.push(
      db
        .prepare(
          `SELECT v.package_id, v.version FROM versions v JOIN packages p ON p.id = v.package_id
             WHERE v.yanked = 0 AND p.approved_at IS NOT NULL AND v.package_id IN (${chunk.map(() => "?").join(", ")})`,
        )
        .bind(...chunk),
    );
  }
  const results = await db.batch<{ package_id: string; version: string }>(statements);
  const byId = new Map<string, string[]>();
  for (const r of results.flatMap((x) => x.results)) byId.set(r.package_id, [...(byId.get(r.package_id) ?? []), r.version]);
  const problems: string[] = [];
  for (const d of metadata.dependencies) {
    const versions = byId.get(d.id) ?? [];
    const constraint = parseConstraint(d.constraint)!;
    if (versions.length === 0) {
      problems.push(`dependency \`${d.id}\`: not on the registry (or every version is yanked)`);
    } else if (!versions.some((v) => satisfies(parseVersion(v)!, constraint))) {
      problems.push(`dependency \`${d.id} ${d.constraint}\`: no published version satisfies it (have ${versions.join(", ")})`);
    }
  }
  return problems;
}

export interface CommitArgs {
  id: string;
  version: string;
  sortKey: string;
  metadata: string;
  sha256: string;
  size: number;
  userId: number;
  at: string;
  /** The admin a new package is approved by at once; null leaves it pending. Existing packages are unaffected. */
  approvedBy?: number | null;
}

/**
 * The D1 half of a publish, one transaction: create the package (approved or pending) and first
 * owner if new, insert the version guarded by ownership (racing first publishes cannot both land),
 * and bump the revision only if it landed. Returns whether it did; throws if the version exists.
 */
export async function commitVersion(db: D1Database, a: CommitArgs): Promise<boolean> {
  const approvedBy = a.approvedBy ?? null;
  const [, , insert] = await db.batch([
    db
      .prepare(
        `INSERT INTO packages (id, revision, created_at, updated_at, approved_at, approved_by, created_by)
           VALUES (?1, 0, ?2, ?2, CASE WHEN ?3 IS NULL THEN NULL ELSE ?2 END, ?3, ?4) ON CONFLICT (id) DO NOTHING`,
      )
      .bind(a.id, a.at, approvedBy, a.userId),
    db
      .prepare(
        `INSERT OR IGNORE INTO owners (package_id, user_id, added_at)
           SELECT ?1, ?2, ?3 WHERE NOT EXISTS (SELECT 1 FROM owners WHERE package_id = ?1)`,
      )
      .bind(a.id, a.userId, a.at),
    db
      .prepare(
        `INSERT INTO versions (package_id, version, sort_key, metadata, sha256, size, published_at, published_by)
           SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
            WHERE EXISTS (SELECT 1 FROM owners WHERE package_id = ?1 AND user_id = ?8)`,
      )
      .bind(a.id, a.version, a.sortKey, a.metadata, a.sha256, a.size, a.at, a.userId),
    // changes() is the version insert's.
    db.prepare("UPDATE packages SET revision = revision + 1, updated_at = ?2 WHERE id = ?1 AND (SELECT changes()) > 0").bind(a.id, a.at),
  ]);
  return (insert!.meta.changes ?? 0) > 0;
}

export function archiveOptions(id: string, version: string, sha256: string): R2PutOptions {
  return {
    httpMetadata: {
      contentType: "application/zip",
      cacheControl: CACHE_CONTROL.archive,
      contentDisposition: `attachment; filename="${id}-${version}.zip"`,
    },
    customMetadata: { sha256 },
    sha256,
  };
}

/**
 * Writes the archive unless an object exists; returns whether it wrote. Same bytes are reused;
 * other bytes are a concurrent publish (409) or, once old enough, a failed publish's leftover.
 */
async function putArchive(bucket: R2Bucket, key: string, archive: Uint8Array, options: R2PutOptions, id: string, version: string): Promise<boolean> {
  const sha256 = options.customMetadata!.sha256;
  const head = await bucket.head(key);
  if (head) {
    if (head.customMetadata?.sha256 === sha256) return false;
    if (Date.now() - head.uploaded.getTime() < ORPHAN_AGE_MS) {
      throw new ApiError(409, "publish_in_progress", `another publish of ${id} ${version} is in progress; retry in a few minutes`, undefined, {
        "Retry-After": "600",
      });
    }
    const replaced = await bucket.put(key, archive, { ...options, onlyIf: { etagMatches: head.etag } });
    if (!replaced) throw new ApiError(409, "publish_in_progress", `another publish of ${id} ${version} is in progress`, undefined, { "Retry-After": "60" });
    return true;
  }
  const put = await bucket.put(key, archive, { ...options, onlyIf: { etagDoesNotMatch: "*" } });
  if (!put) throw new ApiError(409, "publish_in_progress", `another publish of ${id} ${version} is in progress`, undefined, { "Retry-After": "60" });
  return true;
}

/**
 * Undoes our R2 write after D1 refused the version. R2 has no conditional delete, so this deletes
 * only our bytes, then puts them back if a concurrent identical publish committed meanwhile.
 */
export async function rollbackArchive(
  bucket: R2Bucket,
  db: D1Database,
  key: string,
  id: string,
  version: string,
  archive: Uint8Array,
  options: R2PutOptions,
): Promise<void> {
  const sha256 = options.customMetadata!.sha256;
  const head = await bucket.head(key);
  if (!head || head.customMetadata?.sha256 !== sha256) return;
  await bucket.delete(key);
  const row = await db.prepare("SELECT sha256 FROM versions WHERE package_id = ? AND version = ?").bind(id, version).first<{ sha256: string }>();
  if (row && row.sha256 === sha256) await bucket.put(key, archive, options);
}

/** Puts the archive back if it is missing or holds other bytes than the committed row's. */
export async function ensureArchive(bucket: R2Bucket, key: string, archive: Uint8Array, options: R2PutOptions): Promise<void> {
  const head = await bucket.head(key);
  if (head?.customMetadata?.sha256 !== options.customMetadata!.sha256) await bucket.put(key, archive, options);
}

/**
 * A non-admin may have at most MAX_PENDING_PER_USER packages they created awaiting approval.
 * Counted by creator, not owner: owners can be added and removed, which must neither evade
 * the cap nor let one user fill another's. Best effort under concurrent first publishes.
 */
async function checkPendingLimit(db: D1Database, userId: number): Promise<void> {
  const r = await db.prepare("SELECT COUNT(*) AS n FROM packages WHERE created_by = ? AND approved_at IS NULL").bind(userId).first<{ n: number }>();
  if (r && r.n >= MAX_PENDING_PER_USER) {
    throw new ApiError(403, "pending_limit", `you have ${MAX_PENDING_PER_USER} packages awaiting approval; wait for an admin to review them before publishing new ones`);
  }
}

async function checkRate(c: C, userId: number): Promise<void> {
  const since = now(new Date(Date.now() - PUBLISH_RATE.windowSeconds * 1000));
  const r = await c.env.DB.prepare("SELECT COUNT(*) AS n, MIN(published_at) AS oldest FROM versions WHERE published_by = ? AND published_at > ?")
    .bind(userId, since)
    .first<{ n: number; oldest: string | null }>();
  if (r && r.n >= PUBLISH_RATE.count) {
    const retry = r.oldest ? Math.max(1, Math.ceil((Date.parse(r.oldest) + PUBLISH_RATE.windowSeconds * 1000 - Date.now()) / 1000)) : 60;
    throw new ApiError(429, "rate_limited", `at most ${PUBLISH_RATE.count} publishes per hour`, undefined, { "Retry-After": String(retry) });
  }
}

/** Reads a request body, refusing more than `max` bytes even without a Content-Length. */
async function readBody(request: Request, max: number): Promise<Uint8Array> {
  if (!request.body) return new Uint8Array(0);
  const declared = Number(request.headers.get("Content-Length") ?? "NaN");
  const reader = request.body.getReader();
  if (Number.isInteger(declared) && declared >= 0) {
    // One allocation of the declared size.
    const out = new Uint8Array(declared);
    let at = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (at + value.length > declared) throw badRequest("the body is longer than its Content-Length");
      out.set(value, at);
      at += value.length;
    }
    return at === declared ? out : out.subarray(0, at);
  }
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > max) {
      await reader.cancel();
      throw tooLarge(`the request body is larger than ${max} bytes`);
    }
    chunks.push(value);
  }
  const out = new Uint8Array(total);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.length;
  }
  return out;
}

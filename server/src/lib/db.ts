// D1 queries shared by the API, the website and the docs host.

import type { Env } from "../env";
import { type Urls, type VersionMetadata, type VersionRow, versionMetadata } from "./metadata";

export function urls(env: Env): Urls {
  return { site: env.SITE_URL || "https://fluxpkg.dev", docs: env.DOCS_URL || "https://docs.fluxpkg.dev" };
}

export interface PackageRow {
  id: string;
  revision: number;
  created_at: string;
  updated_at: string;
  /** Null while the package waits for an admin's approval. */
  approved_at: string | null;
  approved_by: number | null;
  /** The user whose publish created the package (null only for rows older than migration 0004 with no version). */
  created_by: number | null;
}

export type PackageStatus = "approved" | "pending";

export function packageStatus(row: { approved_at: string | null }): PackageStatus {
  return row.approved_at === null ? "pending" : "approved";
}

const VERSION_COLUMNS = `v.package_id, v.version, v.metadata, v.sha256, v.size, v.yanked, v.published_at,
  u.login AS published_by_login, v.docs_status, v.docs_version, p.approved_at AS package_approved_at`;

const VERSION_FROM = `FROM versions v JOIN users u ON u.id = v.published_by JOIN packages p ON p.id = v.package_id`;

/** Revision and approval of a package, or null when there is none. */
export async function getPackageState(db: D1Database, id: string): Promise<{ revision: number; approved_at: string | null } | null> {
  return db.prepare("SELECT revision, approved_at FROM packages WHERE id = ?").bind(id).first<{ revision: number; approved_at: string | null }>();
}

export async function getVersionRow(db: D1Database, id: string, version: string): Promise<VersionRow | null> {
  return db.prepare(`SELECT ${VERSION_COLUMNS} ${VERSION_FROM} WHERE v.package_id = ? AND v.version = ?`).bind(id, version).first<VersionRow>();
}

export interface PackageDocument {
  id: string;
  revision: number;
  status: PackageStatus;
  owners: string[];
  latest: string | null;
  versions: Record<string, VersionMetadata>;
}

export interface PackageData {
  pkg: PackageRow;
  owners: string[];
  /** Newest first. */
  versions: VersionRow[];
}

/** The package row, its owners and its versions, in one round trip. */
export async function loadPackage(db: D1Database, id: string): Promise<PackageData | null> {
  const [pkg, owners, versions] = await db.batch([
    db.prepare("SELECT id, revision, created_at, updated_at, approved_at, approved_by, created_by FROM packages WHERE id = ?").bind(id),
    db.prepare("SELECT u.login FROM owners o JOIN users u ON u.id = o.user_id WHERE o.package_id = ? ORDER BY o.added_at, u.login").bind(id),
    db.prepare(`SELECT ${VERSION_COLUMNS} ${VERSION_FROM} WHERE v.package_id = ? ORDER BY v.sort_key DESC`).bind(id),
  ]);
  const row = (pkg!.results as PackageRow[])[0];
  if (!row) return null;
  return {
    pkg: row,
    owners: (owners!.results as { login: string }[]).map((r) => r.login),
    versions: versions!.results as VersionRow[],
  };
}

/**
 * The newest non-yanked release, else the newest non-yanked pre-release.
 * `versions` is newest first, where `2.0.0-beta` sorts above `1.9.0`.
 */
export function latestVersion(versions: { version: string; yanked: number }[]): string | null {
  const live = versions.filter((v) => v.yanked === 0);
  return (live.find((v) => !v.version.includes("-")) ?? live[0])?.version ?? null;
}

/** SQL ORDER BY terms for the same preference: releases first, then newest. */
export const LATEST_ORDER = "CASE WHEN version LIKE '%-%' THEN 1 ELSE 0 END, sort_key DESC";

export function packageDocument(data: PackageData, env: Env): PackageDocument {
  const u = urls(env);
  const versions: Record<string, VersionMetadata> = {};
  for (const v of data.versions) versions[v.version] = versionMetadata(v, u);
  return {
    id: data.pkg.id,
    revision: data.pkg.revision,
    status: packageStatus(data.pkg),
    owners: data.owners,
    latest: latestVersion(data.versions),
    versions,
  };
}

export async function ownerLogins(db: D1Database, id: string): Promise<string[]> {
  const r = await db
    .prepare("SELECT u.login FROM owners o JOIN users u ON u.id = o.user_id WHERE o.package_id = ? ORDER BY o.added_at, u.login")
    .bind(id)
    .all<{ login: string }>();
  return r.results.map((x) => x.login);
}

export async function isOwner(db: D1Database, id: string, userId: number): Promise<boolean> {
  const r = await db.prepare("SELECT 1 AS x FROM owners WHERE package_id = ? AND user_id = ?").bind(id, userId).first();
  return r !== null;
}

export interface SearchHit {
  id: string;
  name: string;
  version: string;
  summary: string | null;
  updated_at: string;
  downloads: number;
}

export interface SearchPage {
  query: string;
  total: number;
  page: number;
  per_page: number;
  packages: SearchHit[];
}

function likeEscape(s: string): string {
  return s.replace(/[\\%_]/g, (c) => `\\${c}`);
}

/** The newest non-yanked version of every approved package, as a CTE named `latest` with `rn = 1` marking it. */
const LATEST_CTE = `latest AS (
      SELECT v.package_id, v.version, v.metadata,
             ROW_NUMBER() OVER (PARTITION BY v.package_id ORDER BY ${LATEST_ORDER}) AS rn
        FROM versions v JOIN packages p ON p.id = v.package_id WHERE v.yanked = 0 AND p.approved_at IS NOT NULL
    )`;

/**
 * Matches id, name and summary of each approved package's newest non-yanked version.
 * Ranks exact id, id prefix, name prefix, then the rest, each by id; "" lists all.
 */
export async function search(db: D1Database, query: string, page: number, perPage: number): Promise<SearchPage> {
  const q = query.trim();
  const sub = `%${likeEscape(q)}%`;
  const prefix = `${likeEscape(q)}%`;
  const latest = `WITH ${LATEST_CTE}, hits AS (
      SELECT l.package_id, l.version, json_extract(l.metadata, '$.name') AS name,
             json_extract(l.metadata, '$.summary') AS summary
        FROM latest l WHERE l.rn = 1
    )`;
  const where = `WHERE (?1 = '' OR h.package_id LIKE ?2 ESCAPE '\\' OR h.name LIKE ?2 ESCAPE '\\' OR h.summary LIKE ?2 ESCAPE '\\')`;
  const [count, rows] = await db.batch([
    db.prepare(`${latest} SELECT COUNT(*) AS n FROM hits h ${where}`).bind(q, sub),
    db
      .prepare(
        `${latest} SELECT h.package_id AS id, h.name, h.version, h.summary, p.updated_at,
            COALESCE((SELECT SUM(d.count) FROM downloads d WHERE d.package_id = h.package_id), 0) AS downloads
           FROM hits h JOIN packages p ON p.id = h.package_id ${where}
           ORDER BY CASE WHEN h.package_id = ?1 THEN 0 WHEN h.package_id LIKE ?3 ESCAPE '\\' THEN 1
                         WHEN h.name LIKE ?3 ESCAPE '\\' THEN 2 ELSE 3 END, h.package_id
           LIMIT ?4 OFFSET ?5`,
      )
      .bind(q, sub, prefix, perPage, (page - 1) * perPage),
  ]);
  return {
    query: q,
    total: (count!.results[0] as { n: number } | undefined)?.n ?? 0,
    page,
    per_page: perPage,
    packages: (rows!.results as SearchHit[]).map((r) => ({ ...r, downloads: Number(r.downloads) })),
  };
}

export async function downloadCount(db: D1Database, id: string): Promise<number> {
  const r = await db.prepare("SELECT COALESCE(SUM(count), 0) AS n FROM downloads WHERE package_id = ?").bind(id).first<{ n: number }>();
  return r?.n ?? 0;
}

export interface FrontHit extends SearchHit {
  created_at: string;
}

export interface FrontPage {
  packages: number;
  versions: number;
  downloads: number;
  /** By `updated_at`, newest first. */
  updated: FrontHit[];
  /** By downloads, most first. */
  popular: FrontHit[];
  /** By `created_at`, newest first. */
  newest: FrontHit[];
}

/** The front page's counts and three lists of `limit` approved packages each, in one round trip. */
export async function frontPage(db: D1Database, limit: number): Promise<FrontPage> {
  const select = `WITH ${LATEST_CTE}
    SELECT h.package_id AS id, json_extract(h.metadata, '$.name') AS name, h.version,
           json_extract(h.metadata, '$.summary') AS summary, p.updated_at, p.created_at,
           COALESCE((SELECT SUM(d.count) FROM downloads d WHERE d.package_id = h.package_id), 0) AS downloads
      FROM latest h JOIN packages p ON p.id = h.package_id WHERE h.rn = 1`;
  const [counts, updated, popular, newest] = await db.batch([
    db.prepare(
      `SELECT (SELECT COUNT(*) FROM packages WHERE approved_at IS NOT NULL) AS packages,
              (SELECT COUNT(*) FROM versions v JOIN packages p ON p.id = v.package_id WHERE p.approved_at IS NOT NULL) AS versions,
              (SELECT COALESCE(SUM(d.count), 0) FROM downloads d JOIN packages p ON p.id = d.package_id WHERE p.approved_at IS NOT NULL) AS downloads`,
    ),
    db.prepare(`${select} ORDER BY p.updated_at DESC, h.package_id LIMIT ?`).bind(limit),
    db.prepare(`${select} ORDER BY downloads DESC, p.updated_at DESC, h.package_id LIMIT ?`).bind(limit),
    db.prepare(`${select} ORDER BY p.created_at DESC, h.package_id LIMIT ?`).bind(limit),
  ]);
  const hits = (r: D1Result | undefined) => (r!.results as FrontHit[]).map((h) => ({ ...h, downloads: Number(h.downloads) }));
  const c = counts!.results[0] as { packages: number; versions: number; downloads: number };
  return { packages: c.packages, versions: c.versions, downloads: Number(c.downloads), updated: hits(updated), popular: hits(popular), newest: hits(newest) };
}

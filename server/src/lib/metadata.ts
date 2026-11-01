// Version metadata: validating what a publish sends and building what the API returns.

import { canonicalize, isValidId, parseConstraint, ID_RE } from "./version";

export interface Dependency {
  id: string;
  constraint: string;
}

/**
 * The metadata a publisher supplies, normalised; what `versions.metadata`
 * stores. The server-owned fields (`dist`, `docs`, `yanked`,
 * `published_at`, `published_by`) come from columns when responding.
 */
export interface StoredMetadata {
  id: string;
  name: string;
  version: string;
  kind: "package";
  summary: string | null;
  description: string | null;
  authors: string[];
  license: string | null;
  website: string | null;
  globals: string[];
  serverside: boolean;
  clientside: boolean;
  dependencies: Dependency[];
}

export interface VersionMetadata extends StoredMetadata {
  dist: { sha256: string; size: number; url: string };
  docs: string;
  yanked: boolean;
  published_at: string;
  published_by: string;
}

const MAX = {
  name: 128,
  summary: 512,
  description: 64 * 1024,
  license: 128,
  website: 2048,
  authors: 64,
  author: 256,
  globals: 1024,
  global: 128,
  dependencies: 256,
  constraint: 256,
} as const;

export interface MetadataResult {
  metadata: StoredMetadata | null;
  allowMissingDependencies: boolean;
  details: string[];
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** Validates a publish's `metadata` for the URL's `id` and `version`, collecting every problem; unknown and server-owned members are ignored. */
export function validateMetadata(raw: unknown, id: string, version: string): MetadataResult {
  const details: string[] = [];
  if (!isObject(raw)) return { metadata: null, allowMissingDependencies: false, details: ["metadata: must be a JSON object"] };

  const optString = (key: string, max: number): string | null => {
    const v = raw[key];
    if (v === undefined || v === null) return null;
    if (typeof v !== "string") {
      details.push(`metadata.${key}: must be a string or null`);
      return null;
    }
    if (v.length > max) details.push(`metadata.${key}: longer than ${max} characters`);
    return v;
  };
  const stringList = (key: string, maxItems: number, maxLen: number): string[] => {
    const v = raw[key];
    if (v === undefined || v === null) return [];
    if (!Array.isArray(v) || v.some((x) => typeof x !== "string")) {
      details.push(`metadata.${key}: must be an array of strings`);
      return [];
    }
    if (v.length > maxItems) details.push(`metadata.${key}: more than ${maxItems} entries`);
    if (v.some((x: string) => x.length > maxLen)) details.push(`metadata.${key}: an entry is longer than ${maxLen} characters`);
    return v as string[];
  };
  const bool = (key: string): boolean => {
    const v = raw[key];
    if (v === undefined || v === null) return false;
    if (typeof v !== "boolean") {
      details.push(`metadata.${key}: must be a boolean`);
      return false;
    }
    return v;
  };

  if (raw.id !== id) details.push(`metadata.id: \`${String(raw.id)}\` does not match the URL's \`${id}\``);
  if (typeof raw.version !== "string") details.push("metadata.version: must be a string");
  else if (canonicalize(raw.version) !== version) details.push(`metadata.version: \`${raw.version}\` does not match the URL's \`${version}\``);
  if (raw.kind !== undefined && raw.kind !== null && raw.kind !== "package") details.push(`metadata.kind: must be "package"`);

  let name = "";
  if (typeof raw.name !== "string" || raw.name.trim() === "") details.push("metadata.name: must be a non-empty string");
  else if (raw.name.length > MAX.name) details.push(`metadata.name: longer than ${MAX.name} characters`);
  else name = raw.name;

  const summary = optString("summary", MAX.summary);
  const description = optString("description", MAX.description);
  const license = optString("license", MAX.license);
  const website = optString("website", MAX.website);
  if (website !== null) {
    let ok = false;
    try {
      const u = new URL(website);
      ok = u.protocol === "https:" || u.protocol === "http:";
    } catch {
      ok = false;
    }
    if (!ok) details.push("metadata.website: must be an http or https URL");
  }
  const authors = stringList("authors", MAX.authors, MAX.author);
  const globals = stringList("globals", MAX.globals, MAX.global);
  const serverside = bool("serverside");
  const clientside = bool("clientside");
  if (serverside && clientside) details.push("metadata: serverside and clientside cannot both be true");

  const dependencies: Dependency[] = [];
  const deps = raw.dependencies;
  if (deps !== undefined && deps !== null) {
    if (!Array.isArray(deps)) details.push("metadata.dependencies: must be an array");
    else {
      if (deps.length > MAX.dependencies) details.push(`metadata.dependencies: more than ${MAX.dependencies} entries`);
      const seen = new Set<string>();
      deps.forEach((d: unknown, i: number) => {
        const at = `metadata.dependencies[${i}]`;
        if (!isObject(d)) {
          details.push(`${at}: must be an object with \`id\` and \`constraint\``);
          return;
        }
        if (typeof d.id !== "string" || !isValidId(d.id)) {
          details.push(`${at}.id: \`${String(d.id)}\` is not a package id (${ID_RE.source})`);
          return;
        }
        let constraint = "*";
        if (d.constraint !== undefined && d.constraint !== null) {
          if (typeof d.constraint !== "string") {
            details.push(`${at}.constraint: must be a string`);
            return;
          }
          constraint = d.constraint.trim() === "" ? "*" : d.constraint.trim();
        }
        if (constraint.length > MAX.constraint || parseConstraint(constraint) === null) {
          details.push(`${at}.constraint: \`${constraint}\` is not a version constraint`);
          return;
        }
        if (d.id === id) details.push(`${at}: a package cannot depend on itself`);
        if (seen.has(d.id)) details.push(`${at}: \`${d.id}\` is listed more than once`);
        seen.add(d.id);
        dependencies.push({ id: d.id, constraint });
      });
    }
  }

  const allow = raw.allow_missing_dependencies;
  if (allow !== undefined && allow !== null && typeof allow !== "boolean") details.push("metadata.allow_missing_dependencies: must be a boolean");

  return {
    metadata:
      details.length > 0
        ? null
        : { id, name, version, kind: "package", summary, description, authors, license, website, globals, serverside, clientside, dependencies },
    allowMissingDependencies: allow === true,
    details,
  };
}

/** A `versions` row joined with the publisher's login. */
export interface VersionRow {
  package_id: string;
  version: string;
  metadata: string;
  sha256: string;
  size: number;
  yanked: number;
  published_at: string;
  published_by_login: string;
  docs_status?: string;
  docs_version?: string | null;
}

export interface Urls {
  site: string;
  docs: string;
}

export function trimSlash(url: string): string {
  return url.replace(/\/+$/, "");
}

export function archiveUrl(urls: Urls, id: string, version: string): string {
  return `${trimSlash(urls.site)}/api/v1/packages/${id}/versions/${version}/archive`;
}

export function docsUrl(urls: Urls, id: string, version: string): string {
  return `${trimSlash(urls.docs)}/${id}/${version}/`;
}

/** The full version metadata object, members in API order. */
export function versionMetadata(row: VersionRow, urls: Urls): VersionMetadata {
  const m = JSON.parse(row.metadata) as StoredMetadata;
  return {
    id: row.package_id,
    name: m.name,
    version: row.version,
    kind: "package",
    summary: m.summary ?? null,
    description: m.description ?? null,
    authors: m.authors ?? [],
    license: m.license ?? null,
    website: m.website ?? null,
    globals: m.globals ?? [],
    serverside: m.serverside ?? false,
    clientside: m.clientside ?? false,
    dependencies: m.dependencies ?? [],
    dist: { sha256: row.sha256, size: row.size, url: archiveUrl(urls, row.package_id, row.version) },
    docs: docsUrl(urls, row.package_id, row.version),
    yanked: row.yanked !== 0,
    published_at: row.published_at,
    published_by: row.published_by_login,
  };
}

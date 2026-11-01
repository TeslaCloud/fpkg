// Cache-Control and ETag values, conditional requests and the caches.default wrapper.

import type { Ctx } from "../env";

export const CACHE_CONTROL = {
  archive: "public, max-age=31536000, immutable",
  version: "public, max-age=31536000, immutable",
  package: "public, max-age=60, stale-while-revalidate=600",
  search: "public, max-age=60",
  docs: "public, max-age=86400, stale-while-revalidate=604800",
  docsRedirect: "public, max-age=60",
  page: "public, max-age=60",
  pagePrivate: "private, max-age=60",
  /** Archive members served as text; immutable like the archive. */
  source: "public, max-age=31536000, immutable",
  /** A docs generation failure, retried at most this often per colo. */
  docsFailed: "public, max-age=300",
  noStore: "no-store",
  /** For responses with an ETag but no cache lifetime. */
  revalidate: "no-cache",
} as const;

export const ETAG = {
  archive: (sha256: string) => `"${sha256}"`,
  version: (sha256: string, yanked: boolean) => `"${sha256}-${yanked ? 1 : 0}"`,
  package: (id: string, revision: number) => `"${id}-${revision}"`,
  docs: (fdocVersion: string, sha256: string, pathHash: string) => `"${fdocVersion}-${sha256}-${pathHash}"`,
} as const;

/** Strips a weak validator's `W/`. */
function opaque(tag: string): string {
  return tag.startsWith("W/") ? tag.slice(2) : tag;
}

/**
 * True when the request's `If-None-Match` matches `etag` (RFC 9110 13.1.2:
 * `*` or a list of entity tags, compared weakly).
 */
export function ifNoneMatch(request: Request, etag: string | null): boolean {
  const header = request.headers.get("If-None-Match");
  if (!header || !etag) return false;
  if (header.trim() === "*") return true;
  const want = opaque(etag);
  return header.split(",").some((t) => opaque(t.trim()) === want);
}

/** A `304` carrying the validators and cache headers of the full response. */
export function notModified(etag: string, cacheControl: string, extra: HeadersInit = {}): Response {
  const headers = new Headers(extra);
  headers.set("ETag", etag);
  headers.set("Cache-Control", cacheControl);
  return new Response(null, { status: 304, headers });
}

/** Turns a full response into a `304` if the request already has it. */
export function conditional(request: Request, res: Response): Response {
  const etag = res.headers.get("ETag");
  if (res.status !== 200 || !ifNoneMatch(request, etag)) return res;
  res.body?.cancel().catch(() => {});
  const keep: Record<string, string> = {};
  for (const h of ["Vary", "Expires", "X-Fpkg-Sha256"]) {
    const v = res.headers.get(h);
    if (v) keep[h] = v;
  }
  return notModified(etag!, res.headers.get("Cache-Control") ?? CACHE_CONTROL.revalidate, keep);
}

/**
 * The request's URL (plus `variant`, e.g. FDOC_VERSION) as a plain GET, so
 * conditional and auth headers never vary the entry.
 */
export function cacheKey(request: Request | string, variant?: string): Request {
  const url = new URL(typeof request === "string" ? request : request.url);
  if (variant) url.searchParams.set("__v", variant);
  return new Request(url.toString(), { method: "GET" });
}

function isCacheable(res: Response, cacheNotFound: boolean): boolean {
  const cc = res.headers.get("Cache-Control") ?? "";
  return (res.status === 200 || (cacheNotFound && res.status === 404)) && /\bpublic\b/.test(cc) && !res.headers.has("Set-Cookie");
}

export interface CacheOptions {
  /** Extra cache key component; see `cacheKey`. */
  variant?: string;
  /** Called only when the response was built (a miss), with the response. */
  onMiss?: (res: Response) => void;
  /** Also store public 404s (for their own, short, max-age). */
  cacheNotFound?: boolean;
}

/**
 * Serves from `caches.default`, else `build`s and stores a public `200`.
 * Answers `If-None-Match` with a `304`; a `304` from `build` passes through unstored.
 */
export async function cached(
  request: Request,
  ctx: Ctx,
  build: () => Promise<Response>,
  options: CacheOptions = {},
): Promise<Response> {
  if (request.method !== "GET" && request.method !== "HEAD") return build();
  const cache = caches.default;
  const key = cacheKey(request, options.variant);
  const hit = await cache.match(key);
  if (hit) return conditional(request, hit);
  const res = await build();
  if (isCacheable(res, options.cacheNotFound ?? false)) {
    ctx.waitUntil(cache.put(key, res.clone()).catch((e) => console.error("cache put failed", e)));
    if (res.status === 200) options.onMiss?.(res);
  }
  return conditional(request, res);
}

/** Drops a URL's entry (in this colo) after a mutation. */
export async function purge(url: string, variant?: string): Promise<void> {
  await caches.default.delete(cacheKey(url, variant));
}

/** Stores a response for a URL (in this colo). */
export async function store(url: string, res: Response, variant?: string): Promise<void> {
  await caches.default.put(cacheKey(url, variant), res);
}

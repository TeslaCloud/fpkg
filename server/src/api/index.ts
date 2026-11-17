// The registry API, mounted at /api/v1.

import { Hono, type Context } from "hono";
import type { AppEnv } from "../env";
import { CACHE_CONTROL, ETAG, cached, ifNoneMatch, notModified } from "../lib/cache";
import { getPackageState, getVersionRow, isOwner, loadPackage, ownerLogins, packageDocument, search, urls } from "../lib/db";
import { ApiError, badRequest, errorResponse, forbidden, jsonResponse, notFound, today } from "../lib/http";
import { versionMetadata } from "../lib/metadata";
import { bearerUser, requireUser } from "../lib/auth";
import { addOwner, removeOwner, setYanked, versionResponse } from "../lib/packages";
import { canonicalize, isValidId } from "../lib/version";
import { publish } from "./publish";

export const api = new Hono<AppEnv>();

type C = Context<AppEnv>;

/** The id from the URL; ids outside the grammar cannot exist. */
function packageId(c: C): string {
  const id = c.req.param("id")!;
  if (!isValidId(id)) throw notFound(`no package \`${id}\``);
  return id;
}

/** The URL's version canonicalised; 404 if it is not a version. Reads redirect to it (`canonicalRedirect`); writes accept any spelling. */
function versionParam(c: C, id: string): string {
  const raw = c.req.param("version")!;
  const v = canonicalize(raw);
  if (!v) throw notFound(`no version \`${raw}\` of \`${id}\``);
  return v;
}

/** A 301 to the canonical version URL (`1.0` -> `1.0.0`), so each version has one cache entry for yanks to update. */
function canonicalRedirect(c: C, version: string): Response | null {
  const raw = c.req.param("version")!;
  if (raw === version) return null;
  const url = new URL(c.req.url);
  const segs = url.pathname.split("/");
  const at = segs.indexOf("versions") + 1;
  segs[at] = version;
  url.pathname = segs.join("/");
  return new Response(null, { status: 301, headers: { Location: url.toString(), "Cache-Control": CACHE_CONTROL.version } });
}

/**
 * A pending package exists only for its owners and admins, identified by their bearer token;
 * anyone else, including a bad token (no 401: that would reveal the id is taken), gets the
 * same 404 as for an unknown id. Responses about it are never shared.
 */
async function requireViewer(c: C, id: string): Promise<void> {
  const user = await bearerUser(c.env, c.executionCtx, c.req.raw).catch((e: unknown) => {
    if (e instanceof ApiError && e.status === 401) return null;
    throw e;
  });
  if (!user || (!user.admin && !(await isOwner(c.env.DB, id, user.userId)))) throw notFound(`no package \`${id}\``);
}

/** `res` with `Cache-Control: no-store`: a pending package's response, for its viewer only. */
function unshared(res: Response): Response {
  const out = new Response(res.body, res);
  out.headers.set("Cache-Control", CACHE_CONTROL.noStore);
  return out;
}

// --- Reads ----------------------------------------------------------------

api.get("/packages/:id", (c) => {
  const id = packageId(c);
  return cached(c.req.raw, c.executionCtx, async () => {
    // A revalidation costs one indexed read of `revision`.
    if (c.req.header("If-None-Match")) {
      const state = await getPackageState(c.env.DB, id);
      if (state === null) throw notFound(`no package \`${id}\``);
      const etag = ETAG.package(id, state.revision);
      if (state.approved_at !== null && ifNoneMatch(c.req.raw, etag)) return notModified(etag, CACHE_CONTROL.package);
    }
    const data = await loadPackage(c.env.DB, id);
    if (!data) throw notFound(`no package \`${id}\``);
    const pending = data.pkg.approved_at === null;
    if (pending) await requireViewer(c, id);
    return jsonResponse(packageDocument(data, c.env), 200, {
      ETag: ETAG.package(id, data.pkg.revision),
      "Cache-Control": pending ? CACHE_CONTROL.noStore : CACHE_CONTROL.package,
    });
  });
});

async function versionRead(c: C, id: string, version: string): Promise<Response> {
  const row = await getVersionRow(c.env.DB, id, version);
  if (!row) throw notFound(`no version \`${version}\` of \`${id}\``);
  if (row.package_approved_at !== null) return versionResponse(c.env, row);
  await requireViewer(c, id);
  return unshared(versionResponse(c.env, row));
}

api.get("/packages/:id/versions/:version", (c) => {
  const id = packageId(c);
  const version = versionParam(c, id);
  const moved = canonicalRedirect(c, version);
  if (moved) return moved;
  return cached(c.req.raw, c.executionCtx, () => versionRead(c, id, version));
});

api.get("/packages/:id/versions/:version/archive", (c) => {
  const id = packageId(c);
  const version = versionParam(c, id);
  const moved = canonicalRedirect(c, version);
  if (moved) return moved;
  return cached(
    c.req.raw,
    c.executionCtx,
    async () => {
      const state = await getPackageState(c.env.DB, id);
      if (!state) throw notFound(`no version \`${version}\` of \`${id}\``);
      const pending = state.approved_at === null;
      if (pending) await requireViewer(c, id);
      const obj = await c.env.ARCHIVES.get(`archives/${id}/${version}.zip`);
      if (!obj) throw notFound(`no version \`${version}\` of \`${id}\``);
      const sha = obj.customMetadata?.sha256 ?? "";
      const etag = ETAG.archive(sha);
      const cacheControl = pending ? CACHE_CONTROL.noStore : CACHE_CONTROL.archive;
      const headers: Record<string, string> = {
        "Content-Type": "application/zip",
        "Content-Length": String(obj.size),
        "Content-Disposition": `attachment; filename="${id}-${version}.zip"`,
        ETag: etag,
        "X-Fpkg-Sha256": sha,
        "Cache-Control": cacheControl,
      };
      if (ifNoneMatch(c.req.raw, etag)) {
        await obj.body.cancel();
        return notModified(etag, cacheControl, { "X-Fpkg-Sha256": sha });
      }
      return new Response(obj.body, { status: 200, headers });
    },
    {
      // Counted on a cache miss only.
      onMiss: () =>
        c.executionCtx.waitUntil(
          c.env.DB.prepare(
            `INSERT INTO downloads (package_id, version, day, count) VALUES (?, ?, ?, 1)
               ON CONFLICT (package_id, version, day) DO UPDATE SET count = count + 1`,
          )
            .bind(id, version, today())
            .run()
            .catch((e) => console.error("download count failed", e)),
        ),
    },
  );
});

api.get("/packages/:id/owners", async (c) => {
  const id = packageId(c);
  const state = await getPackageState(c.env.DB, id);
  if (state === null) throw notFound(`no package \`${id}\``);
  if (state.approved_at === null) await requireViewer(c, id);
  const etag = ETAG.package(id, state.revision);
  if (ifNoneMatch(c.req.raw, etag)) return notModified(etag, CACHE_CONTROL.revalidate);
  return jsonResponse({ id, owners: await ownerLogins(c.env.DB, id) }, 200, { ETag: etag, "Cache-Control": CACHE_CONTROL.revalidate });
});

api.get("/search", (c) =>
  cached(c.req.raw, c.executionCtx, async () => {
    const q = c.req.query("q") ?? "";
    if (q.length > 128) throw badRequest("`q` is longer than 128 characters");
    const page = intParam(c.req.query("page"), 1, 1, 10_000, "page");
    const perPage = intParam(c.req.query("per_page"), 20, 1, 100, "per_page");
    return jsonResponse(await search(c.env.DB, q, page, perPage), 200, { "Cache-Control": CACHE_CONTROL.search });
  }),
);

function intParam(raw: string | undefined, dflt: number, min: number, max: number, name: string): number {
  if (raw === undefined || raw === "") return dflt;
  if (!/^\d+$/.test(raw)) throw badRequest(`\`${name}\` must be a positive integer`);
  const n = Number(raw);
  if (n < min || n > max) throw badRequest(`\`${name}\` must be between ${min} and ${max}`);
  return n;
}

api.get("/me", async (c) => {
  const user = await requireUser(c.env, c.executionCtx, c.req.raw);
  return jsonResponse({ login: user.login, token_name: user.tokenName });
});

// --- Writes ---------------------------------------------------------------

api.put("/packages/:id/versions/:version", (c) => publish(c));

for (const [action, yanked] of [["yank", true], ["unyank", false]] as const) {
  api.post(`/packages/:id/versions/:version/${action}`, async (c) => {
    const user = await requireUser(c.env, c.executionCtx, c.req.raw);
    const id = packageId(c);
    const version = versionParam(c, id);
    const row = await getVersionRow(c.env.DB, id, version);
    if (!row) throw notFound(`no version \`${version}\` of \`${id}\``);
    if (!user.admin && !(await isOwner(c.env.DB, id, user.userId))) throw forbidden(`\`${user.login}\` is not an owner of \`${id}\``);
    const fresh = await setYanked(c.env, c.executionCtx, c.req.url, row, yanked);
    return jsonResponse(versionMetadata(fresh, urls(c.env)));
  });
}

api.put("/packages/:id/owners/:login", async (c) => {
  const user = await requireUser(c.env, c.executionCtx, c.req.raw);
  const id = packageId(c);
  const login = c.req.param("login");
  if ((await getPackageState(c.env.DB, id)) === null) throw notFound(`no package \`${id}\``);
  if (!user.admin && !(await isOwner(c.env.DB, id, user.userId))) throw forbidden(`\`${user.login}\` is not an owner of \`${id}\``);
  const { owners } = await addOwner(c.env, c.executionCtx, c.req.url, id, login);
  return jsonResponse({ id, owners });
});

api.delete("/packages/:id/owners/:login", async (c) => {
  const user = await requireUser(c.env, c.executionCtx, c.req.raw);
  const id = packageId(c);
  const login = c.req.param("login");
  if ((await getPackageState(c.env.DB, id)) === null) throw notFound(`no package \`${id}\``);
  if (!(await isOwner(c.env.DB, id, user.userId))) throw forbidden(`\`${user.login}\` is not an owner of \`${id}\``);
  return jsonResponse({ id, owners: await removeOwner(c.env, c.executionCtx, c.req.url, id, login) });
});

// --- Errors ---------------------------------------------------------------

const ROUTES: [RegExp, string[]][] = [
  [/^\/packages\/[^/]+$/, ["GET", "HEAD"]],
  [/^\/packages\/[^/]+\/versions\/[^/]+$/, ["GET", "HEAD", "PUT"]],
  [/^\/packages\/[^/]+\/versions\/[^/]+\/archive$/, ["GET", "HEAD"]],
  [/^\/packages\/[^/]+\/versions\/[^/]+\/(yank|unyank)$/, ["POST"]],
  [/^\/packages\/[^/]+\/owners$/, ["GET", "HEAD"]],
  [/^\/packages\/[^/]+\/owners\/[^/]+$/, ["PUT", "DELETE"]],
  [/^\/search$/, ["GET", "HEAD"]],
  [/^\/me$/, ["GET", "HEAD"]],
];

/** 404, or 405 with `Allow` for a known path; Hono does not carry a sub-app's notFound through `route()`, so index.ts calls this. */
export function apiNotFound(c: Context): Response {
  const path = new URL(c.req.url).pathname.replace(/^\/api\/v1/, "");
  const route = ROUTES.find(([re]) => re.test(path));
  if (route) {
    return errorResponse(new ApiError(405, "method_not_allowed", `${c.req.method} is not allowed here`, undefined, { Allow: route[1].join(", ") }));
  }
  return errorResponse(notFound(`no endpoint ${c.req.method} /api/v1${path}`));
}

api.notFound(apiNotFound);

api.onError((err, c) => {
  if (err instanceof ApiError) return errorResponse(err);
  console.error(`${c.req.method} ${c.req.url}:`, err);
  return errorResponse(new ApiError(500, "internal", "internal server error"));
});

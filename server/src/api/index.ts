// The registry API, mounted at /api/v1.

import { Hono, type Context } from "hono";
import type { AppEnv } from "../env";
import { CACHE_CONTROL, ETAG, cached, ifNoneMatch, notModified, purge, store } from "../lib/cache";
import { getRevision, getVersionRow, isOwner, loadPackage, ownerLogins, packageDocument, search, urls } from "../lib/db";
import { ApiError, badRequest, errorResponse, forbidden, jsonResponse, notFound, now, today } from "../lib/http";
import { versionMetadata } from "../lib/metadata";
import { requireUser } from "../lib/auth";
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

function packageUrl(c: C, id: string): string {
  return new URL(`/api/v1/packages/${id}`, c.req.url).toString();
}

function versionUrl(c: C, id: string, version: string): string {
  return new URL(`/api/v1/packages/${id}/versions/${version}`, c.req.url).toString();
}

// --- Reads ----------------------------------------------------------------

api.get("/packages/:id", (c) => {
  const id = packageId(c);
  return cached(c.req.raw, c.executionCtx, async () => {
    // A revalidation costs one indexed read of `revision`.
    if (c.req.header("If-None-Match")) {
      const revision = await getRevision(c.env.DB, id);
      if (revision === null) throw notFound(`no package \`${id}\``);
      const etag = ETAG.package(id, revision);
      if (ifNoneMatch(c.req.raw, etag)) return notModified(etag, CACHE_CONTROL.package);
    }
    const data = await loadPackage(c.env.DB, id);
    if (!data) throw notFound(`no package \`${id}\``);
    return jsonResponse(packageDocument(data, c.env), 200, {
      ETag: ETAG.package(id, data.pkg.revision),
      "Cache-Control": CACHE_CONTROL.package,
    });
  });
});

async function versionResponse(c: C, id: string, version: string): Promise<Response> {
  const row = await getVersionRow(c.env.DB, id, version);
  if (!row) throw notFound(`no version \`${version}\` of \`${id}\``);
  return jsonResponse(versionMetadata(row, urls(c.env)), 200, {
    ETag: ETAG.version(row.sha256, row.yanked !== 0),
    "Cache-Control": CACHE_CONTROL.version,
  });
}

api.get("/packages/:id/versions/:version", (c) => {
  const id = packageId(c);
  const version = versionParam(c, id);
  const moved = canonicalRedirect(c, version);
  if (moved) return moved;
  return cached(c.req.raw, c.executionCtx, () => versionResponse(c, id, version));
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
      const obj = await c.env.ARCHIVES.get(`archives/${id}/${version}.zip`);
      if (!obj) throw notFound(`no version \`${version}\` of \`${id}\``);
      const sha = obj.customMetadata?.sha256 ?? "";
      const etag = ETAG.archive(sha);
      const headers: Record<string, string> = {
        "Content-Type": "application/zip",
        "Content-Length": String(obj.size),
        "Content-Disposition": `attachment; filename="${id}-${version}.zip"`,
        ETag: etag,
        "X-Fpkg-Sha256": sha,
        "Cache-Control": CACHE_CONTROL.archive,
      };
      if (ifNoneMatch(c.req.raw, etag)) {
        await obj.body.cancel();
        return notModified(etag, CACHE_CONTROL.archive, { "X-Fpkg-Sha256": sha });
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
  const revision = await getRevision(c.env.DB, id);
  if (revision === null) throw notFound(`no package \`${id}\``);
  const etag = ETAG.package(id, revision);
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
    if ((row.yanked !== 0) !== yanked) {
      const at = now();
      await c.env.DB.batch([
        c.env.DB.prepare("UPDATE versions SET yanked = ? WHERE package_id = ? AND version = ?").bind(yanked ? 1 : 0, id, version),
        c.env.DB.prepare("UPDATE packages SET revision = revision + 1, updated_at = ? WHERE id = ?").bind(at, id),
      ]);
      // This colo gets the new version body at once; others age out.
      const fresh = await versionResponse(c, id, version);
      c.executionCtx.waitUntil(
        Promise.all([store(versionUrl(c, id, version), fresh.clone()), purge(packageUrl(c, id))]).catch((e) => console.error("cache update failed", e)),
      );
      return jsonResponse(await fresh.json());
    }
    return jsonResponse(versionMetadata(row, urls(c.env)));
  });
}

api.put("/packages/:id/owners/:login", async (c) => {
  const user = await requireUser(c.env, c.executionCtx, c.req.raw);
  const id = packageId(c);
  const login = c.req.param("login");
  if ((await getRevision(c.env.DB, id)) === null) throw notFound(`no package \`${id}\``);
  if (!user.admin && !(await isOwner(c.env.DB, id, user.userId))) throw forbidden(`\`${user.login}\` is not an owner of \`${id}\``);
  const target = await c.env.DB.prepare("SELECT id, login FROM users WHERE lower(login) = lower(?) ORDER BY id DESC LIMIT 1")
    .bind(login)
    .first<{ id: number; login: string }>();
  if (!target) throw new ApiError(404, "user_not_found", `no user \`${login}\`: they need to log in to the registry once first`);
  const at = now();
  const [inserted] = await c.env.DB.batch([
    c.env.DB.prepare("INSERT OR IGNORE INTO owners (package_id, user_id, added_at) VALUES (?, ?, ?)").bind(id, target.id, at),
    c.env.DB.prepare(
      "UPDATE packages SET revision = revision + 1, updated_at = ? WHERE id = ? AND (SELECT changes()) > 0",
    ).bind(at, id),
  ]);
  if ((inserted!.meta.changes ?? 0) > 0) c.executionCtx.waitUntil(purge(packageUrl(c, id)).catch(() => {}));
  return jsonResponse({ id, owners: await ownerLogins(c.env.DB, id) });
});

api.delete("/packages/:id/owners/:login", async (c) => {
  const user = await requireUser(c.env, c.executionCtx, c.req.raw);
  const id = packageId(c);
  const login = c.req.param("login");
  if ((await getRevision(c.env.DB, id)) === null) throw notFound(`no package \`${id}\``);
  if (!(await isOwner(c.env.DB, id, user.userId))) throw forbidden(`\`${user.login}\` is not an owner of \`${id}\``);
  const target = await c.env.DB.prepare(
    "SELECT u.id FROM owners o JOIN users u ON u.id = o.user_id WHERE o.package_id = ? AND lower(u.login) = lower(?)",
  )
    .bind(id, login)
    .first<{ id: number }>();
  if (!target) throw new ApiError(404, "not_found", `\`${login}\` is not an owner of \`${id}\``);
  const at = now();
  // The count guard keeps the last owner even under concurrent removals.
  const [deleted] = await c.env.DB.batch([
    c.env.DB.prepare(
      "DELETE FROM owners WHERE package_id = ?1 AND user_id = ?2 AND (SELECT COUNT(*) FROM owners WHERE package_id = ?1) > 1",
    ).bind(id, target.id),
    c.env.DB.prepare("UPDATE packages SET revision = revision + 1, updated_at = ? WHERE id = ? AND (SELECT changes()) > 0").bind(at, id),
  ]);
  if ((deleted!.meta.changes ?? 0) === 0) throw new ApiError(409, "last_owner", `\`${login}\` is the last owner of \`${id}\` and cannot be removed`);
  c.executionCtx.waitUntil(purge(packageUrl(c, id)).catch(() => {}));
  return jsonResponse({ id, owners: await ownerLogins(c.env.DB, id) });
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

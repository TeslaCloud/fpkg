// End to end against the Worker with local D1, R2, queue and cache.

import { SELF, createExecutionContext, createMessageBatch, env, getQueueResult, waitOnExecutionContext } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { enqueueDocs, fdocThemeScriptHash } from "../src/docs/generate";
import { generateToken, sha256Hex, signSession } from "../src/lib/auth";
import { packageZip, zip } from "./zipwriter";

const SITE = "https://fluxpkg.dev";
const API = `${SITE}/api/v1`;
const DOCS = "https://docs.fluxpkg.dev";
const run = Math.floor(Math.random() * 1e6);

let nextGithubId = 1000 + run;

async function seedUser(login: string): Promise<{ id: number; token: string }> {
  const user = await env.DB.prepare("INSERT INTO users (github_id, login, created_at) VALUES (?, ?, ?) RETURNING id")
    .bind(nextGithubId++, login, "2026-11-01T00:00:00Z")
    .first<{ id: number }>();
  const token = generateToken();
  await env.DB.prepare("INSERT INTO tokens (user_id, name, hash, created_at) VALUES (?, ?, ?, ?)")
    .bind(user!.id, `${login}-token`, await sha256Hex(token), "2026-11-01T00:00:00Z")
    .run();
  return { id: user!.id, token };
}

function meta(id: string, version: string, extra: Record<string, unknown> = {}) {
  return {
    id,
    name: "Test Package",
    version,
    kind: "package",
    summary: "A test package.",
    description: "A package for the end-to-end test.",
    authors: ["Tester"],
    license: "MIT",
    website: "https://example.com",
    globals: [],
    serverside: false,
    clientside: false,
    dependencies: [],
    ...extra,
  };
}

async function publish(token: string | null, id: string, version: string, archive: Uint8Array, metadata: unknown = meta(id, version)) {
  const form = new FormData();
  form.append("metadata", JSON.stringify(metadata));
  form.append("archive", new File([archive], `${id}-${version}.zip`, { type: "application/zip" }));
  return SELF.fetch(`${API}/packages/${id}/versions/${version}`, {
    method: "PUT",
    body: form,
    headers: token ? { Authorization: `Bearer ${token}` } : {},
  });
}

const auth = (token: string) => ({ headers: { Authorization: `Bearer ${token}` } });

describe("the registry API", () => {
  it("publishes, serves, revalidates, yanks and manages owners", async () => {
    const id = `e2e_${run}`;
    const dep = `e2e_dep_${run}`;
    const luna = await seedUser(`luna${run}`);
    const other = await seedUser(`other${run}`);
    const archive = packageZip(id);
    const sha = await sha256Hex(archive);

    // Auth and validation failures, with the error envelope.
    let res = await publish(null, id, "1.0.0", archive);
    expect(res.status).toBe(401);
    expect(res.headers.get("X-Fpkg-Api")).toBe("1");
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("unauthorized");

    res = await publish(luna.token, id, "1.0", archive);
    expect(res.status).toBe(422);
    let body = (await res.json()) as { error: { code: string; details: string[] } };
    expect(body.error.code).toBe("validation_failed");
    expect(body.error.details.join()).toMatch(/must be written canonically, as `1.0.0`/);

    res = await publish(luna.token, id, "2.0.0", archive, meta(id, "2.0.0", { name: "Other Name", website: "javascript:alert(1)" }));
    expect(res.status).toBe(422);
    body = (await res.json()) as typeof body;
    expect(body.error.details.join("\n")).toMatch(/is not the published version 2.0.0/);
    expect(body.error.details.join("\n")).toMatch(/disagrees with the metadata's name/);
    expect(body.error.details.join("\n")).toMatch(/website: must be an http or https URL/);

    const bad = zip([{ name: `${id}/packagespec.lua`, data: "s.version = '1.0.0'" }, { name: `${id}/../x.lua`, data: "x" }]);
    res = await publish(luna.token, id, "1.0.0", bad);
    expect(res.status).toBe(422);
    expect(((await res.json()) as typeof body).error.details.join()).toMatch(/archive: member .* has a `\.\.` component/);

    const withDep = meta(id, "1.0.0", { dependencies: [{ id: dep, constraint: "~> 1.0" }] });
    res = await publish(luna.token, id, "1.0.0", archive, withDep);
    expect(res.status).toBe(422);
    expect(((await res.json()) as typeof body).error.details.join()).toMatch(new RegExp(`dependency \`${dep}\`: not on the registry`));

    // Publish with allow_missing_dependencies, then the conflict.
    res = await publish(luna.token, id, "1.0.0", archive, { ...withDep, allow_missing_dependencies: true });
    expect(res.status).toBe(201);
    const published = (await res.json()) as Record<string, any>;
    expect(published).toMatchObject({
      id,
      version: "1.0.0",
      kind: "package",
      name: "Test Package",
      dependencies: [{ id: dep, constraint: "~> 1.0" }],
      dist: { sha256: sha, size: archive.length, url: `${API}/packages/${id}/versions/1.0.0/archive` },
      docs: `${DOCS}/${id}/1.0.0/`,
      yanked: false,
      published_by: `luna${run}`,
    });
    expect(Object.keys(published)).toEqual([
      "id", "name", "version", "kind", "summary", "description", "authors", "license", "website", "globals",
      "serverside", "clientside", "dependencies", "dist", "docs", "yanked", "published_at", "published_by",
    ]);
    expect(published.published_at).toMatch(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
    expect((await publish(luna.token, id, "1.0.0", archive)).status).toBe(409);
    expect((await publish(other.token, id, "1.1.0", packageZip(id, "1.1.0"), meta(id, "1.1.0"))).status).toBe(403);

    // The package document, with its ETag and a 304.
    res = await SELF.fetch(`${API}/packages/${id}`);
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=60, stale-while-revalidate=600");
    expect(res.headers.get("ETag")).toBe(`"${id}-1"`);
    const doc = (await res.json()) as Record<string, any>;
    expect(doc).toMatchObject({ id, revision: 1, owners: [`luna${run}`], latest: "1.0.0" });
    expect(Object.keys(doc.versions)).toEqual(["1.0.0"]);
    res = await SELF.fetch(`${API}/packages/${id}`, { headers: { "If-None-Match": `"${id}-1"` } });
    expect(res.status).toBe(304);
    expect(res.headers.get("ETag")).toBe(`"${id}-1"`);

    // A second version moves the revision and `latest`.
    expect((await publish(luna.token, id, "1.1.0", packageZip(id, "1.1.0"), meta(id, "1.1.0"))).status).toBe(201);
    res = await SELF.fetch(`${API}/packages/${id}`, { headers: { "If-None-Match": `"${id}-1"` } });
    expect(res.status).toBe(200);
    expect(((await res.json()) as Record<string, any>)).toMatchObject({ revision: 2, latest: "1.1.0" });

    // One version, immutable, and `1.1` finds `1.1.0`.
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.0.0`);
    expect(res.headers.get("ETag")).toBe(`"${sha}-0"`);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=31536000, immutable");
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.0.0`, { headers: { "If-None-Match": `"${sha}-0"` } })).status).toBe(304);
    res = await SELF.fetch(`${API}/packages/${id}/versions/01.1`, { redirect: "manual" });
    expect(res.status).toBe(301);
    expect(res.headers.get("Location")).toBe(`${API}/packages/${id}/versions/1.1.0`);
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.0/archive`, { redirect: "manual" });
    expect(res.status).toBe(301);
    expect(res.headers.get("Location")).toBe(`${API}/packages/${id}/versions/1.0.0/archive`);
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.1`)).status).toBe(200);
    expect((await SELF.fetch(`${API}/packages/${id}/versions/9.9.9`)).status).toBe(404);

    // The archive, byte for byte, then a 304 without a body.
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.0.0/archive`);
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toBe("application/zip");
    expect(res.headers.get("ETag")).toBe(`"${sha}"`);
    expect(res.headers.get("X-Fpkg-Sha256")).toBe(sha);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=31536000, immutable");
    expect(new Uint8Array(await res.arrayBuffer())).toEqual(archive);
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.0.0/archive`, { headers: { "If-None-Match": `"${sha}"` } });
    expect(res.status).toBe(304);
    expect(await res.text()).toBe("");
    const stored = await env.ARCHIVES.head(`archives/${id}/1.0.0.zip`);
    expect(stored?.httpMetadata?.cacheControl).toBe("public, max-age=31536000, immutable");
    expect(stored?.customMetadata?.sha256).toBe(sha);

    // Yank: the version body and the package document change.
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.1.0/yank`, { method: "POST" })).status).toBe(401);
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.1.0/yank`, { method: "POST", ...auth(other.token) })).status).toBe(403);
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.1.0/yank`, { method: "POST", ...auth(luna.token) });
    expect(res.status).toBe(200);
    expect(((await res.json()) as { yanked: boolean }).yanked).toBe(true);
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.1.0`);
    expect(res.headers.get("ETag")).toMatch(/-1"$/);
    // Every spelling of the version reaches the one, updated, cache entry.
    res = await SELF.fetch(`${API}/packages/${id}/versions/01.1`);
    expect(((await res.json()) as { yanked: boolean }).yanked).toBe(true);
    res = await SELF.fetch(`${API}/packages/${id}`);
    expect(((await res.json()) as Record<string, any>)).toMatchObject({ revision: 3, latest: "1.0.0" });
    // A yanked version still downloads (locks keep working).
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.1.0/archive`)).status).toBe(200);
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.1.0/unyank`, { method: "POST", ...auth(luna.token) });
    expect(((await res.json()) as { yanked: boolean }).yanked).toBe(false);

    // An admin can yank anything.
    const admin = await seedUser("admin");
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.1.0/yank`, { method: "POST", ...auth(admin.token) })).status).toBe(200);

    // Owners: add, list, refuse the last one's removal.
    res = await SELF.fetch(`${API}/packages/${id}/owners/other${run}`, { method: "PUT", ...auth(other.token) });
    expect(res.status).toBe(403);
    res = await SELF.fetch(`${API}/packages/${id}/owners/OTHER${run}`, { method: "PUT", ...auth(luna.token) });
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ id, owners: [`luna${run}`, `other${run}`] });
    const revisionNow = async () => ((await (await SELF.fetch(`${API}/packages/${id}`)).json()) as { revision: number }).revision;
    expect(await revisionNow()).toBe(6); // 2 publishes, yank, unyank, admin yank, owner added
    await SELF.fetch(`${API}/packages/${id}/owners/other${run}`, { method: "PUT", ...auth(luna.token) });
    expect(await revisionNow()).toBe(6); // already an owner: nothing moved
    expect((await SELF.fetch(`${API}/packages/${id}/owners/nobody_${run}`, { method: "PUT", ...auth(luna.token) })).status).toBe(404);
    res = await SELF.fetch(`${API}/packages/${id}/owners`);
    expect(await res.json()).toEqual({ id, owners: [`luna${run}`, `other${run}`] });
    const ownersTag = res.headers.get("ETag")!;
    expect((await SELF.fetch(`${API}/packages/${id}/owners`, { headers: { "If-None-Match": ownersTag } })).status).toBe(304);
    res = await SELF.fetch(`${API}/packages/${id}/owners/luna${run}`, { method: "DELETE", ...auth(other.token) });
    expect(await res.json()).toEqual({ id, owners: [`other${run}`] });
    expect(await revisionNow()).toBe(7);
    res = await SELF.fetch(`${API}/packages/${id}/owners/other${run}`, { method: "DELETE", ...auth(other.token) });
    expect(res.status).toBe(409);
    expect(await revisionNow()).toBe(7);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("last_owner");

    // The now satisfied dependency check.
    expect((await publish(luna.token, dep, "1.2.0", packageZip(dep, "1.2.0"), meta(dep, "1.2.0"))).status).toBe(201);
    const dependent = `e2e_user_${run}`;
    const withSatisfiedDep = meta(dependent, "0.1.0", { dependencies: [{ id: dep, constraint: "~> 1.0" }] });
    expect((await publish(luna.token, dependent, "0.1.0", packageZip(dependent, "0.1.0"), withSatisfiedDep)).status).toBe(201);
    const unsatisfied = meta(dependent, "0.2.0", { dependencies: [{ id: dep, constraint: ">= 2.0" }] });
    res = await publish(luna.token, dependent, "0.2.0", packageZip(dependent, "0.2.0"), unsatisfied);
    expect(((await res.json()) as typeof body).error.details.join()).toMatch(/no published version satisfies it \(have 1.2.0\)/);

    // Search, me, and the error shapes.
    res = await SELF.fetch(`${API}/search?q=e2e_dep_${run}`);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=60");
    const hits = (await res.json()) as { total: number; packages: { id: string; version: string }[] };
    expect(hits.packages[0]).toMatchObject({ id: dep, version: "1.2.0" });
    res = await SELF.fetch(`${API}/search?q=${run}&per_page=2&page=1`);
    expect((await res.json()) as Record<string, unknown>).toMatchObject({ total: 3, page: 1, per_page: 2 });
    expect(await (await SELF.fetch(`${API}/me`, auth(luna.token))).json()).toEqual({ login: `luna${run}`, token_name: `luna${run}-token` });
    expect((await SELF.fetch(`${API}/me`)).status).toBe(401);
    expect((await SELF.fetch(`${API}/me`, auth("fpk_" + "x".repeat(43)))).status).toBe(401);
    res = await SELF.fetch(`${API}/packages/${id}`, { method: "PATCH" });
    expect(res.status).toBe(405);
    expect(res.headers.get("Allow")).toBe("GET, HEAD");
    res = await SELF.fetch(`${API}/nothing/here`);
    expect(res.status).toBe(404);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("not_found");

    // A revoked token stops working.
    await env.DB.prepare("UPDATE tokens SET revoked_at = '2026-11-01T00:00:00Z' WHERE hash = ?").bind(await sha256Hex(other.token)).run();
    expect((await SELF.fetch(`${API}/me`, auth(other.token))).status).toBe(401);
  });

  it("refuses bodies over the limit with 413", async () => {
    const user = await seedUser(`big${run}`);
    const id = `e2e_big_${run}`;
    const res = await SELF.fetch(`${API}/packages/${id}/versions/1.0.0`, {
      method: "PUT",
      headers: { Authorization: `Bearer ${user.token}`, "Content-Type": "multipart/form-data; boundary=x", "Content-Length": String(80 * 1024 * 1024) },
      body: "--x--",
    });
    expect(res.status).toBe(413);
    expect(((await res.json()) as { error: { code: string } }).error.code).toBe("payload_too_large");
  });
});

describe("docs", () => {
  it("generates docs from the queue and on demand, and serves them", async () => {
    const user = await seedUser(`docs${run}`);
    const id = `e2e_docs_${run}`;
    const archive = packageZip(id, "1.0.0", "Docs Package");
    const sha = await sha256Hex(archive);
    expect((await publish(user.token, id, "1.0.0", archive, { ...meta(id, "1.0.0"), name: "Docs Package" })).status).toBe(201);

    // The queue consumer, at its own FDOC_VERSION prefix.
    const queued = { ...env, FDOC_VERSION: `q${run}` };
    const batch = createMessageBatch("fluxpkg-docs", [{ id: "m1", timestamp: Date.now(), attempts: 1, body: { id, version: "1.0.0" } }]);
    const qctx = createExecutionContext();
    await worker.queue(batch, queued);
    const result = await getQueueResult(batch, qctx);
    expect(result.explicitAcks).toEqual(["m1"]);
    const listed = await env.DOCS.list({ prefix: `docs/q${run}/${id}/1.0.0/` });
    const paths = listed.objects.map((o) => o.key.slice(`docs/q${run}/${id}/1.0.0/`.length));
    expect(paths).toContain("index.html");
    expect(paths).toContain("assets/app.js");
    const row = await env.DB.prepare("SELECT docs_status, docs_version FROM versions WHERE package_id = ?").bind(id).first();
    expect(row).toMatchObject({ docs_status: "ready" });

    // On demand: a new FDOC_VERSION has nothing in R2 yet.
    const fresh = { ...env, FDOC_VERSION: `od${run}` };
    let ctx = createExecutionContext();
    let res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/`), fresh, ctx);
    await waitOnExecutionContext(ctx);
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toBe("text/html; charset=utf-8");
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=86400, stale-while-revalidate=604800");
    const etag = res.headers.get("ETag")!;
    expect(etag).toMatch(new RegExp(`^"od${run}-${sha}-[0-9a-f]{16}"$`));
    const etagCsp = res.headers.get("Content-Security-Policy");
    expect(etagCsp).toMatch(/script-src 'self' 'sha256-[A-Za-z0-9+/=]+';/);
    const html = await res.text();
    expect(html).toContain("Docs Package");
    expect(await env.DOCS.head(`docs/od${run}/${id}/1.0.0/index.html`)).not.toBeNull();
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/core/Globals.html`), fresh, ctx);
    expect(res.status).toBe(200);
    expect(await res.text()).toContain(`href="${SITE}/packages/${id}/1.0.0/source/lib/sh_${id}.lua#L`);

    // Served again (cache or R2), with a 304 on If-None-Match.
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/`, { headers: { "If-None-Match": etag } }), fresh, ctx);
    expect(res.status).toBe(304);
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/assets/style.css`), fresh, ctx);
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toBe("text/css; charset=utf-8");
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/missing.html`), fresh, ctx);
    expect(res.status).toBe(404);

    // Expired by the R2 lifecycle rule while D1 says ready: regenerated.
    const expired = await env.DOCS.list({ prefix: `docs/od${run}/${id}/1.0.0/` });
    await env.DOCS.delete(expired.objects.map((o) => o.key));
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/core/Globals.html?expired`), fresh, ctx);
    await waitOnExecutionContext(ctx);
    expect(res.status).toBe(200);
    expect(await env.DOCS.head(`docs/od${run}/${id}/1.0.0/index.html`)).not.toBeNull();

    // Too large to generate inside a request: 202 and a queued job.
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/`), { ...env, FDOC_VERSION: `big${run}`, DOCS_ON_DEMAND_BYTES: "10" }, ctx);
    await waitOnExecutionContext(ctx);
    expect(res.status).toBe(202);
    expect(res.headers.get("Retry-After")).toBe("30");
    expect(res.headers.get("Cache-Control")).toBe("no-store");

    // The job is enqueued once until it ages out.
    const dedupe = { ...env, FDOC_VERSION: `dd${run}` };
    expect(await enqueueDocs(dedupe, id, "1.0.0")).toBe(true);
    expect(await enqueueDocs(dedupe, id, "1.0.0")).toBe(false);
    await env.DB.prepare("UPDATE versions SET docs_queued_at = '2000-01-01T00:00:00Z' WHERE package_id = ?").bind(id).run();
    expect(await enqueueDocs(dedupe, id, "1.0.0")).toBe(true);

    // Over the memory cap the queue records a failure, served as a 500 without regenerating.
    const capped = { ...env, FDOC_VERSION: `cap${run}`, DOCS_MAX_SOURCE_BYTES: "10" };
    const capBatch = createMessageBatch("fluxpkg-docs", [{ id: "m2", timestamp: Date.now(), attempts: 1, body: { id, version: "1.0.0" } }]);
    await worker.queue(capBatch, capped);
    expect((await getQueueResult(capBatch, createExecutionContext())).explicitAcks).toEqual(["m2"]);
    const failed = await env.DB.prepare("SELECT docs_status, docs_version, docs_error FROM versions WHERE package_id = ?").bind(id).first<Record<string, string>>();
    expect(failed).toMatchObject({ docs_status: "failed", docs_version: `cap${run}` });
    expect(failed!.docs_error).toMatch(/^fdoc: the package has more than 0\.0 MiB of Lua, Lumen and NL source/);
    ctx = createExecutionContext();
    res = await worker.fetch(new Request(`${DOCS}/${id}/1.0.0/`), capped, ctx);
    expect(res.status).toBe(500);
    expect(await res.text()).toContain("more than 0.0 MiB");

    // The docs CSP allows exactly fdoc's theme script.
    expect(etagCsp).toBe(`default-src 'self'; script-src 'self' ${await fdocThemeScriptHash()}; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`);

    // Redirects and the index.
    res = await SELF.fetch(`${DOCS}/${id}/`, { redirect: "manual" });
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe(`${DOCS}/${id}/1.0.0/`);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=60");
    res = await SELF.fetch(`${DOCS}/${id}/1.0/`, { redirect: "manual" });
    expect(res.headers.get("Location")).toBe(`${DOCS}/${id}/1.0.0/`);
    expect((await SELF.fetch(`${DOCS}/${id}/2.0.0/`)).status).toBe(404);
    expect((await SELF.fetch(`${DOCS}/nope_${run}/`)).status).toBe(404);
    expect(await (await SELF.fetch(`${DOCS}/`)).text()).toContain(`${DOCS}/${id}/1.0.0/`);

    // The source route the docs link to.
    res = await SELF.fetch(`${SITE}/packages/${id}/1.0.0/source/lib/sh_${id}.lua`);
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toBe("text/plain; charset=utf-8");
    expect(await res.text()).toContain(`function ${id}_hello(who)`);
    expect((await SELF.fetch(`${SITE}/packages/${id}/1.0.0/source/nope.lua`)).status).toBe(404);
    // A malformed escape is a missing file, not a server error.
    expect((await SELF.fetch(`${SITE}/packages/${id}/1.0.0/source/%E0`)).status).toBe(404);
    expect((await SELF.fetch(`${SITE}/packages/${id}/1.0.0/source/lib/%E0%A4`)).status).toBe(404);
    expect((await SELF.fetch(`${DOCS}/${id}/1.0.0/%E0`)).status).toBe(404);
    res = await SELF.fetch(`${SITE}/packages/${id}/1.0/source/lib/sh_${id}.lua`, { redirect: "manual" });
    expect(res.status).toBe(301);
    expect(res.headers.get("Location")).toBe(`${SITE}/packages/${id}/1.0.0/source/lib/sh_${id}.lua`);
    // Redirects re-encode what they decoded.
    res = await SELF.fetch(`${DOCS}/${id}/1.0/a%20b%3Fc.html`, { redirect: "manual" });
    expect(res.headers.get("Location")).toBe(`${DOCS}/${id}/1.0.0/a%20b%3Fc.html`);
  });
});

describe("the website", () => {
  it("renders pages with a strict CSP and manages tokens", async () => {
    const user = await seedUser(`web${run}`);
    const id = `e2e_web_${run}`;
    expect((await publish(user.token, id, "1.0.0", packageZip(id))).status).toBe(201);

    let res = await SELF.fetch(`${SITE}/packages/${id}`);
    expect(res.status).toBe(200);
    const csp = res.headers.get("Content-Security-Policy")!;
    expect(csp).toContain("default-src 'none'");
    expect(csp).not.toContain("script-src");
    const page = await res.text();
    expect(page).not.toMatch(/<script/i);
    expect(page).toContain(`s.depends &#39;${id}&#39;, &#39;~&gt; 1.0&#39;`);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=60");
    expect((await SELF.fetch(`${SITE}/`)).status).toBe(200);
    expect(await (await SELF.fetch(`${SITE}/search?q=${id}`)).text()).toContain(`/packages/${id}`);
    expect((await SELF.fetch(`${SITE}/packages/nope_${run}`)).status).toBe(404);

    // Logged out, /tokens sends you to log in.
    res = await SELF.fetch(`${SITE}/tokens`, { redirect: "manual" });
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe("/login?next=/tokens");

    // Logged in (a session cookie signed like the OAuth callback does).
    const cookie = `fpkg_session=${await signSession(env.SESSION_SECRET!, user.id, `web${run}`)}`;
    res = await SELF.fetch(`${SITE}/tokens`, { headers: { Cookie: cookie } });
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("private, no-store");
    const csrf = /name="csrf" value="([^"]+)"/.exec(await res.text())![1]!;

    const form = (fields: Record<string, string>) => {
      const f = new FormData();
      for (const [k, v] of Object.entries(fields)) f.append(k, v);
      return f;
    };
    res = await SELF.fetch(`${SITE}/tokens`, { method: "POST", headers: { Cookie: cookie }, body: form({ name: "laptop" }) });
    expect(res.status).toBe(403);
    res = await SELF.fetch(`${SITE}/tokens`, { method: "POST", headers: { Cookie: cookie, Origin: "https://evil.example" }, body: form({ name: "x", csrf }) });
    expect(res.status).toBe(403);
    res = await SELF.fetch(`${SITE}/tokens`, { method: "POST", headers: { Cookie: cookie }, body: form({ name: "laptop", csrf }) });
    expect(res.status).toBe(201);
    const token = /<code class="token">(fpk_[A-Za-z0-9_-]{43})<\/code>/.exec(await res.text())![1]!;
    expect(await (await SELF.fetch(`${API}/me`, auth(token))).json()).toEqual({ login: `web${run}`, token_name: "laptop" });

    // The list shows it with its last use; revoking stops it.
    const listing = await (await SELF.fetch(`${SITE}/tokens`, { headers: { Cookie: cookie } })).text();
    expect(listing).toContain("laptop");
    expect(listing).not.toContain(token);
    const tid = (await env.DB.prepare("SELECT id FROM tokens WHERE hash = ?").bind(await sha256Hex(token)).first<{ id: number }>())!.id;
    res = await SELF.fetch(`${SITE}/tokens/${tid}/revoke`, { method: "POST", headers: { Cookie: cookie }, body: form({ csrf }), redirect: "manual" });
    expect(res.status).toBe(303);
    expect((await SELF.fetch(`${API}/me`, auth(token))).status).toBe(401);

    // A forged cookie is just a visitor.
    res = await SELF.fetch(`${SITE}/tokens`, { headers: { Cookie: "fpkg_session=abc.def" }, redirect: "manual" });
    expect(res.status).toBe(302);
  });
});

#!/usr/bin/env node
// Usage: node scripts/smoke.mjs <registry-url> <token> [docs-url]
// Publishes a throwaway package and checks the API, caching and docs end to end.

import { zipSync } from "fflate";
import { createHash } from "node:crypto";

const [registry, token, docsArg] = process.argv.slice(2);
if (!registry || !token) {
  console.error("usage: node scripts/smoke.mjs <registry-url> <token> [docs-url]");
  process.exit(2);
}
const base = registry.replace(/\/+$/, "");
const api = `${base}/api/v1`;
const docs = (docsArg ?? `${base}/_docs`).replace(/\/+$/, "");
const id = `smoke_${Date.now().toString(36)}`;
const auth = { Authorization: `Bearer ${token}` };
let failures = 0;

function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${ok || !detail ? "" : `: ${detail}`}`);
  if (!ok) failures++;
}

const enc = new TextEncoder();
const archive = zipSync(
  {
    [`${id}/packagespec.lua`]: enc.encode(`Package:describe(function(s)\n  s.name = 'Smoke'\n  s.version = '1.0.0'\n  s.summary = 'Smoke test.'\nend)\n`),
    [`${id}/lib/sh_smoke.lua`]: enc.encode(`--- Says hello.\n-- @param who [String] whom to greet\nfunction smoke_hello(who)\n  print('hello ' .. who)\nend\n`),
  },
  { level: 6, mtime: new Date(1980, 0, 1) },
);
const sha = createHash("sha256").update(archive).digest("hex");
const metadata = { id, name: "Smoke", version: "1.0.0", kind: "package", summary: "Smoke test.", authors: ["smoke"], dependencies: [] };

async function publish(meta = metadata, body = archive, version = "1.0.0") {
  const form = new FormData();
  form.append("metadata", JSON.stringify(meta));
  form.append("archive", new Blob([body], { type: "application/zip" }), `${id}.zip`);
  return fetch(`${api}/packages/${id}/versions/${version}`, { method: "PUT", body: form, headers: auth });
}

let res = await fetch(`${api}/me`, { headers: auth });
const me = await res.json().catch(() => ({}));
check("GET /me with the token", res.status === 200 && typeof me.login === "string", `${res.status} ${JSON.stringify(me)}`);
check("every response carries X-Fpkg-Api: 1", res.headers.get("x-fpkg-api") === "1");

res = await publish({ ...metadata, version: "2.0.0" }, archive, "2.0.0");
const invalid = await res.json().catch(() => ({}));
check("422 with details when the spec's version disagrees", res.status === 422 && Array.isArray(invalid.error?.details), `${res.status}`);

res = await publish();
const published = await res.json().catch(() => ({}));
check("PUT publish -> 201 with dist", res.status === 201 && published.dist?.sha256 === sha, `${res.status} ${JSON.stringify(published).slice(0, 200)}`);
check("PUT the same version again -> 409", (await publish()).status === 409);

res = await fetch(`${api}/packages/${id}`);
const etag = res.headers.get("etag");
const doc = await res.json().catch(() => ({}));
check("GET the package document", res.status === 200 && doc.latest === "1.0.0" && etag === `"${id}-${doc.revision}"`, `${res.status} ${etag}`);
res = await fetch(`${api}/packages/${id}`, { headers: { "If-None-Match": etag } });
check("If-None-Match on the package document -> 304", res.status === 304, `${res.status}`);

res = await fetch(`${api}/packages/${id}/versions/1.0.0`);
check("GET the version: immutable, ETag sha-0", res.status === 200 && res.headers.get("etag") === `"${sha}-0"` && /immutable/.test(res.headers.get("cache-control") ?? ""));

res = await fetch(`${api}/packages/${id}/versions/1.0.0/archive`);
const bytes = new Uint8Array(await res.arrayBuffer());
check(
  "GET the archive: same bytes, ETag and X-Fpkg-Sha256",
  res.status === 200 && createHash("sha256").update(bytes).digest("hex") === sha && res.headers.get("x-fpkg-sha256") === sha && res.headers.get("etag") === `"${sha}"`,
);
res = await fetch(`${api}/packages/${id}/versions/1.0.0/archive`, { headers: { "If-None-Match": `"${sha}"` } });
check("If-None-Match on the archive -> 304", res.status === 304, `${res.status}`);

res = await fetch(`${api}/search?q=${id}`);
const hits = await res.json().catch(() => ({}));
check("search finds it", res.status === 200 && hits.packages?.[0]?.id === id, JSON.stringify(hits).slice(0, 200));

res = await fetch(`${api}/packages/${id}/versions/1.0.0/yank`, { method: "POST", headers: auth });
check("yank", res.status === 200 && (await res.json()).yanked === true);
res = await fetch(`${api}/packages/${id}`, { headers: { "If-None-Match": etag } });
const afterYank = await res.json().catch(() => ({}));
check("the package document moved after the yank", res.status === 200 && afterYank.latest === null && afterYank.revision > doc.revision);
res = await fetch(`${api}/packages/${id}/versions/1.0.0/unyank`, { method: "POST", headers: auth });
check("unyank", res.status === 200 && (await res.json()).yanked === false);

res = await fetch(`${api}/packages/${id}/owners`);
check("owners", res.status === 200 && (await res.json()).owners?.[0] === me.login);
res = await fetch(`${api}/packages/${id}/owners/${me.login}`, { method: "DELETE", headers: auth });
check("the last owner cannot be removed -> 409", res.status === 409);

// Docs: the queue consumer or on-demand generation, whichever is first.
let page = null;
for (let i = 0; i < 60; i++) {
  res = await fetch(`${docs}/${id}/1.0.0/`);
  if (res.status === 200) {
    page = await res.text();
    break;
  }
  if (res.status !== 202) break;
  await new Promise((r) => setTimeout(r, 500));
}
check("docs page generated by fdoc (wasm)", page !== null && page.includes("Smoke") && page.includes('name="generator" content="fdoc"'), `${res.status}`);
res = await fetch(`${docs}/${id}/`, { redirect: "manual" });
check("docs /<id>/ redirects to the latest version", res.status === 302 && (res.headers.get("location") ?? "").endsWith(`/${id}/1.0.0/`));
res = await fetch(`${base}/packages/${id}/1.0.0/source/lib/sh_smoke.lua`);
check("the source route serves the member as text/plain", res.status === 200 && (await res.text()).includes("smoke_hello"));

console.log(failures === 0 ? "smoke: all checks passed" : `smoke: ${failures} check(s) failed`);
process.exit(failures === 0 ? 0 : 1);

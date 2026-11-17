// The fluxpkg.dev website: front page, search, package pages, archive sources, GitHub login and
// API tokens. No scripts. Admin pages are in admin.ts.

import { Hono } from "hono";
import type { AppEnv } from "../env";
import {
  SESSION_COOKIE,
  STATE_COOKIE,
  base64url,
  clearCookie,
  generateToken,
  parseCookies,
  requireSecret,
  safeEqual,
  sessionCookie,
  sha256Hex,
  sign,
  signSession,
  verify,
} from "../lib/auth";
import { CACHE_CONTROL, cached } from "../lib/cache";
import { downloadCount, frontPage, getPackageState, isOwner, loadPackage, packageDocument, search, type FrontHit, type SearchHit } from "../lib/db";
import { ApiError, now } from "../lib/http";
import { trimSlash, type VersionMetadata } from "../lib/metadata";
import { canonicalize, isValidId } from "../lib/version";
import { readMemberFromR2 } from "../lib/zip";
import { pathHash } from "../docs/generate";
import { admin } from "./admin";
import { Html, STYLE, compact, day, html, minute, page, size } from "./html";
import { type C, type Visitor, adminOf, render, requirePost, safeNext, secure, site, visitor } from "./session";

export const web = new Hono<AppEnv>();

/** Largest archive member the source route serves. */
const MAX_SOURCE = 16 * 1024 * 1024;
const MAX_ACTIVE_TOKENS = 50;
/** Packages per list on the front page. */
const FRONT_LIST = 8;
const SEARCH_PAGE = 20;

/** The page chrome for a visitor. */
function chrome(c: C, v: Visitor | null) {
  return { user: v ? { login: v.session.login, admin: v.admin } : null, csrf: v?.csrf ?? null, docs: trimSlash(c.env.DOCS_URL) };
}

/** Whether a visitor may see a pending package: its owners and admins (`isAdmin`: already checked with `adminOf`). */
async function canSeePending(c: C, v: Visitor | null, id: string, isAdmin = false): Promise<boolean> {
  if (!v) return false;
  if (isAdmin || (await adminOf(c, v))) return true;
  return isOwner(c.env.DB, id, v.session.uid);
}

// --- Static ---------------------------------------------------------------

web.get("/static/style.css", () =>
  new Response(STYLE, { headers: { "Content-Type": "text/css; charset=utf-8", "Cache-Control": "public, max-age=86400" } }),
);

// --- Pages ----------------------------------------------------------------

function packageRow(p: SearchHit, meta: Html | string): Html {
  return html`<li><div class="row"><a class="name" href="/packages/${p.id}">${p.name}</a><span class="id">${p.id}</span><span class="meta">${meta}</span></div>
    ${p.summary ? html`<div class="summary">${p.summary}</div>` : ""}</li>`;
}

function frontList(title: string, hits: FrontHit[], meta: (h: FrontHit) => string): Html {
  return html`<section class="card"><div class="card-head"><h2>${title}</h2></div>
    ${hits.length > 0 ? html`<ul class="pkgs">${hits.map((h) => packageRow({ ...h, summary: null }, meta(h)))}</ul>` : html`<p class="empty">Nothing yet.</p>`}</section>`;
}

web.get("/", (c) =>
  render(c, async (v) => {
    const fp = await frontPage(c.env.DB, FRONT_LIST);
    const docs = trimSlash(c.env.DOCS_URL);
    const hero = html`<section class="hero"><div class="wrap">
      <h1>Packages for Flux</h1>
      <p>The registry of Flux packages: find one, add it to your schema with <code>fpkg add</code>, and publish your own.</p>
      <form class="search" method="get" action="/search"><input type="search" name="q" placeholder="Search packages" aria-label="Search packages"><button class="btn" type="submit">Search</button></form>
      <div class="stats"><div><b>${fp.packages}</b><span>packages</span></div><div><b>${fp.versions}</b><span>versions</span></div><div><b>${compact(fp.downloads)}</b><span>downloads</span></div></div>
    </div></section>`;
    const body = html`<div class="grid cols-3">
      ${frontList("Recently updated", fp.updated, (h) => `${h.version} · ${day(h.updated_at)}`)}
      ${frontList("Most downloaded", fp.popular, (h) => `${compact(h.downloads)} downloads`)}
      ${frontList("New packages", fp.newest, (h) => day(h.created_at))}
    </div>
    <div class="section-head"><h2>Getting started</h2><a href="${docs}/">Package documentation</a></div>
    <div class="grid cols-3">
      <div class="card"><div class="card-body"><h3>1. Add a package</h3><p class="muted small">From your schema, with the newest version that fits:</p><pre><code>fpkg add cable</code></pre></div></div>
      <div class="card"><div class="card-body"><h3>2. Pin it</h3><p class="muted small"><code>fpkg add</code> writes the dependency to your <code>packagespec.lua</code>:</p><pre><code>s.depends('cable', '~> 1.0')</code></pre></div></div>
      <div class="card"><div class="card-body"><h3>3. Publish yours</h3><p class="muted small">Log in with GitHub, create a token, then from your package:</p><pre><code>fpkg login
fpkg publish</code></pre></div></div>
    </div>`;
    return page({ title: "fluxpkg - packages for Flux", body, hero, ...chrome(c, v) });
  }),
);

web.get("/search", (c) =>
  render(c, async (v) => {
    const q = (c.req.query("q") ?? "").slice(0, 128);
    const pageNo = Math.max(1, Math.min(10_000, Number.parseInt(c.req.query("page") ?? "1", 10) || 1));
    const res = await search(c.env.DB, q, pageNo, SEARCH_PAGE);
    const pages = Math.max(1, Math.ceil(res.total / SEARCH_PAGE));
    const link = (p: number, label: string) => html`<a class="btn sm" href="/search?q=${encodeURIComponent(q)}&amp;page=${p}">${label}</a>`;
    const body = html`<h1>${q ? "Search" : "All packages"}</h1>
      <p class="muted">${res.total} package${res.total === 1 ? "" : "s"}${q ? html` matching <code>${q}</code>` : ""}${pages > 1 ? html`, page ${pageNo} of ${pages}` : ""}.</p>
      <div class="card results">${res.packages.length > 0
        ? html`<ul class="pkgs">${res.packages.map((p) => packageRow(p, html`${p.version} · ${compact(p.downloads)} downloads · ${day(p.updated_at)}`))}</ul>`
        : html`<p class="empty">No package matches. Try a shorter query, or a word from the summary.</p>`}</div>
      ${pages > 1 ? html`<div class="pagination">${pageNo > 1 ? link(pageNo - 1, "Previous") : ""}${pageNo < pages ? link(pageNo + 1, "Next") : ""}</div>` : ""}`;
    return page({ title: q ? `${q} - fluxpkg search` : "All packages - fluxpkg", body, query: q, current: "packages", ...chrome(c, v) });
  }),
);

/** The `s.depends` line for the newest version; the constraint form needs parentheses in Lua. */
export function installLine(id: string, version: string | null): string {
  const m = version ? /^(\d+)\.(\d+)/.exec(version) : null;
  return m ? `s.depends('${id}', '~> ${m[1]}.${m[2]}')` : `s.depends '${id}'`;
}

function versionRows(doc: { versions: Record<string, VersionMetadata> }, pending: boolean): Html {
  return html`<table><thead><tr><th>Version</th><th>Published</th><th>By</th><th class="num">Size</th><th></th></tr></thead><tbody>${Object.values(doc.versions).map(
    (m) => html`<tr><td><code>${m.version}</code> ${m.yanked ? html`<span class="badge warn">yanked</span>` : ""}</td>
      <td>${day(m.published_at)}</td><td><a href="https://github.com/${encodeURIComponent(m.published_by)}" rel="nofollow noopener">${m.published_by}</a></td>
      <td class="num">${size(m.dist.size)}</td>
      <td class="actions-cell">${pending ? html`<a href="/packages/${m.id}/${m.version}/source/packagespec.lua">spec</a>` : html`<a href="${m.docs}">docs</a> · <a href="${m.dist.url}">zip</a>`}</td></tr>`,
  )}</tbody></table>`;
}

function realm(m: VersionMetadata): string {
  return m.serverside ? "Server only" : m.clientside ? "Client only" : "Shared";
}

web.get("/packages/:id", (c) =>
  render(c, async (v) => {
    const id = c.req.param("id");
    const data = isValidId(id) ? await loadPackage(c.env.DB, id) : null;
    if (!data) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
    const pending = data.pkg.approved_at === null;
    const isAdmin = v ? (await adminOf(c, v)) !== null : false;
    if (pending && !(await canSeePending(c, v, id, isAdmin))) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
    const doc = packageDocument(data, c.env);
    const latestId = doc.latest ?? data.versions[0]?.version ?? null;
    const m = latestId ? doc.versions[latestId]! : null;
    const downloads = await downloadCount(c.env.DB, id);
    const body = html`<div class="page-head">
        <div>
          <h1>${m?.name ?? id} <span class="version">${latestId ?? ""}</span>
            ${pending ? html`<span class="badge pend">awaiting approval</span>` : ""}
            ${!pending && doc.latest === null ? html`<span class="badge warn">every version is yanked</span>` : ""}</h1>
          ${m?.summary ? html`<p class="lede">${m.summary}</p>` : ""}
          <p class="muted small"><code>${id}</code></p>
        </div>
        <div class="actions">
          ${m && !pending ? html`<a class="btn primary" href="${m.docs}">Documentation</a>` : ""}
          ${isAdmin ? html`<a class="btn" href="/admin/packages/${id}">Admin</a>` : ""}
        </div>
      </div>
      ${pending
        ? html`<div class="notice pend"><p><strong>This package is awaiting approval.</strong> A registry admin reviews every new package before it becomes public.
            Until then only its owners and admins can see it, nothing can depend on it, and its documentation is not generated.
            ${isAdmin ? html`<a href="/admin/packages/${id}">Review it</a>.` : "Publishing further versions meanwhile is fine."}</p></div>`
        : ""}
      <div class="layout">
        <div>
          <div class="snippet"><div class="snippet-head"><span>packagespec.lua</span><span>or run <code>fpkg add ${id}</code></span></div><pre><code>${installLine(id, doc.latest)}</code></pre></div>
          ${m?.description && m.description !== m.summary ? html`<h2>About</h2><p class="prose">${m.description}</p>` : ""}
          ${m && m.dependencies.length > 0
            ? html`<h2>Dependencies</h2><ul class="deps">${m.dependencies.map((d) => html`<li><a href="/packages/${d.id}">${d.id}</a> <code>${d.constraint}</code></li>`)}</ul>`
            : ""}
          <h2>Versions</h2><div class="card table-wrap">${versionRows(doc, pending)}</div>
        </div>
        <aside>
          <div class="card"><div class="card-head"><h3>Details</h3></div><div class="card-body"><dl class="kv">
            ${m ? html`<dt>Version</dt><dd><code>${m.version}</code></dd>` : ""}
            ${m?.license ? html`<dt>License</dt><dd>${m.license}</dd>` : ""}
            ${m ? html`<dt>Realm</dt><dd>${realm(m)}</dd>` : ""}
            <dt>Downloads</dt><dd>${downloads}</dd>
            ${m ? html`<dt>Published</dt><dd>${day(m.published_at)}</dd>` : ""}
            ${m && m.authors.length > 0 ? html`<dt>Authors</dt><dd>${m.authors.join(", ")}</dd>` : ""}
            ${m && m.globals.length > 0 ? html`<dt>Globals</dt><dd>${m.globals.map((g, i) => html`${i > 0 ? ", " : ""}<code>${g}</code>`)}</dd>` : ""}
          </dl></div></div>
          <div class="card"><div class="card-head"><h3>Links</h3></div><div class="card-body"><ul class="links">
            ${m && !pending ? html`<li><a href="${m.docs}">Documentation</a></li>` : ""}
            ${m?.website ? html`<li><a href="${m.website}" rel="nofollow noopener">Website</a></li>` : ""}
            ${m && !pending ? html`<li><a href="${m.dist.url}">Download ${m.version}</a> <span class="faint small">${size(m.dist.size)}</span></li>` : ""}
            ${m ? html`<li><a href="/packages/${id}/${m.version}/source/packagespec.lua">packagespec.lua</a></li>` : ""}
            ${!pending ? html`<li><a href="/api/v1/packages/${id}">API document</a></li>` : ""}
          </ul></div></div>
          <div class="card"><div class="card-head"><h3>Owners</h3></div><div class="card-body"><ul class="owners">${doc.owners.map(
            (o) => html`<li><img class="avatar" src="https://avatars.githubusercontent.com/${encodeURIComponent(o)}?s=44" alt="" width="22" height="22"><a href="https://github.com/${encodeURIComponent(o)}" rel="nofollow noopener">${o}</a></li>`,
          )}</ul></div></div>
        </aside>
      </div>`;
    return page({
      title: `${m?.name ?? id} - fluxpkg`,
      body,
      current: "packages",
      ...chrome(c, v),
      ...(pending ? { cacheControl: CACHE_CONTROL.noStore } : {}),
    });
  }),
);

// An archive member as text/plain; the docs' "view source" links land here.
web.get("/packages/:id/:version/source/*", (c) => {
  // From the raw path: /packages/<id>/<version>/source/<member path>.
  const raw = new URL(c.req.url).pathname.split("/").slice(2);
  const [id, version] = [raw[0] ?? "", raw[1] ?? ""];
  let comps: string[];
  try {
    comps = raw.slice(3).map(decodeURIComponent);
  } catch {
    throw new ApiError(404, "not_found", "There is no such file.");
  }
  const rel = comps.join("/");
  const canonical = canonicalize(version);
  if (!isValidId(id) || !canonical || rel === "" || comps.some((s) => s === "" || s === "." || s === ".." || s.includes("/")))
    throw new ApiError(404, "not_found", "There is no such file.");
  if (canonical !== version) {
    return new Response(null, {
      status: 301,
      headers: { Location: `${site(c.env)}/packages/${id}/${canonical}/source/${raw.slice(3).join("/")}`, "Cache-Control": CACHE_CONTROL.source },
    });
  }
  // A pending package's files are served to its owners and admins only, and never cached.
  return cached(c.req.raw, c.executionCtx, async () => {
    const state = await getPackageState(c.env.DB, id);
    if (!state) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
    const pending = state.approved_at === null;
    if (pending && !(await canSeePending(c, await visitor(c), id))) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
    const found = await readMemberFromR2(c.env.ARCHIVES, `archives/${id}/${version}.zip`, `${id}/${rel}`, MAX_SOURCE);
    if (!found) throw new ApiError(404, "not_found", `${id} ${version} has no file \`${rel}\`.`);
    if (!found.bytes) throw new ApiError(413, "payload_too_large", `\`${rel}\` is too large to show.`);
    const sha = found.object.customMetadata?.sha256 ?? "";
    return new Response(found.bytes, {
      headers: {
        "Content-Type": "text/plain; charset=utf-8",
        "Cache-Control": pending ? CACHE_CONTROL.noStore : CACHE_CONTROL.source,
        ETag: `"${sha}-${await pathHash(rel)}"`,
        "Content-Security-Policy": "default-src 'none'; sandbox",
        "X-Content-Type-Options": "nosniff",
      },
    });
  });
});

// --- GitHub OAuth ---------------------------------------------------------

web.get("/login", async (c) => {
  const secret = requireSecret(c.env);
  if (!c.env.GITHUB_CLIENT_ID || !c.env.GITHUB_CLIENT_SECRET) throw new ApiError(500, "misconfigured", "GitHub OAuth is not configured (see server/README.md)");
  const nonce = base64url(crypto.getRandomValues(new Uint8Array(16)));
  const state = await sign(secret, "state", { nonce, next: safeNext(c.req.query("next")), exp: Math.floor(Date.now() / 1000) + 600 });
  const authorize = new URL("https://github.com/login/oauth/authorize");
  authorize.searchParams.set("client_id", c.env.GITHUB_CLIENT_ID);
  authorize.searchParams.set("redirect_uri", `${site(c.env)}/auth/github/callback`);
  authorize.searchParams.set("state", state);
  authorize.searchParams.set("allow_signup", "true");
  const headers = new Headers({ Location: authorize.toString(), "Cache-Control": CACHE_CONTROL.noStore });
  headers.append("Set-Cookie", `${STATE_COOKIE}=${nonce}; Path=/auth; HttpOnly;${secure(c.env) ? " Secure;" : ""} SameSite=Lax; Max-Age=600`);
  return new Response(null, { status: 302, headers });
});

web.get("/auth/github/callback", async (c) => {
  const secret = requireSecret(c.env);
  const code = c.req.query("code");
  const stateValue = c.req.query("state");
  if (c.req.query("error")) throw new ApiError(400, "oauth_denied", `GitHub said: ${c.req.query("error_description") ?? c.req.query("error")}`);
  if (!code || !stateValue) throw new ApiError(400, "bad_request", "the callback needs `code` and `state`");
  const state = await verify<{ nonce: string; next: string; exp: number }>(secret, "state", stateValue);
  const nonce = parseCookies(c.req.header("Cookie") ?? null)[STATE_COOKIE];
  if (!state || !nonce || !safeEqual(state.nonce, nonce)) throw new ApiError(400, "bad_state", "the login link has expired or was not started here; try again");

  const tokenRes = await fetch("https://github.com/login/oauth/access_token", {
    method: "POST",
    headers: { Accept: "application/json", "Content-Type": "application/json", "User-Agent": "fluxpkg.dev" },
    body: JSON.stringify({
      client_id: c.env.GITHUB_CLIENT_ID,
      client_secret: c.env.GITHUB_CLIENT_SECRET,
      code,
      redirect_uri: `${site(c.env)}/auth/github/callback`,
    }),
  });
  const tokenBody = (await tokenRes.json().catch(() => ({}))) as { access_token?: string; error_description?: string };
  if (!tokenRes.ok || !tokenBody.access_token) throw new ApiError(502, "oauth_failed", `GitHub refused the login: ${tokenBody.error_description ?? tokenRes.status}`);
  const userRes = await fetch("https://api.github.com/user", {
    headers: { Authorization: `Bearer ${tokenBody.access_token}`, Accept: "application/vnd.github+json", "User-Agent": "fluxpkg.dev" },
  });
  const gh = (await userRes.json().catch(() => ({}))) as { id?: number; login?: string };
  if (!userRes.ok || typeof gh.id !== "number" || typeof gh.login !== "string") throw new ApiError(502, "oauth_failed", "could not read the GitHub user");

  const user = await upsertUser(c.env.DB, gh.id, gh.login);
  const headers = new Headers({ Location: `${site(c.env)}${safeNext(state.next)}`, "Cache-Control": CACHE_CONTROL.noStore });
  headers.append("Set-Cookie", sessionCookie(await signSession(secret, user.id, user.login), secure(c.env)));
  headers.append("Set-Cookie", clearCookie(STATE_COOKIE, secure(c.env)).replace("Path=/", "Path=/auth"));
  return new Response(null, { status: 302, headers });
});

/**
 * Creates or renames the user with this GitHub id. GitHub logins can be reused, so any other row
 * holding the login gets its GitHub id appended, leaving lookups by login one match.
 */
export async function upsertUser(db: D1Database, githubId: number, login: string): Promise<{ id: number; login: string }> {
  const at = now();
  const [, row] = await db.batch([
    db.prepare("UPDATE users SET login = login || '~' || github_id WHERE lower(login) = lower(?) AND github_id != ?").bind(login, githubId),
    db
      .prepare("INSERT INTO users (github_id, login, created_at) VALUES (?, ?, ?) ON CONFLICT (github_id) DO UPDATE SET login = excluded.login RETURNING id, login")
      .bind(githubId, login, at),
  ]);
  return (row!.results as { id: number; login: string }[])[0]!;
}

web.post("/logout", async (c) => {
  await requirePost(c);
  const headers = new Headers({ Location: "/", "Cache-Control": CACHE_CONTROL.noStore });
  headers.append("Set-Cookie", clearCookie(SESSION_COOKIE, secure(c.env)));
  return new Response(null, { status: 303, headers });
});

// --- Tokens ---------------------------------------------------------------

interface TokenRow {
  id: number;
  name: string;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

async function tokensPage(c: C, v: Visitor, extra: { created?: { name: string; token: string }; error?: string } = {}, status = 200): Promise<Response> {
  const rows = (
    await c.env.DB.prepare("SELECT id, name, created_at, last_used_at, revoked_at FROM tokens WHERE user_id = ? ORDER BY id DESC")
      .bind(v.session.uid)
      .all<TokenRow>()
  ).results;
  const body = html`<h1>API tokens</h1>
    <p class="muted">Tokens let <code>fpkg</code> publish, yank and manage owners as <strong>${v.session.login}</strong>. Paste one into <code>fpkg login</code>.</p>
    ${extra.error ? html`<div class="notice error">${extra.error}</div>` : ""}
    ${extra.created
      ? html`<div class="notice ok"><p>Your new token <strong>${extra.created.name}</strong>. Copy it now: it is not shown again.</p>
          <code class="token">${extra.created.token}</code></div>`
      : ""}
    <div class="card"><div class="card-head"><h2>New token</h2></div><div class="card-body">
      <form class="field" method="post" action="/tokens"><input type="hidden" name="csrf" value="${v.csrf}">
        <input type="text" name="name" required maxlength="64" placeholder="Name, such as laptop or ci" aria-label="Token name"> <button class="btn primary" type="submit">Create token</button></form>
    </div></div>
    <h2>Your tokens</h2>
    <div class="card table-wrap">${rows.length === 0
      ? html`<p class="empty">None yet.</p>`
      : html`<table><thead><tr><th>Name</th><th>Created</th><th>Last used</th><th></th></tr></thead><tbody>${rows.map(
          (t) => html`<tr><td>${t.name}</td><td>${day(t.created_at)}</td><td>${t.last_used_at ? minute(t.last_used_at) : "never"}</td>
            <td class="actions-cell">${t.revoked_at
              ? html`<span class="badge quiet">revoked ${day(t.revoked_at)}</span>`
              : html`<form class="inline" method="post" action="/tokens/${t.id}/revoke"><input type="hidden" name="csrf" value="${v.csrf}"><button class="link danger" type="submit">Revoke</button></form>`}</td></tr>`,
        )}</tbody></table>`}</div>`;
  return page({ title: "API tokens - fluxpkg", body, current: "tokens", ...chrome(c, v), status, cacheControl: "private, no-store" });
}

web.get("/tokens", async (c) => {
  const v = await visitor(c);
  if (!v) return new Response(null, { status: 302, headers: { Location: "/login?next=/tokens", "Cache-Control": CACHE_CONTROL.noStore } });
  return tokensPage(c, v);
});

web.post("/tokens", async (c) => {
  const { v, form } = await requirePost(c);
  const name = String(form.get("name") ?? "").trim();
  if (name === "" || name.length > 64) return tokensPage(c, v, { error: "A token needs a name of at most 64 characters." }, 422);
  const active = await c.env.DB.prepare("SELECT COUNT(*) AS n FROM tokens WHERE user_id = ? AND revoked_at IS NULL")
    .bind(v.session.uid)
    .first<{ n: number }>();
  if ((active?.n ?? 0) >= MAX_ACTIVE_TOKENS) return tokensPage(c, v, { error: `You have ${MAX_ACTIVE_TOKENS} active tokens; revoke one first.` }, 429);
  const token = generateToken();
  await c.env.DB.prepare("INSERT INTO tokens (user_id, name, hash, created_at) VALUES (?, ?, ?, ?)")
    .bind(v.session.uid, name, await sha256Hex(token), now())
    .run();
  return tokensPage(c, v, { created: { name, token } }, 201);
});

web.post("/tokens/:tid/revoke", async (c) => {
  const { v } = await requirePost(c);
  const tid = Number(c.req.param("tid"));
  if (!Number.isInteger(tid)) throw new ApiError(404, "not_found", "no such token");
  await c.env.DB.prepare("UPDATE tokens SET revoked_at = ? WHERE id = ? AND user_id = ? AND revoked_at IS NULL").bind(now(), tid, v.session.uid).run();
  return new Response(null, { status: 303, headers: { Location: "/tokens", "Cache-Control": CACHE_CONTROL.noStore } });
});

// --- Admin ----------------------------------------------------------------

web.route("/admin", admin);

// --- Errors ---------------------------------------------------------------

export async function webNotFound(c: C): Promise<Response> {
  const v = await visitor(c);
  return page({ title: "Not found - fluxpkg", body: html`<h1>Not found</h1><p>There is nothing at this address.</p>`, status: 404, ...chrome(c, v) });
}

web.notFound(webNotFound);

export async function webError(err: Error, c: C): Promise<Response> {
  const status = err instanceof ApiError ? err.status : 500;
  if (!(err instanceof ApiError)) console.error(`${c.req.method} ${c.req.url}:`, err);
  const message = err instanceof ApiError ? err.message : "Something went wrong on our side.";
  const v = await visitor(c).catch(() => null);
  return page({
    title: `${status === 404 ? "Not found" : "Error"} - fluxpkg`,
    body: html`<h1>${status === 404 ? "Not found" : status === 403 ? "Not allowed" : "Something went wrong"}</h1><p>${message}</p>`,
    status,
    cacheControl: CACHE_CONTROL.noStore,
    ...chrome(c, v),
  });
}

web.onError(webError);

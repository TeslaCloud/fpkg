// The fluxpkg.dev website: package pages, search, GitHub login and API tokens. No scripts.

import { Hono, type Context } from "hono";
import type { AppEnv, Env } from "../env";
import {
  SESSION_COOKIE,
  STATE_COOKIE,
  base64url,
  clearCookie,
  csrfToken,
  generateToken,
  parseCookies,
  requireSecret,
  safeEqual,
  sessionCookie,
  sha256Hex,
  sign,
  signSession,
  verify,
  verifySession,
  type Session,
} from "../lib/auth";
import { CACHE_CONTROL, cached } from "../lib/cache";
import { downloadCount, loadPackage, packageDocument, search } from "../lib/db";
import { ApiError, now } from "../lib/http";
import { trimSlash, type VersionMetadata } from "../lib/metadata";
import { canonicalize, isValidId } from "../lib/version";
import { readMemberFromR2 } from "../lib/zip";
import { pathHash } from "../docs/generate";
import { Html, STYLE, html, page } from "./html";

export const web = new Hono<AppEnv>();
type C = Context<AppEnv>;

/** Largest archive member the source route serves. */
const MAX_SOURCE = 16 * 1024 * 1024;
const MAX_ACTIVE_TOKENS = 50;

// --- Sessions -------------------------------------------------------------

interface Visitor {
  session: Session;
  csrf: string;
}

/** The logged-in visitor, or null (no cookie, a bad one, or no secret configured). */
async function visitor(c: C): Promise<Visitor | null> {
  const value = parseCookies(c.req.header("Cookie") ?? null)[SESSION_COOKIE];
  const secret = c.env.SESSION_SECRET;
  if (!value || !secret) return null;
  const session = await verifySession(secret, value);
  if (!session) return null;
  return { session, csrf: await csrfToken(secret, value) };
}

/** Renders for a visitor; anonymous pages go through the cache, logged-in ones are private. */
async function render(c: C, build: (v: Visitor | null) => Promise<Response>): Promise<Response> {
  const v = await visitor(c);
  if (v) return build(v);
  return cached(c.req.raw, c.executionCtx, () => build(null));
}

function secure(env: Env): boolean {
  return env.SITE_URL.startsWith("https:");
}

function site(env: Env): string {
  return trimSlash(env.SITE_URL);
}

/** Checks a form POST: a session, a matching CSRF token and (if sent) our own Origin. */
async function requirePost(c: C): Promise<{ v: Visitor; form: FormData }> {
  const origin = c.req.header("Origin");
  if (origin && origin !== "null" && origin !== new URL(c.env.SITE_URL).origin) throw new ApiError(403, "forbidden", "cross-origin form posts are refused");
  const v = await visitor(c);
  if (!v) throw new ApiError(401, "unauthorized", "log in first");
  const form = await c.req.raw.formData();
  const token = form.get("csrf");
  if (typeof token !== "string" || !safeEqual(token, v.csrf)) throw new ApiError(403, "forbidden", "the form has expired; reload the page and try again");
  return { v, form };
}

/** A same-site path to return to after login. */
function safeNext(next: string | undefined | null): string {
  return next && next.startsWith("/") && !next.startsWith("//") && !next.includes("\\") ? next : "/";
}

function chrome(v: Visitor | null) {
  return { user: v ? { login: v.session.login } : null, csrf: v?.csrf ?? null };
}

// --- Static ---------------------------------------------------------------

web.get("/static/style.css", () =>
  new Response(STYLE, { headers: { "Content-Type": "text/css; charset=utf-8", "Cache-Control": "public, max-age=86400" } }),
);

// --- Pages ----------------------------------------------------------------

function packageList(hits: { id: string; name: string; version: string; summary: string | null }[]): Html {
  return html`<ul class="packages">${hits.map(
    (p) => html`<li><a href="/packages/${p.id}"><strong>${p.name}</strong></a> <code>${p.id}</code> <span class="muted">${p.version}</span>
      ${p.summary ? html`<div class="muted">${p.summary}</div>` : ""}</li>`,
  )}</ul>`;
}

function searchForm(q = ""): Html {
  return html`<form class="search" method="get" action="/search"><input type="search" name="q" value="${q}" placeholder="Search packages" aria-label="Search packages"><button type="submit">Search</button></form>`;
}

web.get("/", (c) =>
  render(c, async (v) => {
    const all = await search(c.env.DB, "", 1, 10_000);
    const recent = [...all.packages].sort((a, b) => (a.updated_at < b.updated_at ? 1 : -1)).slice(0, 20);
    const body = html`<h1>Flux packages</h1>
      <p class="muted">${all.total} packages. Install with <code>fpkg add &lt;id&gt;</code>; documentation at <a href="${trimSlash(c.env.DOCS_URL)}/">docs</a>.</p>
      ${searchForm()}
      <h2>Recently updated</h2>
      ${recent.length > 0 ? packageList(recent) : html`<p>Nothing has been published yet.</p>`}`;
    return page({ title: "fluxpkg", body, ...chrome(v) });
  }),
);

web.get("/search", (c) =>
  render(c, async (v) => {
    const q = (c.req.query("q") ?? "").slice(0, 128);
    const pageNo = Math.max(1, Math.min(10_000, Number.parseInt(c.req.query("page") ?? "1", 10) || 1));
    const perPage = 20;
    const res = await search(c.env.DB, q, pageNo, perPage);
    const pages = Math.max(1, Math.ceil(res.total / perPage));
    const link = (p: number, label: string) => html`<a href="/search?q=${encodeURIComponent(q)}&amp;page=${p}">${label}</a>`;
    const body = html`<h1>Search</h1>${searchForm(q)}
      <p class="muted">${res.total} result${res.total === 1 ? "" : "s"}${q ? html` for <code>${q}</code>` : ""}.</p>
      ${packageList(res.packages)}
      <p>${pageNo > 1 ? link(pageNo - 1, "Previous") : ""} ${pageNo < pages ? link(pageNo + 1, "Next") : ""}</p>`;
    return page({ title: q ? `${q} - fluxpkg search` : "Search - fluxpkg", body, ...chrome(v) });
  }),
);

function installLine(id: string, version: string | null): string {
  const m = version ? /^(\d+)\.(\d+)/.exec(version) : null;
  return m ? `s.depends '${id}', '~> ${m[1]}.${m[2]}'` : `s.depends '${id}'`;
}

function versionRows(doc: { versions: Record<string, VersionMetadata> }): Html {
  return html`<table><thead><tr><th>Version</th><th>Published</th><th>By</th><th></th></tr></thead><tbody>${Object.values(doc.versions).map(
    (m) => html`<tr><td><code>${m.version}</code> ${m.yanked ? html`<span class="badge warn">yanked</span>` : ""}</td>
      <td>${m.published_at.slice(0, 10)}</td><td>${m.published_by}</td>
      <td><a href="${m.docs}">docs</a> · <a href="${m.dist.url}">zip</a> <span class="muted">${(m.dist.size / 1024).toFixed(1)} KiB</span></td></tr>`,
  )}</tbody></table>`;
}

web.get("/packages/:id", (c) =>
  render(c, async (v) => {
    const id = c.req.param("id");
    const data = isValidId(id) ? await loadPackage(c.env.DB, id) : null;
    if (!data) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
    const doc = packageDocument(data, c.env);
    const latestId = doc.latest ?? data.versions[0]?.version ?? null;
    const m = latestId ? doc.versions[latestId]! : null;
    const downloads = await downloadCount(c.env.DB, id);
    const body = html`<h1>${m?.name ?? id} <span class="muted">${latestId ?? ""}</span></h1>
      <p><code>${id}</code>${doc.latest === null ? html` <span class="badge warn">every version is yanked</span>` : ""}</p>
      ${m?.summary ? html`<p>${m.summary}</p>` : ""}
      ${m?.description && m.description !== m.summary ? html`<p class="muted">${m.description}</p>` : ""}
      <pre>${installLine(id, doc.latest)}</pre>
      <p>${m ? html`<a href="${m.docs}">Documentation</a>` : ""}
         ${m?.website ? html` · <a href="${m.website}" rel="nofollow noopener">Website</a>` : ""}
         ${m?.license ? html` · License: ${m.license}` : ""} · ${downloads} downloads</p>
      ${m && m.authors.length > 0 ? html`<p>Authors: ${m.authors.join(", ")}</p>` : ""}
      <p>Owners: ${doc.owners.map((o, i) => html`${i > 0 ? ", " : ""}<a href="https://github.com/${encodeURIComponent(o)}">${o}</a>`)}</p>
      ${m && m.dependencies.length > 0
        ? html`<h2>Dependencies</h2><ul>${m.dependencies.map((d) => html`<li><a href="/packages/${d.id}">${d.id}</a> <code>${d.constraint}</code></li>`)}</ul>`
        : ""}
      <h2>Versions</h2>${versionRows(doc)}`;
    return page({ title: `${m?.name ?? id} - fluxpkg`, body, ...chrome(v) });
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
  return cached(c.req.raw, c.executionCtx, async () => {
    const found = await readMemberFromR2(c.env.ARCHIVES, `archives/${id}/${version}.zip`, `${id}/${rel}`, MAX_SOURCE);
    if (!found) throw new ApiError(404, "not_found", `${id} ${version} has no file \`${rel}\`.`);
    if (!found.bytes) throw new ApiError(413, "payload_too_large", `\`${rel}\` is too large to show.`);
    const sha = found.object.customMetadata?.sha256 ?? "";
    return new Response(found.bytes, {
      headers: {
        "Content-Type": "text/plain; charset=utf-8",
        "Cache-Control": CACHE_CONTROL.source,
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
      ? html`<div class="notice"><p>Your new token <strong>${extra.created.name}</strong>. Copy it now: it is not shown again.</p>
          <code class="token">${extra.created.token}</code></div>`
      : ""}
    <h2>New token</h2>
    <form method="post" action="/tokens"><input type="hidden" name="csrf" value="${v.csrf}">
      <input name="name" required maxlength="64" placeholder="Name, such as laptop or ci" aria-label="Token name"> <button type="submit">Create</button></form>
    <h2>Your tokens</h2>
    ${rows.length === 0
      ? html`<p>None yet.</p>`
      : html`<table><thead><tr><th>Name</th><th>Created</th><th>Last used</th><th></th></tr></thead><tbody>${rows.map(
          (t) => html`<tr><td>${t.name}</td><td>${t.created_at.slice(0, 10)}</td><td>${t.last_used_at ? t.last_used_at.slice(0, 16).replace("T", " ") : "never"}</td>
            <td>${t.revoked_at
              ? html`<span class="badge">revoked ${t.revoked_at.slice(0, 10)}</span>`
              : html`<form method="post" action="/tokens/${t.id}/revoke"><input type="hidden" name="csrf" value="${v.csrf}"><button class="link" type="submit">Revoke</button></form>`}</td></tr>`,
        )}</tbody></table>`}`;
  return page({ title: "API tokens - fluxpkg", body, ...chrome(v), status, cacheControl: "private, no-store" });
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

// --- Errors ---------------------------------------------------------------

export async function webNotFound(c: C): Promise<Response> {
  const v = await visitor(c);
  return page({ title: "Not found - fluxpkg", body: html`<h1>Not found</h1><p>There is nothing at this address.</p>`, status: 404, ...chrome(v) });
}

web.notFound(webNotFound);

web.onError(async (err, c) => {
  const status = err instanceof ApiError ? err.status : 500;
  if (!(err instanceof ApiError)) console.error(`${c.req.method} ${c.req.url}:`, err);
  const message = err instanceof ApiError ? err.message : "Something went wrong on our side.";
  const v = await visitor(c).catch(() => null);
  return page({
    title: `${status === 404 ? "Not found" : "Error"} - fluxpkg`,
    body: html`<h1>${status === 404 ? "Not found" : "Something went wrong"}</h1><p>${message}</p>`,
    status,
    cacheControl: CACHE_CONTROL.noStore,
    ...chrome(v),
  });
});

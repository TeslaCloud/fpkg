// The admin pages under /admin: the approval queue, every package and user, and the actions
// (approve, reject, yank, owners) with their log. Admins are the logins in ADMIN_LOGINS.

import { Hono } from "hono";
import type { AppEnv } from "../env";
import { adminLogins } from "../lib/auth";
import { CACHE_CONTROL } from "../lib/cache";
import { downloadCount, getVersionRow, loadPackage, packageStatus, type PackageRow } from "../lib/db";
import { ApiError } from "../lib/http";
import { trimSlash } from "../lib/metadata";
import { addOwner, approvePackage, logAdminAction, rejectPackage, removeOwner, setYanked } from "../lib/packages";
import { canonicalize, isValidId } from "../lib/version";
import { Html, compact, day, html, minute, page, size } from "./html";
import { type C, type Visitor, adminOf, requirePost, visitor } from "./session";

export const admin = new Hono<AppEnv>();

const PAGE = 50;

// --- Access ---------------------------------------------------------------

class Redirect extends Error {
  constructor(public readonly location: string) {
    super("redirect");
  }
}

/** The visitor as an admin; anonymous visitors are sent to log in, others get a 403. */
async function requireAdmin(c: C): Promise<Visitor> {
  const v = await visitor(c);
  if (!v) {
    const url = new URL(c.req.url);
    throw new Redirect(`/login?next=${encodeURIComponent(url.pathname + url.search)}`);
  }
  const a = await adminOf(c, v);
  if (!a) throw new ApiError(403, "forbidden", "This page is for registry admins.");
  return a;
}

/** An admin's form post, CSRF-checked. */
async function adminPost(c: C): Promise<{ v: Visitor; form: FormData }> {
  const { v, form } = await requirePost(c);
  if (!(await adminOf(c, v))) throw new ApiError(403, "forbidden", "This action is for registry admins.");
  return { v, form };
}

function redirect(location: string): Response {
  return new Response(null, { status: 303, headers: { Location: location, "Cache-Control": CACHE_CONTROL.noStore } });
}

admin.onError((err) => {
  if (err instanceof Redirect) return redirect(err.location);
  throw err;
});

// --- Rendering ------------------------------------------------------------

/** Outcomes a redirect reports back, by code, so nothing free-form is reflected. */
const MESSAGES: Record<string, { kind: string; text: string }> = {
  approved: { kind: "ok", text: "The package is approved and public; its documentation is being generated." },
  rejected: { kind: "ok", text: "The package was rejected and removed. Its owners can publish it again." },
  yanked: { kind: "ok", text: "The version is yanked." },
  unyanked: { kind: "ok", text: "The version is available again." },
  owner_added: { kind: "ok", text: "The owner was added." },
  owner_removed: { kind: "ok", text: "The owner was removed." },
  nothing: { kind: "", text: "Nothing changed." },
  user_not_found: { kind: "error", text: "There is no such user. They need to log in to the registry once first." },
  last_owner: { kind: "error", text: "The last owner of a package cannot be removed." },
  not_owner: { kind: "error", text: "That login is not an owner of this package." },
};

function flash(c: C): Html {
  const code = c.req.query("m");
  const m = code && Object.hasOwn(MESSAGES, code) ? MESSAGES[code] : undefined;
  return m ? html`<div class="notice ${m.kind}">${m.text}</div>` : new Html("");
}

function adminPage(c: C, v: Visitor, title: string, current: string, body: Html): Response {
  const subnav = html`<nav class="subnav" aria-label="Admin">
    <a href="/admin"${current === "dashboard" ? new Html(' aria-current="page"') : ""}>Dashboard</a>
    <a href="/admin/packages"${current === "packages" ? new Html(' aria-current="page"') : ""}>Packages</a>
    <a href="/admin/users"${current === "users" ? new Html(' aria-current="page"') : ""}>Users</a>
  </nav>`;
  return page({
    title: `${title} - admin - fluxpkg`,
    body: html`${subnav}${flash(c)}${body}`,
    user: { login: v.session.login, admin: true },
    csrf: v.csrf,
    docs: trimSlash(c.env.DOCS_URL),
    current: "admin",
    cacheControl: "private, no-store",
  });
}

function csrfField(v: Visitor): Html {
  return html`<input type="hidden" name="csrf" value="${v.csrf}">`;
}

function statusBadge(row: { approved_at: string | null }): Html {
  return packageStatus(row) === "pending" ? html`<span class="badge pend">pending</span>` : html`<span class="badge ok">approved</span>`;
}

/** Approve and reject, for a pending package. */
function reviewForms(v: Visitor, id: string, withReason: boolean): Html {
  return html`<form class="inline" method="post" action="/admin/packages/${id}/approve">${csrfField(v)}<button class="btn ok sm" type="submit">Approve</button></form>
    <form class="inline" method="post" action="/admin/packages/${id}/reject">${csrfField(v)}${withReason ? html`<input type="text" name="reason" maxlength="256" placeholder="Reason (kept in the log)" aria-label="Reason"> ` : ""}<button class="btn danger sm" type="submit">Reject</button></form>`;
}

// --- Queries --------------------------------------------------------------

interface PackageSummary {
  id: string;
  created_at: string;
  updated_at: string;
  approved_at: string | null;
  name: string | null;
  summary: string | null;
  latest: string | null;
  versions: number;
  owners: string | null;
  bytes: number;
  downloads: number;
}

const SUMMARY_SELECT = `SELECT p.id, p.created_at, p.updated_at, p.approved_at,
    (SELECT json_extract(v.metadata, '$.name') FROM versions v WHERE v.package_id = p.id ORDER BY v.sort_key DESC LIMIT 1) AS name,
    (SELECT json_extract(v.metadata, '$.summary') FROM versions v WHERE v.package_id = p.id ORDER BY v.sort_key DESC LIMIT 1) AS summary,
    (SELECT v.version FROM versions v WHERE v.package_id = p.id ORDER BY v.sort_key DESC LIMIT 1) AS latest,
    (SELECT COUNT(*) FROM versions v WHERE v.package_id = p.id) AS versions,
    (SELECT group_concat(u.login, ', ') FROM owners o JOIN users u ON u.id = o.user_id WHERE o.package_id = p.id) AS owners,
    (SELECT COALESCE(SUM(v.size), 0) FROM versions v WHERE v.package_id = p.id) AS bytes,
    (SELECT COALESCE(SUM(d.count), 0) FROM downloads d WHERE d.package_id = p.id) AS downloads
  FROM packages p`;

async function pendingPackages(db: D1Database): Promise<PackageSummary[]> {
  return (await db.prepare(`${SUMMARY_SELECT} WHERE p.approved_at IS NULL ORDER BY p.created_at`).all<PackageSummary>()).results;
}

interface ActionRow {
  id: number;
  actor: string;
  action: string;
  package_id: string;
  version: string | null;
  detail: string | null;
  at: string;
}

async function recentActions(db: D1Database, packageId: string | null, limit: number): Promise<ActionRow[]> {
  const where = packageId === null ? "" : "WHERE a.package_id = ?2";
  return (
    await db
      .prepare(`SELECT a.id, u.login AS actor, a.action, a.package_id, a.version, a.detail, a.at FROM admin_actions a JOIN users u ON u.id = a.actor_id ${where} ORDER BY a.id DESC LIMIT ?1`)
      .bind(...(packageId === null ? [limit] : [limit, packageId]))
      .all<ActionRow>()
  ).results;
}

function likeEscape(s: string): string {
  return s.replace(/[\\%_]/g, (c) => `\\${c}`);
}

function pageParam(c: C): number {
  return Math.max(1, Math.min(10_000, Number.parseInt(c.req.query("page") ?? "1", 10) || 1));
}

function pager(base: string, pageNo: number, total: number): Html {
  const pages = Math.max(1, Math.ceil(total / PAGE));
  if (pages <= 1) return new Html("");
  const sep = base.includes("?") ? "&" : "?";
  const link = (p: number, label: string) => html`<a class="btn sm" href="${base}${sep}page=${p}">${label}</a>`;
  return html`<div class="pagination">${pageNo > 1 ? link(pageNo - 1, "Previous") : ""}<span class="muted">Page ${pageNo} of ${pages}</span>${pageNo < pages ? link(pageNo + 1, "Next") : ""}</div>`;
}

// --- Dashboard ------------------------------------------------------------

function actionLine(a: ActionRow, withPackage: boolean): Html {
  const what: Record<string, string> = {
    approve: "approved",
    reject: "rejected",
    yank: "yanked",
    unyank: "unyanked",
    owner_add: "added owner",
    owner_remove: "removed owner",
  };
  return html`<li><strong>${a.actor}</strong> ${what[a.action] ?? a.action}
    ${withPackage ? html`<a href="/admin/packages/${a.package_id}">${a.package_id}</a>` : ""} ${a.version ? html`<code>${a.version}</code>` : ""}
    ${a.detail ? html`<span class="muted">${a.detail}</span>` : ""}<time datetime="${a.at}">${minute(a.at)}</time></li>`;
}

admin.get("/", async (c) => {
  const v = await requireAdmin(c);
  const db = c.env.DB;
  const [counts, pending, actions] = await Promise.all([
    db
      .prepare(
        `SELECT (SELECT COUNT(*) FROM packages WHERE approved_at IS NOT NULL) AS approved,
                (SELECT COUNT(*) FROM packages WHERE approved_at IS NULL) AS pending,
                (SELECT COUNT(*) FROM versions) AS versions,
                (SELECT COUNT(*) FROM users) AS users,
                (SELECT COUNT(*) FROM tokens WHERE revoked_at IS NULL) AS tokens,
                (SELECT COALESCE(SUM(count), 0) FROM downloads) AS downloads`,
      )
      .first<{ approved: number; pending: number; versions: number; users: number; tokens: number; downloads: number }>(),
    pendingPackages(db),
    recentActions(db, null, 20),
  ]);
  const n = counts!;
  const body = html`<h1>Admin</h1>
    <div class="tiles">
      <div class="tile${n.pending > 0 ? " attention" : ""}"><b>${n.pending}</b><span>awaiting approval</span></div>
      <div class="tile"><b>${n.approved}</b><span>public packages</span></div>
      <div class="tile"><b>${n.versions}</b><span>versions</span></div>
      <div class="tile"><b>${compact(Number(n.downloads))}</b><span>downloads</span></div>
      <div class="tile"><b>${n.users}</b><span>users</span></div>
      <div class="tile"><b>${n.tokens}</b><span>active tokens</span></div>
    </div>
    <div class="section-head"><h2>Awaiting approval</h2><span class="muted small">Oldest first. Read the spec before approving; rejecting deletes the package and its archives.</span></div>
    <div class="card">${pending.length === 0
      ? html`<p class="empty">The queue is empty.</p>`
      : html`<ul class="pkgs">${pending.map(
          (p) => html`<li>
            <div class="row"><a class="name" href="/admin/packages/${p.id}">${p.name ?? p.id}</a><span class="id">${p.id}</span>
              <span class="badge quiet">${p.latest ?? "no version"}</span>
              <span class="meta">by ${p.owners ?? "nobody"} · ${minute(p.created_at)} · ${p.versions} version${p.versions === 1 ? "" : "s"} · ${size(p.bytes)}</span></div>
            ${p.summary ? html`<div class="summary">${p.summary}</div>` : ""}
            <div class="actions">
              <a class="btn sm" href="/packages/${p.id}">Package page</a>
              ${p.latest ? html`<a class="btn sm" href="/packages/${p.id}/${p.latest}/source/packagespec.lua">packagespec.lua</a>` : ""}
              ${reviewForms(v, p.id, false)}
            </div></li>`,
        )}</ul>`}</div>
    <h2>Recent actions</h2>
    <div class="card">${actions.length === 0 ? html`<p class="empty">Nothing yet.</p>` : html`<ul class="timeline">${actions.map((a) => actionLine(a, true))}</ul>`}</div>`;
  return adminPage(c, v, "Dashboard", "dashboard", body);
});

// --- Packages -------------------------------------------------------------

/** The SQL condition of each value of the list's status filter. */
const STATUS_FILTER = { pending: "p.approved_at IS NULL", approved: "p.approved_at IS NOT NULL", all: "1 = 1" };

admin.get("/packages", async (c) => {
  const v = await requireAdmin(c);
  const q = (c.req.query("q") ?? "").slice(0, 128).trim();
  const asked = c.req.query("status");
  const status = asked === "pending" || asked === "approved" ? asked : "all";
  const pageNo = pageParam(c);
  const where = [
    STATUS_FILTER[status],
    q
      ? "(p.id LIKE ?1 ESCAPE '\\' OR EXISTS (SELECT 1 FROM owners o JOIN users u ON u.id = o.user_id WHERE o.package_id = p.id AND u.login LIKE ?1 ESCAPE '\\'))"
      : "?1 = ?1",
  ].join(" AND ");
  const like = `%${likeEscape(q)}%`;
  const [count, rows] = await c.env.DB.batch([
    c.env.DB.prepare(`SELECT COUNT(*) AS n FROM packages p WHERE ${where}`).bind(like),
    c.env.DB.prepare(`${SUMMARY_SELECT} WHERE ${where} ORDER BY p.updated_at DESC LIMIT ?2 OFFSET ?3`).bind(like, PAGE, (pageNo - 1) * PAGE),
  ]);
  const total = (count!.results[0] as { n: number }).n;
  const list = rows!.results as PackageSummary[];
  const query = `/admin/packages?q=${encodeURIComponent(q)}&status=${status}`;
  const body = html`<h1>Packages</h1>
    <form class="toolbar" method="get" action="/admin/packages">
      <div class="search"><input type="search" name="q" value="${q}" placeholder="Package id or owner login" aria-label="Filter packages"></div>
      <select name="status" aria-label="Status"><option value="all"${status === "all" ? new Html(" selected") : ""}>All</option><option value="pending"${status === "pending" ? new Html(" selected") : ""}>Pending</option><option value="approved"${status === "approved" ? new Html(" selected") : ""}>Approved</option></select>
      <button class="btn" type="submit">Filter</button>
    </form>
    <p class="muted">${total} package${total === 1 ? "" : "s"}.</p>
    <div class="card table-wrap">${list.length === 0
      ? html`<p class="empty">No package matches.</p>`
      : html`<table><thead><tr><th scope="col">Package</th><th scope="col">Status</th><th scope="col">Latest</th><th scope="col" class="num">Versions</th><th scope="col">Owners</th><th scope="col" class="num">Downloads</th><th scope="col">Updated</th></tr></thead><tbody>${list.map(
          (p) => html`<tr><td><a href="/admin/packages/${p.id}"><strong>${p.name ?? p.id}</strong></a><br><span class="muted small"><code>${p.id}</code></span></td>
            <td>${statusBadge(p)}</td><td><code>${p.latest ?? ""}</code></td><td class="num">${p.versions}</td><td>${p.owners ?? ""}</td><td class="num">${p.downloads}</td><td>${day(p.updated_at)}</td></tr>`,
        )}</tbody></table>`}</div>
    ${pager(query, pageNo, total)}`;
  return adminPage(c, v, "Packages", "packages", body);
});

function adminPackageId(c: C): string {
  const id = c.req.param("id") ?? "";
  if (!isValidId(id)) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
  return id;
}

admin.get("/packages/:id", async (c) => {
  const v = await requireAdmin(c);
  const id = adminPackageId(c);
  const data = await loadPackage(c.env.DB, id);
  if (!data) throw new ApiError(404, "not_found", `There is no package \`${id}\`.`);
  const pkg: PackageRow = data.pkg;
  const pending = pkg.approved_at === null;
  const [downloads, actions, approver] = await Promise.all([
    downloadCount(c.env.DB, id),
    recentActions(c.env.DB, id, 50),
    pkg.approved_by === null ? null : c.env.DB.prepare("SELECT login FROM users WHERE id = ?").bind(pkg.approved_by).first<{ login: string }>(),
  ]);
  const newest = data.versions[0];
  const meta = newest ? (JSON.parse(newest.metadata) as { name?: string; summary?: string | null; description?: string | null; website?: string | null; license?: string | null }) : null;
  const body = html`<div class="page-head">
      <div><h1>${meta?.name ?? id} ${statusBadge(pkg)}</h1><p class="muted small"><code>${id}</code>${meta?.summary ? html` · ${meta.summary}` : ""}</p></div>
      <div class="actions"><a class="btn" href="/packages/${id}">Package page</a>${pending ? reviewForms(v, id, true) : ""}</div>
    </div>
    ${pending
      ? html`<div class="notice pend"><p><strong>Awaiting approval.</strong> Only its owners and admins can see this package. Check the spec and the files it ships
          before approving; rejecting deletes the package, its ${data.versions.length} version${data.versions.length === 1 ? "" : "s"} and their archives.</p></div>`
      : ""}
    <div class="tiles">
      <div class="tile"><b>${data.versions.length}</b><span>versions</span></div>
      <div class="tile"><b>${downloads}</b><span>downloads</span></div>
      <div class="tile"><b>${data.owners.length}</b><span>owners</span></div>
      <div class="tile"><b>${day(pkg.created_at)}</b><span>created</span></div>
      <div class="tile"><b>${pkg.approved_at ? day(pkg.approved_at) : "-"}</b><span>approved${approver ? ` by ${approver.login}` : ""}</span></div>
    </div>
    <div class="grid cols-2">
      <section class="card"><div class="card-head"><h2>Owners</h2></div>
        <ul class="pkgs">${data.owners.map(
          (o) => html`<li><div class="row"><a class="name" href="https://github.com/${encodeURIComponent(o)}" rel="nofollow noopener">${o}</a>
            <span class="meta"><form class="inline" method="post" action="/admin/packages/${id}/owners/${encodeURIComponent(o)}/remove">${csrfField(v)}<button class="link danger" type="submit">Remove</button></form></span></div></li>`,
        )}</ul>
        <div class="card-body"><form class="field" method="post" action="/admin/packages/${id}/owners">${csrfField(v)}<input type="text" name="login" required maxlength="64" placeholder="GitHub login" aria-label="GitHub login"><button class="btn sm" type="submit">Add owner</button></form></div>
      </section>
      <section class="card"><div class="card-head"><h2>Metadata</h2></div><div class="card-body"><dl class="kv">
        <dt>Revision</dt><dd>${pkg.revision}</dd>
        <dt>Updated</dt><dd>${minute(pkg.updated_at)}</dd>
        ${meta?.license ? html`<dt>License</dt><dd>${meta.license}</dd>` : ""}
        ${meta?.website ? html`<dt>Website</dt><dd><a href="${meta.website}" rel="nofollow noopener">${meta.website}</a></dd>` : ""}
        ${meta?.description ? html`<dt>Description</dt><dd class="prose">${meta.description}</dd>` : ""}
      </dl></div></section>
    </div>
    <h2>Versions</h2>
    <div class="card table-wrap"><table><thead><tr><th scope="col">Version</th><th scope="col">Published</th><th scope="col">By</th><th scope="col" class="num">Size</th><th scope="col">Docs</th><th scope="col">Files</th><th scope="col"></th></tr></thead><tbody>${data.versions.map(
      (r) => html`<tr><td><code>${r.version}</code> ${r.yanked ? html`<span class="badge warn">yanked</span>` : ""}</td>
        <td>${minute(r.published_at)}</td><td>${r.published_by_login}</td><td class="num">${size(r.size)}</td>
        <td>${pending ? html`<span class="muted">after approval</span>` : html`<span class="badge quiet">${r.docs_status ?? "pending"}</span>`}</td>
        <td><a href="/packages/${id}/${r.version}/source/packagespec.lua">packagespec.lua</a>${pending ? "" : html` · <a href="/api/v1/packages/${id}/versions/${r.version}/archive">zip</a>`}</td>
        <td class="actions-cell"><form class="inline" method="post" action="/admin/packages/${id}/versions/${r.version}/${r.yanked ? "unyank" : "yank"}">${csrfField(v)}<button class="btn sm${r.yanked ? "" : " danger"}" type="submit">${r.yanked ? "Unyank" : "Yank"}</button></form></td></tr>`,
    )}</tbody></table></div>
    <h2>Admin log</h2>
    <div class="card">${actions.length === 0 ? html`<p class="empty">No admin action on this package yet.</p>` : html`<ul class="timeline">${actions.map((a) => actionLine(a, false))}</ul>`}</div>`;
  return adminPage(c, v, meta?.name ?? id, "packages", body);
});

// --- Actions --------------------------------------------------------------

admin.post("/packages/:id/approve", async (c) => {
  const { v } = await adminPost(c);
  const id = adminPackageId(c);
  const done = await approvePackage(c.env, c.executionCtx, c.req.url, id, v.session.uid);
  return redirect(`/admin/packages/${id}?m=${done ? "approved" : "nothing"}`);
});

admin.post("/packages/:id/reject", async (c) => {
  const { v, form } = await adminPost(c);
  const id = adminPackageId(c);
  const reason = String(form.get("reason") ?? "").trim().slice(0, 256) || null;
  const done = await rejectPackage(c.env, id, v.session.uid, reason);
  return done ? redirect("/admin?m=rejected") : redirect(`/admin/packages/${id}?m=nothing`);
});

for (const [action, yanked] of [["yank", true], ["unyank", false]] as const) {
  admin.post(`/packages/:id/versions/:version/${action}`, async (c) => {
    const { v } = await adminPost(c);
    const id = adminPackageId(c);
    const version = canonicalize(c.req.param("version") ?? "");
    const before = version ? await getVersionRow(c.env.DB, id, version) : null;
    if (!before) throw new ApiError(404, "not_found", `There is no version ${c.req.param("version")} of \`${id}\`.`);
    const after = await setYanked(c.env, c.executionCtx, c.req.url, before, yanked);
    if (after !== before) await logAdminAction(c.env.DB, v.session.uid, action, id, version);
    const done = yanked ? "yanked" : "unyanked";
    return redirect(`/admin/packages/${id}?m=${after === before ? "nothing" : done}`);
  });
}

admin.post("/packages/:id/owners", async (c) => {
  const { v, form } = await adminPost(c);
  const id = adminPackageId(c);
  const login = String(form.get("login") ?? "").trim();
  if (login === "" || login.length > 64) return redirect(`/admin/packages/${id}?m=user_not_found`);
  try {
    const { added } = await addOwner(c.env, c.executionCtx, c.req.url, id, login);
    if (added) await logAdminAction(c.env.DB, v.session.uid, "owner_add", id, null, login);
    return redirect(`/admin/packages/${id}?m=${added ? "owner_added" : "nothing"}`);
  } catch (e) {
    if (e instanceof ApiError && e.code === "user_not_found") return redirect(`/admin/packages/${id}?m=user_not_found`);
    throw e;
  }
});

admin.post("/packages/:id/owners/:login/remove", async (c) => {
  const { v } = await adminPost(c);
  const id = adminPackageId(c);
  const login = c.req.param("login") ?? "";
  try {
    await removeOwner(c.env, c.executionCtx, c.req.url, id, login);
    await logAdminAction(c.env.DB, v.session.uid, "owner_remove", id, null, login);
    return redirect(`/admin/packages/${id}?m=owner_removed`);
  } catch (e) {
    if (e instanceof ApiError && (e.code === "last_owner" || e.code === "not_found")) {
      return redirect(`/admin/packages/${id}?m=${e.code === "last_owner" ? "last_owner" : "not_owner"}`);
    }
    throw e;
  }
});

// --- Users ----------------------------------------------------------------

interface UserRow {
  id: number;
  login: string;
  github_id: number;
  created_at: string;
  packages: number;
  pending: number;
  tokens: number;
  last_used_at: string | null;
}

admin.get("/users", async (c) => {
  const v = await requireAdmin(c);
  const q = (c.req.query("q") ?? "").slice(0, 128).trim();
  const pageNo = pageParam(c);
  const like = `%${likeEscape(q)}%`;
  const [count, rows] = await c.env.DB.batch([
    c.env.DB.prepare("SELECT COUNT(*) AS n FROM users WHERE login LIKE ?1 ESCAPE '\\'").bind(like),
    c.env.DB.prepare(
      `SELECT u.id, u.login, u.github_id, u.created_at,
              (SELECT COUNT(*) FROM owners o JOIN packages p ON p.id = o.package_id WHERE o.user_id = u.id AND p.approved_at IS NOT NULL) AS packages,
              (SELECT COUNT(*) FROM owners o JOIN packages p ON p.id = o.package_id WHERE o.user_id = u.id AND p.approved_at IS NULL) AS pending,
              (SELECT COUNT(*) FROM tokens t WHERE t.user_id = u.id AND t.revoked_at IS NULL) AS tokens,
              (SELECT MAX(t.last_used_at) FROM tokens t WHERE t.user_id = u.id) AS last_used_at
         FROM users u WHERE u.login LIKE ?1 ESCAPE '\\' ORDER BY u.id DESC LIMIT ?2 OFFSET ?3`,
    ).bind(like, PAGE, (pageNo - 1) * PAGE),
  ]);
  const total = (count!.results[0] as { n: number }).n;
  const list = rows!.results as UserRow[];
  const admins = adminLogins(c.env);
  const body = html`<h1>Users</h1>
    <form class="toolbar" method="get" action="/admin/users"><div class="search"><input type="search" name="q" value="${q}" placeholder="GitHub login" aria-label="Filter users"></div><button class="btn" type="submit">Filter</button></form>
    <p class="muted">${total} user${total === 1 ? "" : "s"}. Admins are set with <code>ADMIN_LOGINS</code> in wrangler.toml.</p>
    <div class="card table-wrap">${list.length === 0
      ? html`<p class="empty">No user matches.</p>`
      : html`<table><thead><tr><th scope="col">Login</th><th scope="col" class="num">GitHub id</th><th scope="col">Joined</th><th scope="col" class="num">Packages</th><th scope="col" class="num">Pending</th><th scope="col" class="num">Tokens</th><th scope="col">Token last used</th></tr></thead><tbody>${list.map(
          (u) => html`<tr><td><a href="/admin/packages?q=${encodeURIComponent(u.login)}">${u.login}</a> ${admins.has(u.login.toLowerCase()) ? html`<span class="badge">admin</span>` : ""}</td>
            <td class="num">${u.github_id}</td><td>${day(u.created_at)}</td><td class="num">${u.packages}</td><td class="num">${u.pending}</td><td class="num">${u.tokens}</td><td>${u.last_used_at ? minute(u.last_used_at) : "never"}</td></tr>`,
        )}</tbody></table>`}</div>
    ${pager(`/admin/users?q=${encodeURIComponent(q)}`, pageNo, total)}`;
  return adminPage(c, v, "Users", "users", body);
});

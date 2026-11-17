// New packages by non-admins wait for approval: what pending packages look like to whom, and the admin pages.

import { SELF, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { MAX_PENDING_PER_USER } from "../src/api/publish";
import { generateToken, sha256Hex, signSession } from "../src/lib/auth";
import { packageZip } from "./zipwriter";

const SITE = "https://fluxpkg.dev";
const API = `${SITE}/api/v1`;
const DOCS = "https://docs.fluxpkg.dev";
const run = Math.floor(Math.random() * 1e6);
let githubId = 700_000 + run;

async function seedUser(login: string): Promise<{ id: number; login: string; token: string }> {
  const user = await env.DB.prepare("INSERT INTO users (github_id, login, created_at) VALUES (?, ?, '2026-11-01T00:00:00Z') RETURNING id")
    .bind(githubId++, login)
    .first<{ id: number }>();
  const token = generateToken();
  await env.DB.prepare("INSERT INTO tokens (user_id, name, hash, created_at) VALUES (?, 't', ?, '2026-11-01T00:00:00Z')").bind(user!.id, await sha256Hex(token)).run();
  return { id: user!.id, login, token };
}

async function publish(token: string, id: string, version: string, extra: Record<string, unknown> = {}) {
  const form = new FormData();
  form.append("metadata", JSON.stringify({ id, name: "Test Package", version, summary: "A test package.", dependencies: [], ...extra }));
  form.append("archive", new File([packageZip(id, version)], "a.zip"));
  return SELF.fetch(`${API}/packages/${id}/versions/${version}`, { method: "PUT", body: form, headers: { Authorization: `Bearer ${token}` } });
}

const auth = (token: string) => ({ headers: { Authorization: `Bearer ${token}` } });
const cookieFor = async (u: { id: number; login: string }) => `fpkg_session=${await signSession(env.SESSION_SECRET!, u.id, u.login)}`;
const form = (fields: Record<string, string>) => {
  const f = new FormData();
  for (const [k, v] of Object.entries(fields)) f.append(k, v);
  return f;
};
const json = async (res: Response) => (await res.json()) as Record<string, any>;

/** An admin's page and its CSRF token. */
async function adminGet(cookie: string, path: string): Promise<{ status: number; text: string; csrf: string }> {
  const res = await SELF.fetch(`${SITE}${path}`, { headers: { Cookie: cookie } });
  const text = await res.text();
  return { status: res.status, text, csrf: /name="csrf" value="([^"]+)"/.exec(text)?.[1] ?? "" };
}

async function adminPost(cookie: string, path: string, fields: Record<string, string>): Promise<Response> {
  return SELF.fetch(`${SITE}${path}`, { method: "POST", headers: { Cookie: cookie }, body: form(fields), redirect: "manual" });
}

describe("approval", () => {
  it("keeps a non-admin's new package private until an admin approves it through the admin pages", async () => {
    const user = await seedUser(`pub${run}`);
    const other = await seedUser(`oth${run}`);
    const admin = await seedUser("admin");
    const id = `ap_${run}`;

    let res = await publish(user.token, id, "1.0.0");
    expect(res.status).toBe(202);
    expect(res.headers.get("Location")).toBe(`${API}/packages/${id}/versions/1.0.0`);
    expect(await json(res)).toMatchObject({ id, version: "1.0.0", published_by: user.login });

    // Anonymous readers and other users: the package does not exist.
    for (const url of [
      `${API}/packages/${id}`,
      `${API}/packages/${id}/versions/1.0.0`,
      `${API}/packages/${id}/versions/1.0.0/archive`,
      `${API}/packages/${id}/owners`,
      `${SITE}/packages/${id}`,
      `${SITE}/packages/${id}/1.0.0/source/packagespec.lua`,
      `${DOCS}/${id}/1.0.0/`,
    ]) {
      expect((await SELF.fetch(url)).status, url).toBe(404);
      expect((await SELF.fetch(url, auth(other.token))).status, url).toBe(404);
      // A bad token is not told apart from no token: a 401 would say the id is taken.
      expect((await SELF.fetch(url, auth("fpk_" + "x".repeat(43)))).status, url).toBe(404);
      expect((await SELF.fetch(url, { headers: { Authorization: "Basic xyz" } })).status, url).toBe(404);
    }
    expect((await SELF.fetch(`${API}/packages/nope_${run}`, auth("fpk_" + "x".repeat(43)))).status).toBe(404);
    expect((await SELF.fetch(`${DOCS}/${id}/`, { redirect: "manual" })).status).toBe(404);
    expect((await json(await SELF.fetch(`${API}/search?q=${id}`))).total).toBe(0);
    expect(await (await SELF.fetch(`${SITE}/`)).text()).not.toContain(`/packages/${id}`);

    // Its owner and an admin see it with their token; nothing of it is shared through the cache.
    res = await SELF.fetch(`${API}/packages/${id}`, auth(user.token));
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect(await json(res)).toMatchObject({ status: "pending", latest: "1.0.0", owners: [user.login] });
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.0.0/archive`, auth(admin.token));
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect((await SELF.fetch(`${API}/packages/${id}/versions/1.0.0/archive`)).status).toBe(404);
    res = await SELF.fetch(`${API}/packages/${id}/versions/1.0.0`, auth(user.token));
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect((await json(await SELF.fetch(`${API}/packages/${id}/owners`, auth(user.token)))).owners).toEqual([user.login]);
    expect(await env.DB.prepare("SELECT 1 FROM downloads WHERE package_id = ?").bind(id).first()).toBeNull();

    // Nothing can depend on it, and its docs are not queued.
    const dependent = `ap_dep_${run}`;
    res = await publish(admin.token, dependent, "1.0.0", { dependencies: [{ id, constraint: "~> 1.0" }] });
    expect(res.status).toBe(422);
    expect((await json(res)).error.details.join()).toMatch(/not on the registry/);
    expect((await env.DB.prepare("SELECT docs_status FROM versions WHERE package_id = ?").bind(id).first<{ docs_status: string }>())!.docs_status).toBe("pending");

    // The owner may publish more versions meanwhile; they stay pending too.
    expect((await publish(user.token, id, "1.1.0")).status).toBe(202);
    expect((await publish(other.token, id, "1.2.0")).status).toBe(403);

    // The website for the owner's session: the page with its banner, the spec, never cached.
    const cookie = await cookieFor(user);
    res = await SELF.fetch(`${SITE}/packages/${id}`, { headers: { Cookie: cookie } });
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect(await res.text()).toContain("awaiting approval");
    expect((await SELF.fetch(`${SITE}/packages/${id}`, { headers: { Cookie: await cookieFor(other) } })).status).toBe(404);
    res = await SELF.fetch(`${SITE}/packages/${id}/1.0.0/source/packagespec.lua`, { headers: { Cookie: cookie } });
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("no-store");
    expect((await SELF.fetch(`${SITE}/packages/${id}/1.0.0/source/packagespec.lua`)).status).toBe(404);

    // Admin pages: anonymous visitors log in first, other users are refused, admins see the queue.
    res = await SELF.fetch(`${SITE}/admin`, { redirect: "manual" });
    expect(res.status).toBe(303);
    expect(res.headers.get("Location")).toBe("/login?next=%2Fadmin");
    expect((await SELF.fetch(`${SITE}/admin`, { headers: { Cookie: cookie } })).status).toBe(403);
    expect(await (await SELF.fetch(`${SITE}/packages/${id}`, { headers: { Cookie: cookie } })).text()).not.toContain('href="/admin"');
    const adminCookie = await cookieFor(admin);
    let page = await adminGet(adminCookie, "/admin");
    expect(page.status).toBe(200);
    expect(page.text).toContain(`/admin/packages/${id}`);
    expect(page.text).toContain(`/packages/${id}/1.1.0/source/packagespec.lua`);
    expect(page.text).toContain('href="/admin"');
    page = await adminGet(adminCookie, `/admin/packages/${id}`);
    expect(page.status).toBe(200);
    expect(page.text).toContain("Awaiting approval");
    expect(page.text).toContain("1.1.0");
    expect(page.text).toContain("after approval");
    page = await adminGet(adminCookie, `/admin/packages?status=pending&q=${user.login}`);
    expect(page.text).toContain(`/admin/packages/${id}`);
    expect((await adminGet(adminCookie, `/admin/packages?status=approved&q=${id}`)).text).not.toContain(`/admin/packages/${id}`);
    page = await adminGet(adminCookie, `/admin/users?q=${user.login}`);
    expect(page.text).toContain(user.login);

    // Approving needs the CSRF token and an admin.
    expect((await adminPost(adminCookie, `/admin/packages/${id}/approve`, {})).status).toBe(403);
    expect((await adminPost(cookie, `/admin/packages/${id}/approve`, { csrf: /name="csrf" value="([^"]+)"/.exec(await (await SELF.fetch(`${SITE}/tokens`, { headers: { Cookie: cookie } })).text())![1]! })).status).toBe(403);
    const { csrf } = await adminGet(adminCookie, "/admin");
    res = await adminPost(adminCookie, `/admin/packages/${id}/approve`, { csrf });
    expect(res.status).toBe(303);
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=approved`);

    // Public now: the document (one revision bump), the site, search, docs queued, dependable, and 201 from here on.
    res = await SELF.fetch(`${API}/packages/${id}`);
    expect(res.status).toBe(200);
    expect(res.headers.get("Cache-Control")).toBe("public, max-age=60, stale-while-revalidate=600");
    expect(await json(res)).toMatchObject({ status: "approved", latest: "1.1.0", revision: 3 });
    expect((await SELF.fetch(`${SITE}/packages/${id}`)).status).toBe(200);
    // (The earlier search response is cached for a minute; a different URL asks the database.)
    expect((await json(await SELF.fetch(`${API}/search?q=${id}&per_page=5`))).total).toBe(1);
    // (The local queue consumer may already have run a job.)
    const docsRows = (await env.DB.prepare("SELECT docs_status FROM versions WHERE package_id = ?").bind(id).all<{ docs_status: string }>()).results;
    expect(docsRows.map((r) => r.docs_status === "queued" || r.docs_status === "ready")).toEqual([true, true]);
    expect((await publish(admin.token, dependent, "1.0.0", { dependencies: [{ id, constraint: "~> 1.0" }] })).status).toBe(201);
    expect((await publish(user.token, id, "1.2.0")).status).toBe(201);
    page = await adminGet(adminCookie, `/admin/packages/${id}?m=approved`);
    expect(page.text).toContain("approved by admin");
    expect(page.text).toContain("is approved and public");
    res = await adminPost(adminCookie, `/admin/packages/${id}/approve`, { csrf });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=nothing`);
    expect((await adminGet(adminCookie, "/admin")).text).toContain(`<strong>admin</strong> approved`);

    // Yanking and owners through the admin pages, each logged.
    res = await adminPost(adminCookie, `/admin/packages/${id}/versions/1.2.0/yank`, { csrf });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=yanked`);
    expect((await json(await SELF.fetch(`${API}/packages/${id}/versions/1.2.0`))).yanked).toBe(true);
    res = await adminPost(adminCookie, `/admin/packages/${id}/versions/1.2/unyank`, { csrf });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=unyanked`);
    expect((await adminPost(adminCookie, `/admin/packages/${id}/versions/9.9.9/yank`, { csrf })).status).toBe(404);
    res = await adminPost(adminCookie, `/admin/packages/${id}/owners`, { csrf, login: other.login.toUpperCase() });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=owner_added`);
    expect((await json(await SELF.fetch(`${API}/packages/${id}/owners`))).owners.sort()).toEqual([user.login, other.login].sort());
    res = await adminPost(adminCookie, `/admin/packages/${id}/owners`, { csrf, login: `nobody_${run}` });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=user_not_found`);
    res = await adminPost(adminCookie, `/admin/packages/${id}/owners/${user.login}/remove`, { csrf });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=owner_removed`);
    res = await adminPost(adminCookie, `/admin/packages/${id}/owners/${other.login}/remove`, { csrf });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=last_owner`);
    expect((await json(await SELF.fetch(`${API}/packages/${id}/owners`))).owners).toEqual([other.login]);
    const log = (await env.DB.prepare("SELECT action, version, detail FROM admin_actions WHERE package_id = ? ORDER BY id").bind(id).all()).results;
    expect(log).toEqual([
      { action: "approve", version: null, detail: null },
      { action: "yank", version: "1.2.0", detail: null },
      { action: "unyank", version: "1.2.0", detail: null },
      { action: "owner_add", version: null, detail: other.login.toUpperCase() },
      { action: "owner_remove", version: null, detail: user.login },
    ]);
    page = await adminGet(adminCookie, `/admin/packages/${id}`);
    expect(page.text).toContain("removed owner");
  });

  it("rejects a pending package, removing it and its archives, and caps pending packages per user", async () => {
    const user = await seedUser(`rej${run}`);
    const admin = await seedUser("admin");
    const adminCookie = await cookieFor(admin);
    const id = `rej_${run}`;
    expect((await publish(user.token, id, "1.0.0")).status).toBe(202);
    expect((await publish(user.token, id, "1.1.0")).status).toBe(202);
    expect(await env.ARCHIVES.head(`archives/${id}/1.1.0.zip`)).not.toBeNull();

    const { csrf } = await adminGet(adminCookie, "/admin");
    let res = await adminPost(adminCookie, `/admin/packages/${id}/reject`, { csrf, reason: "not a Flux package" });
    expect(res.status).toBe(303);
    expect(res.headers.get("Location")).toBe("/admin?m=rejected");
    expect(await env.DB.prepare("SELECT 1 FROM packages WHERE id = ?").bind(id).first()).toBeNull();
    expect(await env.DB.prepare("SELECT 1 FROM versions WHERE package_id = ?").bind(id).first()).toBeNull();
    expect(await env.DB.prepare("SELECT 1 FROM owners WHERE package_id = ?").bind(id).first()).toBeNull();
    expect(await env.ARCHIVES.head(`archives/${id}/1.0.0.zip`)).toBeNull();
    expect(await env.ARCHIVES.head(`archives/${id}/1.1.0.zip`)).toBeNull();
    expect((await SELF.fetch(`${API}/packages/${id}`, auth(user.token))).status).toBe(404);
    expect(await env.DB.prepare("SELECT action, detail FROM admin_actions WHERE package_id = ?").bind(id).first()).toEqual({ action: "reject", detail: "not a Flux package" });
    expect((await adminGet(adminCookie, "/admin?m=rejected")).text).toContain("rejected and removed");
    expect((await adminGet(adminCookie, `/admin/packages/${id}`)).status).toBe(404);

    // The id is free again, and an approved package cannot be rejected.
    expect((await publish(user.token, id, "1.0.0")).status).toBe(202);
    await env.DB.prepare("UPDATE packages SET approved_at = created_at WHERE id = ?").bind(id).run();
    res = await adminPost(adminCookie, `/admin/packages/${id}/reject`, { csrf });
    expect(res.headers.get("Location")).toBe(`/admin/packages/${id}?m=nothing`);
    expect((await SELF.fetch(`${API}/packages/${id}`)).status).toBe(200);

    // At most MAX_PENDING_PER_USER packages awaiting approval per creator; more versions of them are fine.
    const cap = await seedUser(`cap${run}`);
    for (let i = 0; i < MAX_PENDING_PER_USER; i++) expect((await publish(cap.token, `cap_${i}_${run}`, "1.0.0")).status).toBe(202);
    res = await publish(cap.token, `cap_${MAX_PENDING_PER_USER}_${run}`, "1.0.0");
    expect(res.status).toBe(403);
    expect((await json(res)).error.code).toBe("pending_limit");
    expect((await publish(cap.token, `cap_0_${run}`, "1.1.0")).status).toBe(202);
    // Handing the packages to someone else neither frees the creator's cap nor uses up theirs.
    const bystander = await seedUser(`by${run}`);
    for (let i = 0; i < MAX_PENDING_PER_USER; i++) {
      expect((await SELF.fetch(`${API}/packages/cap_${i}_${run}/owners/${bystander.login}`, { method: "PUT", ...auth(cap.token) })).status).toBe(200);
      expect((await SELF.fetch(`${API}/packages/cap_${i}_${run}/owners/${cap.login}`, { method: "DELETE", ...auth(cap.token) })).status).toBe(200);
    }
    expect((await publish(cap.token, `cap_${MAX_PENDING_PER_USER}_${run}`, "1.0.0")).status).toBe(403);
    expect((await publish(bystander.token, `by_${run}`, "1.0.0")).status).toBe(202);
    // Rejecting one frees a slot.
    expect((await adminPost(adminCookie, `/admin/packages/cap_0_${run}/reject`, { csrf })).headers.get("Location")).toBe("/admin?m=rejected");
    expect((await publish(cap.token, `cap_${MAX_PENDING_PER_USER}_${run}`, "1.0.0")).status).toBe(202);
  });

  it("publishes an admin's new package as public at once", async () => {
    const admin = await seedUser("admin");
    const id = `adm_${run}`;
    expect((await publish(admin.token, id, "1.0.0")).status).toBe(201);
    expect(await json(await SELF.fetch(`${API}/packages/${id}`))).toMatchObject({ status: "approved", revision: 1 });
    expect(await env.DB.prepare("SELECT approved_by FROM packages WHERE id = ?").bind(id).first()).toEqual({ approved_by: admin.id });
    expect(await env.DB.prepare("SELECT docs_status FROM versions WHERE package_id = ?").bind(id).first()).toEqual({ docs_status: "queued" });
    expect((await json(await SELF.fetch(`${API}/search?q=${id}`))).total).toBe(1);
  });

  it("drops the admin rights of a session whose user no longer holds the admin login", async () => {
    const admin = await seedUser("admin");
    const user = await seedUser(`stale${run}`);
    const id = `stale_${run}`;
    expect((await publish(user.token, id, "1.0.0")).status).toBe(202);
    const cookie = await cookieFor(admin);
    const { csrf } = await adminGet(cookie, "/admin");
    expect(csrf).not.toBe("");
    // Another account took the login on GitHub and logged in here: this row was renamed.
    await env.DB.prepare("UPDATE users SET login = 'admin~old' WHERE id = ?").bind(admin.id).run();
    expect((await SELF.fetch(`${SITE}/admin`, { headers: { Cookie: cookie } })).status).toBe(403);
    // The still valid CSRF token does not help: the admin check comes after it.
    expect((await adminPost(cookie, `/admin/packages/${id}/approve`, { csrf })).status).toBe(403);
    expect(await env.DB.prepare("SELECT approved_at FROM packages WHERE id = ?").bind(id).first()).toEqual({ approved_at: null });
    expect(await env.DB.prepare("SELECT 1 FROM admin_actions WHERE package_id = ?").bind(id).first()).toBeNull();
  });
});

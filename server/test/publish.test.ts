// Publish internals: the dependency query, the D1 commit and the R2 rollback.

import { SELF, env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { archiveOptions, commitVersion, ensureArchive, rollbackArchive } from "../src/api/publish";
import { generateToken, sha256Hex } from "../src/lib/auth";
import { packageZip } from "./zipwriter";

const API = "https://fluxpkg.dev/api/v1";
const run = Math.floor(Math.random() * 1e6);
let githubId = 500_000 + run;

async function seedUser(login: string): Promise<{ id: number; token: string }> {
  const user = await env.DB.prepare("INSERT INTO users (github_id, login, created_at) VALUES (?, ?, '2026-11-01T00:00:00Z') RETURNING id")
    .bind(githubId++, login)
    .first<{ id: number }>();
  const token = generateToken();
  await env.DB.prepare("INSERT INTO tokens (user_id, name, hash, created_at) VALUES (?, 't', ?, '2026-11-01T00:00:00Z')").bind(user!.id, await sha256Hex(token)).run();
  return { id: user!.id, token };
}

async function publish(token: string, id: string, version: string, extra: Record<string, unknown> = {}) {
  const form = new FormData();
  form.append("metadata", JSON.stringify({ id, name: "Test Package", version, dependencies: [], ...extra }));
  form.append("archive", new File([packageZip(id, version)], "a.zip"));
  return SELF.fetch(`${API}/packages/${id}/versions/${version}`, { method: "PUT", body: form, headers: { Authorization: `Bearer ${token}` } });
}

const revision = async (id: string) => (await env.DB.prepare("SELECT revision FROM packages WHERE id = ?").bind(id).first<{ revision: number }>())?.revision;

describe("publish", () => {
  it("checks more dependencies than D1 binds parameters (101)", async () => {
    const u = await seedUser(`deps${run}`);
    const id = `p_deps_${run}`;
    const dependencies = Array.from({ length: 101 }, (_, i) => ({ id: `missing_${i}_${run}`, constraint: "*" }));
    const res = await publish(u.token, id, "1.0.0", { dependencies });
    expect(res.status).toBe(422);
    const body = (await res.json()) as { error: { details: string[] } };
    expect(body.error.details).toHaveLength(101);
    expect(body.error.details[100]).toMatch(`missing_100_${run}`);
  });

  it("bumps the revision only when the guarded version insert lands", async () => {
    const owner = await seedUser(`own${run}`);
    const other = await seedUser(`oth${run}`);
    const id = `p_rev_${run}`;
    expect((await publish(owner.token, id, "1.0.0")).status).toBe(201);
    expect(await revision(id)).toBe(1);
    const args = { id, version: "2.0.0", sortKey: "x", metadata: "{}", sha256: "00", size: 1, at: "2026-11-01T00:00:00Z" };
    expect(await commitVersion(env.DB, { ...args, userId: other.id })).toBe(false);
    expect(await revision(id)).toBe(1);
    expect(await env.DB.prepare("SELECT 1 FROM versions WHERE package_id = ? AND version = '2.0.0'").bind(id).first()).toBeNull();
    expect(await commitVersion(env.DB, { ...args, userId: owner.id })).toBe(true);
    expect(await revision(id)).toBe(2);
    // A brand-new package starts at revision 1 with its first version.
    const fresh = `p_new_${run}`;
    expect(await commitVersion(env.DB, { ...args, id: fresh, version: "1.0.0", userId: other.id })).toBe(true);
    expect(await revision(fresh)).toBe(1);
  });

  it("never leaves a committed row without its archive after a rollback", async () => {
    const u = await seedUser(`rb${run}`);
    const id = `p_rb_${run}`;
    const archive = packageZip(id);
    const sha = await sha256Hex(archive);
    const key = `archives/${id}/1.0.0.zip`;
    const options = archiveOptions(id, "1.0.0", sha);

    // A wrote the object; no row anywhere: the rollback deletes it.
    await env.ARCHIVES.put(key, archive, options);
    await rollbackArchive(env.ARCHIVES, env.DB, key, id, "1.0.0", archive, options);
    expect(await env.ARCHIVES.head(key)).toBeNull();

    // A wrote it, B reused the identical bytes and committed, then A's D1
    // write failed and A rolls back: the object must survive.
    await env.ARCHIVES.put(key, archive, options);
    const committed = await commitVersion(env.DB, { id, version: "1.0.0", sortKey: "k", metadata: "{}", sha256: sha, size: archive.length, userId: u.id, at: "2026-11-01T00:00:00Z" });
    expect(committed).toBe(true);
    await rollbackArchive(env.ARCHIVES, env.DB, key, id, "1.0.0", archive, options);
    expect((await env.ARCHIVES.head(key))?.customMetadata?.sha256).toBe(sha);

    // The other order: A deleted before B's commit; B's ensureArchive puts it back.
    await env.ARCHIVES.delete(key);
    await ensureArchive(env.ARCHIVES, key, archive, options);
    expect((await env.ARCHIVES.head(key))?.customMetadata?.sha256).toBe(sha);

    // Another publisher's bytes are never deleted.
    const other = packageZip(id, "1.0.0", "Other");
    const otherOptions = archiveOptions(id, "1.0.0", await sha256Hex(other));
    await env.ARCHIVES.put(key, other, otherOptions);
    await rollbackArchive(env.ARCHIVES, env.DB, key, id, "1.0.0", archive, options);
    expect(await env.ARCHIVES.head(key)).not.toBeNull();
  });
});

describe("pre-releases", () => {
  it("prefers the newest release for latest, falls back to a pre-release, and keeps them out of plain constraints", async () => {
    const u = await seedUser(`pre${run}`);
    const id = `p_pre_${run}`;
    expect((await publish(u.token, id, "1.9.0")).status).toBe(201);
    expect((await publish(u.token, id, "2.0.0-beta")).status).toBe(201);

    const doc = (await (await SELF.fetch(`${API}/packages/${id}`)).json()) as { latest: string; versions: Record<string, unknown> };
    expect(Object.keys(doc.versions)).toEqual(["2.0.0-beta", "1.9.0"]);
    expect(doc.latest).toBe("1.9.0");
    let res = await SELF.fetch(`https://docs.fluxpkg.dev/${id}/`, { redirect: "manual" });
    expect(res.headers.get("Location")).toBe(`https://docs.fluxpkg.dev/${id}/1.9.0/`);
    const hits = (await (await SELF.fetch(`${API}/search?q=${id}`)).json()) as { packages: { version: string }[] };
    expect(hits.packages[0]!.version).toBe("1.9.0");

    // Only a pre-release: it is the latest.
    const only = `p_only_${run}`;
    expect((await publish(u.token, only, "1.0.0-rc.1")).status).toBe(201);
    expect(((await (await SELF.fetch(`${API}/packages/${only}`)).json()) as { latest: string }).latest).toBe("1.0.0-rc.1");
    res = await SELF.fetch(`https://docs.fluxpkg.dev/${only}/`, { redirect: "manual" });
    expect(res.headers.get("Location")).toBe(`https://docs.fluxpkg.dev/${only}/1.0.0-rc.1/`);

    // A dependency check only accepts a pre-release that the constraint names.
    const user = `p_dep_${run}`;
    res = await publish(u.token, user, "1.0.0", { dependencies: [{ id: only, constraint: "~> 1.0" }] });
    expect(res.status).toBe(422);
    expect(((await res.json()) as { error: { details: string[] } }).error.details.join()).toMatch(/no published version satisfies it \(have 1.0.0-rc.1\)/);
    res = await publish(u.token, user, "1.0.0", { dependencies: [{ id: only, constraint: ">= 1.0.0-beta" }] });
    expect(res.status).toBe(201);
  });
});

// The GitHub login flow with GitHub's endpoints stubbed.

import { SELF, env } from "cloudflare:test";
import { afterEach, describe, expect, it, vi } from "vitest";

const SITE = "https://fluxpkg.dev";

afterEach(() => vi.restoreAllMocks());

function cookiesOf(res: Response): Record<string, string> {
  const out: Record<string, string> = {};
  for (const c of res.headers.getSetCookie()) {
    const [pair] = c.split(";");
    const eq = pair!.indexOf("=");
    out[pair!.slice(0, eq)] = pair!.slice(eq + 1);
  }
  return out;
}

describe("GitHub OAuth", () => {
  it("logs in with a signed state, upserts the user and sets a session", async () => {
    const githubId = 900_000 + Math.floor(Math.random() * 1000);
    const login = `octo${githubId}`;
    const real = globalThis.fetch;
    const calls: string[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation(async (input, init) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url === "https://github.com/login/oauth/access_token") {
        calls.push(`token ${JSON.parse(String(init?.body)).code}`);
        return Response.json({ access_token: "gho_test" });
      }
      if (url === "https://api.github.com/user") {
        calls.push(`user ${new Headers(init?.headers).get("Authorization")}`);
        return Response.json({ id: githubId, login });
      }
      return real(input, init);
    });

    // vitest.config.ts binds a test GITHUB_CLIENT_ID and GITHUB_CLIENT_SECRET.
    expect(env.GITHUB_CLIENT_ID).toBeTruthy();

    let res = await SELF.fetch(`${SITE}/login?next=/tokens`, { redirect: "manual" });
    expect(res.status).toBe(302);
    const authorize = new URL(res.headers.get("Location")!);
    expect(authorize.origin + authorize.pathname).toBe("https://github.com/login/oauth/authorize");
    expect(authorize.searchParams.get("redirect_uri")).toBe(`${SITE}/auth/github/callback`);
    const state = authorize.searchParams.get("state")!;
    const nonce = cookiesOf(res)["fpkg_oauth"]!;
    expect(nonce).toBeTruthy();

    // A state without its nonce cookie, or a forged one, is refused.
    res = await SELF.fetch(`${SITE}/auth/github/callback?code=c1&state=${encodeURIComponent(state)}`, { redirect: "manual" });
    expect(res.status).toBe(400);
    res = await SELF.fetch(`${SITE}/auth/github/callback?code=c1&state=${encodeURIComponent(state + "x")}`, {
      redirect: "manual",
      headers: { Cookie: `fpkg_oauth=${nonce}` },
    });
    expect(res.status).toBe(400);
    expect(calls).toEqual([]);

    res = await SELF.fetch(`${SITE}/auth/github/callback?code=c1&state=${encodeURIComponent(state)}`, {
      redirect: "manual",
      headers: { Cookie: `fpkg_oauth=${nonce}` },
    });
    expect(res.status).toBe(302);
    expect(res.headers.get("Location")).toBe(`${SITE}/tokens`);
    expect(calls).toEqual(["token c1", "user Bearer gho_test"]);
    const setCookie = res.headers.getSetCookie().find((c) => c.startsWith("fpkg_session="))!;
    expect(setCookie).toMatch(/HttpOnly; Secure; SameSite=Lax; Max-Age=2592000/);
    const session = cookiesOf(res)["fpkg_session"]!;

    const row = await env.DB.prepare("SELECT login FROM users WHERE github_id = ?").bind(githubId).first<{ login: string }>();
    expect(row?.login).toBe(login);
    res = await SELF.fetch(`${SITE}/tokens`, { headers: { Cookie: `fpkg_session=${session}` } });
    expect(res.status).toBe(200);
    expect(await res.text()).toContain(login);
  });
});

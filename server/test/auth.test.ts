import { describe, expect, it } from "vitest";
import { csrfToken, generateToken, looksLikeToken, sha256Hex, sign, signSession, verify, verifySession } from "../src/lib/auth";

const SECRET = "a-test-secret-of-some-length";

describe("session cookies", () => {
  it("round-trips and expires after 30 days", async () => {
    const t0 = Date.UTC(2026, 10, 1);
    const value = await signSession(SECRET, 7, "luna", t0);
    expect(await verifySession(SECRET, value, t0 + 1000)).toMatchObject({ uid: 7, login: "luna" });
    expect(await verifySession(SECRET, value, t0 + 29 * 86400_000)).not.toBeNull();
    expect(await verifySession(SECRET, value, t0 + 31 * 86400_000)).toBeNull();
  });

  it("refuses tampering, other secrets and other purposes", async () => {
    const value = await signSession(SECRET, 7, "luna");
    const [body, mac] = value.split(".");
    const forged = btoa(JSON.stringify({ uid: 1, login: "admin", exp: 9999999999 })).replace(/=+$/, "").replace(/\+/g, "-").replace(/\//g, "_");
    expect(await verifySession(SECRET, `${forged}.${mac}`)).toBeNull();
    expect(await verifySession(SECRET, `${body}.${mac!.slice(0, -2)}AA`)).toBeNull();
    expect(await verifySession("another-secret-entirely", value)).toBeNull();
    expect(await verifySession(SECRET, "garbage")).toBeNull();
    expect(await verifySession(SECRET, `${value}.x`)).toBeNull();
    const state = await sign(SECRET, "state", { uid: 7, login: "luna", exp: 9999999999 });
    expect(await verifySession(SECRET, state)).toBeNull();
    expect(await verify(SECRET, "state", state)).not.toBeNull();
  });

  it("derives a stable CSRF token per session", async () => {
    const a = await signSession(SECRET, 1, "a");
    expect(await csrfToken(SECRET, a)).toBe(await csrfToken(SECRET, a));
    expect(await csrfToken(SECRET, a)).not.toBe(await csrfToken(SECRET, a + "x"));
  });
});

describe("API tokens", () => {
  it("are fpk_ plus 32 random bytes in base64url", () => {
    const t = generateToken();
    expect(t).toMatch(/^fpk_[A-Za-z0-9_-]{43}$/);
    expect(looksLikeToken(t)).toBe(true);
    expect(generateToken()).not.toBe(t);
    expect(looksLikeToken("fpk_short")).toBe(false);
  });

  it("are stored as SHA-256 hex", async () => {
    expect(await sha256Hex("abc")).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  });
});

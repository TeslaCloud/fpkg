// API tokens (stored as SHA-256 hex), HMAC-signed session cookies and OAuth state, and bearer auth.

import type { Ctx, Env } from "../env";
import { ApiError, now, unauthorized } from "./http";

const encoder = new TextEncoder();

export function base64url(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function fromBase64url(text: string): Uint8Array | null {
  if (!/^[A-Za-z0-9_-]*$/.test(text)) return null;
  try {
    const s = atob(text.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((text.length + 3) % 4));
    return Uint8Array.from(s, (c) => c.charCodeAt(0));
  } catch {
    return null;
  }
}

export function hex(bytes: ArrayBuffer | Uint8Array): string {
  return [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export async function sha256Hex(data: string | ArrayBuffer | Uint8Array): Promise<string> {
  const bytes = typeof data === "string" ? encoder.encode(data) : data;
  return hex(await crypto.subtle.digest("SHA-256", bytes));
}

export const TOKEN_PREFIX = "fpk_";
const TOKEN_RE = /^fpk_[A-Za-z0-9_-]{43}$/;

/** A new API token: `fpk_` and 32 random bytes in base64url. */
export function generateToken(): string {
  return TOKEN_PREFIX + base64url(crypto.getRandomValues(new Uint8Array(32)));
}

export function looksLikeToken(token: string): boolean {
  return TOKEN_RE.test(token);
}

// --- Signed values ---------------------------------------------------------
// `<base64url(JSON payload)>.<base64url(HMAC-SHA256(secret, purpose + "." + payload))>`;
// the purpose ("session", "state") keeps one kind from being replayed as another.

async function hmacKey(secret: string): Promise<CryptoKey> {
  return crypto.subtle.importKey("raw", encoder.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
}

export async function sign(secret: string, purpose: string, payload: object): Promise<string> {
  const body = base64url(encoder.encode(JSON.stringify(payload)));
  const mac = await crypto.subtle.sign("HMAC", await hmacKey(secret), encoder.encode(`${purpose}.${body}`));
  return `${body}.${base64url(new Uint8Array(mac))}`;
}

/** The payload of a value signed for `purpose`, or null (bad signature, malformed, or `exp` passed). */
export async function verify<T extends { exp: number }>(secret: string, purpose: string, value: string, at = Date.now()): Promise<T | null> {
  const dot = value.indexOf(".");
  if (dot <= 0 || value.indexOf(".", dot + 1) !== -1) return null;
  const body = value.slice(0, dot);
  const mac = fromBase64url(value.slice(dot + 1));
  if (!mac || mac.length !== 32) return null;
  // subtle.verify compares in constant time.
  const ok = await crypto.subtle.verify("HMAC", await hmacKey(secret), mac, encoder.encode(`${purpose}.${body}`));
  if (!ok) return null;
  const raw = fromBase64url(body);
  if (!raw) return null;
  let payload: T;
  try {
    payload = JSON.parse(new TextDecoder().decode(raw)) as T;
  } catch {
    return null;
  }
  if (typeof payload !== "object" || payload === null || typeof payload.exp !== "number" || payload.exp * 1000 <= at) return null;
  return payload;
}

export interface Session {
  /** users.id */
  uid: number;
  login: string;
  /** Expiry, seconds since the epoch. */
  exp: number;
}

export const SESSION_COOKIE = "fpkg_session";
export const STATE_COOKIE = "fpkg_oauth";
export const SESSION_SECONDS = 30 * 24 * 3600;

export function requireSecret(env: Env): string {
  const s = env.SESSION_SECRET;
  if (!s || s.length < 16) throw new ApiError(500, "misconfigured", "SESSION_SECRET is not set (see server/README.md)");
  return s;
}

export async function signSession(secret: string, uid: number, login: string, at = Date.now()): Promise<string> {
  return sign(secret, "session", { uid, login, exp: Math.floor(at / 1000) + SESSION_SECONDS } satisfies Session);
}

export async function verifySession(secret: string, value: string, at = Date.now()): Promise<Session | null> {
  const s = await verify<Session>(secret, "session", value, at);
  return s && typeof s.uid === "number" && typeof s.login === "string" ? s : null;
}

export function sessionCookie(value: string, secure: boolean): string {
  return `${SESSION_COOKIE}=${value}; Path=/; HttpOnly;${secure ? " Secure;" : ""} SameSite=Lax; Max-Age=${SESSION_SECONDS}`;
}

export function clearCookie(name: string, secure: boolean): string {
  return `${name}=; Path=/; HttpOnly;${secure ? " Secure;" : ""} SameSite=Lax; Max-Age=0`;
}

export function parseCookies(header: string | null): Record<string, string> {
  const out: Record<string, string> = {};
  for (const part of (header ?? "").split(";")) {
    const eq = part.indexOf("=");
    if (eq > 0) out[part.slice(0, eq).trim()] = part.slice(eq + 1).trim();
  }
  return out;
}

/** The CSRF token of a session: an HMAC of the session cookie value. */
export async function csrfToken(secret: string, sessionValue: string): Promise<string> {
  const mac = await crypto.subtle.sign("HMAC", await hmacKey(secret), encoder.encode(`csrf.${sessionValue}`));
  return base64url(new Uint8Array(mac));
}

/** Compares two strings in time independent of where they differ. */
export function safeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let d = 0;
  for (let i = 0; i < a.length; i++) d |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return d === 0;
}

export function adminLogins(env: Env): Set<string> {
  return new Set(
    (env.ADMIN_LOGINS ?? "")
      .split(/[\s,]+/)
      .map((s) => s.trim().toLowerCase())
      .filter(Boolean),
  );
}

export function isAdmin(env: Env, login: string): boolean {
  return adminLogins(env).has(login.toLowerCase());
}

// --- Bearer tokens ---------------------------------------------------------

export interface ApiUser {
  userId: number;
  login: string;
  tokenId: number;
  tokenName: string;
  admin: boolean;
}

/** `last_used_at` is written at most this often per token. */
const LAST_USED_INTERVAL_MS = 3600 * 1000;

/** Authenticates `Authorization: Bearer fpk_...`: null without the header, 401 for a bad or revoked token. */
export async function bearerUser(env: Env, ctx: Ctx, request: Request): Promise<ApiUser | null> {
  const header = request.headers.get("Authorization");
  if (!header) return null;
  const m = /^Bearer\s+(\S+)\s*$/i.exec(header);
  if (!m) throw unauthorized("the Authorization header must be `Bearer fpk_...`");
  const token = m[1]!;
  if (!looksLikeToken(token)) throw unauthorized("that is not an fpkg API token");
  const hash = await sha256Hex(token);
  const row = await env.DB.prepare(
    `SELECT t.id AS token_id, t.name AS token_name, t.last_used_at, t.revoked_at, u.id AS user_id, u.login
       FROM tokens t JOIN users u ON u.id = t.user_id WHERE t.hash = ?`,
  )
    .bind(hash)
    .first<{ token_id: number; token_name: string; last_used_at: string | null; revoked_at: string | null; user_id: number; login: string }>();
  if (!row) throw unauthorized("unknown API token");
  if (row.revoked_at) throw unauthorized("this API token has been revoked");
  const stale = !row.last_used_at || Date.parse(row.last_used_at) < Date.now() - LAST_USED_INTERVAL_MS;
  if (stale) {
    const at = now();
    const cutoff = now(new Date(Date.now() - LAST_USED_INTERVAL_MS));
    ctx.waitUntil(
      env.DB.prepare("UPDATE tokens SET last_used_at = ? WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)")
        .bind(at, row.token_id, cutoff)
        .run()
        .catch((e) => console.error("last_used_at update failed", e)),
    );
  }
  return { userId: row.user_id, login: row.login, tokenId: row.token_id, tokenName: row.token_name, admin: isAdmin(env, row.login) };
}

export async function requireUser(env: Env, ctx: Ctx, request: Request): Promise<ApiUser> {
  const user = await bearerUser(env, ctx, request);
  if (!user) throw unauthorized();
  return user;
}

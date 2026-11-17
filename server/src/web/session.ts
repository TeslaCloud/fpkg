// The website's sessions: the signed cookie, CSRF checks on form posts, and who counts as an admin.

import type { Context } from "hono";
import type { AppEnv, Env } from "../env";
import { SESSION_COOKIE, csrfToken, isAdmin, parseCookies, safeEqual, verifySession, type Session } from "../lib/auth";
import { cached } from "../lib/cache";
import { ApiError } from "../lib/http";
import { trimSlash } from "../lib/metadata";

export type C = Context<AppEnv>;

export interface Visitor {
  session: Session;
  csrf: string;
  /** By the login in the cookie: enough for navigation, not for admin pages (see `requireAdmin`). */
  admin: boolean;
}

/** The logged-in visitor, or null (no cookie, a bad one, or no secret configured). */
export async function visitor(c: C): Promise<Visitor | null> {
  const value = parseCookies(c.req.header("Cookie") ?? null)[SESSION_COOKIE];
  const secret = c.env.SESSION_SECRET;
  if (!value || !secret) return null;
  const session = await verifySession(secret, value);
  if (!session) return null;
  return { session, csrf: await csrfToken(secret, value), admin: isAdmin(c.env, session.login) };
}

/** Renders for a visitor; anonymous pages go through the cache, logged-in ones are private. */
export async function render(c: C, build: (v: Visitor | null) => Promise<Response>): Promise<Response> {
  const v = await visitor(c);
  if (v) return build(v);
  return cached(c.req.raw, c.executionCtx, () => build(null));
}

export function secure(env: Env): boolean {
  return env.SITE_URL.startsWith("https:");
}

export function site(env: Env): string {
  return trimSlash(env.SITE_URL);
}

/** Refuses a form POST from another origin, without a session, or with a stale CSRF token. */
export async function requirePost(c: C): Promise<{ v: Visitor; form: FormData }> {
  const origin = c.req.header("Origin");
  if (origin && origin !== "null" && origin !== new URL(c.env.SITE_URL).origin) throw new ApiError(403, "forbidden", "cross-origin form posts are refused");
  const v = await visitor(c);
  if (!v) throw new ApiError(401, "unauthorized", "log in first");
  const form = await c.req.raw.formData();
  const token = form.get("csrf");
  if (typeof token !== "string" || !safeEqual(token, v.csrf)) throw new ApiError(403, "forbidden", "the form has expired; reload the page and try again");
  return { v, form };
}

/**
 * The visitor as an admin, or null. The cookie's login is checked against the user's current row:
 * a session signed before its user was renamed (another account took the login) must not keep
 * admin rights for the cookie's lifetime.
 */
export async function adminOf(c: C, v: Visitor | null): Promise<Visitor | null> {
  if (!v || !v.admin) return null;
  const row = await c.env.DB.prepare("SELECT login FROM users WHERE id = ?").bind(v.session.uid).first<{ login: string }>();
  return row && isAdmin(c.env, row.login) ? v : null;
}

/** A same-site path to return to after login. */
export function safeNext(next: string | undefined | null): string {
  return next && next.startsWith("/") && !next.startsWith("//") && !next.includes("\\") ? next : "/";
}

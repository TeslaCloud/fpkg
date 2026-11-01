// Server-rendered HTML for the website and the docs host's own pages. No scripts: the CSP forbids them.

import { CACHE_CONTROL } from "../lib/cache";

export function esc(s: unknown): string {
  return String(s ?? "")
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** A piece of HTML that is already escaped. */
export class Html {
  constructor(public readonly value: string) {}
  toString(): string {
    return this.value;
  }
}

/** Tagged template that escapes every interpolation except Html values (and arrays of them). */
export function html(strings: TemplateStringsArray, ...values: unknown[]): Html {
  let out = strings[0]!;
  values.forEach((v, i) => {
    out += render(v) + strings[i + 1]!;
  });
  return new Html(out);
}

function render(v: unknown): string {
  if (v instanceof Html) return v.value;
  if (Array.isArray(v)) return v.map(render).join("");
  if (v === null || v === undefined || v === false) return "";
  return esc(v);
}

export const SITE_CSP = [
  "default-src 'none'",
  "style-src 'self'",
  "img-src 'self' https://avatars.githubusercontent.com",
  "form-action 'self' https://github.com",
  "base-uri 'none'",
  "frame-ancestors 'none'",
].join("; ");

export const SECURITY_HEADERS: Record<string, string> = {
  "Content-Security-Policy": SITE_CSP,
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "strict-origin-when-cross-origin",
  "X-Frame-Options": "DENY",
};

export const STYLE = `
:root { color-scheme: light dark; --fg: #1d1f23; --muted: #646a73; --bg: #fff; --line: #e3e5e8; --accent: #3b5bdb; --code: #f4f5f7; }
@media (prefers-color-scheme: dark) { :root { --fg: #e4e6ea; --muted: #9aa0a8; --bg: #16181c; --line: #2c3036; --accent: #8ca4ff; --code: #22252a; } }
* { box-sizing: border-box; }
body { margin: 0; font: 15px/1.55 system-ui, -apple-system, "Segoe UI", sans-serif; color: var(--fg); background: var(--bg); }
a { color: var(--accent); text-decoration: none; } a:hover { text-decoration: underline; }
header.top { display: flex; gap: 1rem; align-items: center; padding: .75rem 1.25rem; border-bottom: 1px solid var(--line); }
header.top .brand { font-weight: 700; color: var(--fg); margin-right: auto; }
main { max-width: 60rem; margin: 0 auto; padding: 1.5rem 1.25rem 4rem; }
h1 { font-size: 1.6rem; margin: .2rem 0 .6rem; } h2 { font-size: 1.15rem; margin-top: 2rem; }
.muted { color: var(--muted); }
code, pre, .token { font: 13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; background: var(--code); border-radius: 4px; }
code { padding: .1rem .3rem; } pre { padding: .75rem 1rem; overflow-x: auto; }
.token { display: block; padding: .75rem 1rem; word-break: break-all; user-select: all; border: 1px solid var(--accent); }
table { border-collapse: collapse; width: 100%; } th, td { text-align: left; padding: .4rem .5rem; border-bottom: 1px solid var(--line); vertical-align: top; }
form.search { display: flex; gap: .5rem; } form.search input { flex: 1; }
input, button { font: inherit; padding: .4rem .6rem; border: 1px solid var(--line); border-radius: 4px; background: var(--bg); color: var(--fg); }
button { cursor: pointer; } button.link { border: 0; padding: 0; color: var(--accent); background: none; }
ul.packages { list-style: none; padding: 0; } ul.packages li { padding: .6rem 0; border-bottom: 1px solid var(--line); }
.badge { font-size: 12px; padding: .05rem .4rem; border-radius: 999px; border: 1px solid var(--line); color: var(--muted); }
.badge.warn { color: #c92a2a; border-color: #c92a2a; }
.notice { padding: .75rem 1rem; border: 1px solid var(--line); border-radius: 6px; margin: 1rem 0; }
.error { border-color: #c92a2a; }
`;

export interface PageUser {
  login: string;
}

export interface PageOptions {
  title: string;
  body: Html;
  user?: PageUser | null;
  /** CSRF token for the logout form, when logged in. */
  csrf?: string | null;
  status?: number;
  /** Overrides the default cache header (public, or private when logged in). */
  cacheControl?: string;
  headers?: Record<string, string>;
}

export function page(o: PageOptions): Response {
  const nav = o.user
    ? html`<a href="/tokens">Tokens</a>
        <form method="post" action="/logout"><input type="hidden" name="csrf" value="${o.csrf ?? ""}"><button class="link" type="submit">Log out ${o.user.login}</button></form>`
    : html`<a href="/login">Log in with GitHub</a>`;
  const doc = html`<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${o.title}</title>
<link rel="stylesheet" href="/static/style.css">
</head>
<body>
<header class="top"><a class="brand" href="/">fluxpkg</a><a href="/search">Search</a>${nav}</header>
<main>
${o.body}
</main>
</body>
</html>
`;
  return new Response(doc.value, {
    status: o.status ?? 200,
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": o.cacheControl ?? (o.user ? CACHE_CONTROL.pagePrivate : CACHE_CONTROL.page),
      ...SECURITY_HEADERS,
      ...(o.headers ?? {}),
    },
  });
}

/** A standalone page for the docs host (no site chrome, no cookies). */
export function plainPage(title: string, body: Html, status: number, cacheControl: string, headers: Record<string, string> = {}): Response {
  const doc = html`<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${title}</title><style>${new Html(STYLE)}</style></head>
<body><main>${body}</main></body></html>
`;
  return new Response(doc.value, {
    status,
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": cacheControl,
      "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'",
      "X-Content-Type-Options": "nosniff",
      ...headers,
    },
  });
}

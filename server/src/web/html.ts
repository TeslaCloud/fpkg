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
:root {
  color-scheme: light dark;
  --bg: #f7f7fa; --surface: #ffffff; --fg: #1a1c23; --muted: #5d6370; --faint: #8a90a0;
  --line: #e3e5eb; --line-strong: #cbd0db;
  --accent: #5440d4; --accent-ink: #ffffff; --accent-soft: #ebe8fb; --accent-strong: #3f2fa8;
  --code-bg: #f0f1f5; --code-fg: #2b2e3a;
  --warn: #b3232d; --warn-soft: #fbe9ea; --ok: #1d7a3e; --ok-soft: #e4f4e9; --pend: #9a5a00; --pend-soft: #fff1da;
  --hero-bg: linear-gradient(135deg, #3a2ca3 0%, #5440d4 55%, #7a66f2 100%); --hero-fg: #ffffff;
  --shadow: 0 1px 2px rgba(22, 25, 40, .06), 0 10px 28px -18px rgba(22, 25, 40, .3);
  --radius: 10px;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #121318; --surface: #1a1c23; --fg: #e7e8ed; --muted: #9ba1ae; --faint: #6f7584;
    --line: #272a34; --line-strong: #3a3e4b;
    --accent: #a394ff; --accent-ink: #121318; --accent-soft: #272343; --accent-strong: #c6bcff;
    --code-bg: #22242d; --code-fg: #dcdee6;
    --warn: #ff8088; --warn-soft: #3a1f23; --ok: #62d48d; --ok-soft: #1a3324; --pend: #ffbe5c; --pend-soft: #3b2d13;
    --hero-bg: linear-gradient(135deg, #2a2078 0%, #4a3bb9 55%, #6c57e3 100%);
    --shadow: 0 1px 2px rgba(0, 0, 0, .5), 0 10px 28px -18px rgba(0, 0, 0, .9);
  }
}
* { box-sizing: border-box; }
html { -webkit-text-size-adjust: 100%; }
body { margin: 0; font: 15px/1.6 system-ui, -apple-system, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; color: var(--fg); background: var(--bg); display: flex; flex-direction: column; min-height: 100vh; }
a { color: var(--accent); text-decoration: none; }
a:hover { text-decoration: underline; }
h1, h2, h3 { line-height: 1.25; margin: 0 0 .5rem; letter-spacing: -.01em; }
h1 { font-size: 1.75rem; } h2 { font-size: 1.2rem; margin-top: 2rem; } h3 { font-size: 1rem; }
p { margin: .5rem 0 1rem; }
.muted { color: var(--muted); } .faint { color: var(--faint); } .small { font-size: 13px; }
.wrap { width: 100%; max-width: 68rem; margin: 0 auto; padding: 0 1.25rem; }
main { flex: 1; padding: 1.75rem 0 4rem; }
code, pre, kbd { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, "Liberation Mono", monospace; font-size: 13px; }
code { background: var(--code-bg); color: var(--code-fg); padding: .1rem .35rem; border-radius: 5px; }
pre { margin: 0; padding: .9rem 1rem; overflow-x: auto; background: var(--code-bg); color: var(--code-fg); border-radius: var(--radius); line-height: 1.5; }
pre code { background: none; padding: 0; }

/* Header */
.top { background: var(--surface); border-bottom: 1px solid var(--line); position: sticky; top: 0; z-index: 2; }
.top .wrap { display: flex; align-items: center; gap: 1rem; min-height: 3.5rem; flex-wrap: wrap; padding-top: .4rem; padding-bottom: .4rem; }
.brand { display: inline-flex; align-items: center; gap: .5rem; font-weight: 700; font-size: 1.1rem; color: var(--fg); letter-spacing: -.01em; }
.brand:hover { text-decoration: none; }
.brand svg { width: 24px; height: 24px; display: block; }
.brand .mark { fill: var(--accent); }
.top nav { display: flex; align-items: center; gap: 1.1rem; margin-left: auto; flex-wrap: wrap; }
.top nav a { color: var(--muted); font-weight: 500; }
.top nav a:hover, .top nav a[aria-current] { color: var(--fg); text-decoration: none; }
.top nav .me { color: var(--fg); font-weight: 600; }
.top .search { flex: 0 1 20rem; }
.top .search input { background: var(--bg); }

/* Forms and buttons */
input, select, textarea, button { font: inherit; color: var(--fg); }
input[type=text], input[type=search], input[type=email], select, textarea { width: 100%; padding: .5rem .7rem; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); }
input:focus, select:focus, textarea:focus, button:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; border-color: var(--accent); }
.btn, button.btn { display: inline-flex; align-items: center; gap: .4rem; padding: .45rem .9rem; border: 1px solid var(--line-strong); border-radius: 8px; background: var(--surface); color: var(--fg); font-weight: 500; cursor: pointer; line-height: 1.3; white-space: nowrap; }
.btn:hover { text-decoration: none; border-color: var(--accent); color: var(--accent); }
.btn.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-ink); }
.btn.primary:hover { background: var(--accent-strong); border-color: var(--accent-strong); color: var(--accent-ink); }
.btn.danger { color: var(--warn); border-color: var(--warn); }
.btn.danger:hover { background: var(--warn-soft); }
.btn.ok { color: var(--ok); border-color: var(--ok); }
.btn.ok:hover { background: var(--ok-soft); }
.btn.sm { padding: .25rem .6rem; font-size: 13px; }
button.link { border: 0; padding: 0; background: none; color: var(--accent); cursor: pointer; font: inherit; }
button.link:hover { text-decoration: underline; }
button.link.danger { color: var(--warn); }
form.inline { display: inline; }
.search { display: flex; gap: .5rem; }
.search input { flex: 1; }
.field { display: flex; gap: .5rem; align-items: center; flex-wrap: wrap; }
.field input { flex: 1; min-width: 12rem; }
.field label { font-weight: 500; }
.actions { display: flex; gap: .5rem; flex-wrap: wrap; align-items: center; }
.actions form.inline { display: inline-flex; gap: .4rem; align-items: center; }
.actions input[type=text] { width: auto; max-width: 16rem; padding: .3rem .6rem; }

/* Hero */
.hero { background: var(--hero-bg); color: var(--hero-fg); padding: 3rem 0 2.75rem; }
.hero h1 { font-size: 2.2rem; margin: 0 0 .4rem; color: var(--hero-fg); }
.hero p { margin: 0 0 1.4rem; font-size: 1.05rem; opacity: .9; max-width: 38rem; }
.hero .search { max-width: 40rem; }
.hero .search input { padding: .8rem 1rem; font-size: 1.05rem; border: 0; border-radius: 10px; background: #fff; color: #1a1c23; box-shadow: var(--shadow); }
.hero .search .btn { padding: .8rem 1.3rem; border: 0; border-radius: 10px; background: #1a1c23; color: #fff; font-size: 1rem; }
.hero .search .btn:hover { background: #000; color: #fff; }
.hero .stats { display: flex; gap: 2rem; margin-top: 1.6rem; flex-wrap: wrap; }
.hero .stats b { display: block; font-size: 1.4rem; line-height: 1.2; }
.hero .stats span { font-size: 13px; opacity: .85; }
.hero code { background: rgba(255,255,255,.18); color: #fff; }

/* Cards and grids */
.card { background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); box-shadow: var(--shadow); }
.card > .card-head { display: flex; align-items: baseline; justify-content: space-between; gap: 1rem; padding: .85rem 1rem; border-bottom: 1px solid var(--line); }
.card > .card-head h2, .card > .card-head h3 { margin: 0; font-size: .95rem; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); }
.card > .card-body { padding: 1rem; }
.grid { display: grid; gap: 1.25rem; }
.grid.cols-3 { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.grid.cols-2 { grid-template-columns: repeat(2, minmax(0, 1fr)); }
.tiles { display: grid; gap: 1rem; grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr)); margin: 1rem 0 1.5rem; }
.tile { background: var(--surface); border: 1px solid var(--line); border-radius: var(--radius); padding: .9rem 1rem; }
.tile b { display: block; font-size: 1.5rem; line-height: 1.2; letter-spacing: -.02em; }
.tile span { color: var(--muted); font-size: 13px; }
.tile.attention b { color: var(--pend); }

/* Package lists */
.pkgs { list-style: none; margin: 0; padding: 0; }
.pkgs li { padding: .7rem 1rem; border-bottom: 1px solid var(--line); }
.pkgs li:last-child { border-bottom: 0; }
.pkgs .row { display: flex; align-items: baseline; gap: .6rem; flex-wrap: wrap; }
.pkgs .name { font-weight: 600; }
.pkgs .id { color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; }
.pkgs .meta { margin-left: auto; color: var(--faint); font-size: 13px; white-space: nowrap; }
.pkgs .summary { color: var(--muted); font-size: 14px; margin-top: .15rem; }
.results .pkgs li { padding: .9rem 1rem; }
.empty { padding: 1.5rem 1rem; color: var(--muted); text-align: center; }

/* Badges */
.badge { display: inline-block; font-size: 12px; font-weight: 600; line-height: 1.5; padding: 0 .5rem; border-radius: 999px; background: var(--accent-soft); color: var(--accent-strong); vertical-align: middle; white-space: nowrap; }
.badge.quiet { background: var(--code-bg); color: var(--muted); }
.badge.warn { background: var(--warn-soft); color: var(--warn); }
.badge.ok { background: var(--ok-soft); color: var(--ok); }
.badge.pend { background: var(--pend-soft); color: var(--pend); }

/* Package page */
.page-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; flex-wrap: wrap; margin-bottom: 1.25rem; }
.page-head h1 { margin: 0; display: flex; align-items: center; gap: .6rem; flex-wrap: wrap; }
.page-head .version { font-size: 1rem; font-weight: 500; color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
.page-head .lede { margin: .3rem 0 0; color: var(--muted); font-size: 1.05rem; }
.layout { display: grid; grid-template-columns: minmax(0, 1fr) 17rem; gap: 1.5rem; align-items: start; }
.layout aside .card + .card { margin-top: 1rem; }
.kv { margin: 0; display: grid; grid-template-columns: auto 1fr; gap: .3rem .8rem; font-size: 14px; }
.kv dt { color: var(--muted); } .kv dd { margin: 0; min-width: 0; overflow-wrap: anywhere; }
.links { list-style: none; margin: 0; padding: 0; }
.links li { padding: .3rem 0; }
.owners { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: .4rem; }
.owners li { display: flex; align-items: center; gap: .5rem; }
.avatar { width: 22px; height: 22px; border-radius: 50%; background: var(--code-bg); flex: none; }
.snippet { border: 1px solid var(--line); border-radius: var(--radius); overflow: hidden; background: var(--code-bg); }
.snippet .snippet-head { display: flex; justify-content: space-between; gap: 1rem; padding: .4rem .9rem; font-size: 12px; color: var(--muted); border-bottom: 1px solid var(--line); background: var(--surface); }
.snippet pre { border-radius: 0; }
.prose { max-width: 48rem; white-space: pre-wrap; overflow-wrap: anywhere; }
.deps { list-style: none; margin: 0; padding: 0; display: flex; flex-wrap: wrap; gap: .5rem; }
.deps li { border: 1px solid var(--line); border-radius: 8px; padding: .3rem .7rem; background: var(--surface); }

/* Tables */
.table-wrap { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: 14px; }
th, td { text-align: left; padding: .55rem .75rem; border-bottom: 1px solid var(--line); vertical-align: top; }
th { font-size: 12px; text-transform: uppercase; letter-spacing: .05em; color: var(--muted); font-weight: 600; }
tr:last-child td { border-bottom: 0; }
td.num, th.num { text-align: right; white-space: nowrap; }
td.actions-cell { white-space: nowrap; }
.card table { margin: 0; }

/* Notices and misc */
.notice { padding: .8rem 1rem; border: 1px solid var(--line); border-left: 4px solid var(--accent); border-radius: 8px; margin: 1rem 0; background: var(--surface); }
.notice.error { border-left-color: var(--warn); }
.notice.pend { border-left-color: var(--pend); }
.notice.ok { border-left-color: var(--ok); }
.notice p:last-child { margin-bottom: 0; }
.token { display: block; padding: .8rem 1rem; word-break: break-all; user-select: all; border: 1px dashed var(--accent); border-radius: 8px; background: var(--accent-soft); color: var(--accent-strong); font-size: 14px; }
.pagination { display: flex; gap: 1rem; margin-top: 1.25rem; }
.toolbar { display: flex; gap: .75rem; align-items: center; flex-wrap: wrap; margin: 1rem 0; }
.toolbar .search { flex: 1 1 16rem; }
.toolbar select { width: auto; }
.subnav { display: flex; gap: .25rem; margin: 0 0 1.25rem; border-bottom: 1px solid var(--line); }
.subnav a { padding: .5rem .8rem; color: var(--muted); font-weight: 500; border-bottom: 2px solid transparent; margin-bottom: -1px; }
.subnav a:hover { text-decoration: none; color: var(--fg); }
.subnav a[aria-current] { color: var(--accent); border-bottom-color: var(--accent); }
.section-head { display: flex; align-items: baseline; justify-content: space-between; gap: 1rem; margin-top: 2rem; }
.section-head h2 { margin: 0; }
.footer { border-top: 1px solid var(--line); padding: 1.25rem 0; color: var(--muted); font-size: 13px; }
.footer .wrap { display: flex; gap: 1.25rem; flex-wrap: wrap; }
.footer a { color: var(--muted); }
.timeline { list-style: none; margin: 0; padding: 0; }
.timeline li { padding: .5rem 1rem; border-bottom: 1px solid var(--line); font-size: 14px; display: flex; gap: .75rem; flex-wrap: wrap; }
.timeline li:last-child { border-bottom: 0; }
.timeline time { color: var(--faint); font-size: 13px; margin-left: auto; white-space: nowrap; }

@media (max-width: 760px) {
  .grid.cols-3, .grid.cols-2, .layout { grid-template-columns: 1fr; }
  .hero { padding: 2.25rem 0 2rem; } .hero h1 { font-size: 1.7rem; }
  .top .search { order: 3; flex: 1 1 100%; }
  .pkgs .meta { margin-left: 0; flex-basis: 100%; }
}
`;

/** A short fingerprint of the stylesheet, so a changed design is fetched past the stylesheet's max-age. */
export const STYLE_VERSION = (() => {
  let h = 0x811c9dc5;
  for (let i = 0; i < STYLE.length; i++) h = Math.imul(h ^ STYLE.charCodeAt(i), 0x01000193) >>> 0;
  return h.toString(16).padStart(8, "0");
})();

export const STYLE_HREF = `/static/style.css?v=${STYLE_VERSION}`;

const LOGO = new Html(
  `<svg viewBox="0 0 24 24" aria-hidden="true"><path class="mark" d="M12 2.2 21 7.1v9.8l-9 4.9-9-4.9V7.1l9-4.9Zm0 2.3L5.4 8.1 12 11.7l6.6-3.6L12 4.5ZM5 9.8v6.1l6 3.3v-6.1L5 9.8Zm14 0-6 3.3v6.1l6-3.3V9.8Z"/></svg>`,
);

export interface PageUser {
  login: string;
  admin: boolean;
}

export interface PageOptions {
  title: string;
  body: Html;
  /** Rendered above `main`, full width (the front page's hero). */
  hero?: Html;
  user?: PageUser | null;
  /** CSRF token for the logout form, when logged in. */
  csrf?: string | null;
  /** The docs host, for the navigation. */
  docs: string;
  /** The header search box's current query. */
  query?: string;
  /** The navigation entry to mark as current: "packages", "admin", "tokens". */
  current?: string;
  status?: number;
  /** Overrides the default cache header (public, or private when logged in). */
  cacheControl?: string;
  headers?: Record<string, string>;
}

export function page(o: PageOptions): Response {
  const cur = (name: string) => (o.current === name ? new Html(" aria-current=\"page\"") : "");
  const nav = html`<nav>
      <a href="/search"${cur("packages")}>Packages</a>
      <a href="${o.docs}/">Docs</a>
      ${o.user?.admin ? html`<a href="/admin"${cur("admin")}>Admin</a>` : ""}
      ${o.user
        ? html`<a href="/tokens"${cur("tokens")}>Tokens</a><span class="me">${o.user.login}</span>
            <form class="inline" method="post" action="/logout"><input type="hidden" name="csrf" value="${o.csrf ?? ""}"><button class="link" type="submit">Log out</button></form>`
        : html`<a class="btn sm" href="/login">Log in with GitHub</a>`}
    </nav>`;
  const doc = html`<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${o.title}</title>
<link rel="stylesheet" href="${STYLE_HREF}">
</head>
<body>
<header class="top"><div class="wrap">
  <a class="brand" href="/">${LOGO}fluxpkg</a>
  ${o.hero ? "" : html`<form class="search" method="get" action="/search"><input type="search" name="q" value="${o.query ?? ""}" placeholder="Search packages" aria-label="Search packages"></form>`}
  ${nav}
</div></header>
${o.hero ?? ""}
<main><div class="wrap">
${o.body}
</div></main>
<footer class="footer"><div class="wrap">
  <span>fluxpkg, the package registry for Flux.</span>
  <a href="${o.docs}/">Documentation</a>
  <a href="/api/v1/search">API</a>
  <a href="https://github.com/TeslaCloud/flux-ce" rel="noopener">Flux</a>
</div></footer>
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

/** A standalone page for the docs host (no cookies, no forms); `site` links the brand back to the registry. */
export function plainPage(title: string, body: Html, status: number, cacheControl: string, headers: Record<string, string> = {}, site?: string): Response {
  const doc = html`<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${title}</title><style>${new Html(STYLE)}</style></head>
<body>
<header class="top"><div class="wrap"><a class="brand" href="${site ?? "/"}">${LOGO}fluxpkg</a><nav><a href="/">Documentation</a>${site ? html`<a href="${site}/">Packages</a>` : ""}</nav></div></header>
<main><div class="wrap">${body}</div></main>
</body></html>
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

/** `12345` as `12.3k`; counts in lists. */
export function compact(n: number): string {
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

/** `2026-11-01T18:04:11Z` as `2026-11-01`. */
export function day(iso: string): string {
  return iso.slice(0, 10);
}

/** `2026-11-01T18:04:11Z` as `2026-11-01 18:04`. */
export function minute(iso: string): string {
  return iso.slice(0, 16).replace("T", " ");
}

/** Bytes as KiB or MiB. */
export function size(bytes: number): string {
  return bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MiB` : `${(bytes / 1024).toFixed(1)} KiB`;
}

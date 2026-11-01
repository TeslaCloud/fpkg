// fluxpkg.dev and docs.fluxpkg.dev in one Worker: `fetch` dispatches on the host, `queue` generates docs.

import { Hono } from "hono";
import { api, apiNotFound } from "./api/index";
import { docsFetch } from "./docs/host";
import { handleDocsBatch } from "./docs/generate";
import type { AppEnv, DocsJob, Env } from "./env";
import { withApiHeader } from "./lib/http";
import { web, webNotFound } from "./web/index";

const site = new Hono<AppEnv>();
site.route("/api/v1", api);
site.route("/", web);
site.notFound((c) => (new URL(c.req.url).pathname.startsWith("/api/") ? apiNotFound(c) : webNotFound(c)));

/**
 * The docs-root-relative path of a docs request, or null for the site. A DOCS_URL
 * with a path matches on any host, since `wrangler dev` rewrites every host to one.
 */
export function docsPath(url: URL, env: Env): string | null {
  let docs: URL;
  try {
    docs = new URL(env.DOCS_URL);
  } catch {
    return null;
  }
  const prefix = docs.pathname.replace(/\/+$/, "");
  if (prefix === "") return url.hostname === docs.hostname ? url.pathname : null;
  if (url.pathname === prefix || url.pathname.startsWith(`${prefix}/`)) return url.pathname.slice(prefix.length) || "/";
  return null;
}

export default {
  async fetch(request: Request, env: Env, ctx: ExecutionContext): Promise<Response> {
    const url = new URL(request.url);
    const docs = docsPath(url, env);
    let res: Response;
    try {
      res = docs !== null ? await docsFetch(request, docs, env, ctx) : await site.fetch(request, env, ctx);
    } catch (e) {
      console.error(`${request.method} ${request.url}:`, e);
      res = new Response("internal server error", { status: 500 });
    }
    return withApiHeader(res);
  },

  async queue(batch: MessageBatch<DocsJob>, env: Env): Promise<void> {
    await handleDocsBatch(batch, env);
  },
} satisfies ExportedHandler<Env, DocsJob>;

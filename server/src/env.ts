/** The Worker's bindings, vars and secrets (wrangler.toml). */
export interface Env {
  DB: D1Database;
  ARCHIVES: R2Bucket;
  DOCS: R2Bucket;
  DOCS_QUEUE: Queue<DocsJob>;
  ADMIN_LOGINS: string;
  FDOC_VERSION: string;
  SITE_URL: string;
  DOCS_URL: string;
  /** Optional: source bytes above which a docs request enqueues instead of generating (default 2 MiB). */
  DOCS_ON_DEMAND_BYTES?: string;
  /** Optional: source bytes above which docs generation is refused (default 3 MiB). */
  DOCS_MAX_SOURCE_BYTES?: string;
  SESSION_SECRET?: string;
  GITHUB_CLIENT_ID?: string;
  GITHUB_CLIENT_SECRET?: string;
}

/** A docs generation job on DOCS_QUEUE. */
export interface DocsJob {
  id: string;
  version: string;
}

/** What Hono's context is typed with. */
export interface AppEnv {
  Bindings: Env;
}

/** What the helpers need of an ExecutionContext (Hono's and the runtime's differ). */
export interface Ctx {
  waitUntil(promise: Promise<unknown>): void;
}

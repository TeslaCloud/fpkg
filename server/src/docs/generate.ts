// Docs generation: archive from R2 -> source members -> fdoc -> DOCS at docs/<FDOC_VERSION>/<id>/<version>/.
// Used by the queue consumer and, on a miss, by the docs host.

import type { DocsJob, Env } from "../env";
import { sha256Hex } from "../lib/auth";
import { CACHE_CONTROL } from "../lib/cache";
import { getVersionRow } from "../lib/db";
import { trimSlash, type StoredMetadata } from "../lib/metadata";
import { now } from "../lib/http";
import { Unzip, UnzipInflate } from "fflate";
import { FdocError, fdocGenerate, fdocGenerateBytes, type FdocFile, type FdocInput, type FdocPage } from "./fdoc";

/** The members fdoc reads. */
export const SOURCE_EXTENSIONS = [".lua", ".lumen", ".nl", ".nlua"];

/**
 * Source bytes above which a request enqueues a job and answers 202 instead of generating:
 * a Worker that runs out of CPU or memory dies without answering. `DOCS_ON_DEMAND_BYTES` overrides.
 */
export const ON_DEMAND_SOURCE_LIMIT = 2 * 1024 * 1024;

/**
 * Source bytes above which fdoc is not run and a failure is recorded: its wasm memory grows
 * ~18 MiB per MiB of source, within 128 MiB. `DOCS_MAX_SOURCE_BYTES` overrides.
 */
export const MAX_SOURCE_BYTES = 3 * 1024 * 1024;

/** A `queued` job older than this is presumed lost and may be enqueued again. */
export const QUEUE_STALE_MS = 15 * 60 * 1000;

export function maxSourceBytes(env: Env): number {
  return Number(env.DOCS_MAX_SOURCE_BYTES) || MAX_SOURCE_BYTES;
}

export function onDemandBytes(env: Env): number {
  return Math.min(Number(env.DOCS_ON_DEMAND_BYTES) || ON_DEMAND_SOURCE_LIMIT, maxSourceBytes(env));
}

/** R2 writes in flight at once while storing pages. */
const WRITE_CONCURRENCY = 8;

export function docsPrefix(env: Env, id: string, version: string): string {
  return `docs/${env.FDOC_VERSION}/${id}/${version}/`;
}

export function contentType(path: string): string {
  const ext = path.slice(path.lastIndexOf(".") + 1).toLowerCase();
  return (
    {
      html: "text/html; charset=utf-8",
      css: "text/css; charset=utf-8",
      js: "text/javascript; charset=utf-8",
      json: "application/json; charset=utf-8",
      svg: "image/svg+xml",
      txt: "text/plain; charset=utf-8",
    } as Record<string, string>
  )[ext] ?? "application/octet-stream";
}

export async function pathHash(path: string): Promise<string> {
  return (await sha256Hex(path)).slice(0, 16);
}

let themeHash: Promise<string> | null = null;

/**
 * The CSP hash of fdoc's one inline script (the theme switch), taken once per isolate from
 * a trivial generation, so no other inline script can run.
 */
export function fdocThemeScriptHash(): Promise<string> {
  themeHash ??= (async () => {
    const out = await fdocGenerate({ title: "csp", files: [{ path: "a.lua", text: "" }] });
    const index = out.pages.find((p) => p.path === "index.html")?.content ?? "";
    const scripts = [...index.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]!);
    if (scripts.length !== 1) throw new FdocError(`expected one inline script in fdoc's pages, found ${scripts.length}`);
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(scripts[0]!));
    return `'sha256-${btoa(String.fromCharCode(...new Uint8Array(digest)))}'`;
  })();
  themeHash.catch(() => (themeHash = null));
  return themeHash;
}

/** A generated page's CSP: same-origin scripts and styles, plus fdoc's theme switch by hash. */
export async function docsCsp(): Promise<string> {
  return [
    "default-src 'self'",
    `script-src 'self' ${await fdocThemeScriptHash()}`,
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data: https:",
    "font-src 'self' data:",
    "connect-src 'self'",
    "object-src 'none'",
    "base-uri 'none'",
    "form-action 'self'",
    "frame-ancestors 'none'",
  ].join("; ");
}

export type Generation =
  | { status: "ok"; pages: FdocPage[]; sha256: string; warnings: string[] }
  | { status: "missing" }
  | { status: "too_large"; bytes: number };

/**
 * Generates a version's docs unless its source exceeds `maxSourceBytes`. Throws FdocError when
 * fdoc refuses the package (deterministic), other errors for retryable infrastructure failures.
 */
export async function generateDocs(env: Env, id: string, version: string, maxSourceBytes: number): Promise<Generation> {
  const row = await getVersionRow(env.DB, id, version);
  if (!row) return { status: "missing" };
  const meta = JSON.parse(row.metadata) as StoredMetadata;
  const input = await encodeInput(env, id, version, meta.name, maxSourceBytes);
  if ("tooLarge" in input) return { status: "too_large", bytes: input.tooLarge };
  const out = await fdocGenerateBytes(input.bytes);
  return { status: "ok", pages: out.pages, sha256: row.sha256, warnings: out.warnings };
}

/** The fdoc input as UTF-8 JSON; separate so the sources and JSON string are garbage before fdoc runs. */
async function encodeInput(env: Env, id: string, version: string, title: string, max: number): Promise<{ bytes: Uint8Array } | { tooLarge: number }> {
  const sources = await loadSources(env, id, version, max);
  if ("tooLarge" in sources) return sources;
  const json = JSON.stringify({
    title,
    version,
    source_url: `${trimSlash(env.SITE_URL)}/packages/${id}/${version}/source/`,
    files: sources.files,
  } satisfies FdocInput);
  return { bytes: new TextEncoder().encode(json) };
}

function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) if (a[i] !== b[i]) return a[i]! - b[i]!;
  return a.length - b.length;
}

/**
 * An archive's source members relative to the package root, sorted by UTF-8 bytes like the CLI.
 * Streamed from R2 so only the inflated sources are held; reading stops once they pass `max`.
 */
export async function loadSources(env: Env, id: string, version: string, max: number): Promise<{ files: FdocFile[] } | { tooLarge: number }> {
  const prefix = `${id}/`;
  const obj = await env.ARCHIVES.get(`archives/${id}/${version}.zip`);
  if (!obj) throw new Error(`the archive of ${id} ${version} is missing from R2`);
  const encoder = new TextEncoder();
  const decoder = new TextDecoder("utf-8"); // replacement, not fatal
  const files: { key: Uint8Array; file: FdocFile }[] = [];
  let total = 0;
  let tooLarge = false;
  let failure: Error | null = null;
  const unzip = new Unzip((file) => {
    if (!file.name.startsWith(prefix) || !SOURCE_EXTENSIONS.some((x) => file.name.endsWith(x))) return; // not started: skipped
    if (tooLarge || failure) return;
    if (total + (file.originalSize ?? 0) > max) {
      tooLarge = true;
      total += file.originalSize ?? 0;
      return;
    }
    const chunks: Uint8Array[] = [];
    file.ondata = (err, chunk, final) => {
      if (err) {
        failure = err;
        return;
      }
      total += chunk.length;
      if (total > max) {
        tooLarge = true;
        return;
      }
      chunks.push(chunk);
      if (final) {
        const path = file.name.slice(prefix.length);
        files.push({ key: encoder.encode(path), file: { path, text: decoder.decode(concatBytes(chunks)) } });
      }
    };
    file.start();
  });
  unzip.register(UnzipInflate);
  const reader = obj.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    unzip.push(value);
    if (tooLarge || failure) {
      await reader.cancel();
      break;
    }
  }
  if (failure) throw new Error(`cannot unzip ${id} ${version}: ${(failure as Error).message}`);
  if (tooLarge) return { tooLarge: total };
  unzip.push(new Uint8Array(0), true);
  files.sort((a, b) => compareBytes(a.key, b.key));
  return { files: files.map((f) => f.file) };
}

function concatBytes(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 1) return chunks[0]!;
  const out = new Uint8Array(chunks.reduce((n, c) => n + c.length, 0));
  let o = 0;
  for (const c of chunks) {
    out.set(c, o);
    o += c.length;
  }
  return out;
}

/**
 * Enqueues a docs job unless one for this FDOC_VERSION is younger than QUEUE_STALE_MS; one
 * conditional UPDATE decides, so concurrent requests enqueue once. Returns whether it enqueued.
 */
export async function enqueueDocs(env: Env, id: string, version: string): Promise<boolean> {
  const at = now();
  const stale = now(new Date(Date.now() - QUEUE_STALE_MS));
  const r = await env.DB.prepare(
    `UPDATE versions SET docs_status = 'queued', docs_version = ?1, docs_queued_at = ?2, docs_error = NULL
      WHERE package_id = ?3 AND version = ?4
        AND NOT (docs_status = 'queued' AND docs_version = ?1 AND docs_queued_at > ?5)
        AND NOT (docs_status IN ('ready', 'failed') AND docs_version = ?1)`,
  )
    .bind(env.FDOC_VERSION, at, id, version, stale)
    .run();
  if ((r.meta.changes ?? 0) === 0) return false;
  await env.DOCS_QUEUE.send({ id, version });
  return true;
}

/** Writes the pages to DOCS and marks the version's docs ready for this FDOC_VERSION. */
export async function storeDocs(env: Env, id: string, version: string, sha256: string, pages: FdocPage[]): Promise<void> {
  const prefix = docsPrefix(env, id, version);
  const csp = await docsCsp();
  let next = 0;
  const worker = async () => {
    while (next < pages.length) {
      const page = pages[next++]!;
      await env.DOCS.put(prefix + page.path, page.content, {
        httpMetadata: { contentType: contentType(page.path), cacheControl: CACHE_CONTROL.docs },
        customMetadata: { sha256, csp: page.path.endsWith(".html") ? csp : "", fdoc: env.FDOC_VERSION },
      });
    }
  };
  await Promise.all(Array.from({ length: Math.min(WRITE_CONCURRENCY, pages.length) }, worker));
  await env.DB.prepare("UPDATE versions SET docs_status = 'ready', docs_version = ?, docs_error = NULL WHERE package_id = ? AND version = ?")
    .bind(env.FDOC_VERSION, id, version)
    .run();
}

/**
 * `docs_error` prefix for fdoc's own (deterministic) failures, not retried until a new
 * FDOC_VERSION. Other failures start with `error: ` and are retried on demand.
 */
export const FDOC_FAILURE = "fdoc: ";

export async function markFailed(env: Env, id: string, version: string, error: string, deterministic: boolean): Promise<void> {
  console.error(`docs for ${id} ${version} (fdoc ${env.FDOC_VERSION}) failed: ${error}`);
  const text = `${deterministic ? FDOC_FAILURE : "error: "}${error} (${now()})`.slice(0, 4000);
  await env.DB.prepare("UPDATE versions SET docs_status = 'failed', docs_version = ?, docs_error = ? WHERE package_id = ? AND version = ?")
    .bind(env.FDOC_VERSION, text, id, version)
    .run();
}

export async function docsReady(env: Env, id: string, version: string): Promise<boolean> {
  const r = await env.DB.prepare("SELECT docs_status, docs_version FROM versions WHERE package_id = ? AND version = ?")
    .bind(id, version)
    .first<{ docs_status: string; docs_version: string | null }>();
  return r?.docs_status === "ready" && r.docs_version === env.FDOC_VERSION;
}

/** One queue job: generate unless already done for this FDOC_VERSION. */
export async function runDocsJob(env: Env, job: DocsJob): Promise<void> {
  if (await docsReady(env, job.id, job.version)) return;
  try {
    const max = maxSourceBytes(env);
    const gen = await generateDocs(env, job.id, job.version, max);
    if (gen.status === "missing") return; // the version is gone; nothing to do
    if (gen.status === "too_large") {
      const mib = (n: number) => `${(n / 1048576).toFixed(1)} MiB`;
      throw new FdocError(`the package has more than ${mib(max)} of Lua, Lumen and NL source (${mib(gen.bytes)} seen), more than docs generation in the Worker can hold in memory`);
    }
    for (const w of gen.warnings) console.warn(`fdoc: ${job.id} ${job.version}: ${w}`);
    await storeDocs(env, job.id, job.version, gen.sha256, gen.pages);
  } catch (e) {
    if (e instanceof FdocError) {
      await markFailed(env, job.id, job.version, e.message, true);
      return;
    }
    throw e;
  }
}

export const MAX_ATTEMPTS = 3;

/** The queue consumer. Infrastructure errors retry; the last attempt records the failure. */
export async function handleDocsBatch(batch: MessageBatch<DocsJob>, env: Env): Promise<void> {
  for (const msg of batch.messages) {
    const job = msg.body;
    if (!job || typeof job.id !== "string" || typeof job.version !== "string") {
      console.error("dropping a malformed docs job", job);
      msg.ack();
      continue;
    }
    try {
      await runDocsJob(env, job);
      msg.ack();
    } catch (e) {
      const message = e instanceof Error ? e.message : String(e);
      if (msg.attempts >= MAX_ATTEMPTS) {
        await markFailed(env, job.id, job.version, message, false).catch(() => {});
        msg.ack();
      } else {
        console.error(`docs job ${job.id} ${job.version} failed (attempt ${msg.attempts}), retrying: ${message}`);
        msg.retry({ delaySeconds: 30 * msg.attempts });
      }
    }
  }
}

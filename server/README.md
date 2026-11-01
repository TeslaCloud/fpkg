# fluxpkg.dev server

The registry (`fluxpkg.dev`) and the documentation host (`docs.fluxpkg.dev`)
as one Cloudflare Worker with D1, R2, a queue, the Cache API and `fdoc`
compiled to WebAssembly.

```
wrangler.toml          routes, bindings, limits, vars (secrets are set with wrangler, not stored)
migrations/            D1 schema: 0001_init.sql, 0002_docs_state.sql, 0003_docs_queue.sql
src/index.ts           fetch (host dispatch) and queue entry points
src/api/               /api/v1: reads, publish (publish.ts), yank, owners, search, me
src/docs/              docs host (host.ts), generation and queue consumer (generate.ts), wasm glue (fdoc.ts)
src/web/               website: index, search, package pages, archive sources, GitHub login, tokens
src/lib/               version grammar, zip validation, packagespec literal scan, cache, auth, D1 queries, multipart
scripts/build-fdoc.sh  builds wasm/fdoc.wasm from ../../fdoc (or $FDOC_DIR) and writes .fdoc-version
scripts/conformance.sh wrangler dev + local D1/R2/queue, seeds a token, runs the Rust suite and smoke checks
scripts/smoke.mjs      HTTP smoke checks against any running registry
test/                  vitest in the Workers runtime (unit tests and an end-to-end flow)
```

## One-time account setup

Done once by hand. Everything below needs `npx wrangler login` (or
`CLOUDFLARE_API_TOKEN`) first.

1. **Zone.** Add `fluxpkg.dev` to the Cloudflare account and point the
   registrar at Cloudflare's name servers. Create proxied DNS records for
   `fluxpkg.dev` and `docs.fluxpkg.dev` (an `AAAA` to `100::` is enough; the
   Worker routes answer before any origin).
2. **Workers Paid plan** (Workers & Pages -> Plans). Needed for the 30 s CPU
   limit (`limits.cpu_ms`), queue consumers and 100 MB request bodies.
3. **D1:**
   ```sh
   npx wrangler d1 create fluxpkg
   ```
   Put the printed `database_id` into `wrangler.toml`. (The deploy workflow
   can instead substitute a `D1_DATABASE_ID` Actions secret for an all-zero
   placeholder id.)
4. **R2:**
   ```sh
   npx wrangler r2 bucket create fluxpkg-archives
   npx wrangler r2 bucket create fluxpkg-docs
   # Old FDOC_VERSION prefixes are garbage collected after 30 days:
   npx wrangler r2 bucket lifecycle add fluxpkg-docs expire-docs docs/ --expire-days 30
   ```
   The rule expires current docs too after 30 days; the docs host notices the
   missing `index.html` and regenerates on the next request, so this is safe.
5. **Queue:**
   ```sh
   npx wrangler queues create fluxpkg-docs
   ```
   The consumer is attached by `wrangler deploy` from `wrangler.toml`.
6. **GitHub OAuth app** (GitHub -> Settings -> Developer settings -> OAuth
   Apps): homepage `https://fluxpkg.dev`, callback
   `https://fluxpkg.dev/auth/github/callback`. No scopes are requested.
7. **Secrets:**
   ```sh
   openssl rand -hex 32 | npx wrangler secret put SESSION_SECRET
   npx wrangler secret put GITHUB_CLIENT_ID
   npx wrangler secret put GITHUB_CLIENT_SECRET
   ```
   Rotating `SESSION_SECRET` logs everyone out of the website; API tokens are
   unaffected.
8. **Admins:** set `ADMIN_LOGINS` in `wrangler.toml` (comma or space
   separated GitHub logins).
9. **GitHub Actions secrets** for `.github/workflows/deploy.yml`:
   `CLOUDFLARE_API_TOKEN` (permissions: Workers Scripts Edit, Workers Routes
   Edit, D1 Edit, Workers R2 Storage Edit, Queues Edit, Zone Cache Purge),
   `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_ZONE_ID`, and optionally
   `D1_DATABASE_ID`. Every deploy is gated on CI: after a CI run on `main`
   (workflow_run), or by running the CI workflow itself first
   (`workflow_call`) for manual and `fdoc-updated` deploys. The workflow uses
   a `production` environment, which GitHub creates on first use (add
   protection rules there if wanted).
10. **Optional, "change fdoc and push":** deploys run after CI on `main` of
    this repository. To redeploy when `Meow/fdoc` changes, add a workflow
    there that runs
    `gh api repos/<owner>/fpkg/dispatches -f event_type=fdoc-updated`
    with a token allowed to dispatch to this repository.

## Local development

No Cloudflare login is needed for any of this; `--local` runs D1, R2, the
queue and the cache in workerd on your machine.

```sh
npm ci
scripts/build-fdoc.sh          # wasm/fdoc.wasm and .fdoc-version, from ../../fdoc
cp .dev.vars.example .dev.vars # SESSION_SECRET, and a local OAuth app if you want to log in
npm run migrate:local
npm run dev                    # http://127.0.0.1:8788/, docs at http://127.0.0.1:8788/_docs/
npm run typecheck
npm test
```

`wrangler dev` rewrites every request's URL and `Host` to the first route
(`fluxpkg.dev`), so locally the docs host lives under the path of
`DOCS_URL` instead (`--var DOCS_URL:http://127.0.0.1:8788/_docs`). To get
an API token without GitHub, insert one:

```sh
TOKEN=fpk_$(node -e 'process.stdout.write(require("crypto").randomBytes(32).toString("base64url"))')
HASH=$(printf %s "$TOKEN" | shasum -a 256 | cut -d' ' -f1)
npx wrangler d1 execute DB --local --command "INSERT INTO users (github_id, login, created_at) VALUES (1, 'me', '2026-01-01T00:00:00Z');
  INSERT INTO tokens (user_id, name, hash, created_at) VALUES (1, 'dev', '$HASH', '2026-01-01T00:00:00Z');"
echo "$TOKEN"
```

## Conformance

```sh
scripts/conformance.sh             # everything below, then stop and clean up
scripts/conformance.sh --no-cargo  # skip the Rust suite (HTTP smoke checks only)
scripts/conformance.sh --serve     # start, seed, print the exports, wait for Ctrl-C
PORT=9000 scripts/conformance.sh   # default port 8790
```

It builds `wasm/fdoc.wasm` if missing, starts `wrangler dev --local` on
`127.0.0.1:$PORT` with a fresh temporary state, inserts the users
`conformance` (with a token) and `conformance2` into the local D1, exports
`FPKG_TEST_REGISTRY` and `FPKG_TOKEN`, runs `cargo test -p fpkg-core --test
conformance` from the repository root when that test exists, and then
`scripts/smoke.mjs` (publish, 409/422, ETag/304, yank, owners, a generated
docs page). Everything it started is stopped and removed on exit.

## API

Paths are under `/api/v1`. Every response carries `X-Fpkg-Api: 1`; errors are
`{ "error": { "code", "message", "details"? } }`.

| Endpoint | Success | Notes |
| --- | --- | --- |
| `GET /packages/{id}` | `200` package document | `404 not_found` for an unknown id |
| `GET /packages/{id}/versions/{v}` | `200` version metadata | a non-canonical `{v}` (`1.0`, `07.0.9`) answers `301` to the canonical URL |
| `GET .../archive` | `200` zip | `Content-Disposition: attachment; filename="<id>-<v>.zip"`, `Content-Length`; `301` like the above |
| `PUT /packages/{id}/versions/{v}` | `201` version metadata with `dist`, `Location` | multipart `metadata` + `archive`; `{v}` must be canonical (`X.Y.Z[-suffix]`) |
| `POST .../yank`, `.../unyank` | `200` version metadata | idempotent; no revision bump when nothing changes; any spelling of `{v}` |
| `GET /packages/{id}/owners` | `200 { "id", "owners": [login...] }` | `ETag "<id>-<revision>"`, `Cache-Control: no-cache`, `304` |
| `PUT /packages/{id}/owners/{login}` | `200 { "id", "owners" }` | idempotent; `404 user_not_found` unless the user has logged in once |
| `DELETE /packages/{id}/owners/{login}` | `200 { "id", "owners" }` | `409 last_owner`; `404` if not an owner |
| `GET /search?q=&page=&per_page=` | `200 { "query", "total", "page", "per_page", "packages": [{ "id", "name", "version", "summary", "updated_at", "downloads" }] }` | `page` from 1, `per_page` 1 to 100 (default 20); empty `q` lists everything |
| `GET /me` | `200 { "login", "token_name" }` | bearer tokens only |

Error codes: `bad_request` (400: not multipart, unparsable body, bad query
parameters), `unauthorized` (401, with `WWW-Authenticate`), `forbidden`
(403: not an owner), `not_found` / `user_not_found` (404),
`method_not_allowed` (405, with `Allow`), `version_exists` (409),
`publish_in_progress` (409, with `Retry-After`), `last_owner` (409),
`payload_too_large` (413), `validation_failed` (422, with `details`, an
array of human-readable strings, each prefixed with where the problem is:
`metadata.dependencies[0].constraint: ...`, `archive: member ... has ...`),
`rate_limited` (429, with `Retry-After`), `internal` (500).

## Behaviour details

* **Docs host locally.** Production dispatches on `Host` (the hostname of
  `DOCS_URL`). When `DOCS_URL` has a path (`http://127.0.0.1:8788/_docs`),
  requests under that path go to the docs host on any host instead, because
  `wrangler dev` rewrites every request to the first route's host.
* **Absolute URLs** (`dist.url`, `docs`, OAuth callback, source links in the
  docs) come from the `SITE_URL` and `DOCS_URL` vars.
* **Stored metadata.** `versions.metadata` holds the publisher's metadata
  normalised to the members `id` ... `dependencies`; `dist`, `docs`,
  `yanked`, `published_at` and `published_by` are built from columns on
  every response. Unknown members are dropped; `allow_missing_dependencies`
  is honoured and not stored. Missing optional members become `null`, `[]`
  or `false`; a dependency without a constraint is stored as `*`.
* **Metadata validation:** `name` required (at most 128 characters),
  `website` must be an `http(s)` URL, length caps on every string and list,
  `serverside` and `clientside` not both true, no self-dependency, no
  duplicate dependency.
* **The literal scan** of `packagespec.lua` uses a small Lua/GLua tokenizer,
  so comments (`--`, `--[[ ]]`, `//`, `/* */`) and other strings are never
  mistaken for an assignment. It takes `<name>.version = <string literal>`
  and `<name>.name = ...` with any variable name and `'...'`, `"..."` or
  long-bracket strings. A computed value (`'1.' .. x`), two different
  literals (an `IS_DEVELOPMENT` branch) or no version literal is refused with
  a `422`; an absent `name` literal is accepted, a disagreeing one refused.
  The spec's version may be loose (`'1.0'`) as long as it canonicalises to
  the URL's; the URL itself must be canonical. `\xNN` and `\ddd` escapes are
  bytes decoded as UTF-8, so `'Caf\xc3\xa9'` is `Café`.
* **Zip validation.** Archives are at most 64 MiB, 10,000 members and
  256 MiB uncompressed, with each member's ratio under 100:1 (100:1 itself is
  refused). Every path is under `<id>/`, uses forward slashes, has no `.` or
  `..` components, drive letters or leading `/`, and has components of at
  most 255 UTF-8 bytes; `<id>/packagespec.lua` must exist. Refused:
  directory entries and any other non-regular entry (by the Unix file type
  in the external attributes, whatever host made it), hosts other than
  MS-DOS (0), Unix (3) and OS X (19), encrypted members, methods other than
  store and deflate, zip64 and multi-disk archives, duplicate names, a path
  that is both a file and a directory, and bytes outside members: members
  must tile the file from offset 0 to the central directory in order. Local
  headers must agree with the central directory, and every member's CRC-32
  and size are verified while it inflates. Names that differ only in case
  are distinct members.
* **Concurrent publishes.** The R2 write is conditional (`onlyIf
  etagDoesNotMatch: *`), so two publishes of one version cannot overwrite
  each other's bytes; the loser gets `409 publish_in_progress`. Identical
  bytes already there are reused. An object without a `versions` row older
  than 10 minutes is a leftover of a failed publish and is replaced. The D1
  batch creates a new package, inserts the version guarded by ownership (so
  two first publishes of one id by different users cannot both land) and
  bumps the revision only if that insert wrote a row; a new package starts
  at revision 1. R2 has no conditional delete, so a rollback deletes only an
  object with its own bytes and puts it back if a concurrent publish of
  identical bytes committed meanwhile; every commit also rewrites a missing
  object. Either way, a committed row keeps its archive.
* **Dependency check** queries in chunks of 90 ids (D1 binds at most 100
  parameters per statement); up to 256 dependencies are accepted. Each
  dependency needs a satisfying, non-yanked version unless
  `allow_missing_dependencies` is set.
* **Rate limits:** at most 120 publishes per user per hour (`429` with
  `Retry-After`), at most 50 active tokens per user. Reads are not limited in
  the Worker; use Cloudflare's WAF rate limiting rules if needed.
* **Body limits:** `413` when `Content-Length` exceeds 64 MiB plus 2 MiB of
  multipart overhead, when the archive part exceeds 64 MiB, or when the
  metadata part exceeds 1 MiB. The body is read once and the parts are views
  into it, so a maximal archive is resident once in the 128 MiB Worker.
* **Admins** (`ADMIN_LOGINS`) can yank and unyank any version and add owners
  to any package; removing owners and publishing need ownership. Admins are
  identified by GitHub login, and logins can be renamed and later taken by
  someone else, so keep `ADMIN_LOGINS` current when an admin renames.
* **Pre-releases:** a suffixed version satisfies a constraint only when one
  of its requirements names a pre-release of the same X.Y.Z (`>= 2.0.0-beta`
  admits `2.0.0-rc1`; `~> 1.2`, `< 2.0` and `*` never admit `2.0.0-beta`);
  the publish dependency check uses that rule. `latest`, the version search
  shows, and the docs host's `/<id>/` redirect are the newest non-yanked
  release, or the newest non-yanked pre-release when there is no release.
* **Caching.** Archives and version metadata: `public, max-age=31536000,
  immutable`, ETags `"<sha256>"` and `"<sha256>-0"`/`"<sha256>-1"` (the
  `yanked` flag). Package documents: `max-age=60, stale-while-revalidate=600`,
  ETag `"<id>-<revision>"`. Search and website pages: `max-age=60` (private
  when logged in). Docs pages: `max-age=86400, stale-while-revalidate=604800`,
  ETag `"<FDOC_VERSION>-<sha256>-<path hash>"`. Owners are not cached
  (`no-cache` with the package ETag). Cache keys are the URL alone; docs
  keys also carry `FDOC_VERSION`, so a new fdoc never serves old pages.
  Mutations (publish, yank, owners) delete the package document's entry in
  their colo; a yank also stores the new version body there, which reaches
  every reader because each version has one canonical URL.
* **`docs_status` per fdoc version.** `docs_version` is the `FDOC_VERSION`
  the status refers to; `docs_error` and `docs_queued_at` complete it.
  Statuses: `pending` (never enqueued), `queued`, `ready`, `failed`. Docs are
  current when `docs_status = 'ready' AND docs_version = FDOC_VERSION`. A
  failure of fdoc itself (including the size cap below) is recorded with an
  `fdoc: ` prefix and served as a `500` page without retrying until the next
  fdoc; other failures (R2, D1) are retried by the queue three times and
  then on the next request.
* **Memory and size caps.** fdoc's wasm memory grows by about 18 MiB per MiB
  of source (all of flux-ce, 4.3 MiB of source, peaks at 78 MiB), and the
  isolate also holds the input and output as JS strings, all within
  128 MiB. So packages with more than 3 MiB of `.lua`/`.lumen`/`.nl`/`.nlua`
  source (`DOCS_MAX_SOURCE_BYTES` overrides) get no docs, recorded as an fdoc
  failure. Sources are streamed out of the R2 object with fflate's streaming
  `Unzip`, so the archive is never resident and reading stops once the cap is
  passed. Member paths are sorted by their UTF-8 bytes, as the CLI does.
* **On-demand generation** inside a request happens when the source is at
  most 2 MiB (`DOCS_ON_DEMAND_BYTES` overrides, never above the queue cap).
  Above that the request enqueues a job and answers `202` with
  `Retry-After: 30`. One conditional UPDATE deduplicates enqueueing: a
  version is enqueued again only after 15 minutes (a job presumed lost), and
  never when its docs are already ready or failed for this fdoc. Publishes
  enqueue the same way.
* **Docs 404s** are cached for 60 s (keyed with `FDOC_VERSION`), and an
  isolate remembers the page list of its last 64 on-demand generations, so
  requests for missing pages do not each cost a generation.
* **The wasm module** is imported by `src/docs/fdoc.ts` and compiled by
  `wrangler.toml`'s `CompiledWasm` rule. Each generation gets a fresh
  instance, so memory is returned and a trap cannot poison later calls;
  `fdoc_last_panic` reports the panic message of a trap (Rust panics abort on
  `wasm32-unknown-unknown`). fdoc errors (no source files, an unparsable
  `plugin.yml`) come back as `{ "error": ... }`; a null or empty output is
  also an fdoc failure.
* **Docs pages' CSP:** fdoc pages carry one inline script (the theme
  switch). Only that script is allowed, by its SHA-256 hash, which the
  Worker takes from what the bundled fdoc emits for a trivial input (once
  per isolate).
* **Source route** (`/packages/<id>/<version>/source/<path>`, path relative
  to the package root; a non-canonical version answers `301`, a malformed
  `%` escape `404`): reads the member with ranged R2 reads (end record,
  central directory, local header, data) and serves it as `text/plain` with
  `Content-Security-Policy: sandbox`, immutable, `ETag "<sha256>-<path
  hash>"`. Members over 16 MiB get a `413`.
* **Website:** no JavaScript; CSP `default-src 'none'; style-src 'self'`.
  Forms carry a CSRF token (an HMAC of the session cookie) and cross-origin
  `Origin` headers are refused. The OAuth `state` is signed and bound to a
  nonce cookie scoped to `/auth`. On login, any other user row holding the
  same GitHub login is renamed to `<login>~<github_id>`. Package descriptions
  are shown as plain text. Download counts are summed over all days.

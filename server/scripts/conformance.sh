#!/usr/bin/env bash
# Runs the registry locally on a fresh D1 with a test token, then the fpkg-core
# conformance test and scripts/smoke.mjs. Flags: --no-cargo, --serve; PORT (default 8790).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
root="$(cd "$here/.." && pwd)"
port="${PORT:-8790}"
serve=0
cargo=1
for arg in "$@"; do
  case "$arg" in
    --serve) serve=1 ;;
    --no-cargo) cargo=0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

state="$(mktemp -d "${TMPDIR:-/tmp}/fpkg-conformance.XXXXXX")"
log="$state/wrangler.log"
pid=""

cleanup() {
  status=$?
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    # wrangler runs in its own process group (set -m below); stop all of it.
    kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    kill -KILL -- "-$pid" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$log" ]; then
    echo "--- wrangler log (last 40 lines) ---" >&2
    tail -n 40 "$log" >&2 || true
  fi
  rm -rf "$state"
  exit "$status"
}
trap cleanup EXIT INT TERM

cd "$here"
if [ ! -f wasm/fdoc.wasm ] || [ ! -f .fdoc-version ]; then
  scripts/build-fdoc.sh >/dev/null
fi
fdoc_version="$(cat .fdoc-version)"

if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
  echo "port $port is in use; set PORT" >&2
  exit 1
fi

npx wrangler d1 migrations apply DB --local --persist-to "$state" >/dev/null

token="$(node -e 'process.stdout.write("fpk_" + require("crypto").randomBytes(32).toString("base64url"))')"
hash="$(node -e 'process.stdout.write(require("crypto").createHash("sha256").update(process.argv[1]).digest("hex"))' "$token")"
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
npx wrangler d1 execute DB --local --persist-to "$state" --command "
  INSERT INTO users (github_id, login, created_at) VALUES (1, 'conformance', '$now');
  INSERT INTO tokens (user_id, name, hash, created_at) VALUES (1, 'conformance', '$hash', '$now');
  INSERT INTO users (github_id, login, created_at) VALUES (2, 'conformance2', '$now');" >/dev/null

set -m
npx wrangler dev --local --ip 127.0.0.1 --port "$port" --persist-to "$state" \
  --var "FDOC_VERSION:$fdoc_version" \
  --var "SITE_URL:http://127.0.0.1:$port" \
  --var "DOCS_URL:http://127.0.0.1:$port/_docs" \
  --var "SESSION_SECRET:conformance-$(node -e 'process.stdout.write(require("crypto").randomBytes(16).toString("hex"))')" \
  --var "ADMIN_LOGINS:conformance-admin" \
  >"$log" 2>&1 &
pid=$!
set +m

for _ in $(seq 1 120); do
  if curl -fs -o /dev/null "http://127.0.0.1:$port/api/v1/search" 2>/dev/null; then break; fi
  if ! kill -0 "$pid" 2>/dev/null; then echo "wrangler dev exited" >&2; exit 1; fi
  sleep 0.5
done
curl -fsS -o /dev/null "http://127.0.0.1:$port/api/v1/search"

export FPKG_TEST_REGISTRY="http://127.0.0.1:$port/"
export FPKG_TOKEN="$token"
echo "export FPKG_TEST_REGISTRY=$FPKG_TEST_REGISTRY"
echo "export FPKG_TOKEN=$FPKG_TOKEN"
echo "# docs: http://127.0.0.1:$port/_docs/   (a second user, conformance2, exists for owner tests)"

if [ "$serve" -eq 1 ]; then
  echo "serving; Ctrl-C to stop"
  wait "$pid"
  exit 0
fi

has_conformance=0
if [ -f "$root/crates/fpkg-core/Cargo.toml" ]; then
  if [ -e "$root/crates/fpkg-core/tests/conformance.rs" ] || [ -e "$root/crates/fpkg-core/tests/conformance/main.rs" ] \
    || grep -Eq '^\s*name\s*=\s*"conformance"' "$root/crates/fpkg-core/Cargo.toml"; then
    has_conformance=1
  fi
fi
if [ "$cargo" -eq 0 ]; then
  echo "== --no-cargo: skipping the fpkg-core conformance test"
elif [ "$has_conformance" -eq 1 ]; then
  echo "== cargo test -p fpkg-core --test conformance (against $FPKG_TEST_REGISTRY)"
  (cd "$root" && cargo test -p fpkg-core --test conformance)
else
  echo "== no fpkg-core conformance test yet; HTTP smoke checks only"
fi

echo "== smoke checks"
node scripts/smoke.mjs "$FPKG_TEST_REGISTRY" "$FPKG_TOKEN" "http://127.0.0.1:$port/_docs"

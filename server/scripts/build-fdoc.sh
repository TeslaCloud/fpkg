#!/usr/bin/env bash
# Builds fdoc ($FDOC_DIR, default ../../fdoc) to wasm/fdoc.wasm and writes
# .fdoc-version (`<crate version>+<short hash>[-dirty]`), which it also prints.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fdoc="${FDOC_DIR:-$here/../../fdoc}"
fdoc="$(cd "$fdoc" && pwd)"

if ! rustup target list --installed 2>/dev/null | grep -qx wasm32-unknown-unknown; then
  rustup target add wasm32-unknown-unknown
fi

# --lib: the binary target would build to the same fdoc.wasm name.
cargo build --manifest-path "$fdoc/Cargo.toml" --release --target wasm32-unknown-unknown --lib >&2

mkdir -p "$here/wasm"
cp "$fdoc/target/wasm32-unknown-unknown/release/fdoc.wasm" "$here/wasm/fdoc.wasm"

crate_version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' "$fdoc/Cargo.toml" | head -n1)"
hash="$(git -C "$fdoc" rev-parse --short=7 HEAD 2>/dev/null || echo unknown)"
if [ -n "$(git -C "$fdoc" status --porcelain 2>/dev/null)" ]; then hash="$hash-dirty"; fi
version="$crate_version+$hash"
printf '%s\n' "$version" > "$here/.fdoc-version"

# The Worker loads the module without any imports; refuse one that needs some.
node -e '
  const m = new WebAssembly.Module(require("fs").readFileSync(process.argv[1]));
  const imports = WebAssembly.Module.imports(m);
  const exports = new Set(WebAssembly.Module.exports(m).map((e) => e.name));
  const want = ["memory", "fdoc_alloc", "fdoc_free", "fdoc_generate", "fdoc_version"];
  if (imports.length) { console.error("fdoc.wasm has imports:", JSON.stringify(imports)); process.exit(1); }
  for (const w of want) if (!exports.has(w)) { console.error("fdoc.wasm lacks the export", w); process.exit(1); }
' "$here/wasm/fdoc.wasm"

echo "$version"

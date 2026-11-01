#!/bin/sh
# Prepares vendor/luajit-src: the nl fork of LuaJIT as a luajit-src crate,
# which the workspace's [patch.crates-io] points mlua at.
#
#   scripts/prepare-nl.sh            uses ../nl
#   NL_DIR=/path/to/nl scripts/prepare-nl.sh
set -eu
HERE=$(cd "$(dirname "$0")/.." && pwd)
NL_DIR=${NL_DIR:-$HERE/../nl}
[ -f "$NL_DIR/tests/vm/support/prepare_luajit_src.sh" ] ||
  { echo "prepare-nl: $NL_DIR is not the nl repository (set NL_DIR)" >&2; exit 1; }
exec "$NL_DIR/tests/vm/support/prepare_luajit_src.sh" "$HERE/vendor/luajit-src"

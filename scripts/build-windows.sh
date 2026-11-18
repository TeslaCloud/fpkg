#!/bin/sh
# Cross-compiles fpkg for 64-bit Windows from Linux, with MinGW-w64:
# target/x86_64-pc-windows-gnu/release/fpkg.exe, which needs no DLLs but the
# system's.
#
#   scripts/prepare-nl.sh
#   scripts/build-windows.sh
#
# Needs x86_64-w64-mingw32-gcc (Debian and Ubuntu: gcc-mingw-w64-x86-64) and
# the Rust target (rustup target add x86_64-pc-windows-gnu).
#
# The VM embeds its runtime library as bytecode, which its own interpreter
# compiles during the build. The interpreter of a Windows build does not run
# here, so the fork is built for this machine first and passed to the build as
# NL_HOST_LUAJIT.
set -eu
HERE=$(cd "$(dirname "$0")/.." && pwd)
TARGET=x86_64-pc-windows-gnu
SRC=$HERE/vendor/luajit-src
[ -f "$SRC/luajit2/src/Makefile" ] ||
  { echo "build-windows: vendor/luajit-src is missing, run scripts/prepare-nl.sh first" >&2; exit 1; }
command -v x86_64-w64-mingw32-gcc >/dev/null ||
  { echo "build-windows: x86_64-w64-mingw32-gcc is not installed" >&2; exit 1; }

HOST=$HERE/target/nl-host-luajit
rm -rf "$HOST"
mkdir -p "$HOST"
cp -R "$SRC/luajit2/." "$HOST/"
cp "$SRC/luajit_relver.txt" "$HOST/.relver"
make -C "$HOST/src" -j"$(nproc)" NL_RUNTIME_DIR="$SRC/nl-runtime" >"$HOST/build.log" 2>&1 ||
  { tail -n 40 "$HOST/build.log" >&2; exit 1; }

cd "$HERE"
NL_HOST_LUAJIT=$HOST/src/luajit CARGO_PROFILE_RELEASE_STRIP=symbols \
  cargo build --release --locked -p fpkg --target "$TARGET"
echo "$HERE/target/$TARGET/release/fpkg.exe"

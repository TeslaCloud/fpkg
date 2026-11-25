#!/bin/sh
# Installs fpkg on Linux (x86_64 or aarch64): downloads the archive of the
# latest release from GitHub, checks it against the release's SHA256SUMS and
# puts the fpkg binary into ~/.local/bin, or into the directory given.
#
#   curl -fsSL https://raw.githubusercontent.com/TeslaCloud/fpkg/master/install.sh | sh
#   sh install.sh /usr/local/bin
#
# FPKG_VERSION=X.Y.Z installs that version instead of the latest.
# FPKG_RELEASES is the releases page the archives come from
# (https://github.com/TeslaCloud/fpkg/releases).
set -eu

repo=TeslaCloud/fpkg
releases=${FPKG_RELEASES:-https://github.com/$repo/releases}
dest=${1:-$HOME/.local/bin}

fail() {
  echo "install.sh: $*" >&2
  exit 1
}

case $(uname -s) in
  Linux) ;;
  *) fail "this installs the Linux build; on $(uname -s), see https://github.com/$repo#installing-fpkg" ;;
esac
case $(uname -m) in
  x86_64 | amd64) arch=x86_64 ;;
  aarch64 | arm64) arch=aarch64 ;;
  *) fail "there is no release for $(uname -m); build fpkg from source (https://github.com/$repo#building)" ;;
esac

# fetch <url> <file>
if command -v curl > /dev/null 2>&1; then
  fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget > /dev/null 2>&1; then
  fetch() { wget -q -O "$2" "$1"; }
else
  fail "curl or wget is needed"
fi
if command -v sha256sum > /dev/null 2>&1; then
  check_sum() { sha256sum -c -; }
elif command -v shasum > /dev/null 2>&1; then
  check_sum() { shasum -a 256 -c -; }
else
  fail "sha256sum or shasum is needed to check the download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
trap 'exit 1' HUP INT TERM

version=${FPKG_VERSION:-}
if [ -z "$version" ]; then
  fetch "https://api.github.com/repos/$repo/releases/latest" "$tmp/latest.json" ||
    fail "could not find the latest release of fpkg (none is published yet, or GitHub cannot be reached); set FPKG_VERSION, or see https://github.com/$repo#installing-fpkg"
  version=$(sed -n 's/^ *"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' "$tmp/latest.json" | head -n 1)
  [ -n "$version" ] || fail "the latest release names no version"
fi
version=${version#v}

name=fpkg-$version-linux-$arch
echo "Downloading fpkg $version for Linux $arch"
fetch "$releases/download/v$version/$name.tar.gz" "$tmp/$name.tar.gz" ||
  fail "could not download $releases/download/v$version/$name.tar.gz"
fetch "$releases/download/v$version/SHA256SUMS" "$tmp/SHA256SUMS" ||
  fail "could not download $releases/download/v$version/SHA256SUMS"

grep " $name.tar.gz\$" "$tmp/SHA256SUMS" > "$tmp/sum" ||
  fail "SHA256SUMS of $version has no line for $name.tar.gz"
(cd "$tmp" && check_sum < sum > /dev/null) ||
  fail "$name.tar.gz does not match its SHA-256 in SHA256SUMS; nothing was installed"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
[ -f "$tmp/$name/fpkg" ] || fail "$name.tar.gz has no $name/fpkg"

mkdir -p "$dest"
cp "$tmp/$name/fpkg" "$dest/.fpkg.new"
chmod 755 "$dest/.fpkg.new"
mv -f "$dest/.fpkg.new" "$dest/fpkg"
echo "Installed $("$dest/fpkg" --version) to $dest/fpkg"

case :$PATH: in
  *":$dest:"*) ;;
  *)
    echo
    echo "$dest is not in your PATH. Add it in ~/.profile (or the file of your shell):"
    echo "  export PATH=\"$dest:\$PATH\""
    echo "or, in fish: fish_add_path $dest"
    ;;
esac

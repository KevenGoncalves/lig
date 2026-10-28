#!/bin/sh
# Install lig from a GitHub release:
#
#   curl -fsSL https://github.com/KevenGoncalves/lig/releases/latest/download/install.sh | sh
#
#   LIG_INSTALL_DIR  where to put lig (default: ~/.local/bin)
#   LIG_VERSION      install another release (e.g. 1.0.5)
set -eu

REPO="KevenGoncalves/lig"
VERSION="${LIG_VERSION:-@VERSION@}"
VERSION="${VERSION#v}"
DIR="${LIG_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'lig: %s\n' "$*"; }
fail() { printf 'lig: %s\n' "$*" >&2; exit 1; }

case "$VERSION" in
    @*) fail "this copy of the script has no version; run the one from a release, or set LIG_VERSION" ;;
esac
[ "$(uname -s)" = Linux ] || fail "lig runs on Linux only"
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) fail "there's no prebuilt lig for $(uname -m); build it from source instead" ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    fail "curl or wget is needed"
fi

name="lig-$VERSION-$arch-unknown-linux-musl"
url="https://github.com/$REPO/releases/download/v$VERSION/$name.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "downloading lig $VERSION ($arch)"
fetch "$url" "$tmp/$name.tar.gz" || fail "couldn't download $url"
if command -v sha256sum >/dev/null 2>&1; then
    fetch "$url.sha256" "$tmp/$name.tar.gz.sha256" || fail "couldn't download the checksum"
    (cd "$tmp" && sha256sum -c --quiet "$name.tar.gz.sha256") || fail "the download doesn't match its checksum"
else
    say "sha256sum isn't installed; skipping the checksum"
fi
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

mkdir -p "$DIR"
cp "$tmp/$name/lig" "$DIR/lig.new"
chmod 755 "$DIR/lig.new"
mv "$DIR/lig.new" "$DIR/lig"
say "installed $("$DIR/lig" --version) in $DIR"

case ":$PATH:" in
    *":$DIR:"*) ;;
    *) say "$DIR isn't in your PATH; add it in your shell's config" ;;
esac
say "next: run \`lig install\` to set it up in your shell, then open a new terminal"

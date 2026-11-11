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
system="$(uname -s)"
arch="$(uname -m)"
case "$system:$arch" in
    Linux:x86_64 | Linux:amd64) target=x86_64-unknown-linux-musl ;;
    Linux:aarch64 | Linux:arm64) target=aarch64-unknown-linux-musl ;;
    Darwin:arm64 | Darwin:aarch64) target=aarch64-apple-darwin ;;
    # A shell under Rosetta reports x86_64 on Apple Silicon too.
    Darwin:x86_64)
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
            target=aarch64-apple-darwin
        else
            fail "there's no prebuilt lig for Intel Macs; macOS builds require Apple Silicon"
        fi
        ;;
    *) fail "there's no prebuilt lig for $system/$arch; build it from source instead" ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
else
    fail "curl or wget is needed"
fi

name="lig-$VERSION-$target"
url="https://github.com/$REPO/releases/download/v$VERSION/$name.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "downloading lig $VERSION ($arch)"
fetch "$url" "$tmp/$name.tar.gz" || fail "couldn't download $url"
fetch "$url.sha256" "$tmp/$name.tar.gz.sha256" || fail "couldn't download the checksum"
if command -v sha256sum >/dev/null 2>&1; then
    (cd "$tmp" && sha256sum -c "$name.tar.gz.sha256") || fail "the download doesn't match its checksum"
elif command -v shasum >/dev/null 2>&1; then
    (cd "$tmp" && shasum -a 256 -c "$name.tar.gz.sha256") || fail "the download doesn't match its checksum"
else
    fail "sha256sum or shasum is required to verify the download"
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

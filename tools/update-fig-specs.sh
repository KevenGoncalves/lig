#!/usr/bin/env bash
# Regenerate specs/fig/ from https://github.com/withfig/autocomplete (src/).
#
# Needs git, node/npm (only to compile the TypeScript sources with esbuild)
# and cargo. Nothing from this process ends up in the lig binary except the
# generated JSON.
set -euo pipefail

REPO=https://github.com/withfig/autocomplete.git
REF=${1:-master}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

git clone --depth 1 --branch "$REF" "$REPO" "$WORK/src-repo"
cd "$WORK/src-repo"
echo "withfig/autocomplete $(git rev-parse HEAD)" >"$WORK/REVISION"

# Spec helper libraries the sources import; no install scripts are run.
npm install --no-save --no-package-lock --ignore-scripts --no-audit --no-fund \
  esbuild@0.24.2 @fig/autocomplete-generators@2 @fig/autocomplete-helpers@1 semver@7 strip-json-comments@5 yaml@2

find src -name '*.ts' ! -name '*.d.ts' >"$WORK/entries"
# Each spec becomes a self-contained script defining `__lig_module`.
xargs node_modules/.bin/esbuild --bundle --format=iife --global-name=__lig_module \
  --platform=node --outdir="$WORK/build" --outbase=src --log-level=error <"$WORK/entries"

cd "$ROOT"
rm -rf specs/fig
cargo run --release -p figconv --example figconv -- "$WORK/build" specs/fig
cp "$WORK/src-repo/LICENSE" specs/fig/LICENSE
cp "$WORK/REVISION" specs/fig/REVISION
echo "specs/fig updated from $(cat "$WORK/REVISION")"

#!/bin/sh
# Write the AUR package for a release: aur.sh <version> <folder with the .sha256 files> <output folder>
set -eu
version=$1
sums=$2
out=$3
here=$(dirname "$0")
mkdir -p "$out"
for file in PKGBUILD .SRCINFO lig.install; do
    text=$(sed "s/@VERSION@/$version/g" "$here/$file")
    for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
        sum=$(cut -d' ' -f1 "$sums/lig-$version-$target.tar.gz.sha256")
        text=$(printf '%s\n' "$text" | sed "s/@SHA_$target@/$sum/")
    done
    printf '%s\n' "$text" > "$out/$file"
done

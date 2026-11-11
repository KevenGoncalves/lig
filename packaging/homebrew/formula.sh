#!/bin/sh
# Print the Homebrew formula for a release: formula.sh <version> <folder with the .sha256 files>
set -eu
version=$1
dir=$2
formula=$(sed "s/@VERSION@/$version/g" "$(dirname "$0")/lig.rb")
for target in aarch64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
    sum=$(cut -d' ' -f1 "$dir/lig-$version-$target.tar.gz.sha256")
    formula=$(printf '%s\n' "$formula" | sed "s/@SHA_$target@/$sum/")
done
printf '%s\n' "$formula"

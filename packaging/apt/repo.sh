#!/bin/sh
# Build a signed APT repository: repo.sh <folder with .deb files> <output folder>
#
# Signs with the one secret key in the GnuPG keyring, and writes its public key to
# <output>/lig.gpg for users' /etc/apt/keyrings. Users add:
#   deb [signed-by=/etc/apt/keyrings/lig.gpg] https://kevengoncalves.github.io/lig/apt stable main
set -eu
debs=$1
out=$2
mkdir -p "$out/pool/main"
cp "$debs"/*.deb "$out/pool/main/"
cd "$out"
for arch in amd64 arm64; do
    dir=dists/stable/main/binary-$arch
    mkdir -p "$dir"
    apt-ftparchive --arch "$arch" packages pool > "$dir/Packages"
    gzip -9kf "$dir/Packages"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=lig \
    -o APT::FTPArchive::Release::Label=lig \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Components=main \
    release dists/stable > Release
mv Release dists/stable/Release
gpg --batch --yes --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
key=$(gpg --batch --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
gpg --batch --export "$key" > lig.gpg

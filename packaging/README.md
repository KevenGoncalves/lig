# Packaging

The release workflow (`.github/workflows/release.yml`) builds every package from the release
binaries and publishes to each channel whose secret is set; a channel without one is skipped.

| Channel | Files |
| --- | --- |
| `.deb`, `.rpm` on the GitHub release | `nfpm.yaml`, `postinstall.sh` |
| Homebrew ([KevenGoncalves/homebrew-tap](https://github.com/KevenGoncalves/homebrew-tap)) | `homebrew/` |
| Fedora Copr (built from source) | `rpm/lig.spec`, `../.copr/Makefile` |
| APT repository on GitHub Pages | `apt/repo.sh` |
| AUR (`lig-bin`, not published yet) | `aur/` |

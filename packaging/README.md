# Packaging

The release workflow (`.github/workflows/release.yml`) builds every package from the release
binaries and, when the secret for a channel is set, publishes to it. A channel without its
secret is skipped, so a release never fails for lack of one.

| Channel | Files | Secret | Status |
| --- | --- | --- | --- |
| `.deb`, `.rpm` on the release | `nfpm.yaml`, `postinstall.sh` | none | live |
| Homebrew tap | `homebrew/` | `HOMEBREW_TAP_TOKEN` | live: `brew install kevengoncalves/tap/lig` |
| AUR (`lig-bin`) | `aur/` | `AUR_SSH_KEY` | ready, not published (see below) |
| Fedora Copr | `rpm/lig.spec`, `../.copr/Makefile` | `COPR_WEBHOOK_URL` | live: `dnf copr enable kevengoncalves/lig` |

## Homebrew

[KevenGoncalves/homebrew-tap](https://github.com/KevenGoncalves/homebrew-tap) holds
`Formula/lig.rb`, written by `homebrew/formula.sh` from the release's `.sha256` files.
`HOMEBREW_TAP_TOKEN` is a fine-grained token with **Contents: Read and write** on
`homebrew-tap` only; it expires after a year, and then only the tap step fails until it's
replaced (`gh secret set HOMEBREW_TAP_TOKEN --repo KevenGoncalves/lig`).

## AUR

`aur/aur.sh` writes `PKGBUILD`, `.SRCINFO` and `lig.install` for a release; the release
workflow pushes them to `ssh://aur@aur.archlinux.org/lig-bin.git`. The package was tested in
an Arch container (`.SRCINFO` matches `makepkg --printsrcinfo`, `namcap` is clean), but it
isn't published: AUR registration was paused in November 2026, so there's no account yet.

When registration reopens:

1. Create an account at <https://aur.archlinux.org/register>.
2. Make a key for it: `ssh-keygen -t ed25519 -N "" -C "aur lig-bin" -f ~/.ssh/aur_lig`, and
   paste `~/.ssh/aur_lig.pub` into **My Account → SSH Public Key**.
3. Publish the current release once by hand (the workflow updates it from then on):

   ```bash
   git clone ssh://aur@aur.archlinux.org/lig-bin.git /tmp/lig-bin
   packaging/aur/aur.sh <version> <folder with the release's .sha256 files> /tmp/lig-bin
   cd /tmp/lig-bin && git add PKGBUILD .SRCINFO lig.install && git commit -m "lig <version>"
   GIT_SSH_COMMAND="ssh -i ~/.ssh/aur_lig" git push origin HEAD:master
   ```

4. `gh secret set AUR_SSH_KEY --repo KevenGoncalves/lig < ~/.ssh/aur_lig`
5. Add `yay -S lig-bin` to the install section of the main README.

## Fedora Copr

Copr builds lig from source for each release: the release workflow posts to the package's
custom webhook, and Copr runs `.copr/Makefile`, which makes an SRPM of the tag named in
`Cargo.toml` with `rpm/lig.spec`. Tested in a Fedora container (`make srpm`, then
`rpmbuild --rebuild` and install).

Setup, once:

1. Log in to <https://copr.fedorainfracloud.org> with a Fedora account
   (<https://accounts.fedoraproject.org>).
2. **New project** `lig`: pick the Fedora chroots (and `epel-9`/`epel-10` for RHEL and its
   clones), and tick **Enable internet access during builds** (cargo downloads the crates).
3. **Packages → New package**, source type **SCM**: name `lig`, clone URL
   `https://github.com/KevenGoncalves/lig.git`, SRPM build method **make srpm**. Build it
   once from there.
4. **Settings → Integrations**: copy the custom webhook URL, which ends in
   `/webhooks/custom/<ID>/<UUID>/`, add the package name: `…/<UUID>/lig/`, and save it:
   `gh secret set COPR_WEBHOOK_URL --repo KevenGoncalves/lig`
5. Add `sudo dnf copr enable kevengoncalves/lig && sudo dnf install lig` to the main README.

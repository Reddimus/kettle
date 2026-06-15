# AUR `kettle-bin` package

[`PKGBUILD`](PKGBUILD) is a ready-to-use AUR package definition that
installs the prebuilt Linux x86_64 binary from a tagged GitHub
release. Once submitted to the AUR, Arch / Manjaro / EndeavourOS
users can install with any AUR helper:

```sh
yay -S kettle-bin
# or
paru -S kettle-bin
# or
trizen -S kettle-bin
```

## One-time AUR submission (maintainer)

1. **Create an AUR account** at <https://aur.archlinux.org/> if you
   don't have one, and add an SSH public key under Account → My
   account.

2. **Clone the (new) AUR repo:**

   ```sh
   git clone ssh://aur@aur.archlinux.org/kettle-bin.git
   cd kettle-bin
   ```

   The AUR creates the repo on first push if the name is available.

3. **Copy the PKGBUILD + generate `.SRCINFO`:**

   ```sh
   cp /path/to/kettle/packaging/arch/PKGBUILD .
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD .SRCINFO
   git commit -m "kettle-bin v1.3.5"
   git push origin master
   ```

4. **Verify on the AUR** at
   `https://aur.archlinux.org/packages/kettle-bin` once the push
   propagates.

## Per-release maintenance

On every new kettle tag (`v1.3.6`, etc.):

```sh
# 1. Bump pkgver + refresh sha256 in PKGBUILD.
sed -i 's/^pkgver=.*$/pkgver=1.3.6/' PKGBUILD
NEW_SHA=$(curl -fsSL https://github.com/Reddimus/kettle/releases/download/v1.3.6/kettle-linux-x86_64.tar.gz.sha256 | awk '{print $1}')
sed -i "s/^sha256sums=.*$/sha256sums=('${NEW_SHA}')/" PKGBUILD

# 2. Regenerate .SRCINFO (the AUR's machine-readable index).
makepkg --printsrcinfo > .SRCINFO

# 3. Sanity-check the build (optional but recommended).
makepkg -fci   # builds + installs locally, won't touch system if it fails

# 4. Push.
git add PKGBUILD .SRCINFO
git commit -m "kettle-bin v1.3.6"
git push
```

The cycle-254 `.sha256` sidecars on each GitHub release make step 1
deterministic — no manual checksumming. From the main kettle repo, run
`scripts/check-package-templates.sh --require-release` after updating the
template; it verifies `pkgver`, the PKGBUILD hash, the Homebrew Linux hash, and
the published release sidecar all agree.

## Why `kettle-bin` and not `kettle`?

AUR convention: `<name>-bin` packages install a prebuilt binary,
`<name>` builds from source. We ship `kettle-bin` because:

- The release tarball already carries a stripped, optimized binary
  built on a clean CI runner — there's no benefit to rebuilding
  on every user's machine, just a 3-minute wait per install.
- Source-build PKGBUILDs need `cargo` + the full system dep list
  (pkg-config, libfontconfig-dev, libfreetype-dev, libx11-dev,
  libxkbcommon-dev, libwayland-dev, libxcb-devel, etc.). The
  binary path skips all of that.

A source-build companion (`kettle`, no suffix) is a future-cycle
addition for users who specifically want to build from source —
ROADMAP "Homebrew tap + AUR package + nixpkgs flake" tracks it.

## Why this lives in the main repo

Same rationale as `packaging/homebrew/` — the PKGBUILD pins exact
SHA-256s tied to a kettle release, so bumping it lives best in
the PR that bumps `Cargo.toml`. The AUR repo gets a one-line copy
on every release.

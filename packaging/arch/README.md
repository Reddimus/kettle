# AUR `kettle-bin` package

Every stable GitHub release includes a ready-to-use `PKGBUILD` for the Linux
x86_64 binary. The source tree stores [`PKGBUILD.in`](PKGBUILD.in); release CI
fills its version and SHA-256 only after verifying the final archive.

Arch, Manjaro, and EndeavourOS users can install the published AUR package with
an AUR helper once it has been submitted:

```sh
yay -S kettle-bin
# or: paru -S kettle-bin
```

## One-time AUR submission

1. Create an AUR account and register an SSH public key.
2. Clone `ssh://aur@aur.archlinux.org/kettle-bin.git`.
3. Download the generated package definition and create `.SRCINFO`:

   ```sh
   curl -fL https://github.com/Reddimus/kettle/releases/latest/download/PKGBUILD \
     -o PKGBUILD
   makepkg --printsrcinfo > .SRCINFO
   makepkg -fci
   git add PKGBUILD .SRCINFO
   git commit -m "initial kettle-bin release"
   git push origin master
   ```

## Per-release maintenance

Refresh from the generated release asset instead of hand-editing a version or
checksum:

```sh
curl -fL https://github.com/Reddimus/kettle/releases/latest/download/PKGBUILD \
  -o PKGBUILD
makepkg --printsrcinfo > .SRCINFO
makepkg -fci
git add PKGBUILD .SRCINFO
git commit -m "kettle-bin $(sed -n 's/^pkgver=//p' PKGBUILD)"
git push
```

From the Kettle repository, verify the generated Homebrew and AUR assets
against their published sidecars with:

```sh
scripts/check-package-templates.sh --require-release
```

The package is named `kettle-bin` because it installs the optimized prebuilt
binary. A source-building `kettle` package would require Rust and the complete
font, window-system, and graphics development stack.

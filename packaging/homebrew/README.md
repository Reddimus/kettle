# Homebrew formula asset (tap not yet published)

Every stable GitHub release includes a ready-to-use `kettle.rb` formula. The
source tree stores [`kettle.rb.in`](kettle.rb.in), whose version and checksums
are deliberately unresolved until release CI has built and verified the exact
macOS and Linux archives.

There is currently no public `Reddimus/homebrew-kettle` repository. Therefore
`brew tap reddimus/kettle` and `brew install reddimus/kettle/kettle` do not
resolve yet. The generated formula is a verified release asset and the
instructions below are the remaining maintainer publication workflow, not a
claim that the tap is live.

## One-time publication setup

1. Create a public `homebrew-kettle` repository under the same GitHub owner.
   Homebrew maps `brew tap reddimus/kettle` to that repository.
2. Download the generated formula from the latest release into the tap:

   ```sh
   mkdir -p Formula
   curl -fL https://github.com/Reddimus/kettle/releases/latest/download/kettle.rb \
     -o Formula/kettle.rb
   ```

3. Commit and push `Formula/kettle.rb`.

Only after those steps succeed can users install on macOS or Linuxbrew with:

```sh
brew tap reddimus/kettle
brew install kettle
```

The formula installs the macOS application bundle or Linux binary, desktop
launcher, icons, man page, and offline documentation as appropriate.

## Per-release maintenance

The release finalizer computes SHA-256 directly from the verified archives and
renders `kettle.rb` with `scripts/render-package-templates.py`. This avoids the
invalid intermediate state where a new version points at the previous
release's checksums. Update the tap from the newly published asset:

```sh
curl -fL https://github.com/Reddimus/kettle/releases/latest/download/kettle.rb \
  -o Formula/kettle.rb
brew audit --strict --online kettle
brew test kettle
git add Formula/kettle.rb
git commit -m "kettle $(sed -n 's/^  version \"\([^\"]*\)\"/\1/p' Formula/kettle.rb)"
git push
```

From the Kettle repository, this command strictly verifies that the published
formula and AUR metadata match their release sidecars:

```sh
scripts/check-package-templates.sh --require-release
```

The tap is the intended deployment target. Until it exists, install the macOS
application or Linux archive from the GitHub release. The `.in` file in this
repository is the reviewed source template, and the generated release asset is
the formula intended for eventual tap publication.

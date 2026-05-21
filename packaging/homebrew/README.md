# Homebrew tap setup

[`kettle.rb`](kettle.rb) is a ready-to-use Homebrew formula that
installs the prebuilt binary from a tagged GitHub release. To make
`brew install kettle` work for end users, the formula needs to live
in a *Homebrew tap* — a separate GitHub repo named
`homebrew-<project>` that Homebrew clones on `brew tap`.

## One-time setup (maintainer)

1. **Create the tap repo.** A public repo named exactly
   `homebrew-kettle` under the same GitHub org/user as kettle —
   Homebrew expects the `homebrew-` prefix so `brew tap
   reddimus/kettle` resolves to `Reddimus/homebrew-kettle`.

2. **Copy this directory's `kettle.rb` into the tap repo** at the
   path `Formula/kettle.rb`. The `Formula/` subdirectory is
   conventional and `brew tap` discovers it without extra config.

3. **Tag the tap repo `v1.0.0`** (the tap version is independent
   of kettle's version; just a marker that the tap is live).

End users then install with two commands:

```sh
brew tap reddimus/kettle
brew install kettle
```

On both macOS and Linuxbrew. The formula handles platform-specific
artifact selection (`.app` zip on macOS, tarball on Linux) and
installs the XDG launcher + icons under `share/` on Linux so the
kettle tile shows up in GNOME Activities / KDE Krunner the same way
the cycle-0 `install.sh` does.

## Per-release maintenance

On every new kettle tag, **two lines in `kettle.rb` need updating** —
the `version` and the per-platform `sha256` hashes. The hashes live
next to the artifacts on the release page (the cycle-254 `.sha256`
sidecars). One-liner to fetch both for a given version:

```sh
VER=1.3.5
for asset in kettle-macos-universal.zip kettle-linux-x86_64.tar.gz; do
  printf '%s  ' "$asset"
  curl -fsSL "https://github.com/Reddimus/kettle/releases/download/v${VER}/${asset}.sha256" \
    | awk '{print $1}'
done
```

Then drop the two hex strings into the matching `sha256` fields in
`kettle.rb` and commit. `brew livecheck kettle` (run in the tap
repo) flags this drift automatically — the `livecheck` block in
the formula resolves against `/releases/latest` via the same
GitHub redirect the cycle-253 `install-online.sh` uses.

## Why this lives in the main repo

A homebrew tap is a separate repo, but the *formula* is part of
the kettle release surface — it pins exact SHA-256s for that
release, and shifting it lives best alongside the artifact
publication that produces those hashes. Storing the template here
means:

- The formula bumps as part of the same PR that bumps `Cargo.toml`
  (single source of truth for the version).
- Future contributors looking at "how does kettle ship?" see all
  packaging paths in one tree (`packaging/{linux,macos,windows,
  homebrew}/`).
- The tap repo gets a one-line copy on every release rather than
  carrying its own drift.

Once the tap repo is up, this directory becomes the canonical
template; the tap repo is the deployment target.

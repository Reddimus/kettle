# Version history

This page summarizes Kettle's release history. `CHANGELOG.md`, Git tags, and
GitHub releases remain the authoritative sources for exact change lists and
artifacts.

## Current baseline

- Latest release: `v2.34.1`
- Current workspace version: `2.34.1`
- Release records inspected: 127 Git tags and 127 changelog headings, from
  `v0.1.0` through `v2.34.1`
- Version-bearing files that must stay in lockstep: workspace `Cargo.toml`,
  `flake.nix`, Homebrew formula, Arch `PKGBUILD`, and the changelog

## Release eras

- `v0.1.0` to `v1.0.1` (2026-05-19 to 2026-05-20): initial public terminal
  foundation, packaging, release artifacts, and install documentation.
- `v1.1.0` to `v1.47.0` (2026-05-20 to 2026-05-29): rapid parity cycles for
  VT behavior, config compatibility, packaging reliability, and the first broad
  audit/test hardening passes.
- `v2.0.0` to `v2.9.0` (2026-05-30 to 2026-06-06): cross-platform polish,
  Windows behavior, theme work, dev-record support, screenshot coverage, and
  renderer correctness fixes.
- `v2.10.0` to `v2.20.0` (2026-06-07 to 2026-06-12): multiplexing, WSL-aware
  split behavior, tab tear-off work, control-plane foundations, and major
  performance improvements.
- `v2.21.0` to `v2.28.0` (2026-06-13 to 2026-06-19): GPU/background features,
  animated media, tab/theme settings, scrollbar work, and release packaging
  refreshes.
- `v2.29.0` to `v2.34.1` (2026-06-19 to 2026-07-03): cwd-aware titles and
  shell integration, GPU device-loss resilience, Ubuntu titlebar fixes,
  keyboard text selection, package-template lockstep, and dev-record launcher
  sync.

## Maintainer checks

Before cutting a public release, verify:

- `cargo metadata --no-deps` reports the intended workspace package version.
- `rg -n 'version = "|pkgver=|sha256|## \\[' Cargo.toml flake.nix packaging CHANGELOG.md`
  shows only the intended version/hash changes.
- `scripts/check-package-templates.sh --require-release` passes against the
  release artifacts and `.sha256` sidecars.
- GitHub release assets, Homebrew hashes, and Arch checksums match the exact
  tag being published.

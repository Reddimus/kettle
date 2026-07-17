# Version history

This page summarizes Kettle's release history. `CHANGELOG.md`, Git tags, and
GitHub releases remain the authoritative sources for exact change lists and
artifacts.

## Current baseline

- Latest release: `v2.36.4`
- Current workspace version: `2.36.4`
- Release records inspected: 136 Git tags and 136 changelog headings, from
  `v0.1.0` through `v2.36.4`
- Version-bearing source files that must stay in lockstep: workspace
  `Cargo.toml`, `flake.nix`, and the changelog. Release CI renders the Homebrew
  formula and Arch `PKGBUILD` from verified archives so their checksums cannot
  refer to a different version.

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
- `v2.29.0` to `v2.36.4` (2026-06-19 to 2026-07-16): cwd-aware titles and
  shell integration, GPU device-loss resilience, Ubuntu titlebar fixes,
  keyboard text selection, package-template lockstep, bounded/private recording
  retention, graphics-resource accounting, hardened control/MCP and durable
  state boundaries, and restartable installer-owned updates.

## Maintainer checks

Before cutting a public release, verify:

- `cargo metadata --no-deps` reports the intended workspace package version.
- `rg -n 'version = "|## \\[' Cargo.toml flake.nix CHANGELOG.md` shows only
  the intended version changes.
- `scripts/check-package-templates.sh --local` validates the package renderer
  before the tag exists.

After publication, run `scripts/check-package-templates.sh --require-release`
to prove the generated Homebrew/AUR metadata matches the exact tagged archive
sidecars.

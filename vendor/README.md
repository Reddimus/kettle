# Vendored Rust dependencies

Kettle normally consumes Rust dependencies from crates.io. A crate is copied
here only when a released dependency has a correctness defect on Kettle's
supported path and no fixed upstream release is available.

## `alacritty_terminal-0.26.0`

- Source: crates.io `alacritty_terminal` 0.26.0.
- Upstream revision recorded in the crate's `.cargo_vcs_info.json`.
- License: Apache-2.0; upstream license and README are retained.
- Local changes: keyboard-mode stack overflow evicts the oldest keyboard mode,
  not an unrelated title-stack entry; active keyboard flags are tracked per
  screen so direct flag changes are queryable, survive screen switches, and
  remain synchronized with the active stack entry.
- Excluded: the 46 MB upstream terminal reference fixture corpus and its
  explicit test target. Kettle does not build dependency-owned tests; its own
  regression tests cover the patched mode-stack behavior through the public
  terminal parser.

Remove the `[patch.crates-io]` entry and this directory after upgrading to an
upstream release that contains the fix.

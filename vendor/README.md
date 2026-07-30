# Vendored Rust dependencies

Kettle normally consumes Rust dependencies from crates.io. A crate is copied
here only when a released dependency has a correctness defect on Kettle's
supported path and no fixed upstream release is available.

The patched packages remain outside the product workspace, but
`vendor/Cargo.toml` groups them into a validation-only workspace.
`vendor/Cargo.lock` is committed so direct package tests never resolve a fresh
dependency graph; package-local `Cargo.lock` files and `target/` directories
remain generated noise and must not be committed. Run every retained unit
target, doctest, and warnings-denied clippy target with `just vendor-check`.
CI exercises the parser patches on Linux and the PTY patch on both Linux and
native Windows. Dependabot explicitly excludes `vendor/**`: vendored manifests
and this lock may be refreshed only as part of an intentional, reviewed
vendor-source update that revalidates the recorded provenance and local patch.

## `alacritty_terminal-0.26.0`

- Source: crates.io `alacritty_terminal` 0.26.0.
- Upstream revision recorded in the crate's `.cargo_vcs_info.json`.
- License: Apache-2.0; upstream license and README are retained.
- Local changes: keyboard-mode stack overflow evicts the oldest keyboard mode,
  not an unrelated title-stack entry; active keyboard flags are tracked per
  screen so direct flag changes are queryable, survive screen switches, and
  remain synchronized with the active stack entry. The grid also exposes a
  monotonic `history_origin` which advances when bounded scrollback evicts or
  explicitly purges rows; Kettle uses it to keep OSC 133 prompt anchors from
  aliasing unrelated rows after the history buffer wraps. After a scroll, the
  terminal also removes a selection whose complete range has left retained
  history, while preserving selections that merely moved into still-retained
  scrollback. DEC private modes 47, 1047, and 1049 now preserve/clear the
  alternate screen at their correct boundary and retain 1049 cursor
  save/restore semantics. An engine-owned, ordered 256-event graphics journal
  reports committed screen lifecycle and scroll-region mutations, coalesces
  compatible adjacent scrolls without losing the full monotonic screen delta,
  and exposes sticky overflow plus the current active screen for fail-safe
  resynchronization.
- Excluded: the 46 MB upstream terminal reference fixture corpus and its
  explicit reference-test target. This crate is excluded from root workspace
  membership, so `cargo test --workspace` covers the patched behavior through
  Kettle's public terminal-parser integration but does not run package-owned
  targets. Retained direct unit tests cover the mode stack, monotonic history
  origin, selection eviction, alternate-screen semantics, graphics-event
  ordering/coalescing, and overflow recovery; run them with
  `cargo test --locked --manifest-path vendor/Cargo.toml --target-dir
  target/vendor-check -p alacritty_terminal`.

Remove the `[patch.crates-io]` entry and this directory after upgrading to an
upstream release that contains all of these fixes.

## `vte-0.15.0`

- Source: crates.io `vte` 0.15.0.
- Upstream revision recorded in the crate's `.cargo_vcs_info.json`.
- License: Apache-2.0 OR MIT; both upstream license files and the README are
  retained.
- Local changes: synchronized-output buffering accepts a bounded queue of 256
  unforgeable, out-of-band markers associated with exact parser byte offsets.
  Marker-aware advance and forced-stop APIs replay callbacks in wire order,
  including across nested DEC 2026 boundaries, while a handler hook lets
  `alacritty_terminal` journal the same ordering point. Kettle uses these
  markers to defer graphics control strings before decoding can mutate
  buffer-local state, then replay each action against the exact terminal
  screen and cursor state that existed at its position in the PTY stream.
  One unrelated single-token fix: an OSC debug log borrowed its buffer
  redundantly, which upstream's own `#![deny(clippy::all)]` rejects from Rust
  1.97 onward under `clippy::useless_borrows_in_formatting`. Drop the fix if a
  later upstream release already carries it.
- Excluded: the crates.io registry marker, generated lockfile/build output,
  upstream CI metadata, parser-log example/demo fixture, and unrelated
  documentation sample. This crate is excluded from root workspace membership.
  Run its retained ANSI parser unit tests directly with
  `cargo test --locked --manifest-path vendor/Cargo.toml --target-dir
  target/vendor-check -p vte --features ansi`.

Remove the `[patch.crates-io]` entry and this directory after upgrading to an
upstream VTE release that provides an equivalent bounded, out-of-band,
byte-ordered synchronized-update marker API, or after Kettle no longer needs
to route graphics controls around the text parser.

## `portable-pty-0.9.0`

- Source: crates.io `portable-pty` 0.9.0.
- Upstream revision recorded in the crate's `.cargo_vcs_info.json`.
- License: MIT; the upstream license is retained.
- Local changes: adds an opt-in `MasterPty::take_nonblocking_writer` contract.
  The Windows ConPTY backend places only the caller's byte-pipe handle in
  `PIPE_NOWAIT`, so writes return partial/zero progress when conin is full. The
  synchronous handle passed to `CreatePseudoConsole` remains unchanged, as
  required by the Windows API. Validation-only maintenance also replaces an
  uninitialized Win32 attribute buffer with initialized storage and applies
  behavior-preserving lint cleanups required by Kettle's warnings-denied
  direct-package clippy gate. Five additional Unix-only cleanups apply Rust
  1.97's suggestions for redundant imports, borrows, conversions, and
  `Option` dereferencing. Drop those cleanups if a later upstream release
  already carries them.
- Excluded: the crates.io package's registry marker, generated lockfile, and
  standalone examples. Their explicit target stanzas and example-only
  development dependencies are removed from the local manifest; the optional
  `serde` dependency is retained and annotated for dependency auditing because
  generated derive code uses it when `serde_support` is enabled. This crate is
  also excluded from root workspace membership. Kettle exercises the public
  path through its PTY regressions; run the retained package-owned native unit
  test directly with
  `cargo test --locked --manifest-path vendor/Cargo.toml --target-dir
  target/vendor-check -p portable-pty`.

Remove the `[patch.crates-io]` entry and this directory after upgrading to an
upstream release that provides an equivalent nonblocking writer.

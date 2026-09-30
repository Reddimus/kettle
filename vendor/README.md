# Vendored Rust dependencies

Kettle normally consumes Rust dependencies from crates.io. A crate is copied
here only when a released dependency has a correctness defect on Kettle's
supported path and no fixed upstream release is available.

The patched packages remain outside the product workspace, but
`vendor/Cargo.toml` groups them, all but `cosmic-text` (see its section), into
a validation-only workspace.
`vendor/Cargo.lock` is committed so direct package tests never resolve a fresh
dependency graph; package-local `Cargo.lock` files and `target/` directories
remain generated noise and must not be committed. Run every retained unit
target, doctest, and warnings-denied clippy target with `just vendor-check`.
CI exercises the parser patches on Linux and the PTY patch on Linux, macOS, and
native Windows. Dependabot excludes this validation workspace and reads patched
crate manifests only as resolution support files. Updating vendored manifests
or this lock requires a reviewed vendor-source update that revalidates provenance
and local patches.

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
  resynchronization. The Windows ConPTY backend also no longer probes a bare
  `conpty.dll`. A bare name searches the application directory, the working
  directory, and `PATH`, which is a DLL preloading vector for a terminal
  launched inside an untrusted project; the same probe was removed from
  `portable-pty` for the same reason. Kettle does not use this backend — panes
  come from `portable-pty` — but `tty` still compiles into the binary, and a
  known preload pattern should not ship even unreferenced. Scrolling a region
  rotates its rows in one slice move while they sit contiguously in the ring
  buffer. Upstream swaps them one at a time, one wrapped index per row in the
  region for every line scrolled. The terminal answers the queries programs
  probe it with: XTVERSION with the `Config::xtversion` name (unanswered when
  unset), DECXCPR (`CSI ? 6 n`), DECRQSS for SGR, DECSTBM and DECSCUSR (an
  unknown setting is answered as invalid and never echoed), XTGETTCAP for a
  fixed set of truecolor, underline, cursor-shape and palette capabilities
  (each name answered separately, in one write, and echoed only as validated
  hex, at most 32 per request), and DECRQM for modes 47 and 1047. DSR and
  DECXCPR count the row from the top margin under origin mode, as xterm
  does, never below row 1 (DECRC does not restore DECOM here, so the cursor
  can sit above the margin); upstream reported the absolute row.
  `Term::set_color_scheme` and `Config::color_scheme_dark` give the light or
  dark scheme that `CSI ? 996 n` answers and DEC mode 2031 reports on every
  colour change the caller signals, dark to dark included; DECSTR and RIS
  turn mode 2031 off.
- Excluded: the 46 MB upstream terminal reference fixture corpus and its
  explicit reference-test target. This crate is excluded from root workspace
  membership, so `cargo test --workspace` covers the patched behavior through
  Kettle's public terminal-parser integration but does not run package-owned
  targets. Retained direct unit tests cover the mode stack, monotonic history
  origin, selection eviction, alternate-screen semantics, graphics-event
  ordering/coalescing, overflow recovery, region scrolls against the
  row-by-row swap, and the query replies (including Neovim's exact undercurl
  and truecolor probes); run them with
  `cargo test --locked --manifest-path vendor/Cargo.toml --target-dir
  target/vendor-check -p alacritty_terminal`.

Remove the `[patch.crates-io]` entry and this directory after upgrading to an
upstream release that contains all of these fixes.

## `cosmic-text-0.19.0`

- Source: crates.io `cosmic-text` 0.19.0.
- Upstream revision recorded in the crate's `.cargo_vcs_info.json`.
- License: MIT OR Apache-2.0; both upstream license files and the changelog are
  retained. The README is not: it is a gallery of screenshots that are not
  vendored, and its image links would not resolve.
- Local change: backports pop-os/cosmic-text commit
  `1e0074c83926041c16f9ee76afafe91819927013` ("fix: don't panic on lines with
  mixed-direction paragraph separators", 2026-08-08), which no release carries
  yet. `ShapeLine::build` asserted that every bidi paragraph in a line shares
  the first one's direction. cosmic-text splits lines only on CR and LF, while
  the bidi algorithm also ends a paragraph at U+2029, NEL and FS, so a line
  holding those between left-to-right and right-to-left text failed the
  assertion. The line is now laid out in its first paragraph's direction. The
  change is `src/shape.rs` only, +6 -14, identical to upstream's.
- Excluded: the registry marker, generated lockfile, upstream CI metadata and
  helper scripts, the README and its screenshots, the bundled test fonts,
  samples, the integration tests and benchmarks that read them, and
  `deny.toml`. Their `[[test]]` and
  `[[bench]]` stanzas, the benchmark-only development dependencies, and the
  package's `[profile.test]` (ignored for a non-root package) are removed from
  the local manifest.
- Validation: unlike the crates below, this one is not a member of
  `vendor/Cargo.toml`. As a member, its optional editor and `no_std` features
  would pull `syntect` and friends into `vendor/Cargo.lock`, and its required
  `fontdb` brings `ttf-parser`; the vendor audit deliberately admits no
  exceptions, while the product audit already carries the scoped `ttf-parser`
  one (`scripts/check-ttf-parser-scope.sh`). The product workspace compiles the
  crate through `[patch.crates-io]`, and `kettle-render`'s
  `paragraph_separator_shaping_tests` shape such lines through both `Buffer`
  and `BufferLine`; they fail against the unpatched release. The retained
  upstream unit tests and doctests (4 and 3) passed when this copy was made,
  run from a scratch workspace member.

Remove the `[patch.crates-io]` entry and this directory after upgrading to a
cosmic-text release that contains `1e0074c8`.

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
  Terminal queries reach the handler: XTVERSION (`CSI > q`), DEC private
  device status (`CSI ? Ps n`), and the bodies of DECRQSS (`DCS $ q`) and
  XTGETTCAP (`DCS + q`), kept across reads in a buffer bounded at 1 KiB; a
  longer body is reported without its contents so it is answered as invalid.
  The parser unhooks on ESC, CAN and SUB alike, so `Perform` gains
  `dcs_terminated_by_st`, called after `unhook` only when the string ended
  with ST: the 8-bit `0x9C`, or an ESC whose very next byte is `\`. A query is
  answered only then; one that CAN, SUB or another sequence cuts off gets no
  reply.
  DEC mode 2031 is a named private mode (`ColorSchemeReports`).
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
  required by the Windows API. The ConPTY loader no longer probes a bare
  `conpty.dll`; Kettle does not support sideloaded OpenConsole, so it resolves
  only the system `kernel32.dll` exports and cannot execute a DLL found through
  the application directory, working directory, or `PATH` during pane creation.
  A second opt-in command-builder flag creates Windows automation children
  suspended, assigns them to a shared kill-on-close Job Object, and resumes only
  after assignment succeeds. The assignment happens inside `CreateProcessW`'s
  owning backend because attaching from Kettle after spawn leaves an
  unavoidable window in which immediate descendants do not inherit the job.
  Rollback proves a failed assignment/resume really terminated the suspended
  process; cloned killers retain the same Job handle without a fallible
  duplication step. Job accounting exposes whether a live descendant can still
  write before Kettle closes ConPTY, and process-handle signalling
  disambiguates the valid exit code 259 from `STILL_ACTIVE`.
  A third opt-in, `CommandBuilder::set_require_cwd`, hands the configured
  `cwd` to the OS unchanged. Upstream replaces a `cwd` that is not a directory
  at spawn time with the home directory, so a directory deleted after Kettle's
  automation checked it would relocate the command. With the opt-in the spawn
  fails instead. Interactive panes leave it off and keep the HOME recovery.
  On Unix, dropping the master writer now closes only its duplicate descriptor
  and never writes a newline or VEOF byte into the terminal; deliberate EOF
  remains Kettle's live-termios `PtyStdin::try_signal_eof` path.
  Between `fork` and `exec` the Unix child makes only direct system calls and
  async-signal-safe library calls, and never allocates. It marks inherited
  descriptors close-on-exec instead of closing them, so std's exec-error
  channel still reports a failed `exec`. Linux uses one
  `close_range(CLOSE_RANGE_CLOEXEC)` call when the kernel accepts it. macOS 11
  and later mark the descriptors the kernel lists through
  `proc_pidinfo(PROC_PIDLISTFDS)` into a buffer the parent allocates before
  `fork`; older macOS kernels can omit high descriptors from that list, so
  they are not asked. Every other case falls back to one `fcntl` per
  descriptor number below the soft limit, capped at 1,048,576: other Unix
  systems, Linux when `close_range` is rejected, and macOS when the list is
  unreadable or fills the buffer. The macOS list keeps pane spawns from paying
  that loop under the 1,048,576 soft limit that Node-based launchers such as
  VS Code set.
  Validation-only maintenance also replaces an uninitialized Win32 attribute
  buffer with initialized storage and applies behavior-preserving lint cleanups
  required by Kettle's warnings-denied direct-package clippy gate. Five
  additional Unix-only cleanups apply Rust 1.97's suggestions for redundant imports,
  borrows, conversions, and `Option` dereferencing. Drop those cleanups if a
  later upstream release already carries them.
- Excluded: the crates.io package's registry marker, generated lockfile, and
  standalone examples. Their explicit target stanzas and example-only
  development dependencies are removed from the local manifest; the optional
  `serde` dependency is retained and annotated for dependency auditing because
  generated derive code uses it when `serde_support` is enabled. This crate is
  also excluded from root workspace membership. Kettle exercises the public
  path through its PTY regressions; run the retained package-owned native unit
  tests directly with
  `cargo test --locked --manifest-path vendor/Cargo.toml --target-dir
  target/vendor-check -p portable-pty --features serde_support`. The feature is
  intentional: it compiles the backward-compatible default for the new
  containment field and its enabled round trip on every native vendor gate.

Remove the `[patch.crates-io]` entry and this directory after upgrading to an
upstream release that provides both an equivalent nonblocking writer and
pre-resume process-tree containment.

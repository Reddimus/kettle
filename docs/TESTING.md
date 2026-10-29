# Testing

Run `just gauntlet` before opening a PR. Before a release or a supply-chain
change, run `just gauntlet-strict`; use `just gauntlet-full` for the required
native gates on your current OS, especially after packaging or renderer changes.
CI adds the other operating systems, the minimum Rust version, packaging,
scheduled dependency scans, and longer fuzz runs. Linux and macOS are supported
runtime hosts. Windows CI retains compile and regression coverage, but does not
produce a supported package. Record every skipped check and its reason.

## Run it

Run commands from the repository root. [Justfile](../Justfile) defines the
recipes; `just --list` shows those available on your host.
Install the source-build dependencies in [INSTALL.md](INSTALL.md#from-source).
Use Rust stable with rustfmt and Clippy, Python 3, Git, and just.
The workspace declares Rust 1.95 as its minimum version in
[Cargo.toml](../Cargo.toml).

For the normal edit loop:

```sh
cargo fmt -p <crate>
cargo fmt --all --check
just gauntlet
```

Replace `<crate>` with the package you changed. Format only that crate while
iterating. The workspace check catches formatting drift elsewhere.
A focused test helps during development, but does not replace the gate.

`kettle-vt::control` has a 14-state by 25-byte-class transition/event matrix,
UTF-8 shielding checks across all 64 continuation values and three outstanding
lengths, and bound checks for every open state. Its differential harness retains
both former strippers as test-only source snapshots. Existing exec and session
log stripping vectors run the shared implementation and both originals; ordinary
output must match an original exactly. A second log oracle adapts only ground C1
recognition, allowing cumulative intentional differences to be checked too.
The edge table sweeps every two-chunk split. A 65,545-byte bound case splits
at every offset near its introducer and its bound and at a prime stride
between them; other long cases use one-byte feeds and sweep the restart
boundary. One deterministic 1 MiB arbitrary-byte
stream uses random chunks of 1 through 8192 bytes and matches the unmodified
exec oracle over the whole stream. Stage 2 probes pin the extractor's distinct
C1, private-ESC and bulk-recovery policies. Run these with
`cargo test --locked --offline -p kettle-vt control::` and the consumer vectors
with the full `kettle-core` and `kettle` suites. The extractor benchmarks remain
in `crates/kettle-vt/benches/extract.rs`.

The `kettle-ui` tests selected by `sandbox_pattern_` cover safe and restricted
Lua pattern budgets: empty replacement expansions, expanded output bytes,
separate calls and iterators, protected calls, callback retirement, and budget
resets. `lua_pattern::tests::differential_lua54_patterns` compares ordinary
matching and replacement behavior against native Lua. For an offline run, use
`cargo test -j 2 --locked --offline -p kettle-ui sandbox_pattern_`, followed by
`cargo test -j 2 --locked --offline -p kettle-ui` for the full crate suite.

The renderer's `ime_preedit_` tests cover a single composition glyph with a
nonblinking block cursor and composition over existing terminal text with
`cell-width = 0.5`, in both grid and legacy modes. A portable shaping test also
checks that `cell-height = 0.5` retains the full composition line height in its
opaque background and text clip. The headless screenshots
require a GPU adapter and report a skip when none is available. Run
`cargo test -j 2 --locked --offline -p kettle-render -p kettle-ui ime_preedit_ -- --nocapture`
to see those skips, then run the same command without the filter for both full
crate suites.

The renderer recovery tests include successful device creation on a backend
that failed while drawing, checking that another backend is tried first.
`python3 scripts/test-install-media.py` interrupts uninstall after each recovery
helper deletion and retries through installed helpers or a fresh package install.
`python3 scripts/perf/macos-compare-score-self-test.py` forces process identity
probes to time out during registration and after successful registration. It
verifies that the benchmark wrapper reaps a child that ignores SIGTERM on
timeout, payload completion, and a shutdown signal, preserving exit statuses
and removing the registry entry and completion marker.

`cargo test -j 2 --locked --offline -p kettle-core
search_tolerates_grid_commit_before_output_publication` uses the terminal's
commit hook to run a search after the grid mutex is released and before output
publication. It retains yielded forward/reverse regex work, rewrites the row
without changing its layout or generation, checks results against the new
cells, then verifies that the real publication helper invalidates the old token
and wakes once. It needs no PTY, GPU, sleeps or scheduler timing.
`cargo test -j 2 --locked --offline -p kettle-ui
output_frame_preserves_publication_after_cached_grid_reads` pauses frame
preparation with new grid text and the old generation, exercises the real link
cache, then drives the real publication helper with a no-text local parser
turn. The pre-fix order consumes the invalidation while retaining old links;
the fixed frame cutoff retains a retry. It also checks snapshot-reuse tags and
the late-wake/presentation recheck. This Unix fixture needs a local PTY, but no
GPU or control socket. The UI tests
`hover_generation_candidate_preserves_racing_output_as_pending`,
`pane_snapshot_reuse_fails_closed_on_output_layout_or_order_changes`, and
`stale_presented_output_wakeup_rearms_without_losing_a_race` protect the
corresponding frame/reuse/wake invalidation rules. See
[output generation and grid reads](ARCHITECTURE.md#output-generation-and-grid-reads)
for the reader analysis. Finish with full `kettle-core` and `kettle-ui` tests and
Clippy for both crates with `--all-targets -- -D warnings`.

### Main gates

The MCP stdio integration suite exercises Codex guidance, turn-end-only fallback,
one artifact per hook, deduplication after primary display, stable replacement
keys and shutdown cleanup through the real server process and a fixture control
transport. Inbox unit tests cover rotated filenames, in-flight rewrites, links,
FIFOs, oversized entries and bounded directories. These checks do not substitute
for native Codex hook trust and rendered-card acceptance.
Unix process regressions also send SIGTERM, SIGINT and SIGHUP while stdin holds
an unfinished request. Each must remove the private inbox, preserve a linked
durable original, and retain the signal termination status. This exercises
native Codex's signal-before-transport-close shutdown order, beyond clean EOF.
A barrier-controlled destructor regression verifies that simultaneous closers
serialize resource removal before either caller can report cleanup complete.
The agy process fixture deliberately disables the default config while enabling
the active controller's policy, then switches that live policy off. This covers
profile/config selection and makes default-file shortcuts fail the regression.
Native agy payload regressions cover `write_file` with `Path` and explicit
`ArtifactMetadata.UserFacing`, plus `edit_file` with `Path`, as captured from
agy 1.4.3 hooks. Plugin tests exercise the emitted matcher against those names.
Edits preserve the native sidecar's user-facing flag; private, missing,
malformed, oversized and linked sidecars receive no automatic preview.
The SDK's startup tool list can still advertise legacy names; use captured
native hook inputs to verify the adapter contract. A headless payload probe
does not substitute for foreground provenance and rendered-shelf acceptance.
Native macOS process tests cover terminal input with stdout/stderr redirected,
kernel launch arguments, wrong terminal devices, exited/reused identities and
argv/environment separation. Mode tests exclude every print alias, subcommands,
unknown flags and incomplete options. Native agy TUI acceptance must additionally
observe an artifact-only edit update the existing shelf item without an MCP call.

`python3 scripts/test-release.py` parses the macOS app and helper permission
manifests and executes the real workflow signing loop with fixture binaries.
It requires an Apple Events purpose message, limits the app to that entitlement,
and keeps nested helpers unentitled. Native signed acceptance must also verify
editor/PDF synchronization and preserve macOS's per-destination consent review.

- `just gauntlet`: checks formatting, locked workspace build/test/Clippy,
  default-feature core Clippy, rustdoc, media test features, and helper fixtures.
  Run before a PR; needs the source-build tools and native libraries.
- `just gauntlet-strict`: adds vendor tests, deny/audit/shear, the advisory
  scope guard, the static checks, tracked-file audit, fuzz guards, Mermaid,
  and release fixtures. Run before releases or supply-chain changes; install
  the tools listed below.
- `just gauntlet-full`: runs strict plus `full-native-gates` for this OS.
  Run after packaging, installer, or renderer work and before a release;
  needs release builds, native tools, a GPU adapter, and a graphical session.
- `just fuzz-check`: checks formatting and lints the fuzz workspace on stable.
  Run after changing fuzz entry points; Unix only, with a notice on Windows.
- `just fuzz-guard`: tests the planner and target table, then checks that
  shipped binaries exclude fuzzing features. Run after fuzz or feature changes;
  needs Python, Bash, and Cargo on Unix.
- `just vendor-check`: directly tests and lints the patched parser and PTY
  crates and checks the clipboard patch. Run after vendor changes;
  needs the native Rust toolchain and Python on the current OS.
- `just tracked-audit`: runs its self-test and audits every tracked path.
  Run after file, asset, manifest, or documentation changes; needs Git and
  Python 3 on Unix, with output under `target/diagnostics`.
- `just mermaid-check`: compiles diagrams in tracked Markdown.
  Run after diagram changes; needs Python, npx, and Chrome/Chromium on Unix.
  Missing rendering tools skip locally unless required mode is enabled.

The strict gate requires `cargo-deny`, `cargo-audit`, `cargo-shear`, `zizmor`,
`lychee`, `shellcheck`, `ruff`, and `typos`. Missing tools fail their commands. The audit recipe fetches the advisory database
into `target/advisory-db`. Supply-chain scans can need network access.
The Mermaid helper can fetch its CLI through npx.
Do not assume either gate is offline.

### Focused recipes

`cargo test -p kettle --test source_id --test build_freshness` checks GUI/worker
identity and Cargo caching. On Unix, the Cargo fixture also edits, renames and
deletes linked sources and creates missing targets. Unchanged source-only
libraries must stay cached; the executable refreshes its Git status each build.
The fixtures also prove that `vendor/target`, `vendor/*/target`, and package-local
vendor lockfiles do not change identity, while vendored source and the vendor
workspace lockfile still count. Cargo's recursive directory watches can rerun
the build scripts after generated files change; those watches remain necessary
to detect new, renamed, and deleted sources. Generated contents are never read
by the source hash walker.

`cargo test -p kettle adopted_scan_` exercises the Linux child-list scanner with
portable fixtures for work limits, deadlines, and continuation past live children.
Linux CI must also run the native exec teardown tests. The `kettle-state` test
`private_create_falls_back_without_clobbering_or_relaxing_permissions` injects
unsupported hard links and exercises the native atomic rename fallback,
no-clobber behavior, and private-directory checks.

Use these to isolate a failure or validate the area you changed:

| Recipe | What it checks; when to use it | Prerequisites |
| --- | --- | --- |
| `just test` | Workspace unit and integration tests | Rust and native build dependencies |
| `just clippy` | Warnings-denied workspace lint | Clippy and native build dependencies |
| `just core-default-features-check` | Core without features enabled by sibling crates | Clippy |
| `just media-protocol-test` | Stub worker and injected worker panic paths | Rust; builds test-only features |
| `just doc` | Workspace rustdoc with warnings denied | Rustdoc |
| `just shell-integration-check` | Executes shipped snippets and path fixtures | Python; installed native shells |
| `just live-ui-helper-selftest` | Harness parsing, readiness, and command-generation fixtures | Python; no desktop |
| `just native-titlebar-self-test` | Native checker admission and diagnostic fixtures | Python; no desktop |
| `just wayland-first-frame-self-test` | First-frame trace classification | Python; no compositor |
| `just vm-launcher-test` | QEMU launcher's command and network boundary | Python; no guest disk |
| `just script-fixture-self-test` | Live-probe timing and menu-capture ownership fixtures | Unix and Python; no desktop |
| `just deny` | License, source, and ban policy for both lock graphs | cargo-deny |
| `just audit` | RustSec scans of product and vendor graphs | cargo-audit and advisory database access |
| `just lru-scope` | Reviewed dependency path for the advisory exception | Python, Cargo metadata |
| `just dbus-scope` | Exact all-target D-Bus graph for maintenance-only exceptions | Python, Cargo metadata |
| `just shear` | Unused normal, dev and build dependencies; Rust files no module includes | cargo-shear |
| `just static-checks` | All five checks below, as the static-checks workflow runs them | The five tools below |
| `just zizmor` | Workflow analysis: injection, persisted credentials, cache poisoning, permissions | zizmor; offline |
| `just links` | Local Markdown links, images and heading fragments | lychee; offline |
| `just shell-lint` | Standalone shell scripts and the pre-commit hook at warning severity | ShellCheck |
| `just python-lint` | Python syntax, undefined names, unused imports and variables (`ruff.toml`) | Ruff |
| `just typos` | Spelling in code and docs, with the reasoned allowlist in `_typos.toml` | typos |
| `just coverage` | Advisory line coverage for the limit-heavy crates, under `target/coverage` | cargo-llvm-cov, llvm-tools-preview |
| `just mutants SHARD` | Advisory mutation testing of one `k/n` slice, under `target/mutants` | cargo-mutants; hours for a full run |
| `just sanitizer-test` | AddressSanitizer over Unix transport, state-file and PTY watcher tests | Linux x86_64, nightly-2026-09-24 with rust-src |

The live smoke recipes appear once in the [Live UI smokes](#live-ui-smokes)
table. Choose the row that matches the behavior you changed.
Do not run every interactive smoke for a documentation-only edit.

### What strict and full mean

`gauntlet` is the core Rust gate. It also runs the live-helper, titlebar,
Wayland-trace, shell, VM-launcher, and fuzz-workspace checks.
Its media subgate enables `test-worker` and `test-faults`, which ordinary
workspace tests do not enable.

`gauntlet-strict` adds the direct vendor and repository-integrity gates.
It does not prove that an installed application works on a desktop.
It does not require a published macOS update archive.

`gauntlet-full` adds the explicit native dependency list in Justfile.
Both Unix hosts check icons, package templates, release/package fixtures,
online-installer fixtures, offscreen GPU output, and CLI behavior.
Both also run touchpad, split-exit resize, text-presentation, and line-edit
smokes against real windows.

Linux adds isolated installer checks and the Xvfb launch smoke.
macOS adds the Xcode icon compiler, modal shortcuts, native titlebar, Dock menu,
agent CLI probes, and the comparator, standing, and A/V-sync self-tests.
Other-OS legs are reported as not applicable.
A current-OS pass still needs the other native CI legs before release.

### Host requirements and skipped tests

Use a non-root account. Root can bypass the permission checks under test.
Use an init process for container runs so killed fixture children get reaped.
Native PTY tests need permission to create Unix PTYs or Windows ConPTYs.
Restricted sandboxes can skip those tests.

The full workspace suite includes offscreen wgpu tests.
Most parser, state, and UI-decision tests need no GPU or child process.
An adapter-less host can skip GPU coverage, so inspect the test output.
A passing Cargo summary with a skip is reduced coverage.
Real-decoder tests in `kettle-media-native` and `kettle-media-worker` pass
without running when no trusted ffmpeg is installed. On Linux, the HEIF and
AVIF still tests also need ffmpeg 7.1 or later.
Native decoder deadline fixtures use isolated children with held input pipes
and a 30-second fail-safe; the tested deadline remains 300 ms.

Linux GPU tests need a Vulkan loader and a hardware or software adapter.
`libvulkan1` and `mesa-vulkan-drivers` provide the CI software path.
macOS uses Metal. Windows uses an available wgpu backend.
Offscreen tests need an adapter but do not need a visible display.
Live smokes need a usable window-system session as well.

For private-path and trusted-decoder changes, repeat focused tests under
umask `002`. Fixtures must create private roots explicitly rather than inherit
ambient permissions. Existing unrelated directories must keep their modes.

Report the exact command, revision, host, and skipped coverage in a PR.
Distinguish an offscreen render from a native window, and a fixture from an
installed-package check. Do not claim another platform passed from compilation.

## What the tests cover

The source is the coverage index. Test names state their contract.
Use the current test list instead of a count copied into documentation:

```sh
cargo test -p <crate> -- --list
cargo test -p kettle-core --lib -- --list
cargo test -p kettle-config --test documented_examples_parse
```

The first two commands list tests after Cargo prepares the test targets.
They can build code. Use source searches when you only need to inspect coverage:

```sh
rg -n '#\[test\]|#\[cfg\(test\)\]' crates/kettle-core/src
rg -n 'selection|scrollback|replay' crates/kettle-core/src/term.rs
rg --files crates/kettle-render/tests
```

Read the owning module and nearby tests before changing a behavior.
Unit tests usually live in the same Rust file as the implementation.
Larger suites use child test modules; integration tests live under `tests/`.
The table lists main homes, not every test file.
All paths below are relative to the repository root.

| Crate | Main test homes | Main boundary |
| --- | --- | --- |
| `kettle-vt` | `crates/kettle-vt/src/`, `crates/kettle-vt/tests/escape_prefix.rs` | Escape extraction, images, private protocols |
| `kettle-core` | `crates/kettle-core/src/term.rs`, `crates/kettle-core/src/`, `crates/kettle-core/tests/` | Grid, PTY lifecycle, selection, search, recording |
| `kettle-config` | `crates/kettle-config/src/`, `crates/kettle-config/tests/` | Parsing, defaults, keybinds, documented examples |
| `kettle-media` | `crates/kettle-media/src/`, `crates/kettle-media/tests/` | Bounded wire protocol, worker client, video sniffing |
| `kettle-media-worker` | `crates/kettle-media-worker/src/`, `crates/kettle-media-worker/tests/` | Process boundary, playback, native audio |
| `kettle-media-native` | `crates/kettle-media-native/src/`, `crates/kettle-media-native/tests/` | Decoders, sandboxing, audio output |
| `kettle-media-render` | `crates/kettle-media-render/src/`, `crates/kettle-media-render/tests/` | Raster, SVG, Mermaid, PDF, hostile inputs |
| `kettle-render` | `crates/kettle-render/src/lib.rs`, `crates/kettle-render/src/headless_tests.rs`, `crates/kettle-render/tests/` | Text, geometry, GPU pixels, menus |
| `kettle-ui` | `crates/kettle-ui/src/app.rs`, `crates/kettle-ui/src/app/`, `crates/kettle-ui/src/` | Input routing, windows, settings, control actions |
| `kettle-i18n` | `crates/kettle-i18n/src/`, `crates/kettle-i18n/tests/` | Locale selection and catalogue/build rules |
| `kettle-state` | `crates/kettle-state/src/` | Private state and filesystem policy |
| `kettle-ctl` | `crates/kettle-ctl/src/`, `crates/kettle-ctl/tests/pipe_instances.rs` | Local IPC, framing, permissions, liveness, the published JSON Schema |
| `kettle-update` | `crates/kettle-update/src/`, `crates/kettle-update/tests/` | Trust root, manifests, archive/update boundaries |
| `kettle-remote` | `crates/kettle-remote/src/` | Process ancestry and remote-session detection |
| `kettle-test-support` | `crates/kettle-test-support/src/`, `crates/kettle-test-support/tests/` | Shared fixtures, source scans, MSRV drift |
| `kettle` | `crates/kettle/src/`, `crates/kettle/tests/` | CLI, exec, MCP, source identity, native preview |

The `search_input` tests in `kettle-ui` alternate navigation and highlight
scans within one long wrapped line, including zero-width regex matches, and
check that output invalidates saved highlight offsets. They need no window or
GPU. In `kettle-core`, `shell_probe::cleanup_tests` uses a controllable child
on every platform to cover failed termination, polling errors, bounded cleanup,
and eventual background reaping. The Windows pipe tests still require a native
Windows runner.

The remote reconnect tests run offline without a remote daemon or cluster:

```sh
cargo test -j 2 --locked --offline -p kettle-remote reconnect_
cargo test -j 2 --locked --offline -p kettle-remote connection_environment_preserves_absence_and_rejects_incomplete_data
cargo test -j 2 --locked --offline -p kettle-remote reads_connection_environment_after_exactly_argc_arguments
cargo test -j 2 --locked --offline -p kettle-remote environments_share_the_scan_budget
cargo test -j 2 --locked --offline -p kettle-remote the_final_argument_area_read_obeys_the_scan_deadline
cargo test -j 2 --locked --offline -p kettle-remote deleted_client_fixture_has_no_descendants
cargo test -j 2 --locked --offline -p kettle-ui remote_reconnect_
cargo test -j 2 --locked --offline --manifest-path vendor/Cargo.toml -p portable-pty --features serde_support
cargo test -j 2 --locked --offline -p kettle-remote
```

`reconnect_refuses_unavailable_client_environment`,
`reconnect_falls_back_from_deleted_executable`, and
`reconnect_preserves_multicall_client_invocation` cover the three reconnect
regressions. Native Linux/macOS subprocess probes read the client's environment
through procfs or KERN_PROCARGS2, and portable fixtures cover filtering,
explicit absence, size limits, malformed values, relative connection paths,
and debug redaction. The macOS tests reject unreadable and SIP-redacted
captures. Both environment parser fixtures use Unix path semantics on every
host. Linux and macOS tests cover multiple clients sharing the aggregate byte
ceiling; a macOS clock fixture covers expiry during the final argument-area
read. Native capture also remains available from the snapshot after the client
exits. Linux exercises a running executable replaced on disk with bare `ssh`
as its invocation, while portable Unix fixtures cover replacement resolution
and preserved `argv[0]`. The copied Bash fixture blocks in a builtin on an open
pipe; a Linux/macOS regression checks that it creates no descendants. The UI
PTY probe checks the captured environment and original invocation in both a
split and an empty mux, without a window or GPU.
`remote_reconnect_replays_preserve_environment_and_invocation` repeats the
probe after duplicate pane/tab, split, undo-close, and a live tab move, then
checks another replay from each result. Session round-trip tests
`remote_reconnect_session_restore_falls_back_to_configured_shell` and
`remote_reconnect_environment_session_restore_omits_private_values` verify
shell fallback for invocation-only and environment-only settings and ensure
private values stay out of JSON. A separate UI regression
checks that an unavailable environment routes to the existing failure notice
without creating a pane. Ignored probe functions run only as subprocess fixtures. Native branches run on their respective CI hosts;
a macOS pass does not verify procfs or Windows. Windows keeps its existing
environment behavior.

### Drift guards and source scans

Some contracts cross files or cannot be exercised cheaply in every unit test.
Source-scan tests pin the production call path in addition to helper behavior.
A helper test alone cannot show that the UI still calls that helper.

Examples worth finding before a refactor:

- `crates/kettle-config/tests/documented_examples_parse.rs` checks documented
  configuration examples against the real parser.
- `crates/kettle-config/tests/docs_text_action_examples_parse.rs` checks
  documented text-action examples.
- `crates/kettle-config/tests/harness_action_names_resolve.rs` scans smoke
  scripts for literal action names and resolves them through production rules.
- `crates/kettle-ui/src/app/ui_text_drift_tests.rs` scans user-visible text
  sinks so new English literals do not bypass the translation catalogue.
- `crates/kettle-i18n/tests/build_gates.rs` checks catalogue/schema failures.
- `crates/kettle-test-support/tests/msrv_consistency.rs` keeps the declared
  Rust floor aligned with its CI job and related toolchain settings.
- `crates/kettle-test-support/tests/independent_hazards.rs` checks that
  test-only source cannot satisfy a production source guard.

Core and UI also pin lifecycle ordering, nonblocking teardown, input routing,
and redraw paths with source guards near their behavioral tests.
Use `rg -n 'source_guard|production_source|drift' crates` to find them.
When moving a function, read the guard's invariant before changing its matcher.
Do not make the scan pass by letting it match its own assertion or a comment.
Keep a behavioral regression when the contract can be exercised directly.

The tracked audit protects repository files rather than runtime behavior.
It checks path/case collisions, object identity, text hygiene, TOML/JSON,
local Markdown targets, and bounded font/image structures.
Its ledger is `target/diagnostics/tracked-files-audit.json`.
For staged release checks, the underlying script also accepts
`--require-clean-index`:

```sh
python3 scripts/audit-tracked-files.py --require-clean-index
```

### Features and separate workspaces

A workspace build unifies features. It can hide a core crate that accidentally
uses an optional dependency without declaring the feature.
That is why the gauntlet lints `kettle-core` alone with default features.

The media stub worker and panic injection are test-only feature legs.
Run `just media-protocol-test` after changing their boundaries.
Do not enable those features in a shipped binary to make a test pass.

The fuzz workspace has its own manifest and lockfile.
The vendor validation workspace also has its own manifest and lockfile.
Root workspace tests do not run those package-owned suites.
Use the recipes below rather than assuming `cargo test --workspace` covers them.

### Vendor coverage

[vendor/README.md](../vendor/README.md) describes the retained patches.
`just vendor-check` combines `vendor-parser-check`, `vendor-pty-check`, and
`vendor-clipboard-check`.
The parser gate tests and lints `vte` with `ansi` and `alacritty_terminal`.
The PTY gate uses `serde_support` for compatibility with serialized builders.
The clipboard gate runs display-free Linux URI-list tests, then the X11 INCR
paste test under `xvfb-run` when xvfb-run and xclip are installed. Other hosts
check formatting for that patch.

CI runs parser patches on Linux and portable-pty on all three native hosts.
Only Windows executes its ConPTY-specific code.
Only Unix executes its descriptor and process-group paths.
A local vendor pass covers only the current platform.

`cosmic-text` stays outside the vendor validation workspace.
The product build and audit cover it; renderer paragraph-separator tests cover
its patch. Consult the vendor README for its upstream validation notes.
Keep upstream formatting when editing a vendor patch.
Do not reformat unchanged vendor files to satisfy the root formatting gate.

## End-to-end harness: selection, copy and `.cast` replay

The terminal harness lives in
[crates/kettle-core/src/term.rs](../crates/kettle-core/src/term.rs).
`harness()` constructs a real terminal grid without a PTY or child process.
`feed()` drives the processor; `feed_ex()` first drives Kettle's extractor,
then feeds pass-through bytes into the processor.
Use `feed_ex()` when the regression depends on extraction as well as VT state.
These tests need neither a desktop nor an authenticated client.

Run the selection and replay contracts:

```sh
cargo test -p kettle-core --lib selection_while_scrolled_reads_visible_row_not_active_screen
cargo test -p kettle-core --lib simple_drag_selection_while_scrolled_copies_visible_rows
cargo test -p kettle-core --lib replays_asciicast_v2_output_into_grid
cargo test -p kettle-core --test diagram_copy
```

Selection tests distinguish viewport coordinates from grid/history coordinates.
A scrolled viewport must copy the visible rows, not the active screen's rows.
The UI applies the display offset before constructing the grid selection.
Keep copy text and highlight geometry consistent after scrolling or resizing.

The `.cast` test parses asciicast v2 output events and replays their bytes.
It checks grid text and SGR state. The neighboring recorder round-trip test
also writes a trace through the real recorder before reading it back.
See [RECORDING.md](RECORDING.md) for the recording format and user controls.

To turn an interactive failure into a regression:

1. Reduce the output to the bytes needed to show the fault.
2. Remove private paths, prompts, credentials, and unrelated session text.
3. Choose the smallest grid and history that still reproduce it.
4. Feed the bytes, including meaningful read boundaries.
5. Assert the resulting text, selection, style, reply, or bounded state.
6. Include a nearby negative case if the fix recognizes a special pattern.

Do not commit a private terminal recording as a shortcut.
A hand-authored trace is enough when only protocol bytes matter.
The diagram-copy fixtures live in
`crates/kettle-core/tests/fixtures/diagram-copy/`.
Their README explains the captures and expected copy projections.

This harness proves terminal state, not compositor presentation or clipboard
ownership. Use a live smoke for native input, desktop paste, window routing,
or pixel evidence. A real PTY regression remains necessary for lifecycle,
foreground process-group, EOF, and child cleanup behavior.

When diagnosing a replay failure, compare the raw output with the projected
grid first. Then check scroll offset, resize/reflow, and selection endpoints.
Avoid replacing exact expected text with a loose substring merely to get green.

## Diagram gate

`just mermaid-check` runs [scripts/check-mermaid.py](../scripts/check-mermaid.py)
on Mermaid fences in tracked Markdown.
It renders them with Mermaid CLI 11 and Chrome/Chromium.
A parser error fails the gate.

List affected documents without rendering:

```sh
python3 scripts/check-mermaid.py --list
```

For a required local rendering check:

```sh
KETTLE_MERMAID_REQUIRED=1 just mermaid-check
```

Without required mode, missing npx or a browser prints a skip and exits
successfully. CI sets required mode on Linux.
`PUPPETEER_EXECUTABLE_PATH` or `CHROME_PATH` can select an installed browser.
The helper can fetch the CLI through npx, so plan for that dependency.

In sequence messages, escape a literal semicolon as `#59;`.
Inside quoted node labels, use `&quot;` for an embedded quote.
Keep labels short enough to read in the rendered document.
This documentation gate is separate from the media renderer's Mermaid tests.
The renderer checks journey/treemap label contrast against actual pixels,
authored-style output preservation, unchanged geometry and fonts, and fixed
canvases independent of the pane palette. Native card checks remain separate
from these portable rendering regressions.

## Fuzzing

The byte-processing contracts have deterministic in-process sweeps and
cargo-fuzz entry points. The sweeps run with ordinary crate tests.
The libFuzzer targets live in [fuzz/](../fuzz/) with a separate lockfile.
They do not add libFuzzer to the product graph.
Native render and ffprobe coverage runs on Linux and macOS, not Windows.

[targets.json](../fuzz/targets.json) defines the schedule and seed crates.
[scripts/fuzz-plan.py](../scripts/fuzz-plan.py) turns that table into the
workflow matrix and rejects names that differ from `fuzz/Cargo.toml`.
The times below are minutes per target, not total workflow duration.

| Target | Seed crate | Contract | Nightly / Sunday |
| --- | --- | --- | --- |
| `term` | `kettle-core` | PTY parsing, resize, history, selection, search, and replies stay bounded | 20 / 60 |
| `vt` | `kettle-vt` | Whole and chunked extraction agree; images and accounting stay bounded | 20 / 60 |
| `images` | `kettle-vt` | Image decoders, completion metadata, and inline markers obey their limits | 10 / 10 |
| `wire` | `kettle-media` | Directional decode budgets, round-trip bytes, and stream decoding agree | 20 / 20 |
| `session` | `kettle-media` | Session pixels match the requested size and decoded bytes | 10 / 10 |
| `sniff` | `kettle-media` | Video and HEIF/AVIF still prefix sniffing handles arbitrary bytes and declared lengths | 10 / 10 |
| `render` | `kettle-media-render` | Worker renders fit the box and satisfy the parent's reply checks | 20 / 60 |
| `pdf` | `kettle-media-render` | A PDF page, at the page each input picks, passes the same checks | 20 / 60 |
| `ffprobe` | `kettle-media-native` | Descriptions, rates, rotation, codecs, and packet indexes parse safely | 10 / 10 |
| `ctl` | `kettle-ctl` | Request framing and serialization agree; pages stay bounded | 10 / 10 |
| `update` | `kettle-update` | Signatures, asset names, hashes, and archive paths obey trust rules | 10 / 10 |
| `config` | `kettle-config` | BOM/line endings preserve parsing; keybinds and colors round-trip | 10 / 10 |

### What CI runs

[.github/workflows/fuzz.yml](../.github/workflows/fuzz.yml) uses
`nightly-2026-09-24` and cargo-fuzz 0.13.2.
It runs at 07:41 UTC daily.
Sunday uses each target's Sunday time and minimizes every corpus.
Only `term`, `vt`, `render`, and `pdf` get the longer Sunday duration.

A matching PR runs every target for 60 seconds.
The PR path filter covers the workflow, `fuzz/**`, the planner,
`crates/kettle-test-support/src/mutate.rs`, and crate fuzzing modules.
It does not trigger on every ordinary parser source edit.

Manual dispatch accepts space-separated target names; empty means all targets.
The default duration is 3,600 seconds per target.
Positive requested durations are capped at 18,000 seconds.
The planner rejects unknown target names and invalid durations.
Each target job gets its requested runtime rounded up to minutes plus
30 minutes for build, seed, and minimization overhead.
At most six target jobs run in parallel.

Each run restores a prior corpus and adds deterministic sweep seeds.
`KETTLE_FUZZ_CORPUS` names the seed output root.
A matching file in `fuzz/dict/` supplies a target dictionary.
The run uses these libFuzzer limits:

- Maximum input: 262,144 bytes.
- Resident-memory limit: 2,560 MiB.
- Per-input timeout: 30 seconds.

PRs consume cached corpora but do not save them.
Scheduled and dispatched runs save corpora, including after a failure.
Failed targets upload finding inputs from their artifact directory for 30 days.
A failed scheduled plan or fuzz job opens or updates one tracking issue.
A failure without an input artifact may be a planning, build, or seeding error.

### Run locally

Use `just fuzz-check` for the stable format/lint gate on Unix.
Use `just fuzz-guard` after changing target registration or feature wiring.
Those checks do not replace a timed sanitizer run.
On Windows, `fuzz-check` only prints that the targets are Unix-only.

For a timed run, install the workflow's pinned nightly and cargo-fuzz version.
The following example seeds the media targets and runs `render` for five minutes:

```sh
cargo install cargo-fuzz --version 0.13.2 --locked
KETTLE_FUZZ_CORPUS=$PWD/fuzz/corpus cargo test --locked -p kettle-media -p kettle-media-render -p kettle-media-native --lib fuzzing
cargo +nightly-2026-09-24 fuzz run render fuzz/corpus/render -- -dict=fuzz/dict/render.dict -max_total_time=300 -max_len=262144 -rss_limit_mb=2560 -timeout=30
```

For another target, seed its crate from the table and select that target.
Use its dictionary only if the matching dictionary exists.
Reproduce a saved finding with the same pinned nightly:

```sh
cargo +nightly-2026-09-24 fuzz run <target> <input>
```

`<target>` is a table entry; `<input>` is the saved finding file.
Keep the input and error report while reducing the case.
Add a focused regression through the ordinary crate tests after fixing it.
A successful short local run does not prove an input space is safe.

## Supply-chain fixture gates

Installer and release work needs fixture checks as well as Rust updater tests.
These suites use temporary repositories, fake services, or synthetic archives.
They do not publish releases or require production signing credentials.
Run the relevant recipe while iterating, then strict and full before release.

`cargo test -p kettle-update` also exercises real loopback HTTP responses:
error statuses, oversized declared and chunked bodies, truncated bodies,
and exact signed artifact sizes. These transport checks supplement signature
and archive validation; they do not prove public HTTPS availability.

| Recipe | What it protects |
| --- | --- |
| `just update-manifest-test` | Signed-feed bytes, trust root, sidecars, signing command wiring |
| `just release-assets-test` | Exact draft asset names, sizes, and SHA-256 binding |
| `just release-script-test` | Release preparation, version/document updates, immutable historical references |
| `just package-manifest-test` | Inner manifests, Linux packing at the 128-entry limit, bounded extraction, media installation |
| `just online-installer-test` | Online install parsing, implied parent directories, transport limits, no downgrade, hostile archives |
| `just package-templates` | Deterministic Homebrew/AUR template rendering and optional published checks |
| `just icons-check-required` | Generated icon geometry, rasters, and formats with tooling required |
| `just icns-smoke` | macOS release icon assets through Xcode actool |
| `just linux-installer-smoke` | Real install/uninstall paths in isolated prefixes |

### Fixture commands

The main Python suites can also run directly:

```sh
python3 scripts/test-update-manifest.py
python3 scripts/test-verify-release-assets.py
python3 scripts/test-release.py
python3 scripts/test-package-manifest.py
python3 scripts/test-install-media.py
python3 scripts/test-install-online.py
```

Run all of them after changing signer/publisher boundaries, installer parsing,
archive handling, or release metadata.
Release fixtures need Git and the tools used by the mocked command paths.
Online-installer coverage needs a supported Linux/POSIX host and GNU tar;
its real transport fixtures need curl and openssl.
The online-installer suite also exercises installed curl against a local HTTPS
server. It tests bounded retries and file-size enforcement without remote data.
Check its output for unavailable native prerequisites.

The package suite checks path traversal, links, collisions, metadata bounds,
case aliases, and safe extraction into a newly created output root.
It accepts aliases in an existing parent only after canonicalizing that parent.
It must still reject a pre-planted output-root link.
Do not weaken these tests to make an unusual temporary directory pass.

On macOS, the update-manifest suite also tests the native keychain helper
against disposable keychains. It does not rewrite the developer's user search
list. Other hosts cannot prove that native helper works.
Mock signing fixtures check argument order and paths containing spaces;
they do not establish notarization or Gatekeeper acceptance.

### Real packages and published assets

`just linux-installer-smoke` builds the release binary and worker, then uses
isolated prefixes. It checks local development and release-tarball layouts,
recording-path validation, ownership markers, uninstall safety, and assets.
When the matching tag and asset exist, it also exercises the online installer.
A not-yet-published asset can skip that online part.
Record that separately from the local install/uninstall result.

`just package-templates` checks source rendering on a feature branch.
At an exact clean release tag, auto mode also checks published assets.
The underlying script's `--require-release` makes that check mandatory:

```sh
scripts/check-package-templates.sh --require-release
```

This mode needs the release to exist and needs network access.
A source-only template pass is not publication proof.

`just macos-update-smoke` downloads or reuses a published notarized archive,
checks its sidecar, and runs the real bundle updater with codesign and spctl:

```sh
just macos-update-smoke
```

The script sets `KETTLE_MACOS_ARCHIVE_REQUIRED=1` itself.
A missing archive fails on macOS; other hosts print a skip.
The recipe accepts a tag argument; empty selects the latest release.
It also accepts an existing archive path with its sidecar beside it, so a
notarized candidate can be tested before publication.
This gate is outside `gauntlet-full`; its default route needs network access.
It needs Rust, gh for downloads, and native macOS signing-verification tools.
Synthetic update tests cannot replace it.

### Release trust boundary

The release workflow separates the protected signer from the publisher.
The signer has the Ed25519 secret and read-only repository permission.
The publisher has repository write permission and no signing secret.
Before publishing, it verifies the signature, binds local archives to the
signed manifest, regenerates package metadata, and verifies the remote draft.

Keep these checks in the fixture suites when changing either job.
A successful archive build alone does not prove the published set is complete.
The release and upgrade jobs are mapped in [CI](#ci).

## Live UI smokes

Use an isolated desktop and a disposable application instance.
The Python harness starts its own instance with a full control server.
It saves geometry, screen/cell data, logs, and screenshots as each case needs.
Default Unix artifacts go under `target/diagnostics`.
Some captures contain terminal text or local paths; inspect them before sharing.

Most recipes below build or select a release binary.
`--cargo-release` uses Cargo's reported executable, including a custom target
directory or configured target triple.
A direct `--kettle` invocation uses exactly the binary you supply.
Record its revision and build identity.

### Prepare the desktop

Linux needs an X11 or Wayland session and a working GPU backend.
Xvfb can cover windowed control paths with software rendering.
It cannot establish physical input, monitor behavior, or native Wayland behavior.
Tests that need focus may need a window manager as well.

macOS needs an unlocked Aqua console.
Native event injection, accessibility, and screen capture may require grants
for the process launching the test.
A locked or unavailable desktop fails the Python harness preflight.
It is not a successful skip.

Clipboard tests replace the desktop clipboard.
Linux bitmap paste needs `wl-copy` on Wayland or `xclip` on X11.
Video receipt tests need ffmpeg to generate their default fixtures and a usable
media worker for native posters.
Follow the existing worker installation/signing path in
[INSTALL.md](INSTALL.md); a plain unsigned sibling is not macOS release proof.

### Smoke map

This is the single index of live smoke names and recipes.
"Linux/macOS" describes the maintained recipe path in this snapshot.
Some helper code retains Windows branches, but those are not supported native
release acceptance. A dash means no dedicated just recipe exists.
Prerequisites beyond the desktop are included in the proof column.

| Name | Just recipe | What it proves or captures | Platform |
| --- | --- | --- | --- |
| Live renderer | `just live-render-smoke` | Grid cursor blink changes only a cursor-sized region | Linux/macOS |
| Agent TUI | `just agent-tui-smoke` | Shell, installed clients, tmux, and clean/configured Neovim states; optional clients can skip | Linux/macOS |
| Agent CLI | `just agent-cli-smoke` | Mandatory PTY/JSON/MCP probes plus available client version/help checks; no composer attachment proof | Linux/macOS; required in macOS CI |
| Native titlebar | `just native-titlebar-smoke` | Light caption, traffic-light separation, resize, native fullscreen and restoration | macOS |
| Modal shortcuts | `just modal-shortcuts-smoke` | Command chords do not act as ordinary modal letters | macOS |
| Interaction | `just interaction-smoke` | Multiline input, wheel, selection/copy, tabs, context-menu split, and screenshots | Linux/macOS |
| Selection autoscroll | `just selection-autoscroll-smoke` | Held selection scrolls at both vertical pane edges; native pointer on macOS, control path on Linux | Linux/macOS |
| Image paste receipt | `just image-paste-receipt-smoke` | Bitmap paste and expanded, compact, hover-expanded receipts; replaces clipboard | Linux/macOS |
| Video paste receipt | `just video-paste-receipt-smoke` | Two pasted video files produce bounded poster receipts; needs ffmpeg and worker | Linux/macOS |
| Hover wheel | `just hover-wheel-smoke` | Wheel targets hovered pane; no swapchain screenshot support required | Linux/macOS |
| Window close isolation | `just window-close-isolation-smoke` | One detached child's exit leaves the sibling OS window and pane alive; Linux needs xdotool | Linux X11/macOS |
| Touchpad scroll | `just touchpad-scroll-smoke` | Fractional wheel deltas accumulate into scrolling through the real conversion path | Linux/macOS |
| Tabbar click | `just tabbar-click-smoke` | Multi-tab click state with PNG and tab geometry evidence | Linux/macOS |
| Pane drag | `just pane-drag-smoke` | Intended pane reorder gesture; currently known broken because ctl input cannot arm native drag | Linux/macOS |
| Tear-off | `just tearoff-smoke` | Control-plane detach/broadcast, then real pointer tear/follow/redock/cancel on X11 | Linux/macOS control tier; X11 live tier |
| Tab title | `just tab-title-smoke` | Cwd-derived title recovers from shell truncation | Linux/macOS |
| Split titlebar | `just split-titlebar-smoke` | Cwd titles at both pane edges and focused/receiving/inactive colors | Linux/macOS |
| Text presentation | `just text-presentation-smoke` | Text-style bullet uses a monochrome face | Linux/macOS |
| Update toast | `just update-toast-smoke` | Presented-frame gating, seven-second expiry, terminal preservation, Details/About, dismissal and once-per-state restart receipts | Linux/macOS |
| Split exit resize | `just split-exit-resize-smoke` | Exiting split restores survivor grid and tty winsize | Linux/macOS |
| Steady uploads | `just steady-uploads-smoke` | Unchanged frames and cursor blink avoid GPU uploads; paced output uses mapped writes where available | Linux/macOS |
| Cursor blink layer | `just cursor-blink-layer-smoke` | Native layer handoff, idle behavior, and exits; `--pixels` needs Screen Recording | macOS |
| Job control | `just job-control-smoke` | Shell editing across suspend/bg/fg/exit, alternate screen, and negative controls | Linux/macOS |
| Zoom keybind | `just zoom-keybind-smoke` | App zoom chords resolve and change geometry | Linux/macOS |
| Alt-arrow zoom | `just alt-arrow-zoom-smoke` | Focus actions fall through at edges or hidden neighbors and dispatch for visible neighbors | Linux/macOS |
| Zoomed layout keys | `just zoomed-layout-keys-smoke` | Pane-focus chords reach a Kitty-owning client when zoom hides destinations; unzoom remains owned | Linux/macOS |
| Pane focus reports | `just pane-focus-reports-smoke` | DEC 1004 reports follow keyboard focus once per change | Linux/macOS |
| Color scheme | `just color-scheme-smoke` | DEC 2031 subscriber receives light/dark changes | Linux/macOS |
| Theme picker | `just theme-picker-smoke` | Preview, Esc restore, Enter persistence, Settings and menu entry points | Linux/macOS |
| Remote links | `just remote-links-smoke` | Shift-picked missing path bypasses confirmation; tmux leg skips if unavailable | Linux/macOS |
| Raw paste | `just raw-paste-smoke` | Multiline paste into an unbracketed shell proceeds; replaces clipboard | Linux/macOS |
| Program keys | `just program-keys-smoke` | Client-owned Shift+Left reaches Kitty client, then returns to split resize | Linux/macOS |
| Search selection | `just search-selection-smoke` | Open search bar preserves grid selection, copy, menus, focus, shortcuts, and Esc | Linux/macOS |
| Bell flash | `just bell-flash-smoke` | Only ringing pane brightens, then pixels return to baseline | Linux/macOS |
| Default window size | `just default-window-size-smoke` | Monitor-fitted defaults and explicit sizing; needs a sufficiently wide monitor | Linux/macOS |
| Line edit chords | `just line-edit-chords-smoke` | Text-action backspace, macOS default chord, and search word deletion | Linux/macOS |
| Dock menu | `just dock-menu-smoke` | Real Dock rows, window list, and New Window effect; accessibility and no other Kettle instance | macOS |
| Live stress | `just stress-live [S1 … S10]` | Floods, resize storms, 101 tabs and 151 panes, huge pastes and titles, config churn, hostile media and 64 control clients leave Kettle alive and answering; S6 replaces the clipboard, then restores it; S9 needs a working worker | Linux/macOS |
| Underline scroll | `just underline-scroll-smoke` | Repeated git-diff/delta scrolling preserves underline pixels; needs git, delta, less | Linux/macOS |
| Search history | - | Scrollback search status, cells, and highlight pixels; direct helper case | Linux/macOS; Linux CI quarantined |
| Native image paste routes | - | Native paste shortcuts, PRIMARY/middle-click, managed image path, and optional real composer attachments | Linux/macOS |
| Launch-scoped cards | - | Simulated agent TTY launches, receipt privacy, card pixels, exit, and shelf fallback | Linux |
| Centered media viewer | - | Worker-backed viewer routing and live pixels through native X11 input | Linux X11 |
| Centered confirmations | `just confirmation-smoke` | Two-pane close, safe Cancel, pointer/Space release, scrolling, retained Settings, visible text, and input isolation | Linux X11 |

S9 checks held pixels for good media, refusals for broken inputs, and metadata-only
cards for unsupported or oversized files. The worker and its parent directory
must not allow group or other writes; stage a private executable copy if the
build used a shared-write umask. On macOS the worker also needs the expected
Developer ID signature. Record an unavailable worker as an unrun gate.

Confirmation acceptance checks the centered panel and button bounds, initial
Cancel focus, Enter-repeat isolation, Space activation on release, explicit
confirmation, and blocked outside clicks. Pressing a button and releasing
outside it or over a different button must leave the question open. Exercise
a two-pane close request, scrolling in a short window, and an actual keybinding
conflict opened over Settings. Cancellation must preserve both panes and the
configuration. Geometry tests cover narrow windows, translated labels, and
scaled text. Native screenshots must show heading and button text; GPU checks
verify retained Settings beneath the dimming. The accessibility projection must
contain only the modal controls and restore Settings after cancellation.
The visibility checker must reject a filled button without label pixels. A
native Kitty recorder verifies a held Space release, and SGR mouse reports
verify that opening the dialog ends each of five held buttons exactly once.

### Run a helper directly

For cases without a just recipe, use an existing release binary:

```sh
python3 scripts/check-live-ui-smoke.py --kettle ./target/release/kettle search-history
python3 scripts/check-image-paste-parity.py --kettle ./target/release/kettle
```

On Linux, isolate native paste routes from your normal clipboard with Xvfb:

```sh
env -u WAYLAND_DISPLAY xvfb-run -a python3 scripts/check-image-paste-parity.py --kettle ./target/release/kettle
```

That helper needs xclip, X11 utilities, and libXtst on Linux.
Its optional `--codex` argument selects an installed client.
CI supplies a checksum-pinned Codex 0.155.1 binary and an offline provider.
The test waits for the actual composer, adds attachments, and submits no prompt.
The mandatory recorder client proves terminal routing separately.
A version/help probe does not prove that a real composer accepts an attachment.

Linux card and viewer checks use independently built workers.
Run both helpers inside an isolated X11 server to protect your desktop and clipboard:

```sh
env -u WAYLAND_DISPLAY xvfb-run -a python3 scripts/check-agent-cards-smoke.py --kettle ./target/release/kettle --worker ./target/media-worker/kettle-media-worker --out target/diagnostics/agent-cards
env -u WAYLAND_DISPLAY xvfb-run -a python3 scripts/check-media-viewer-smoke.py --kettle ./target/release/kettle --worker ./target/media-worker/kettle-media-worker --out target/diagnostics/media-viewer
env -u WAYLAND_DISPLAY xvfb-run -a python3 scripts/check-confirmation-smoke.py --kettle ./target/release/kettle --out target/diagnostics/confirmations
```

The card fixture needs Rust, Python, and Node already installed.
It also needs FFmpeg to make its JPEG, WebP, animated GIF, and MP4 fixtures,
and xclip for its private clipboard.
It uses simulated Codex 0.163 and Claude Code 2.1.303 harnesses and makes no
model calls. Both native profiles must render and open PNG, JPEG, WebP, GIF, SVG,
Mermaid, Markdown gallery, PDF, video, and generic-file cards. The npm profile
and unscoped shelf fallback use SVG. Separate SVG cases retain Codex 0.162.0
and Claude Code 2.1.280 coverage, and simulate later minor versions to check
minimum-version admission. Every launch must clean up its cards on exit.
The native Codex fixture also checks preview context menus, keyboard dismissal,
image/frame/page and source copying, playback, pinned previews, and menus in narrow previews.
The viewer helper checks distinct PDF pages, Home/End and first/last page
boundaries, and Markdown gallery paging in addition to focus, pan, and pin.
Control-API clicks must open the centered preview's menu and dismiss the viewer
without changing the terminal.
Xvfb fixture success does not establish official-client or GNOME Wayland
acceptance. The viewer helper uses native X11 input.

### Read the result

The helper's `all` case does not include every scenario.
Dock, clipboard receipt, selection-autoscroll, hover-wheel, and window-isolation
cases have explicit opt-ins. Use the table instead of treating `all` as a gate.

`just stress-live` runs every scenario when given no names. Each one must
leave Kettle alive with no panic in its log, answering `get_state` within 2 s
and echoing a typed command within 1 s. The script's docstring lists the
scenarios; `--self-test`, part of `just script-fixture-self-test`, checks the
pass criteria without a desktop.
The tab scenario must reach 101 tabs and 151 panes and close back to one of
each. Run it with enough process file handles, for example `ulimit -n 2048`
in the test's launch shell. A reduced workload fails the gate.

The tear-off recipe has two tiers. Its shell helper skips native pointer work
outside X11. The control tier alone cannot prove the native drag gesture.
The pane-drag recipe is explicitly known broken in Justfile.
Do not count its expected failure as a pass.

The steady-upload case needs a focused window for blink and output phases.
`--screenshots-only` runs the narrower screenshot phase for environments such
as Xvfb without a window manager. Record that restriction.
A screenshot-capability failure is distinct from a rendering failure.

Inline-card GPU tests print `INLINE_CARD_GPU_ACCEPTANCE` for actual acceptance
and `INLINE_CARD_GPU_SKIPPED` when the adapter cannot exercise it.
Require acceptance for both Grid and Legacy cases you claim to have tested.
Headless readbacks still do not prove native input or installed-package behavior.

The `regression_` tests in `kettle-render` and `kettle-ui` cover close-chip
contrast with an overridden chrome background, pane and lane dimming order,
single dimming of terminal decorations, inactive color emoji dimming with an
ordinary-text contrast floor in Grid and Legacy, picker pointer routing, translated
theme queries and scroll-arrow containment in subpixel strips. The headless
pixel cases print a skip when no GPU adapter is available; portable colour,
geometry, draw-order and picker interaction tests still run. Report those
skips separately from pixel acceptance. Picker interaction tests exercise
all four modal projections without a native window.
The emoji pixel cases also skip explicitly when no color emoji font is available.

Portable renderer checks cover color-glyph dimming material, shader inputs and
cache invalidation on focus or dimming changes. Animation stills tests use
distinct APNG frame colors and fractional delays to verify that selection keeps
frame starts unrounded while returned timestamps use milliseconds.

### Authenticated agent and configured editor checks

The default agent TUI smoke uses version/help probes for installed agents.
For real marker prompts and the Claude diff-panel probe, explicitly opt in:

```sh
KETTLE_AGENT_AUTH_SMOKE=1 just agent-tui-smoke
```

This path uses installed client credentials and makes real model calls.
Confirm access before running it; its result is separate from offline coverage.
For configured Neovim/AstroNvim, the direct helper accepts `--astro-config` and
`--nvim-data`. Supply your installed configuration/runtime directories.
The helper copies them into an isolated sandbox before use.
Keep the provenance record and distinguish clean from configured editor results.

## Manual / interactive checks

Use these for behavior that fixtures or synthetic input cannot establish.
Run the relevant automated smoke first; do not repeat its scripted steps here.
Use the exact release bundle for release acceptance.
Record the OS, display backend, GPU, renderer mode, scale, and application build.
Keep failures and skipped areas with the evidence.

### Terminal applications and physical input

- [ ] Use your actual editor, pager, multiplexer, and agent TUI with its normal
  configuration. Check repaint, mouse use, icons, truecolor, and undercurl.
- [ ] Try physical Enter and modified Enter, word/line deletion, selection,
  paste, and held keys. Confirm modal dismissal does not leak a repeat.
- [ ] Check real client focus and theme changes. Synthetic recorders cannot
  prove that a third-party client interprets the reports correctly.
- [ ] Use an installed terminal conformance tool for VT screens that matter
  to the change. Record the tool and failing screen, not just "VT checked."

### Selection, search, and clipboard ownership

- [ ] Drag beyond each pane edge with the physical pointer. Check autoscroll
  speed, reverse direction, release outside the pane, and exact copied text.
- [ ] Inspect search highlights in long scrollback, soft-wrapped text, wide
  characters, and a narrow window. Check the physical shortcuts you use.
- [ ] Paste through the native menu, keyboard, and primary/middle-click routes
  available on the host. Check interoperability with another desktop app.
- [ ] Check desktop clipboard lifetime after the source application exits.
  A control-plane paste does not establish native clipboard ownership.

### Images, cards, and video

- [ ] Display real Sixel, Kitty, and iTerm2 images in split panes. Scroll,
  resize, change scale, and cross screen modes; inspect clipping at every edge.
- [ ] Put pane titlebars at both top and bottom. Images must stay out of
  padding, titlebars, sibling panes, and window chrome.
- [ ] Use the supported installed clients for inline cards and attachment
  receipts. Check pending, ready, unavailable, compact, and hover states.
- [ ] On Linux, check launch-scoped cards on the actual GNOME Wayland desktop.
  The simulated agent fixture does not prove official-client acceptance.
- [ ] Run `kettle show --inline` on a PNG, an SVG and a Mermaid file and click
  each card. Then pipe it (`| cat`), run it as a background job and in a pane
  too narrow for a card: each must put the item on the shelf and say why.
- [ ] Play and seek video through the shipped worker. Listen for audio, check
  pause/resume, output-device changes, and recovery after a device disappears.
- [ ] Page through a long PDF with `‹` `›`, Page Up/Down and Home/End, then open
  it outside Kettle: Preview on macOS, Papers or Evince on GNOME. The ignored
  `preview_opens_a_marked_copy_live` and `papers_opens_a_copy_live` tests in
  `kettle-ui` open a one-page PDF copy for you to look at.
- [ ] Show a file Kettle does not draw, such as a `.docx` or an MP3, and use
  Show in Finder (macOS) or Show in Folder (GNOME) from its card's menu and the
  viewer's `↗`. The file manager must open its folder with that file selected.
- [ ] Show an iPhone HEIC photo, which is a grid of tiles, and an AVIF: on
  macOS, and on Linux with ffmpeg 7.1 or later. With an older ffmpeg each must
  show as a file card whose viewer says which decoder is missing.

### Window system and appearance

- [ ] Move between monitors and scales. Inspect text, cursor, split borders,
  titlebars, menus, and the first content row after resize or fullscreen.
- [ ] Check real minimize/restore, occlusion, window close, and focus changes.
  Verify other OS windows survive and keep working.
- [ ] Drag and tear off tabs with the native pointer, redock them, and cancel
  a gesture. Control-plane movement alone does not test that gesture.
- [ ] On macOS, inspect decorated and borderless windows with blur on/off,
  then toggle Reduce Transparency. Check corners, seam, traffic lights,
  caption, and drag region by sight.
- [ ] Inspect Finder and Dock icons from the exact packaged app.
  Compilation or an offscreen icon image is not installed appearance proof.

Use [APPEARANCE-GATE.md](APPEARANCE-GATE.md) for the release capture contract.
Keep matching-commit icon artifacts when the local Xcode version cannot compile
release assets. Borrowed CI assets do not count as local compilation.

### Shell integration and completion

- [ ] Use your configured shell with the shipped snippet from
  [SHELL-INTEGRATION.md](SHELL-INTEGRATION.md). Check cwd and prompt navigation.
- [ ] Exercise your plugins, custom bindings, Vi mode, Unicode candidates,
  directory completion, cycling, dismissal, and redraw with a long command.
- [ ] Confirm completion neither opens a competing pager nor changes a custom
  binding unexpectedly. Native fixture success covers only its supplied setup.

### Unix suspend/resume compatibility

`just job-control-smoke` covers offline suspend/resume fixtures.
Use the real client only when checking actual client behavior.
Choose its installed executable explicitly:

```sh
python3 scripts/check-job-control-smoke.py --kettle ./target/release/kettle --codex /absolute/path/to/codex --shell zsh --configured-shell --cycles 100
```

`/absolute/path/to/codex` is a placeholder for your installed executable.
The opt-in check uses local client configuration and authentication.
It starts an idle session and submits no model prompt.
A trust or login dialog fails readiness; the harness does not approve it.
Use a trusted working directory and inspect failure text before sharing it.

- [ ] Type and edit before launch, while suspended, after background/foreground
  transitions, and after exit. Include the main and alternate screens.
- [ ] Check your real shell/editor combination and split layout. Preserve
  binary versions and launch settings when assigning a fault to Kettle/client.

The harness uses PTY/control input rather than physical keyboard events.
Check physical chords separately when native event routing is the suspected
failure. Unix job control has no equivalent Windows acceptance leg.

### Automation surfaces and real integrations

- [ ] Connect the actual agent/MCP client and check permissions, refusal text,
  disconnect/reconnect, and the UI result of its requested operation.
- [ ] Inspect redaction and user-facing receipts before publishing evidence.
  A passing MCP self-test does not prove the host client's configuration.
- [ ] Check real SSH/tmux nesting and local versus remote path opening in a
  disposable session. The fixture cannot model every remote desktop policy.

See [AGENT.md](AGENT.md) for the control and agent surfaces.
Keep large request/response examples there rather than duplicating them here.

### Responsiveness and release packages

- [ ] During sustained output, resize, select, switch tabs, type, and close a
  pane. Check perceived responsiveness and cleanup on the real desktop.
- [ ] Test installed Ubuntu and macOS release packages with the relevant
  manual areas above. Record the actual package identity.
- [ ] Check post-update launch, native signing, desktop integration, and
  uninstall on the target host when those paths changed.

Use [PERFORMANCE.md](PERFORMANCE.md) for measurement tools and limitations.
Do not infer a latency or memory improvement from a responsive-looking demo.
The owner-controlled standing method has its own publication gates below.

## CI

This map describes the checked-in workflow definitions, not a claim that a
particular revision has passed them. Open the linked YAML for exact commands,
path filters, permissions, and artifact retention.
All schedule times below are UTC.
A path-filtered workflow may not run for a source-only change outside its filter.

### Core matrix: `ci.yml`

[.github/workflows/ci.yml](../.github/workflows/ci.yml) runs on every PR and push
to `main`. Its jobs are:

| Job ID | Host | What it runs |
| --- | --- | --- |
| `fish-shell-integration` | Ubuntu 24.04 | Shell fixtures with Fish 3.7.x, 4.2.1, and 4.8.1; Apple payload fixture |
| `macos-26-release-icon` | macOS 26 | Release AppIcon compilation with Xcode 26.x and retained assets |
| `build` | Ubuntu, macOS, Windows latest | Stable workspace and native checks described below |
| `msrv` | Ubuntu latest | Locked workspace build and tests with Rust 1.95 |
| `nightly` | Ubuntu latest | Workspace build/tests with current nightly; nonblocking early warning |
| `aarch64-linux` | Ubuntu latest | ARM64 GNU cross-build; nonblocking early warning |

The `build` matrix runs formatting, locked workspace build, warnings-denied
Clippy, and tests. It adds the media stub-worker and panic-fault feature legs.
It validates Justfile parsing and current-platform recipe visibility.
It runs portable-pty tests/Clippy on each native OS, shell snippets in installed
native interpreters, and the live-helper and Wayland-trace self-tests.

The `build` job allows 60 minutes on Linux and Windows and 90 minutes on macOS.
The macOS budget covers cold-cache compilation, native helper suites and both
optimized binaries. Its agent CLI smoke uses the release binary already built
in that job, invoking the same script as `just agent-cli-smoke` without a second
release build. All mandatory PTY, JSON and MCP probes remain enabled.

Linux adds these checks inside `build`:

- Direct vendor parser tests/Clippy and the clipboard URI patch.
- Tracked-file audit, required Mermaid rendering, and generated icons.
- Package templates, update-manifest, draft-asset, release-script, package/media
  installation, and online-installer fixtures.
- Fuzz guards and live-probe/menu-screenshot fixtures.
- Warnings-denied workspace rustdoc.
- Independent release application and media-worker builds.
- Native image-paste routes under Xvfb with the pinned offline composer client.
- Simulated launch-scoped cards and centered media viewer under Xvfb.
- Isolated Linux install/uninstall, including the online path when available.
- Hidden main/alternate/split job-control fixtures and negative controls.
- A windowed Xvfb GPU launch plus offscreen GPU-info, menu, and PNG checks.
- The quarantined search-history live step, with artifacts retained even when
  it fails. That step uses `continue-on-error`; it does not gate the matrix.

macOS adds these checks inside `build`:

- Comparator scoring, standing-method, and A/V-sync self-tests.
- Release AppIcon compilation through the Xcode-selection helper.
- Update-manifest and release-script fixtures, including native-only cases.
- Package-manifest and media-installation fixtures.
- Independent release application and worker builds.
- Native image-paste routes with the pinned offline composer client.
- Modal-shortcut and light-titlebar native checks.
- Required Kettle-owned agent CLI probes; unavailable third-party clients skip.
- Hidden zsh job-control fixtures and negative controls.
- Offscreen GPU-info, menu, and PNG checks on the runner's Metal adapter.

Windows retains these additional checks inside `build`:

- Native Python and Cargo metadata execution for both advisory scope guards.
- Native ConPTY vendor coverage and PowerShell shell integration.
- Package-manifest extraction fixtures under Windows filesystem semantics.
- A retained multi-resolution ICO resource check.
- A release-profile build and piped version output through PowerShell.

All three hosts run CLI introspection and error-path checks.
The shared CLI helper rebuilds and compares the exact Git/dirty identity.
Windows CI does not install, update, uninstall, package, or publish Kettle.
Cross-compilation does not establish native ARM runtime behavior.

The MSRV and nightly jobs need software Vulkan because workspace tests include
offscreen rendering. Their build/test result is separate from live desktop
acceptance. The nightly job here is not the timed libFuzzer workflow.

### Workflow syntax: `actionlint.yml`

[.github/workflows/actionlint.yml](../.github/workflows/actionlint.yml)
runs for PRs and `main` pushes touching workflows, labeler configuration, or
Dependabot configuration.

The Mermaid parser, layout and paint dependencies are grouped for coordinated
review. Their exact family contract remains enforced by renderer tests; an
individual dependency resolving successfully does not approve a family upgrade.

- Job `actionlint` rejects external action/workflow references without immutable
  40-character commit SHAs, installs shellcheck, and runs pinned actionlint.
- It checks workflow syntax and embedded shell blocks on Ubuntu.
- It does not execute every workflow or prove runner-specific behavior.

### Static checks: `static-checks.yml`

[.github/workflows/static-checks.yml](../.github/workflows/static-checks.yml)
runs on every PR and `main` push, in about a minute on Ubuntu, and compiles
nothing. `just static-checks` runs the same steps locally.

- zizmor, offline, analyzes workflows for template injection, persisted
  credentials, cache poisoning and excessive permissions. `.github/zizmor.yml`
  records the one disabled audit and why.
- ShellCheck checks standalone `*.sh`/`*.bash` scripts and the pre-commit hook
  at warning severity; actionlint covers workflow run blocks.
- typos checks spelling with the reasoned allowlist in `_typos.toml`.
- Ruff checks Python syntax, undefined names, and unused imports and variables
  with `ruff.toml`. It does not enforce formatting.
- lychee checks local Markdown links, images and heading fragments without
  network access. The tracked-file audit checks link targets but strips
  fragments.

### Deep checks: `deep-checks.yml`

[.github/workflows/deep-checks.yml](../.github/workflows/deep-checks.yml) runs
Sundays at 07:00, on manual dispatch, and when the workflow itself changes.
Its three jobs are advisory: they point at work rather than gate merges.

- `coverage` runs `just coverage` and uploads HTML and LCOV. GPU and decoder
  tests that skip without their host count as uncovered, so read it for
  uncovered branches in limits and refusal paths, not for a percentage.
- `mutants` runs one of 26 weekly `cargo-mutants` slices over kettle-vt,
  kettle-media, kettle-config, kettle-update and kettle-state; a dispatch can
  name any `k/n` shard. A surviving mutant is a lead to check, not a failure;
  a failing unmutated baseline fails the job. A test that kills a mutant does
  not replace checking that a regression test fails against its original bug.
- `asan` runs `just sanitizer-test` on Ubuntu x86_64 with the pinned fuzz
  nightly and an instrumented standard library. System libraries stay
  uninstrumented.

### Advisories: `audit.yml`

[.github/workflows/audit.yml](../.github/workflows/audit.yml) runs for PRs and
`main` pushes touching Cargo manifests/locks, the lru scope scripts/tests, or
this workflow. It also runs daily at 06:00.

- Job `pull-request-audit` runs only for PRs. It checks the lru exception scope
  and audits the product and vendor lock graphs with read-only permissions.
- Job `trusted-audit` runs for pushes/schedules. Its matrix covers `product`
  and `vendor-validation`; RustSec reports checks/issues in this trusted job.
- Trusted legs also run explicit warnings-denied audit commands so a scheduled
  report cannot leave an affected graph green merely because an issue opened.

The product command explicitly ignores `RUSTSEC-2026-0253` under the scope guard.
It also ignores the maintenance-only notices `RUSTSEC-2026-0370`,
`RUSTSEC-2026-0371` and `RUSTSEC-2026-0372` for the exact reviewed D-Bus graph.
`scripts/check-dbus-scope.py` verifies exact package versions, registry sources
and every reverse dependency across all targets before admitting these notices.
Its regression suite covers graph drift, source changes and unexpected
consumers. `--deny warnings` remains enabled; vulnerability notices and all
other warnings still fail. The vendor graph inherits none of these exceptions.
Read the Cargo audit configuration and guard before changing an exception.
A changed or resolved dependency path must trigger reassessment and cleanup.

### Dependency policy: `deny.yml` and `shear.yml`

[.github/workflows/deny.yml](../.github/workflows/deny.yml) runs on PRs and
`main` pushes touching Cargo manifests/locks, `deny.toml`, or itself.
It also runs Sundays at 06:00.

- Job `cargo-deny` has `product` and `vendor-validation` matrix entries.
- Each checks licenses, sources, and bans against its manifest.
- Advisory scanning belongs to the audit workflow.

[.github/workflows/shear.yml](../.github/workflows/shear.yml) runs on PRs and
`main` pushes touching Cargo manifests/locks, Rust sources or itself, plus
Sundays at 06:00. It compiles nothing.

- Job `shear` runs pinned cargo-shear with `--locked --deny-warnings` on Ubuntu.
- It reports declared normal, dev and build dependencies no code uses, and Rust
  files no module includes. Reviewed false positives live under each crate's
  `[package.metadata.cargo-shear]`; a stale entry there also fails.
- cargo-shear replaced cargo-machete, which missed an unused `kettle-render`
  dev-dependency.

### Timed fuzzing: `fuzz.yml`

[.github/workflows/fuzz.yml](../.github/workflows/fuzz.yml) runs daily at
07:41, on manual dispatch, and for its fuzz-related PR path filter.
See [Fuzzing](#fuzzing) for targets, durations, limits, and reproduction.

- Job `plan` tests the planner and emits its target matrix.
- Job `fuzz` runs each target on Ubuntu with the pinned nightly, restored
  corpus, sweep seeds, optional dictionary, and sanitizer runtime limits.
- Job `report` runs only after a failed scheduled plan/fuzz result and opens
  or updates the tracking issue. PR and dispatch failures do not use it.

Linux CI package setup rewrites Azure HTTP entries in direct sources and mirror
lists to the Ubuntu HTTPS archive, bounds network waits and omits recommendations.
Required Xauthority, fonts and cross-linker headers remain explicit dependencies.

### Native Linux audio: `linux-audio.yml`

[.github/workflows/linux-audio.yml](../.github/workflows/linux-audio.yml)
runs on manual dispatch and PRs touching media crates, the audio checker,
root Cargo files, or the workflow.

- Job `audio` installs ffmpeg and PulseAudio on Ubuntu.
- It builds the worker independently in the `media-worker` profile.
- It builds matching worker, decoder, and audio-output test executables and
  passes Cargo's JSON artifact records to `scripts/check-linux-audio.py`.
- The checker requires native playback, including video before a delayed audio
  track, and preserves its results as artifacts.
- Its decoder tests use a stand-in probe to check video-only fallback when
  audio probing stalls.

This native service path is stronger evidence than parser-only audio fixtures.
It still does not replace listening on the release user's hardware.

### Native macOS window: `native-window.yml`

[.github/workflows/native-window.yml](../.github/workflows/native-window.yml)
runs on manual dispatch and PRs touching `kettle-ui`, the named native checker
helpers, modal-shortcut helper, live-UI helper, or the workflow.

- Job `titlebar` runs on macOS 26.
- It runs checker unit tests, builds a debug Kettle fixture, and drives the
  native caption/fullscreen check in the logged-in user's session.
- It preserves JSON, logs, and PNG evidence even after failure.

The modal-shortcut helper is in the trigger filter; this job does not run that
helper. The core CI macOS leg runs it separately.
Do not infer release-bundle acceptance from this debug fixture.

### Nix package: `nix.yml`

[.github/workflows/nix.yml](../.github/workflows/nix.yml) runs on every PR and
push to `main`.

- Job `check` runs on Ubuntu 24.04 with upstream Nix installed by the action.
- It evaluates all supported systems without updating the lockfile.
- It builds the x86_64 Linux cargo-test and package-content checks.
- It launches the installed package under Xvfb, then explicitly builds the
  package without creating a result symlink.

[flake.nix](../flake.nix) limits its sandboxed Rust tests to `kettle-vt` and
`kettle-remote`. The sandbox's root ancestry does not meet positive private-path
fixtures' ownership policy. Native CI remains responsible for the full workspace.

The package-content derivation compares installed desktop assets, icons, man
page, and shell snippets against the checked-in sources, including modes and
file count. The runtime derivation checks the installed package's dynamic GUI
libraries under Xvfb without borrowing `LD_LIBRARY_PATH`.
Other-system evaluation is not a native package build or runtime pass.

### Release pipeline: `release.yml`

[.github/workflows/release.yml](../.github/workflows/release.yml) runs on
`v*` tag pushes and manual dispatch. Publication jobs require a `v` tag ref.
The jobs execute in this order:

1. `pretest` runs locked workspace tests and direct vendor tests on Ubuntu
   24.04. It checks that no shipped binary enables fuzzing features.
   On a release tag, it requires a GitHub-verified annotated tag at current
   `origin/main`, with matching Cargo, Nix, and changelog versions.
2. `package` waits for pretest. Its required matrix builds Linux x86_64,
   Linux aarch64, and a universal macOS package, including the separate worker.
   Native legs also run the vendor suites.
3. `finalize` waits for all packages. The protected signer verifies the complete
   platform set and extracted packages, signs the canonical update feed with
   the production trust root, and renders package-manager metadata.
4. `publish` waits for finalization. It re-verifies the staged set without the
   signing secret, uploads a complete draft, checks the exact remote assets,
   and publishes once.
5. `upgrade` waits for publication on push-triggered runs and calls the reusable
   upgrade workflow against the new release.

Both Linux GNU packages build on Ubuntu 22.04.
The workflow rejects glibc requirements above 2.35 in the binaries.
The aarch64 package is cross-built; packaging is not native runtime acceptance.
The macOS leg runs on macOS 26, builds both architectures, compiles icons,
signs nested code and the app, notarizes, and staples the bundle.

The finalized set contains twelve files: three archives and their sidecars,
the signed manifest and signature with their sidecars, and Homebrew/AUR metadata.
Every platform package is required before publication.
An upgrade failure happens after publication and needs follow-up; it cannot
retroactively block the release that has already become public.

### Published upgrades: `upgrade.yml`

[.github/workflows/upgrade.yml](../.github/workflows/upgrade.yml) is callable
from another workflow and supports manual dispatch with `to` and optional
`from` release tags.

- Job `linux` has Ubuntu 24.04 x86_64 and ARM runner entries. It installs the
  older release with that release's installer into a private prefix, updates
  to the newest release, checks a second update is a no-op, then uninstalls.
- Job `macos` checks out the selected release and runs the published notarized
  archive swap/seal smoke.

Linux requires `to` to be the newest published stable release.
When `from` is empty, it selects the previous stable tag.
These jobs need published assets and network access.
They prove the deployed feed/package route, unlike fixture-only updater tests.

### Review and repository maintenance

[Greptile settings](../.greptile/config.json) enable advisory reviews on PRs,
pushes and rebases. Automatic approval and PR-description edits are disabled.

[.github/workflows/claude-review.yml](../.github/workflows/claude-review.yml)
handles PR opened/synchronize/reopened/ready-for-review events.

- Job `review` runs only when `CLAUDE_REVIEW_ENABLED=true`, for non-draft PRs
  opened by `Reddimus` from the same repository. It needs available service quota.
- It may post concrete inline findings; it does not build or test the product.
- Its error-reporting step surfaces suppressed review failures.

[.github/workflows/labeler.yml](../.github/workflows/labeler.yml) handles
`pull_request_target` opened/synchronize/reopened events.

- Job `label` adds path-based labels using `.github/labeler.yml`.
- It reads changed paths without checking out PR code and does not remove labels.
- This is repository automation, not test evidence.

[.github/workflows/stale.yml](../.github/workflows/stale.yml) runs daily at
06:30 and on manual dispatch.

- Job `stale` marks issues after 90 inactive days and PRs after 60 days.
- It closes them after another 14 days, subject to exemption labels.
- It is tracker maintenance, not a product validation gate.

### Interpreting CI evidence

Check the job and step, not only the workflow badge.
A green matrix can include an explicitly skipped native prerequisite or a
quarantined step. Read the output for the behavior you intend to claim.

Keep the failing input or artifact directory when reporting a failure.
Include the exact revision and runner, and say whether the result came from
portable fixtures, software GPU rendering, native hardware, or a package.
A passing old commit is not evidence for a changed candidate.

## Settings text and column layout

Settings text comes from `kettle-i18n`.
Use the existing focused suites while changing text, measurement, or clipping:

```sh
cargo test -p kettle-ui settings
cargo test -p kettle-render settings
```

UI tests cover translated labels, notes, footer text, and accessibility names.
Renderer tests cover column measurement, separate label/value clipping, category
hit targets, focused-row visibility, and notes in short panels.
The short-panel regressions include 284-pixel and 200-pixel surfaces.

For live acceptance, use the exact candidate and actual target GPU:

- [ ] Inspect Appearance, Behavior, and Graphics in both wide and narrow windows.
- [ ] Check the label/value gap, aligned values, active-category underline,
  long paths, wide characters, and long adapter names.
- [ ] Change blur/opacity and GPU settings. Confirm contextual notes disappear
  when irrelevant and pending changes keep neutral restart/new-window wording.
- [ ] Reduce height and scroll fields. Keep the focused row and both contextual
  and pending notes visible; footer clicks must stay inert.
- [ ] Check keyboard, mouse, Tab/Shift+Tab, and Vim navigation in each language
  affected by the change.

A previous appearance capture does not verify new settings text or layout.
Keep current native evidence with the change instead of carrying a historical
release's acceptance forward.

## Standing publication contract

The standing method has portable parser/statistical fixtures and separate
native measurement requirements. Use
[scripts/perf/standing-method.md](../scripts/perf/standing-method.md) as the
complete freeze, control, pilot, and publication protocol.
Do not copy its owner invocation into an automatic gate.

Run the portable suite in the normal timezone and the date-boundary timezone:

```sh
just macos-standing-self-test
TZ=Pacific/Kiritimati just macos-standing-self-test
```

These tests cover result compatibility, statistical rules, identities, coverage,
publication extraction, method matching, and rejection of incomplete artifacts.
Schema compatibility checks use explicit unittest comparisons so optimization
cannot remove the assertions.
Synthetic fixture values are not measurement or acceptance evidence.

The publication tool is
`scripts/perf/macos-standing-publication.py`.
Its fill and factcheck modes both require original session directories and a
matching ordinary shared control. They regenerate the typed extraction before
using it. The template lives at
`scripts/perf/macos-standing/publication-template.fixture`.
There is no preview waiver for missing raw evidence.

The local report recipes do analysis rather than launch the ordinary control:

- `just macos-standing-control-report`: combines sessions against a supplied A/A
  control; provide the required control and session arguments from Justfile.
- `just macos-standing-observer-report`: analyzes stamp or observer equivalence;
  provide its input and output arguments.
- `just macos-standing-publication`: explicit fill/factcheck entry point;
  provide the publication tool's required arguments.

Before freeze, finish the harness changes, required repository gates, native
and portable CI, and independent review on the fixed candidate.
Keep path/mode/content identities and verify owned children are reaped.
Do not change runtime harness code during an open measurement set.

Only the owner starts the ordinary shared A/A in a scheduled measurement window.
Functional permissions, focus/visibility, before/after blink, timestamps,
observer cost, and real cursor-exit production need native pilots.
A parser fixture cannot establish TCC grants or native timing capability.

Cursor-exit tests validate identities, complete frames, calibration/warmup
handling, hashes, and pooled costs. Empty or missing native evidence cannot
become a pass merely because its parser accepts the format.
Keep the raw per-key evidence and the matching method identity.
Native exit publication remains diagnostic until producer perturbation is
independently validated under the standing protocol.

Do not start the owner control from CI or a default just recipe.
Do not claim calibrated performance, an A/A pass, or publication acceptance
from the portable self-test alone.

Linux X11 smoke helpers remove inherited `WAYLAND_DISPLAY` and `WAYLAND_SOCKET`
from their child environments. This keeps windows and input inside the selected
X11 display, including Xvfb launched from a Wayland desktop.

The media viewer smoke also exercises video with native X11 input: opening stays
paused, the padded 90% split viewer expands the picture without cropping, picture clicks and K toggle
playback, Left/Right seek while paused, and T switches to a padded Theater view without
changing terminal contents. A 1920x1080 fixture also plays to its last frame
without further input or control polling. The worker process-boundary test
`real_ffmpeg_plays_large_frames_to_the_end` exercises the same failure under
production resource limits with a real trusted FFmpeg. Small fixtures alone do
not catch automatic output-encoder threading exhausting the address space.
Native fullscreen transitions and restoration require
a desktop compositor and must be checked on each native platform; Xvfb without a
window manager does not prove fullscreen presentation. Renderer regressions also
check that short viewers retain a picture and portrait video retains seeking
and time by using the available letterbox width.

For native video acceptance, open a video card in a split and check that its
poster fits inside 90% of its owning split without cropping. Check the central
Play affordance and controls beneath the picture. Check the compact white
Play/Pause and display icons on their dark strip, the full-width thin timeline
in the theme accent, and the seek handle on hover. Click Play/Pause and the
picture, press K, then drag across the timeline's hit area and seek with
Left/Right while paused. Check narrow and scaled layouts: targets stay usable,
time labels stay visible, and the picture remains uncropped. GPU tests verify
that progress and timeline hover reuse prepared text. Pixel checks at 1x,
1.25x, and 2x verify the central Play's vertical alignment, optical offset,
and size; all bundled themes retain a visible accent. Check the initial
timestamp and unchanged picture/control geometry when playback starts. Tests with narrow layouts cross elapsed-time digit boundaries; larger text retains fixed
icon sizes, and an active silent preview shows Stop. Toggle T and check the
pane-sized and window-sized views. Enter fullscreen with F; the first Esc must
restore the window while leaving the viewer open, and the second must close
it. Repeat with the Close and Pin controls while fullscreen. Finally, open and
close a viewer in a window that was already fullscreen: it must leave that
window fullscreen. Check the accessible names and toggle states for both size
controls, including Spanish. Preserve the original pane focus and verify that
player keys do not reach its terminal.

Update-toast tests use injected clocks for foreground expiry and hover pauses,
and private cache fixtures for concurrent claims, restart persistence, abandoned
claims, contention recovery and failed writes. An unreadable record must remain
byte-identical. Native checks must retain terminal grid, cursor and
pending input across presentation, dismissal and Details, including background,
narrow-window, Spanish and reduced-motion cases.
Also verify download-lock isolation, failed-receipt retry, automatic recovery
from delivery-lock contention, window-owner closure and persistent dismissal.
On X11, xdotool/xwininfo exercise manual re-docking and closing background
windows through the control API. Large-font warnings and hyphenated version
tokens are checked against the actual text shaper.

The media-native runner also tests refused child termination: it must not report
cleanup until the recorded child is reaped. Optional audio fallback requires
that confirmation; otherwise playback opening fails and the parent stops the
worker group.

For macOS Dock reopen, run two compiled CLI versions in one private bundle.
Replace its installed executable while the old terminal stays open, then
reopen from the actual Dock. Verify the new version and PID, repeat without
adding windows, and run a command in the old PTY. A metadata-only version
fixture does not prove this behavior. Portable activation tests cover the
focus-or-open request, recording identity and default-launch compatibility.

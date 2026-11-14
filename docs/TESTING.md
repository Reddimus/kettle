# Testing

Run `just gauntlet` before opening a PR. Before a release or a supply-chain
change, run `just gauntlet-strict`; use `just gauntlet-full` for the required
native gates on your current OS, especially after packaging or renderer changes.
CI adds the other operating systems, the minimum Rust version, packaging,
scheduled dependency scans, and longer fuzz runs. Linux and macOS are supported
runtime hosts. Windows CI retains compile and regression coverage, but does not
produce a supported package. Record every skipped check and its reason.

Profile/layout UI regression coverage lives in kettle-ui's `menu_text_tests`,
`macos_menu::tests` and `picker_text_tests`: English/Spanish menu routes,
profile lists including empty, native submenu dispatch order, and the shared
Save Layout prompt prefix/caret. A subprocess with a temporary `XDG_CONFIG_HOME`
saves and reloads a split layout, checks picker discovery and validates the
palette's profile discovery without changing the real config directory.
The portable profile inventory regression blocks an injected enumerator while
menu dispatch uses the empty startup cache and then the last completed cache.
It checks coalesced refreshes and eventual latest results without reading owner
profiles. Existing menu-model tests pass an explicit empty profile list.
Native rendering, OS directory opening, actual profile/window launches and
Spanish language acceptance still require a live UI pass.

The portable accessibility pane-lifetime regression uses window-less mux
fixtures and the production lifecycle drain with the accessibility adapter absent.
It closes split panes/tabs, reaps panes repeatedly, moves a tab between windows
in both directions, and checks that retained announcement text, output cursor,
serial, typing/input-row state and row IDs survive. Departed records are removed
by live pane ID even when cache and pane counts happen to be equal.

Portable Wayland tear-off geometry coverage exercises the same horizontal
strip layout used by painting, including centered single-tab custom headers,
window-button reservations, narrow/wide surfaces, non-default padding and tab
minimum widths, and fractional/double scales. Ordinary top/bottom/side grips
retain their offsets. These tests do not exercise a compositor grab; native
Wayland interaction remains a separate private-session check.

## Run it

The shell path regression runs Fish integration from directories containing
spaces, quotes, `cx`, and `ctrl-x`. It verifies that source-location comments
in `bind` output cannot disable correlation or change stock key bindings.
Native bash and zsh tests also source generated agent functions and wrappers
with existing aliases and verify that both the functions and aliases survive.
Rust shell fixtures find executables through `PATH`, including custom prefixes
and relative entries, and skip files without executable permission.

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

The renderer's `horizontal_translucent_active_surface` tests check that active
horizontal tabs use the terminal background colour and composed alpha. Quad
coverage checks the rounded chrome hole, opaque band at alpha 1, accent line,
top and bottom bars, dark and light themes, and fractional display scaling.
The headless pixel test compares tab and terminal RGBA at opacity 0.86 for
titlebar and ordinary strips; it prints `TAB_SURFACE_GPU_SKIP` without an
adapter. Run
`cargo test -j 2 --locked --offline -p kettle-render horizontal_translucent_active_surface -- --nocapture`,
then the full `kettle-render` and `kettle-ui` suites.

The horizontal tab regressions cover a single tab at rest and on hover,
multiple tabs, optional hints when space is tight, and the shared close geometry
used by painting, hit tests and AccessKit. Run
`cargo test -j 2 --locked --offline -p kettle-render -p kettle-ui horizontal_single_tab_controls`
and `cargo test -j 2 --locked --offline -p kettle-render tab_controls_and_shortcut_hints`
for the visibility and bundled-theme contrast checks. The native titlebar
checker consumes `ui_geometry`'s per-tab `hovered`, `hint` and `close` fields;
`python3 scripts/test-native-titlebar.py` checks its scaled fixtures without a
live window. Full-size control metrics and contrast wiring have portable source
guards; these do not replace native titlebar screenshots.

Window-chrome regressions use `kettle-render`'s `solid_chrome_is_quiet`,
`inactive_tab_separator`, `shortcut_hint_fits_natural_advance` and
`tooltip_stays_inside_narrow` tests. `kettle-ui`'s
`narrow_chrome_keeps_active_title` checks 420 by 260 point top/bottom strips,
traffic-light insets, 1x/1.5x/2x scaling, active-tab overflow, control hit corners,
gaps, AccessKit bounds and the production `ui_geometry` serializer.
`new_tab_tooltips_translate` checks English/Spanish labels and live shortcuts.
Full render/UI suites and clippy remain required; native focus changes,
hover/press feedback, tooltip placement and Spanish acceptance need a desktop.

The `kettle-ui` `scrollback_receipt_hit` regressions exercise pill hit detection
and pane scroll actions with PTY-backed `WindowState` fixtures and the shared
receipt placement inputs. They cover a notice shifting an expanded receipt over
the pill, collapsed and nonoverlapping receipts, secondary and outside presses,
and independent panes and windows. Run
`cargo test --locked --offline -j 2 -p kettle-ui --lib scrollback_receipt_hit`.
These fixtures create no App, native window, renderer, clipboard or global UI
services. Native and control event dispatch still require a live integration pass.

The renderer recovery tests include successful device creation on a backend
that failed while drawing, checking that another backend is tried first.
`python3 scripts/test-install-media.py` interrupts uninstall after each recovery
helper deletion and retries through installed helpers or a fresh package install.
`python3 scripts/perf/macos-compare-score-self-test.py` forces process identity
probes to time out during registration and after successful registration. It
verifies that the benchmark wrapper reaps a child that ignores SIGTERM on
timeout, payload completion, and a shutdown signal, preserving exit statuses
and removing the registry entry and completion marker.
Its `AppleWindowCleanupTests` executes the embedded AppleScript runner with all
native calls mocked. It checks that a returned window ID is logged and closed
when the post-launch PID query times out or fails, while a failed initial query
prevents launch. It also covers successful completion, malformed launch output,
and retained cleanup logs after a close failure. Run that class alone for a
process-free check; the full suite includes real child-process fixtures. The
Linux Fish CI job runs it alongside `ApplePayloadTests`.

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

### Symbol shortcuts

`kettle-ui` keyboard regressions exercise imported `Ctrl+Plus` with a native
Shift/`+`/unmodified-`=` event. They also retain explicit Shift precedence,
letter chords, unshifted symbols and layout-specific punctuation. The control
API's keybind dispatcher has no unmodified layout character, so it does not
stand in for this native event check.

### Remote session reproduction

`cargo test -p kettle-remote` pins OpenSSH's last-`-F` precedence and final-`@`
login split, binding/tag selection, absolute client configuration paths and
PowerShell's `-ec`/`-NoExit` classification. The tests cover host-specific path
semantics on every native CI runner. Native `ssh -G` configuration-only probes
and PowerShell encoded commands can verify the client behavior without opening
an SSH connection or starting an interactive shell.

### Main gates

The MCP unit tests in `crates/kettle/src/mcp_tools.rs` include
`a_card_reason_is_a_fixed_word_with_a_fixed_line` for fixed ASCII wording,
distinct lines under 256 bytes, and excluded characters.
`a_shelf_result_says_why_there_is_no_card` checks all three known reasons,
unknown-word privacy, and unchanged receipts when no card was requested.
`a_show_result_accepts_old_clients_and_unknown_card_reasons` covers missing
and future reason words during deserialization.
`a_card_result_carries_none_of_the_cards_text` checks receipt privacy even
with a reason set. `the_instructions_follow_the_placement` pins the guidance
to say once that no card shows, without retrying or changing anything.
Run these with `cargo test -p kettle --bin kettle mcp`.

The `kettle-core` tests selected by `regression_clipboard_` cover the clipboard
journal's load limit and byte-budget eviction. Both must latch reply loss even
when the ordinary reply channel is empty, and draining must not clear it.
The Unix MCP tests selected by `regression_real_pipe_` use a saturated OS pipe:
a reader draining 128 bytes every 20 ms must stay alive beyond the test stall
limit, while an unread pipe must time out and restore stdout's status flags.
Run the real-pipe tests on both Linux and macOS; Linux also exercises queued-byte
observations before a whole pipe-buffer slot becomes writable. Portable short
write and stopped-writer tests remain in the MCP suite. Run offline with
`cargo test -j 2 --locked --offline -p kettle-core regression_clipboard_` and
`cargo test -j 2 --locked --offline -p kettle regression_real_pipe_`, then run
the full suites with `cargo test -j 2 --locked --offline -p kettle-core -p kettle`.

`regression_one_byte_pipe_reads_keep_a_small_response_alive` fills a real Unix
pipe, queues a 512-byte response, and drains one byte every 100 ms for twice
the shortened stall limit before collecting the remaining response. Its unread
control, `regression_unread_pipe_stalls_a_small_response`, must still time out.
Run both on native Linux and macOS; macOS exercises the one-byte fallback when
an atomic write cannot fit, while Linux retains queued-byte progress tracking.
The media worker's `continuous_due_frames_` tests run in child processes with
a real 200 ms watchdog. A presentation budget longer than that deadline requires
renewal after successful writes; a shorter budget requires Close to be handled
between batches. Run offline with:

```sh
export CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2
cargo test -j 2 --locked --offline -p kettle-media-worker continuous_due_frames_
cargo test -j 2 --locked --offline -p kettle --bin kettle small_response
cargo test -j 2 --locked --offline -p kettle-media-worker -p kettle
```

On Linux, `file_hook_card_reports_a_lost_pickup_after_metadata_wait` and
`file_hook_card_reports_a_changed_root_after_metadata_wait` in `kettle-ui`
admit a live native fixture, wait for file metadata, then clear or replace the
pane root before publication. They check that the successful file receipt
reports `pane_cannot` when no card pickup can be issued. The portable file-card
tests retain command-line delivery, current geometry and metadata-failure coverage.
The theme-picker smoke uses a separate private configuration fixture (a native
private directory on Windows), then archives its final config with the artifacts.
Its helper regressions check Unix mode 0700 under umask 002, independence from
artifact-directory permissions, and cleanup after failed entry on Windows.
The split-titlebar smoke uses a non-login POSIX shell on Unix, so user startup
files cannot replace its controlled titles. The theme context-menu check scrolls
its row into view on short surfaces. Underline probes exclude the surface's
one-pixel focus outline while retaining interior underline and leak detection.
The Git streaming helper checks that both output pipes close after success and
consumer failure.

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

The active-fork teardown fixture deliberately starts more slowly than its
300 ms exec timeout. An injected test sink waits for eight complete PID records
before entering the lifecycle loop, so teardown runs against an active session
instead of racing fixture startup. This checks timeout cleanup, not startup
latency; production exec code and its timeout remain unchanged.

The Linux MCP supervisor regressions must run on a native Linux runner with
`setsid`, `sh`, `sleep`, procfs and pidfds available:

```sh
cargo test -p kettle --test mcp_supervisor
cargo test -p kettle --test exec exec_sigterm_and_sigint_follow_bounded_cancellation
```

`mcp_supervisor_preserves_setsid_on_exit_and_stops_on_timeout_and_cancel` launches `sleep 300`
under `setsid`, uses the PID the fixture prints (pinned with a pidfd), and checks
daemon survival on normal exit, and containment on timeout and cancellation
separately. The fixture redirects daemon stdio away from the PTY. Cancellation
still emits no MCP reply. `cancelling_one_mcp_supervisor_preserves_the_other_tree_and_output`
keeps two commands alive together, cancels one, verifies the other's exact PID
is alive, then checks its complete output and normal-exit daemon survival.
`exec_normal_exit_preserves_escaped_daemon` also checks the real CLI. The
SIGTERM regression checks that real `kettle exec` exits 130 and leaves no escaped sleep process;
the stdout-failure regression checks server shutdown cancels supervised tools.
`server_death_stops_supervisor_and_escaped_command` kills the MCP server with
SIGTERM and SIGKILL and requires both its supervisor and the escaped command
to stop without waiting for the 600-second timeout.
These are native Linux tests, not soft skips on failure. On the former
in-process implementation the escaped sleep survives timeout and cancellation;
the former CLI signal disposition also exits by SIGTERM instead of returning
130. Those pre-change Linux failures require Linux execution to demonstrate.

Portable Unix coverage includes `exec_internal_capture_preserves_raw_strip_tail_and_status`,
`exec_sigterm_and_sigint_follow_bounded_cancellation`, and
`mcp_capture_matches_internal_in_process_capture`. They pin the byte tail cap,
raw/stripped output, nonzero status, cwd, geometry, startup diagnostics and exact
MCP text/structured content, including existing truncation semantics. The
internal-capture regression was run before implementation on macOS and failed
with 125 instead of 7: the CLI tried to spawn the unknown internal switch as
the command. The portable signal regression also failed against the original
source: SIGTERM produced no numeric exit code instead of 130.
`exec::supervisor::tests::supervisor_*` also run on Unix: both pipes
under pressure, ignored SIGTERM escalation, outer timeout, inherited writers,
argument boundaries, and refusal to spawn with autoreaping SIGCHLD.
`supervisor_reaps_a_child_that_exits_after_kill_grace` injects delayed
termination after the bounded wait and uses WNOWAIT to require the background
owner to reap the eventual exit. `process_tree_teardown_is_idempotent_after_direct_child_reap`
requires repeated teardown to retain its original verified result.
`real_mcp_stdout_failure_returns_failure_status` closes a real stdout pipe and
requires numeric exit 1 rather than SIGPIPE on every Unix host.
`normal_completion_reserves_pid_until_output_acknowledgement` runs in an
isolated Unix process (with adoption enabled on Linux) and verifies the child is
still waitable at final output submission, then exercises acknowledgement,
cancellation, timeout and output failure. The Linux parent-death startup-race
unit test requires SIGTERM pending when registration discovers a lost parent.

`supervisor_command_uses_stable_linux_executable` checks the generated Linux
launch command on every Unix host. Native Linux coverage in
`mcp_supervisor_runs_after_server_executable_is_replaced` copies and starts an
MCP server, atomically replaces its executable with a different program, and
requires two later commands to run through the server's original image.
`exec_internal_capture_cancels_during_tail_delivery` waits for captured bytes
to reach an unread pipe after command exit, then requires SIGTERM to produce
exit 130 without reading the tail. The native Linux test
`internal_capture_tail_delivery_keeps_setsid_contained` adds a detached daemon
and checks containment during cancellation, timeout, and broken-pipe failure.

Linux runtime containment cannot be certified by these portable tests. If the Linux target standard library is installed, try
`cargo check --locked --offline -p kettle --target x86_64-unknown-linux-gnu`;
a missing cross C compiler can still block dependency build scripts. Record
that limitation and run the native suite in Linux CI.

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

`gauntlet` is the core Rust gate. It also runs the live-helper, UI-state, titlebar,
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
Linux portal tests require `dbus-daemon`; their private-bus fixture waits for
a successful socket connection, since a socket pathname can exist before the
daemon starts accepting connections.

The `term::command_marks::tests` fixtures feed the original OSC 133/CSI wire
through the terminal's bounded pump queue and real extractor, VT parser,
mutation journal and synchronized-update path. A test-only launch hook drains
native output separately; a raw-output tap verifies exact input bytes. On Unix,
the same wires also run through a native PTY and must produce the same visible
text and relative command rows. ConPTY interprets child output and emits its own
VT stream, so its native fixtures check text/outcomes and reply forwarding,
without treating that stream as a byte-preserving scroll oracle. Native Windows
CI is required to accept this fixture change; portable passes alone do not prove
ConPTY behavior.

The command-output tests parse their wires into a bare engine through the same
extractor and VT parser, applying each scroll journal before the next mark, so
they need no pseudoterminal; only the reflow test opens one, because reflow is
the `Terminal`'s work. The fixtures open one terminal at a time and retry a
failed open a few times: on macOS, opening `/dev/ptmx` fails now and then with
errno -6 while other threads or processes open pseudoterminals at the same
moment, with far fewer in use than `kern.tty.ptmx_max` allows.

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

`staged_replacement_and_discard_remove_reservations_in_a_writable_parent` creates
an explicitly mode-0775 export parent and checks that replacement and discarded
exports leave only the destination. Unix cleanup removes the identity-matching
empty reservation through the held selected-parent descriptor when private-parent
verification is unavailable. The `screenshot_demo_tests` destination fixtures
are explicitly mode 0600 on Unix; their parents keep their created permissions.
Session loading and restore-recovery fixtures also set mode 0600 explicitly,
so they exercise recovery rather than the trusted reader's writable-file refusal.
The macOS `private_creation_has_no_inherited_acl_before_first_hardening` fixture
sets its searchable private-state parent to mode 0755 before seeding its ACL.
Repeat these tests under both umask `022` and `002`.

`kettle-state`'s Windows-only
`private_directory_creation_uses_a_protected_current_user_dacl` checks both
levels of a newly created directory tree through `READ_CONTROL` handles,
verifies the current-user owner and protected full-access DACL, and creates
child directories and files. Read-only verification must query the held
directory rather than use the file-only `ReOpenFile` ACL-mutation path.
`staged_replacement_preserves_open_destination_handles` checks atomic
replacement while an existing reader retains the old bytes. The renderer's
`offscreen_screenshot_replaces_an_open_file_in_the_system_temp_directory`
exercises the same open-tempfile destination used by `bell_visual`,
`menu_visual`, and `scrollbar_visual`, without needing a GPU. These tests run
in the existing `windows-latest` workspace test job; a pass on Unix does not
verify the Windows `FileRenameInfoEx` replacement semantics.

`kettle-state`'s `trusted_leaf_follow_` tests cover relative config and Lua
dotfile targets, writable link parents, link loops, and the link/component
budgets. Run these on Unix and Windows; the Windows junction-chain test also
checks a junction replaceable through an Everyone-writable parent. File-symlink
tests report a skip when Windows does not grant the required privilege; the
junction test still runs without that privilege. The supplementary-group query
test simulates 257 groups without changing the process credentials.

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
| `kettle-render` | `crates/kettle-render/src/geometry.rs`, `crates/kettle-render/src/*_tests.rs`, `crates/kettle-render/tests/` | Text, geometry, GPU pixels, menus |
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

The control-client cancellation-retirement test cancels only after its peer
has read the request. It checks both local refusal of a second request and
server-observed closure. A timer started before the call can cancel before any
bytes are written, which correctly leaves the connection usable and does not
exercise retirement.

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

The graphics decode accounting regressions run with
`cargo test -j 2 --locked --offline -p kettle-vt transient_ -- --nocapture`.
The maximum-transmission tests feed a 96 MiB Kitty upload that inflates to
the 64 MiB cap and decodes a 4096x4096 RGBA image. Extractor cases exercise
1-byte, 4 KiB and maximum-size protocol chunks, plus seven 11 MiB chunks,
a 15 MiB chunk and the remainder. The final sequence decodes while its lease
is still live. The synchronized case queues the entire maximum upload before
replaying it with a full 16 MiB final sequence; it verifies saved-buffer reuse
with a peak of 192,937,970 bytes, four bytes above the immediate path.
`deferred_graphics_replay_` tests check protocol parity, terminators, and
preservation of an unrelated partial PTY sequence and graphics deferral.
Each maximum-upload case prints and asserts the transient high-water charge,
verifies idle-terminal admission, and releases all leases. The 1-byte case
processes about 100 million APC sequences and takes substantially longer.
`chunked_upload_segments_never_move_and_are_charged` checks stable segment
addresses and exact capacity charges. `segmented_` tests cover base64 quartets,
whitespace and padding across segment boundaries.
Other cases cover a full terminal while a second terminal decodes under
the same process account, failed decode/growth rollback, shared query/staging
ceilings, and iTerm2's bounded base64 stage. These are CPU accounting checks;
they do not require a GPU or live UI.

### Placement depth, screenshot retirement, and resize capture

`kettle-core`'s `anonymous_relative_depth_admission_uses_its_own_parent` and
`relative_replacement_preserves_anonymous_descendant_depth` exercise anonymous
relative placement admission and replacement. The shell-backed column-reflow
capture test runs only on Unix; `local_column_reflow_invalidates_active_and_finished_capture_without_a_shell`
checks the same invalidation through the local geometry commit on every platform.

`kettle-render`'s `idle_ticks_retire_failed_worker_charges_without_new_screenshot_admission`
uses a controlled completion callback and real budget reservations. It checks that
idle ticks keep polling until both charges retire, even while a retry cannot be
admitted. `idle_gpu_ticks_retire_a_screenshot_after_worker_startup_failure` checks
actual queue completion without mapped uploads or another submission; it skips
when no GPU adapter is available. A source guard pins polling and its next wake
to the UI tick, independent of frame visibility and screenshot admission.

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

The renderer's `production_source()` joins `lib.rs` and its extracted modules,
stripping test-only items from each file before joining. Its existing test
modules stay at the crate root to preserve test names. The UI text-sink guard
also scans each extracted renderer file; extend both source lists when moving
production code into another module. Function-order guards bound a method by
its closing brace so moving the following method does not widen their scan.

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

`cargo test --locked --offline -p kettle-media --lib exit_grace` models a
caller resuming after an observed worker exit's grace period with a reply
already queued. The tests backdate the recorded exit without sleeping and
check clean versus crashing exits, missing events, deadlines and cancellation.

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
check formatting for that patch. `kettle-ui --test clipboard_transfer` also
exercises the default image/file acquisition bounds with scripted correctly
typed INCR chunks (byte ceiling and absolute deadline). Its
`default_paste_x11_owner_cannot_renew_media_incr_forever` test drives the actual
arboard image/file acquisitions against an unterminated, correctly typed X11
owner; run it with `KETTLE_X11_CLIPBOARD_TEST=1` on a private X server. The
clipboard gate runs it under `xvfb-run`; macOS and Windows cannot execute it.
`cargo test --locked --offline -p kettle-ui clipboard::tests` covers display
backend selection, off-thread native operations, distinct CLIPBOARD/PRIMARY
contents, PRIMARY routing with an injected clipboard-only provider and platform
fallback, cap/deadline errors, stale-reply isolation, initialization failure and
image/file fallback without arboard, and that the Wayland text path adds no
dependency. On Linux, `wayland::native::selection` tests cover text-type ranking
and the bounded pipe read (cap, deadline, end of file, invalid UTF-8). These
tests do not exercise a compositor; the live check below does.

On a Linux native runner, build and run the full `kettle-ui` suite and clippy.
In a disposable GNOME Wayland session with XWayland disabled, verify text copy
and paste in both directions with another native application, OSC 52 CLIPBOARD
policy and ordering, and search/modal paste. Check large and stalled selections,
and verify that image/file clipboard paste logs its limitation and falls through
without a crash. Repeat on private X11 and XWayland to check automatic selection
copy, middle-click PRIMARY, and OSC 52 PRIMARY policy and ordering. Without
XWayland, direct PRIMARY requests are unavailable and must not alias CLIPBOARD.
No live Wayland result can be inferred from a macOS suite pass.

The trigger tests include DECSTBM rows 2-5, upward and downward retained-row
movement, and eligibility after an old match scrolls out.

CI runs parser patches on Linux and portable-pty on all three native hosts.
Only Windows executes its ConPTY-specific code.
Only Unix executes its descriptor and process-group paths.
A local vendor pass covers only the current platform.

The PTY descriptor regressions run a separate harness process so lowering
`RLIMIT_NOFILE` cannot affect concurrent tests. They open a non-close-on-exec
descriptor at 5000 (or the highest usable lower number), lower the soft limit
below it, and execute a child that verifies `fcntl(F_GETFD)` returns `EBADF`.
Another forced-fallback case lowers both the soft and hard limits below the
open descriptor. On macOS, setup caps the raised soft limit by
`kern.maxfilesperproc` before calling `setrlimit`; a portable calculation test
covers an unlimited hard limit with a 10240 kernel ceiling.
Both normal cleanup and a test-only forced fallback run: Linux's fallback
enumerates `/proc/self/fd` in the child with raw `getdents64`, while macOS's
fallback walks to the parent-read `kern.maxfilesperproc`, independently of
lowered process limits. Synthetic directory-record tests run on every Unix
host; the Linux enumeration itself requires the native Linux vendor gate.

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

The `exec::` tests also exercise timeout and cancellation with a real locked
cast file. The dev-only `kettle-core/test-recording` feature supplies a
thread-scoped hook before the persistence worker's final flush. A 100 ms delay
checks that capture returns with a complete, newline-terminated trace and an
immediately available exclusive lock, without a finalization warning. A gated
flush checks that a stuck writer receives the two-second grace, then detaches
with a warning within that bound (plus scheduling tolerance). Subprocesses
capture stderr without redirecting handles shared by parallel tests. These
portable tests run on Windows CI too, where the active file lock blocks reads.

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

## UI states

`just ui-states` captures Kettle's own UI states as PNGs: the default window,
the command palette (empty, with a query, with no match), Settings, Search
(matches and no match), three tabs, splits and the theme picker. Each state
starts a fresh Kettle with a private HOME and XDG directories and
`record = false`, reaches the state through `kettle ctl` (actions and UI
keys), and captures it with `kettle ctl screenshot`, which renders the
window's own frame without the desktop or other windows. Add
`--languages en,es`, `--themes dark,light` and `--sizes default,narrow` for
the full matrix; files are named `state.language.theme.size.png` under
`target/ui-states`.

`--compare DIR` checks each capture against the PNG of the same name in
`DIR`: a pixel differs when a channel moves by more than
`--channel-tolerance` (24), and a state fails when more than `--max-changed`
percent (0.5) of its pixels differ or its size changed. A missing baseline is
reported, never created. A change to a baseline belongs in the pull request
that changes the UI, with the before and after images in its description.
The committed Linux baselines in `tests/ui-states/linux` cover the full
80-state matrix. Their source, rendering environment, and expected visible
states are recorded in `tests/ui-states/README.md`. Linux CI compares fresh
private-Wayland captures against them; macOS retains its separate 80-state
geometry assertions without PNG comparison. A new baseline does not prove the
comparison ran: retain the actual capture-and-compare CI result for that tree.

The resolved `--out` and `--compare` directories must be disjoint. Equality,
ancestor/descendant paths, and symlink aliases are rejected before deleting
output or loading the live helper. Use a separate capture directory when
comparing; `--compare target/ui-states` overlaps the default output and fails.
The final component of `--out` must not be a symlink, even without `--compare`
or with a disjoint baseline directory. Rejection preserves the symlink and its
target before any output deletion or live-helper loading.

`just ui-states-self-test` runs stdlib-only headless regression tests, including
baseline preservation and injected geometry fixtures at 1x/2x backing scales
and default/narrow widths. It also tests the private Linux environment with
injected process/system boundaries: inherited desktop/config removal, socket
survival across fixture runtime overrides, font/GPU/compositor rejection,
startup/run failures, retained diagnostics, and identity-checked cleanup.
The real runner lifecycle also handles 240 synthetic adopted PTY exits across
80 cases, bursts above 32, failure cleanup, and nested subreaper scopes.
It runs in `gauntlet` and the CI matrix without starting Kettle or Weston.

On macOS CI, `--geometry-only` runs all ten states in English/Spanish,
dark/light and default/narrow sizes, saving `*.geometry.json` and
`ui-states.json` under `target/ui-state-geometry`. It asserts positive in-bounds
content and terminal rectangles, pane count/focus and split separation, tab
count/active state and titlebar clearance, the expected modal, picker rows and
no-match state, Settings rows/search bounds, and Search controls, match state
and reserved space. Geometry failures return nonzero and retain the offending
JSON. This mode needs a native graphical session and the built Kettle, but
does not capture PNGs or require Pillow. It cannot use `--compare`. The existing
native caption/fullscreen and other UI smoke assertions remain separate gates.

Linux CI calls `scripts/run-ui-states-wayland.py` for the same 80-state matrix.
The hosted Linux dependency step installs Weston, Fontconfig and Mesa through
the existing dependency installer. Other Xvfb smoke checks keep their existing
environment. To run the private matrix on a prepared Linux host with an already
built Kettle:

```sh
python3 scripts/run-ui-states-wayland.py --kettle ./target/release/kettle \
  --out target/ui-states --diagnostics target/ui-wayland-environment
```

The runner rejects other platforms before creating directories or processes.
It starts a private headless Weston with the GL renderer and a 1600x1000,
scale-1 output, plus a private D-Bus daemon without service activation paths.
Both session and system bus addresses point into this private session; extra
XDG config/data search directories are private and empty.
It removes inherited desktop, shell, agent and Kettle config overrides, uses
private HOME/XDG directories, removes `DISPLAY` and `WAYLAND_SOCKET`, and gives
clients an absolute private `WAYLAND_DISPLAY`. That socket remains reachable
when each fixture supplies its own 0700 `XDG_RUNTIME_DIR`. The existing helper
still owns Kettle, its explicit control-server launch and PTY cleanup; fixture
configs retain `record = false`. No global desktop input or socket is used.

Private Fontconfig configuration includes only `assets/fonts`, with no system
config includes. The runner verifies `fc-list` against the repository TTF set
and records every hash, including all four JetBrains Mono faces and the four
bundled Fira Sans faces. Software-rendering environment settings are requests:
the runner additionally requires Weston's actual headless/GL/llvmpipe log and
version, a GL llvmpipe CPU adapter with driver details from `kettle --gpu-info`,
and matching adapter selection in every live fixture log before and after
screenshot. Fontdb's system-directory fallback also fails the run.
An unknown or different adapter fails the run. The private GPU config requests
GL/software for both diagnostics and live fixtures. Diagnostic parsers accept
LF and CRLF line endings while retaining strict adapter/backend checks and
the original raw diagnostic files. Portable regressions force CRLF input on
every host; Windows CI also exercises the complete fixture path.

Startup defaults to 20 seconds (`--startup-timeout`, maximum 60); the matrix
defaults to 900 seconds (`--run-timeout`, maximum 900). Native queries have
20-second limits, `ps` has a 2-second limit, and process termination uses
3-second TERM/KILL waits. A run timeout unwinds the live helper so it can clean
up its PTYs. Weston and D-Bus use recorded direct PIDs and stable pidfd handles;
Weston's own desktop-shell child is recorded by direct parent and executable,
and a child-subreaper scope permits waiting if it outlives Weston. Signals
require an identity check. Failure to verify identity or finish cleanup fails
the run and preserves diagnostics. Child logs have a 16 MiB file limit.
The helper freezes Kettle before killing PTYs, so their unreaped exits can pass
to the outer subreaper when Kettle exits. After every fixture teardown, including
failed startup, the runner drains these newly adopted direct children while
excluding baseline children, recorded services and Weston's recorded clients
by stable identity. It tries `waitpid` before signaling and retains each PID,
identity, wait PID/status and post-wait diagnostic. Final cleanup uses the same
drain after stopping the services and cleaning Weston's clients. Each drain
retains at most 32 adopted handles per batch, processes at most 32 batches, and
shares a 30-second wait/inventory budget; inventory reads are capped at 64 KiB.
Each post-wait `ps` probe retains its separate 2-second limit. Exhaustion or
cleanup errors fail the run and retain receipts; they do not waive ownership
checks or accept surviving children. Nested scopes keep subreaping enabled
until the final owner closes.

The `ui-states-*` CI artifact includes captures and separate
`ui-wayland-environment/run-*` diagnostics even on failure: a native manifest
with source HEAD/diff digest, untracked-source and binary hashes, OS/Python and
package versions, private environment, font inventory/hashes, GPU diagnostics,
Weston backend/renderer/version, fixture logs and PID/wait/ps receipts. Private
scratch directories are removed after success and retained on failure; their
paths are in the manifest. Output, baseline and diagnostics directories must
be disjoint. When `tests/ui-states/linux` exists, CI compares against it;
otherwise it explicitly reports capture-only operation. Do not mix baselines
from different compositors/font environments.

The initial Linux pixel gate remains pending until the final merged build,
after the font and C08 changes land. Capture its full matrix in the final CI
environment, retain the build/commit and font/environment identity, and review
all 80 PNGs before adding them under `tests/ui-states/linux` in an explained PR
with before/after artifacts. Re-run CI with those baselines to establish the
pixel gate. This environment preparation has not run Weston or captured pixels
on native Linux; the first hosted Linux execution, artifact inspection and
baseline promotion remain pending. No baseline is generated or accepted by
headless fixture tests; capture-only CI does not prove pixel equality. Native
geometry evidence must also identify the exact capture script/build used.

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
| `just package-manifest-test` | Inner manifests, Linux packing and macOS ZIP compaction at the 128-entry limit, bounded extraction, media installation |
| `just online-installer-test` | Online install parsing, implied parent directories, transport limits, no downgrade, hostile archives |
| `just package-templates` | Deterministic Homebrew/AUR template rendering and optional published checks |
| `just icons-check-required` | Generated icon geometry, rasters, and formats with tooling required |
| `just icns-smoke` | macOS release icon assets through Xcode actool |
| `just linux-installer-smoke` | Real install/uninstall paths in isolated prefixes |

The macOS release job creates its ZIP with `ditto`, then runs
`python3 scripts/package-manifest.py compact-macos --archive ARCHIVE.zip` to
omit redundant parent-directory records. The portable package fixtures check
nested notice bytes, executable modes, ZIP metadata, empty directories and the
unchanged 128-entry limit. The native release job checks the extracted bundle's
signature and notarization again; the signed candidate also needs
`scripts/check-macos-update-smoke.sh ARCHIVE.zip` before tagging.

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
Its hermetic tests compare installed TokyoNight license bytes for Homebrew,
Arch, and the common Nix install hook. Nix's `tokyonight-license` flake check
compares the final store files on both supported platforms. A Linux packaging
fixture checks that NOTICE includes the complete license for existing updaters.
Installer tests cover recorded license replacement and removal, and reject
linked sources and destinations. The Linux updater transaction test compares
installed license bytes with the verified archive and checks their provenance;
a portable map test keeps the Linux document destinations and modes covered on
other CI hosts.
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
| Native titlebar | `just native-titlebar-smoke` | Both titlebar styles: traffic-light separation, single-tab centring, geometry, resize, native fullscreen and restoration | macOS |
| Modal shortcuts | `just modal-shortcuts-smoke` | Command chords do not act as ordinary modal letters | macOS |
| Interaction | `just interaction-smoke --isolated-clipboard` | Multiline input, wheel, selection/copy, tabs, context-menu split, and screenshots | Linux/macOS |
| Selection autoscroll | `just selection-autoscroll-smoke` | Held selection scrolls at both vertical pane edges; native pointer on macOS, control path on Linux | Linux/macOS |
| Image paste receipt | `just image-paste-receipt-smoke --isolated-clipboard` | Bitmap paste and expanded, compact, hover-expanded receipts; replaces clipboard | Linux/macOS |
| Video paste receipt | `just video-paste-receipt-smoke --isolated-clipboard` | Two pasted video files produce bounded poster receipts; needs ffmpeg and worker | Linux/macOS |
| Hover wheel | `just hover-wheel-smoke` | Wheel targets hovered pane; no swapchain screenshot support required | Linux/macOS |
| Window close isolation | `just window-close-isolation-smoke` | One detached child's exit leaves the sibling OS window and pane alive; Linux needs xdotool | Linux X11/macOS |
| Touchpad scroll | `just touchpad-scroll-smoke` | Fractional wheel deltas accumulate into scrolling through the real conversion path | Linux/macOS |
| Tabbar click | `just tabbar-click-smoke` | Multi-tab click state with PNG and tab geometry evidence | Linux/macOS |
| Pane drag | `just pane-drag-smoke` | Shared pane drag via ctl input; checks arming, slop, right-column drop and final geometry | Linux/macOS |
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
| Raw paste | `just raw-paste-smoke` | Multiline paste into an unbracketed shell proceeds; macOS restores its clipboard, other hosts need isolation | Linux/macOS |
| Program keys | `just program-keys-smoke` | Client-owned Shift+Left reaches Kitty client, then returns to split resize | Linux/macOS |
| Search selection | `just search-selection-smoke --isolated-clipboard` | Open search bar preserves grid selection, copy, menus, focus, shortcuts, and Esc | Linux/macOS |
| Bell flash | `just bell-flash-smoke` | Only ringing pane brightens, then pixels return to baseline. Needs motion allowed: `reduce-motion = off`, or the system's Reduce Motion off under `auto` | Linux/macOS |
| Default window size | `just default-window-size-smoke` | Monitor-fitted defaults and explicit sizing; needs a sufficiently wide monitor | Linux/macOS |
| Line edit chords | `just line-edit-chords-smoke` | Text-action backspace, macOS default chord, and search word deletion | Linux/macOS |
| Dock menu | `just dock-menu-smoke` | Real Dock rows, window list, minimized New Tab restoration and New Window effect; accessibility, plus an isolated bundle or no other Kettle instance | macOS |
| Window app ID | `just window-app-id-smoke` | New windows retain a renamed build's X11 class after its executable is replaced; runs under private Xvfb | Linux; required in Linux CI |
| Live stress | `just stress-live [S1 … S10]` | Floods, resize storms, 101 tabs and 151 panes, huge pastes and titles, config churn, hostile media and 64 control clients leave Kettle alive and answering; S6 replaces the clipboard, then restores it; S9 needs a working worker | Linux/macOS |
| Underline scroll | `just underline-scroll-smoke` | Repeated git-diff/delta scrolling preserves underline pixels; needs git, delta, less | Linux/macOS |
| Search history | - | Scrollback search status, cells, and highlight pixels; direct helper case | Linux/macOS; required in Linux CI |
| Native image paste routes | - | Native paste shortcuts, PRIMARY/middle-click, managed image path, and optional real composer attachments | Linux/macOS |
| Launch-scoped cards | - | Simulated agent TTY launches, receipt privacy, card pixels, exit, and shelf fallback | Linux |
| Centered media viewer | - | Worker-backed viewer routing and live pixels through native X11 input | Linux X11 |
| Centered confirmations | `just confirmation-smoke` | Two-pane close, safe Cancel, pointer/Space release, scrolling, retained Settings, visible text, and input isolation | Linux X11 |

To run the Dock smoke alongside an installed Kettle session, pass an isolated
candidate bundle's executable explicitly:

```sh
launchctl asuser $(id -u) python3 scripts/check-live-ui-smoke.py \
  --kettle /path/to/candidate/kettle.app/Contents/MacOS/kettle dock-menu
```

The harness requires exactly one running process from that bundle and exactly
one running Dock tile with its canonical bundle URL. It uses retained native
Accessibility handles for that tile and its menu items; ambiguous identity fails
before interaction. An unbundled executable still requires no other Kettle
instance. This checks the menu's behavior, not the icon's composited pixels.


The portable `kettle-ui` regressions below exercise queued facts expiry behind a
blocked check, card placement after a pane narrows during the wait, pickup
issuance after a wait longer than its lifetime, and failed WSL translation at
delivery. Each failed before its corresponding fix. Run them without a desktop:

```sh
cargo test -j 2 --locked --offline -p kettle-ui regression_waiting_file_facts_expire_while_another_check_is_stalled
cargo test -j 2 --locked --offline -p kettle-ui regression_waiting_file_card_uses_current_pane_geometry
cargo test -j 2 --locked --offline -p kettle-ui regression_file_pickup_lifetime_starts_at_publication
cargo test -j 2 --locked --offline -p kettle-ui regression_failed_wsl_translation_does_not_deliver_a_space
```

These fixtures do not replace native Linux pickup or Windows WSL mount checks.

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

The Claude launcher unit tests in `crates/kettle/src/agent_setup.rs` cover
switches, one-value, optional-value, and multi-value options, including aliases
and inline values. They check that option values stay values, that optional
and multi-value options stop before an argument starting with `-`, and that a
prompt after multi-value options needs `--`. Refused groups cover print mode
and its options, detached sessions, hooks-off modes, help and version output,
subcommands, unknown options, missing values, extra prompts, and non-Unicode
arguments. Refusal notice tests cover unknown options, hooks-off modes and
strict MCP configuration. `NoCard` unit tests check fixed notices for both
agent names across unreadable and unsupported versions, executable checks,
server failures and declined registration. They pin deliberately silent
refusals and check that argument values, prompts, paths, versions, pids,
environment values, control characters, and unsafe or overlong option names
never leak into the notice. These formatting tests do not prove that every
Codex fallback reaches the notice path; see
[Linux card launches](AGENT.md#linux-card-launches).

Status unit tests cover the fixed Kettle, own-command and unchecked launch
lines without echoing environment values. Print-form tests cover function,
wrapper and entry selection for both agents, including entry scripts without
a shell selection. Marker-reader tests accept only known agent names and
exact `0:<shell_pid>` or `1:<shell_pid>` values; bare `0`/`1`, different PIDs,
leading zeros, signs, extra characters, empty, missing and non-Unicode values
are unchecked. Unknown names do not read the environment.

`crates/kettle-ui/src/codex_shell.rs` byte-compares all wrapper and entry
forms against `scripts/fixtures/agent-wrappers/`, and both agents' startup
snippets against `scripts/fixtures/shell-integration/`. Real bash wrapper
and sh entry runs use a stand-in Kettle to check verbatim arguments (empty,
spaces, newlines and literal `--`), the account environment prefix on both
branches, exit-status propagation, and fallback outside a pane or when Kettle
is missing or not executable. Saved entry scripts named `claude` or `codex`
exit 2 with the refusal message on stderr; `claude-entry` and `codex-entry`
retain normal launch and fallback behavior. Bash, sh, zsh and fish syntax
checks include Kettle paths with quotes and newlines. The forwarding test
executes bash and sh, not zsh; a real zsh wrapper forwarding run is a separate
manual check.

Native zsh and fish startup tests cover both agents with no user function,
a user function, aliases in zsh, abbreviations in fish, autoloadable functions,
and stale inherited markers in both directions. They check that fish
abbreviations leave Kettle's function defined, zsh aliases leave an empty
marker, and checked markers contain the defining shell's own PID and remain
exported to sh children. They also check that zsh's one-shot hook removes
itself. Native tests skip an unavailable shell, and zsh can skip when the
system zshenv prevents injection. The portable
`pane_spawn_strips_inherited_agent_function_markers` test in `mux.rs` checks
that both markers enter the spawn's removal list before the terminal starts.

`scripts/check-shell-integration.py` also asserts Codex markers for Kettle's
function and an autoloadable user function, including the defining shell's
own PID, stale inherited values, and export to sh children. It also checks
that a zsh alias leaves an empty marker and a fish abbreviation does not
suppress Kettle's function. Rust reader tests cover PID mismatches in child
shells. Its fish checks run only when fish is installed; the CI fish job exercises
Fish 3.7, 4.2 and 4.8, and the Linux build job also installs fish. A real ccx
run and real card delivery end to end through a user wrapper are not part of
CI. The [manual ccx acceptance](#ccx-entry-script-acceptance) records the
verified entry recipe and tested versions; that scoped result does not make
CI exercise the live integration.

The drift guard classifies every option and subcommand in the captured Claude
Code 2.1.317 `claude --help`, and reads each option's arity from its
declaration (`<value>`, `[value]`, `<values...>` or none), so each option needs
a matching arity or an explicit exclusion. To diff the capture against the
installed CLI, opt in with:

```sh
cargo test -p kettle claude_help_matches_installed_cli -- --ignored
```

This check needs an installed Claude Code CLI. It checks help drift, not
rendered-card acceptance. Native Linux verification must also check that the
fallback notice appears only in an eligible Kettle pane with terminal stdin
and stdout; non-terminal launches must stay silent.

### Read the result

The helper's `all` case does not include every scenario.
Dock, clipboard receipt, selection-autoscroll, hover-wheel, and window-isolation
cases have explicit opt-ins. Use the table instead of treating `all` as a gate.

Clipboard-replacing cases require `--isolated-clipboard`: `interaction`,
`search-selection`, image/video paste receipts, `all`, and `raw-paste` outside
macOS. Use it only on a disposable desktop, such as a separate Xvfb server;
it does not create isolation. For example, with a built binary:

```sh
xvfb-run -a python3 scripts/check-live-ui-smoke.py --kettle target/release/kettle --isolated-clipboard raw-paste
```

The corresponding `just` recipes forward this option. macOS `raw-paste` instead
uses the modal probe's bounded snapshot of every clipboard item and format.
An uncapturable clipboard refuses the check before writing. Restoration only
replaces the probe's own text and generation, preserving a later foreign copy.
Startup failures do not read or replace the clipboard. Clipboard providers are
terminated and reaped if writing the fixture fails. Raw paste requires standalone
command output, so it works with shell echo disabled and cannot pass on
command echo alone.
Video receipt checks require both copied filenames, including names split
across physical terminal rows.
Shared fixture configs disable automatic copy-on-selection, so selection-only
checks do not replace the clipboard when releasing a drag.

`just stress-live` runs every scenario when given no names. Each one must
leave Kettle alive with no panic in its log, answering `get_state` within 2 s
and echoing a typed command within 1 s. The script's docstring lists the
scenarios; `--self-test`, part of `just script-fixture-self-test`, checks the
pass criteria without a desktop.
The tab scenario must reach 101 tabs and 151 panes and close back to one of
each. Run it with enough process file handles, for example `ulimit -n 2048`
in the test's launch shell. A reduced workload fails the gate.
The control storm allows connection-cap closures. On macOS, closing an accepted
socket can make `LOCAL_PEERPID` return `ENOTCONN` before a request is sent; only
the native error 57 is accepted there. Other transport failures still fail.

The tear-off recipe has two tiers. Its shell helper skips native pointer work
outside X11. The control tier alone cannot prove the native drag gesture.
The pane-drag recipe uses the shared control/native gesture handlers. It selects
the right column by position, since split rounding can make the left column
wider, and verifies the final right-edge geometry as well as pane order.
This control-plane check does not prove native pointer event delivery.

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

Font defaults are exercised for both platform choices on every host: 13 px
on macOS and 14 px elsewhere. Startup tests load the bundled font to check
fresh and maximized grids, including 1280-logical-pixel displays and the
1440x900 non-macOS default-window smoke guard. The card-hover outline,
inactive emoji/text and tab-control pixel tests run explicitly at both 13 px
and 14 px even on macOS. `inactive_bundled_text_retains_contrast_after_raster_coverage`
checks the real bundled mask at four subpixel phases without a GPU, on black
and white backgrounds in both blend modes. The painted stems must retain
4.5:1 contrast after coverage and output rounding. A separate headless test
draws dark text after four spaces on white in Grid and Legacy at both sizes,
including Legacy's half-pixel placement at 14 px. The portable check does not
replace these readbacks.

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
Inspect each probe in `result.json`, even when the helper exits successfully:
optional client failures do not fail the whole run. An `auth_failed` probe can
also mean exhausted service quota or another client error. Read the captured
client output to determine the cause before changing authentication.
For configured Neovim/AstroNvim, the direct helper accepts `--astro-config` and
`--nvim-data`. Supply your installed configuration/runtime directories.
The helper copies them into an isolated sandbox before use.
Keep the provenance record and distinguish clean from configured editor results.

## Terminal conformance

`just conformance` runs two outside suites in a live Kettle window opened at
exactly 80x25 cells. They are local runs, not CI gates: both need a desktop
(on Linux, wrap the recipe in `xvfb-run -a`), and
`scripts/check-conformance.py` fetches them into `target/conformance` at pinned
versions rather than vendoring them: esctest2 (GPL-2.0) by commit and
ucs-detect from PyPI into a private virtual environment.

**esctest2** (`just conformance-esctest`) checks escape sequences against
xterm. It reads screen cells with DECRQCRA, which Kettle does not answer: a
checksum reply would let any program reading the terminal's input probe the
screen. The script replaces that one check with `kettle ctl read_cells`, read
after a cursor-position report proves the terminal has parsed everything
before it, and keeps DECRQCRA's origin-mode offset by asking the terminal for
DECOM and its margins. The run fails when the set of failing tests differs
from `scripts/conformance-esctest-expected.txt`, which groups every known
failure by reason; `--update` rewrites it and puts new failures under an
unclassified heading to triage.

On 2026-10-30 Kettle 5.7.0 passed 259 of 567 tests and failed 265 (xterm
itself is expected to fail 43). Triage found six failures in controls Kettle
implements: CUU and CUD stopping at the margins only under origin mode,
DECSC/DECRC dropping origin mode, DECALN leaving the margins and cursor, and
LNM never applied. Those fixes and the seven mode 1048 tests brought 5.7.1
to 272 passed and 252 failed.

The 2026-10-31 margin follow-up passes 327 tests and fails 197, with no new
failures. DEC mode 69 and DECSLRM now cover origin-relative positioning,
reports, editing, wrapping, tabs and rectangular vertical scrolling. Partial
scrolls retain surrounding columns and do not add rows to history; a wide glyph
straddling a margin is cleared as a unit to preserve cell pairing. Portable
regressions also cover wide cells, selection, vi movement and graphics.
Seven-bit DECSCL selections reset terminal state and disable horizontal
margins below level 4; mode reports are unavailable below level 3. Requests
for raw eight-bit C1 replies leave the existing state unchanged. This does
not establish full VT compatibility. The expected list retains the remaining
protection, rectangle, column-editing, reverse-wrap, colour, mode-report and
window-policy cases under their actual feature dependencies.

The character-protection follow-up removes another 30 failures: the native
suite passes 357 tests and matches 167 remaining expected failures, with no
new failures. Character-protection regressions in the vendored engine exercise DECSCA,
SPA/EPA, DECSEL/DECSED, ordinary EL/ED/ECH, SGR independence, cursor
save/restore, status reports, wide-cell overwrites and erasure at pending
wrap. They use escape sequences through the real parser; the initial cases
fail with the implementation reverted. The focused native esctest subset
also checks the existing xterm rule that selective erase respects ISO
protection, which the suite labels as a reference compatibility exception.
A subset run does not compare the global expected list; check its reported
failure count explicitly and run the complete suite for integration.

The rectangular copy/fill/erase follow-up removes another 24 failures: the
native suite passes 381 tests with 143 expected failures and no new failures.
Portable rectangular-operation tests cover parser dispatch, defaults, clipped
coordinates and destinations, page-number clamping, origin-mode margins,
overlap with complete cell attributes, unchanged cursor and pending wrap,
DEC versus ISO protection, selective-erase attributes, and wide-cell boundaries.
A separate whole-glyph reference checks 2,048 horizontal overlapping-copy
combinations across four mixed CJK rows. A leading-spacer regression fails
with its cleanup disabled and verifies unrelated preceding-row protection.
These controls require VT level four or above.

The column-editing and reverse-wrap follow-up passes 413 esctest tests,
with 111 expected failures and no new failures: another 32 cases now pass.
The adapter explicitly selects xterm's post-383 reverse-wrap profile, which
separates mode 45 from mode 1045; the suite's legacy default expects different
mode-45 behavior. With that profile the focused native subset passes all
50 tests. Portable cases also cover clipped counts, cursors outside margins,
protected wide spacers in history, old full-row wraps after changing margins,
partial continuation annotations through rectangle operations and margin
changes, pending wrap, resets, and the existing cell-size cap.

The attribute/mode/report follow-up passes 439 native esctest cases, with
85 classified expected failures and no new failures: 26 more cases now pass.
A completed repeat verified the result after an earlier run's process-table
cleanup timeout; the failed cleanup run is not counted as a harness pass.

Portable attribute-operation cases cover stream versus rectangle extents,
printed spaces versus undrawn blanks, cumulative set/toggle/reset operations,
preserved text/colours/protection/cursor state, and DECRQSS extent/blink round
trips. Mode save/restore is tested in both directions and across multiple
modes; MoreFix tests explicitly negotiated on/off behavior and reset to the
legacy tab rule. Status-report cases verify fixed modes, absent peripherals,
and continued refusal of checksum queries. Blink rendition is stored and
reset independently of bold while rendered text remains steady.

The keypad/Backarrow/no-clear follow-up passes 444 native esctest cases,
with 41 known bugs and 82 classified expected failures. Five more cases pass:
DECNKM and DECBKM reports, VT5 no-clear selection, and the two DECNCSM
set/query cases. The last two require adapting esctest's xterm-specific
window-resize precondition: Kettle supports no-clear independently of window
resizing. The adapter removes that precondition only for those two named
cases, retains every behavior assertion, and leaves geometry tests unchanged.
`just conformance-adapter-self-test` checks both successful and failing
assertions and the unaffected option decorators; it runs in the strict gate
and native CI matrix. An initial run reported two unexpected-pass errors
from that precondition; it is retained as diagnostic evidence, not counted
as the final conformance result.

Portable cases cover every split of the no-clear sequence, VT4 versus VT5,
mode save/restore, cursor homing in one-row panes, wide cells, selections,
retained-graphics events, alternate screens, RIS and DECSTR. Backarrow tests
exercise configured startup bindings, explicit BS/DEL negotiation, modifiers,
Kitty precedence and reset. Save/restore regressions retain both Backarrow
flags, cover every control-sequence split, replacement saves and reset-cleared
snapshots, and assert actual key bytes from parsed Control-H and EscapeSequence
settings. Immediate save/restore before negotiation preserves each startup
binding; explicitly negotiated BS and DEL survive intervening changes. Removing the negotiated-key binding override
makes the keyboard regression fail; removing the modes makes the core
regressions fail.

The screen-reverse follow-up passes 457 native esctest cases, retains 41
upstream known-bug results and matches 69 expected failures with no new failures.

Screen-reverse regressions exercise every split of query/set/reset/save/restore,
DECSTR preservation and RIS reset. Palette updates made while reversed are
checked after returning to normal video. Native GPU captures in both text paths
verify default background reversal, explicit RGB preservation, SGR inversion
and exact frame restoration across cached captures. Removing mode activation
makes the pixel regression fail. Cursor tests preserve explicit custom colours.

The extended-colour regressions exercise all seven floating colour formats,
known sRGB primaries and neutral values, signed/scientific components, invalid
and degenerate inputs, and every split of palette, dynamic and special-colour
OSC sequences. Special-colour tests cover alias queries, reset isolation,
default-off modes, ANSI override and retained OSC resources through resets.
The native esctest run passes 456 cases, retains 41 upstream known-bug cases
and has 70 explicit expected failures, with no new failures. Twelve special-colour
cases now pass. Fixture setup uses the suite's black/white defaults and resets
special resources between cases; every original test assertion remains intact.
A native GPU regression exercises all five attributes in both renderer paths;
removing the render hook makes it fail. The fixed D65/sRGB conversion is a
portable policy: differences from esctest's X server conversion and physical
xy input validation are explicit exceptions. Its RGB values are not substituted
into the implementation.

ANSI keyboard-mode tests exercise defaults, every chunk boundary, RIS,
DECSTR, capability withdrawal, fixed headless reports, and hardware-report
refusal. Real PTYs cover split UTF-8 against a child's unfinished control and
synchronized-update source changes and deadlines. Hard reset clears partial
local controls, including a reset buffered inside a synchronized update. Queue fixtures cover
backpressure, stopped readers, parser acknowledgment, rejected messages,
keyboard lock, and mouse/focus ordering without local echo. Removing lock
activation, the echo-before-write barrier or parser separation makes the
corresponding regressions fail. The two native KAM/SRM conformance cases pass;
the full integrated conformance run verifies the 67 intentional policy
exceptions. No case in that list remains an unexplained missing feature.

The integrated run records **459 passes, 41 upstream known-bug results and
67 intentional expected failures**. The known-bug count is not a pass count:
the pinned suite's decorators can skip a test before its assertions or catch a
failure as `KnownBug`. In addition to the individually named 67 exceptions,
the following table accounts for all 41 known-bug results under this profile.
Names below are the suite's test method names with the common `test_` prefix
omitted.

| Count | Cases | Selected policy or coverage limit |
| --- | --- | --- |
| 11 | `APC_8bit`, `DECID_8bit`, `HTS_8bit`, `IND_8bit`, `NEL_8bit`, `PM_8bit`, `RI_8bit`, `S8C1T_CSI`, `S8C1T_DCS`, `S8C1T_SPA_EPA`, `SOS_8bit` | Decorators require `disableWideChars`. This UTF-8 profile does not enable legacy eight-bit control processing. |
| 19 | `DECRQM_DEC_DECAAM`, `DECRQM_DEC_DECARM`, `DECRQM_DEC_DECARSM`, `DECRQM_DEC_DECCANSM`, `DECRQM_DEC_DECCRTSM`, `DECRQM_DEC_DECESKM`, `DECRQM_DEC_DECHDPXM`, `DECRQM_DEC_DECHEM`, `DECRQM_DEC_DECKBUM`, `DECRQM_DEC_DECKPM`, `DECRQM_DEC_DECMCM`, `DECRQM_DEC_DECNAKB`, `DECRQM_DEC_DECNULM`, `DECRQM_DEC_DECOSCNM`, `DECRQM_DEC_DECPCCM`, `DECRQM_DEC_DECRLCM`, `DECRQM_DEC_DECRLM`, `DECRQM_DEC_DECVCCM`, `DECRQM_DEC_DECXRLM` | Fixed or unmodeled device/peripheral modes; the suite marks these unsupported or permanently reset. No hardware capability is fabricated. |
| 3 | `DECRQSS_DECELF`, `DECRQSS_DECLFKC`, `DECRQSS_DECSMKR` | Locator, local function-key and modifier-key settings are unmodeled. |
| 1 | `DECSCL_RISOnChange` | Upstream explicitly does not try this case. Portable reset tests are separate evidence, not a native pass of this test. |
| 2 | `DECSED_doesNotRespectISOProtect`, `DECSEL_doesNotRespectISOProtect` | Selective ED/EL retain xterm-compatible ISO protection. ISO protection also applies to ordinary ED/EL/ECH; DECSERA remains DEC-only. |
| 5 | `ManipulateSelectionData_default`, `SMTitle_SetHexQueryHex`, `SMTitle_SetHexQueryUTF8`, `SMTitle_SetUTF8QueryHex`, `SMTitle_SetUTF8QueryUTF8` | Decorators require `xtermWinopsEnabled`. Clipboard/title disclosure and program-controlled window policies remain enforced. |

These classifications document this pinned run; the expected-failure guard
compares the 67 failing test names. It does not independently freeze the suite's
41 known-bug decorator outcomes. Neither count establishes complete VT, xterm
or Unicode conformance.

**ucs-detect** (`just conformance-ucs`) measures how many columns the
terminal gives Unicode text, against `wcwidth`. Kettle 5.7.0 scored 99.6% on
narrow and 94.9% on wide characters, where the misses are the 26 regional
indicators, which `unicode-width` makes one column wide. Grapheme clusters
score low, 0.7% for emoji ZWJ sequences, 50% for VS16, 0% for VS15, and 84%
across 85 languages, chiefly Indic conjuncts. Kettle measures each codepoint
separately in that release, so a cluster takes the sum of its codepoints'
widths. New terminals now default to Unicode 18 extended-grapheme cells (mode 2027);
`grapheme-clustering = false` restores scalar layout. These historical scores
do not measure the new default.

`just grapheme-tables-check` reproduces the generated tables from the pinned,
licensed inputs without network access. Patched-engine tests run all 853
Unicode 18 boundary vectors and a million-scalar stream with finite context.
UI `c08_` regressions cover default emoji clusters, the config opt-out,
DECRST/DECSET/DECRQM, RIS/DECSTR for both defaults, Settings translations and
reload isolation through real pane creation. Core `grapheme_` tests cover
portable mode/query/reset/save semantics, UTF-8 splits,
regional indicators, modifiers, Indic conjuncts, zero-width prefixes, storage
limits, late VS16, VS15 narrowing, margins, insertion, REP, overwrite,
copying, search and reflow. The vendored `grapheme_width_tests` additionally
check Mc marks (including GCB=Extend), Indic conjuncts, the Sinhala ZWJ
tailoring, Thai/Lao spacing vowels, standalone modifiers, VS15 spacer cleanup,
and scalar-mode preservation. Their edge, DECSLRM, autowrap and insertion
matrices check cursor columns and cell flags; UTF-8 split and storage-cap
cases check streaming state. ASCII accent regressions assert the actual cells,
cursor, spacer flags and rows after insertion and bottom-margin autowrap;
core cases also cover UTF-8 splits, margins, history and the scroll journal.
They fail on the old production shortcut, independently of the ASCII run
comparison. Core regressions check resized cluster columns
through copying, search and reflow. Selector regressions check interrupted VS16
after combining marks, ZWJ and VS15, and Prepend plus VS15 after width saturation.
They exercise mode 2027, UTF-8 splits, cursor and spacer state, screen and DECSLRM
edges, autowrap, insertion, ignored-control interruptions and discarded scalars.
Core cases also check edge wrapping and selection, search and reflow columns.
Renderer tests exercise shaping metadata and row
keys, plus real headless GPU pixels for both renderer settings, unsupported ZWJ
fallback, monochrome presentation and cached cursor frames. Tests that cannot
obtain a GPU report an explicit `GRAPHEME_*_GPU_SKIP`; a portable test pass does
not establish pixel coverage.

Menu chrome is pinned by headless GPU pixels. `kettle-render`'s
`menu_levels_and_pickers_paint_rounded_panels_on_a_soft_shadow` checks each open
menu level and a picker card for rounded corners, a shadow that fades out and
sits heavier below, an inset selection with no accent rail, and a picker query
and caret above the card. `submenu_covers_its_parents_selection_labels_and_shadowed_edge`
checks that a submenu's panel and shadow cover its parent's selection and
labels, `submenu_edge_hides_its_parents_selection_and_labels` that its bordered
edge is opaque where it crosses them, and `an_open_menu_covers_a_toast_beneath_it`
that menus cover toasts.
`menu_visual` checks that the corners of everything the menu changed stay
untouched. `kettle-ui`'s `painted_menu_rows_match_hit_testing_and_accessibility_bounds`
holds pointer hit testing and screen-reader bounds to the renderer's painted
rows, `kettle_render::context_menu_row_rects`. Without a GPU the pixel tests
print a skip and pass.

`kettle-render`'s `emoji_shaping_tests` use only the bundled JetBrains Mono
Nerd Font for default-emoji/VS16 raster measurements, VS15 exclusion,
aspect-ratio fitting and single-image fallback. A separate host-font test
checks supported ZWJ and skin-tone compositions; it reports `EMOJI_FONT_SKIP`
when a host lacks coverage. Portable glyph-emission tests check one shared
scale and preserved shaped offsets for lightning plus a combining acute in
both renderer settings and both 2027 modes, without a GPU. A portable Indic
ZWJ classification test excludes non-emoji from the single-image rule; a
host-font test also verifies the two-image Devanagari Sangam MN conjunct,
reporting `INDIC_FONT_SKIP` when that face is unavailable.
Arabic joining is checked against the isolated
form, with distinct initial, medial and final glyph IDs, cell metadata and
optional ligatures both on and off. The bundled fonts lack Arabic, so that
test requires a host fallback and reports `ARABIC_FONT_SKIP` if unavailable.
`emoji_ink_is_fitted_without_changing_grid_widths_or_baselines` checks actual
glyph instances using the bundled font in both renderers and both 2027 modes;
it reports `EMOJI_SIZING_GPU_SKIP` without an adapter.

`kettle-render`'s `mark_fallback` unit tests need no GPU. They check the
combining-class table, the placement maths (gaps, stacking, below marks that
never rise, above marks that sink halfway, aligned and double classes), whole
pixel rounding, the quarter-line overflow limit and the bounded cache. With
the bundled JetBrains Mono alone, anchored marks (`Z\u{308}`, `q\u{323}` and
mark-to-mark stacks) keep their place; unanchored ones (`#\u{301}`,
`@\u{323}`), `.case` substitutes and a dot after a composed `Aacute` clear
their base; and with `mkmk` turned off a stacked acute no longer follows the
diaeresis. `mark_fallback_tests` render headless frames: font-placed marks
use an isolated database of bundled JetBrains Mono and Fira Sans and must be
pixel-identical with fallback placement removed. Portable fixture checks verify
the selected glyphs and faces, zero-offset anchors, and the shaped offsets
controlled by `mark` and `mkmk`; Fira Sans anchors the grave below that
JetBrains Mono lacks. A steady frame or zoom must reuse the cached placement,
a row laid out late must still be placed, and a focused block cursor must
cover a moved mark with its inverted
copy. Switching between a moved-mark cell and a built-in shade also checks
that only the current cell contributes cursor patch bounds, with queue and
mapped uploads and pixel-identical crops in both blink phases. `cursor_marks`
tests grow the cursor's own atlas past its first texture
across several cells and check when it must be replaced. The bundled cases
run on every GPU host; `Z\u{324}\u{308}`, `a\u{300}\u{316}` and a tall stack
need a host fallback face and report `MARK_FONT_SKIP` without one. Without an
adapter the tests report `MARK_GPU_SKIP`.

Core `overline_is_independent_and_reported_and_reset` covers SGR 53/55,
DECRQSS, reset, saved cursor attributes and decorated blanks. Parser and
renderer tests cover attribute dispatch and top-of-cell quads, including
conceal and coexistence with underline/strikeout. The esctest expectation
file has no SGR 53 exception to remove. Live conformance and rendering remain
separate acceptance checks.

The generator round-trip check runs with implicit text encodings treated as
errors on every platform. It generates into a temporary directory, compares
the bytes with the checked-in tables, and rejects missing or stale output.
Unicode inputs and generated Rust are explicitly UTF-8, including on Windows
where the default encoding may be a legacy code page.

Compare the same candidate in both modes with fresh output directories:

```sh
python3 scripts/check-conformance.py ucs --kettle /path/to/kettle --grapheme-mode off --ucs-all --out /private/run/ucs-off
python3 scripts/check-conformance.py ucs --kettle /path/to/kettle --grapheme-mode on --ucs-all --out /private/run/ucs-on
```

The pinned ucs-detect suite normally enables clustering when supported. The
`off`/`on` adapter holds the mode throughout the suite, including language
measurements, and forces an actual DECRQM query inside every mode context. It
writes `mode2027.json` and restores the
previous mode on success or failure. An unsupported or unconfirmed fixed-mode run
fails before measurements. `auto`, the default, retains the upstream context;
none of these options changes expected widths, scoring or capability reports.
`--ucs-all` forwards the pinned tool's `--all` flag and includes every width-table
entry; without it upstream measures only
contested entries. Fixture launches remove inherited color overrides so Blessed
queries the terminal capabilities. The parent environment is unchanged.


The 2026-11-01 macOS run used ucs-detect 2.3.8 with `--ucs-all`, the same
candidate binary and fresh panes in each mode. All ten recorded mode contexts
confirmed support and the requested state. These are the suite's reported case
counts, including its cached language results, rather than independent samples.

| Measurement | Cases | Mode off | Mode on |
| --- | ---: | ---: | ---: |
| VS15 | 158 | 0.00% | 0.00% |
| VS16 | 426 | 50.00% | 100.00% |
| ZWJ emoji | 1,445 | 0.69% | 100.00% |
| Regional-indicator pairs | 262 | 100.00% | 100.00% |
| Single regional indicators | 26 | 0.00% | 100.00% |
| Fitzpatrick modifiers | 5 | 100.00% | 0.00% |
| Narrow codepoints | 36,254 | 99.94% | 99.93% |
| Wide codepoints | 44,568 | 99.94% | 99.99% |
| Language sequences, 118 language entries | 25,043 | 94.35% | 79.17% |

These scores compare with the tool's width model, not solely with mode 2027,
and predate the cluster-width corrections. In that run VS15 retained its
allocated width and standalone modifiers attached to the preceding cell,
measuring zero advance. All 3,651 directly measured language mismatches in
that mode were one cell versus the tool's expected two; another 1,565 failures
inherited those results from a language sharing the sequence. The current
width rules address those spacing-mark/conjunct, VS15 and modifier cases;
portable regressions do not establish new ucs-detect percentages. Narrow-codepoint
differences included zero-width prefixes and marks, soft hyphen, and two Khmer
width choices; the five remaining wide-codepoint differences were the same
modifiers. The full YAML retains each mismatch. Rerun the live suite before
claiming updated scores or complete Unicode rendering conformance.


## Manual / interactive checks

Use these for behavior that fixtures or synthetic input cannot establish.
Run the relevant automated smoke first; do not repeat its scripted steps here.
Use the exact release bundle for release acceptance.
Record the OS, display backend, GPU, renderer mode, scale, and application build.
Keep failures and skipped areas with the evidence.

### Native macOS page-up qualification

Run the bounded reproduction against an identified binary:

```sh
launchctl asuser "$(id -u)" python3 scripts/check-pageup-stress.py \
  --kettle /path/to/kettle.app/Contents/MacOS/kettle --out-dir /private/tmp/pageup-run
```

The default is ten fresh windows and 200 page-up/down pairs: 100 control
actions, 50 keybinding dispatches and 50 native Shift+Page Up/Down pairs.
Each window must first contain 1,600 confirmed output lines; an echoed shell
command is not readiness. Every up must enter scrollback and every down must
restore offset zero. The fixture uses `/bin/sh` without user startup files.
Keep the desktop unlocked and allow the owned window to take focus. Native
input is refused if that window cannot be identified as frontmost.

The new output directory retains executable/script hashes, build version,
OS, process identities, before/after JSON, visible PNGs, load and failure
evidence. `--windows` accepts 1 through 30. The portable negative controls run
in `just script-fixture-self-test`. Passing this bounded check is not a
root-cause explanation for earlier unidentified intermittent failures.

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
After the suspend/resume cycles it sends the client's `/quit` command and
verifies shell editing again; Ctrl+C can clear an idle composer without exiting.
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
- [ ] Check the SSH launcher's `~/.ssh/config` import with a scratch `HOME`
  only, never your own: create `$HOME/.ssh` at `0700` and a `0600` `config`
  with two `Host` lines, then start Kettle with that `HOME` and press
  `Ctrl+Shift+S`. Both aliases show `from ~/.ssh/config`; typing a third name
  offers **Connect to "…"**; `-oX` shows the refusal row. Repeat with
  `ssh-config-hosts = false` (no aliases) and on Windows with `USERPROFILE`.
  Run VoiceOver on macOS and Orca on Linux against the same fixture. Keep the
  owner's real aliases out of screenshots and audit artifacts.
- [ ] Check Save and Remove with the same scratch `HOME` and a scratch
  `--config` file. Type `me@db.example.com`, press Down to the Save row and
  Enter: the name editor suggests `db.example.com`; Enter adds the
  `ssh-host` line and reopens the launcher on it with `Saved "…"` announced.
  Save a new target under the same name and confirm the Replace question
  starts on Cancel. Type the name exactly, press Down to Remove and confirm:
  the line goes and `~/.ssh/config` is unchanged. Check that the wheel, Up
  from the first row and a hover never select either action row, and that a
  host added to the file by hand before the reload makes Save refuse.

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

Development and test builds use `line-tables-only` debug information to keep
hosted-runner artifacts within disk limits while retaining source locations in
backtraces. These workflow overrides apply to the dev and test profiles.

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
- Required search-history, text-presentation, split-exit-resize, program-keys,
  color-scheme and steady-uploads live steps. Artifacts are retained on failure.

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
  Child fixtures that change `umask` must restore it before returning: LLVM
  writes profiles at process exit. The missing-parent creation fixture is
  covered by this job. Leaving its `0400` mask active creates a profile without
  owner-read permission; with Rust 1.99.1's LLVM 23, merging that profile traps.
  Its unit test also compares ordinary file creation before and after the
  fixture, so leaked process state fails without coverage instrumentation.
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
A green matrix can include an explicitly skipped native prerequisite or an
optional early-warning check. Read the output for the behavior you intend to claim.

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
Short notices use a content-sized capsule; two-row notices retain inline
controls and longer notices stack them in a rounded card. Geometry tests cover
fractional scales, translated labels and large fonts. GPU checks verify clear
corners, text above the fill, focused-control fading, retained uploads and
complete removal. Pointer ownership, hover pauses and cursor feedback share
the rounded body's hit test; inset controls retain rectangular click targets.

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

The Linux window-app-ID smoke copies the selected binary to a private
`kettle-fork` executable, reads its native `WM_CLASS`, unlinks only that copy,
and creates another window in the same process. Both windows must retain
`kettle-fork`, rather than adopting Linux's ` (deleted)` executable suffix.
Control clients use the untouched input binary, and the fixture reaps its
process before removing its private configuration. Portable Rust tests cover
unavailable paths and a cached executable path surviving replacement. X11 and
Wayland receive the same startup identity through winit's shared name field;
the X11 test does not establish a native Wayland compositor observation.

### Shared modal editing and dismissal

`kettle-ui` tests exercise the shared editor on ASCII, combining marks, emoji
ZWJ sequences and CJK, then wire title, command, SSH, layout, theme and Settings
path state through the same editor accessors. They cover keyboard commands,
clipboard failure, selection replacement and mid-string IME projections. Native
macOS modifier tests apply every Option-as-Alt policy before word commands in
each modal field. Title and Settings path initialization tests reject oversized
ASCII and UTF-8 values and accept values at the byte cap. Pointer composition
tests model an AppKit phrase surviving winit's IME toggle and require explicit
native cancellation between disable and enable before caret movement. They reject
a queued old commit and accept fresh composition at the new caret. Run
`cargo test -j 2 --locked --offline -p kettle-ui review_pointer_cancels_platform_composition_before_moving_caret`
for this regression. These tests inject the toggle and native cancellation; they
do not run a native IME. For native verification, compose a phrase, click elsewhere
in each modal field, and continue composing; only the new phrase should commit at
the clicked position. The shared outside-press mutation sink is exercised for
every transient popup,
Settings capture and confirmation, including the context menu's right-click
exception. Native and control input both use this sink.

`kettle-render::text_field` tests load the bundled fonts without a GPU and
compare caret and selection placement with the shaped pointer stops, including
long scrolled inputs, grapheme clusters, CJK and RTL. Fitting RTL text must keep
all shaped clusters visible and retain its full selection width at every caret
boundary, including Home and stale logical scroll offsets. Localized prefix/hint slot
tests cover English and Spanish at narrow widths. Settings value-slot tests compare
the editor's start with the fitted label and gap at terminal cell-width scales
0.5, 1 and 2, including narrow panels and wide labels. These unit checks do not
replace native IME candidate-window, pointer drag, clipboard and live-GPU checks
on macOS and Linux; record those separately when run.

### Independent FFmpeg stream progress

The native `kettle-media-native` FFmpeg tests include a real Matroska fixture
with a 1.6 s interior audio gap. The test holds video after 400 ms and requires
a post-gap PCM timestamp without consuming future pictures. The shared decoder
stalls at the two-second test deadline; independent contained helpers pass.
A separate short-audio fixture checks that a seek past its samples keeps a
supported status and returns clean audio EOF. A deterministic queued-video
regression verifies cancellation after the video helper has already ended;
it fails when helper EOF is mistaken for consumer EOF.
Other tests retain timestamp/sample continuity across seeks, delayed audio,
audio before video, and video progress after PCM cancellation. The fallback
stand-in records both attempted helper PIDs and verifies both are reaped before
the replacement video-only session is accepted. Metadata tests reject wrong
stream kinds, malformed headers, out-of-order timestamps and oversized packets.

Three short macOS probes of the interior-gap test observed at most two helpers.
The complete test invocations took 0.11--0.15 s and 0.04 s of reported user plus
system CPU. The largest sampled simultaneous helper RSS sum was about 29 MiB;
sampling can miss the true peak, and these small-fixture measurements do not
establish steady-state or large-video throughput. Production footprint limits
and the aggregate playback CPU allowance remain in force.

### Native command correlation

Build the candidate and run this on a macOS or Linux desktop. The output
folder must be new:

```sh
python3 scripts/check-command-correlation.py --kettle ./target/debug/kettle \
  --output /tmp/kettle-command-correlation-evidence
```

The harness uses private startup files and history paths, copies the candidate
binary, and exercises each available Bash, Zsh, Fish and PowerShell interpreter
inside isolated Kettle windows. It checks exit/status propagation, persistent
shell variables and positional arguments, eight earlier queued command lines,
stale and untagged completions, multiline input, synchronized-output capture,
Zsh Vi command mode, Fish Vi normal mode, read-only rejection, timeout recovery
and pane-closure errors. Bash Vi and Fish editor-hook withdrawal check
strict refusal before a side effect. Bash 3.2 checks legacy fallback and strict
refusal before the fixture side effect. Results and owned-process identities
remain in the evidence directory. `--shell NAME` selects an installed test leg;
unavailable default legs are explicitly skipped.

Readline disables bracketed paste while queued Bash commands execute. The
admission helper waits at most five seconds through busy responses and the
specific strict Bash multiline refusal, which writes no input. It records
all admission attempts. Other shells, single-line requests and unrelated
parameter errors are not retried; a permanently disabled paste mode still
fails. An executed command's failure or timeout is returned without retry.
Portable regressions cover these boundaries. A delayed native Bash prompt
provides a control for the transition before Readline enables paste again.

The Linux build job runs it under Xvfb; macOS runs it on its native desktop.
Windows runs the PowerShell token, native-status, bounded-input and binding
ownership and post-installation binding-replacement fixtures through
`just shell-integration-check`, alongside portable
parser, controller and client tests. This Unix desktop harness does not claim
live Windows console coverage.

Raw unpaced typeahead above the platform input limit is a separate limit:
a direct macOS Zsh/PTY control dropped the target from a 48-line burst without
Kettle or tokens, while an eight-line control delivered all commands.
Correlation does not make arbitrary terminal input lossless. Keep that failed
stress evidence separate from correlation acceptance.

Characterization tests retain two legacy constraints: an activation claimed
before confirmation times out cannot be rolled back, and a JSON exit event
already blocked in the inherited stdout sink retains its original child code
after a late stop. These tests document the existing contracts; they do not
claim either race was fixed.

The synchronized-output regression previously returned exit status 1 with an
empty capture in Bash, Zsh and Fish. Correlated boundaries now share the
bounded DEC 2026 marker journal: portable tests cover byte-offset anchors,
explicit and forced flush, original marker times, cancellation and overflow.
The native probe also leaves synchronization active through command completion
and requires the command's text in its successful capture. A matching
completion wakes the semantic owner even when paint wakes are disabled;
stale or duplicate completions do not create that wake.

Command checks retry explicit `busy` refusals before input is written. A
supported shell waiting for its next prompt returns `busy` for both strict and
automatic submissions. Unsupported capability and other errors fail the check;
a timeout is never retried by this harness.

Fish's native recorder trace confirms its own `A;click_events=1` follows the
bundled integration's versioned A. The capability tracker preserves that
legacy duplicate while recognizing explicit `A;kettle=0` withdrawal, including
without an intervening C. Portable extractor/tracker tests cover every chunk
split and BEL/ST; the native Fish editor-hook regression fails with the old
stale capability. Bash Vi refusal fixtures select Vi at shell startup, preserve
stock key bindings and exit explicitly. Ubuntu Bash 5.2.21 can crash on `set -o vi`
before its first Readline prompt even without Kettle integration; startup
`-o vi` avoids that interpreter bug. The fixture does not delete a Readline
prefix during initialization. Fish stock-prefix checks compare bindings before
and after installation: Fish 3.7 uses `fish_clipboard_copy` where newer Vi
maps use `fish_vi_dec`. Bash 3.2 also runs the legacy fallback and strict-refusal checks.

Bash prompt revalidation accepts both `bind -X` display formats used by
Readline 5.2 and 5.3. Function and macro inventories are checked separately
from shell callbacks: an owner macro containing the same text as the callback
stays owned. The positive fixture installs the integration and revalidates it
across repeated prompts; it fails with the old colon-rejecting guard.

The native Fish fixture explicitly selects the `default` map after returning
from Vi to Emacs. Fish 3.7 under `--no-config` has no variable-change hook to
reset the active map; leaving it in `insert` removes ordinary Enter handling.
The native failure path captures screen/state and a screenshot before closing
the fixture, preserving the original failure even if diagnostics fail.

The Fish withdrawal fixture explicitly emits `fish_prompt` after changing the
binding. A repaint alone can reuse the cached prompt without running that hook,
so it cannot establish a new integration capability. Capability reflects the
last hook announcement; it does not reserve future editor state. Native paired
runs use the same hook announcement and show old-build admission versus
new-build refusal before the fixture side effect. Disabled ZLE is separately
refused and covered by the native Zsh fixture.

Ownership fixtures also preserve longer owner sequences underneath reserved
chords in Bash, Zsh and Fish. All three previously advertised support for these
sequences; the guards now refuse correlation before installing any conflicting
binding. Fish inspects complete binding records, including prefix overlap and
mode changes, rather than querying only exact chords.

A native automatic-submission regression delays Fish's prompt hook by 200 ms
and queues eight earlier commands. The old path entered legacy fallback between
D and A and returned an earlier command's exit code 0 and output. The corrected
path refuses input while awaiting the next prompt, then returns only the target
command with exit code 1. Portable capability-state tests retain fresh legacy
fallback and versioned/legacy duplicate transitions. The Fish Vi fixture uses a
private 10 ms Escape delay, waits beyond it, and records the actual active map.

## Linux UI qualification

The manual `Linux UI qualification` workflow builds one release binary and
binds it to the exact source tree and live-UI helper hashes. Three fresh hosted
runner allocations download that same artifact. Each runs 100 repetitions of
all six scenarios: search-history, text-presentation,
split-exit-resize, program-keys, color-scheme and steady-uploads. The last uses
`--screenshots-only`, matching its existing Xvfb scope.

Case artifact directories are resolved before invoking a scenario, so recorder
paths remain valid when a PTY starts in a different working directory. A native
relative-output regression covers the same command shape used by the workflow.

Each scenario starts fresh fixtures and records their PIDs, pre-cleanup state
and actual exit codes. Normal Unix teardown records the signals it sends while
the unreaped process-group leader is still alive. This distinguishes Linux's
normal SIGKILL after freezing the owned group from an unexpected kill. A
nonzero exit can qualify only with a matching live-root signal witness and no
pre-cleanup failure; crashes and unwitnessed signals still fail.
A failed case stops its allocation without retrying. At the deadline, cleanup
freezes and records the supervisor and its descendants through stable process
handles before killing those exact instances, including separate PTY sessions.
Linux child-subreaper containment also drains orphaned descendants after a
supervisor exits. Forced cleanup always fails qualification; its ownership and
closure receipt is retained. Transient inventory failures retry within an
eight-second deadline and remain in the failed receipt. Native regressions
exercise an unresponsive tree and an unrelated decoy that must survive. Case logs, screenshots,
receipts and every completed count are uploaded even when a job fails. The
collector rejects missing allocations, changed artifacts, failures, incomplete
rounds or fewer than 300 successes per scenario. A successful workflow is
qualification evidence for its recorded candidate, not automatic promotion of
any scenario. Preserve failures and restart after fixing their cause.

Dispatch against the final integrated candidate, then download and inspect the
`ui-qualification-result` and three shard artifacts. Promote passing scenarios
only in a separately reviewed change to their existing required Linux job.
Physical GPU and desktop-focus claims require their own native evidence.

[Qualification run 41314378019](https://github.com/Reddimus/kettle/actions/runs/41314378019)
passed all six scenarios 300 times each across three fresh allocations, using
one binary (`1bad2c889198707e03f2013bc91a1f59ce34462ee6e5e65fd6631cf9757f6579`)
and helper (`0017cdd9571ee366b35699b9337045e865b4f6e8a894b3da6bb0520c2d357ae9`).
All 1,800 case receipts passed with closed fixtures, no timeout and no forced
cleanup. The qualified source was `83900fc2`, tree `c2774768`, preserved by the
normal merge at `35a66745`. The earlier teardown-classification and relative
recorder-path failures remain recorded in runs 41308288788 and 41311723497;
none of their counts contributes to qualification. These six steps now gate
the existing required Linux build job. Steady-uploads retains screenshots-only
scope, and these Xvfb results do not establish physical-GPU or desktop-focus
coverage. These hashes identify that historical run, rather than qualifying
later changed helper bytes. A subsequent macOS native run exposed a pending
`CLD_STOPPED` event returned by `waitid` despite `WEXITED`; a stopped child is
not an exit. The exit probe now accepts only exited, killed or dumped events.
The native regression leaves the stop event unconsumed, and the qualification
supervisor fixtures run in both required Unix CI legs. Changed candidates need
fresh qualification receipts; counts are never inherited from an older helper. Native
steady-uploads also checks paced printing. Its measured command and output
repeat a completed warm-up exactly, after an alternate round has scrolled the
old completion marker away. A changed marker is not evidence of a warm glyph
cache. Each round records counters before any assertion; queue-write refusal
and the mapped-work minimum remain enforced. An earlier native run recorded two
texture writes with different warm-up and measured markers; three diagnostic
reruns passed, so that observation does not establish a persistent renderer bug.

The search-history probe waits for focused-match pixel rectangles as well as
match status and the expected visible row. Search may defer viewport highlights
to another frame after navigation; accepting its earlier status produced a
false failure despite a highlighted subsequent screenshot. Portable regressions
cover that transition and still require failure when rectangles never appear.
The qualification supervisor's native pilot exercises its cleanup path; it does
not count toward hosted Linux qualification.

The rounded-toast follow-up received a fresh qualification at
[run 41337919259](https://github.com/Reddimus/kettle/actions/runs/41337919259):
all six scenarios passed 300 times each across three fresh runner allocations.
All 1,800 individual receipts had closed fixtures, no timeout and no forced
cleanup. Source `532cf329`, tree `cf9956a8`, produced binary
`192e479e3643bd1721a2561f12f19930da3dd99b2337ec9e8e33509bc321b6ca`
with helper
`ecb567319447e7ca2a67ba4c46316e10d0e7d8b6003a24c1c24635e9e94a2fc8`.
The normal merge at `f765718c` preserved that exact tree. Earlier qualification
runs retain their recorded source and helper identities.

## Scaling diagnostic scope

[The 2026-11-01 scaling observations](SCALING-2026-11-01.md) cover PTY, native
window and retained-history states. They retain failed admissions and startup
outliers and are explicitly excluded from calibrated performance standings.

## Lua pattern fixture scheduling

The isolated pattern regression requires the expected work-limit error from
all five native call forms in each sandbox mode. Its four-second stalled-call
watchdog advances only when another required call has completed. A final
completion record and successful child exit are both required. The test reuses
one engine per mode, retains runaway-child cleanup, and bounds progress reads
to two bytes. Host scheduling time is not counted as extra pattern work.

## Settings accessibility values

The settings-row regression checks a theme name, a numeric value and an empty
value. macOS list options expose their name as static text, so each non-empty
value is included in that name. Linux retains the value description used by
AT-SPI, and Windows retains its separate name and value. The native macOS
check opens Settings and reads these row names through the accessibility tree;
it does not establish VoiceOver speech output. Control diagnostics retain the
field name alone, including on macOS.

## Static text names on every platform

`theme_picker_readout_is_spoken_on_every_platform` and
`settings_no_matches_is_spoken_on_every_platform` hand the modal projection to
AccessKit's consumer, as the platform adapters do, and read each node's name as
AT-SPI and UI Automation do: a label's from its value, any other node's from its
label. They check the theme picker's footer text (with the lowest colour's
number and the below-3:1 warning) and Settings' no-match line in English and
Spanish. They do not run Orca or Narrator.

## Configured-session and desktop acceptance, 2026-11-01

On native macOS, the configured AstroNvim/LazyVCS copy completed a five-minute,
60-cycle disposable-repository edit/write/scroll soak, followed by clean and
configured split checks. The loaded plugin source and owner configuration
identities stayed unchanged. The real configured Codex CLI 0.166.0 and Claude
Code 2.1.260 authenticated through their existing native configuration and
accepted a synthetic image through native Command-V in ready composers. Both
showed `[Image #1]`, and Kettle displayed its image receipt and preview. No
model prompt was submitted. Clipboard bytes, the Codex configuration and
Claude user-settings files were unchanged after fixture cleanup; the owned
worktree source stayed clean. A prior attempt stopped at a
new-repository trust dialog and used an invalid print-only interactive flag;
it is retained as failed setup evidence, not counted as acceptance.

In an isolated Ubuntu ARM64 GNOME Wayland desktop, real Super-key search
launched the installed desktop entry with recording off and on. Off produced
no recording. On displayed the recording indicator and wrote a valid v2 cast
containing the output marker, ordered timestamps and no input events. The
closed file was mode 0600 in a mode-0700 directory. The installed debug source
build was stripped only in its owned installer staging copy; allocated ELF
sections and entry point matched the frozen original. This is source-install
acceptance, not proof of the final published release package. All desktop and
recording fixtures closed.

Real Japanese Romaji IME checks committed `nihongo` as Japanese text exactly
once, moved the composition caret, cancelled a later composition and changed
focus between owned windows without committing into either receiver. These
were native composition events rather than injected Unicode. Enabled/current
input sources, clipboard, input-menu and dictation settings were restored.
A separate native search-field run committed `nihongo` as `日本語`, moved its
caret and selected `本`. A later converted composition was cancelled through
both IME stages without changing the committed query. The raw terminal
receiver remained empty until an intentional key after search closed. Input
sources, clipboard and the changed input-menu and dictation settings were
restored. Spoken search-result and caret feedback still needs its own
assistive-technology observation.

Native key events also exercised a fixture configured to send Backspace as
0x08. DEC mode 67 set sent 0x08, reset sent 0x7f, and DECSTR/RIS restored the
configured 0x08. Mode 66 changed keypad 1 between `ESC O q` and `1`. KAM
blocked terminal input and local echo while native New Tab and Close Tab
recovery remained usable. After unlocking, SRM reset echoed accepted input
once; SRM set delivered input without local echo. These receipts identify the
pre-toast application source; hosted CI and later renderer checks retain
their separate artifact identities.

The release-cut candidate requires separate appearance and update acceptance.

Actual macOS VoiceOver was also started. Its owned caption window displayed
`Copy version info` and menu-item contextual help while the isolated About
panel was focused. The accessibility tree labels that menu `About Kettle`.
The automatic spoken parent-panel name remains unverified: caption sequences
captured the item and help text, while VoiceOver's own accessibility-tree query
timed out. No audio recording or complete speech-output certification is
claimed. VoiceOver was turned off again, and input sources, clipboard and the
changed utility navigation state were restored.


The post-toast integrated macOS run passed seven available live scenarios,
including foreground idle and paced-printing upload checks. Invoking-process
native-event preflight denied access, so program-key ownership, native line
editing and the page-up stress run were skipped in that invocation. Keep these
skips distinct from the earlier native run and hosted Linux qualification.
Any later manual acceptance must identify its actual source and artifact.

After publication, validate Ubuntu desktop acceptance against the actual public
archive in an isolated prefix and desktop session. Verify its published digest
and installed version, then use Super-key search to launch the installed entry
with recording off and on. Off must create no recording. On must produce a
valid v2 cast with the expected output marker, ordered timestamps and no input
events; the file must be mode 0600 in a mode-0700 directory. Retain archive,
installed executable, launcher, recording and fixture-closure identities.
Uninstall only that prefix and close the owned desktop session. Source-install
and rehearsal observations remain separate from this publication check.


The signed main preflight for 5.8 used the universal 5.7.1 candidate from
[run 41343253350](https://github.com/Reddimus/kettle/actions/runs/41343253350)
at `f765718cfed0`. Its application, vendored, asset, packaging and verification
script sources are unchanged in the 5.8.0 version cut; workspace versions and
release documentation are separate metadata. The same signed bundle passed
modal-shortcut, native-titlebar, Dock-menu and update-toast smokes and six
composited appearance modes. Its system preferences were restored and all
seven owned appearance windows closed.

The manually launched debug run passed nine scenarios before page navigation
failed with an unfocused window. A signed main run passed its first two
scenarios, then its split-size shell probe received an unexpected leading `r`
and failed before measuring the resize; the remaining seven suite cases were
unrun in that invocation. Those interruptions and the separate native-access
skips remain diagnostic evidence. No terminal root-cause fix is claimed and
none of those partial counts contributes to hosted Linux qualification.

The `macos-signing` environment permits `main` and version tags. The branch
rehearsal [41355144293](https://github.com/Reddimus/kettle/actions/runs/41355144293)
passed release pretests, then macOS signing was rejected before a runner
started because the cut branch was outside that policy. The remaining jobs
were stopped and their evidence retained. Keep the protection policy intact:
use the main appearance preflight with unchanged application code for the
version/documentation cut, then run a nonpublishing signed rehearsal of the exact final 5.8.0 `main`
commit before creating its tag. That final gate before tagging requires all ten native
scenarios, 200 page-navigation pairs across ten fresh windows, the four window
and toast smokes, six composited appearance modes, restored preferences and
closed fixtures. Retain its exact source and package identities separately.

For macOS publication acceptance, copy the previously published notarized app
to a private writable folder and run that copy's own `kettle update --yes`
against the production signed feed. Require the new version and source, verify
the resulting terminal/worker identity and bundle trust, then require a second
update to report that it is current. Preserve the original app and the owner's
installation. Record this separately from the candidate archive swap smoke.


Renderer dependency upgrades retain one exact Mermaid family and matching resvg/usvg
versions. The dependency contract rejects either half of a split SVG update.
SVG 0.49 inherits parent baselines in formatted spans; native text metric tests
compare that offset with the raw baseline shift. Filter-layer admission follows
the renderer's rotation/skew decomposition before measuring intermediate layers.

## ccx entry-script acceptance

The 2026-11-03 native Dell acceptance ran on Ubuntu 26.04.1 at
`4d0206a1c1e040e89a5c0b8bade02cc862c58ddd`, with its matching debug terminal
and media-worker build. It used an owned checkout, private HOME and XDG state,
and isolated Xvfb windows. The installed app, shell files, account settings,
and other panes were preserved.

| Check | Observed result |
|---|---|
| Native `just gauntlet` | Passed, including ownership-dependent tests |
| Matching terminal/worker card smoke | Scoped cards passed; unscoped controls returned shelf with `not_launched` |
| Printed wrapper and entry forms | Twelve alias-preservation, parsing, argument and exit-status cases passed |
| Existing `codex()` function with fixture agent | One rendered card and the expected `CODEX_HOME`; the backup function returned shelf with `not_launched` |
| Installed ccx 2.6.26 with fixture agent | Five interactive cases passed through its separate PTY: fresh, `--session-id`, `--resume`, `--continue`, and `--continue --fork-session`; each card retired when its session ended |
| ccx non-session calls | `--version`, `auth status`, and `-p --no-session-persistence -- <prompt>` reached the plain fallback with their arguments |
| Real ccx and Claude Code 2.1.317 | One explicitly approved user prompt called `kettle_show` on a sample SVG; the raw tool receipt reported `status: "sent"`, `delivery: "card"`, `kind: "svg"`, `verified: true`, `model_has_seen: false`, and no `card_reason`; one settled card and rendered colored pixels were captured |

The real-client run used the retained 2.1.317 executable after the installed
launcher updated to 2.1.318. It does not establish real-client acceptance for
2.1.318, other ccx versions, macOS, background sessions, or every content type.
One user prompt can cause multiple client requests; it is not a billing-request
count. Recorded test agents exited before their private profiles were removed.

To repeat the no-model card matrix with matching binaries:

```sh
cargo build -p kettle
cargo build --profile media-worker -p kettle-media-worker
env -u WAYLAND_DISPLAY xvfb-run -a python3 scripts/check-agent-cards-smoke.py \
  --kettle ./target/debug/kettle \
  --worker ./target/media-worker/kettle-media-worker --out /new/private/results
```

See the [entry-script recipe](AGENT.md#wrapping-your-own-claude-or-codex-function)
for ccx. Redirect both ccx state and HOME when testing it: redirecting only
`CLAUDE_CONFIG_DIR` does not isolate ccx's write-back to the user's default
Claude files. Use fixture agents for flag and fallback coverage. Run a real
account prompt only with explicit approval, preserve existing acknowledgement
preferences in the temporary profile, and stop on login or trust dialogs.
Keep credentials and raw account transcripts out of evidence; retain only the
matching tool receipt fields, card geometry, screenshot, source revision, and
owned-process cleanup result. Native ownership tests must see the host's real
UIDs; a user namespace that reports `/` as UID 65534 is not a valid substitute.

### Settings window regression coverage

`kettle-render` settings tests check proportional panel sizing, sidebar versus
single-pane placement at 1280x820, 800x600 and 560x600, nonoverlapping slots,
focused-row scrolling, pinned notes, category/search/chip hits and editor
rectangles. English, Spanish and pseudo-localized text must fit their clipped
slots. `kettle-ui` settings tests check label/description/config-key search,
Spanish accent folding, catalogue grouping, focus routing and disabled rows;
AccessKit tests cover row descriptions and the search text input. Every field
must have a short description in both catalogues, and `kettle-i18n` checks
schema and Spanish draft status coverage. These pure tests do not establish
live rendering or native screen-reader acceptance.

Pointer regression tests walk reachable categories from the initial selection,
and apply overflow-arrow clicks and wheel paging through `SettingsNav` in both
directions. Short-window tests cover the focused Settings editor and a picker
header plus its selected result at 120 pixels, including 24- and 46-pixel top
chrome for pickers. Accessibility action tests focus Search during chord
capture and then type into the query. Resize-chip tests cover Settings, all
four pickers, and a picker card without a query projection. The translated
layout test includes the pseudo-locale only with debug assertions, so its
English and Spanish cases also compile and run in release mode.

Run these checks offline with the local Cargo cache:

```sh
cargo test -j 2 --locked --offline -p kettle-ui -p kettle-render --lib settings_
cargo test -j 2 --locked --offline -p kettle-ui -p kettle-render --lib short_
cargo test -j 2 --locked --offline -p kettle-ui --lib accessibility_search_focus_cancels_capture
cargo test -j 2 --locked --offline -p kettle-render --release settings_slots_fit_english_spanish_and_measured_pseudo_text
cargo test -j 2 --locked --offline -p kettle-ui -p kettle-render --no-fail-fast
```

### macOS menu bar regression coverage

`cargo test -p kettle-ui macos_menu` runs the pure menu model on every platform:
English and Spanish structure, translated titles and separators, action mapping
(the Window menu's Rename Tab… and Move Tab to New Window among it, the move
rows off for a lone tab or `detachable-tabs = false`), Cmd-only hints following remaps and unbinds, and focused-window availability.
`cargo test -p kettle-ui menu_paste_availability` exercises writable local and
cross-window broadcast recipients, read-only focus, and group exclusions using
the same recipient selection as paste delivery.
Source guards check that AppKit only performs native application and Services equivalents,
leaving Kettle shortcuts on the original keyboard dispatch and terminal-writer
path, and that resume, focus/selection dispatch and config reload reconcile the
menu while retaining Services and Windows registration.
`automatic_window_tabbing_is_off_before_the_event_loop_exists` checks that
`run_with` turns AppKit's automatic window tabbing off once, before the event
loop is built, and `move_tab_rows_agree_with_the_wrapping_mux_at_every_position`
pins Move Tab Left / Right as enabled wherever `Mux::nudge_active_tab` moves the
tab, the first and last tab included, since the move wraps.
`help_menu_is_last_and_flagged_with_help_and_shortcuts` checks that Help is the
last menu, flagged for `setHelpMenu`, and holds Kettle Help and Keyboard
Shortcuts in English and Spanish. `menu_bar_follows_macos_conventions` checks
one full-screen item, on Control-Command-F unless the keymap owns it and titled
for the focused window's state; all four splits; the native Minimize, Zoom and
Bring All to Front; and English title case.
`native_window_items_route_through_kettle_in_both_menus` checks that full
screen, Minimize and Zoom keep AppKit's selectors but route through Kettle and
winit, in the visible menu and the key-equivalent menu alike, with Minimize and
Zoom off in full screen.
`cargo test -p kettle-ui --test macos_menu_native` runs the production menu subclass
on the process main thread on macOS. It dispatches synthetic AppKit key events
to native commands and a nested, registered Services menu, checks disabled
Services items, and verifies that enabled and disabled Kettle equivalents are
declined. It uses a process-local Services fixture without installing a Quick
Action; other platforms report this test as skipped. The harness-free executable
also honors a test-name substring filter.
Existing keyboard tests exercise program-ownership yielding and terminal
encoding. These checks do not establish physical keyboard delivery or live
Quick Action invocation; an unlocked macOS session is needed for mouse,
keyboard, System Settings Services shortcuts and window list acceptance.

The command palette's ranking runs on every platform with
`cargo test -p kettle-config --lib -- fuzzy palette`:
`score_rates_the_best_alignment_not_the_first`,
`strong_alignments_start_words_and_run_contiguously` and
`long_inputs_fall_back_to_the_first_match` pin the best-alignment scorer and
its bounded fallback, `weak_matches_drop_out_when_a_strong_one_exists` the
weak-match cutoff, and `highlights_follow_the_ranking_alignment` the marked
characters. For picker geometry, `kettle-ui`'s
`overflowing_picker_rows_start_at_the_header_and_fill_the_panel` and
`picker_rows_and_wheel_scroll_all_four_modal_projections` hold painted rows,
hit testing and accessibility to one layout, and `kettle-render`'s
`picker_scroll_thumb_stays_in_the_right_gap_and_tracks_the_offset` and
`picker_scroll_thumb_is_drawn_last_at_three_to_one` pin the scrollbar thumb.
`bound_only_palette_lists_every_bound_command_with_its_shortcut` and
`enter_in_the_shortcut_list_runs_the_selected_bound_row` cover the Keyboard
Shortcuts list, and `the_shortcut_list_shows_its_placeholder_in_a_standard_window`
its placeholder in a standard window's header in both languages; `kettle-render`'s
`an_empty_picker_header_gives_its_hint_up_before_its_placeholder` holds the rule
that the key hint gives way to an empty query's placeholder.
`picker_key_hint_slot_is_secondary_text` pins the header key
hint's colour without a GPU; `a_picker_header_paints_its_key_hint_as_secondary_text`
and `an_empty_picker_query_paints_its_placeholder` check the frame and print
`PICKER_HEADER_GPU_SKIP` when no GPU adapter exists.

menu while retaining Services and Windows registration. Existing keyboard tests
exercise program-ownership yielding and terminal encoding. These checks do not
establish live AppKit menu interaction; native CI must compile the AppKit layer,
and an unlocked macOS session is needed for mouse, keyboard, Services and window
list acceptance.

### Paste protection and compact context menus

`cargo test -p kettle-config paste_protection` exercises every C0 byte and the
three config policies; `cargo test -p kettle-ui paste` checks paste preview,
flattening and ESC neutralization. `cargo test -p kettle-ui context_menu` and
`cargo test -p kettle-render menu` cover menu grouping, nested accessibility,
shared geometry, panel flipping and clipping ancestor text under submenus.
`cargo test -p kettle-ui menu_text_tests` builds the real Read only row and
Preferences submenu, projects them, and checks that check boxes and radios
reach AccessKit with their state while the painted labels keep their marks,
in the open panel and in a parent panel; the name is unmarked except on
macOS, where it keeps the mark. `cargo test -p kettle-ui
a_toggle_row_keeps_its_marker` checks both namings on any host.
Whether VoiceOver speaks a toggled menu item's state without the mark is
still a live check on macOS.
Native acceptance still needs clipboard, PRIMARY and PuTTY-style text paste,
mixed bracketed/raw broadcast recipients, each of the three dialog buttons,
and hover/click/arrow-key menu navigation at 800x600 in English and Spanish.
Check VoiceOver/AccessKit nesting, resize/DPI changes, and `kettle ctl
ui_geometry` parent-panel bounds against the painted panels. No headless unit
test substitutes for that rendering and assistive-technology pass.

### First-launch shell integration

`cargo test -p kettle-ui shell_integration` exercises the checked private
startup files with native modern Bash, Zsh and Fish when installed.
It checks competing startup variants, compiled-only Zsh startup, earlier Fish
`conf.d/00-env.fish` changes, and one-time loading before child shells. The Bash
`ENV` tests check login state, logout processing, profile selection, suppression
flags, custom rcfiles, restored `ENV`, disabled POSIX/`inherit_errexit` modes and
startup declaration scope. On macOS, native system Bash 3.2 tests verify the
untouched fallback with both default and explicit login launches; a separately
installed modern Bash exercises automatic injection. Missing native interpreters
are reported as skips. `owned_dir::tests::recent_variants_` checks the 24-hour
mtime boundary, reuse refresh and the four-expired-variant cap.
`cargo test -p kettle-core automatic_tests` checks launch construction and the
macOS fallback without a PTY. Command outcome tests cover duplicate and
unknown statuses, bounded retention and shared DPI-aware scrollbar rectangles;
`repeated_output_starts_are_one_command` checks the reader's actual OSC path.
Accessibility tests check English/Spanish exit-code descriptions against that
geometry. Native UI/GPU acceptance still requires rendered ticks on each OS;
headless source and geometry checks do not establish visual acceptance.

`kettle-render`'s `scrollbar_tests` pin the pane scrollbar.
`scrollbar_colours_meet_their_floors_on_every_bundled_theme` sweeps every
bundled theme in both blend modes, solid and translucent: the rest thumb 3:1
against the background and 2:1 against its channel, the active thumb 4.5:1
and 3:1 against its channel, the channel 1.5:1 and at rest at most 1.6:1, and
no translucent layer over 90% cover.
`command_ticks_stand_out_on_the_channel_and_the_thumb_on_every_bundled_theme`
holds ticks, solid and translucent, to 3:1 against the channel and the
background, or against the thumb where they cross it, and
`ticks_split_at_the_thumb_edges` cuts a tick at the thumb's edges. The
geometry tests cover the scrolled-back width and full-height channel, the
rest channel only with history, `always`'s steady width, the 40 logical px
thumb at 1x and 2x, the edge-touching hit strip, the jump button's clearance
at widths 2-40, when the bar paints solid, and, in
`narrow_and_short_panes_keep_the_bar_inside` and `an_empty_pane_draws_no_bar`,
panes narrower or shorter than the bar at 1x and 2x.
`headless_tests::a_pane_with_history_paints_its_thumb_channel_and_ticks`
samples the thumb, channel and ticks on the channel and on the thumb in
rendered frames, at rest and scrolled back, on TokyoNight Night and Kettle
Day at 1x and 2x, solid and translucent, away from any glyph. Linux UI state
captures whose panes have history, such as the narrow `search-matches`,
`search-nomatch` and `splits` states, show the rest channel.
`kettle-ui`'s `scrollbar_tests` check that the pointer grabs the thumb the
renderer draws and that the AccessKit ScrollBar bounds are its painted
channel.

Chrome state regression coverage includes the auto tab default, exit-policy
status matrix, replay of original launch environment and cwd in the same pane,
source-located config diagnostics and notice limits, persistent-notice hit
geometry, and tab-list titles, selection and controls on both strip axes.
These unit and PTY tests do not establish live rendering or screen-reader
acceptance; native UI checks remain required on each platform.

## Quick terminal

Portable tests cover all five positions at multiple screen sizes and offsets,
percentage bounds, the show/hide/autohide/focus-return model, animation endpoints,
config defaults and diagnostics, Settings reads and capture routing, CLI parsing
and activation serialization, including bounded Wayland desktop tokens. Activation transport tests resend a toggle with
its original launch ID and verify that it is applied only once. Pure key-mapping
tests cover the documented Carbon and X11 chords.

```sh
cargo test --locked --offline -p kettle-config -p kettle-ctl -p kettle-i18n -p kettle-ui -p kettle
cargo clippy --locked --offline -p kettle-config -p kettle-ctl -p kettle-i18n -p kettle-ui -p kettle --all-targets -- -D warnings
cargo clippy --locked --offline --target aarch64-apple-darwin -p kettle-ui --all-targets -- -D warnings
cargo clippy --locked --offline --target x86_64-unknown-linux-gnu -p kettle-ui --all-targets -- -D warnings
cargo fmt --all --check
```

Native macOS and X11 CI must compile their registration backends. With a live
desktop, verify repeated CLI/global/menu/palette toggles create one quick window,
focus returns on explicit hide, switching apps autohides without stealing focus,
tabs and splits keep their processes while hidden, monitor selection and all
five geometries work across mixed-DPI displays, and Reduce Motion disables the
slide. Also check a taken hotkey, rebind/clear on reload, CapsLock/NumLock on X11,
window-list/cycling exclusion, English/Spanish Settings, and Wayland/Windows
desktop shortcuts. A GUI-free test run does not establish these native behaviors.

### Quick-terminal review regressions

The `review_quick_` tests exercise cold-launch identity with a Lua startup
`split_right`, real PTY splits and the production session snapshot/write path;
reopen after hide, repeated focus and autohide; move-before-resize placement
with a simulated 1x-to-2x scale change and animation disabled; physical Option
capture independent of terminal modifier filtering; and Settings capture,
config round trips and Carbon/X11 key mapping for shifted US punctuation.
Source guards bind these seams to App construction, session saving and the
existing reveal routes. They do not run a native event loop or register a live
global shortcut. The placement test runs portably, with logical-coordinate
assertions on macOS and physical-coordinate assertions on other platforms.

### Titlebar regression coverage

The `review_` tests in `kettle-ui` and `kettle-render` cover strips below two
pixels, configured tab minimum widths, startup policy ordering, and the exact
40-row startup surface at 1x and 2x scale. Recording-label tests use numeric,
literal-only, repeated-title and path formats, including raw-input and failure
notices. Bundled-font tests check 72-point title-editor input, hints, caret and
pointer slots; the offscreen renderer also compares painting with measurement.
Run them offline with `cargo test -j 2 --locked --offline -p kettle-ui -p kettle-render -p kettle-config --no-fail-fast review_`.
These checks do not replace native window sizing or live title-editor checks.

The `measured_startup_titlebar_` tests cover the hidden-window correction from
an estimated 28-point band to AppKit's measured 32-point band, unchanged and
smaller bands, either explicit grid axis, and default monitor fitting at 1x and
2x. A source guard requires both window constructors to correct once before
renderer sizing and the first reveal. Run them offline with
`cargo test -j 2 --locked --offline -p kettle-ui measured_startup_titlebar_`.
The portable `queued_macos_startup_resizes_preserve_the_corrected_grid` and
`macos_resize_filter_accepts_live_changes_and_precedes_layout` tests replay
queued sizes after the native window has already changed. They cover the
100x30 to 100x29 to 100x30 startup reflow at 1x, 1.5x and 2x, and require
current native resize events to remain eligible for layout. On macOS, the
default-window-size smoke must still report an initial and final 100x30 PTY,
no corrective grid changes and zero child-observed SIGWINCH signals; the
portable replay does not replace that native acceptance.
`macos_native_tab_resize_matches_live_view_instead_of_window_rectangle` also
covers automatic AppKit tabs, where the window-derived content rectangle is
taller than the live NSView frame. It requires the final view-sized event to
pass and stale view-sized events to fail at 1x, 1.25x, 1.5x and 2x, using winit's
logical-to-physical rounding and the native window's backing scale. Source
guards pin the live view query to the existing main-thread native borrow helper.
This portable check does not replace live native-tab, grow/shrink, minimize,
fullscreen or DPI acceptance.
`native_tab_exit_completion_resumes_without_a_later_frame_notification` replays
a held final resize and fullscreen completion after AppKit already restored the
saved frame, so no later frame notification is available. It covers differing
window/view rectangles at four backing scales, pending DPI changes and a missing
view. The old window-sized event leaves the surface suspended; the live-view
completion commits one layout and releases suspension. A source guard connects
the replay to the completion producer and its platform fallbacks. Native
fullscreen exit without a later resize remains a required live acceptance check.
The live macOS check is
`launchctl asuser $(id -u) python3 scripts/check-conformance.py esctest`;
it requires an unlocked desktop and cannot run in the sandbox.

## Cursor and selection regression checks

`kettle-render` covers 4.5:1 highlight ink, maximum-contrast fallbacks,
readable user-foreground precedence, dim/link/ANSI/indexed/truecolour text,
same-hue 3:1 focused fills on the default dark and light themes and every
bundled theme, and inactive fills across bundled themes: grey in the row and
per-cell fills, measured against a see-through background as shown, and
unchanged where nothing shows through; at see-through opacities from 99% to
30%, every bundled theme's focused fill keeps its side of the background and
its unfocused grey stays between them, 1.3:1 from the focused fill. Offscreen tests cover an unfocused
window's grey selection pixels, link and SGR decorations over selected fills;
link underlines over a focused or unfocused selection and over a search match, drawn in the highlighted text
colour at their resting weight; selected cells with their own background; one
selection fill kept across TokyoNight Night program backgrounds it separates
from, with fills of their own only where it does not; inactive split dimming;
steady hollow cursors and DEC cursor hiding. They report `C09_GPU_SKIP`
without an adapter.
`kettle-core` tests both copy policies against printed spaces, blank padding,
blocks, blank rows, wraps, tabs, wide cells, combining marks and byte caps.
`kettle-ui` and `kettle-config` cover selection modifiers, cursor live reload,
Settings round trips and the new key validation. Settings rows continue to use
shared `settings_layout` geometry for rendering, hit testing, AccessKit and
`ui_geometry`; these controls do not add an editable text field or IME anchor.

Run the four crate suites and `kettle-i18n` offline, followed by their
`cargo clippy --all-targets -- -D warnings` and `cargo fmt --all --check`.
Native acceptance still needs focus transitions, split cursors, Option/Alt-drag
and Shift-click on macOS/Linux, IME candidate placement and English/Spanish
Settings at narrow widths. Source and headless tests do not establish live
window or fluent-language acceptance.

### Paste safety and program clipboard feedback

Portable `kettle-ui` tests cover complete and unterminated ESC/CSI/OSC and C1
control strings, DEL, CRLF/CR/LF retention, UTF-8 safety, visible-control previews,
translated summaries, conditional fourth buttons, keyboard consent, and shared
hit-test/AccessKit geometry in narrow windows. OSC 52 notice tests cover per-pane
five-second throttling, Unicode counts, suppression, one-time read/write policy
messages and polite accessibility status. `kettle-config` tests the
`osc52-notify` boolean and control-character consent policy; `kettle-i18n` checks
the English and Spanish draft messages.

Run the full `kettle-ui`, `kettle-config`, `kettle-i18n` and `kettle-render`
suites and their all-target clippy checks. Live acceptance still needs a desktop:
paste text containing controls, navigate all four actions in English and Spanish,
verify screen-reader announcements and cancel focus, then issue allowed and
blocked OSC 52 operations in separate panes and dismiss the notices. Check
`kettle ctl ui_geometry` bounds against each painted button and notice. The
dialog has no IME caret; the notice overlays the terminal without moving its
IME position. Clipboard backend success/failure and native screen-reader speech
cannot be established by the portable model tests alone.


Link and hint UX has portable coverage in `kettle-core` (path context and
prefix-free codes), `kettle-render` (rest/hover/OSC 8 decoration in both
`link-underline` modes, including a mode change on a live renderer,
destination pill avoidance and elision, legend bounds), `kettle-config`
(`link-underline` default, values and typo report), and `kettle-ui` (hover
invalidation, translated gestures and legend actions, label placement, OSC 8
targets, AccessKit nodes, context menus, native menu model and palette
shortcuts).
`ui_geometry` reports `pane_statuses` and `hint_labels` with the projected
bounds; destination text is omitted from diagnostics. These overlays reserve
no terminal cells and leave pointer cell mapping and terminal IME geometry
unchanged. Native rendering, hand cursor behavior, screen-reader announcements
and Spanish layout still require live platform checks.

## Bounded session-output restore

`kettle-core::restore::tests` covers primary-screen capture during alternate
screen use, SGR and wide/grapheme replay, control/hyperlink stripping, per-pane
line and row-byte caps, and content/divider/new-output ordering.
`kettle-ui::session_content::tests` checks the serialized 8 MiB cap, private
round trips, corrupt and mismatched sidecars, unsafe controls, and removal.
Session writer tests cover output-disable cleanup. Config and Settings tests
cover defaults, bounds, both key spellings and persisted values. Run the full
`kettle-ui`, `kettle-core`, `kettle-state`, `kettle-config` and `kettle-i18n`
suites, plus clippy and format checks. Native CI must exercise platform-specific
private-file handling. Live acceptance still needs a graphical session: restore
styled CJK/emoji output, search and copy old rows, inspect the dated divider and
fresh prompt, and confirm geometry/IME and screen-reader positions agree.

System appearance regression coverage lives in kettle-config's default and
legacy-theme tests and kettle-ui's appearance, picker, Settings and native
material tests. kettle-config also holds the default light theme's foreground
to 7:1 and its ANSI text colours (slots 1–7 and 9–15) to 4.5:1 against its
background, pins the bundled TokyoNight Day to upstream's colours, and checks
that Kettle Day keeps TokyoNight Day's surfaces, darkens each text colour
without turning its hue, and credits its sources in its header. Portable
tests cover portal value/filter policy and macOS appearance selection; Linux
private-bus tests cover color-scheme snapshots,
SettingChanged and owner loss using the shared reduced-motion transport.
Native acceptance still requires light/dark switching on macOS and Linux
(Wayland and X11), including multiple windows, open overlays, custom side
palettes, material/titlebar contrast, IME, and OSC 996/2031 subscribers.

kettle-render checks that a live frame shows the configured background alpha,
as an offscreen capture of the same frame does, on PreMultiplied and
PostMultiplied surfaces. kettle-ui's native material tests check that blur
counts as unavailable only on a settled answer: not while a Wayland probe runs
or waits to retry, and not once it has found blur. Settings tests pin the
Window blur row's unavailable note in English and Spanish and check that the footer drops
the blur hint on the Window blur and Background opacity rows while blur is
unavailable; a source guard checks that the Settings projection passes the
window's blur state to both. Live acceptance still needs GNOME Wayland and X11
at 60% opacity with blur on, where the desktop shows through sharply and the
note appears, KDE Plasma on X11 and Windows 10, where the note appears, and
KDE Plasma on Wayland, where blur works without the note.

### Linux header bar

Portable `app::linux_titlebar_tests` cover 46 logical pixel header geometry,
one and many tabs, window-control names/actions and hit bounds in English and
Spanish, resize edges/corners, empty-space button routing, startup grids and
winit feature drift. The existing portal fixture also covers GNOME button-layout
snapshots, live changes and owner replacement on Linux. Renderer tests cover
28-point button geometry, rounded hover/pressed fills and
4.5:1 focused / 3:1 unfocused glyph contrast across bundled themes. Header tests
also cover narrow control lanes, localized tooltips and ctl geometry parity,
when the lone tab is drawn as the title, presses on that title (drag, 400 ms
double-click, window menu, ignored middle press) beside two-tab presses left to
the tab strip, and the leading `+` agreeing across hit tests, AccessKit and ctl
at 1x and 2x with left-side buttons. Renderer tests check that a lone title
paints no tab surface, accent or hover fill and keeps 4.5:1 focused and 3:1
unfocused against the header in TokyoNight Night and Kettle Day, the header
hairline's 1.5:1 floor and its break under an active tab, the `+` tooltip
anchor, and that `≡` is drawn as a 14 by 12 point icon at every font size.
Run the kettle-config, kettle-i18n, kettle-ui and kettle-render suites, then
host clippy with `--all-targets -- -D warnings`. The Linux CI matrix compiles
and runs the same suites on Linux, where the header code is built. Cross-clippy with
`--target x86_64-unknown-linux-gnu -p kettle-ui --all-targets` requires both Rust
std and a Linux C compiler/sysroot for native dependencies.

Live acceptance still requires GNOME/Mutter and KDE/KWin Wayland sessions:
check actual decoration negotiation, fractional scale, minimize/restore/close,
window menus, drag/double-click on empty space and on a lone title, one tab to
two and back, all resize edges, maximize/tile/fullscreen
transitions, inactive colors, localized tooltips, AT-SPI button activation,
IME candidate placement and startup rows. Exercise dropdowns, reorder, tear-off,
redock, tab close/activity/bell marks and config reload, plus X11 and borderless
fallbacks. Portable checks do not establish compositor or rendering acceptance.

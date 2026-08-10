# Audit follow-up — deferred items

The v2.32.0 audit (two exhaustive multi-agent passes: per-crate + seven
cross-cutting dimensions, every finding adversarially re-verified) confirmed
~114 findings. v2.32.0 shipped every HIGH-severity correctness / robustness /
security bug plus the cheap high-value mediums and the full docs sweep. The
items below were **deliberately deferred** because they are large, structural,
or need a live GPU / the maintainer's interactive desktop to validate. They are
tracked here so they are not lost.

## Large / structural

- **RUSTSEC-2026-0192 upstream exit for `ttf-parser`.** Issue
  Reddimus/kettle#36 stays open until `ttf-parser` disappears from the dependency
  graph. The only accepted temporary path is `glyphon → cosmic-text → fontdb`;
  `scripts/check-ttf-parser-scope.sh` guards that scope in CI. Upstream tracking:
  RazrFalcon/fontdb#90, pop-os/cosmic-text#352, and grovesNL/glyphon#123. Close
  this only after updating the text-rendering stack, confirming
  `cargo tree -i ttf-parser` reports no matches, and removing
  RUSTSEC-2026-0192 ignores from `deny.toml` and `.github/workflows/audit.yml`.
- **`app.rs` god-file split + testability seams.** Extract dispatch / frame /
  modals / ctl-glue into focused subsystems and make per-event handlers return a
  typed `Outcome` command list (pure deciders + a thin applier), replacing the
  source-text drift guards with behavioral unit tests. Large; best done as its own
  multi-session refactor after the small correctness fixes have settled.
- **Vertical-list pickers.** The command-palette / layout / ssh pickers render as a
  one-line bottom strip that clips matches on narrow windows; rework into a
  scrollable vertical list reusing the context-menu panel machinery (also makes
  room for per-row keybind hints).

## Terminal / protocol

- **Kitty graphics acknowledgement/query response path.** Immediate
  acknowledgements/errors and capability-query replies still need bounded
  `Chunk::PtyReply`-style plumbing, so probers such as `kitten icat` can
  distinguish supported operations from silent failure.
- **Kitty image-id replacement lifecycle.** Retransmitting data for an existing
  id must remove every old physical/virtual/relative placement before retaining
  the new pixels, including for transmit-only `a=t`. The decoder and terminal
  core need an ordered replacement signal rather than inferring this from a
  later placement.
- **Kitty relative `Q=` selection.** Relative-chain origin maps currently
  collapse concrete parents by image id; preserve the full `(image id,
  placement id)` key so a child selects the exact named parent when one image
  has multiple placements.
- ~~**Inline-image scrolling inside partial DECSTBM margins.**~~ Done in the
  next release: the terminal engine now emits bounded, ordered scroll-region
  events with direction, margins, count, and monotonic screen-top ids. Images
  wholly inside the margins move and permanently crop their destination/source
  range at an edge; images already crossing a margin remain fixed. Full-screen
  and top-anchored coalesced scrolls preserve exact history anchors, while
  overflow clears both graphics buffers and resynchronizes fail-safe.
- ~~**OSC 52 selection target** (`p`/`s` vs `c`).~~ Done in the next release:
  `ClipboardType::Selection` now reads and writes Linux PRIMARY through
  arboard, without falling back to CLIPBOARD on a failed query; platforms
  without a separate selection retain their single clipboard channel.
- ~~**Vi-mode over scrollback.**~~ Done in the next release: Kettle now toggles
  `alacritty_terminal`'s native vi mode and dispatches its `ViMotion`, so the
  engine owns viewport following, history bounds, cursor, selection, reflow,
  and eviction. The UI retains only pane ownership and visual-mode intent.

## UX / feedback

- ~~**Surface malformed-config diagnostics in the GUI.**~~ Done in v2.36.5:
  live reload now fires an edge-triggered desktop notification listing the
  ignored malformed lines (`should_notify_malformed` + `load_reloaded_config`).
- Keybind-capture should warn when reassigning an in-use chord; Settings
  GPU/padding writes should surface a persist failure. Search now has a
  grapheme-aware editor with selection, caret/word movement, Home/End, and
  bounded paste; the command palette, layout picker, and other older text
  overlays still need that editor behavior consolidated behind one shared
  component.

## Performance (measure first)

- ~~**redraw scroll-on-output lock**~~ Done in the next release: tab activity
  and `scroll-on-output` now use each pane's lock-free `output_generation`
  edge. Idle/UI-only frames acquire no history locks, in-place and alternate
  screen output are no longer missed, and the terminal lock is taken only when
  the user opted into `scroll-on-output` and a generation actually changed.
- ~~**Remote-poll tick**~~ Done in the next release: one bounded process-tree
  worker now coalesces the newest roots across the checked-out window and every
  stored window. The UI consumes only completed snapshots; it never walks the
  OS process table. Linux retains no cwd string per process, reads cwd only for
  the selected foreground pid, and applies byte/node/task/argv bounds.
- ~~**`placeholder_tiles` scanned the grid before checking for any virtuals**~~
  Fixed in the next release. `placeholder_tiles` ran `placeholder_cells` — a
  walk of every visible cell under the `term` lock — and only then locked
  `virtuals` to discover the map was empty, which it is in every pane that has
  never received a kitty virtual placement. It now snapshots `virtuals` first,
  drops that guard, and reads the grid only when there is something to resolve.
  Measured on a 300x80 grid of text, `opt-level = 1`, Apple Silicon:
  **127 µs per call**, paid per visible pane per frame, and paid while holding
  the mutex the PTY reader blocks on. The snapshot-then-read order is
  `relative_tiles`' existing discipline, so the ABBA hazard is unchanged. The
  snapshot lives in `virtuals_snapshot`, whose owned return type makes the lock
  release a compile-time property rather than a review note: the previous source
  guard asserted text order and passed on a flattened body that held the guard
  across the grid read, and a brace-depth rewrite of it still passed on a body
  that moved the guard out of its block.
- **PTY I/O worker consolidation:** each pane currently uses a parser thread
  plus a blocking pump thread so DEC 2026 deadlines remain independent of a
  blocked native read. The channel is bounded and buffers are recycled, so this
  is measured thread-count debt rather than a memory-growth bug. Profile large
  pane counts before considering an async/native-overlapped reader or a shared
  I/O service; any replacement must preserve ConPTY teardown, Unix portability,
  parser deadlines, and per-pane backpressure.
- Per-window `FontSystem` sharing; lazy system-font load on first frame. Both are
  speculative — profile on the maintainer's machine before implementing.

## Testing coverage

- **Test scratch directories accumulated without bound, 2026-08-09.** A sweep
  of the Windows machine found **148** `kettle*` entries in `%TEMP%`; the Ubuntu
  machine had its own smaller set. Most came from one helper:
  `activation.rs::test_paths` built a path from the pid and deleted whatever a
  *previous* run with the same pid and label had left — never its own. Since the
  pid varies, every run leaked one directory, forever, on every machine that
  ever ran the suite.

  Fixed by switching it to `kettle_test_support::private_tempdir`, whose
  `TempDir` removes the directory when the guard drops. Verified by counting
  before and after a run: unchanged. The entries already on disk are historical
  and safe to delete by hand.

  Worth noting how it was found: no test failed, no gate fired, and reading the
  helper would have looked reasonable — it *does* call `remove_dir_all`. Only
  looking at a machine that had run the suite hundreds of times made it visible.


- **Ten live-UI scenarios run in no automated gate.**
  `scripts/check-live-ui-smoke.py` launches a real windowed kettle and drives it
  through ten scenarios (`tabbar`, `tab-title`, `tearoff`, `split-titlebar`,
  `zoom-keybind`, `underline`, `agent-tui`, `search-history`, `interaction`,
  `touchpad-scroll`). Its other three `case` values do less: `all` runs those
  ten, `session-check` only asserts that a graphical session is usable and
  returns without launching kettle, and `self-test` exercises the helper's own
  pure functions. **CI runs `self-test` only**, on all three OSes; every real
  scenario is a `just` recipe a human has to remember.

  Note what is and is not missing. CI does launch a real kettle under Xvfb, and
  the Nix workflow's runtime smoke waits until an installed kettle creates a
  visible X11 window. Neither drives the control server, and neither asserts
  anything about what was rendered. That narrower gap — nothing automated speaks
  to a running kettle over ctl — is the real one.

  The recorded reason for the gap was a GPU/interactive-desktop requirement, and
  Linux already has the infrastructure to test it: CI installs `xvfb` and
  `mesa-vulkan-drivers`, and `search-history` was run under `xvfb-run` with
  `LIBGL_ALWAYS_SOFTWARE=1` on Ubuntu 24.04 for this audit.

  **That run failed, and the failure was worth having.** It reported
  `timed out waiting for control server` — the helper's 25-second `ctl
  list_panes` probe never succeeded, so the scenario body never ran. What it
  proves on its own is only that kettle stayed alive for those 25 seconds; it is
  the warning kettle logged alongside it that identified the cause, the
  `002`-umask directory bug fixed in #178 and #180. Three features were disabled
  on that machine with nothing but a log line to say so, and no automated test
  in the repo caught it before #178 added one.

  **Rerun after the fix, 2026-08-09.** `search-history` under Xvfb on the same
  machine now gets **past** the control server — the probe succeeds, the
  scenario body runs, and it writes real screen dumps
  (`bottom.cells.json`, `bottom.screen.json`). It then fails at the next step,
  which is a different and previously unreachable problem:

      kettle ctl screenshot failed: screenshot output file could not be opened:
      private path crosses an untrusted directory edge at ~/Repos/kettle:
      parent /home/kevim/Repos has mode 0775

  The screenshot lands under the checkout's `target/diagnostics`, so the
  private-path verifier judges the *user's* directories. Refusing is arguably
  right for a path kettle did not create, but a screenshot diagnostic is not
  private state, and requiring `~/Repos` to be `0700` to take one is a policy
  nobody would choose. Either the screenshot path should not go through the
  private-file writer, or the writer needs a mode for artifacts the user asked
  for by name. Not fixed here because it is a policy decision, not a bug.

  So the gap is demonstrated and the remedy is partly measured: the scenario now
  reaches and drives a running kettle, which is what no automated test does, and
  the next blocker is known and named rather than guessed at. A first gate should rerun `search-history` on Linux
  now that #178 and #180 are in, and only then pin it — quarantined as
  non-blocking until its flake rate is known over a week.

  Add cases one at a time rather than enabling `all`; whether each survives Xvfb
  has to be established, not assumed. Note that the pointer-driven tear/re-dock
  gesture is **not** in `all` at all — the Python `tearoff` case is deliberately
  mouseless (`perform_action("move_tab_to_new_window")`), and the real-pointer
  version lives in `scripts/check-tearoff-live-smoke.sh`, driven by `xdotool` and
  dispatched only by the Unix `tearoff-smoke` recipe. macOS and Windows have no
  such gate configured today; whether their runners could host one is untested
  rather than known to be impossible.

## Deferred from the 2026-08-09 umask work

- ~~**The config-reload watcher keeps a handle it may never have registered.**~~
  Fixed in the next release. The decision it was waiting on: a config directory
  that cannot be watched is not fatal — everything else works and reload is a
  convenience — so it warns and continues, but does not pretend the watcher
  exists. Both results are now checked, matching the remote-command block below
  it, and a source guard pins that neither `watch(` call is discarded.

  Original entry:

- **The config-reload watcher keeps a handle it may never have registered.**
  `app.rs` ignores the result of both the directory create and
  `w.watch(&dir, …)`, then stores `Some(w)` regardless. If the watch call fails,
  kettle holds a live watcher that is watching nothing and live config reload is
  silently off for the session — the same failure shape as the umask bug it sits
  next to, which is how it was noticed.

  Not fixed with that work because the fix is a decision, not a line: a config
  directory that cannot be watched is not obviously fatal to startup, so the
  choice is between failing the launch, retrying, and surfacing it in the UI
  rather than a log. The remote-command block below it already fails closed
  through the private-file helpers and logs, which is the shape to copy.

## Deferred from the 2026-08-07 full-repo audit

- **~~`handle_action`'s tail resizes unconditionally~~ — fixed.** Recorded when
  PR #168 shipped, because #168's commit message claimed the child "sees one
  resize to the final geometry, or none at all" and that was true only for the
  paths it changed. `handle_action`'s tail called `resize_all` after *every*
  action, so a Lua hook queueing `EditWindowTitle` then `CommandPalette` before
  a redraw still produced a shrink and a grow for a net-zero frame.

  Deferred at the time on a stated risk: that `save_session`, which runs
  immediately after the tail, might depend on the resize having happened. That
  risk turned out to be unfounded when it was finally checked — `SGeometry` is
  the OS window rect from winit and `STab` is the split tree; neither records
  the PTY grid. The tail now marks the frame dirty instead, and a windowless
  path still resizes eagerly because a deferred resize that never flushes is
  worse than an eager one that fires twice.

  Worth keeping the entry rather than deleting it: the deferral was justified by
  an assumption nobody had verified, and presented as a measured trade-off. That
  is a different and less defensible thing than deferring on measured evidence,
  which is what the other items here rest on.

- **The Linux installer smoke's online leg can fail on a transient network
  blip, and it gates every pull request.** Observed on PR #162, run
  31255512657:

      kettle install-online.sh: v2.53.0 must ship a bounded Ed25519-signed
      manifest, but .../v2.53.0/kettle-update-manifest.json[.sig] could not be
      fetched. ... Refusing to downgrade to the weaker same-origin checksum.

  The release was **not** broken: all 14 assets are published, and both
  `kettle-update-manifest.json` and its `.sig` return HTTP 200 on direct fetch.
  The manifest simply could not be retrieved during that run.

  Refusing to continue is the correct response — a missing manifest is
  indistinguishable from suppression, and falling back to the same-origin
  checksum would be exactly the downgrade an attacker wants. So the fix is not
  to soften the check. It is to make the *fetch* resilient: a bounded retry with
  backoff, distinguishing a transport failure from a manifest that is genuinely
  absent or oversized, and failing closed only after the retries are exhausted.

  Not attempted here because `install-online.sh` is a shipped, security-relevant
  script and a release cut is the wrong moment to change how it fetches
  signatures. Until then, a red `build (ubuntu-latest)` on this step should be
  re-run once and investigated only if it repeats.


- **Two `production_source` forms that leave test-only text in the slice.**
  Neither occurs in this workspace; both are recorded so the contract's limits
  are written down rather than discovered later.

  1. `#[cfg_attr(not(test), cfg(test))]` — an item that exists only in test
     builds, expressed through `cfg_attr`. All `cfg_attr` attributes are ignored,
     so the item survives. This predates the shared helper and is not a
     regression.
  2. `/** needle */ #[cfg(test)] fn f() {}` written on a single line. Doc
     backtracking requires the attribute to start its line, so the doc text
     survives the item's removal. `rustfmt` normalises this form, which is why
     the workspace does not contain it.

  Both leave *test* text behind, which can only make a positive guard pass
  spuriously. Neither deletes production text, which is the failure that makes a
  negative guard pass while protecting nothing — that direction is what the
  helper's tests are weighted toward, and it is why these two are deferred rather
  than fixed under release pressure.


- **The `kettle/tests/exec.rs` PTY suite is flaky on macOS — the whole suite, not one test.**
  The test's own comment already records that it "has failed intermittently on
  macOS CI with empty stdout"; this entry adds the missing part, which is a
  measured rate and a decisive answer to whether the v2.54.0 change set caused
  it. `crates/kettle/tests/exec.rs:346` — the `out.contains("agent-marker-7f3")`
  assertion — fails with empty stdout.

  Measured on an Apple-silicon macOS 15 host, `main` at `f67cce6` versus this
  release branch, same machine, back to back:

  | | full 26-test binary | test alone |
  |---|---|---|
  | `main` (f67cce6) | 1 / 25 | 0 / 30 |
  | `integration/v2.54.0` | 2 / 25 | 1 / 30 |

  Three conclusions, all of which needed the numbers rather than a guess:

  1. **It is pre-existing.** 1/55 on `main` against 3/55 on the branch is not a
     significant difference at these sample sizes. The branch did not introduce
     it — and `git log main..HEAD -- crates/kettle/tests/exec.rs` is empty, so
     the test itself is untouched.
  2. **It is not caused by this release's ANSI-stripper change**, which was the
     obvious suspicion because the test runs `--strip-ansi` and
     `crates/kettle/src/exec.rs` was edited here. `main` predates that change and
     still flakes.
  3. **It is not purely a concurrency artifact.** It reproduces with the test run
     alone, so contention between the binary's 26 PTY tests is not required —
     which rules out the cheapest possible fix.

  The one captured diagnostic instance carried "asciicast capture stopped
  (recording I/O failed or finalization exceeded its bound)" immediately before
  the empty read, which points at recording finalization racing the child's
  final flush rather than at the stripper. Not chased further here because it is
  pre-existing and unrelated to this release's scope; it wants its own change
  with its own measurement, and the rate above is the baseline to beat.

  **Corrected 2026-08-08.** The entry above named a single test. That was too
  narrow. Three different tests in this file have now been observed failing on
  `main`, at 2/30 per full-binary run:

  | test | where seen |
  |---|---|
  | `exec_streams_stdout_and_exits_zero` | the measurement above, and a Codex gauntlet run |
  | `exec_record_writes_replayable_asciicast` | CI on PR #164, and `main` locally |
  | `exec_raw_mode_eof_is_explicit_and_does_not_destroy_terminal_replies` | `main` locally |

  So the flake is a property of the PTY harness these tests share, not of any one
  assertion — which also means "re-run once and investigate if it repeats" was
  the wrong stopping rule: a repeat shows up as a *different* test name and reads
  like an unrelated failure. Any pull request can go red at random on the macOS
  leg, and the correct response is to check whether the failing test is in this
  file before assuming a regression.

  The shared suspect remains recording finalization racing the child's final
  flush; all three tests drive a PTY child to completion and read what it wrote.
  Still deferred, but it should be fixed at the harness rather than per test.

  **It reproduces locally — under `cargo test --workspace`, 2026-08-09.**
  An earlier version of this paragraph said it did not, on the strength of
  thirty runs of the exec binary and forty of one test under CPU load. That
  scoping was wrong and the conclusion it invited — "do not try to reproduce
  this on a developer machine" — would have sent the next investigator away
  from the one recipe that works.

  Running the **whole workspace** reproduces it at roughly **1 in 14** on an
  Apple-silicon macOS host: forty-five `cargo test --workspace` runs, one
  failure. The failing test that time was
  `exec_raw_mode_eof_is_explicit_and_does_not_destroy_terminal_replies` —
  the third name in the table above, and the same family of symptom
  (`missing explicit raw-mode EOF diagnostic: ""`, an empty read where output
  was expected).

  So the missing condition is not CPU load, it is **whole-workspace
  concurrency**: many test binaries and their PTY children running at once.
  That is also what CI does, which is why CI sees it and an exec-only loop
  does not. Reproduce with:

      for i in $(seq 1 30); do cargo test --workspace --no-fail-fast; done

  and expect roughly two failures. That is a tractable loop for whoever
  attacks the harness fix, and it is the thing this entry was missing.

  So the test now diagnoses itself. On failure it re-runs the same command
  **without** `--strip-ansi` and reports both, which answers the one question
  the symptom cannot: an empty stripped result is equally consistent with the
  child producing nothing and with the read or strip path losing it. If the raw
  retry contains the marker, the loss is on this side; if it does not, look at
  the child or the PTY read. At roughly one pull request in three, the next
  occurrence should not be long, and it will arrive with evidence attached
  rather than a symptom every candidate cause shares.

  **Rate revised upward, 2026-08-09.** The 2/30 figure above came from repeated
  runs of one binary on one host. Counting the macOS leg of this cycle's pull
  requests instead gives a worse picture: **4 failures in 13 CI runs** — three
  `exec_streams_stdout_and_exits_zero` (PR #176 once, PR #180 twice) and one
  `exec_record_writes_replayable_asciicast` (PR #181), every one green on an
  unmodified re-run. The fourth landed while this entry was being written and
  is the corrected framing arriving on schedule: a repeat showed up as a
  different test name, which is precisely what makes "re-run once and
  investigate if it repeats" the wrong stopping rule. ~31% per pull request is
  not "occasionally"; it means roughly
  one PR in three goes red on macOS for no reason, and every one of those costs a
  human the judgement call this entry exists to answer. Treat the harness fix as
  higher priority than the 2/30 framing implied, and record the run count when
  updating this figure — a rate measured by re-running one binary and a rate
  measured across CI runs are not the same number.

- **`kettle ctl screenshot` times out on macOS, so the live-UI smoke cannot
  finish there.** This is the blocker for `just agent-tui-smoke` on macOS, which
  until this release could never run at all — it gated on
  `DISPLAY`/`WAYLAND_DISPLAY` and skipped with exit 0, reading exactly like a
  pass. The gate is fixed; this is what the smoke hits next.

  What is established, by direct measurement on an Apple-silicon macOS 15 host
  running the bare `target/release/kettle` binary:

  - `kettle ctl list_panes` and `kettle ctl ui_geometry` **work** — the control
    server is healthy and Kettle believes it has a window with sane geometry.
  - `kettle --screenshot out.png` and `--screenshot-menu` **work** (a 61 KB
    96x28 PNG). The offscreen render-and-read-back path is fine.
  - `kettle ctl screenshot` **times out at exactly 10 s**
    (`crates/kettle-ui/src/app.rs:16922`). That path calls `request_redraw()` and
    waits for the presented frame, unlike the offscreen path.
  - macOS System Events reports the process has **0 windows**, and AppleScript
    cannot bring it frontmost.
  - `/Applications/kettle.app` exists and is what real users launch; the smoke
    drives the bare binary.

  So the leading hypothesis is that a non-bundled Mach-O does not register a
  presenting window with the macOS window server, meaning `request_redraw()`
  never delivers a frame and the ctl screenshot legitimately cannot complete —
  a HARNESS problem, with real users unaffected because they launch the bundle.
  **That hypothesis is NOT confirmed.** An investigation that was constructing a
  minimal `.app` bundle to test it died on a network failure before reaching a
  verdict, so the alternative — that the screenshot path itself is broken on
  Metal — has not been excluded.

  Settle it before assuming either. Useful evidence: `CGWindowListCopyWindowInfo`
  at the CoreGraphics level rather than the accessibility level, and whether the
  same ctl screenshot succeeds when driven against `/Applications/kettle.app`.
  If it is the harness, the smoke must drive a bundle; either way it must fail
  loudly rather than skip, because a silent skip is what hid this for so long.

  Consequence to state plainly: **the live interactive leg — tmux, Codex CLI,
  Claude Code CLI, and Neovim/AstroNvim inside a real Kettle window — is not
  verified on macOS.** The non-interactive `scripts/check-agent-cli-smoke.sh`
  passed all 12 checks in the direct host measurement above, including the
  configured-AstroNvim path. Its mandatory Kettle-owned probes now also run in
  macOS CI through `just agent-cli-smoke`; optional machine-local clients remain
  explicit skips there. That non-interactive evidence is the full extent of
  what macOS coverage currently proves.

- **One shared streaming control-state kernel for the three ANSI parsers.** The
  `kettle exec` stripper, the session-log scrubber, and the VT extractor have now
  drifted from each other **three separate times**, and the 2026-08-07 audit found
  holes in all three simultaneously — each in a *different* state, each already
  fixed in one parser and not the others. The 2026-08-07 pass fixed the holes but
  deliberately did not unify them: two of the parsers were being repaired
  concurrently and a unifying refactor would have collided with that work.

  A shared kernel must cover ground/pass, ESC, escape intermediates, CSI,
  OSC, DCS/SOS/PM/APC, 7-bit and C1 introducers and terminators, OSC BEL, raw ST,
  CAN/SUB, ESC-from-anywhere redispatch, split ESC/ST/UTF-8 input across feed
  boundaries, bounded recovery, and explicit UTF-8 lead ownership. Policy hooks
  decide forwarding versus suppression, protocol extraction, size budgets, and
  session resets, so the three consumers share state semantics without sharing
  output behavior.

  Until this lands, treat any fix to one of the three as incomplete until the
  same input has been checked against the other two. The
  `ansi_stripper_control_events_cover_every_state_cross_product` test added in
  this pass is the shape the shared kernel's conformance matrix should take.

- **`scroll_page_up` does not enter scrollback on macOS — reproduced three times.**
  `scripts/perf/kettle-live-probes.py` seeds 1600 lines and then asserts
  `display_offset > 0` after `perform_action scroll_page_up`. That assertion
  failed during macOS comparator development and again in the full comparator
  run, under different machine loads:

      kettle-live-probes: scroll_page_up did not enter scrollback

  Two independent reproductions make contention flakiness the less likely
  explanation, so this is now a **probable real defect** rather than an
  unconfirmed observation. It was recorded rather than fixed because it was found
  during a benchmark run and diagnosing it properly needs a live UI session, not
  a hurried patch. Start from `Action::ScrollPageUp` in
  `crates/kettle-ui/src/app.rs:13204` and the `display_offset` the control plane
  reports through `read_screen`; establish first whether the viewport actually
  fails to move or whether only the reported offset is wrong, because those are
  different bugs.

## Search follow-up and platform evidence

The two-track search audit is recorded in
[AUDIT-2026-07-22-SEARCH.md](AUDIT-2026-07-22-SEARCH.md): track A covers every
search-owning file/crate, public contract, cap, and complexity boundary; track B
covers the 88-frame report, Xvfb/XTest reproduction, Terminator comparison, and
live UI states. The implementation addresses the reproduced signed-history and
soft-wrap failures, but the following evidence cannot be inferred from Ubuntu
unit tests and remains explicit release-environment work until recorded there:

- native Windows 11/ConPTY keyboard, IME, DPI, accessibility, and live renderer
  exercise (plus Windows 11 WSL shell/TUI flow);
- macOS native modifier, IME, accessibility, and Metal live-window exercise;
- installed-release desktop-launch verification after the signed release is
  available, including the Ubuntu Super-key launcher and the user's recording
  configuration;
- deeper authenticated Codex CLI / Claude Code CLI and configured AstroNvim
  sessions beyond deterministic fixtures, where installed credentials and
  configuration are external prerequisites.

Moving retained-history traversal to a worker is deliberately deferred pending
profiling. The current event-loop design advances through nominal 1000-line
ranges after a 500 ms idle deadline but performs only one bounded core work
slice per turn, preserves cursor progress under continuous output, and uses a
quiet-period verification only for non-navigation work. Output-interrupted
explicit navigation stays Results limited until the user retries. Query/reflow
changes invalidate work by scan token. One engine
call and one aggregate core slice are each capped at 64 KiB text; the aggregate
slice additionally yields after 262,144 inspected cells or 256 complete logical
haystacks. One logical haystack is capped at 256 rows, 64 KiB, and 262,144
cells, and one projection at 65,536 spans. Any threaded alternative must
snapshot safely without holding the terminal lock across UI work or applying
results after output, resize, or query changes.

These are sourced from the two audit run outputs and the synthesis plans; pick
them up in priority order (robustness/correctness before perf before polish).

## v2.39.0 full-repository audit (2026-07-23)

The whole-repository audit recorded in
[AUDIT-2026-07-23-FULL.md](AUDIT-2026-07-23-FULL.md) confirmed 59 findings and
shipped 52. The seven below were deliberately deferred as multi-session refactors
or cross-crate plumbing that should not be rushed into one release:

- **`app.rs` god-file split.** Extract along the seams the
  `Action` enum already implies — `action_dispatch`, drag/menu modules reusing
  the `detach.rs` pattern, `session_glue` (ctl/MCP/recorder wiring), and
  `window_lifecycle` — keeping `App` as a struct plus small `impl App` blocks
  split across files. Best paired with making per-event handlers return a typed
  `Outcome` list (pure deciders + thin applier) so the source-text drift guards
  become behavioural unit tests. Supersedes the earlier `app.rs` entry above.
- **`kettle-render/src/lib.rs` module split.** The remaining file still
  interleaves the `impl Renderer` frame pipeline with overlay/menu data types,
  GPU adapter selection, the screenshot capture pipeline, and text-fit geometry
  helpers, even though eight sibling submodules were already carved out. Extract
  in isolation order: `gpu.rs` (adapter selection, no `Renderer` state) →
  `screenshot.rs` → `overlays.rs` (data structs) → `text_fit.rs` (pure helpers).
- ~~**OSC 52 selection target (`p`/`s` vs `c`).**~~ Done in the next release;
  see the terminal/protocol entry above.
- ~~**OSC 133 prompt marks desync once scrollback wraps.**~~ Done in the next
  release: the vendored grid maintains a monotonic `history_origin`; prompt
  marks store stable document-row ids, prune on genuine eviction/reset, ignore
  alternate-screen rows, and clear on reflow rather than targeting unrelated
  text.
- **Command palette / layout picker / SSH launcher stay single-line bars.** Fold
  them into the responsive, multi-row layout the search bar gained in v2.38.0
  (also makes room for per-row keybind hints). Extends the vertical-list-pickers
  entry above.

Two related Kitty findings from the same audit are also resolved in the next
release. Global/frame deletion now crosses the extractor/core boundary and
updates physical, virtual, relative, animation, and stored-image state using
the full selector set; same-read delete/replacement order is explicit.
Physical and relative placements now retain and re-resolve source crop,
destination cells, pixel offsets, aspect ratio, and cursor-movement intent.
These fixes do **not** close the acknowledgement/query, existing-id
retransmission lifecycle, or exact `(image id, placement id)` `Q=` parent gaps
listed above.

## Deferred from the #187 review round

- ~~**Config load applies no directory trust check.**~~ Fixed in the next
  release. Implicit default/profile reads hold a verified parent chain through
  the regular-file open and check leaf ownership, mutation rights and link
  count on Unix and Windows; symlinked configs validate both locations. The
  watcher uses the same read-only verifier after best-effort repair, while an
  explicit `--config FILE` is represented as a deliberate trust grant.
- ~~**Activation test servers cannot be stopped.**~~ Fixed. Test servers now
  own a stop/wake/join guard, so the listener and non-delete-shared Windows
  election lock close before the scratch directory guard drops. The stale
  sweep remains only for process aborts and historical leftovers.
- ~~**The watcher source guard is textual and remains bypassable.**~~ Fixed.
  Registration is a generic candidate-plus-closure helper whose behavioral
  test proves success retains the candidate and failure drops it; the brittle
  source parser was removed.
- ~~**The cache-resolver ownership test can self-skip.**~~ Fixed. The real
  resolver runs in re-executed children with conflicting variables cleared for
  each XDG/HOME/LOCALAPPDATA branch, and kettle-state's base-list helper is also
  exercised with controlled absolute values rather than ambient environment.

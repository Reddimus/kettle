# Deferred audit work

Open work checked against this repository snapshot; completed audit history is omitted.
Each entry names the gap, its source, the reason for deferral, and the closing evidence.

## Security and boundaries

- **D-Bus maintenance notices.** The product graph accepts the informational
  unmaintained notices RUSTSEC-2026-0370/0371/0372 for `zvariant 5.15.1`,
  `zvariant_derive 5.15.1` and `zbus_names 4.3.5`. The all-target exact reverse
  graph and registry sources are guarded by `scripts/check-dbus-scope.py`;
  `cargo audit --deny warnings` remains enforced for every other notice and
  vulnerability. Reassess these exceptions when the guarded Linux desktop
  integration dependencies change. The vendor-validation graph has no waiver.
  Moving to zbus 6 is blocked upstream: the newest `accesskit_unix` (0.24.1)
  requires zbus 5.19 and `atspi` 0.29, and `atspi` 0.30 still requires zbus 5.

- **Upstream `lru` advisory.** `Cargo.lock` still pins `lru 0.16.4` through
  `glyphon 0.12.0`; `deny.toml` ignores RUSTSEC-2026-0253. Deferred because the
  reviewed glyphon path does not call the affected `pop()` method, and a fork
  adds supply-chain surface. Close #207 when upstream accepts `lru >=0.18.2`;
  remove the audit ignores and `scripts/check-lru-scope.py` together.
- **`setsid()` descendants on macOS.** macOS has no child subreaper, so
  `kettle exec` and the in-process MCP `kettle_run` path cannot adopt
  descendants that leave the PTY session, and a timeout or cancellation can
  leave them running. Linux runs each MCP command under its own supervising
  `kettle exec` child, whose subreaper stops escaped descendants on timeout,
  cancellation and termination signals (`crates/kettle/src/exec_supervisor.rs`;
  native tests in `crates/kettle/tests/mcp_supervisor.rs`). On every platform a
  normal exit deliberately leaves detached daemons running. Deferred until a
  macOS supervisor can track descendants across session changes. Close with
  native macOS timeout, cancellation and concurrent-command isolation tests.
- **D-Bus messages sized before they are read.** `zbus` 5.19 resizes its
  receive buffer to a message's declared length (up to 128 MiB) before
  reading the body, and the settings-portal subscription keeps the default
  64-message queue whatever `max_queued` asks, so a hostile session-bus peer
  can make Kettle allocate up to that much per message. Deferred because the
  fix belongs in zbus's transport; Kettle cannot bound it from outside without
  vendoring zbus, which is also blocked on the zbus 6 migration (#633–#635).
  Close by moving to a zbus release that reads incrementally against a cap, or
  vendoring the transport fix.

## Terminal and protocol

- **Extractor policy hooks for the streaming control kernel.**
  Exec/MCP and session logs now share `kettle-vt::control` and its stripper,
  with frozen differential oracles and a state/event conformance matrix.
  `Extractor` and `PrivateOutputFilter` retain their bulk scanners: C1 refusal,
  high-byte escape prefixes, private non-ST ESC handling, and recovery after
  a whole over-cap bulk run differ from the stripper. The private recovery
  boundary depends on feed chunking, so a byte-only transition replacement
  changes existing output. Close with run-aware policy hooks and a frozen
  extractor oracle comparing emitted chunks exactly across existing vectors,
  edge splits and random bytes.
- **`kettle exec` JSON exit event after a late stop.** Once the final
  `Finish` event is queued, a timeout or cancellation can still pick a stop
  status while the blocking final write is in progress
  (`crates/kettle/src/exec.rs`, `WorkerOutput`), so a complete exit event can
  report the child's status while the process returns 124/130/74. Deferred
  because a blocking `write` to an inherited stdout cannot be cancelled
  portably: non-blocking mode is shared with every holder of the pipe, and
  reopening it as a separate description works only on Linux. Close with a
  cancellable writer (bounded writes after readiness on a descriptor Kettle
  owns) and a JSON regression in which the event and the returned status
  agree under a blocked stdout.
- **Activation claimed just before its confirmation times out.** When a
  launch hands its request to a running window, the window can claim it just
  as the launcher's five-second wait ends; the launcher then treats the
  primary as busy and opens standalone, so two windows appear
  (`crates/kettle-ui/src/activation_server.rs`, `crates/kettle-ctl/src/activation.rs`).
  An "in progress" reply would fix new launchers, but launchers already
  installed stop reading after five seconds and cannot decode it. Deferred
  for a protocol decision: acknowledge the claim immediately (and give up the
  completion guarantee), or accept a rare duplicate window from old launchers.
- **`run_command` completion correlation.** `kettle ctl run_command`
  accepts a pane only when the shell looks idle, then takes the next command
  completion as its own. Text queued earlier with `send_text` can hold more
  than one command, so per-write bookkeeping cannot tell whose completion
  arrives first (`crates/kettle-ui/src/app/ctl.rs`). Deferred because
  reliable correlation needs the shell integration to echo a request token
  for each run. Close with a token in the command-start mark that
  `run_command` matches.
- **Grapheme-cluster widths (DEC mode 2027).** ucs-detect on 5.7.0
  (`docs/TESTING.md`, "Terminal conformance") found that a grapheme cluster
  takes the sum of its codepoints' widths: 0.7% of emoji ZWJ sequences, 50% of
  VS16 and 0% of VS15 sequences, and 84% of graphemes across 85 languages,
  chiefly Indic conjuncts, get the width `wcwidth` gives the whole cluster.
  The grid measures each codepoint (`unicode-width`) in the vendored
  `alacritty_terminal`. Deferred as a product decision, not a defect: a
  program's cursor arithmetic must agree with the terminal, so clustering
  would arrive behind mode 2027, which a program sets when it measures
  clusters too, or as a new default that changes how every program's output
  lays out. Close by choosing, then measuring and storing whole clusters per
  cell with their recorded width, with ucs-detect scores before and after.
- **Unimplemented DEC and xterm controls.** The left/right margin follow-up
  removes 55 failures, character protection removes another 30, and
  rectangular copy/fill/erase removes another 24. Column editing and reverse
  wrapping remove another 32. Attribute changes, mode save/restore, MoreFix
  and truthful fixed-mode/device reports remove another 26; esctest2 now
  fails 85 tests, listed by reason in
  `scripts/conformance-esctest-expected.txt`. Remaining features include
  additional modes and colour syntaxes. Cases
  involving those controls and margins remain grouped with their missing
  feature. The list also retains deliberate identity, checksum and window
  policies. Close each feature by implementing it in the vendored engine and
  removing its passing tests; `just conformance-esctest` fails until the list
  matches. Seven-bit DECSCL selections are implemented; raw eight-bit C1
  replies remain unsupported without changing the selected terminal state.

## UX

- **Intermittent macOS page-up failure.** The page-up action in
  `crates/kettle-ui/src/app/actions.rs` and `scripts/check-live-ui-smoke.py`
  need investigation: earlier runs stayed at display offset zero after 1,600 lines,
  while later runs passed without an explained fix or retained build hashes.
  Deferred because the failure is intermittent. Close with a root-cause fix or
  bounded stress results for identified artifacts; retain executable/harness
  hashes, before/after screen JSON, visible PNGs and system load on failure.

## Media

- **Audio after an interior gap waits for video.** In FFmpeg playback
  (`crates/kettle-media-native/src/ffmpeg/playback.rs`), the packet splitter
  blocks on video delivery before it reads the next audio packet's
  timestamp, so audio that resumes after a multi-second gap is held until
  video catches up. Filling the gap with silence when that packet arrives is
  too late, and inferring gaps from video timestamps is unsound because the
  streams interleave freely. Close with a demuxer that reads audio ahead
  independently of video delivery (separate bounded queues), then fills
  interior gaps with bounded silence.

## Performance

- **PTY worker consolidation.** `crates/kettle-core/src/term.rs` uses a parser
  thread plus a blocking read pump per pane. Deferred pending large-pane-count
  profiling: queues are bounded and buffers recycled, while the split protects
  DEC 2026 deadlines from blocked reads. Close with measurements justifying the
  current design or a shared/native async reader that preserves deadlines,
  per-pane backpressure, Unix portability and ConPTY teardown.
- **Font sharing and lazy loading.** Each renderer still owns a `FontSystem` in
  `crates/kettle-render/src/lib.rs`; `crates/kettle-ui/src/font_preload.rs` now
  preloads the first window's fonts. Further sharing or lazy loading remains
  speculative. Close after profiling shows whether either change is worthwhile,
  then implement and measure any justified change.
- **Search traversal off the event loop.** `crates/kettle-ui/src/app/search.rs`
  still advances retained-history searches in bounded slices after an idle
  deadline. Deferred pending profiling. Close with evidence that these slices
  meet latency goals, or a worker that snapshots safely, releases terminal locks
  before UI work and rejects stale output/resize/query results. Preserve the
  core text/cell/haystack budgets and projection cap.

## Testing

- **App testability.** `crates/kettle-ui/src/app.rs` is now split into
  subsystem modules under `app/`, but event handlers still act on `App` and
  `WindowState` directly, and many guards read source text. Deferred because
  typed per-event outcomes change behaviour-bearing code paths, unlike the
  pure-move split. Close by returning typed outcomes from the input, frame and
  lifecycle handlers, keeping application code thin, and replacing
  source-text guards with behavioural tests where an outcome makes them
  observable.
- **Live UI gate coverage and quarantine.** `scripts/check-live-ui-smoke.py`
  has manual cases beyond CI's selection; `.github/workflows/ci.yml` still makes
  Linux search-history and several other live checks advisory. Deferred until
  runner reliability is measured. Close by adding scenarios individually,
  recording flake rates and making reliable checks required. Keep native-pointer
  tear/re-dock coverage in `scripts/check-tearoff-live-smoke.sh`; the Python
  `tearoff` case only tests the control action. Native macOS caption/modifier
  checks now exist, but do not cover every live scenario.

## Platform

- **Remaining native macOS search evidence.** `docs/TESTING.md` and
  `scripts/check-live-ui-smoke.py` cover modifiers and live rendering, but do not
  establish a complete native IME/accessibility search exercise. Deferred for a
  native desktop run. Close with identified artifacts and recorded input-method
  and assistive-technology results; retain current appearance skips in
  `docs/APPEARANCE-GATE.md` rather than inferring a pass from older cuts.
- **Installed-release desktop launch and recording.** `scripts/install.sh` and
  `scripts/check-linux-installers.sh` cover installation wiring, but do not prove
  the user's installed signed release launches from Ubuntu Super-key search with
  the intended recording configuration. Deferred until that release and desktop
  are available. Close with installed artifact identity, launcher and recording
  evidence on the target desktop.
- **Deeper configured agent/TUI sessions.** `scripts/check-live-ui-smoke.py`
  covers configured Neovim/LazyVCS and opt-in authenticated marker prompts;
  interactive Codex/Claude attachment and longer configured AstroNvim workflows
  remain outside that proof. Deferred for installed credentials/configuration
  and native interaction. Close with identified clients/configurations and
  captured real-session results beyond deterministic markers.
- **Two Windows state-file races.** `ExclusiveFileLock::remove` releases the
  lock before deleting its file on Windows, so another process can open the
  file in between (`crates/kettle-state`, used by kettle-ctl activation
  cleanup), and `open_existing_private_file` requests `WRITE_OWNER` before
  hardening the DACL, which an account with only Modify permission is refused.
  Deferred because both need a native Windows reproduction to prove the fix
  and its regression. Close with handle-based retirement (delete-on-close
  while held) and a conditional `WRITE_OWNER` request limited to the
  ownership-migration branch, each with a Windows test.


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
  agree under a blocked stdout. The blocked-writer characterization test
  preserves the current child-status event and later timeout-status result;
  changing that contract remains deferred.
- **Activation claimed just before its confirmation times out.** When a
  launch hands its request to a running window, the window can claim it just
  as the launcher's five-second wait ends; the launcher then treats the
  primary as busy and opens standalone, so two windows appear
  (`crates/kettle-ui/src/activation_server.rs`, `crates/kettle-ctl/src/activation.rs`).
  An "in progress" reply would fix new launchers, but launchers already
  installed stop reading after five seconds and cannot decode it. Deferred
  for a protocol decision: acknowledge the claim immediately (and give up the
  completion guarantee), or accept a rare duplicate window from old launchers.
  The claim-before-confirmation-timeout characterization test retains this
  behavior without treating a claim as completed activation.
- **Intentional DEC and xterm policy exceptions.** The implemented margins,
  protection, rectangular operations, column editing, reverse wrap, attributes,
  mode save/restore, special colours, screen reverse and software keyboard
  modes have portable and native conformance coverage. The remaining 67
  esctest2 cases are deliberate policies, listed individually in
  `scripts/conformance-esctest-expected.txt`: program-driven geometry and
  title operations are disabled, identity remains Kettle, memory checksums and
  physical DEC keyboard metadata are refused, and printer formatting, legacy
  Hebrew/NRCS and smooth scrolling are fixed inactive. The seven X colour
  syntaxes use fixed D65/sRGB conversion, physical chromaticity validation,
  channel clipping and a continuous CIE Lab low-light inverse; 21 suite
  colour-profile expectations remain exceptions. Changing any policy requires
  a product decision and matching tests. Seven-bit DECSCL is implemented;
  raw eight-bit C1 replies remain unsupported without changing selected state.
  The 41 additional upstream known-bug results are accounted for separately
  in `docs/TESTING.md`; they include skipped profiles and compatibility
  exceptions and are not conformance passes.

## Performance

The [2026-11-01 diagnostics](SCALING-2026-11-01.md) sampled PTY, window and
history scaling. Calibration refused the busy host; the partial 64-PTY
campaign and startup outlier remain inconclusive. These entries stay open.

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
- **Native live UI cases outside CI.** The six formerly advisory Linux cases
  passed 300 repetitions each across three fresh allocations and now gate the
  required build job; see `docs/TESTING.md`. Other manual cases still need
  behavior-specific native evidence. Keep native-pointer tear/re-dock coverage
  in `scripts/check-tearoff-live-smoke.sh`; the Python `tearoff` case only tests
  the control action. Native macOS caption/modifier checks do not cover every
  live scenario.

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
  Source-install Super-key and private recording checks passed on 2026-11-01;
  acceptance of the frozen public package remains a post-publication check.
- **Two Windows state-file races.** `ExclusiveFileLock::remove` releases the
  lock before deleting its file on Windows, so another process can open the
  file in between (`crates/kettle-state`, used by kettle-ctl activation
  cleanup), and `open_existing_private_file` requests `WRITE_OWNER` before
  hardening the DACL, which an account with only Modify permission is refused.
  Deferred because both need a native Windows reproduction to prove the fix
  and its regression. Close with handle-based retirement (delete-on-close
  while held) and a conditional `WRITE_OWNER` request limited to the
  ownership-migration branch, each with a Windows test.


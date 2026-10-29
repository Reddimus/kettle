# Deferred audit work

Open work checked against this repository snapshot; completed audit history is omitted.
Each entry names the gap, its source, the reason for deferral, and the closing evidence.

## Security and boundaries

- **D-Bus maintenance notices.** The product graph accepts the informational
  unmaintained notices RUSTSEC-2026-0370/0371/0372 for `zvariant 5.15.0`,
  `zvariant_derive 5.15.0` and `zbus_names 4.3.4`. The all-target exact reverse
  graph and registry sources are guarded by `scripts/check-dbus-scope.py`;
  `cargo audit --deny warnings` remains enforced for every other notice and
  vulnerability. Reassess these exceptions when the guarded Linux desktop
  integration dependencies change. The vendor-validation graph has no waiver.

- **Upstream `lru` advisory.** `Cargo.lock` still pins `lru 0.16.4` through
  `glyphon 0.12.0`; `deny.toml` ignores RUSTSEC-2026-0253. Deferred because the
  reviewed glyphon path does not call the affected `pop()` method, and a fork
  adds supply-chain surface. Close #207 when upstream accepts `lru >=0.18.2`;
  remove the audit ignores and `scripts/check-lru-scope.py` together.
- **Per-terminal graphics decode ceiling.** Control strings up to 64 KiB no
  longer charge the shared graphics account, and a string refused for memory
  is discarded through its limit, so one pane's images cannot break another
  pane's titles, prompt marks or clipboard writes. Images still compete for
  the 512 MiB process account: one maximum kitty transmission can transiently
  hold about 390 MiB (accumulated payload, whitespace-free copy, decoded and
  inflated bytes, decoder scratch) in `crates/kettle-vt/src/kitty.rs`, so a
  per-terminal ceiling below that would refuse legitimate images. Deferred
  until decoding streams from the accumulated payload. Close by releasing each
  stage before the next, then adding a per-terminal transient ceiling.
- **`setsid()` descendants outside the Linux command line.** The Linux
  `kettle exec` command line adopts descendants that leave the PTY session and
  stops them on timeout or cancellation (`crates/kettle/src/exec.rs`). macOS
  has no child-subreaper, and the MCP `kettle_run` tool runs concurrent
  commands in one process, where an adopted orphan cannot be attributed to one
  command. Deferred because both need a per-command supervisor process. Close
  by running each MCP command under a supervising `kettle exec` child, and on
  macOS by a supervisor that tracks its descendants.

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

## UX

- **Full editing in modal text fields.** `crates/kettle-ui/src/modal_input.rs`
  still appends to strings; title, command, SSH, layout and Settings path fields
  lack a caret, selection, Home/End and word movement. Deferred because editor
  state also needs renderer and IME changes. Close by sharing `SearchEditor`
  behavior with `crates/kettle-render/src/lib.rs` overlays and testing caret,
  selection, horizontal scrolling and mid-string preedit.
- **Consistent outside-click dismissal.** `crates/kettle-ui/src/app.rs` closes
  context menus on outside clicks but swallows clicks for palettes and pickers.
  Deferred for per-modal hit-testing and a deliberate click-through policy.
  Close with consistent dismissal, documented click consumption and regressions.
- **Intermittent macOS page-up failure.** `scroll_page_up` in
  `crates/kettle-ui/src/app.rs` and `scripts/check-live-ui-smoke.py` needs
  investigation: earlier runs stayed at display offset zero after 1,600 lines,
  while later runs passed without an explained fix or retained build hashes.
  Deferred because the failure is intermittent. Close with a root-cause fix or
  bounded stress results for identified artifacts; retain executable/harness
  hashes, before/after screen JSON, visible PNGs and system load on failure.

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

- **App testability and remaining module split.**
  `crates/kettle-ui/src/app.rs` has sibling modules but still holds substantial
  dispatch, frame and lifecycle logic. Deferred as a multi-session refactor.
  Close by extracting the remaining subsystems, using typed per-event outcomes
  with thin application code, and replacing source-text guards with behavioral
  tests where possible.
- **Renderer module split.** `crates/kettle-render/src/lib.rs` still combines
  the frame pipeline, adapter selection, screenshot capture, overlay types and
  text-fit helpers. Deferred as a large refactor. Close by extracting GPU,
  screenshot, overlay and pure geometry modules with focused coverage.
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

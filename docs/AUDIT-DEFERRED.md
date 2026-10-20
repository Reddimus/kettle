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
- **Per-terminal transient graphics ceiling.**
  `crates/kettle-vt/src/graphics_limits.rs::reserve_transient_cpu` charges only
  the process budget; `extract.rs::append_seq` treats allocation failure like
  an oversized sequence. One pane can exhaust shared space and disrupt other
  panes' control strings. Deferred for a budget/API redesign. Close with a
  per-terminal in-flight ceiling and distinct limit/allocation-failure recovery.
- **macOS update cleanup races.** `Staging::discard` and `remove_leftover` in
  `crates/kettle-update/src/macos.rs` check identity, rename, recheck, then delete
  by pathname. A writer of the enclosing directory can still substitute a tree
  before deletion. Deferred to avoid adding recursive FFI during the updater
  change. Close with a descriptor-relative `unlinkat` walk with depth/entry caps.
- **Unix descendants can escape timeout teardown with `setsid()`.**
  `crates/kettle-core/src/term.rs` contains the PTY session, but a detached,
  reparented process cannot safely be identified by ancestry or PTY access.
  Deferred because stronger ownership needs process-creation architecture.
  Close with OS-owned containment, such as a delegated Linux cgroup or a
  supervisor/subreaper with a proven ownership contract.
- **SSH reconnect drops `-P tag`.** `crates/kettle-remote/src/lib.rs::detect_ssh`
  consumes the value but neither retains it nor marks the session
  unreproducible; `clone_session_command` can therefore select another host.
  Left out of the boundary-audit fixes as a separate low-priority change.
  Close by reproducing the tag or suppressing Reconnect, with a regression.

## Terminal and protocol

- **One streaming control-state kernel for three ANSI parsers.**
  `crates/kettle/src/exec.rs`, `crates/kettle-core/src/term.rs` and
  `crates/kettle-vt/src/extract.rs` still implement separate state machines.
  Deferred because concurrent parser repairs would collide with a shared
  refactor. Close with shared ESC/CSI/string/C1, cancellation, split UTF-8 and
  bounded-recovery semantics, policy hooks for each consumer, and a state/event
  conformance matrix. Until then, check each parser fix against all three.
- **Grid/generation publication ordering.** `publish_output_if_ready` in
  `crates/kettle-core/src/term.rs` publishes after the grid lock is released;
  readers can pair a new grid with an old generation. Deferred because review
  rejected the proposed consequences and fix. Close with a demonstrated reader
  impact and ordering regression, or an analysis proving the pairing harmless.

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

### Media and performance

- **Video with delayed audio:** when a file's audio starts later than its picture
  (for example an audio track offset by 10 s), playback paces video by the audio
  clock, so the picture waits for audio that has not begun. The paths are
  `crates/kettle-media-worker/src/play.rs` and
  `crates/kettle-media-native/src/ffmpeg.rs`. A draft that pads silence in the
  FFmpeg audio graph (`aresample=async=1:first_pts=...`) broke
  `real_ffmpeg_keeps_audio_before_video_without_opening_deadlock` and
  `real_ffmpeg_clips_codec_padding_to_the_opened_duration` in
  `crates/kettle-media-native/tests/ffmpeg.rs`. Closing it needs either a player
  that paces by wall clock until audio begins, or an audio graph that pads only
  leading silence. Rare in practice.

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
- **macOS staging emptiness branch.** `Staging::create` in
  `crates/kettle-update/src/macos.rs` rejects children left before inherited ACLs
  are cleared, but that rejection has no automated coverage. Deferred because
  timing a microsecond race would not prove the guard. Close with a deterministic
  seam or fixture that fails when the check is removed.
- **macOS foreign-owner sweep branch.** `sweep_interrupted_updates` in
  `crates/kettle-update/src/macos.rs` checks uid before deleting leftovers, but
  tests do not reach the foreign-owner refusal. Deferred because one user cannot
  create a second uid's files. Close with a second-uid fixture or a pure
  `(file_type, uid, euid)` decision test that fails without the ownership guard.
- **Source-guard stripping limits.** `production_source` in
  `crates/kettle-test-support/src/lib.rs` still leaves test text for
  `#[cfg_attr(not(test), cfg(test))]` and a same-line
  `/** needle */ #[cfg(test)] fn f() {}`. Deferred because neither form occurs
  in workspace callers, and they can only make positive guards pass spuriously.
  Close with parser support and regressions for both forms.
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

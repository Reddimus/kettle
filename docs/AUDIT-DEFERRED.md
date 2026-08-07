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
- **PTY I/O worker consolidation:** each pane currently uses a parser thread
  plus a blocking pump thread so DEC 2026 deadlines remain independent of a
  blocked native read. The channel is bounded and buffers are recycled, so this
  is measured thread-count debt rather than a memory-growth bug. Profile large
  pane counts before considering an async/native-overlapped reader or a shared
  I/O service; any replacement must preserve ConPTY teardown, Unix portability,
  parser deadlines, and per-pane backpressure.
- Per-window `FontSystem` sharing; lazy system-font load on first frame. Both are
  speculative — profile on the maintainer's machine before implementing.

## Deferred from the 2026-08-07 full-repo audit

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

- **`scroll_page_up` did not enter scrollback in one macOS live-probe run.**
  `scripts/perf/kettle-live-probes.py` seeds 1600 lines and then asserts
  `display_offset > 0` after `perform_action scroll_page_up`; that assertion
  failed once during macOS comparator development. It was observed on a heavily
  loaded machine and has not been reproduced deliberately, so it is recorded as
  **unconfirmed** rather than diagnosed: it is either a real scrollback defect or
  probe flakiness under contention. Reproduce on a quiet machine before
  investigating — do not assume which.

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

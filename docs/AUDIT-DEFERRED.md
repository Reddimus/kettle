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

- **Kitty graphics `a=q` capability reply** (and the `Chunk::PtyReply` plumbing it
  needs) so probers like `kitten icat` don't conclude graphics are unsupported.
- **OSC 52 selection target** (`p`/`s` vs `c`): route PRIMARY writes/reads to the
  X11 PRIMARY selection on Linux instead of always CLIPBOARD.
- **Vi-mode over scrollback**: vi navigation is currently viewport-only; make
  `k`/`j` scroll at the viewport edge and `g`/`G` jump to history top/bottom.

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

- **redraw scroll-on-output lock**: gate the per-pane history `Term` lock behind
  the lock-free `output_generation` atomic so idle frames acquire zero locks.
- **Remote-poll tick**: reuse BFS scratch buffers, refresh cwd only for the one
  foreground pid that needs it, and fan out across all windows (not just the
  painting one).
- Per-window `FontSystem` sharing; lazy system-font load on first frame. Both are
  speculative — profile on the maintainer's machine before implementing.

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

- **`app.rs` god-file split (24.7k lines).** Extract along the seams the
  `Action` enum already implies — `action_dispatch`, drag/menu modules reusing
  the `detach.rs` pattern, `session_glue` (ctl/MCP/recorder wiring), and
  `window_lifecycle` — keeping `App` as a struct plus small `impl App` blocks
  split across files. Best paired with making per-event handlers return a typed
  `Outcome` list (pure deciders + thin applier) so the source-text drift guards
  become behavioural unit tests. Supersedes the earlier `app.rs` entry above.
- **`kettle-render/src/lib.rs` module split.** The remaining ~11k-line file still
  interleaves the `impl Renderer` frame pipeline with overlay/menu data types,
  GPU adapter selection, the screenshot capture pipeline, and text-fit geometry
  helpers, even though eight sibling submodules were already carved out. Extract
  in isolation order: `gpu.rs` (adapter selection, no `Renderer` state) →
  `screenshot.rs` → `overlays.rs` (data structs) → `text_fit.rs` (pure helpers).
- **OSC 52 selection target (`p`/`s` vs `c`).** `TermEvent::ClipboardStore/Load`
  carry an `alacritty_terminal::term::ClipboardType`; route `Selection` through
  the existing arboard `LinuxClipboardKind::Primary` path used by
  `copy_selection`/`paste_primary`. Needs the event→handler plumbing, not an
  `app.rs`-local change. (Overlaps the older OSC 52 entry above.)
- **OSC 133 prompt marks desync once scrollback wraps.** Absolute line numbers
  drift after the grid's history ring hits `max_scroll_limit`
  (`total_lines()`/`history_size()` both cap). Needs an anchor that survives the
  ring cap rather than a raw absolute line index.
- **Global kitty `a=d,d=f` animation clear is dropped.** `kitty.rs` clears its
  own frame/anim state but the clear never reaches the renderer (only `id != 0`
  stores round-trip), so cleared animations keep playing. Needs a
  `Chunk::PtyReply`-style clear signal across the vt→render boundary.
- **State/lock-file `0600` is a no-op on Windows.** A correct ACL needs new
  `windows-sys` `Win32_Security` / `Win32_Security_Authorization` features and a
  `SetNamedSecurityInfoW` owner-only DACL on the lock/state files.
- **Command palette / layout picker / SSH launcher stay single-line bars.** Fold
  them into the responsive, multi-row layout the search bar gained in v2.38.0
  (also makes room for per-row keybind hints). Extends the vertical-list-pickers
  entry above.
- **Clone-safe non-blocking ctl write (`kettle-ctl` transport).** The audit
  finding that `write_all_until` toggles `O_NONBLOCK` on the shared open file
  description (so a concurrently-used `try_clone`d sibling could observe
  `WouldBlock` during a write) is real, but the attempted fix — dropping the
  fd flag and relying on per-`send` `MSG_DONTWAIT` — hangs on macOS, which does
  not reliably honor `MSG_DONTWAIT` on AF_UNIX stream sockets (a full send
  buffer blocks forever instead of returning `WouldBlock`). The fd-level
  `O_NONBLOCK` guard is therefore retained. A clone-safe rewrite must keep the
  fd blocking and achieve per-write non-blocking another way — e.g. `poll`ing
  `POLLOUT` before each `send` on a still-blocking fd, or writing through a
  dup'd fd whose flags are private — verified on real macOS. The client's
  request/response is exclusive today, so the shared-flag window is not
  currently reachable.
- **Update-archive extract residual on Linux (post-fix hardening).** v2.39.0
  closed the verify/extract TOCTOU on Windows outright (a mandatory
  `LockFileEx` range lock) and closed the delete-and-recreate variant on Linux
  (verify and extract now read the *same* handle, not a re-opened path). What
  remains on Linux is an in-place overwrite of the *same inode* by a same-user
  process that already holds a writable fd on the private `0600` temp archive,
  in the narrow window between the hash read and the extract read — no privilege
  boundary is crossed (the attacker is already the same user). The complete
  guarantee is to stop reading the archive from disk twice: read it once into
  memory (bounded by the existing 256 MiB `MAX_ARTIFACT_BYTES` cap), hash that
  buffer, and extract from a `std::io::Cursor` over it. Deferred as a clean but
  non-trivial refactor of `verify_sha256`/`extract_archive` in
  `crates/kettle-update/src/install.rs`.

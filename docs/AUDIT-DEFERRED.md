# Audit follow-up — deferred items

The v2.32.0 audit (two exhaustive multi-agent passes: per-crate + seven
cross-cutting dimensions, every finding adversarially re-verified) confirmed
~114 findings. v2.32.0 shipped every HIGH-severity correctness / robustness /
security bug plus the cheap high-value mediums and the full docs sweep. The
items below were **deliberately deferred** because they are large, structural,
or need a live GPU / the maintainer's interactive desktop to validate. They are
tracked here so they are not lost.

## Large / structural

- **In-process GPU device-loss auto-recovery.** v2.31.0 + v2.32.0 make a GPU TDR
  a safe, logged, non-spinning "reopen kettle" state. Full recovery —
  re-`request_device`, rebuild every window's surface / pipelines / atlases on the
  shared `GpuContext`, with bounded retry/backoff — is a cross-window structural
  change that needs a real TDR on a live GPU to validate. (`kettle-render` /
  `kettle-ui`.) Also fold in the OOM-error-scope streak and "refuse `open_window`
  while lost" here.
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
- **Mouse-wheel alternate-scroll** (DEC 1007): translate wheel notches into
  cursor-key arrows on the alternate screen so `less`/`man`/`vim` scroll. Medium;
  touches both the GUI and ctl wheel paths.
- **OSC 52 selection target** (`p`/`s` vs `c`): route PRIMARY writes/reads to the
  X11 PRIMARY selection on Linux instead of always CLIPBOARD.
- **Vi-mode over scrollback**: vi navigation is currently viewport-only; make
  `k`/`j` scroll at the viewport edge and `g`/`G` jump to history top/bottom.

## UX / feedback

- **Surface malformed-config diagnostics in the GUI.** `detect_malformed_values`
  is wired only into the CLI; a typo saved to the live-reloaded config silently
  reverts. Fire a notification / dismissible banner on reload.
- **Middle-click with a context menu open** should dismiss the menu rather than
  paste PRIMARY / close a tab behind it.
- Keybind-capture should warn when reassigning an in-use chord; Settings
  GPU/padding writes should surface a persist failure; overlay text inputs need
  caret movement / Home/End / paste.

## Performance (measure first)

- **redraw scroll-on-output lock**: gate the per-pane history `Term` lock behind
  the lock-free `output_generation` atomic so idle frames acquire zero locks.
- **Remote-poll tick**: reuse BFS scratch buffers, refresh cwd only for the one
  foreground pid that needs it, and fan out across all windows (not just the
  painting one).
- Per-window `FontSystem` sharing; lazy system-font load on first frame. Both are
  speculative — profile on the maintainer's machine before implementing.

These are sourced from the two audit run outputs and the synthesis plans; pick
them up in priority order (robustness/correctness before perf before polish).

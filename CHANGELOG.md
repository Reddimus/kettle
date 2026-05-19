# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Fixed
- Split keys now match Terminator exactly: `Ctrl+Shift+O` splits
  horizontally (top/bottom), `Ctrl+Shift+E` splits vertically
  (left/right); `split_horiz`/`split_vert` action names corrected.

### Added
- `kettle --screenshot <out.png> [--cols --rows]`: renders a representative
  frame **offscreen** (no window) through the real `wgpu`/`glyphon`/quad
  path and writes a PNG. Used to generate the showcase images in
  **docs/UX-COMPARISON.md** — a cited UI/UX comparison matrix (kettle vs
  Ghostty/kitty/WezTerm/Terminator/Alacritty) with a tab-bar hit-region
  mermaid and the prioritized backlog status.
- UX backlog: unfocused-pane **dimming** (`unfocused-split-opacity`,
  default 0.7), **pane zoom/maximize** (`Ctrl+Shift+X`), per-pane
  **scrollbar** (`scrollbar = never|auto|always`), configurable
  **split-divider color**, configurable **cursor-blink interval**, and
  a **copy-on-select** toggle. Dimming/scrollbar use a post-text quad
  pass so they sit above glyphs.
- Tab bar redesign: per-tab close **✕** (click to close), trailing
  **+** new-tab button, **middle-click** a tab to close it,
  always-shown by default, active-tab accent, title eliding. New
  config `tab-bar` (off|auto|always) and `tab-bar-position`
  (top|bottom). Geometry is a single source of truth shared by the
  renderer and click hit-testing.
- kitty animation **now plays end-to-end**: `a=f` frames / `a=a`
  control snapshot through `Chunk::Animation` into a per-terminal
  `Animations` registry; at draw time a placement's image is swapped for
  the frame the playback clock selects, and the event loop schedules
  ~30 fps redraws while any animation is running. Root-frame gap via
  `a=a,r=1,z=`; animations are reaped with the image or by `a=d,d=f`.
- kitty animation playback-timing engine: pure, deterministic
  `current_frame(gaps, state, elapsed_ms)` mapping elapsed time to the
  frame to show — skips gapless frames, honors infinite/finite loop
  counts, `loading`-mode hold-at-end, and stopped→selected-frame. The
  renderer clock + frame substitution is the only remaining sub-item.
- kitty animation (decode/state layer): `a=f` animation-frame
  transmission (chunked via a single in-flight slot, gap from `z` with
  `z<0` = gapless base frames), `a=a` animation control (`c` current
  frame, `s` = stop/run/loading, `v` loop count, `r`+`z` per-frame gap),
  and `a=d,d=f` frame deletion (keeps the base image).
  `KittyState::frames()/animation()` expose the model for the upcoming
  playback/compositing cycle. Cited: kitty
  `docs/graphics-protocol.rst:839`.
- Font-feature tuning: `font-feature` now parses real OpenType tags
  (`liga`, `calt`, `ss01`, `cv01`, `zero`, …) with `+tag` / `-tag` /
  `tag=N` / `tag on|off` dialects, repeatable and comma-separated, and
  applies them through cosmic-text `FontFeatures` on top of the coarse
  ligature toggle (explicit settings win; Advanced shaping kept whenever
  any feature is set). Cited: Ghostty `font-feature`, kitty
  `font_features`.
- kitty placeholders: the **placement id** is now decoded from each
  cell's underline color (256/truecolor/named), feeding the spec's
  run-grouping and left-inheritance so cells of different placements no
  longer inherit across each other.
- kitty Unicode placeholders **now render**: each frame the visible grid
  is scanned for `U+10EEEE`, the image id is read from the cell
  foreground (256-color / truecolor / ANSI-named) plus the msb diacritic,
  contiguous runs apply the left-inheritance rules, and the referenced
  `U=1` virtual image is sliced per cell (`ImageData::crop` +
  `placeholder::tile_src_rect`, exact-tiling) and drawn through the
  existing GPU image pipeline. Virtual images are reaped on
  delete-by-id/all. (`Terminal::placeholder_tiles`.)
- kitty Unicode placeholders (decode layer): `kettle-vt::placeholder` —
  the 297-entry row/column diacritic table, per-cell diacritic parsing,
  32-bit image-id reconstruction (foreground + msb diacritic), and the
  omitted-diacritic left-inheritance algorithm; plus `U=1` **virtual
  placements** in the kitty decoder (`a=p,U=1` / `a=T,U=1` store the
  image and register a rows×cols placement without drawing at the
  cursor). Renderer compositing of placeholder cells is the next cycle.
- VT conformance: XTWINOPS `CSI 18 t` text-area size report
  (`CSI 8 ; rows ; cols t`), DSR `CSI 5 n` device-status (`→ CSI 0 n`),
  and an exact-match DA1 assertion (`CSI c`/`CSI 0 c` → `CSI ? 6 c`).
  44 conformance tests total.
- VT conformance suite — 35 end-to-end tests through the real
  `vte`+`alacritty_terminal` path: CUP/erase/SGR/tabs, scroll region,
  charsets, ICH/DCH/IL/DL, DECSC/DECRC, autowrap, origin mode, DECALN,
  REP, SO/SI, RIS, ECH, CHA/HPA/VPA, SU/SD, DECSCUSR, wide CJK,
  combining marks, OSC 4/8/52, DECRQM, DSR/DA1/DA2, DECSET 1049.
- kitty graphics advanced ops: transmit-only store, place-by-id,
  delete (all/by id), z-index ordering.
- Per-style font families (`font-family-bold/italic/bold-italic`) and a
  ligature shaping toggle.
- Configurable bell (`off|visual|attention|both`) with cross-platform
  window-attention (taskbar/dock urgency); no audio deps.
- Focus-event reporting (DEC ?1004).
- UX polish: safe bracketed paste, double/triple-click word/line select
  with auto-copy, focus-aware hollow cursor, cursor blink, visual bell.
- Offscreen GPU self-test (WGSL compile + render pass) run in CI on
  Linux/macOS/Windows.

## [0.1.0] — 2026-05-19

First cross-platform release; artifacts built on real runners and
attached to the GitHub release (Linux tar+`.desktop`, macOS `.app`,
Windows zip).

### Added
- GPU renderer: `wgpu` + `glyphon`, tiled multi-pane, tab bar, split
  dividers, focus border, cursor/selection/search overlays.
- Engine: `portable-pty` + `alacritty_terminal` + `vte`, per-pane
  reader thread, infinite scrollback option.
- Terminator-style tabs + binary split tree, broadcast input,
  Terminator-compatible keybinds incl. Shift+Arrow resize.
- 512 bundled Ghostty themes (default **TokyoNight Night**); bundled
  JetBrains Mono Nerd Font; Ghostty-syntax config with live reload.
- Regex search overlay; mouse selection + wheel scroll.
- Inline images: Sixel, kitty graphics, iTerm2 (OSC 1337).
- Hyperlinks: OSC 8 + URL autodetection, Ctrl/Cmd-click to open.
- Mouse-reporting passthrough (X10 + SGR 1006).
- Shell integration (OSC 133) + jump-to-prompt.
- Session save/restore (tab/split tree + per-pane cwd).
- SSH multiplexing (launcher + session-persisted SSH tabs).
- MIT licensed; CI matrix; docs with citations + mermaid diagrams.

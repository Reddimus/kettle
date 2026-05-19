# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Security
- **`tab-format`** (alias `tab-title-format`): user-templatable per-tab
  label (default `{n}: {title}`) via the shared `template::fill`;
  unknown placeholders pass through verbatim; the trailing `✕` is
  still appended by the renderer. +1 test.
- **`window-title-format`** (alias `title-format`, Ghostty/WezTerm
  parity): template the OS window title with `{title}` / `{cwd}` /
  `{tab}` placeholders; `{{`/`}}` escape literal braces; unknown
  placeholders are left as literal text (typos visible). Pure
  `kettle_config::template::fill` + 4 tests.
- **`minimum-contrast`** (WezTerm parity) — keep text readable on
  low-contrast themes by lifting each cell's foreground toward
  white/black until it meets a configured WCAG 2.0 ratio (`0.0` = off,
  `4.5` ≈ AA, `7.0` ≈ AAA). Pure `color::with_min_contrast` over
  `relative_luminance`/`contrast_ratio` (+4 tests).
- Mouse-wheel scroll speed is now configurable: `scroll-multiplier`
  (alias `mouse-scroll-multiplier`, default `1.0` ≈ 3 lines per notch,
  clamped 0.1–50) scales both `LineDelta` and `PixelDelta` input;
  Ghostty/kitty parity. Pure `wheel_lines` helper, +2 tests.
- OSC 52 clipboard **writes are now size-capped** (1 MiB, truncated on
  a UTF-8 char boundary) so a hostile program can't push an unbounded
  payload into the system clipboard.
- **OSC 52 clipboard policy** (`osc52 = off|copy|paste|both`, default
  `copy`): clipboard *reads* via OSC 52 — which let a possibly-remote
  program exfiltrate your system clipboard — are now **denied by
  default** (an empty, well-formed reply is sent); writes remain
  allowed. Configurable per the new key (alias `clipboard`).
- Hardened **URL opening**: a URI from terminal output (an OSC 8
  hyperlink or autodetected link, opened via Ctrl/Cmd-click or hint
  mode) is now run through `links::is_safe_url` before the OS handler —
  only `http(s)`/`ftp(s)`/`mailto`/`file://` are allowed; custom
  schemes (`javascript:`, `vscode:`, `data:`, …), control characters,
  whitespace, `file://` path traversal, and absurd lengths are
  refused. Closes a known terminal scheme-handler abuse vector.

### Fixed
- Scrollback **search now scrolls the viewport to the active match**:
  matches in history (and `Tab`/`Shift+Tab` cycling onto them) bring
  the line into view (~⅓ from the top), once per match/query change so
  wheel-scrolling still works. Previously off-screen matches were found
  but never shown. Pure tested `search::reveal_offset`.
- Theme cycling (`next_theme`/`prev_theme`) now matches the current
  theme **case-insensitively and trimmed** (like `by_name`), so a
  config such as `theme = tokyonight night` cycles from the right
  place instead of jumping to the first theme.
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
- `kettle --list-keybinds` prints the resolved default keymap
  (`trigger → action`, sorted) so the binding set is discoverable
  without reading the source (parallels `--list-themes`).
- A theme picked at runtime now **persists across restarts** — it's
  saved in `session.json` (`theme`, `#[serde(default)]` so older
  sessions still load) and reapplied on launch, until you change it
  again or reload the config.
- **Live theme switching**: `next_theme` / `prev_theme` keybind actions
  and "Next theme" / "Previous theme" command-palette entries cycle the
  ~512 bundled themes at runtime — no config edit or reload. Pure
  `Theme::cycle` (wrap-around; unknown current → first theme).
- The scrollback **scrollbar is now interactive**: left-click the
  focused pane's right-edge bar to jump the viewport there, then
  **drag** to scrub through history (x is ignored once grabbed, like a
  normal scrollbar; released on button-up). Geometry moved to a pure,
  tested `kettle_core::scrollbar` module (`thumb` for drawing,
  `target_offset` for the click mapping), shared by the renderer and
  the UI (was duplicated, untested math).
- `--config FILE` selects an explicit config file instead of the
  default path; it is honored by the running terminal (including the
  live-reload watcher, which now watches that file's directory) and by
  `--config-path`, `--check-config`, and `--screenshot`.
- **Middle-click pastes** the clipboard into the focused pane (standard
  X11 terminal behavior; bracketed-paste-safe via the shared
  `paste_clipboard`), when mouse-reporting isn't consuming the click
  and the cursor isn't over the tab bar (where middle-click still
  closes a tab).
- The OS **window title now follows the active pane** — switching tabs
  or focusing another split retitles the window (not just on OSC title
  events), with empty/placeholder titles falling back to `kettle`. The
  `set_title` call is deduped so it isn't a per-frame syscall.
- **Rectangular (block) selection**: hold `Alt` and drag to select a
  column block (iTerm2/Alacritty/WezTerm parity), via a pure
  `selection_kind(clicks, alt)` mapping; word/line still copy on press,
  Simple/Block copy on release.
- Standard launch CLI: `-e/--exec CMD …` runs a command in the first
  tab instead of the shell (consumes the rest of the args, hyphenated
  program flags included — e.g. `kettle -e ssh -t host`) and
  `-d/--working-directory DIR` sets its directory; either overrides a
  saved session for that first tab. (`kettle_ui::run_with(Options)`.)
- New tabs and splits now **inherit the focused pane's working
  directory** (via OSC 7), like WezTerm/iTerm/kitty — open a split and
  you're already in the same project. A since-deleted directory falls
  back to the default (`usable_cwd` guard) instead of failing to spawn.
- Quick-select **hint mode** is now usable (`Ctrl+Shift+H`): every
  visible URL / path / git-hash / IP gets a short label drawn over the
  focused pane (chip + glyph); type the label to open it (URLs via the
  OS handler) or copy it to the clipboard, `Backspace` to correct,
  `Esc` to cancel. New `hint_mode` keybind action.
- Quick-select / hint-mode core (`kettle_core::hints`, pure +
  fully-tested): scans the visible rows for URLs, filesystem paths,
  git hashes and IPv4 addresses (higher-priority kinds win on overlap,
  trailing punctuation trimmed, char-column coordinates) and generates
  minimal-width unique labels over a home-row alphabet. The overlay +
  key-to-act wiring is the next cycle.
- Docs: `ARCHITECTURE.md` refreshed to the current system — crate
  responsibilities, the side-channel chunk set
  (VirtualImage/Animation/RelativePlacement), the per-pane registries,
  the animation redraw tick, an accurate test count, and a **new
  mermaid diagram of the kitty graphics pipeline** (decode → registries
  → placeholder/relative/animation render).
- Search is now a **real regex with smart-case**: the `Ctrl+Shift+F`
  pattern is compiled as a regex (alternation, anchors, `\b`, …),
  case-insensitive unless it contains an uppercase character
  (ripgrep/vim smart-case), and an invalid pattern falls back to a
  literal search instead of returning nothing (`search::build_regex`).
- Command palette (`Ctrl+Shift+K`): a fuzzy action launcher over a
  29-command registry (`kettle_config::palette`) — type to filter,
  `Tab`/`↑↓` to select, `Enter` to run, `Esc` to cancel. Bottom-bar
  overlay reusing the SSH-launcher plumbing; new `command_palette`
  keybind action.
- Fuzzy matcher (`kettle_config::fuzzy`, dependency-free): subsequence
  scoring with prefix / word-boundary / camelCase / contiguity bonuses
  and a length penalty (`score`, `best`). The `Ctrl+Shift+S` SSH
  launcher now fuzzy-matches host names on `Tab`-complete and `Enter`
  (was prefix-only); the matcher is reusable by a future command
  palette.
- VT conformance sweep: IRM insert mode (`CSI 4h` shifts text right),
  DECTCEM cursor visibility (`CSI ?25 h/l`), LNM mode bit
  (`CSI 20 h/l`), DECCKM + DECKPAM/DECKPNM application cursor/keypad
  modes, and mouse-tracking DECSET flags (`?1000/?1002/?1003/?1006`)
  set and cleared — 5 end-to-end tests through the real vte path.
- kitty relative placements: parents can now also be **regular
  placements** (not just placeholders) and **relative chains** are
  resolved — a pure `resolve_chain` walks child→parent with a depth
  bound of 8 (kitty `ETOODEEP`; cycles are bounded, not infinite), with
  parent origins unified from placeholder cells and the image registry.
  This completes the kitty graphics protocol surface.
- kitty relative placements **now render** when the parent is a visible
  Unicode-placeholder (virtual) image: the child image is drawn `(h,v)`
  cells from the parent's placeholder origin (the min abs-line/column of
  its cells), through a per-terminal `Relatives` registry and the pure
  `relative_origin` clamp. Parents that aren't on screen this frame are
  skipped; the placement group still dies with its parent.
- kitty relative placements (decode/state): `a=p,P=,Q=` is recorded as
  a `RelativePlacement` (parent image/placement + `H`/`V` cell offset)
  instead of drawing at the cursor; a placement group dies with its
  parent (parent-image deletion cascades to its relatives). Render-time
  resolution of the on-screen position from the parent is the next
  sub-item.
- kitty animation frame compositing: partial-rect `a=f` frames are
  blended (or `X=1` replaced) over a chosen canvas — a previous frame
  (`c=`), a `Y=` background color, or transparent — and `r=` edits an
  existing frame in place; `a=c` copies a rectangle between frames
  (including onto the root image). New RGBA `ImageData::compose`
  (source-over) and `solid` primitives.
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

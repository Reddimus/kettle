# Roadmap

## Done

- [x] Cargo workspace, MIT, CI matrix (Linux/macOS/Windows)
- [x] PTY + `alacritty_terminal` + `vte` core, per-pane reader thread
- [x] `wgpu` + `glyphon` renderer: text, bg, cursor, selection, search rects
- [x] Ghostty-syntax config + ~500 bundled themes (default TokyoNight Night)
- [x] Bundled JetBrains Mono Nerd Font (embedded)
- [x] Terminator-compatible keybindings + live config reload
- [x] Tabs + **binary split tree**, tiled multi-pane GPU rendering, split
      dividers, focus border, tab bar
- [x] Geometry-based focus nav (`Alt+Arrows`), mouse-click focus, ratio
      resize (`Ctrl+Shift+Arrows`), broadcast/group input
- [x] Regex search overlay (`Ctrl+Shift+F`) — true regex + smart-case
      (insensitive until an uppercase char), literal fallback for an
      invalid pattern (`search::build_regex`, +3 tests)
- [x] Clipboard (OSC 52 + copy/paste), keyboard input encoding, IME text

- [x] Mouse drag text selection + wheel scrollback
- [x] Hyperlinks: OSC 8 + URL autodetection, underline + Ctrl/Cmd-click to open

- [x] Image protocols: Sixel + kitty graphics + iTerm2 OSC 1337, extracted
      ahead of the VT parser, decoded to RGBA, GPU-composited, scroll-anchored

- [x] Mouse reporting passthrough (X10 + SGR 1006): click/drag/wheel to
      vim/tmux/htop/fzf; local selection/scroll when tracking is off
- [x] Shell integration (OSC 133 A/B/C/D) + jump-to-prompt
      (`Ctrl+Up`/`Ctrl+Down`); bash/zsh/fish snippets in
      docs/SHELL-INTEGRATION.md

- [x] Session save/restore: OSC 7 cwd capture, tab/split tree + per-pane
      cwd serialized to session.json, restored on launch, autosaved on
      structural changes + exit

- [x] SSH multiplexing: per-pane argv, `Ctrl+Shift+S` SSH launcher with
      configured `ssh-host` tab-complete, SSH tabs persisted in sessions

- [x] Automated test harness (19 deterministic tests across vt/config/ui:
      decoders, extractor, config, keybinds, split-tree, session round-trip,
      8 MiB perf guard) + CI fmt-check, headless GPU smoke, CLI smoke on all
      three OSes — see docs/TESTING.md

- [x] Offscreen GPU self-test (compiles WGSL + renders a pass with no
      window) run in CI on Linux/macOS/Windows — real cross-platform GPU
      validation
- [x] OS packaging + release workflow: Linux tar+`.desktop`, macOS `.app`
      bundle, Windows zip, built on real runners and attached to GitHub
      releases on tag (see docs/INSTALL.md)

- [x] UX polish: safe bracketed paste (newline-normalized,
      injection-guarded), focus-aware hollow cursor, config cursor blink,
      visual bell flash, double-click word / triple-click line selection
      with auto-copy on release

- [x] kitty graphics advanced ops: `a=t` transmit-only store, `a=p`
      place-by-id, `a=d` delete (all / by id), `z=` z-index ordering

- [x] Per-style font family overrides (`font-family-bold/italic/
      bold-italic`) + ligature toggle (Advanced vs Basic shaping)

- [x] VT conformance test suite: end-to-end through the real
      vte+alacritty path — text/newline/CUP, erase line/display, SGR
      truecolor+bold+reset, tab stops/CR, alt-screen + bracketed-paste
      modes, DECSTBM scroll region (6 tests)

- [x] Extended VT conformance (11 tests total): DEC special-graphics
      line-drawing charset, insert/delete char (ICH/DCH), insert/delete
      line (IL/DL), DECSC/DECRC save-restore, DECAWM autowrap, DECOM
      origin mode within margins

- [x] Conformance for device responses (14 tests total): DSR 6n cursor
      position report, primary Device Attributes reply, SGR
      dim/underline/strikeout + curly-underline (4:3) + reset — verified
      through the real EventProxy PTY write-back path

- [x] Focus-event reporting (DEC ?1004): CSI I / CSI O sent to the
      focused pane on window focus change when the app enables it
- [x] Mouse-encoding unit tests: SGR(1006) + legacy X10, press/release,
      shift/ctrl/motion modifier bits, wheel; tracking-mode detection

- [x] Configurable bell (`bell = off|visual|attention|both`): visual
      flash and/or cross-platform window-attention (taskbar/dock
      urgency) when unfocused, cleared on focus — no audio deps

- [x] Conformance: DECALN screen alignment, REP repeat-char, G1 via
      SO/SI charset invocation (17 conformance tests). SS2/SS3
      single-shift is unsupported upstream (alacritty_terminal) — noted.

- [x] Conformance: RIS full reset clears origin/region, EL erase-left,
      ED erase-below, DA2 secondary device attributes (21 conformance
      tests). DECSTR/ED-1 differ upstream — used engine-supported
      equivalents instead of asserting unsupported behavior.

- [x] Conformance: ECH erase-in-place, ICH shift-off-edge, CHA/HPA/VPA
      absolute moves (VPA keeps column), DECSC/DECRC restores the SGR
      pen (25 conformance tests)

- [x] Conformance: SU/SD scroll up/down, DECSCUSR cursor shape
      (underline/beam/block), wide CJK = 2 cells + WIDE_CHAR_SPACER,
      wide-char wraps when it can't fit (29 conformance tests)

- [x] Conformance: combining mark = zero-width on base cell, OSC 4
      palette query -> ColorRequest, DECRQM reports mode state
      (32 conformance tests). HTS custom tab stops unreliable upstream.

- [x] Conformance: OSC 52 clipboard copy, OSC 8 hyperlink carried on
      cells (and cleared), DECSET 1049 preserves primary-screen content
      (35 conformance tests)

- [x] Config validation: unknown keys collected (typo guard), warned at
      startup, and surfaced by `kettle --check-config` (exit 1 on issues)

- [x] Conformance: DECSET 2026 synchronized output applies content
      atomically + DECRQM reports mode 2026 (37 conformance tests)

- [x] Conformance: NEL/IND/RI line ops (column-preserving), DECID
      reply, cursor-blink mode (?12) event (40 conformance tests)

- [x] Clickable tab bar: left-click a tab segment to switch tabs
- [x] UI/UX overhaul: Terminator-parity split keys; tab bar with
      per-tab ✕ / + / middle-click-close / always-show / top|bottom;
      unfocused-pane dimming; pane zoom (Ctrl+Shift+X); per-pane
      scrollbar; configurable split-divider color; cursor-blink
      interval; copy-on-select toggle (see docs/UX-COMPARISON.md)

- [x] Conformance: CHT/CBT tab navigation (41 conformance tests).
      DECSCA/DECSEL selective-erase unsupported upstream — documented.

- [x] Conformance: XTWINOPS CSI 18 t text-area size (8;rows;cols t),
      DSR CSI 5 n device-status (→ CSI 0 n), exact DA1 reply
      `CSI ? 6 c` incl. the `CSI 0 c` alias (44 conformance tests).
      CSI 14 t pixel size routes through a windowing callback — exercised
      live, not asserted headless.

- [x] kitty graphics Unicode placeholders — decode layer: the 297-entry
      row/column diacritic table, per-cell diacritic parsing, 32-bit
      image-id reconstruction (fg + msb diacritic), and the
      omitted-diacritic left-inheritance algorithm
      (`kettle-vt::placeholder`, 6 tests); plus `U=1` virtual placements
      in the kitty decoder (`a=p,U=1` / `a=T,U=1` register a rows×cols
      placement, store the image, draw nothing at the cursor; deleted
      with the image — 3 tests). Cited to
      `kitty/docs/graphics-protocol.rst:555`.

- [x] kitty Unicode placeholders — renderer path: per-frame grid scan for
      `U+10EEEE`, fg→image-id (256/truecolor/named) + diacritic decode,
      left-inheritance over contiguous runs, virtual image sliced per cell
      (`ImageData::crop`, `placeholder::tile_src_rect`) and drawn through
      the existing image pipeline (`Terminal::placeholder_tiles`, +3
      tests; 89 workspace tests). Placement-id via underline color is the
      remaining sub-item.

- [x] kitty placeholders: placement-id decoded from the cell underline
      color (drives spec run-grouping / left-inheritance; +1 test, 90
      workspace tests).

- [x] Font-feature tuning: `font-feature` parses real OpenType tags
      (`liga/calt/ss01/cv01/zero/…`) with `+`/`-`/`= N`/`on`/`off`
      dialects (repeatable, comma-lists), applied via cosmic-text
      `FontFeatures` on top of the ligature toggle; per-style family
      overrides already shipped (`FontFeature`, 92 tests). Cited:
      Ghostty `font-feature`, kitty `font_features`.

- [x] kitty animation (decode/state layer): `a=f` frame transmission
      (chunked, single in-flight slot; gap from `z`, `z<0` = gapless),
      `a=a` control (`c` current frame, `s` stop/run/loading, `v` loop
      count, `r`+`z` per-frame gap), `a=d,d=f` frame deletion;
      `KittyState::frames/animation` accessors (+2 tests, 94 workspace).
      Frame compositing (`a=c`), partial-rect frames and playback
      timing/rendering are the remaining sub-items.

- [x] kitty animation playback-timing engine: pure `current_frame(gaps,
      state, elapsed_ms)` — gapless-frame skipping, infinite/finite loop
      counts, `loading` hold-at-end, stopped→selected frame
      (`kitty::current_frame`, +1 test, 95 workspace). Only the renderer
      clock + frame→placement substitution remain.

- [x] kitty animation **plays end-to-end**: `a=f`/`a=a` snapshots flow
      through `Chunk::Animation` → per-terminal `Animations` registry; a
      placement's image is swapped for the playback-clock frame at draw
      time, and the UI schedules ~30 fps redraws while any animation
      runs. Root-gap via `a=a,r=1,z=`; cleared with the image / `d=f`
      (`AnimEntry`, +1 extractor test, 96 workspace tests). Cited kitty
      `graphics-protocol.rst:839`.

- [x] kitty animation frame compositing: partial-rect `a=f` frames
      blended/replaced over a previous-frame (`c=`) / `Y=` color /
      transparent canvas, `r=` edits a frame in place; `a=c` copies a
      rectangle between frames (incl. onto the root). RGBA `compose`
      (source-over) + `solid` primitives, +3 tests (99 workspace).
      Cited kitty `graphics-protocol.rst` frame-composition.

- [x] kitty relative placements (decode/state): `a=p,P=,Q=` recorded
      with parent image/placement + `H/V` cell offset; parent-deletion
      cascade (group lifetime); `RelativePlacement` + accessor (+1 test,
      100 workspace tests). Render-time position resolution from the
      parent is the remaining sub-item. Cited kitty
      `graphics-protocol.rst:682`.

- [x] kitty relative placements **render** for placeholder parents: the
      child image is drawn `(h,v)` cells from its parent virtual image's
      placeholder origin (min abs/col of its cells), via a `Relatives`
      registry + pure `relative_origin` clamp; parent off-screen ⇒ not
      shown; group lifetime cascades (+2 tests, 102 workspace). Non-
      placeholder / chained parents remain a sub-item.

- [x] kitty relative placements: **non-placeholder parents** (a regular
      placement's abs_line/col) **and relative chains** — pure
      `resolve_chain` walks child→parent with a depth bound of 8
      (`ETOODEEP`/cycles → not drawn); origins unified from placeholder
      cells + the image registry (+1 test, 103 workspace). The kitty
      graphics protocol surface is now complete.

- [x] Broader conformance sweep: IRM insert mode (shift-right),
      DECTCEM cursor visibility (`?25`), LNM mode bit (`CSI 20h/l`),
      DECCKM/DECKPAM application cursor+keypad, mouse-tracking DECSET
      flags (`?1000/1002/1003/1006`) set+clear — 5 end-to-end tests
      (108 workspace). LNM LF→CRLF output untranslated upstream (noted).

- [x] Dependency-free **fuzzy matcher** (`kettle_config::fuzzy`):
      subsequence scoring with prefix / word-boundary / camelCase /
      contiguity bonuses + length penalty; `score`/`best` (+3 tests,
      111 workspace). SSH launcher `Tab`-complete and `Enter` now
      fuzzy-match host names instead of prefix-only. Reusable by a
      future command palette.

- [x] **Command palette** (`Ctrl+Shift+K`): a fuzzy action launcher
      over a 29-entry command registry (`kettle_config::palette`,
      reusing the fuzzy matcher) — type to filter, `Tab`/`↑↓` select,
      `Enter` dispatch, `Esc` cancel; bottom-bar overlay reusing the
      SSH-launcher plumbing (+3 tests, 114 workspace).

- [x] Quick-select / hint-mode core (`kettle_core::hints`): pure
      detection of URLs / paths / git hashes / IPv4 across the visible
      rows (priority de-overlap, trailing-punctuation trim, char-column
      coords) + minimal-width unique label generator (home-row
      alphabet). +4 tests, 121 workspace. Overlay + keypress wiring is
      the next sub-item.

## Next (in priority order)
- [ ] Hint-mode UI: overlay labels + key-to-act (copy/open) reusing
      the search/palette plumbing; then detachable mux server; native
      macOS menu; signed packaging
- [ ] Detachable mux server (remote attach); broader `vttest` sweep
- [ ] Code-signed/notarized macOS build; Windows MSI; native macOS menu

## Quality bar each cycle

`cargo fmt` · `cargo clippy -D warnings` · `cargo build` · `cargo test` ·
end-to-end run · docs updated · commit.

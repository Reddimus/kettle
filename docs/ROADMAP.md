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
      (`Ctrl+Up`/`Ctrl+Down`); bash/zsh/fish/**powershell** snippets in
      docs/SHELL-INTEGRATION.md (cycle 730 added the PowerShell variant
      via `--shell-integration powershell` for Win11 + cross-platform
      PowerShell Core users)
- [x] OSC 9;4 taskbar progress (PowerShell 7 / Windows Terminal parity):
      the focused pane's `ESC]9;4;state;pct` drives the Windows taskbar
      button via `ITaskbarList3` (normal/error/indeterminate/paused);
      no-op off Windows (cycle 745)
- [x] OSC 9 / OSC 777 desktop notifications: PTY programs can request a
      bounded desktop notification with `OSC 9 ; message` or
      `OSC 777 ; notify ; title ; body`; taskbar progress remains the separate
      `OSC 9;4` path.

- [x] Session save/restore: OSC 7 cwd capture, tab/split tree + per-pane
      cwd serialized to session.json, restored on launch, autosaved on
      structural changes + exit

- [x] SSH multiplexing: per-pane argv, `Ctrl+Shift+S` SSH launcher with
      configured `ssh-host` tab-complete, SSH tabs persisted in sessions

- [x] Automated test harness (318 deterministic tests across vt/config/
      core/ui/render: decoders, extractor, config, keybinds, split-tree,
      session round-trip, 8 MiB perf guard, plugin LuaCommand contracts,
      detachable-tabs serialize/insert, GPU offscreen) + CI fmt-check,
      headless GPU smoke, CLI smoke on all three OSes — see
      docs/TESTING.md. The "automated test harness" entry was first
      logged at 19 tests circa v0.2 and now reflects v1.40.0; the
      same per-cycle drift-guard discipline grew it.

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
      `CSI ? 6;4;52 c` incl. the `CSI 0 c` alias and DECID, advertising
      the shipped sixel and OSC 52 surfaces (44+ conformance tests).
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

- [x] Quick-select **hint mode** wired end-to-end (`Ctrl+Shift+H`):
      labels detected URL/path/hash/IP targets over the focused pane
      (chip quads + glyphs), type a label to open URLs / copy others,
      `Esc` cancels; reuses the overlay + key-routing plumbing.

- [x] New tabs/splits inherit the focused pane's OSC-7 cwd
      (WezTerm/iTerm/kitty parity); stale dir → default fallback
      (`usable_cwd`, +1 test).

- [x] Security: OSC 52 clipboard *writes* size-capped (1 MiB, char-
      boundary safe) against unbounded-payload abuse (+1 test).
- [x] Security: OSC 52 clipboard *read* denied by default
      (`osc52 = off|copy|paste|both`, default `copy`) — blocks remote
      clipboard exfiltration; empty well-formed reply (+1 test).
- [x] Security: untrusted URIs from terminal output are scheme-allowlist
      checked (`links::is_safe_url`) before the OS opener — blocks
      custom-scheme handler abuse, controls, traversal (+1 test).
- [x] Search follows matches into scrollback: the viewport scrolls to
      the active match (≈⅓ from top), deduped per match/query so manual
      scroll still works. Pure `search::reveal_offset` (+1 test).
- [x] `kettle --list-keybinds` prints the default keymap
      (`Trigger::label` + `describe_defaults`, sorted; +2 tests) —
      discoverable without reading source.
- [x] Runtime theme choice **persists across restarts** (saved in
      `session.json` as `theme`, `#[serde(default)]` so old files still
      load; applied on restore unless config-reloaded). +session tests.
- [x] **Runtime theme switching**: `next_theme`/`prev_theme` actions
      (+ "Next/Previous theme" palette entries) cycle the bundled themes
      live (no config edit/reload); pure `Theme::cycle` wrap-around,
      unknown→first (+1 test).
- [x] Scrollbar is now **click-to-jump and drag**: left-click the
      right-edge bar to scroll there, then drag to scrub (x ignored
      once grabbed, like every scrollbar). Pure `kettle_core::scrollbar`
      (`thumb` + `target_offset`) shared by renderer + UI (+3 tests).
- [x] `--config FILE` override: used by the run, live-reload watcher,
      `--config-path`, `--check-config` and `--screenshot` (+CLI test).
- [x] Middle-click in the content area **pastes PRIMARY first on Linux**
      (clipboard fallback elsewhere, bracketed-paste-safe); `Action::Paste`,
      `Action::PastePrimary`, middle-click, and PuTTY-style right-click all
      share the same hardened paste funnel.
- [x] OS window title tracks the **active** pane (tab/focus switches,
      not only OSC events); deduped; pure `window_title` (+1 test).
- [x] Alt-drag **rectangular (block) selection** (`SelectionType::Block`,
      iTerm2/Alacritty/WezTerm parity) via a pure `selection_kind`
      mapping (+1 test); copy-on-select fires on release for drags.
- [x] Standard CLI launch args: `-e/--exec CMD…` (run a command instead
      of the shell, consumes the rest incl. hyphenated flags) and
      `-d/--working-directory DIR`; override a saved session for the
      first tab. `kettle_ui::run_with(Options)` (+1 parse test).
- [x] **OSC 4 / 10 / 11 / 12 color queries reply** with the canonical
      xparsecolor `rgb:RRRR/GGGG/BBBB` form. The engine raised
      `ColorRequest` events but the app silently dropped them, so
      neovim/vim/tmux couldn't detect the active fg/bg/cursor/palette.
      Pure `kettle_render::reply_for_query` resolves against theme +
      OSC overrides; out-of-range → no reply. +2 tests.
- [x] **`CSI 14 t` (text-area pixel size) replies.** The engine
      raised `TextAreaSizeRequest(fmt)` but the app dropped it, so
      sixel/kitty/iTerm2 image apps fell back to guessed cells. Pure
      `kettle_render::reply_for_text_area_size` plugs in the live cell
      + grid dimensions and the formatter renders the canonical
      `CSI 4 ; h ; w t` xtwinops reply. +1 test.
- [x] **DEC mode 12 (cursor blink) honors the running program.**
      `CSI ?12 h/l` from vim/neovim/shell prompts was previously
      ignored — the app's blink decision was bound to the static
      config. Now intersects with the engine's live
      `cursor_style().blinking` per pane via a `cursor_blinking()`
      accessor; `CursorBlinkingChange` resets the blink phase so
      blink-off shows a solid cursor instantly. +1 test.
- [x] **DECSCUSR cursor shape + DEC ?25 visibility from the running
      program.** Vim/neovim/fish flip shape per-mode and full-screen
      TUIs hide the cursor; both were ignored — renderer used the
      static `cursor-style` config. Now seeds the engine's
      `default_cursor_style` from the config and reads
      `RenderableContent.cursor.shape` (which the engine collapses
      `?25 l` into `Hidden`, single guard for both). Added
      `HollowBlock` rendering. +2 tests.
- [x] **Modified named-key encoding** (xterm modifyCursorKeys
      family). `Ctrl+Right` skip-word, `Ctrl+Delete` delete-word,
      `Shift+Tab` back-tab, modified arrows / F-keys / nav keys all
      used to collapse to their unmodified sequence. New
      `xterm_modifier` helper + `CSI 1;<m>…` / `CSI <n>;<m>~` /
      `CSI 1;<m>P..S` for F1-4 / `CSI Z` for Shift+Tab. +2 tests.
- [x] **`mouse-hide-while-typing` + selection clear on typing.**
      OS mouse cursor hides on keystroke (re-shows on next motion),
      and the focused pane's selection clears on any keystroke that
      produces PTY bytes — Alacritty / kitty / WezTerm / iTerm2
      parity. +1 config test.
- [x] **Mouse wheel over tab bar cycles tabs** (kitty / iTerm2 /
      Ghostty parity). Pure `cursor_in_tab_bar_band` geometry
      helper handles top/bottom positions + hidden bar. +1 test.
- [x] **Shift+Click / right-click extend the selection**
      (xterm / iTerm2 / Alacritty / WezTerm convention). Shared
      `extend_selection_to_cursor` updates the existing selection's
      right edge and enters drag mode. Bare Shift+Click on empty
      space falls through to a normal new-selection; bare
      right-click on empty space is a no-op.
- [x] **`word-delimiters` config** (alacritty
      `selection.semantic_escape_chars` parity, aliases
      `selection-word-chars`, `semantic-escape-chars`). Customizes
      double-click word boundaries; empty = engine default. +1 test.
- [x] **Selection auto-scrolls past pane edge during drag.** Pure
      `selection_autoscroll_lines(y, top, bottom)` rate ladder
      (1/2/3 lines per frame by overshoot); `about_to_wait` keeps a
      30 fps tick alive while the drag is active. +1 test.
- [x] **`move_tab_left` / `move_tab_right` actually move the tab.**
      The actions were bound and parsed but the handler was empty —
      `Ctrl+Shift+PageUp/Down` silently did nothing. New
      `Mux::move_active_tab(delta) -> bool` swaps with clamp (no
      wrap). +1 test.
- [x] **OSC 12 (set cursor color) reaches the render path.**
      Engine parsed it and populated `Colors[258]`, renderer
      hard-wired `theme.cursor` — silent drop, mirror of the OSC
      color *query* bug. Renderer now calls `resolve_query(258,…)`
      so override wins over theme. +1 test (OSC 10/11/12 set
      populates slots 256/257/258 exactly).
- [x] **OSC 7 cwd handles UTF-8 percent-encoded paths.** Shells
      encode each UTF-8 byte separately (`%C3%A9` for `é`); the old
      parser produced `Ã©`. Now decodes into a byte buffer and
      converts via `from_utf8_lossy`. +1 test.
- [x] **Bracketed paste strips both 200~ and 201~ markers.** The
      close-marker injection was already guarded, but a hostile
      paste containing the *open* marker could trap the shell in
      paste mode past the wrapper's real close. +1 test paired
      symmetrically with the existing close-marker test.
- [x] **`kettle --check-config` echoes every per-cycle gate.**
      Resolved cursor / bell / OSC 52 / scroll / mouse / tabs /
      title / word-delimiters / ssh values now appear so a user
      can verify their tweaks took effect without grepping source.
- [x] **`TERM_PROGRAM_VERSION` env on spawned shells.** Pairs with
      the existing `TERM_PROGRAM=kettle` so neovim `:checkhealth`,
      fish themers, and shell diagnostics see "kettle v0.1.0"
      instead of an unknown program. Sourced from
      `env!("CARGO_PKG_VERSION")`.
- [x] **Full xterm Ctrl+<punctuation> C0 row.** Added the missing
      mappings: `@` (NUL), `^` (RS 0x1E, vim alt-buf / tmux),
      `_` (US 0x1F), `/` (US 0x1F, tmux/nano undo). +1 test.
- [x] **OSC 4 multi-index query conformance pinned.** `OSC 4 ; 1
      ; ? ; 7 ; ?` (used by tmux / neovim 0.10 / base16-shell-hook
      to batch palette probes) now has an end-to-end test
      confirming one `ColorRequest` per pair. +1 test.
- [x] **`Ctrl+Backspace` = BS (0x08) for delete-word muscle
      memory.** Plain BS still = DEL (0x7F), Alt+BS still =
      ESC+DEL — only the Ctrl flavor was collapsing to plain.
      Alacritty/xterm/Ghostty parity. +1 test.
- [x] **`Alt+1..9` direct tab access** (kitty / Terminator /
      iTerm2 / Ghostty). `Action::GotoTab(u8)` handler existed but
      was orphaned — no `goto_tab:N` parser and no default keybind.
      Now bound by default + `keybind = alt+5=goto_tab:5` parses
      (1-based; 0 rejected to surface the ambiguity). +2 tests.
- [x] **OSC 11 default-bg override reaches the chrome.** Mirror
      of the OSC 12 cursor-color fix in cycle 56 — surface clear,
      active tab-bar segment, and per-cell default-bg check all
      now read from `term_colors[257]` instead of hard-wiring
      `theme.background`.
- [x] **OSC 10 default-fg override reaches per-pane text default.**
      Glyphon `TextArea.default_color` (fallback for spans without
      explicit color) now reads from `term_colors[256]` per pane.
      Chrome (tab bar text) keeps `theme.foreground`.
- [x] **`Action::NewWindow` spawns a separate kettle process.**
      Was collapsed with `Action::NewTab`; Ctrl+Shift+I silently
      opened just a tab. Now `current_exe()` + detached `spawn`;
      falls back to a tab on platforms where the path doesn't
      resolve.
- [x] **`SIGPIPE` restored to `SIG_DFL` at startup.**
      `kettle --list-themes | head` was panicking with "failed
      printing to stdout: Broken pipe" because Rust's runtime
      ignores SIGPIPE by default. Now exits cleanly like every
      other CLI tool.
- [x] **`--screenshot --cols`/`--rows` clamp** to `[20, 400]` and
      `[8, 200]` so a typo like `--cols 100000` no longer panics
      with the wgpu 8192-px texture-size validation error. The
      "wrote" line reports the actual cell dimensions used.
- [x] **`--check-config` surfaces malformed numeric values**
      (font-size, padding, opacity, scroll-multiplier, contrast,
      scrollback, blink-interval). New
      `Config::detect_malformed_values` side scan. +1 test.
- [x] **Shift bypasses mouse tracking** (xterm/Alacritty/kitty/
      Ghostty convention). htop/tmux/vim mouse mode no longer
      locks out local selection and scrollback — hold Shift to
      claim the mouse for kettle.
- [x] **Ctrl+Plus font-zoom works on US layouts.** `Ctrl+Shift+=`
      and `Ctrl+Shift++` (the actual chord users press when they
      think "Ctrl+Plus") and Ctrl+Shift+-/_ now bound. +1 test.
- [x] **Tab title `truncate` honors display columns.** CJK chars
      and emoji (2 cells each) no longer overflow the tab segment.
      Uses `UnicodeWidthChar::width()`. +1 test.
- [x] **Local paste capped at 4 MiB.** Pair to the OSC 52 1 MiB
      cap (cycle 47) — guards against accidentally pasting a multi-
      GB file from the clipboard and freezing the PTY. Reuses
      `clamp_osc52` byte-clamper.
- [x] **`selection-foreground` actually applied.** Config key was
      parsed but ignored at render time. Selected cells now get
      `theme.selection_foreground` (applied after INVERSE so the
      highlight always reads).
- [x] **OS cursor → pointing hand over Ctrl-clickable URLs.**
      Browser/iTerm2/Ghostty affordance; re-syncs on cursor move
      *and* modifier change. Deduped via `last_cursor_icon`.
- [x] **SGR 2 dim/faint rendered.** Engine tracked `Flags::DIM`
      but the renderer ignored it; new `color::dim(fg, bg)` blends
      halfway toward bg (50 % intensity). Applied before
      min-contrast lift. +1 test.
- [x] **SGR 4 underline + SGR 9 strikeout rendered.** Engine
      tracked `Flags::UNDERLINE`, `Flags::UNDERCURL`,
      `Flags::STRIKEOUT` but renderer never drew them. 1-px line
      at `cell_bottom-2` for underline, `cell_mid` for strikeout.
      Curly variant draws plain underline for now; real wave
      needs a shader tweak (deferred).
- [x] **SGR 58 per-cell underline color respected.** Renderer
      reads `cell.underline_color()` for the underline quad; vim
      spell-check / LSP diagnostics now draw their red squiggles
      under otherwise-normal text. +1 conformance test.
- [x] **All five underline-style flags drawn.** UNDERLINE,
      DOUBLE_UNDERLINE, UNDERCURL, DOTTED_UNDERLINE,
      DASHED_UNDERLINE — keyed on `Flags::ALL_UNDERLINES`. Double
      draws two stacked lines; the others draw single lines (wave
      / dotted / dashed visual styles deferred to a shader pass).
      +1 conformance test.
- [x] **Session restore brings back the focused pane.** `STab`
      records `focus: usize` (DFS-order index, `#[serde(default)]`
      for back-compat). +2 tests.
- [x] **`focused-split-color` config key.** Inactive border was
      already overridable via `split-divider-color`; the focused
      border (the "here am I" accent) was hard-wired. +1 test.
- [x] **`--check-config` catches malformed color values.**
      Extended `detect_malformed_values` to also flag bad
      `background`/`foreground`/`cursor-color`/selection/search/
      split-color/palette inputs (each routed through
      `Rgb::parse`, same path the apply arm uses). +1 test.
- [x] **`--check-config` catches malformed `keybind = …` lines.**
      Bad trigger or unknown action no longer silently drops the
      binding without a warning. +1 test.
- [x] **`--check-config` catches unknown theme names.**
      `Theme::by_name` silently falls back to TokyoNight Night
      on a typo; now scans against `Theme::list()` so a
      copy-pasted theme name from another terminal's config
      flags loudly. +1 test.
- [x] **`--check-config` catches unknown enum values.**
      `cursor-style`/`bell`/`osc52`/`tab-bar`/`tab-bar-position`/
      `scrollbar` all had `_ => Default` fallthrough — typos
      flagged as malformed-value now. +1 test (7 bad + ~25 good).
- [x] **`--check-config` catches `font-feature` token typos and
      `ssh-host` lines missing `=`.** Each `font-feature` token
      validated via `FontFeature::parse`; `ssh-host` requires a
      non-empty `name=target` split. +1 test.
- [x] **Tab title falls back to cwd basename pre-OSC 2.** Fresh
      tabs show the working-directory name until the shell
      emits its first title. iTerm2/Ghostty/WezTerm parity. +1
      test.
- [x] **OS window title gets the same cwd-basename fallback.**
      `window_title` (the `Window::set_title` source) now mirrors
      the tab-title behavior; a cwd literally named "kettle"
      no longer collapses the substitution. +2 test asserts.
- [x] **`--check-config` echoes window padding, opacity, split
      colors.** The cycle-59 expansion missed
      padding/opacity/unfocused-split-opacity and the cycle-83
      split-color overrides; new `window:` line (always shown) +
      conditional `splits:` line surface them.
- [x] **`--list-keybinds` renders `Goto tab N` (1-based).** The
      Debug-derived `GotoTab(0)` label leaked the 0-based
      internal index; new `action_label` helper renders the
      1-based human form for `GotoTab` and falls back to Debug
      for the rest. +1 test.
- [x] **SSH tab title seeded from the target.** Fresh SSH tabs
      and session-restored ones with an `ssh` argv now read
      `ssh <target>` until the remote shell sends its first
      OSC 2 — cycle-89's cwd-basename fallback can't help SSH
      panes since they have no local cwd. Pure
      `initial_pane_title(argv)` helper wired into `spawn_pane`
      so both fresh and restored paths share it. +1 test.
- [x] **`CONTRIBUTING.md` added.** Documents the audit-
      cycle pattern that has driven 150+ commits so a new
      contributor can land their first change with the same
      shape — bounded bug → pure helper → wire → pin →
      gate → docs → commit. Real-example walkthrough of
      cycle 151. Linked from README's documentation
      section.
- [x] **macOS release build is actually universal.**
      Artifact has been named `kettle-macos-universal.zip`
      since project genesis but contained a single-arch
      binary. Now uses dual `cargo build --target` +
      `lipo -create` to produce a true universal2 binary.
      Linux + Windows native unchanged.
- [x] **`--check-config` skips empty values.** parse.rs
      documents empty-as-reset semantics; runtime honors it
      (cycle 121/122 + every enum/bool/numeric defaulting on
      empty). But `detect_malformed_values` still flagged
      `theme = ""` etc., disagreeing with the runtime. Now
      one empty-skip gate covers every key. +1 test.
- [x] **Tab-close-middle-click + `CloseWindow` save
      session.** Two exit paths skipped the save that other
      exit paths had. Next launch restored the stale
      multi-tab state from BEFORE the close instead of
      starting fresh.
- [x] **`detect_malformed_values` also strips BOM.** Cycle
      156 sibling to cycle 155 — the diagnostic path does
      its own scan and was still surfacing
      `missing `=` separator: "\u{feff}font-family"` with the
      invisible BOM mangled into the user-facing message.
      +1 test.
- [x] **Config parser strips leading UTF-8 BOM.** Notepad-
      saved config files prepended 0xEF 0xBB 0xBF to byte 0,
      making the first key parse as `\u{feff}theme` and
      surface as an "unknown key" with an invisible character.
      +1 test.
- [x] **Modal open closes any other modal first.** A user
      hitting Ctrl+Shift+K with the SSH launcher already up
      saw both overlays render at once. New
      `close_all_modals()` helper extracted from cycle 111's
      Reset sweep; the four modal-open actions
      (StartSearch / OpenSsh / CommandPalette / HintMode)
      call it before setting their own state.
- [x] **Workspace `repository` URL fixed
      (`kevim/kettle` → `Reddimus/kettle`).** Stale metadata
      in `Cargo.toml`; affects future crates.io / cargo
      install / scrape paths.
- [x] **Session-restore theme check agrees with by_name
      case-handling.** `Theme::list().contains(&name)` is
      case-sensitive; `Theme::by_name` is case-insensitive.
      A lowercase-stored theme name would skip the
      restore. Now both use case-insensitive comparison.
- [x] **Live config reload only fires for actual config
      file changes.** The notify watcher reloaded on
      every event in the dir; cycle 109's atomic session
      save (write-temp + rename) was firing 3+ pointless
      reloads per save. Filter on `event.paths == config`.
- [x] **DEC ?25l hides cursor even when unfocused.** The
      `draw_cursor` gate forgot to check `cursor_visible`;
      the unfocused-window hollow-outline branch fell
      through even when a TUI had sent `\e[?25l`. Now the
      flag gates the whole branch.
- [x] **`--screenshot` honors `background-opacity`.**
      Sibling to cycle 148. Screenshot clear-op hardcoded
      `a: 1.0`; now routes through cfg.background_opacity
      like the live-window path. PNG is RGBA8 so the alpha
      lands directly in the output.
- [x] **`background-opacity` produces real transparency.**
      Surface used `caps.alpha_modes[0]` which is usually
      `Opaque` — clear-op alpha got discarded by the
      composite. Now prefer `PreMultiplied` →
      `PostMultiplied` → `Inherit` → `Auto` when opacity
      < 1.0. Opaque configs unchanged.
- [x] **`Action::from_name` is case-insensitive +
      trimmed.** Same pattern as cycle 146 on the keybind
      action surface. `keybind = ctrl+shift+c = Copy`
      finally resolves to Copy; pre-fix it silently
      dropped (diagnostic flagged it but runtime
      ignored). +1 test.
- [x] **Enum config keys are case-insensitive.** Cycle 138
      made bools case-insensitive; cycle 146 finishes the
      job for the six enum keys (`bell`, `osc52`,
      `tab-bar`, `tab-bar-position`, `scrollbar`,
      `cursor-style`). Diagnostic + runtime agree on case
      variants. +1 test.
- [x] **`--list-themes` case-insensitive alphabetical.**
      Was ASCII-bytewise (uppercase ahead of lowercase →
      `CGA` before `branch`). Now matches `sort` defaults in
      a UTF-8 locale; `branch` < `Calamity` etc. Also flows
      to `next_theme` / `prev_theme` cycle order.
- [x] **Tab-close clicks (middle / ✕) reset blink phase.**
      Last user-driven focus path missing the cycle 134-141
      pattern. `close_tab_at` shifts focus to a neighbor;
      the now-active pane's cursor lands visible
      immediately.
- [x] **`docs/CONFIG.md` documents bool aliases / numeric
      clamps / `beam` alias.** Added a "Type notes"
      preamble listing the cycle-138 bool aliases and the
      cycles-118/131/132/133 numeric clamp ranges. Cursor-
      style row updated with the `beam` alias.
- [x] **`cursor-style = beam` aliases `bar`.** Alacritty
      refugees writing their old spelling no longer get a
      silent Block fallback. Diagnostic no longer flags it.
      +1 test.
- [x] **Typing resets blink phase.** Last user-gesture path
      to gain the cycle 134/135/136/140 blink-reset.
      Alacritty / kitty / iTerm2 / WezTerm all do it on
      every keystroke; matches the rest of kettle's
      user-driven paths.
- [x] **Modal-close paths reset blink phase.** Cycle 134
      covered Action::Reset; cycles 135/136 the focus
      changes. Escape closing search/palette/hint/ssh
      overlays still left the cursor potentially invisible
      for up to one blink_interval. Centralized via new
      `reset_blink_phase` helper (5 call sites + 1 inline
      for the borrow-conflicted CursorBlinkingChange).
- [x] **`font-size` clamps at parse-time too.** Cycle 118
      clamped at renderer-time; cycle 131 added the
      diagnostic; this cycle clamps at parse so
      `cfg.font_size` and the renderer agree. `font: ...
      500pt` in `--check-config` now reads as `font: ...
      72pt` (with the malformed-value diagnostic still firing).
- [x] **Bool config keys accept `yes`/`no`/`off`/`on`/`0`/
      `1`/`enabled`/`disabled` + flag typos.** All five
      bool fields used `e.value != "false"` so any non-
      literal-"false" value silently meant `true` —
      `cursor-style-blink = no` enabled the blink, etc.
      New `parse_bool` helper; bad values keep current state
      instead of flipping; `--check-config` flags typos.
      +1 test.
- [x] **`Renderer::resize` ceiling-clamps at the device's
      max texture dimension.** A window stretched past 8192
      px used to silently fail surface.configure and freeze.
      Clamps now at device.limits().max_texture_dimension_2d.
      Sibling to cycle 119's screenshot-side fix.
- [x] **Mouse focus changes also reset blink phase.**
      Extracted cycle 135's pre/post into `focus_key()` +
      `note_focus_change(pre)` helpers; click-a-tab and
      click-a-pane now share the keyboard path's blink-
      reset behavior. Three call sites, one helper pair.
- [x] **Any focus-changing action resets blink phase.**
      Extends cycle 134 from `Action::Reset` to every action
      that flips which pane has focus (NextTab/PrevTab/
      GotoTab/FocusNext/Prev/Up/Down/Left/Right/ToggleZoom).
      Snapshot pre + compare post around the match; reset
      on diff. No per-arm decoration needed.
- [x] **`Action::Reset` resets cursor blink phase too.**
      Cycle-111 swept modals/selection but missed
      `blink_on`/`last_blink`. Hit Reset on the off-half of
      the blink and the cursor stayed missing for up to one
      interval. Now `blink_on = true; last_blink = now()`
      alongside the existing sweep. Mirrors the
      CursorBlinkingChange handler.
- [x] **`scrollback` clamped at INFINITE_SCROLLBACK (10 M
      lines); over-cap flagged.** Sibling to cycle 132 but
      this one was a memory footgun: `scrollback =
      100000000` would have reserved ~250 GB of history
      rows on first PTY spawn. Cycle 133 clamps + adds the
      diagnostic. The three documented forms (`infinite`,
      `unlimited`, `0`) still resolve to the same cap.
      +1 test.
- [x] **Other 4 clamped numerics warn out-of-range +
      `background-opacity` clamps at parse.** Sibling to
      cycle 131. `background-opacity` had no clamp and could
      reach wgpu with undefined alpha; clamped to [0, 1] now.
      The four already-clamped fields (`unfocused-split-
      opacity`, `scroll-multiplier`, `minimum-contrast`,
      `cursor-blink-interval`) gain `--check-config`
      diagnostics so the user sees the silent clamp. +1
      test covering 9 out-of-range + 14 in-range/boundary.
- [x] **`--check-config` flags `font-size` outside [5, 72].**
      Cycle 118 added a runtime clamp; --check-config still
      echoed `font: 500pt` verbatim with no mention of the
      silent runtime cap to 72pt. Now surfaced as malformed
      (same pattern as cycle 124's palette index ≥ 16). +1
      test covering all four out-of-range + four in-range.
- [x] **`Mux::split` exits zoom (was hiding the other half).**
      Splitting a zoomed pane used to leave zoom on while
      focusing the new pane — so `layout()`'s collapse made
      the just-split-from half silently vanish. tmux/WezTerm
      both exit zoom on split because "show me both" is the
      intent. Extracted `insert_split(&mut Tab, new_id,
      dir)` helper; +1 test.
- [x] **TESTING.md + INSTALL.md test counts caught up to
      213 (was 20 / 33).** 80+ cycles of additions had
      drifted past the testing docs. Rewrote TESTING.md
      with current counts (2/56/75/10/37/33 per crate),
      broader category descriptions referencing the
      audit-cycle pattern, and pointers to CHANGELOG.md
      for per-cycle detail.
- [x] **`--screenshot` pre-validates `.png` extension.**
      Sibling to the cycle-106/107 CLI hard-fails. Bad
      extensions used to surface as a cryptic crate-internal
      error AFTER full GPU work; now caught up-front with a
      named error (case-insensitive `.png` ok).
- [x] **README Quick-start block catches up to the
      introspection surface.** Missing `--list-actions`,
      `--list-ssh-hosts`, `--screenshot`; `--list-keybinds`
      still claimed "default" not "effective"; `--config`
      lacked the cycle-106 hard-fail caveat. All four lines
      added/reworded. README and `--help` are now both
      truthful sources.
- [x] **`--help` text catches up to cycles 103/105/106.**
      `--list-keybinds` help said "default keymap" but it
      now shows the effective map (cycle 103). `--config`
      help didn't mention `--list-keybinds` /
      `--list-ssh-hosts` consumers (cycles 103/105) nor the
      hard-fail on missing path (cycle 106). Both updated.
- [x] **README keybind table surfaces 9 hidden defaults +
      docs-drift guard.** SSH launcher, command palette,
      quick-select hints, split-auto, new window, pane zoom,
      jump-prompt, move-tab, goto-tab-N were all bound but
      unsurfaced in the keybind table. New test
      `readme_documented_chords_are_actually_bound` pins
      each promoted chord so a future rebind catches the
      docs drift here, not after a user PR. +1 test, plus a
      footer pointer to `kettle --list-keybinds` for the
      effective map.
- [x] **`palette = N=#hex` with N ≥ 16 is flagged + docs
      scoped to reality.** Example config advertised
      0..=255; runtime only wrote 0..=15; the 16+ overrides
      silently no-op'd. `--check-config` now flags the typo;
      example text notes the OSC 4 escape-hatch for runtime
      256-color overrides. Full static-config support for
      16..255 deferred (would touch Theme + renderer
      resolve). +1 test.
- [x] **`Action::NewWindow` inherits `--config FILE`.** A
      user with `kettle --config /custom.conf` who opened a
      new window via Ctrl+Shift+I got a child loading the
      default config — their theme/font/keybinds vanished
      from the new window with no warning. The spawn now
      passes `--config self.config_path` to the child.
- [x] **`command =` clears, `ssh-host = empty=...` is
      dropped.** Sibling to cycle 121. Empty `command` used
      to leave `Some("")` and break `shell_argv`; now
      clears to None and falls back to `$SHELL`. Half-empty
      `ssh-host = name=` / `= target` entries that
      `--check-config` already flagged (cycle 88) are now
      also rejected at parse time so the runtime list and
      the diagnostic agree.
- [x] **Empty string-config values stop silently breaking
      rendering.** `font-family =` used to set the family
      to `""`; renderer drifted into cosmic-text's silent
      fallback. Now: empty → keep previous (font-family /
      theme) or reset to None (per-style families). The
      parser docstring's "empty value resets" promise is
      finally honored. +1 test covers all five keys.
- [x] **`Mux::reap` doesn't silently shift focus to a
      different tab.** When a tab BEFORE active died, the
      trailing-only clamp left `active` indexing the wrong
      tab (the tab that filled the removed slot). Now
      decrements `active` per-removal when `ti < *active`.
      Logic extracted to pure `reap_tabs(&mut Vec<Tab>,
      &mut usize, &[u64])`. +1 test covers 5 scenarios.
- [x] **`--screenshot` dynamically caps cells to fit the
      wgpu 8192-per-side texture limit.** Cycle 69's static
      `[20, 400]×[8, 200]` clamps were safe at small fonts but
      busted at 72pt cells (~90px tall × 200 rows = 18000px).
      New pure `cap_axis_cells(req, cell, chrome) -> u32`
      caps each axis runtime-aware; `capture_png` returns the
      actual rendered (cols, rows) so the CLI message says
      `capped from N×M for GPU texture limit at current font
      size` instead of lying. +1 test.
- [x] **`Renderer::new` clamps `cfg.font_size`.** Cycle 73's
      [5.0, 72.0] bound only fired through `set_font_size`
      (runtime); startup took `cfg.font_size` raw. Extreme
      configs (`font-size = 200`) booted oversize cells and
      could hit the wgpu 8192px texture limit. New pure
      `clamp_font_size(f32) -> f32` shared by both setters.
      Verified end-to-end: `font-size = 500` config renders.
- [x] **Palette gained 5 missing actions + a drift guard.**
      `ScrollLine{Up,Down}` (cycle 110), `ScrollPage{Up,Down}`,
      `HintMode`, and `MoveTab{Left,Right}` had keybinds but
      no palette label — Ctrl+Shift+K couldn't reach them.
      New test `palette_includes_every_user_facing_action`
      enumerates every Action variant via an explicit match
      (compile-time exhaustiveness) and asserts each one is
      either in the palette or in a curated `excluded` list
      with a rationale. Catches future drift the same way
      cycle 104's drift test does for `--list-actions`.
- [x] **Shadow-collision audit guards `defaults()`.** New
      `defaults_audit() -> (Bindings, Vec<Trigger>)` records
      every bind call; new test asserts `map.len() ==
      triggers.len()` and panics with the duplicate set if
      not. Catches the class of bug cycle 115 fixed
      one-shot, so a future re-introduction fails CI with a
      named-offender message rather than going unnoticed.
- [x] **Cycle-110 keybind collision fixed.** Cycle 110's
      `Ctrl+Shift+Up/Down → ScrollLineUp/Down` was silently
      shadowing the older `Ctrl+Shift+Arrows → Resize<dir>`
      quartet for Up/Down (Left/Right still resized — half
      shadow, fully inconsistent). The Resize-via-Ctrl+Shift
      quartet was a duplicate of `Shift+Arrows → Resize<dir>`
      (already bound), so dropping it costs nothing. Test grew
      negative guards on `Ctrl+Shift+Left/Right` being free,
      positive guards on `Shift+Arrows → Resize<dir>`. README
      keybind table updated to reflect both fixes (drop the
      `Ctrl+Shift+Arrows` resize column, add a scroll-line /
      page / top-bottom row).
- [x] **`--check-config` echoes `font-feature` count + per-
      style font-family overrides.** Symmetric with the
      existing `ssh: N host(s) configured` line: opt-in keys
      print only when actually set. Surfaces ligatures-toggle
      state too.
- [x] **`Action::CloseWindow` finally closes the window
      (was an alias for `CloseTab`).** Both variants existed
      but the handler arm folded them together to
      `close_tab()`. Now distinct: `CloseTab` is just-this-tab;
      `CloseWindow` drops every tab + pane via new
      `Mux::close_window()`. +1 test.
- [x] **Broadcast scoped to the active tab (not every pane).**
      `Mux::broadcast_write` was iterating the entire panes
      map — typing one char with broadcast on echoed into other
      tabs' panes too (often unrelated work). Now walks
      `tabs[active].root.leaf_ids()`. New `Node::leaf_ids ->
      Vec<u64>`. +1 test.
- [x] **`Action::Reset` sweeps kettle's local UI state too.**
      RIS (`ESC c`) reset the engine but selection, search,
      command palette, hint mode, and SSH launcher all
      survived — leaving stale chrome over a fresh grid. Now
      cleared as part of the action.
- [x] **`scroll_line_up` / `scroll_line_down` actions
      (Ctrl+Shift+Up/Down).** Filled the gap between full-screen
      `Shift+PageUp/Down` and extreme `Shift+Home/End` —
      Alacritty / kitty / WezTerm all ship this chord. +1 test.
- [x] **`Session::save` is atomic (write-temp-then-rename).**
      Cycle 108 fixed the symptom of corrupted-session loads;
      this fixes the cause. Old `fs::write` was non-atomic —
      mid-write kettle death left a half-written file. New
      `save_to_path(s, p) -> io::Result<()>` writes to a
      `.tmp.<pid>.<nanos>` sibling, renames into place; pub
      `save` logs `log::warn!` on any I/O error instead of
      silently swallowing. +2 tests.
- [x] **Corrupted `session.json` is backed up, not silently
      discarded.** A parse error now logs a warning AND
      renames the file to `session.json.broken.<unix-secs>`
      so the user has a forensic artifact and the next save
      doesn't overwrite it. Logic in pure
      `load_from_path(&Path) -> Option<Session>`; +3 tests
      (missing-silent, corrupted-renamed, happy-path-untouched).
- [x] **`--working-directory /typo` hard-fails (exit 1).**
      Sibling to cycle 106. Engine silently used `$HOME` when
      the directory didn't exist; CLI now distinguishes
      "no such file or directory" vs "not a directory" so the
      user sees which kind of typo it was.
- [x] **`--config /typo.conf` hard-fails (exit 1).** Every
      downstream branch silently dropped to `Config::default()`
      when the explicit `--config` path didn't exist. Hard-fail
      at the top of `main` so the diagnostic lands where the
      typo is. Omitting `--config` still falls back silently
      (out-of-the-box path).
- [x] **`--screenshot` uses `Config::load_from` like the rest.**
      Lone hold-out using open-coded `parse_collect` that didn't
      `log::warn!` on malformed values/unknown keys.
- [x] **`--list-ssh-hosts` prints configured ssh-host entries.**
      Companion to `--check-config` (only the count) and the
      Ctrl+Shift+S launcher (in-window). Two-column table
      aligned to longest name (floor 4), sorted; empty configs
      print an explicit fallback line. Formatting in pure
      `format_ssh_hosts(&[...]) -> Vec<String>` so the table
      layout is unit-testable. +1 test.
- [x] **`--list-actions` enumerates valid `keybind` action
      names.** Onboarding inverse of `--list-keybinds`: shows
      what `trigger=…` values parse, sorted, with the
      parametric `goto_tab:N` and `unbind` sentinel as footer
      lines. New `keybinds::action_names() -> Vec<&'static
      str>`; drift-tested against `Action::from_name`. +1 test.
- [x] **`--list-keybinds` honors `--config` and shows the
      *effective* keymap.** Previously always printed defaults;
      no CLI way to confirm overrides + unbinds. New
      `keybinds::describe(&Bindings)` factors the sort+label
      rendering out of `describe_defaults` so `main.rs` passes
      `&cfg.keybinds` (post-apply, defaults + overrides + unbinds
      collapsed). +1 test.
- [x] **OSC 1 (icon name) rewrites to OSC 2 in the extractor.**
      VTE/alacritty silently drop OSC 1 entirely; their dispatch
      only matches `"0" | "2"`. But vim / tmux / ranger / mc
      emit OSC 1 to set their *short* (tab-intended) title — so
      those titles never appeared. kitty / iTerm2 / Gnome
      Terminal / Konsole all alias OSC 1 to OSC 2. The
      extractor now rewrites the leading payload byte from `1`
      to `2` so VTE picks it up downstream; BEL and ST
      terminators both handled. +1 test.
- [x] **OSC 104 (no-param) + OSC 110/111/112 reset
      conformance pins.** Set-side conformance was tested
      across cycles 47/56/65/66; the matching reset-side path
      (OSC 110/111/112 = reset default fg/bg/cursor; OSC 104
      with no params = reset *all* 256 palette indices) was
      exercised via alacritty/vte but not pinned in kettle, so
      a future upstream regression could silently break it.
      +2 tests cover both branches.
- [x] **`docs/kettle.example.config` covers every key (was 9 of
      ~35).** Major onboarding gap — copying the example never
      surfaced `font-feature` / `tab-bar` / `scrollbar` /
      `osc52` / `ssh-host` / `palette = N=#hex` / the unbind
      sentinels / etc. Now grouped by section with the valid
      value ranges and enum variants per key, plus a header
      callout that `#` is full-line-only (no inline trailing
      comments). New drift test (cycle-100 contract) strips
      comments and runs the activated keys through both
      `parse_collect` and `detect_malformed_values`; any new
      key forgotten or any example typo fails the test.
- [x] **`Config::load_from` warns on malformed values.**
      `load_from` previously `log::warn!`-ed unknown keys but
      silently dropped bad values. A user hitting reload after a
      typo got no signal. New
      `load_from_with_diagnostics(path) -> (Config, Vec<String>,
      Vec<String>)` returns both lists; `load_from` wraps it with
      `log::warn!` for each, and `--check-config` shares the same
      helper so the two paths can't drift. +1 test.
- [x] **`Action::ReloadConfig` applies `font-family` changes.**
      The reload handler updated `font-size` via the renderer
      setter but left the cached `font_family` field stale —
      glyphs kept rendering in the *old* family until restart.
      Same "reload swaps `self.cfg` but downstream caches are
      stale" shape as the cycle-44+ cluster. New `Renderer::
      set_font_family` setter + factored `remeasure_cell` so the
      family and size setters share one re-measure path.
      Idempotent guard keeps steady-state reloads free. Covered
      by `--screenshot` smoke; unit-testable without wgpu isn't
      feasible.
- [x] **`keybind = TRIGGER=unbind` removes a default.** The
      `apply_keybind` parser only ever inserted; users had no way
      to remove kettle's default Copy on `Ctrl+Shift+C` for shells
      that want the chord. Action half now accepts the unbind
      sentinels (`unbind` / `none` / `null` / `false` / empty),
      via pure helper `is_unbind_token(s)` so `--check-config`
      treats them as valid rather than malformed. Matches
      Ghostty's `unbind`. +2 tests.
- [x] **`--check-config` flags config lines missing `=`.** The
      line-oriented tokenizer silently `continue`s on any non-comment,
      non-empty line lacking `=`, so `font-family Jetbrains Mono`
      (forgot the equals) or a left-over `[section]` header dropped
      with no warning. `detect_malformed_values` now scans the raw
      text and emits `missing \`=\` separator: "<line>"` for each
      offender. Same shape as the cycle-70/84/85/86/87/88 silent-
      fallback cascade, caught before parsing rather than after.
      +1 test.
- [x] **`kettle -e PROG` seeds the tab title from PROG basename.**
      Cycle 93 fixed SSH; cycles 89/90 backfilled cwd-basename
      for shells. The gap that remained: any *other* explicit
      `-e` program (`htop`, `vim`, `tmux`, `python3 script.py`)
      stayed at "kettle" forever because most TUIs never emit
      OSC 2 and inherit the launching cwd (so cwd-basename gives
      you your repo name, not the program). `initial_pane_title`
      now extracts `Path::file_name(argv[0])` and uses it as the
      seed, with a hand-curated shell allow-list (POSIX shells,
      Windows shells, nu/elvish/xonsh) that still routes through
      the "kettle" placeholder so the cwd-basename fallback runs
      for shells — where the directory name is genuinely more
      useful than the literal "bash". +5 assertions.
- [x] **`scroll-on-keystroke` + `scroll-on-output`** (Alacritty/
      xterm parity). Keystroke default `true` (current behavior, now
      opt-out); output default `false` so background chatter doesn't
      tear you away from the page you're reading. Output detection
      via pure `kettle_core::scrollbar::should_scroll_on_output`
      (per-pane history-size diff; first frame is a no-op). +2 tests.
      Also added an OSC 4 set / OSC 104 reset round-trip conformance
      test pairing with last cycle's OSC 4/10/11/12 query path.
- [x] **Focused-pane border tints yellow on broadcast.**
      Cycle-178 follow-up: the tab-bar accent works only when
      the tab bar is visible. `tab-bar = auto` + single tab
      (default single-window) hides the bar — broadcast had no
      visual cue. Focused-pane border now flips from
      palette[4] (blue) to palette[3] (yellow) when broadcast
      is on, regardless of tab-bar mode.
- [x] **`clear_history` action — clear scrollback without
      resetting the terminal.** Writes `CSI 3 J`. Aliases
      `clear_scrollback` / `clear_buffer`. Honors broadcast.
      Surfaced in the command palette. Unbound by default
      (Ctrl+Shift+L would collide with the shell's form-feed).
      Matches kitty / iTerm2 / WezTerm convention.
- [x] **Drag-and-drop routes through bracketed paste.**
      Cycle-175 follow-up. Vim/fzf/mc with bracketed paste
      enabled used to interpret each char of the dropped path
      as a normal-mode command. Now wrapped in
      `\e[200~ … \e[201~` per-pane, matching the clipboard
      paste handler's behavior.
- [x] **`Config::default_path` treats empty env vars as unset.**
      Cycle-180 sibling for the config-path probe.
      `XDG_CONFIG_HOME=""` → relative `kettle/config` reading
      a stray config in CWD. Filter empty values; refactored
      to `default_path_from(lookup)` for unit testability. +1
      test.
- [x] **`home_dir_fallback` treats empty env vars as unset.**
      Cycle-162 follow-up. `HOME=""` (stripped CI container,
      misconfigured `unset HOME` parent) returned an empty
      PathBuf that flowed through to `cmd.cwd("")` — invalid
      empty path to the OS spawn. Filter empty values; probe
      continues to USERPROFILE / APPDATA. +1 test.
- [x] **Visual indicator when broadcast mode is on.** Active tab's
      left-edge accent flips to theme yellow (palette[3]) when
      broadcast is enabled — closes the loop on cycles 173/174.
      Inactive tabs stay normal (broadcast is per-active-tab,
      cycle-112 invariant). No new config key.
- [x] **Session restore canonicalizes theme name same as parse.**
      Cycle-176 sibling. Restore path used to re-store whatever
      lowercase/typo'd name the session file held; now routes
      through `Theme::find_name` so the invariant holds end-to-end.
- [x] **`--check-config` prints the actual theme name in use.**
      Pre-fix, a typo'd `theme = TokyoNitght Night` had
      `cfg.theme_name` store the typo verbatim while `cfg.theme`
      silently fell back to the default — diagnostic disagreed
      with runtime. Same shape as cycle 139 (font-size clamp).
      New `Theme::find_name` returns canonical casing on match,
      caller leaves `theme_name` at the default on miss. Bonus:
      `theme = tokyonight night` (lowercase) now normalizes to
      canonical "TokyoNight Night" in --check-config. +1 test.
- [x] **Drag-and-drop files insert shell-quoted paths.**
      Standard modern-terminal affordance — drop a file, the
      shell-quoted path lands at the cursor with a trailing space.
      Honors broadcast. POSIX-style single-quote escaping
      (close-escape-reopen) so the same output works on bash /
      zsh / fish / PowerShell 7+. +1 test.
- [x] **Paste distributes to every pane in a broadcast group.**
      Cycle 173 sibling. Paste IS input — same scoping as
      `broadcast_write`. Per-pane `BRACKETED_PASTE` wrap so
      panes with different modes (vim vs shell) each get the
      right byte sequence. Chrome-only.
- [x] **`scroll-on-keystroke` applies to broadcast groups too.**
      With broadcast on (Ctrl+Shift+G), typing wrote to every
      pane but skipped the scroll-to-bottom snap, so any
      scrolled-back pane stayed pinned to history while the
      bytes invisibly reached the remote shell. New
      `Mux::broadcast_scroll_to_bottom` matches `broadcast_write`'s
      scope (cycle-112 leaf_ids). No new test — same chrome-only
      pattern as cycle 151.
- [x] **`Trigger::label` uses Plus/Minus/Equal for the punctuation
      keys.** `Ctrl++` (zoom in) showed as `Ctrl++` — ambiguous on
      first read. Parser already accepts `plus` / `minus` /
      `equal` as named tokens; the label now mirrors that
      convention so `--list-keybinds` rows can be copied back
      into a config without translation. kitty + Ghostty render
      these the same way. +1 test.
- [x] **`font-feature = LIGA on` (uppercase) now actually
      toggles ligatures.** `FontFeature::parse` preserved the
      user's case, but OpenType tags are case-sensitive (lowercase
      per spec). The uppercase tag failed `is_ligature()` (so the
      coarse `font_ligatures` flag stayed stale) AND was silently
      ignored by the cosmic-text shaper. Lowercase the tag at
      parse time. +1 test.
- [x] **`kettle --help` no longer leaks internal `cycle N` refs.**
      `--list-keybinds` and `--config` doc comments shipped audit-
      trail parentheticals in their `--help` output; rewrote in
      plain English, dropped the cycle refs. `--config` description
      now also covers the cycle-164 directory-rejection behavior
      (which had been a silent change since cycle 164 landed).
      Regression test walks every clap Arg's help/long-help and
      the top-level about/long-about, asserting no "cycle " token
      leaks back in — same drift-guard shape as cycle 116's
      defaults_has_no_shadow_collisions. +1 test.
- [x] **Theme bundling resists `.DS_Store` / `Thumbs.db` / editor
      backup junk.** `build.rs` only skipped exact `LICENSE` /
      `README.md`. A macOS / Windows checkout (or a maintainer who
      edited a theme with Vim, leaving a `.swp`) would surface
      junk as a fake theme in `--list-themes`. New `theme_filter`
      module rejects dotfiles, OS desktop metadata, and editor
      backup suffixes; shared with `build.rs` via `include!` so
      the lib's tests cover the same code the build script runs.
      +1 test.
- [x] **Wikipedia-style URLs stay clickable.** Both `links.rs`
      (OSC 8 + autodetect overlay) and `hints.rs` (Ctrl+Shift+H
      quick-select) had private `trim_trailing` functions that
      stripped every trailing `)` / `]` / `}` along with the prose
      punctuation, breaking
      `https://en.wikipedia.org/wiki/Foo_(bar)` into a 404. Shared
      `kettle_core::url_trim::trim_trailing` now strips closing
      brackets only when *unbalanced* in the candidate substring,
      so `..._(bar)` keeps the `)` (1 open + 1 close = balanced)
      and `https://rust-lang.org)` from a `(https://rust-lang.org).`
      excerpt still loses it (0 opens + 1 close = unbalanced).
      Byte-level so multi-byte IRI chars are untouched. +5 tests.
- [x] **`--list-keybinds` columns line up for ALL rows.** `describe()`
      hard-coded the trigger column at 16 chars; `Ctrl+Shift+PageDown`
      (19) and `Ctrl+Shift+PageUp` (17) overflowed it. Now
      `width = max(16, longest)` — same shape as
      `format_ssh_hosts` (cycle 105). +1 test pinning the
      alignment contract.
- [x] **`--config DIR` is now a hard error.** Cycle 106 caught
      `--config /nonexistent` but `--config ~/.config/kettle`
      (where the user dropped the trailing `/config` filename)
      passed the existence check and silently fell back to defaults
      because `read_to_string` returned IsADirectory and the
      diagnostics path swallowed it as a `warn` log. Now the CLI
      hard-fails before any downstream code runs — same shape as
      cycle 107's `--working-directory`. Extracted
      `config_path_problem` as a pure helper; tested against the
      missing/dir/regular-file truth table. +1 test.
- [x] **Keybind modifier parsing: Super-key aliases + strict
      rejection of typos.** `parse_trigger` only knew
      `super`/`cmd`/`command`; users copying `win+t` /
      `meta+x` / `logo+t` from another config silently saw the
      unknown modifier degrade into a plain-key binding
      (`t`/`x`/`t`) — every press of that key triggered the
      bound action. Same shape for any typo'd modifier
      (`cttrl+t`, `supre+t`). Fix: add `win`/`windows`/`meta`/
      `logo` to the Super alias set and reject any non-modifier
      in non-final position so `--check-config` surfaces the
      bad line (it already gates via `parse_trigger.is_some()`).
      +1 test pinning all seven Super aliases + multi-modifier
      chord + three typo rejections.
- [x] **Stale-cwd fallback works on Windows.** Recorded session
      cwd no longer on disk → fall back to OS home. The previous
      `var_os("HOME")`-only probe missed Windows (`HOME` unset by
      default there), so Windows users silently ended up in
      kettle's launch directory. `home_dir_fallback(lookup)`
      probes `HOME` → `USERPROFILE` → `APPDATA`; `lookup` is a
      closure so the order is unit-testable without mutating env.
      Same shape as cycle 159's macOS universal2 fix — Linux+macOS
      worked, Windows didn't. +1 test.
- [x] **OS cursor matches every other GUI: arrow over chrome, I-beam
      only over text.** `sync_cursor_icon` showed the text I-beam
      everywhere the URL-hover Pointer didn't override it, including
      the tab bar and modal overlays (search bar, command palette,
      hint mode, SSH launcher) — none of which accept text
      selection. iTerm2 / WezTerm / Ghostty / kitty all switch to
      the standard arrow over chrome. The fix extracts a pure
      `chrome_cursor_icon(in_tab_bar, modal_open) -> Option<CursorIcon>`
      decision plus an `App::any_modal_open()` reader; the existing
      Pointer/Text branch runs only when chrome doesn't override.
      Test: all four (in_tab_bar × modal_open) states pinned. +1 test.

- [x] **`Ctrl+Shift+W` on a split closes the pane, not the whole tab.**
      `Mux::close_focused` used to `match Err(_)` and treat every error
      variant the same — `Err(None)` (only-leaf, close-tab) was
      conflated with `Err(Some(sibling))` (promote sibling, keep tab).
      Split the arm; merge promote-sibling with the regular `Ok(n)`
      path (both have identical post-conditions). New drift guard
      `close_focused_promotes_sibling_in_two_pane_split`. v1.3.0.
- [x] **Tab `✕` hover affordance** — red chip background +
      `CursorIcon::Pointer` on hover. Click handler was already
      correct (cycle 134 hit-tests `seg.close`); the bug was purely
      visual. Two pure helpers (`hovered_close_button`,
      `tab_close_hover_icon`) keep the geometry + cursor decision
      unit-testable. v1.3.0.
- [x] **Right-click opens a context menu** (Terminator / GNOME /
      iTerm2 parity). Replaces the cycle-49 silent no-op with a
      floating panel (Copy / Paste / sep / Split Right / Split Down /
      Close Pane / sep / New Tab). Reuses the cycle-111 modal-overlay
      infrastructure. Keyboard nav `↑↓ Tab` / `Enter Space` / `Esc`;
      mouse click on row dispatches; click outside dismisses. Pure
      `clamp_context_menu_anchor` so right-click near the bottom-
      right corner flips the panel up-and-left. Shift+right-click
      preserves the cycle-49 extend-selection muscle memory. v1.3.0.
- [x] **Tab-bar activity / bell dots** (Terminator's Activity / Urgent
      Watcher). Per-`Tab` `last_output_at` / `last_seen_at` / `bell`
      fields + pure `classify_tab_activity` decision (active tab
      short-circuits to Normal — focused accent is enough). The
      cycle-165 per-pane history detector also latches the tab's
      output state; `TermEvent::Bell` latches the bell flag. Renderer
      paints a 6-px square in the lower-left of inactive segments:
      palette[3] yellow for Bell, palette[6] cyan for Output. v1.3.0.
- [x] **Undo-close-tab** (WezTerm / browser convention).
      `Mux::closed_tabs: VecDeque<ClosedTab>` bounded LIFO ring of 10.
      `close_tab_at` snapshots the first leaf's argv + OSC-7 cwd
      before drop. New `Pane::argv` field — load-bearing so an SSH
      tab undoes back to the same SSH connection, an `-e PROG` tab
      undoes back to the same program. `Action::UndoCloseTab` (aliases
      `reopen_tab` / `restore_tab`); palette entry "Undo close tab";
      no default keybind (kettle's Terminator-inherited
      `Ctrl+Shift+T = NewTab` muscle memory takes priority). v1.3.0.
- [x] **Duplicate tab + duplicate pane** (iTerm2 parity). New
      `Action::DuplicateTab` / `Action::DuplicatePane` read the
      focused pane's argv (via the v1.3.0 `Pane::argv` field) + OSC-7
      cwd and clone into a new tab / horizontal split. Empty argv
      falls back to the configured shell. Palette entries; no default
      keybinds. v1.3.0.
- [x] **Mouse-drag tab reorder** (kitty / iTerm2 / Ghostty parity).
      Pure `tab_drag_target_index(cursor_x, n, strip_w) -> usize`
      helper + a tiny `tab_drag_active: bool` flag on App. Press in a
      tab segment (not ✕, not +) arms the drag; `CursorMoved` events
      compute the target index and call `Mux::move_active_tab`;
      release disarms. No ghost-render of the dragged segment — kept
      out of scope; bar snaps to the new order at each boundary
      crossing. v1.3.0.

- [x] **Coordinated-disclosure policy + supply-chain automation.**
      `SECURITY.md` points security reports at GitHub's private
      vulnerability-reporting form and enumerates in-scope classes
      (PTY-to-host escape, OSC 52 read-leak, URI scheme abuse past
      `links::is_safe_url`, bracketed-paste injection, resource
      exhaustion past the cycle-47/118 caps). Dependabot weekly Cargo
      + Actions update PRs (patch/minor grouped per ecosystem); new
      `audit.yml` runs `rustsec/audit-check` on every Cargo.lock
      change + daily 06:00 UTC cron with the upstream-transitive
      `paste` advisory (RUSTSEC-2024-0436) on the ignore list. v1.2.1.
- [x] **GitHub issue + PR templates aligned with the cycle pattern.**
      `.github/ISSUE_TEMPLATE/{config,bug_report,feature_request}.yml`
      + `.github/PULL_REQUEST_TEMPLATE.md`. `config.yml` disables blank
      issues, routes security at SECURITY.md, routes Q&A at
      Discussions. v1.2.1.
- [x] **`--config` / `--working-directory` hard-fail smoke (all OSes).**
      Cycle 106/107 had unit tests but no CI exit-code coverage —
      a regression that silently fell back to defaults would have
      passed the unit tests and reached users. CLI smoke now asserts
      both typo'd flags exit nonzero plus the happy-path round-trip
      `--config /tmp/k.cfg --config-path`. Windows-parity smoke (basename
      match on `k\.cfg$` rather than full path) lands in the same
      cycle. v1.2.1.
- [x] **`--help` indented examples render verbatim.** The cycle 227
      / 229 / 237 doc-comments contain indented `  kettle … > …`
      example lines; without `verbatim_doc_comment` clap collapsed
      the leading spaces, flattening the examples into prose. New
      `cli_help_preserves_indented_code_examples` drift guard pins
      all three flags via clap's `CommandFactory`. Same cycle fixes
      the zsh placement (the doc wrote to `~/.config/kettle/_kettle`,
      not on `$fpath`; now `"${fpath[1]}/_kettle"`). v1.2.1.

- [x] **Packaging templates: Homebrew formula, AUR PKGBUILD, Nix
      flake.** Closes the macOS / Arch / NixOS install gaps the
      cycle-253 curl|sh installer intentionally doesn't address.
      Each template lives under `packaging/{homebrew,arch,nix}/`
      with a README walking the per-platform submission /
      maintenance loop. Per-release maintenance is one line + one
      sha256 bump (cycle-254 sidecars give the values). Same
      template-in-source pattern: the PKGBUILD / formula / flake
      pin exact SHA-256s tied to a release, so they bump in the
      same PR as Cargo.toml.
- [x] **Ghost-render of the dragged tab during reorder** (cycle 255).
      The cycle-249 drag-to-reorder snapped the live bar to the new
      order at each boundary crossing but gave no "you're picking
      this tab up" affordance — the dragged segment teleported between
      positions. Now a translucent overlay copy of the active segment
      (background at 0.85 + matching accent strip + soft drop shadow)
      floats under the cursor while `tab_drag_active`. Anchor clamped
      to the bar width via the same shape as the cycle-245 context-
      menu anchor clamp.
- [x] **Per-tab silence watcher** (Terminator parity, v1.3.3
      cycle 252). Inactive tab whose unseen output stopped arriving
      for ≥ `tab-silence-threshold-ms` (default 10 s, clamped
      `[1000, 600_000]`) transitions from the cyan `Output` dot to a
      dim chrome-gray `Silent` dot. Pure `classify_tab_activity`
      now takes `now: Instant` + `silence_threshold: Duration` so
      the wall clock flows in from the caller. Bell still wins over
      Silent (explicit-attention > absence-signal). Backward-clock
      saturation guard so a monotonic-clock skew between calls
      doesn't false-trigger Silent.

## v1.4.0 → v1.7.0 — parity sweep (cycles 288-303, shipped)

- [x] **Smart selection** (iTerm2 parity, cycle 288). Double-click
      expands to URL / file path / IPv4 / git SHA via the cycle-218
      hint regex set instead of the under-/over-shooting alacritty
      Semantic word.
- [x] **Triggers — regex match on PTY output fires window urgency**
      (iTerm2 parity, cycles 289+290). `trigger = REGEX` config key
      + 2 s throttle + window-focused gate + alternation-pattern-
      survives drift guard.
- [x] **Named-workspace session** `--layout NAME` (Terminator parity,
      cycle 291). `<config-dir>/layouts/<NAME>.json`. Path sanitized.
- [x] **Named-config split** `--profile NAME` (Terminator + iTerm2,
      cycle 292). `<config-dir>/profiles/<NAME>.config`. Composes
      with `--layout`.
- [x] **Peacock accent** `accent-color` + `--accent COLOR` (cycle
      293). One config knob cascades to tab strip, focused pane
      border, dragged-tab ghost. Multi-window setups visually
      distinguishable.
- [x] **Annotated screenshots** `--annotate TEXT` (iTerm2 caption
      variant, cycle 294). Translucent bottom strip + caption.
      Distinct from iTerm2's persistent in-terminal annotations
      (those would be a multi-cycle thread).
- [x] **Status bar** `status-bar = off | top | bottom` (iTerm2 /
      kitty parity, cycles 295+296). `HH:MM:SS UTC · theme name ·
      focused pane title`. CPU/MEM widgets via `sysinfo` are a
      follow-up.
- [x] **Vi-mode scrollback** (Alacritty parity, cycles 298-301).
      `Ctrl+Shift+Space` enters; h/j/k/l/0/$/g/G/H/M/L + arrows;
      `v` visual selection; `y` yank to clipboard; Esc exit.
      Magenta hollow block at vi cursor + selection-background
      highlight for the visual range.
- [x] **Remote-control IPC** (kitty `@` parity, cycle 302).
      `kettle --remote-send TEXT` writes to a notify-watched file;
      the running kettle's receiver dispatches `send-text TEXT` to
      its focused pane. File IPC over the cycle-151 notify
      watcher — cross-platform free; per-window socket addressing
      is a planned follow-up.
- [x] **Quake dropdown** `--toggle` (Yakuake / Tilda / Ghostty
      quick-terminal parity, cycle 303). Piggybacks on the cycle-
      302 remote-control IPC; receiver flips
      `window.set_visible()` + `focus_window`. Users bind their
      compositor / DE / OS global hotkey to `kettle --toggle` —
      no cross-platform global-hotkey code in kettle.

## v1.8.0 → v1.31.0 — Terminator-parity sweep (cycles 330-412, shipped)

The big sweep. 82 cycles + 24 releases brought ALL 4 Bucket-D Terminator
feature trees to effectively-complete state. See
`docs/TERMINATOR-AUDIT.md` for the per-sub-cycle audit + cumulative
deliverables table. Highlights:

- [x] **Lua scripting** (WezTerm + Terminator plugin parity, cycles 324-326,
      365-378). `mlua` embedded; `kettle` API surface with `send_text`,
      `exec_action`, `notify`, `set_theme`, `on(event, callback)` event
      hooks (Startup/Bell/TabAdd/TabClose/Output), `add_url_handler`,
      `add_menu_item`. `~/.config/kettle/init.lua` auto-loads.
      `lua-sandbox = safe|trusted` config knob nils unsafe stdlib APIs
      by default. ALL 13 docs/TERMINATOR-PLUGIN-DESIGN.md sub-cycles
      shipped.
- [x] **tmux `-CC` passthrough parser** (iTerm2 parity, cycles 327-328).
      Pure `TmuxControlParser` in `kettle_vt::tmux_cc` covering every
      `%begin/%end/%output/%window-*/%session-*/%layout-change/%client-
      detached/%exit` variant with 11 unit tests pinning edge cases
      (CRLF, partial lines, 64KB overflow recovery, `\nnn` octal decode).
      `docs/TMUX-CC-DESIGN.md` lays out the remaining 5 integration
      sub-cycles (pane state plumbing, window-tab synthesis, input
      routing, layout-change, detach cleanup).
- [x] **Detachable tabs cross-window drag** (Terminator parity, cycles
      397-410). All 11 sub-cycles from `docs/TERMINATOR-DETACHABLE-TABS-
      DESIGN.md` shipped: serialize_tab, extract/insert APIs, SCM_RIGHTS
      `fd_transport` module, drag-state FSM, winit cursor-leave/enter
      transitions, cancel path, Wayland keyboard-fallback
      `Action::MoveTabToNewWindow`, file-fallback (`--tab-handoff PATH`)
      + SCM_RIGHTS IPC path (`--tab-handoff-fd FD`) both end-to-end
      for the JSON payload. Live-PTY adoption (Terminal::from_raw_fd)
      is kettle-core internal opt, tracked separately.
- [x] **Per-pane titlebar** (Terminator parity, cycles 379-407). All 10
      sub-cycles from `docs/TERMINATOR-PANE-TITLEBAR-DESIGN.md`: bg quad
      + title text + 3 color variants (transmit/receive/inactive) +
      size text + icon_bell + click hit-test → EditPaneTitle anchor +
      title_at_bottom flip + cell layout-shift + named broadcast
      groups (`Action::EditPaneGroup`).
- [x] **Background image** (Terminator parity, cycles 380-396). Decoder
      foundation, wgpu texture upload, 4 UV modes (stretch_and_fill,
      tile, center, scale), align horiz/vert, darkness compose,
      transparent path, CPU-side Gaussian blur (3-pass separable box
      blur).
- [x] **Detachable-tabs design doc** (`docs/TERMINATOR-DETACHABLE-TABS-
      DESIGN.md`) + **mux-server design doc** (`docs/MUX-SERVER-DESIGN.md`,
      cycle 329) — architecture + sub-cycle roadmaps for the full
      Terminator-detachable-mode + WezTerm-style attach/detach.
- [x] 85 Terminator config keys parsed; ~65 fully behavior-wired
      (cycles 331-360). All accept both kebab-case + underscore form.
- [x] 20 new `Action::*` variants fully wired end-to-end (cycles 342,
      384, 407).

## v1.32.0 → v1.43.0 — post-sweep production polish (cycles 411-553, shipped)

One hundred thirty-one cycles + twelve releases hardening the
plugin contract, ergonomics, doc-accuracy, doc-durability,
build-time infrastructure (opt-in pre-commit hook + shellcheck
gate), crates.io metadata polish, Linux-install icon-cache
correctness, and packaging-template version lockstep enforcement
around the v1.8.0 → v1.31.0 sweep. See
`docs/TERMINATOR-AUDIT.md`'s post-sweep section for the full
breakdown.

- [x] **Plugin-contract bug fixes** — six silent event-bypass sites
      across `new_tab` and `close_tab` paths now fire the canonical
      `LuaEvent`. Remote-control IPC new-tab → `TabAdd` (cycle 423),
      three `close_tab` paths → `TabClose` (cycle 424: SCM_RIGHTS
      source, file-fallback source, ✕-click), two `new_tab` paths →
      `TabAdd` (cycle 425: NewWindow fallback, exit-action=restart
      respawn). Plugins listening for tab-spawn / tab-close now see
      every trigger source.
- [x] **exit-action = restart** fully end-to-end (cycles 418, 420).
      Closes the cycle-357 "not yet implemented" warn; respawn uses
      live grid (`self.grid_of(self.area())`) not hardcoded 80×24.
- [x] **Helper unification** (cycles 426-428, 433). All six
      LuaCommand consumers (5 event hooks + menu-item) route through
      one `drain_lua_hook_commands` helper; only App::new early
      init stays inline (locals before `self` exists). Adding a
      sixth event is one `fire_event` call.
- [x] **Docs as code** — ARCHITECTURE.md detachable-tabs + plugin +
      bg-image flows upgraded ASCII → mermaid (cycles 421-422);
      CONFIG.md gained a Terminator-parity-keys table covering ~30
      cycles-331-410 keys (cycle 415); INSTALL.md SHA-256 pin
      example bumped v1.3.4 → v1.35.0 (cycles 417, 429, 438);
      audit-doc + ROADMAP tails extended with post-sweep summary
      (cycles 431-432); `packaging/linux/kettle.1` man-page CLI
      flag-doc fill-in (cycle 436) so `man kettle` matches
      `kettle --help`.
- [x] **Drift guards** — cycle 413 pinned 9 load-bearing Terminator-
      parity config keys in `print_default_config_round_trip`;
      cycle 430 pinned the `kettle.notify` + `kettle.set_theme`
      queue/drain contract; cycle 435 pinned
      `kettle.add_menu_item` + `kettle.add_url_handler` contracts;
      cycle 446 pinned `kettle.config_path` return-type contract;
      cycles 471-472 added 3 drift guards on the
      `extra_check_config_lines` helper covering all 7 opt-in
      echo branches.
      Workspace tests 308 → 322.
- [x] **CI doc-warnings gate clean** (cycle 411) — `cargo doc
      -D warnings` passes on `kettle-render` and `kettle-vt` after
      fixing 3 intra-doc link + bare-URL warnings.
- [x] **Agent-first kettle** (v2.16.0) — three opt-in non-GUI entry
      points, control surface OFF by default: `kettle exec -- <argv…>`
      (headless one-shot under a real PTY; propagates the child exit
      code — 124 on `--timeout`, 125 on an internal error — with
      `--strip-ansi` / `--json` / `--record` output modes); the control
      server + `kettle ctl` (`agent-server = off|read-only|full`,
      local-IPC only — a Unix socket `0600` / Windows named pipe;
      `get_state` / `list_tabs` / `list_panes` / `read_screen` /
      `subscribe` read-only plus `send_text` / `run_command` in full,
      OSC-133-correlated exit codes); and `kettle mcp` (a stdio MCP server
      exposing `kettle_run` + list/read/send/run tools, with a
      `--self-test` CI guard). The UI-free **kettle-ctl** crate hosts the
      protocol / transport / discovery / client, and a pane driven by an
      agent shows the agent-attach titlebar badge (`agent-badge`). See
      [`docs/AGENT.md`](AGENT.md). This also ships the long-tracked
      headless / pipe "future feature" as `kettle exec`.

## Next (in priority order)

The Terminator-parity sweep effectively closes the major missing-
features list. What's left is genuinely-multi-week threads + polish.

- [ ] **Renderer visual-regression hardening after v2.25.1.** Treat any pane
      text disappearing/reappearing outside the cursor cell as a bug, not an
      intentional blink. Keep `text-renderer = legacy` only as a user rollback,
      not the product fix. Current coverage includes `just live-render-smoke`,
      which renders a prompt-like `➜  ~` row in a live grid-renderer window and
      asserts cursor blink only changes a cursor-sized pixel region. The
      CI-safe offscreen renderer guard now exercises zsh-style, POSIX,
      lambda/starship-style, git-status, and PowerShell-style prompt fixtures
      through the cell-locked grid pipeline and asserts all non-cursor prompt
      pixels survive a block-cursor blink. Remaining work: run the live smoke on
      Ubuntu plus the Windows 11 / Windows 11 WSL paths before changing renderer
      defaults again, and keep adding fixtures when a new prompt shape exposes a
      renderer edge case.
- [ ] **Tab click and underline-scroll diagnostics.** `just tabbar-click-smoke`
      now reproduces the multi-tab click state and asserts a plain click does
      not show the drag ghost/highlight before movement crosses the drag
      threshold. It also diffs tab-bar screenshots and requires all press-time
      pixel changes to stay inside the old/new active tab rectangles. `just
      underline-scroll-smoke` opens `git diff | delta`, drives repeated down/up
      scroll input, and saves PNG, `read_cells`, and `analysis.json` frames under
      `target/diagnostics`. The underline smoke now also parses each PNG and
      asserts rendered underline pixels are present on the same rows as the
      `read_cells` underlined sentinel text and absent from neighboring plain
      sentinel rows. Native Windows recipes now run the Python stdlib driver
      (`scripts/check-live-ui-smoke.py`); WSL uses the Unix shell scripts.
      Remaining work: run both diagnostics on native Windows 11 and Windows 11
      WSL hardware.
- [ ] **Interactive agent/TUI validation sweep.** The noninteractive smoke
      covers `kettle exec`, MCP self-test, Codex CLI, Claude Code CLI, clean
      Neovim, and configured Neovim/AstroNvim command paths. `just
      agent-tui-smoke` now adds a live grid-renderer window pass for a shell
      marker, a prompt-shaped `➜  ~` marker, optional Codex/Claude CLI version
      probes, tmux attach/send/capture when tmux is installed, and
      clean/configured Neovim marker buffers, with PNG, `read_screen`,
      `read_cells`, and `analysis.json` artifacts that fail on blank captures.
      The tmux 3.4 branch has passed on Ubuntu with a real `tmux.png` capture.
      `just
      interaction-smoke` now covers multiline text entry, scrollback wheel
      movement, tab-bar `+` tab creation, right-click context-menu geometry, and
      screenshots, and it pinned/fixed `read_screen` so default reads follow the
      visible scrolled viewport. The live interaction smoke now includes local
      selection drag, context-menu `Split Right` dispatch, split-window resize
      probes through `send_mouse` / `resize_window`, and an OSC 777 protocol
      notification probe observed through the subscribed `kettle ctl events`
      stream. Remaining work is deeper live-window validation: drive Codex CLI,
      Claude Code CLI, AstroNvim, full tmux workflows beyond attach/send, and
      deeper screenshot states inside Kettle with `text-renderer = grid`. Use
      `send_mouse`, `send_keys`, `ui_geometry`, `read_cells`, and `screenshot`
      so the pass is reproducible instead of a manual eyeball-only sweep, then
      compare captured frames for blank panes, overlapping UI, stale text, and unintended
      blinking.
- [ ] **Performance comparison pass.** Keep Kettle faster than Terminator and
      close to Ghostty for startup, scrollback ingestion, resize, sustained
      output, memory, and GPU/frame-time behavior. The Ubuntu same-machine
      startup/ASCII-flood rows in `docs/PERFORMANCE.md` are refreshed for
      current `main` and still pass the Terminator/Ghostty timing gate; the
      Linux gate now also records advisory max-RSS samples for the same flood
      lifecycle. Remaining work: include Windows 11 and Windows 11 WSL where
      possible, broaden the local peer suite to resize, scrollback, and
      GPU/frame-time probes, and prioritize row-level damage tracking,
      persistent GPU cell buffers, and memory reduction if grid-mode frame cost
      or RSS remains above target.
- [ ] **Cross-platform release validation gap.** CI now proves Linux, macOS,
      Windows, MSRV, nightly, aarch64 build, package templates, screenshot
      smokes, and CLI smokes, but the durable release gate still needs manual
      Windows 11 and Windows 11 WSL interactive passes on real desktops,
      including integrated-GPU default selection (`gpu-power-preference = auto`)
      and multi-shell launch behavior.
- [x] **Interactive keybind editor in the settings overlay** (cycle 766) —
      Keybinds category lists each action's current chord; Enter captures a new
      chord, binds it live, and appends a `keybind` line (via
      `kettle_config::append_keybind`, with `Trigger::label()` as the verified
      round-tripping serializer). Add semantics; unbinding a default is still a
      config-file edit.
- [ ] tmux `-CC` post-parser integration (sub-cycles 3-7 per
      `docs/TMUX-CC-DESIGN.md`): pane-state plumbing, window-tab
      synthesis, input routing, layout-change, detach cleanup.
- [ ] Detachable mux server (WezTerm parity) — a SEPARATE `kettle-muxd`
      binary that owns PTYs cross-process per `docs/MUX-SERVER-DESIGN.md`.
      Distinct from the cycles 397-410 detachable-tabs work (which is
      same-process source → fork → target). Multi-week. The
      protocol / transport / discovery / client **seam** the daemon
      depends on already shipped in the **kettle-ctl** crate (the
      discovery registry reserves a `kind` field — `"gui"` today,
      `"muxd"` later); what remains is the standalone daemon that
      re-hosts the server side and owns PTYs cross-process.
- [x] **stdin forwarding for `kettle exec` on Unix / WSL** — `kettle exec` now
      forwards pipe/file/socket stdin to the child PTY while leaving
      interactive terminal stdin and `/dev/null` alone. Covered by
      `crates/kettle/tests/exec.rs`.
- [ ] **Native Windows ConPTY stdin forwarding for `kettle exec`** — disabled
      until the input lifecycle avoids `STATUS_CONTROL_C_EXIT` for console
      children on Windows CI.
- [x] **Live-grid `screenshot` control method** for `kettle ctl` / the MCP
      surface. It queues the existing live renderer readback, works in
      `agent-server=read-only`, and returns the saved PNG path.
- [ ] Terminal::from_raw_fd in kettle-core for SCM_RIGHTS live-PTY
      adoption (sub-cycle 7 final piece of detachable tabs). Internal
      optimization that preserves running shells across cross-window
      drag.
- [ ] Persistent in-terminal annotations (iTerm2 parity, distinct
      from the cycle-294 screenshot caption) — scrollback-position
      metadata + sticky-note overlay + search-jump-to. Multi-cycle
      (~4).
- [ ] sysinfo CPU / MEM widgets for the cycle-295 status bar.
- [ ] Native macOS menu bar (needs macOS-hands-on dev).
- [ ] Code-signed / notarized macOS build; Windows MSI installer
      (needs Apple Developer cert / Windows code-signing cert).
- [ ] Source-build AUR companion (`kettle`, no `-bin` suffix) +
      Homebrew `--HEAD` formula (build from `main` rather than the
      latest tag).
- [ ] Submit to nixpkgs proper so `nix-env -iA nixpkgs.kettle`
      works without the flake-input dance.
- [ ] Broader `vttest` conformance sweep — per-test cycle.

## Quality bar each cycle

`cargo fmt` · `cargo clippy -D warnings` · `cargo build` · `cargo test` ·
end-to-end run · docs updated · commit.

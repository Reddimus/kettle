# Changelog

All notable changes to kettle. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/); the project moves in small,
durable, fully-tested cycles (lint · build · test · docs · commit · CI).

## [Unreleased]

### Security
- **`--check-config` now catches malformed `keybind = …` lines.**
  `apply_keybind` silently dropped on a bad trigger (typo in
  modifier or key name) or unknown action — a user with
  `keybind = ctrl+shift+nope=copy` or `keybind = ctrl+a=garbage_
  action` got zero feedback that their line never produced a
  binding. Same trap as the cycle-70 / cycle-84 setup. Extended
  `detect_malformed_values` to split each `keybind = ` value on
  `=` and route both halves through `parse_trigger` /
  `Action::from_name` (the same predicates the apply path uses),
  so what `--check-config` accepts is what actually binds. +1
  test (bad trigger + bad action + missing-separator counted;
  valid aliases like `split_horiz` and parametric `goto_tab:5`
  pass cleanly).
- **`--check-config` now catches malformed color values.** The
  cycle-70 `detect_malformed_values` side scan covered numeric/
  duration keys but skipped colors — `background = #not-a-color`
  or `cursor-color = whatever` silently kept the default while
  `--check-config` reported a clean status. Extended to also
  check `background`, `foreground`, `cursor-color`, `selection-
  bg/fg`, `search-bg/fg`, `split-divider-color`, `focused-split-
  color` (incl. alias `split-divider-color-focused`), and
  `palette = N=#hex` (validates both halves). Each goes through
  `Rgb::parse` — same path the apply arm uses — so what
  `--check-config` accepts is what actually applies. +1 test
  covering 6 bad + 7 good values (including X11 3-char hex
  shorthand and color names like `red` which are valid).
- **`focused-split-color` config key.** The inactive pane border
  color was already configurable via `split-divider-color`
  (introduced cycles ago); the *focused* pane's border was
  hard-wired to `theme.palette[4]`. Users with a theme whose
  accent blue blends into nearby content had no way to tune the
  "here am I" indicator without re-theming the whole palette.
  New `focused-split-color` (alias `split-divider-color-focused`)
  fills the gap; `None` keeps the theme-accent default. +1 test.
- **Session restore brings back the focused pane in each tab.**
  `STab` was only saving the split tree (`root`) — restore used
  `first_leaf()` to pick a focus, so every reopened tab landed on
  the leftmost pane regardless of which one the user had focused
  at save time. Now records `STab.focus: usize` as a DFS-order
  index of the focused leaf (pane ids are reallocated across
  restores, so the id itself isn't portable). `#[serde(default)]`
  means pre-cycle session files still load (defaults to `0` =
  first leaf, the previous behavior). +1 round-trip test, +1
  legacy-file test confirming back-compat.
- **All five underline style flags reach the renderer.** Cycle 79
  drew a single line for `Flags::UNDERLINE | UNDERCURL`. The
  engine actually tracks five style bits: UNDERLINE (`\e[4m`),
  DOUBLE_UNDERLINE (`\e[21m` / `\e[4:2m`), UNDERCURL (`\e[4:3m`,
  spell), DOTTED_UNDERLINE (`\e[4:4m`), DASHED_UNDERLINE
  (`\e[4:5m`). The render check now keys on `Flags::ALL_UNDERLINES`
  so every style draws *something*, and `DOUBLE_UNDERLINE` gets a
  second stacked line so the visually-distinct double-underline
  case looks different from plain. Wave/dotted/dashed visual
  styles still draw as a single line — a shader path is deferred,
  but the presence/absence cue is what matters most.
  +1 conformance test confirming each of the five SGR sequences
  reaches the correct engine flag.
- **SGR 58 per-cell underline color is now respected.** The
  cycle-79 underline render used the cell's `fg` for the line
  color — fine for plain `\e[4m` but wrong for neovim spell-check,
  git diff, and LSP diagnostics, which emit `\e[58;2;r;g;b m` to
  draw a *separate* (typically red) squiggle while keeping the
  text in its normal palette color. Renderer now reads
  `cell.underline_color()` and uses it for the underline quad,
  falling back to `fg` when unset. +1 conformance test pinning
  the engine contract: SGR 58 stores the spec, SGR 59 clears it,
  UNDERLINE flag survives.
- **SGR 4 underline + SGR 9 strikeout are rendered.** The engine
  tracked `Flags::UNDERLINE`, `Flags::UNDERCURL` (the `4:3` curly
  variant), and `Flags::STRIKEOUT` correctly — the conformance
  test `sgr_underline_dim_strike` pinned each bit reaching the
  cell since cycle ~14 — but the renderer never turned them into
  pixels. vim's `:set list`, neovim's spell-check, `diff` output,
  `git diff --color-words` deletions, man pages — none of these
  visual cues survived to the screen. New 1-px-tall quads at
  `cell_bottom - 2` for underline (and curly, drawn as a plain
  line for now — a real wave wants a shader tweak) and at
  `cell_mid` for strikeout, both using `fg` so the line color
  follows the text (or the dim / selection override above).
- **SGR 2 dim/faint is rendered.** The engine tracked
  `Flags::DIM` correctly (parsed by vte from `\e[2m`), and there's
  even a `sgr_underline_dim_strike` conformance test confirming the
  bit reaches the cell — but the renderer was ignoring it.
  Programs emitting dim text (fish prompt themers, `less` status
  lines, mc panel headers) rendered at full intensity. New pure
  `kettle_render::color::dim(fg, bg)` blends the fg halfway toward
  the cell bg (50 % intensity, the xterm/alacritty/iTerm2
  convention). Applied *before* the minimum-contrast lift so the
  lift can claw back legibility on themes where dim drops below
  WCAG. +1 helper test.
- **OS cursor turns into a pointing hand over Ctrl-clickable
  URLs.** Browser / iTerm2 / Ghostty convention: the mouse cursor
  morphs from text-I-beam to `CursorIcon::Pointer` while the user
  holds Ctrl (or Cmd, on macOS) and the pointer is on a
  hyperlink — same chord that actually opens the URL. Without
  this affordance, the underline-on-hover (already there) is the
  only hint that the link is clickable. Re-syncs on:
  - `CursorMoved` (position changed → hit-test may flip)
  - `ModifiersChanged` (Ctrl pressed/released → affordance flips
    without the mouse moving)
  - Per-frame in `redraw()` after `update_links()` so a URL
    scrolling out from under a held Ctrl (Ctrl+PageUp, scroll-
    on-output, etc.) doesn't leave the pointer-hand icon stuck
    on a now-empty cell.
  Deduped via `last_cursor_icon` so we don't issue a `set_cursor`
  syscall on every frame.
- **`selection-foreground` is now actually applied.** The config
  key was parsed, stored on `Theme.selection_foreground`, and then…
  ignored by the renderer — selected cells kept their normal text
  color. Dark text on a slightly darker selection background was
  often unreadable. Fixed by capturing the
  `RenderableContent.selection` range at the top of `build_pane`
  and swapping `fg` to `theme.selection_foreground` for cells
  whose point is in the range — applied *after* INVERSE so the
  selection always wins for readability (cells with INVERSE under
  a selection would otherwise render as inverse-fg on selection-bg,
  often invisible).
- **Local paste capped at 4 MiB.** OSC 52 (remote-program write
  into the system clipboard) was capped at 1 MiB back in cycle 47;
  the reverse direction (`paste_clipboard` reads the user's
  clipboard into the PTY) was uncapped — a user with a 1 GB file
  on the clipboard could shove every byte into the PTY in one
  shot and freeze the terminal until the program at the other end
  drained the pipe. 4 MiB fits any realistic code-review / log-
  snippet paste; bigger pastes are almost certainly fat-finger.
  Reuses the existing `clamp_osc52` byte-clamper (UTF-8 char-
  boundary preserved).
- **Tab title truncation honors display columns, not chars.** The
  `truncate(s, n)` helper used `chars().count()` to decide whether
  to cut — but every CJK character or emoji is 2 cells wide in the
  rendered tab segment, so a title like `中文中文中文` (6 chars / 12
  cells) sailed past the segment width without being trimmed and
  overflowed visually. Now sums `UnicodeWidthChar::width()` of each
  char and reserves 1 column for the trailing `…`. Pure helper,
  +1 test covering ASCII / CJK / mixed / edge cases (limit=0,
  exact-fit).
- **`Ctrl+Plus` font-zoom muscle memory works on US layouts.** On
  a US keyboard the `+` glyph lives on `Shift+=` — pressing what a
  user thinks of as "Ctrl+Plus" actually sends `mods=Ctrl+Shift,
  key='+'` to winit. The existing `bind(Ctrl, Char('+'))` binding
  needed bare Ctrl and didn't match. The user got zero feedback;
  font size just stayed put unless they typed `Ctrl+=` instead.
  Fixed by adding the obvious Ctrl+Shift variants of the
  zoom-in / zoom-out chords:
  - Ctrl+Plus, Ctrl+= (already)
  - **Ctrl+Shift+Plus, Ctrl+Shift+= (new)**
  - Ctrl+- (already)
  - **Ctrl+Shift+-, Ctrl+Shift+_ (new — `_` is the shifted `-`)**
  +1 test covering the whole 7-variant matrix.
- **Shift bypasses mouse tracking** (xterm / Alacritty / kitty /
  Ghostty convention). When a TUI like htop, tmux, vim, or fzf
  enables mouse mode (`CSI ?1000h`/`?1002h`/etc.), every click and
  wheel notch was being forwarded to the program — kettle's
  selection, scrollback, and shift-click-extend were completely
  locked out. Now `Shift+click` does a local selection, `Shift+
  drag` extends it, and `Shift+wheel` scrolls kettle's scrollback
  even while the TUI thinks it owns the mouse. Implemented as a
  single early-return in `send_mouse` (so press/release/drag all
  bypass uniformly) plus a parallel guard in the wheel branch.
  Nothing changes when Shift isn't held — mouse tracking still
  works the way it always did.
- **`--check-config` surfaces malformed numeric values.** Every
  numeric/duration config arm was guarded with `if let Ok(v) =
  e.value.parse() { … }` — clean code, but it silently fell back
  to the default when the value didn't parse. A user writing
  `font-size = 14px` or `scrollback = lots` saw a clean
  `status: OK` from `--check-config` while their setting was being
  ignored. New `Config::detect_malformed_values(text)` runs a
  side scan after parse and lists the bad ones; the
  `--check-config` body merges them with the unknown-key list:
  ```
  status:  3 issue(s):
    - unknown key: invalid
    - unknown key: unknown-key
    - malformed value: font-size = "not_a_number"
  ```
  Catches font-size, padding-x/y, background-opacity,
  unfocused-split-opacity, scroll-multiplier, minimum-contrast,
  scrollback (special: accepts `infinite`/`unlimited`/integer),
  and cursor-blink-interval. +1 test covering each. Side scan
  keeps adding-new-validated-keys to one place instead of every
  parse arm.
- **`--screenshot --cols`/`--rows` clamp instead of crashing.**
  Passing a large value (`--cols 100000`) tried to allocate a
  texture exceeding wgpu's per-side limit (8192 px on most GPUs)
  and panicked with `Dimension X value … exceeds the limit of
  8192`. Now clamped to `[20, 400]` cols and `[8, 200]` rows —
  every realistic screenshot fits comfortably, and `--cols 100000`
  produces a 400×200 PNG with a friendly `wrote PATH (400×200
  cells)` instead of a backtrace.
- **`kettle --list-themes | head` no longer panics on broken
  pipe.** Rust's runtime sets `SIGPIPE` to `SIG_IGN` at startup;
  when the reader of a pipeline closes its end early, the next
  `println!` returns `EPIPE` from `write` and the macro panics
  with `failed printing to stdout`. Every shell pipelining
  `--list-themes` (522 lines) or `--list-keybinds` (47 lines) into
  `head`, `grep`, or `less -F` was hitting this panic — silent
  unless you saw stderr, and `rc=0` because `head` itself exits 0.
  Fixed by resetting `SIGPIPE` to `SIG_DFL` at the top of `main`
  (Unix only; Windows has no `SIGPIPE`), so the process exits
  cleanly on EPIPE the way every other CLI tool does. New
  `libc = "0.2"` Unix-only dep (tiny, in the regular ecosystem).
- **`Action::NewWindow` (Ctrl+Shift+I) opens an actual new OS
  window.** The handler was sharing an arm with `Action::NewTab` —
  the parsed keybind dispatched cleanly all the way to a new
  *tab* in the existing window, so users pressing the "new
  window" chord were silently getting a tab (same shape as the
  empty-arm bug fixed in cycle 55). Now spawns a separate kettle
  process via `std::env::current_exe()` + `Command::spawn`, with
  stdio nulled and the child handle dropped so the OS reaps it.
  Falls back to a new tab if the current executable isn't
  resolvable (snap / appimage with custom argv0), keeping the
  keybind useful on weird platforms instead of silently failing.
- **OSC 10 (set default foreground) now reaches the per-pane
  text-area default color.** Companion to the OSC 11 chrome fix
  in cycle 65: a program issuing `OSC 10 ; rgb:RR/GG/BB ST` was
  populating `Colors[256]` and `color::resolve` honored it per-cell
  for fg, but glyphon's per-`TextArea` `default_color` was hard-
  wired to `theme.foreground` — the fallback when a span lacks an
  explicit `Attrs::color` (whitespace / IME composition / chrome
  strings rendered through the buffer). Now per-pane: each
  pane's `TextArea` reads its own `term_colors[256]` override; tab
  bar text and other chrome below keep `theme.foreground`. Same
  precedence as the OSC 10 *query* path.
- **OSC 11 (set default background) now reaches the chrome.**
  The cycle-56 fix paired OSC 12 (cursor color) set with the render
  path; OSC 11 had the same gap but on a larger surface — the
  engine parsed it and populated `Colors[257]`, `color::resolve`
  honored the override for individual cells, but three other places
  hard-wired `theme.background`: the surface clear-color (window
  padding / pane gaps), the active tab-bar segment, and the
  per-cell "is this the default bg, skip the quad?" check. A
  program flipping the bg to red would paint the cells red and
  leave the padding theme-blue — the chrome wouldn't follow. Now
  computed once per `render_frame` from the focused pane's
  `term_colors[257]` and threaded through all three places. Same
  precedence as the OSC 11 *query* path (cycle 44).
- **`Alt+1..9` jumps to tab 1..9** (kitty / Terminator / iTerm2 /
  Ghostty parity). The `Action::GotoTab(u8)` handler has existed
  since the early cycles, but `Action::from_name` had no parser
  for `goto_tab:N` strings and no default keybind, so the action
  was orphaned — users could neither bind it via config nor trigger
  it at all. Now: defaults bind Alt+1..Alt+9 → GotoTab(0..8), and
  config strings `keybind = alt+5=goto_tab:5` work (1-based to
  match the user's mental model; refused on `0` to surface the
  ambiguity rather than silently aliasing first-tab). Alt+0 is
  kept free for users who want to bind "last tab" manually.
  +2 tests (defaults table + parser rules incl. zero-rejection).
- **`Ctrl+Backspace` now sends BS (0x08) for delete-word muscle
  memory.** xterm/alacritty/Ghostty all distinguish the chord:
  plain Backspace → DEL (0x7F, readline `backward-delete-char`),
  Alt+Backspace → ESC+DEL (readline `backward-kill-word` / M-DEL),
  Ctrl+Backspace → BS (0x08). Kettle was mapping Ctrl+Backspace to
  plain DEL — same as a bare Backspace — so users coming from VS
  Code / browsers couldn't get delete-word with their muscle
  memory even after telling bash `bind '"\C-h":backward-kill-word'`
  (the shell never saw the BS that triggers it). +1 test covering
  all three flavors + the Ctrl+Alt combo.
- **OSC 4 multi-index query conformance is now pinned.** The
  cycle-44 fix shipped single-index replies (`OSC 4 ; 1 ; ?`); the
  vte parser already chunks the params so multi-index queries
  (`OSC 4 ; 1 ; ? ; 7 ; ?` — sent by tmux, neovim 0.10+, base16-
  shell-hook to probe an entire palette in one go) emit one
  `ColorRequest` per pair. Added an end-to-end test that asserts
  both indices come through; without per-pair dispatch the batched
  probers would see only the first reply and assume the rest of
  the palette equals the engine default, breaking dark/light
  detection they all rely on.
- **Full xterm Ctrl+<punctuation> C0 row.** Letter mappings
  (Ctrl+A → 0x01, …, Ctrl+Z → 0x1A) were already in place, plus
  `[` `\\` `]` ` `. Missing: `@` (NUL — same as Ctrl+Space), `^`
  (RS 0x1E — vim's alt-buffer toggle and tmux's `Ctrl-^` prefix),
  `_` (US 0x1F), and `/` (US 0x1F — tmux/nano undo). Each was
  previously falling through to "insert the literal character,"
  which silently broke those editor shortcuts. +1 test exercising
  the whole table.
- **`TERM_PROGRAM_VERSION` env var set on every spawned shell.**
  Companion to the existing `TERM_PROGRAM=kettle`; iTerm2 / kitty /
  WezTerm / Ghostty all set this pair. Neovim's
  `:checkhealth provider`, fish's prompt themers, and various
  shell/script diagnostics key off the pair when probing for known
  modern terminals — without the version, kettle showed up as "an
  unknown program calling itself kettle" rather than "kettle
  v0.1.0." Populated from `env!("CARGO_PKG_VERSION")` so a bumped
  `Cargo.toml` flows through with no separate string to maintain.
- **`--check-config` now echoes every per-cycle config gate.** The
  command was added back at cycle 5-ish and still only reported
  five fields (config path, theme, font, scrollback, keybind
  count). Since then we've added ~15 user-facing toggles — bell,
  OSC 52 policy, minimum-contrast, scroll-on-keystroke, scroll-on-
  output, scroll-multiplier, mouse-hide-while-typing, copy-on-
  select, tab-bar mode/position/format, window-title-format,
  word-delimiters, ssh-host count, cursor style/blink/interval —
  and none of them surfaced. A user setting `mouse-hide = false`
  had no way to verify it was actually applied without reading the
  source. `--check-config` now groups them by theme (cursor / bell+
  osc52+contrast / scroll / mouse / tabs / title / words / ssh) so
  the output stays scannable; `word-delimiters` and `ssh` lines
  only render when non-empty.
- **Bracketed paste also strips the *opening* marker `\x1b[200~`.**
  The injection-guard added earlier (and tested in
  `paste_strips_injected_end_marker`) only filtered the closing
  marker `\x1b[201~` — the well-known attack vector that ends paste
  mode early and lets the shell auto-execute the remainder. But the
  opening marker is the same class of bug on the other side: a
  paste containing `\x1b[200~` can confuse some shells into thinking
  they're entering paste mode mid-way, so our genuine `\x1b[201~` at
  the wrapper's end doesn't actually exit it — subsequent typed
  input is then absorbed as paste content. Alacritty / iTerm2 /
  WezTerm all strip both. +1 test (`paste_strips_injected_start_
  marker`) pairs the contract symmetrically with the close-marker
  test.
- **OSC 7 cwd percent-decoding handles UTF-8 paths correctly.**
  Shells (zsh `print -P %d`, bash `printf '\\e]7;…'`) percent-encode
  each *UTF-8 byte* of a non-ASCII filename individually — `café`
  arrives as `caf%C3%A9`. The old parser pushed each decoded byte
  as a `char`, which produced the Latin-1 garbage `cafÃ©` and broke
  new-tab/split cwd inheritance, the window title's `{cwd}`
  placeholder, and the OSC 7 session-restore path for every user
  with a non-ASCII directory in their tree. Fixed by decoding into
  a `Vec<u8>` and converting via `from_utf8_lossy`. +1 conformance
  test covering non-ASCII alone and mixed (`%20` space + `%C3%A9` +
  ASCII).
- **OSC 12 (set cursor color) now actually paints the cursor.**
  Companion bug to the OSC 4/10/11/12 *query* path shipped two
  weeks ago: the engine already parsed `OSC 12 ; rgb:RR/GG/BB ST`
  and populated `Colors[258]`, but the renderer hard-wired the
  cursor quad to `theme.cursor` so the override never reached the
  screen. Drawing now resolves via `kettle_render::color
  ::resolve_query(258, theme, term_colors)` — runtime override
  wins, theme value is the fallback. The same precedence rule the
  *query* path returns, so OSC 12 set + OSC 12 ? now agree.
  Confirmed end-to-end via a new test asserting OSC 10/11/12 SET
  populate engine slots 256 / 257 / 258 with the exact xparsecolor
  values.
- **`move_tab_left` / `move_tab_right` actions now actually move
  the tab.** They were bound to `Ctrl+Shift+PageUp` / `PageDown` in
  the default keymap (Terminator parity), parsed correctly, and
  threaded all the way to `Action::MoveTabLeft|MoveTabRight` in the
  app — and then dispatched into an empty arm. Every press was a
  silent no-op. Wired by a new `Mux::move_active_tab(delta: i32) ->
  bool` that swaps the active tab with its neighbor and clamps at
  the edges (no wrap, matching iTerm2 / Ghostty / WezTerm; wrap
  would have the tab bar lurch across on every press). +1 test
  covering swap, clamp, no-op, and the < 2 tabs case.
- **Selection auto-scrolls when you drag past the pane edge.**
  Previously the highlight stopped at the visible boundary — you
  had to release, scroll, then shift-click to extend. Every modern
  terminal (Alacritty / iTerm2 / WezTerm / kitty / Ghostty) keeps
  the scroll going while the mouse holds past the edge so a
  long-distance "select these 500 lines" gesture is a single
  click-and-drag. Per-frame rate scales with overshoot (1 line/
  frame at the edge, 2 at 10 px past, 3 at 40+ px) via a pure
  `selection_autoscroll_lines(y, top, bottom)` helper. The event
  loop wake-up cadence (`about_to_wait`) now keeps a 30 fps tick
  alive while drag-autoscroll is active, so the user doesn't have
  to wiggle the mouse to keep it moving. +1 test covering all six
  zones (inside, just-past, moderate, big × top/bottom).
- **`word-delimiters` config** (Alacritty `selection.
  semantic_escape_chars` parity, aliases `selection-word-chars` and
  `semantic-escape-chars`). Customizes what counts as a word for
  double-click selection (and the jump-to-prompt search that uses
  the same boundary set). Defaults to empty meaning "use the engine
  default" — `,│\`|:\"' ()[]{}<>\t`. Override to e.g. `()[]{}` to
  drop `/` and `:` from the delimiter set so a double-click on a URL
  or path picks it up whole. Plumbed through a new
  `Terminal::new(word_delimiters: Option<&str>)` arg →
  `TermConfig::semantic_escape_chars`. +1 config-parse test
  covering the canonical key and both aliases.
- **Shift+Click / right-click extend the selection** (xterm /
  Alacritty / iTerm2 / WezTerm convention). Previously every left
  click started a fresh selection at the click point, so the only
  way to grow a selection across a long scrollback was to start
  the drag over and hold all the way through. Now:
  - **Shift+left-click** anchors the current selection's start and
    pulls the end to the click — and you can keep dragging from
    there. Shift+Alt-Click still does block-select (Alt takes
    precedence). Shift+Click on empty space falls through to a
    normal new-selection.
  - **Right-click** extends an existing selection to the click;
    bare right-click on empty space is still a no-op so a stray
    right-click doesn't conjure a selection.
  Shared via a new `extend_selection_to_cursor` helper that updates
  the engine selection's right edge and enters drag mode for live
  follow-up. Copy-on-select fires on right-click extend too.
- **Wheel over tab bar cycles tabs** (kitty / iTerm2 / Ghostty
  parity). Spinning the mouse wheel while the pointer is over the
  tab bar now switches tabs (wheel-up = previous, wheel-down =
  next) instead of scrolling the focused pane's scrollback — the
  same gesture every modern terminal binds. Honors
  `tab-bar-position = bottom` and the hidden-bar case (`tab-bar =
  off` or `auto` with one tab). Pure `cursor_in_tab_bar_band`
  geometry helper, +1 unit test covering top/bottom/hidden bands.
- **`mouse-hide-while-typing` + selection clears on typing.** Two
  QoL gaps every modern terminal (Alacritty, kitty, WezTerm,
  iTerm2, Ghostty) has but kettle didn't:
  - The OS mouse cursor now hides on keyboard input (configurable,
    default on; aliases `mouse-hide`) and reappears on the next
    mouse move — so the pointer doesn't sit over the column you're
    editing.
  - The focused pane's selection is cleared on any keystroke that
    produces PTY bytes — so typing after a select doesn't leave a
    stale highlight behind to confuse the next copy/paste.
  Wired via small `hide_mouse_cursor`/`show_mouse_cursor`/
  `clear_selection_on_input` helpers on App. +1 config test.
- **Modified named keys now encode per xterm modifyCursorKeys** —
  `Ctrl+Right` (skip-word in bash/zsh/readline), `Ctrl+Delete`
  (delete-word), `Shift+Tab` (`CSI Z` back-tab used by readline /
  fzf / TUI forms), and modified arrows / F-keys / Insert / Delete /
  PageUp / PageDown / Home / End all previously collapsed to their
  unmodified sequence — vim users couldn't word-skip, fzf couldn't
  reverse-tab through fields. New pure `xterm_modifier(mods) → u32`
  emits the standard table (1 + shift + 2·alt + 4·ctrl + 8·super)
  and the encoder switches:
  - Arrows / Home / End → `CSI 1;<m>A..D|H|F` when modified
    (unmodified still honors DECCKM, modified always CSI).
  - Insert / Delete / PgUp / PgDn / F5..F12 → `CSI <n>;<m>~`.
  - F1..F4 → `CSI 1;<m>P..S` when modified (SS3 only when bare).
  - `Shift+Tab` → `CSI Z`.
  +2 tests covering the modifier table + every encoded family.
- **DECSCUSR cursor shape & DEC ?25 visibility now honor the
  running program.** Vim / neovim / fish flip cursor shape per-mode
  (`CSI 1 SP q` block in normal, `CSI 5 SP q` beam in insert,
  `CSI 3 SP q` underline in replace), and full-screen TUIs hide the
  cursor with `CSI ?25 l`. The renderer ignored both and always drew
  the static `cursor-style` config shape — so insert mode looked the
  same as normal mode, and the cursor stayed visible over `less`/
  `fzf`/`htop`. Fixed by seeding the engine's `default_cursor_style`
  from `cursor-style` at pane creation (so the user's static config
  is still the default) and reading the live
  `RenderableContent.cursor.shape` per frame — which the engine
  collapses `?25 l` into `CursorShape::Hidden` for, so a single
  guard handles both DECSCUSR and visibility. Added a new
  `HollowBlock` shape for when programs ask for an outline (vim's
  `:set guicursor` does this). +3 tests (config→engine mapping;
  engine ↔ ?25 hide/show round-trip; existing DECSCUSR shape test
  retained).
- **`scroll-on-keystroke` (alias `scroll-on-input`) + `scroll-on-
  output`** (Alacritty / xterm parity): two new bools governing
  scrollback behavior. `scroll-on-keystroke` defaults `true` (typing
  yanks you to the bottom — the longstanding behavior, now opt-out
  so pinning the viewport while typing is possible) and `scroll-on-
  output` defaults `false` (a chatty background process won't tear
  you away from the page you're reading; flip it on to chase the
  newest line). Output detection uses a pure
  `kettle_core::scrollbar::should_scroll_on_output` helper (history-
  size diff against the previous frame; first frame is a no-op) so
  the rule lives outside the render path. +1 config-parse test, +1
  pure-helper test.
- **OSC color set/reset round-trip conformance** — end-to-end test
  that `OSC 4 ; 1 ; rgb:…` writes into the engine's `Colors` slot and
  `OSC 104 ; 1` clears it. Guards the connection between OSC color
  set/reset (parser → engine) and the OSC 4/10/11/12 *query* reply
  path shipped last week — together they prove a full xparsecolor
  loop works.
- **DEC mode 12 (cursor blink) now honors the running program.**
  `CSI ?12 h` / `?12 l` is the standard way for vim, neovim, and
  shell prompts to ask the terminal for a solid (steady) or blinking
  cursor inside their UI. The engine raised
  `TermEvent::CursorBlinkingChange` and tracked the state on
  `cursor_style().blinking`, but the app's blink decision was hard-
  wired to the static `cursor-style-blink` config — every program
  request was silently ignored. Wired via a small
  `Terminal::cursor_blinking()` accessor (engine kept hidden), with
  the redraw + cursor-visibility path now intersecting config and
  live pane state. The event handler resets the blink phase so
  off→solid is immediate (no half-period delay). Default initial
  blink is seeded from `cursor-style-blink` at pane creation. +1
  conformance test.
- **`CSI 14 t` (text-area pixel size) now replies.** Sixel / kitty
  graphics / iTerm2 OSC 1337 apps probe this to compute
  pixel-perfect image placements (a 200-px image needs to know how
  many cells it covers); the engine raised
  `TextAreaSizeRequest(formatter)` but the app's event loop dropped
  it and the apps fell back to a 8×16 cell guess. New pure helper
  `kettle_render::reply_for_text_area_size(cols, rows, cell_w,
  cell_h, fmt)` feeds the engine formatter the live grid + cell
  dimensions and yields the canonical xtwinops reply
  `CSI 4 ; <height-px> ; <width-px> t`. +1 conformance test.
- **OSC 4 / 10 / 11 / 12 color queries now reply** (xparsecolor
  `rgb:RRRR/GGGG/BBBB` form). vim/neovim and tmux use these to detect
  the actual default foreground / background / cursor and the live
  palette so they pick a colorscheme that matches the terminal. The
  engine emitted `ColorRequest` events but the running app silently
  dropped them — now they're resolved against the active theme plus
  any runtime OSC overrides via a pure `kettle_render::reply_for_query`
  (palette 0..=15 → theme, 16..=255 → xterm cube, 256/257/258 →
  fg/bg/cursor; out-of-range → no reply). +2 tests (pure helper +
  end-to-end formatter shape for all four OSC prefixes).
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

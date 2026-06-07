# Testing

kettle is verified by a fast, deterministic test suite plus CI smoke runs on
all three OSes. No GPU or PTY is required for the unit suite, so it runs
everywhere including CI.

## Run it

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## What's covered (automated)

**500+ tests across the workspace** — see
[CHANGELOG.md](../CHANGELOG.md) for the per-cycle additions
(cycle-288 → 303 feature sweep, cycles 330-410 Terminator-parity
sweep, cycles 411-438 production-polish run, cycles 576-587 resource-
cap defense-in-depth sweep, etc.). The workspace grows by 1–3
tests per audit cycle, so per-crate counts below are
range-stable phrasings rather than exact figures — run
`cargo test --workspace` for today's number. Cycle-179's drift
guard scans user-facing docs for hardcoded "N workspace tests"
claims that go stale; TESTING.md is exempt from that scan
(contributor-leaning doc) but follows the same range-stable
discipline here.

- **kettle-vt** (50+ tests): plain-text passthrough is byte-exact;
  iTerm2 / Sixel / kitty (incl. zlib-less RGBA + chunked reassembly)
  decode to the right pixels; OSC 7 / OSC 133 are consumed and
  surrounding text still passes; OSC 1 → OSC 2 rewrite (cycle 102) so
  vim/tmux/ranger short-titles set the tab title; a sequence delivered
  one byte at a time still yields exactly one image; an ~8 MiB
  interleaved stream passes through intact in well under 5 s
  (linear-time / bounded-memory guard).

- **kettle-config** (90+ tests): TokyoNight Night is the verified
  default palette; Ghostty `key = value` overrides, repeats, `palette`
  (0..=15 + cycle-124 out-of-range diagnostic), `infinite` scrollback,
  `ssh-host`; the bundled theme set has >400 entries incl. "TokyoNight
  Night"; Terminator default keybinds and trigger parsing; the
  cycle-104 `from_name` ↔ `action_names` round-trip drift guard; the
  cycle-116 `defaults_has_no_shadow_collisions` audit (no
  HashMap-shadowed bindings); the cycle-117 palette-completeness drift
  guard (now also covering `OpenContextMenu` / `UndoCloseTab` /
  `DuplicateTab` / `DuplicatePane` from v1.3.0); the cycle-100
  example-config drift guard; the cycle-125 README-keybind regression
  guard; cycle-99/108/109 session load/save atomic + corruption-backup
  contracts; cycle-121/122 empty-value resets for every string-config
  key; cycle-118 `clamp_font_size` bounds.

- **kettle-core VT conformance** (80+ tests): drives the *real*
  vte + alacritty_terminal path used by the PTY reader and asserts
  grid/cursor/SGR/mode state across a broad `vttest`-style sweep —
  text + `\r\n` + CUP addressing, erase-line/erase-display, SGR
  truecolor + bold + reset + dim/underline (4:3) + strikeout +
  double-underline + curly + dashed + dotted (plus the cycle-243
  SGR individual attribute-off codes 22/23/24/27/29), tab stops +
  carriage return, alt-screen + bracketed-paste private modes,
  DECSTBM scroll region, DEC special-graphics line-drawing charset,
  ICH/DCH, IL/DL, DECSC/DECRC save-restore, DECAWM autowrap, DECOM
  origin mode, device responses via the real EventProxy PTY
  write-back (DSR 6n cursor-position, primary + secondary device
  attributes, DECRQM mode report, DECALN screen alignment, REP, G1
  via SO/SI, RIS, EL/ED/ECH, CHA/HPA/VPA, DECSC-restores-SGR, SU/SD,
  DECSCUSR cursor shape, NEL/IND/RI, DECID, cursor-blink mode ?12,
  CHT/CBT tab nav, DECSET 1049 alt-screen, DECSET 2026 sync output),
  OSC 4 palette query + 104 reset (cycle 101), OSC 10/11/12 default
  fg/bg/cursor set + 110/111/112 reset siblings (cycle 101), OSC 8
  hyperlink cell-carry, OSC 52 clipboard copy + paste policies,
  wide CJK (2 cells + spacer) + wide-char wrap, combining-mark
  zero-width.

- **kettle-render** (30+ unit tests + 1 visual integration test):
  truncate respects display columns (not chars), the
  `clamp_font_size` floor/ceiling/NaN/∞ contract (cycle 118), the
  `cap_axis_cells` GPU-texture safety guard (cycle 119), color
  resolve / dim / minimum-contrast WCAG math, the offscreen GPU
  pipeline self-test (real wgpu pipelines compile + render through
  Vulkan/Metal/DX12). The cycle-251 `tests/menu_visual.rs`
  integration test renders both `DebugScene::Default` and
  `DebugScene::ContextMenu` PNGs via `capture_png_with`, then
  asserts ≥ 1000 pixels differ between the two AND ≥ 200 fg-leaning
  pixels appear in the menu area — catches the v1.3.0/v1.3.1
  blank-menu render-pass-order regression class that bare logic
  tests can't see.

- **kettle-ui** (80+ tests): split-tree layout tiles with no
  gaps/overlap, `remove_leaf` collapses to the sibling, nested
  splits keep every leaf; `Node::leaf_ids` DFS-order +
  `nth_leaf`/`leaf_index_of` symmetry; `close_tab_at` and
  `close_window` (cycle 113) tab-reaping with active-index
  bookkeeping; cycle-120 `reap_tabs` keeps focus on the same tab
  after a pane death; cycle-240 `close_focused_promotes_sibling_in_two_pane_split`
  (the v1.3.0 fix for `Ctrl+Shift+W` closing whole tabs);
  cycle-251 `next_context_menu_highlight_skips_separators_and_disabled`
  + `clamp_context_menu_anchor_keeps_panel_on_screen`;
  cycle-246 `classify_tab_activity_picks_the_right_indicator`
  + cycle-252 `classify_tab_activity_transitions_to_silent_after_threshold`;
  cycle-247 `closed_tab_ring_bounded_and_lifo`;
  cycle-249 `tab_drag_target_index_clamps_to_strip`;
  cycle-241 `hovered_close_button_finds_only_the_close_rect_hits`
  + `tab_close_hover_icon_overrides_chrome_default`;
  selection-autoscroll ladder; cwd-basename tab-title fallback
  (cycle 89); the SSH and `-e PROG` initial-pane-title heuristics
  (cycle 93 / cycle 95); session JSON round-trips, atomic save
  + corruption-backup contracts; xterm modifier encoding + paste
  payload bracketing + injection-guard.

- **kettle** (binary, 15+ tests): clap argv parsing for the cycle-30
  `-e` + `-d` + `--config` combination; the cycle-105
  `format_ssh_hosts` table renderer (sort + column alignment +
  empty fallback); the cycle-219
  `cli_help_text_has_no_internal_cycle_refs` audit-trail leak
  guard; the cycle-241
  `cli_help_preserves_indented_code_examples` drift guard that
  pins `verbatim_doc_comment` on every flag with an indented
  example block (the bug the v1.2.1 patch landed against).

## Manual / interactive checks

These need a real display and are run by hand (or on real hardware):

- **VT conformance**: run [`vttest`](https://invisible-island.net/vttest/)
  and walk the cursor/erase/SGR/mode screens.
- **TUIs**: `nvim`/AstroNvim (icons, undercurl, truecolor, mouse), `tmux`,
  `htop`, `fzf`, `less`.
- **Images**: `img2sixel`/`chafa -f sixel`, `kitten icat`, iTerm2 `imgcat`.
- **Shell integration**: enable the snippet from
  [SHELL-INTEGRATION.md](SHELL-INTEGRATION.md), then `Ctrl+Up`/`Ctrl+Down`
  to jump between prompt marks.
- **Perf**: `cat` a ~100 MB file / fast `yes` stays responsive.

## Pattern: audit-driven cycles

kettle's test count grows mostly via "audit cycles" — each cycle finds a
silent-fallback bug, parity gap, or docs-drift on a specific surface,
extracts a pure helper if applicable, wires it in, and pins the contract
with a test. See [CHANGELOG.md](../CHANGELOG.md) for the per-cycle list;
the pattern is documented in `### Tests` and `### Fixed` entries that
name the shape of bug each cycle caught.

## CI

`.github/workflows/ci.yml` runs on **ubuntu/macos/windows**:

- `fmt --check`, `build --all-targets`, `clippy -D warnings`,
  `cargo test --workspace` on every OS.
- `cargo doc --no-deps` with `RUSTDOCFLAGS=-D warnings` (Linux only —
  catches broken intra-doc-links, malformed examples; rustdoc is
  platform-agnostic so one runner suffices).
- A **headless GPU smoke** under Xvfb + software Vulkan on Linux.
- The cycle-236 **`--screenshot` end-to-end** + cycle-251
  **`--screenshot-menu` visual regression** smokes on Linux
  (both run the release binary under `LIBGL_ALWAYS_SOFTWARE=1`).
- A CLI smoke on every OS: `--version` SHA-regex,
  `--check-config` lead line, `--config-path`, `--list-themes`
  > 400, `--list-actions` > 50, `--list-keybinds` > 40,
  `--list-ssh-hosts` empty fallback, `--print-default-config`
  round-trip, `--shell-integration <bash|zsh|fish>` snippets,
  `--print-completions <bash|zsh|fish>` scripts,
  `--config /<typo>` + `--working-directory /<typo>` hard-fail
  exit codes (cycle 241), happy-path basename round-trip
  (Windows path-translation parity, cycle 241c).
- Cycle-250 **MSRV verification job** — pinned `dtolnay/rust-
  toolchain@1.89` builds + tests the workspace at the declared
  floor, catches a future transitive-dep MSRV bump at PR time
  instead of release time.
- Cycle-220 **iconutil / ico packaging smoke** on macOS and
  Windows runners — verifies the .icns / .ico build assets stay
  intact on every push (not just release tags).
- Cycle-876 **`dev-record` feature build** — the developer-only session
  recorder is compiled OUT of shipped builds, so the default checks never
  exercise it; CI separately runs `clippy -D` + the recorder tests under
  `--features dev-record` so the gated code + hooks can't bit-rot. See
  [DEV-RECORD.md](DEV-RECORD.md).

Separate workflows:

- `.github/workflows/audit.yml` (cycle 244) — `rustsec/audit-
  check` on every Cargo.lock change + daily 06:00 UTC cron.
- `.github/workflows/release.yml` — multi-platform packaging on
  every `v*` tag push. Cycle-254 adds SHA-256 sidecars (.sha256
  file alongside each artifact); cycle-258 onward, each release
  has six assets (three platform binaries + three sidecars).

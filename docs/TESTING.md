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

**230+ tests across the workspace** — see
[CHANGELOG.md](../CHANGELOG.md) for the per-cycle additions. The workspace
grows by 1–3 tests per audit cycle, so per-crate counts below are
order-of-magnitude snapshots rather than exact figures (run `cargo test
--workspace` for today's number).

- **kettle-vt** (~33 tests): plain-text passthrough is byte-exact;
  iTerm2 / Sixel / kitty (incl. zlib-less RGBA + chunked reassembly)
  decode to the right pixels; OSC 7 / OSC 133 are consumed and
  surrounding text still passes; OSC 1 → OSC 2 rewrite (cycle 102) so
  vim/tmux/ranger short-titles set the tab title; a sequence delivered
  one byte at a time still yields exactly one image; an ~8 MiB
  interleaved stream passes through intact in well under 5 s
  (linear-time / bounded-memory guard).

- **kettle-config** (~70+ tests): TokyoNight Night is the verified
  default palette; Ghostty `key = value` overrides, repeats, `palette`
  (0..=15 + cycle-124 out-of-range diagnostic), `infinite` scrollback,
  `ssh-host`; the bundled theme set has >400 entries incl. "TokyoNight
  Night"; Terminator default keybinds and trigger parsing; the
  cycle-104 `from_name` ↔ `action_names` round-trip drift guard; the
  cycle-116 `defaults_has_no_shadow_collisions` audit (no
  HashMap-shadowed bindings); the cycle-117 palette-completeness drift
  guard; the cycle-100 example-config drift guard; the cycle-125
  README-keybind regression guard; cycle-99/108/109 session
  load/save atomic + corruption-backup contracts; cycle-121/122
  empty-value resets for every string-config key; cycle-118
  `clamp_font_size` bounds.

- **kettle-core VT conformance** (~80+ tests): drives the *real*
  vte + alacritty_terminal path used by the PTY reader and asserts
  grid/cursor/SGR/mode state across a broad `vttest`-style sweep —
  text + `\r\n` + CUP addressing, erase-line/erase-display, SGR
  truecolor + bold + reset + dim/underline (4:3) + strikeout +
  double-underline + curly + dashed + dotted, tab stops + carriage
  return, alt-screen + bracketed-paste private modes, DECSTBM scroll
  region, DEC special-graphics line-drawing charset, ICH/DCH, IL/DL,
  DECSC/DECRC save-restore, DECAWM autowrap, DECOM origin mode,
  device responses via the real EventProxy PTY write-back
  (DSR 6n cursor-position, primary + secondary device attributes,
  DECRQM mode report, DECALN screen alignment, REP, G1 via SO/SI,
  RIS, EL/ED/ECH, CHA/HPA/VPA, DECSC-restores-SGR, SU/SD, DECSCUSR
  cursor shape, NEL/IND/RI, DECID, cursor-blink mode ?12,
  CHT/CBT tab nav, DECSET 1049 alt-screen, DECSET 2026 sync output),
  OSC 4 palette query + 104 reset (cycle 101), OSC 10/11/12 default
  fg/bg/cursor set + 110/111/112 reset siblings (cycle 101), OSC 8
  hyperlink cell-carry, OSC 52 clipboard copy + paste policies,
  wide CJK (2 cells + spacer) + wide-char wrap, combining-mark
  zero-width.

- **kettle-render** (~10 tests): truncate respects display columns
  (not chars), the `clamp_font_size` floor/ceiling/NaN/∞ contract
  (cycle 118), the `cap_axis_cells` GPU-texture safety guard
  (cycle 119), color resolve / dim / minimum-contrast WCAG math,
  the offscreen GPU pipeline self-test (real wgpu pipelines compile
  + render through Vulkan/Metal/DX12).

- **kettle-ui** (~40+ tests): split-tree layout tiles with no
  gaps/overlap, `remove_leaf` collapses to the sibling, nested
  splits keep every leaf; `Node::leaf_ids` DFS-order +
  `nth_leaf`/`leaf_index_of` symmetry; `close_tab_at` and
  `close_window` (cycle 113) tab-reaping with active-index
  bookkeeping; cycle-120 `reap_tabs` keeps focus on the same tab
  after a pane death; selection-autoscroll ladder; cwd-basename
  tab-title fallback (cycle 89); the SSH and `-e PROG`
  initial-pane-title heuristics (cycle 93 / cycle 95); session
  JSON round-trips (incl. SSH panes, focused-pane index, theme),
  `load_from_path` corruption-backup contract, `save_to_path`
  atomicity (write-temp + rename); xterm modifier encoding +
  modified-key SS3/CSI table; paste payload bracketing +
  injection-guard.

- **kettle** (binary, ~4 tests): clap argv parsing for the cycle-30
  `-e` + `-d` + `--config` combination; the cycle-105
  `format_ssh_hosts` table renderer (sort + column alignment +
  empty fallback).

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

`.github/workflows/ci.yml` runs on **ubuntu/macos/windows**: `fmt --check`,
`build --all-targets`, `clippy -D warnings`, `cargo test --workspace`, a
**headless GPU smoke** under Xvfb + software Vulkan on Linux, and a CLI
smoke (`--config-path`, `--list-themes` > 400) on every OS.

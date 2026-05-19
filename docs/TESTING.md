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

- **kettle-vt** (8 tests): plain-text passthrough is byte-exact; iTerm2 / Sixel
  / kitty (incl. zlib-less RGBA + **chunked reassembly**) decode to the right
  pixels; OSC 7 / OSC 133 are consumed and surrounding text still passes; a
  sequence delivered **one byte at a time** still yields exactly one image;
  an ~8 MiB interleaved stream passes through intact in well under 5 s
  (linear-time / bounded-memory guard).
- **kettle-config** (6 tests): TokyoNight Night is the verified default
  palette; Ghostty `key = value` overrides, repeats, `palette`, `infinite`
  scrollback and `ssh-host`; the bundled theme set has >400 entries incl.
  "TokyoNight Night"; Terminator default keybinds and trigger parsing.
- **kettle-core VT conformance** (11 tests): drives the *real* vte +
  alacritty_terminal path used by the PTY reader and asserts grid/cursor
  state — text + `\r\n` + CUP addressing, erase-line/erase-display,
  SGR truecolor + bold + reset, tab stops + carriage return, alt-screen
  & bracketed-paste private modes, DECSTBM scroll region, DEC
  special-graphics line-drawing charset, ICH/DCH, IL/DL,
  DECSC/DECRC save-restore, DECAWM autowrap, DECOM origin mode. The
  automatable, regression-proof core of a `vttest` sweep.
- **kettle-ui** (5 tests): split-tree layout tiles with no gaps/overlap,
  `remove_leaf` collapses to the sibling, nested splits keep every leaf;
  session JSON round-trips (incl. SSH panes) and pre-SSH sessions still load.

## Manual / interactive checks

These need a real display and are run by hand (or on real hardware):

- **VT conformance**: run [`vttest`](https://invisible-island.net/vttest/)
  and walk the cursor/erase/SGR/mode screens.
- **TUIs**: `nvim`/AstroNvim (icons, undercurl, truecolor, mouse), `tmux`,
  `htop`, `fzf`, `less`.
- **Images**: `img2sixel`/`chafa -f sixel`, `kitten icat`, iTerm2 `imgcat`.
- **Shell integration**: enable the snippet from
  [SHELL-INTEGRATION.md](SHELL-INTEGRATION.md), then `Ctrl+Up`/`Ctrl+Down`.
- **Perf**: `cat` a ~100 MB file / fast `yes` stays responsive.

## CI

`.github/workflows/ci.yml` runs on **ubuntu/macos/windows**: `fmt --check`,
`build --all-targets`, `clippy -D warnings`, `cargo test --workspace`, a
**headless GPU smoke** under Xvfb + software Vulkan on Linux, and a CLI smoke
(`--config-path`, `--list-themes` > 400) on every OS.

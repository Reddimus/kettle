# kettle 🫖

A fast, cross-platform, GPU-accelerated terminal emulator written in Rust —
combining the best ideas of **Ghostty**, **Terminator**, **kitty**,
**Alacritty** and **WezTerm** into one tool.

> Status: early but functional. A single window with tabs, panes, search,
> themes and a bundled Nerd Font works today on Linux/macOS/Windows. See
> [docs/ROADMAP.md](docs/ROADMAP.md) for what is landing next.

## Highlights

- **GPU rendering** — `wgpu` (Vulkan/Metal/DX12/GL) + a `cosmic-text` glyph
  atlas, with damage-aware draws.
- **Battle-tested VT core** — built on `alacritty_terminal` + `vte`, so
  vim/tmux/neovim/AstroNvim work out of the box (truecolor, undercurl,
  alt-screen, bracketed paste, mouse, kitty keyboard).
- **Terminator-style multiplexing** — tabs, splits, focus cycling,
  broadcast/group input — with Terminator's default keybindings.
- **Every Ghostty theme bundled** (~500, from iTerm2-Color-Schemes), default
  **TokyoNight Night**. Ghostty-compatible `key = value` config with live
  reload.
- **Bundled JetBrains Mono Nerd Font** — AstroNvim icons render with zero
  setup.
- **Search overlay** — `Ctrl+Shift+F`, regex, highlight + cycle.
- **Hyperlinks** — OSC 8 + URL autodetection, underlined with hover, open
  with `Ctrl`/`Cmd`+click.
- **Inline images** — Sixel, kitty graphics, and iTerm2 (OSC 1337) decoded
  and GPU-composited (`img2sixel`, `kitten icat`, `imgcat`).
- **Shell integration** — OSC 133 prompt marks; jump between prompts with
  `Ctrl+Up`/`Ctrl+Down` (see [docs/SHELL-INTEGRATION.md](docs/SHELL-INTEGRATION.md)).
- **Mouse reporting** — full passthrough so `vim`/`tmux`/`htop`/`fzf` mouse
  works (X10 + SGR 1006).
- **Cross-platform** — one codebase for Windows 11, Linux (X11/Wayland) and
  macOS, via `winit` + `portable-pty` (ConPTY on Windows).

## Quick start

```sh
# Linux build deps (Debian/Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libwayland-dev libxcb1-dev

cargo run --release
```

```sh
kettle --list-themes      # list all bundled themes
kettle --config-path      # show where the config file is read from
```

## Default keybindings (Terminator-compatible)

| Action | Bind | Action | Bind |
|---|---|---|---|
| Split right | `Ctrl+Shift+O` | New tab | `Ctrl+Shift+T` |
| Split down | `Ctrl+Shift+E` | Close pane | `Ctrl+Shift+W` |
| Focus next/prev pane | `Ctrl+Shift+N` / `P` | Close window | `Ctrl+Shift+Q` |
| Next/prev tab | `Ctrl+PgDn` / `PgUp` | Copy / Paste | `Ctrl+Shift+C` / `V` |
| **Search** | **`Ctrl+Shift+F`** | Fullscreen | `F11` |
| Resize split | `Shift+Arrows` or `Ctrl+Shift+Arrows` | Directional focus | `Alt+Arrows` |
| Font in/out/reset | `Ctrl + +` / `-` / `0` | Broadcast on/off | `Super+G` / `Shift+Super+G` |
| Reload config | `Ctrl+Shift+M` | Reset terminal | `Ctrl+Shift+R` |

## Configuration

kettle reads `$XDG_CONFIG_HOME/kettle/config` (Ghostty syntax). Example:

```ini
theme = TokyoNight Night
font-family = JetBrainsMono Nerd Font
font-size = 13
background-opacity = 1.0
cursor-style = block
keybind = ctrl+shift+t=new_tab
```

See [docs/CONFIG.md](docs/CONFIG.md) and the sample at
[`docs/kettle.example.config`](docs/kettle.example.config).

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — system design + diagrams
- [docs/RESEARCH.md](docs/RESEARCH.md) — analysis of other terminals & citations
- [docs/ROADMAP.md](docs/ROADMAP.md) — what's done / next
- [docs/CONFIG.md](docs/CONFIG.md) — every config key

## License

MIT. Bundled assets and adapted code are credited in [NOTICE](NOTICE).

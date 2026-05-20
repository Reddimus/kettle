# kettle 🫖

A fast, cross-platform, GPU-accelerated terminal emulator written in Rust —
combining the best ideas of **Ghostty**, **Terminator**, **kitty**,
**Alacritty** and **WezTerm** into one tool.

![kettle — TokyoNight Night, two-pane split with the redesigned tab bar](docs/images/kettle-hero.png)

> **Status: v1.0 — ready for daily use** on Linux, macOS and Windows 11.
> See [latest release](https://github.com/Reddimus/kettle/releases/latest)
> for prebuilt binaries (Linux tarball with installer, macOS universal
> `.app`, Windows zip with embedded `.ico`). The CI matrix runs on all
> three OSes every push: `fmt` → `clippy -D warnings` → `cargo test
> --workspace` → `cargo doc -D warnings` → headless GPU smoke (Linux) →
> CLI smoke + packaging smoke on every OS. See
> [docs/ROADMAP.md](docs/ROADMAP.md) for what is landing next.

## Highlights

- **GPU rendering** — `wgpu` (Vulkan/Metal/DX12/GL) + a `cosmic-text` glyph
  atlas, with damage-aware draws.
- **Battle-tested VT core** — built on `alacritty_terminal` + `vte`, so
  vim/tmux/neovim/AstroNvim work out of the box (truecolor, undercurl,
  alt-screen, bracketed paste, mouse, kitty keyboard).
- **Terminator-style multiplexing** — tabs (clickable tab bar), splits,
  focus cycling, broadcast/group input (with a yellow active-tab and
  focused-pane accent so you always know broadcast is on) — with
  Terminator's default keybindings.
- **Every Ghostty theme bundled** (~500, from iTerm2-Color-Schemes), default
  **TokyoNight Night**. Ghostty-compatible `key = value` config with live
  reload.
- **Bundled JetBrains Mono Nerd Font** — AstroNvim icons render with zero
  setup.
- **Search overlay** — `Ctrl+Shift+F`, real regex with **smart-case**
  (case-insensitive until you type an uppercase), highlight + cycle.
- **Hyperlinks** — OSC 8 + URL autodetection, underlined with hover, open
  with `Ctrl`/`Cmd`+click.
- **Inline images** — Sixel, kitty graphics, and iTerm2 (OSC 1337) decoded
  and GPU-composited (`img2sixel`, `kitten icat`, `imgcat`).
- **Shell integration** — OSC 133 prompt marks; jump between prompts with
  `Ctrl+Up`/`Ctrl+Down` (see [docs/SHELL-INTEGRATION.md](docs/SHELL-INTEGRATION.md)).
- **Mouse reporting** — full passthrough so `vim`/`tmux`/`htop`/`fzf` mouse
  works (X10 + SGR 1006); focus-event reporting (DEC ?1004) too.
- **Configurable bell** — visual flash and/or window-attention
  (taskbar/dock urgency); `bell = off|visual|attention|both`.
- **Polished input** — safe bracketed paste (newline-normalized,
  injection-guarded), double-click word / triple-click line selection +
  **Alt-drag rectangular (block) selection**, auto-copy, middle-click
  paste, focus-aware hollow cursor, configurable blink, visual bell.
- **Drag-and-drop files** — drop any file onto the window and its
  shell-quoted path is inserted at the cursor (with a trailing space, so
  `cat ` + drop + Enter works). Honors broadcast mode.
- **Session restore** — the tab/split tree and each pane's working directory
  are saved and restored across launches; new tabs/splits also inherit the
  focused pane's current directory (OSC 7).
- **SSH multiplexing** — `Ctrl+Shift+S` opens an SSH launcher (configured
  `ssh-host` names with fuzzy tab-complete, or any `user@host`); SSH tabs
  persist across sessions.
- **Quick-select hints** — `Ctrl+Shift+H` labels every URL / path /
  git-hash / IP on screen; type a label to open it (URLs) or copy it.
- **Command palette** — `Ctrl+Shift+K` opens a fuzzy command palette;
  type to filter every action, `Tab`/`↑↓` to select, `Enter` to run.
- **Live theme switching** — cycle the ~512 bundled themes at runtime
  (palette: "Next/Previous theme", or `next_theme`/`prev_theme` binds).
- **Cross-platform** — one codebase for Windows 11, Linux (X11/Wayland) and
  macOS, via `winit` + `portable-pty` (ConPTY on Windows).

## Quick start

```sh
# Linux build deps (Debian/Ubuntu)
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev

cargo run --release
```

**Easy desktop install on Linux** — drop the binary, launcher entry, and
icon into the standard XDG user paths so kettle shows up in the GNOME
Activities overview / Ubuntu **Super-key** search / KDE Krunner:

```sh
./scripts/install.sh                  # from a cloned repo (builds release first)
./install.sh                          # from a release tarball (uses bundled binary)
./scripts/install.sh --uninstall      # remove everything later
```

See [`docs/INSTALL.md`](docs/INSTALL.md) for prebuilt release tarballs,
macOS `.app`, and Windows packaging.

```sh
kettle --list-themes        # list every bundled theme (~512)
kettle --list-keybinds      # print the *effective* keymap (defaults + your overrides + unbinds)
kettle --list-actions       # list every action name accepted by `keybind = trigger=action`
kettle --list-ssh-hosts     # print configured `ssh-host = name=target` entries
kettle --config-path        # show where the config file is read from
kettle --check-config       # validate config: resolved settings + unknown-key / malformed-value diagnostics
kettle --config FILE        # use a specific config file (live-reloaded; error if it doesn't exist)
kettle -d /path/to/dir      # open the first tab in this directory
kettle -e htop              # run a command instead of the shell
kettle -e ssh -t host       # (-e consumes the rest of the args)
kettle --screenshot OUT.png # render a representative frame offscreen and exit (no window)
```

## Default keybindings (Terminator-compatible)

| Action | Bind | Action | Bind |
|---|---|---|---|
| Split top/bottom | `Ctrl+Shift+O` | New tab | `Ctrl+Shift+T` |
| Split left/right | `Ctrl+Shift+E` | Close pane | `Ctrl+Shift+W` |
| Split (auto-pick) | `Ctrl+Shift+A` | New window | `Ctrl+Shift+I` |
| Focus next/prev pane | `Ctrl+Shift+N` / `P` | Close window | `Ctrl+Shift+Q` |
| Next/prev tab | `Ctrl+PgDn` / `PgUp` | Move tab left/right | `Ctrl+Shift+PgUp` / `PgDn` |
| Goto tab 1..9 | `Alt+1..9` | Zoom / unzoom pane | `Ctrl+Shift+X` |
| Copy / Paste | `Ctrl+Shift+C` / `V` | **Search** | **`Ctrl+Shift+F`** |
| **SSH launcher** | **`Ctrl+Shift+S`** | **Command palette** | **`Ctrl+Shift+K`** |
| **Quick-select hints** | **`Ctrl+Shift+H`** | Fullscreen | `F11` |
| Jump prev/next prompt | `Ctrl+Up` / `Down` | Resize split | `Shift+Arrows` |
| Directional focus | `Alt+Arrows` | Scroll to top/bottom | `Shift+Home` / `End` |
| Scroll line / page | `Ctrl+Shift+Up/Down` / `Shift+PgUp/PgDn` | Reset font size | `Ctrl+0` |
| Font bigger / smaller | `Ctrl+` `+` / `-` | Broadcast on/off | `Super+G` / `Shift+Super+G` |
| Reload config | `Ctrl+Shift+M` | Reset terminal | `Ctrl+Shift+R` |

Full effective keymap with your `--config` applied: `kettle --list-keybinds`.

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
- [docs/UX-COMPARISON.md](docs/UX-COMPARISON.md) — cited UI/UX matrix vs Ghostty/kitty/WezTerm/Terminator/Alacritty
- [docs/INSTALL.md](docs/INSTALL.md) — install per-OS / from source
- [docs/ROADMAP.md](docs/ROADMAP.md) — what's done / next
- [docs/TESTING.md](docs/TESTING.md) — test suite + CI
- [CHANGELOG.md](CHANGELOG.md) — release history
- [docs/CONFIG.md](docs/CONFIG.md) — every config key
- [docs/SHELL-INTEGRATION.md](docs/SHELL-INTEGRATION.md) — OSC 133 prompt-mark hooks for bash / zsh / fish
- [CONTRIBUTING.md](CONTRIBUTING.md) — the audit-cycle pattern + how to land your first change

## License

MIT. Bundled assets, third-party crates kettle consumes (Alacritty's VT
core, WezTerm's `portable-pty`, cosmic-text), and the design-source
projects kettle cites (kitty's graphics protocol spec, Terminator's
splits-and-broadcast convention, Ghostty's config syntax) are all
credited in [NOTICE](NOTICE).

# kettle `>(_)~`

[![CI](https://github.com/Reddimus/kettle/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/ci.yml)
[![Audit](https://github.com/Reddimus/kettle/actions/workflows/audit.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/audit.yml)
[![Latest release](https://img.shields.io/github/v/release/Reddimus/kettle?label=release&color=blue)](https://github.com/Reddimus/kettle/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue?logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Kettle is a GPU-accelerated terminal workspace for macOS, Linux, and
Windows 11. It includes tabs, splits, search, session restore, themes, and local
automation.

![Kettle with a two pane split and the TokyoNight Night theme](docs/images/kettle-hero.png)

## Install

Download a package from the
[latest release](https://github.com/Reddimus/kettle/releases/latest).

| Platform | Package | Setup |
| --- | --- | --- |
| Linux, glibc 2.35+ | `kettle-linux-*.tar.gz` | Run the command below |
| macOS 11+ | `kettle-macos-universal.zip` | Move `kettle.app` to Applications |
| Windows 11 | `kettle-windows-x86_64.zip` | Extract and run `install.ps1` |

Linux installs for the current user under `~/.local`:

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
```

No package needs administrator access. The Linux installer verifies the signed
release before making changes. See [Installation](docs/INSTALL.md) for Nix,
older Linux systems, uninstall steps, and source builds.

On Linux and Windows, check with `kettle --check-update` and install with
`kettle update`. Set `update-policy = off` to disable background checks. macOS
updates come from the release page.

## Quick start

1. Open Kettle from the app launcher or run `kettle`.
2. Split the pane with `Ctrl+Shift+E` or `Ctrl+Shift+O`.
3. Open Settings with `Ctrl+,`.
4. Create a starter config with `kettle --write-default-config`.
5. On Bash, Zsh, Fish, or an explicitly selected PowerShell, add the one-line
   [shell integration](docs/SHELL-INTEGRATION.md#one-liner-recommended) for
   prompt navigation and completion cards.

Kettle reloads the config when you save it. Run `kettle --config-path` to find
the active file and `kettle --check-config` to validate it.

## Highlights

* GPU rendering with damage aware draws and a shared glyph atlas
* Tabs, splits, pane movement, broadcast input, and live tab tear off
* Multiple native windows in one process, with an explicit isolated process mode
* Search across the screen and bounded scrollback
* Shell owned completion candidates in an IDE style card above the active prompt
* Sixel, graphics placement, and OSC 1337 inline images
* Hyperlinks, local file links, drag and drop paths, and quick select hints
* Optional session restore, recording, SSH profiles, and authenticated updates
* A local control API plus an MCP server, both disabled by default
* 500+ themes and a bundled JetBrains Mono Nerd Font

The completion panel stays in a detached lane above the active command, aligned
with the first editable column, and never inserts or runs text itself. Fish and
PowerShell can use it automatically when their stock Tab binding is still
active. Bash, Zsh, and customized shells can publish existing candidates
through [Shell integration](docs/SHELL-INTEGRATION.md).

Native material is enabled by default. macOS and Windows use subtle
translucency; Linux and other targets stay at 99% opacity whether compositor
blur is available or not.

macOS and Windows:

```ini
background-opacity = 0.86
window-blur = true
```

Linux and other targets:

```ini
background-opacity = 0.99
window-blur = true
```

Kettle follows macOS Reduce Transparency immediately. Linux compositors may
ignore the blur hint; Kettle keeps explicitly lower live opacity at a 99% floor
in that case so text stays readable without changing screenshots or your config.
Windows matches its DWM caption to the active theme. macOS keeps AppKit's
caption opaque and follows the selected light or dark appearance. Set opacity
to `1.0` and blur to `false` for a fully opaque window.

## Common keys

| Action | Key |
| --- | --- |
| New tab | `Ctrl+Shift+T` |
| Split left or right | `Ctrl+Shift+E` |
| Split top or bottom | `Ctrl+Shift+O` |
| Focus next or previous pane | `Ctrl+Shift+N` or `Ctrl+Shift+P` |
| Directional focus, Linux/Windows | `Alt+Arrow` |
| Directional focus, macOS | `Ctrl+Cmd+Arrow` |
| Search | `Ctrl+Shift+F` |
| Command palette | `Ctrl+Shift+K` |
| Copy or paste | `Ctrl+Shift+C` or `Ctrl+Shift+V` |
| Settings | `Ctrl+,` |
| Zoom pane | `Ctrl+Shift+X` |

On Linux and Windows, `Alt+Arrow` moves only when a split exists in that
direction. At an outer edge, the running program receives the chord. macOS
keeps bare Option available for text entry unless `macos-option-as-alt` is on.
macOS also provides `Cmd+T`, `Cmd+C/V`, `Cmd+F`, and `Cmd+,`.

Use `kettle --list-keybinds` for the effective map and
`kettle --list-actions` for every bindable action.

## Configuration

Kettle uses a plain `key = value` file:

```ini
theme = TokyoNight Night
font-size = 13
completion-overlay = auto
cursor-style = block
keybind = ctrl+shift+t=new_tab
```

Useful commands:

```sh
kettle --write-default-config
kettle --config-path
kettle --check-config
kettle --list-themes
kettle --list-keybinds
kettle --gpu-info
```

See [Configuration](docs/CONFIG.md) for every setting and
[Settings](docs/SETTINGS.md) for the in-app editor.
Use `update-policy = auto`, `notify`, or `off` to control background checks;
explicit `--check-update` requests still work in every mode.

## Automation

Run a command under Kettle's terminal engine without opening a window:

```sh
kettle exec -- echo ok
```

Enable the local control surface only if every process running as your operating
system user is trusted:

```sh
kettle --agent-server read-only
kettle ctl list_panes
kettle ctl read_screen
kettle mcp
```

Once enabled, any same-user process can connect without a pairing prompt.
`read-only` permits inspection. `full` permits input and arbitrary command
execution as your user. Read [Automation and MCP](docs/AGENT.md) before
enabling it.

## Develop

Kettle requires Rust 1.89 or newer. On Debian or Ubuntu:

```sh
sudo apt-get install -y pkg-config libfontconfig1-dev libfreetype6-dev \
  libx11-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev \
  libxcb1-dev libvulkan1 mesa-vulkan-drivers

git clone https://github.com/Reddimus/kettle
cd kettle
cargo run --release
```

Before sending a change:

```sh
cargo fmt --all --check
just gauntlet
```

Read [Contributing](CONTRIBUTING.md), [Testing](docs/TESTING.md), and the
[performance methodology](scripts/perf/README.md) for the full workflow.

## Documentation

* [Getting started](docs/GETTING-STARTED.md)
* [Installation](docs/INSTALL.md)
* [Configuration](docs/CONFIG.md)
* [Shell integration](docs/SHELL-INTEGRATION.md)
* [Architecture](docs/ARCHITECTURE.md)
* [Testing](docs/TESTING.md)
* [Release process](docs/RELEASING.md)
* [Security and vulnerability reporting](SECURITY.md)
* [Changelog](CHANGELOG.md)

## License

Kettle is available under the [MIT license](LICENSE). Third party code, assets,
protocol sources, and design references are listed in [NOTICE](NOTICE).

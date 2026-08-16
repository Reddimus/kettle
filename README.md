# kettle `>(_)~`

[![CI](https://github.com/Reddimus/kettle/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/ci.yml)
[![Audit](https://github.com/Reddimus/kettle/actions/workflows/audit.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/audit.yml)
[![Latest release](https://img.shields.io/github/v/release/Reddimus/kettle?label=release&color=blue)](https://github.com/Reddimus/kettle/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue?logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Kettle is a fast GPU accelerated terminal workspace for macOS, Linux, and
Windows 11. It starts ready to use with a bundled font and themes, then adds
tabs, splits, multiple windows, search, session restore, and local automation.

![Kettle with a two pane split and the TokyoNight Night theme](docs/images/kettle-hero.png)

## Install

Prebuilt packages are available on the
[latest release](https://github.com/Reddimus/kettle/releases/latest).

| Platform | Package | Setup |
| --- | --- | --- |
| Linux | `kettle-linux-*.tar.gz` | Run the installer below |
| macOS 11 or newer | `kettle-macos-universal.zip` | Drag `kettle.app` to Applications |
| Windows 11 | `kettle-windows-x86_64.zip` | Extract it and run `install.ps1` |

### Linux

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
```

This installs Kettle for the current user under `~/.local`. It does not need
`sudo` or a Rust toolchain. The installer verifies the signed release manifest
and archive before changing the install.

### macOS

Download `kettle-macos-universal.zip`, unzip it, and move `kettle.app` to
`/Applications`. The app contains native Apple Silicon and Intel binaries.

### Windows 11

Extract `kettle-windows-x86_64.zip`, open PowerShell in that folder, and run:

```powershell
.\install.ps1
```

Use `-WithShellIntegration` to install prompt marks too, or `-Uninstall` to
remove Kettle later. The installer creates a Start menu entry and adds the
per-user binary directory to `PATH` without administrator access.

For package details, older Linux distributions, Nix, and source builds, see
[Installation](docs/INSTALL.md).

## Start here

1. Open `kettle` from your app launcher or shell.
2. Split the pane with `Ctrl+Shift+E` or `Ctrl+Shift+O`.
3. Open Settings with `Ctrl+,`, or right click a pane for common controls.
4. Write a starter config with `kettle --write-default-config`.

Kettle reloads config changes when you save. Run `kettle --config-path` to see
which file is active and `kettle --check-config` to validate it.

## What is included

* GPU rendering with damage aware draws and a shared glyph atlas
* Tabs, splits, pane movement, broadcast input, and live tab tear off
* Multiple native windows in one process, with an explicit isolated process mode
* Search across the screen and bounded scrollback
* Shell owned completion candidates in a detached panel at the top of the pane
* Sixel, graphics placement, and OSC 1337 inline images
* Hyperlinks, local file links, drag and drop paths, and quick select hints
* Optional session restore, recording, SSH profiles, and authenticated updates
* A local control API plus an MCP server, both disabled by default
* 500+ themes and a bundled JetBrains Mono Nerd Font

The completion panel stays in a detached lane above the active command and
never inserts or runs text itself. Fish and PowerShell can use it automatically
when their stock Tab binding is still active. Bash, Zsh, and customized shells
can publish existing candidates through [Shell integration](docs/SHELL-INTEGRATION.md).

Native window material is opt in. Combine a translucent background with blur:

```ini
background-opacity = 0.82
window-blur = true
```

On macOS, Kettle uses the native window material and follows Reduce
Transparency immediately. Other platforms use the compositor support exposed
by the operating system. If blur is unavailable, ordinary alpha transparency
still works.

## Common keys

| Action | Key |
| --- | --- |
| New tab | `Ctrl+Shift+T` |
| Split left or right | `Ctrl+Shift+E` |
| Split top or bottom | `Ctrl+Shift+O` |
| Focus next or previous pane | `Ctrl+Shift+N` or `Ctrl+Shift+P` |
| Directional focus | `Alt+Arrow` |
| Search | `Ctrl+Shift+F` |
| Command palette | `Ctrl+Shift+K` |
| Quick select | `Ctrl+Shift+H` |
| Copy or paste | `Ctrl+Shift+C` or `Ctrl+Shift+V` |
| Settings | `Ctrl+,` |
| Zoom pane | `Ctrl+Shift+X` |

On Linux and Windows, `Alt+Arrow` moves to a split only when one exists in that
direction. At an outer edge, Kettle sends the chord to the running program so
its own editor shortcuts still work. macOS also provides familiar Command key
bindings. Bare Option keys remain available for text entry unless
`macos-option-as-alt` is enabled.

Run `kettle --list-keybinds` for the effective map after your config is applied.
The full action list is available through `kettle --list-actions`.

## Configuration

Kettle uses a plain `key = value` file:

```ini
theme = TokyoNight Night
font-family = JetBrainsMono Nerd Font
font-size = 13
background-opacity = 1.0
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
kettle --check-update
```

See [Configuration](docs/CONFIG.md) for every setting and
[Settings](docs/SETTINGS.md) for the in-app editor.
Use `update-policy = auto`, `notify`, or `off` to control background checks;
explicit `--check-update` requests still work in every mode.

## Automation

Kettle can run a command under its terminal engine without opening a window:

```sh
kettle exec -- echo ok
```

It can also expose a local control server to trusted processes running as the
same operating system user:

```sh
kettle --agent-server read-only
kettle ctl list_panes
kettle ctl read_screen
kettle mcp
```

The server is off by default. `read-only` permits inspection; `full` also
permits input and command execution. Read [Automation and MCP](docs/AGENT.md)
before enabling write access.

## Performance

The Windows benchmark harness compares Kettle with Windows Terminal,
Alacritty, WezTerm, Rio, and Tabby using isolated configs where the application
supports them. Release claims require physical displays, repeated samples, a
pinned prior Kettle release, and a clean release candidate. Synthetic tests do
not count as live GPU evidence.

See [Performance methodology](scripts/perf/README.md) for commands, sample
counts, statistical margins, and publication rules.

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

Read [Contributing](CONTRIBUTING.md) and [Testing](docs/TESTING.md) for the full
workflow.

## Documentation

* [Getting started](docs/GETTING-STARTED.md)
* [Installation](docs/INSTALL.md)
* [Configuration](docs/CONFIG.md)
* [Shell integration](docs/SHELL-INTEGRATION.md)
* [Automation and MCP](docs/AGENT.md)
* [Architecture](docs/ARCHITECTURE.md)
* [Testing](docs/TESTING.md)
* [Release process](docs/RELEASING.md)
* [Version history](docs/VERSION-HISTORY.md)
* [Changelog](CHANGELOG.md)

## License

Kettle is available under the [MIT license](LICENSE). Third party code, assets,
protocol sources, and design references are listed in [NOTICE](NOTICE).

# kettle `>(_)~`

[![CI](https://github.com/Reddimus/kettle/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/ci.yml)
[![Audit](https://github.com/Reddimus/kettle/actions/workflows/audit.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/audit.yml)
[![Latest release](https://img.shields.io/github/v/release/Reddimus/kettle?label=release&color=blue)](https://github.com/Reddimus/kettle/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue?logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Kettle is a GPU-accelerated terminal workspace for macOS and Linux. It includes
tabs, splits, search, session restore, themes, and local automation.

![Kettle with a two pane split and the TokyoNight Night theme](docs/images/kettle-hero.png)

## Install

Download a package from the
[latest release](https://github.com/Reddimus/kettle/releases/latest).

| Platform | Package | Setup |
| --- | --- | --- |
| Linux, glibc 2.35+ | `kettle-linux-*.tar.gz` | Run the command below |
| macOS 11+ | `kettle-macos-universal.zip` | Move `kettle.app` to Applications |

Windows distribution ended with 3.3.0. Archived Windows packages remain
attached to their historical releases, but 4.0 and later do not ship or update
Windows. CI still compiles and tests retained Windows code. That coverage does
not restore distribution or supported-platform status.

Linux installs for the current user under `~/.local`:

```sh
curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
```

No package needs administrator access. The Linux installer verifies the signed
release before making changes. See [Installation](docs/INSTALL.md) for Nix,
older Linux systems, uninstall steps, and source builds.

Check with `kettle --check-update` and install with `kettle update`. Set
`update-policy = off` to disable background checks. On macOS this replaces
`kettle.app` in place, and refuses if Homebrew owns the copy.

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

* Tabs, splits, multiple windows, pane movement, and broadcast input
* A macOS Dock menu with New Window, New Tab, and your open windows
* GPU rendering, bounded scrollback, search, and inline image protocols
* An IDE style completion card driven by the shell
* File drop, copied media paths, and private image or video previews
* Session restore, recording, SSH profiles, and signed updates
* A bundled font and more than 500 themes
* An optional local control API and MCP server, both off by default

## Common keys

| Action | Key |
| --- | --- |
| New tab | `Ctrl+Shift+T` |
| Split left or right | `Ctrl+Shift+E` |
| Split top or bottom | `Ctrl+Shift+O` |
| Focus next or previous pane | `Ctrl+Shift+N` or `Ctrl+Shift+P` |
| Directional pane focus, Linux | `Alt+Arrow` |
| Directional pane focus, macOS | `Cmd+Opt+Arrow` or `Ctrl+Cmd+Arrow` |
| Search | `Ctrl+Shift+F` |
| Command palette | `Ctrl+Shift+K` |
| Copy or paste | `Ctrl+Shift+C` or `Ctrl+Shift+V` |
| Settings | `Ctrl+,` |
| Zoom pane | `Ctrl+Shift+X` |
| Delete word / line, macOS | `Opt+Backspace` or `Cmd+Backspace` |

On Linux, `Alt+Arrow` moves only when a split exists in that
direction. At an outer edge, the running program receives the chord. macOS
keeps bare Option available for text entry unless `macos-option-as-alt` is on
— but only for keys that produce text, so `Opt+Backspace` deletes a word and
`Opt+Arrow` moves by one on every setting, and `Cmd+Backspace` deletes to the
start of the line. macOS also provides `Cmd+T`, `Cmd+C/V`, `Cmd+F`, and
`Cmd+,`. `Cmd+Opt+Arrow` is the chord iTerm2 and Ghostty use, so either set of
habits works; `Ctrl+Cmd+Arrow` stays bound for anyone who learned it here first.

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

Native material is on by default. macOS uses blur at 86% opacity; Linux stays
at 99% opacity. Set `background-opacity = 1.0` and
`window-blur = false` for an opaque window.

## Clipboard media

`Ctrl+Shift+V` pastes text or an explicitly copied file list. A clipboard
screenshot becomes a bounded owner-only temporary PNG, and Kettle pastes its
quoted path. A copied or dropped video remains the user's original file. Its
receipt uses a native poster when available without decoding video inside
Kettle. The pasted text is still only a path; the program in the pane has not
opened or received the file.

Read [Terminal client compatibility](docs/TERMINAL-CLIENT-COMPATIBILITY.md)
for shell quoting, WSL translation, privacy limits, and client behavior.

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

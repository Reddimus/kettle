# kettle `>(_)~`

[![CI](https://github.com/Reddimus/kettle/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/ci.yml)
[![Audit](https://github.com/Reddimus/kettle/actions/workflows/audit.yml/badge.svg?branch=main)](https://github.com/Reddimus/kettle/actions/workflows/audit.yml)
[![Latest release](https://img.shields.io/github/v/release/Reddimus/kettle?label=release&color=blue)](https://github.com/Reddimus/kettle/releases/latest)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue?logo=rust)](Cargo.toml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Kettle is a GPU-accelerated terminal workspace for macOS and Linux. It has
tabs, splits, search, session restore, themes, and local automation.

![Kettle with a two pane split and the TokyoNight Night theme](docs/images/kettle-hero.png)

## Install

Download a package from the
[latest release](https://github.com/Reddimus/kettle/releases/latest).

| Platform | Package | Setup |
| --- | --- | --- |
| Linux, glibc 2.35+ | `kettle-linux-*.tar.gz` | Run the installer below |
| macOS 11+ | `kettle-macos-universal.zip` | Move `kettle.app` to Applications |

Neither package needs administrator access. The Linux installer puts Kettle
under `~/.local` for the current user and verifies the signed release before
changing files.

```sh
KETTLE_RAW_URL=https://raw.githubusercontent.com/Reddimus/kettle/main
curl -fsSL "$KETTLE_RAW_URL/scripts/install-online.sh" | sh
```

Windows distribution ended with 3.3.0. Archived Windows packages remain
attached to their historical releases, but 4.0 and later do not ship or update
Windows. CI still compiles and tests retained Windows code. That coverage does
not restore distribution or supported-platform status.

Read [Installation](docs/INSTALL.md) for Nix, older Linux systems, source
builds, and uninstall steps.

Check for a release and install it with:

```sh
kettle --check-update
kettle update
```

Set `update-policy = off` to disable background checks. On macOS, the updater
replaces `kettle.app` in place and refuses a copy owned by Homebrew.

## First run

1. Open Kettle from the app launcher or run `kettle`.
2. Press `Ctrl+Shift+E` for a side-by-side split or `Ctrl+Shift+O` for a
   top-and-bottom split.
3. Press `Ctrl+,` to open Settings.
4. Add the optional [shell integration](docs/SHELL-INTEGRATION.md) for prompt
   navigation and completion cards.

These commands create, locate, validate, and inspect the active configuration:

```sh
kettle --write-default-config
kettle --config-path
kettle --check-config
kettle --list-keybinds
```

Kettle reloads its plain `key = value` configuration when you save it. See
[Configuration](docs/CONFIG.md) for every setting and [Settings](docs/SETTINGS.md)
for the in-app editor.

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
| Delete word or line on macOS | `Opt+Backspace` or `Cmd+Backspace` |

Use `kettle --list-keybinds` for the effective map. macOS also provides
`Cmd+T`, `Cmd+C/V`, `Cmd+F`, and `Cmd+,`. [Getting started](docs/GETTING-STARTED.md)
covers pane movement, tabs, search, and first-run troubleshooting.

## Features

- Tabs, splits, multiple windows, pane movement, and broadcast input
- A macOS Dock menu for new tabs, new windows, and open windows
- GPU rendering, bounded scrollback, search, and inline image protocols
- Shell-driven completion cards
- File drop, copied media paths, and private image or video previews
- Session restore, recording, SSH profiles, and signed updates
- A bundled font and more than 500 themes
- An optional local control API and MCP server, both off by default

## Privacy and local automation

Media paste sends a quoted local path to the pane, not the file contents. Read
[Terminal client compatibility](docs/TERMINAL-CLIENT-COMPATIBILITY.md) for
shell quoting, temporary-file privacy, and client behavior.

Enable the local control API or MCP server only if every process running as
your operating system user is trusted. Once enabled, any same-user process can
connect without a pairing prompt. `read-only` permits inspection. `full`
permits input and arbitrary command execution as your user. Read
[Automation and MCP](docs/AGENT.md) before using either interface.

## Build and test

Kettle requires Rust 1.89 or newer. macOS needs a stable Rust toolchain. Linux
contributors should install the dependencies listed under
[source builds](docs/INSTALL.md#from-source).

```sh
git clone https://github.com/Reddimus/kettle
cd kettle
cargo build --locked --release -p kettle
```

Run the repository gate before sending a change:

```sh
cargo fmt --all --check
just gauntlet
```

Read [Contributing](CONTRIBUTING.md) for the change workflow and
[Testing](docs/TESTING.md) for prerequisites, native checks, and the larger
verification gates. Performance work follows the
[benchmark methodology](scripts/perf/README.md).

## Documentation

- [Getting started](docs/GETTING-STARTED.md) and [Installation](docs/INSTALL.md)
- [Configuration](docs/CONFIG.md) and [Settings](docs/SETTINGS.md)
- [Shell integration](docs/SHELL-INTEGRATION.md) and [Automation](docs/AGENT.md)
- [Architecture](docs/ARCHITECTURE.md) and [Testing](docs/TESTING.md)
- [Release process](docs/RELEASING.md)
- [Security and vulnerability reporting](SECURITY.md)
- [Changelog and older-release index](CHANGELOG.md)

## License

Kettle is available under the [MIT license](LICENSE). Third-party code, assets,
protocol sources, and design references are listed in [NOTICE](NOTICE).

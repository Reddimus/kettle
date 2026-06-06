# Getting started with kettle

A friendly, no-jargon walkthrough for your first few minutes with kettle. If
you've never edited a config file in your life, that's fine — kettle has a
built-in **Settings** panel for the common things.

## 1. Install

- **Windows 11** — download `kettle-windows-x86_64.zip` from the
  [latest release](https://github.com/Reddimus/kettle/releases/latest), unzip
  it anywhere, and run `install.ps1`. Then press the **Windows key** and type
  **kettle**. (No admin prompt — it installs just for you.)
- **Linux** — one line, no `sudo`:
  ```sh
  curl -fsSL https://raw.githubusercontent.com/Reddimus/kettle/main/scripts/install-online.sh | sh
  ```
  Then press **Super** and type **kettle**.
- **macOS** — download `kettle-macos-universal.zip`, unzip, drag `kettle.app`
  to Applications, right-click → Open the first time.

See [INSTALL.md](INSTALL.md) for package managers (AUR, Homebrew, Nix) and
SHA-256 verification.

## 2. Your first window

kettle opens a normal terminal — on Windows it starts **PowerShell 7**, on
Linux/macOS your usual shell. Type commands like you would anywhere. To use
**WSL / Ubuntu** as your shell on Windows, see
[the WSL recipe](CONFIG.md#launching-wsl--ubuntu-as-your-shell-windows).

## 3. Change settings — no file editing required

Press **`Ctrl + ,`** (or **right-click → Settings…**) to open the **Settings**
panel. Use the keyboard:

- **↑ / ↓** — move between options
- **← / →** — change the highlighted option (font size, theme opacity,
  scrollbar, bell, cursor, …)
- **Tab** — cycle category (Appearance → Behavior → Keybinds). The **Keybinds**
  category is an interactive rebinder: pick an action, press a chord (with a
  modifier) to bind it live, and it's written to your config.
- **Esc** — close

Changes apply **instantly** and are saved automatically — no restart, no file
to edit. For the handful of advanced options not in the panel, the
**Advanced** path opens the config file in your editor. Full reference:
[SETTINGS.md](SETTINGS.md) and [CONFIG.md](CONFIG.md).

## 4. Splits, tabs, and getting around

kettle uses **Terminator-style** keys:

| Do this | Press |
|---|---|
| Split left/right | `Ctrl+Shift+E` |
| Split top/bottom | `Ctrl+Shift+O` |
| New tab | `Ctrl+Shift+T` |
| Close pane | `Ctrl+Shift+W` |
| Move focus between panes | `Alt + Arrow` |
| Resize the split | `Shift + Arrow` |
| Next / previous tab | `Ctrl+PageUp` / `Ctrl+PageDown` |
| Settings | `Ctrl+,` |
| Command palette (search every action) | `Ctrl+Shift+K` |

See the full list any time with `kettle --list-keybinds`, or press
`Ctrl+Shift+K` and type what you want.

## 5. If something looks wrong

- **Text too big/small?** Open Settings (`Ctrl+,`) → Appearance → Font size,
  or use `Ctrl + +` / `Ctrl + -`.
- **Want the defaults back?** Delete your config file (its location is shown by
  `kettle --config-path`) and relaunch.
- **Found a bug?** Crash logs are written to
  `%LOCALAPPDATA%\kettle\crash\` (Windows) or
  `~/.local/state/kettle/crash/` (Linux) — attach one to an issue.

Welcome aboard. For everything else, [CONFIG.md](CONFIG.md) is the full
reference and [README.md](../README.md) has the feature tour.

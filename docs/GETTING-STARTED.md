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
  This path needs a current `curl`, GNU `tar`, and OpenSSL 3.0 or newer;
  it stops before extraction if the bounded signed-release checks cannot run.
  Then press **Super** and type **kettle**.
- **macOS** — download `kettle-macos-universal.zip`, unzip, drag `kettle.app`
  to Applications, right-click → Open the first time.

See [INSTALL.md](INSTALL.md) for Nix, the generated-but-not-yet-published
Homebrew/AUR metadata, and SHA-256 verification.

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
- **Tab** — cycle category (Appearance → Background → Behavior → Search → Tabs
  → Graphics → Keybinds). The **Keybinds** category is an interactive rebinder:
  pick an action, press a chord (with a modifier) to bind it live, and it's
  written to your config.
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
| Search screen + scrollback | `Ctrl+Shift+F` |
| Settings | `Ctrl+,` |
| Command palette (search every action) | `Ctrl+Shift+K` |

See the full list any time with `kettle --list-keybinds`, or press
`Ctrl+Shift+K` and type what you want.

## 5. Search screen and scrollback

Press **`Ctrl+Shift+F`** to open the bottom search bar. Enter a Rust regular
expression (up to 4096 UTF-8 bytes); an incomplete or invalid expression is
shown as **Invalid pattern** instead of being changed into a literal search. A
valid expression that exceeds the bounded engine budget shows **Pattern too
complex**.

- `Enter` / `Shift+Enter` finds in the default / opposite direction.
- `F3` / `Shift+F3` always finds next / previous.
- **Wrap** controls whether stepping crosses the history boundary.
- **Smart** ignores case until the pattern contains an uppercase character;
  **Match** always matches case; **Ignore** never matches case.
- **Invert** flips the default direction. `Escape` closes the bar while keeping
  the selected result anchored on screen.

The query editor follows Unicode grapheme boundaries for caret movement and
deletion, so combining marks and emoji sequences are not split. Kettle
remembers the last query for each pane within its current window. The status
shows searching, match/boundary, invalid, too-complex, no-match, or **Results
limited** rather than a global count. Results limited means a pathological
soft-wrapped line, an output-interrupted navigation, or the nearby-highlight
cap made ordering uncertain; Kettle will not claim a possibly wrong first,
last, or miss. Ordinary work-budget pauses resume automatically. This keeps
large or infinite scrollback responsive.

## 6. If something looks wrong

- **Text too big/small?** Open Settings (`Ctrl+,`) → Appearance → Font size,
  or use `Ctrl + +` / `Ctrl + -`.
- **Want the defaults back?** Delete your config file (its location is shown by
  `kettle --config-path`) and relaunch.
- **Found a bug?** Crash logs are written to
  `%LOCALAPPDATA%\kettle\crash\` (Windows) or
  `~/.local/state/kettle/crash/` (Linux). GPU device-loss records are written
  separately to `%LOCALAPPDATA%\kettle\diagnostics\` or
  `~/.cache/kettle/diagnostics/`; they contain adapter/recovery metadata and no
  terminal contents. Attach the relevant file to an issue.

Welcome aboard. For everything else, [CONFIG.md](CONFIG.md) is the full
reference and [README.md](../README.md) has the feature tour.

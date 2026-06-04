# Settings overlay

kettle has a built-in **Settings** panel so you can change the common options
without editing a config file. Open it with **`Ctrl + ,`** or
**right-click → Settings…**.

## Navigating

| Key | Action |
|---|---|
| ↑ / ↓ | Move between options |
| ← / → | Change the highlighted option |
| Space / Enter | Toggle / cycle the highlighted option |
| Tab / Shift+Tab | Next / previous category |
| Esc | Close |

Every change applies **immediately** and is written to your config file (shown
by `kettle --config-path`), so it survives restarts. There's nothing to "save".

## What's in the panel

**Appearance**

| Option | Config key | Notes |
|---|---|---|
| Font size | `font-size` | 6–72 pt |
| Background opacity | `background-opacity` | 20–100% (stored as 0.0–1.0) |
| Window padding | `window-padding-x` | 0–40 px |
| Cursor shape | `cursor-style` | block · beam · underline |
| Cursor blink | `cursor-blink` | on / off |
| Show pane titlebars | `show-titlebar` | on / off |

**Behavior**

| Option | Config key | Notes |
|---|---|---|
| Scrollbar | `scrollbar` | hidden · auto · always |
| Bell | `bell` | off · visual · attention · both |
| Scrollback lines | `scrollback` | 0–100000 |
| Copy on select | `copy-on-select` | on / off |
| Hide mouse while typing | `mouse-hide-while-typing` | on / off |
| Focus mode | `focus` | click · follows-mouse · system |

**Keybinds** — rebind common actions interactively. Each row shows the chord
currently bound to that action; press **Enter** on a row, then press the new
chord you want (any modifier combination). It binds immediately and is saved to
your config as a `keybind = …` line. Press **Esc** to cancel a capture. Covered
actions: split right/down, close pane, new/next/previous tab, search, command
palette, open settings, zoom pane, copy, paste.

## Beyond the panel

The panel covers the most-used options; kettle has many more config keys
(themes, colors, keybinds, shell command, SSH hosts, triggers, plugins, …).
For those, edit the config file directly — the full reference is in
[CONFIG.md](CONFIG.md). You can jump straight to it from kettle with
**right-click → Preferences ▸ Advanced… (open config in $EDITOR)**.

> **Tip:** for keybinds beyond the curated list (or to unbind a default),
> edit the config file directly (`keybind = ctrl+shift+e = split_right`,
> `keybind = ctrl+shift+e = unbind`) and check your effective bindings any
> time with `kettle --list-keybinds`.

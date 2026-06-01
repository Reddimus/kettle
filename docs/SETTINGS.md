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
| Cursor shape | `cursor-shape` | block · beam · underline |
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

## Beyond the panel

The panel covers the most-used options; kettle has many more config keys
(themes, colors, keybinds, shell command, SSH hosts, triggers, plugins, …).
For those, edit the config file directly — the full reference is in
[CONFIG.md](CONFIG.md). You can jump straight to it from kettle with
**right-click → Preferences ▸ Advanced… (open config in $EDITOR)**.

> **Keybinds:** the settings panel doesn't yet edit keybindings interactively —
> rebind keys in the config file (`keybind = ctrl+shift+e = split_right`) and
> see your effective bindings any time with `kettle --list-keybinds`. An
> interactive keybind editor is on the roadmap.

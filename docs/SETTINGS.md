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

With **`vim-menu-nav`** on (the default), the panel also takes vim keys:
`j`/`k` move between options, `h`/`l` change the highlighted option, `g`/`G`
jump to the first/last option, and `Ctrl+d`/`Ctrl+u` move half a page. The
same scheme works in the right-click context menu and the new-tab dropdown
(`h` closes / pops a submenu, `l` drills in or activates), `y`/`n` answer
confirm dialogs, and text-input overlays with a selection (palette, search's
match stepping, layout picker) use `Ctrl+j`/`Ctrl+k` (or `Ctrl+n`/`Ctrl+p`)
so plain letters keep typing. Turn it off with `vim-menu-nav = false`.

Every change applies **immediately** and is written to your config file (shown
by `kettle --config-path`), so it survives restarts. There's nothing to "save".

## What's in the panel

**Appearance**

| Option | Config key | Notes |
|---|---|---|
| Theme | `theme` | curated list of the most popular themes; ←/→ live-previews each. The full 500+-theme bundle is also reachable via the right-click **Theme** submenu, `NextTheme`/`PrevTheme`, or a `theme =` line in your config |
| Font size | `font-size` | 6–72 pt |
| Background opacity | `background-opacity` | 20–100% (stored as 0.0–1.0) |
| Window padding | `window-padding-x` | 0–40 px |
| Cursor shape | `cursor-style` | block · beam · underline |
| Cursor blink | `cursor-blink` | on / off |
| Show pane titlebars | `show-titlebar` | on / off |
| Background | `background-type` | solid color · image · transparent. The wallpaper *path* stays a config line (`background-image`) — see [BACKGROUNDS.md](BACKGROUNDS.md) |
| Background animation | `background-animation` | when focused · always · off — how an animated wallpaper plays |

**Behavior**

| Option | Config key | Notes |
|---|---|---|
| Scrollbar | `scrollbar` | hidden · auto · always |
| Bell | `bell` | off · visual · attention · both |
| Scrollback lines | `scrollback` | 0–100000 |
| Copy on select | `copy-on-select` | on / off |
| Hide mouse while typing | `mouse-hide-while-typing` | on / off |
| Focus mode | `focus` | click · follows-mouse · system |
| Check for updates | `update-check` | on / off |
| Vim menu navigation | `vim-menu-nav` | on / off — hjkl & friends in menus/overlays (see [Navigating](#navigating)) |

**Graphics**

| Option | Config key | Notes |
|---|---|---|
| GPU preference | `gpu-power-preference` | integrated (power-saving) · discrete (performance) · automatic. **Default: discrete** (renders on the dedicated GPU). `low` gives the fastest cold start on a dual-GPU laptop |
| GPU device | `gpu-device-id` + `gpu-vendor-id` + `gpu-name` | Pin a *specific* detected GPU, or **Automatic**. The list is the GPUs found on this machine |
| GPU backend | `gpu-backend` | automatic · DirectX 12 · Vulkan · Metal · OpenGL |
| Force software rendering | `gpu-force-software` | on / off — debugging fallback (slow) |

A footer line shows the **Active GPU** in use right now. GPU changes take effect
on the **next launch** (the GPU/surface graph can't hot-swap), so the panel shows
a *"⚠ restart kettle to apply"* hint after you change one. This is kettle's
cross-platform answer to the OS GPU picker, and unlike it, it persists per-app.

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

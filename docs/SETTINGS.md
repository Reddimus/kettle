# Settings overlay

kettle has a built-in **Settings** panel so you can change the common options
without editing a config file. Open it with **`Ctrl + ,`** or
**right-click → Settings…**.

## Navigating

| Key | Action |
|---|---|
| ↑ / ↓ | Move between options (skips options that don't apply) |
| ← / → | Change the highlighted option |
| Space / Enter | Toggle / cycle the highlighted option |
| Tab / Shift+Tab | Next / previous category |
| Esc | Close |

The panel is also fully **mouse-driven** (v2.24.0): **left-click** a row to cycle
its value forward, **right-click** to cycle back, **scroll-wheel** over a row to
adjust it, **click a category tab** to switch pages, and **click outside** the
panel to close. (A keybind row starts capture on click; the image-path row opens
an inline text prompt.)

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
| Theme | `theme` | curated list of the most popular themes; ←/→ live-previews each. The full 500+-theme bundle is also reachable via the right-click **Theme** submenu (which now **live-previews on hover** — see [the menu](#beyond-the-panel)), `NextTheme`/`PrevTheme`, or a `theme =` line in your config |
| Font size | `font-size` | 6–72 pt |
| Background opacity | `background-opacity` | 20–100% (stored as 0.0–1.0) |
| Window padding | `window-padding-x` | 0–40 px |
| Cursor shape | `cursor-style` | block · beam · underline |
| Cursor blink | `cursor-blink` | on / off |
| Show pane titlebars | `show-titlebar` | on / off |

**Background** (v2.24.0) — options that don't apply to the chosen type are dimmed
and skipped; the page dims its backdrop so the **live** wallpaper previews around
the panel. See [BACKGROUNDS.md](BACKGROUNDS.md).

| Option | Config key | Notes |
|---|---|---|
| Background | `background-type` | solid color · image · **starfield** (zero-config animated) · transparent |
| Image file | `background-image` | the wallpaper path — **editable inline** here (Enter to open the prompt, type a path, Enter to save). Only for `image` |
| Animation | `background-animation` | always (default) · when focused · off — how a starfield / animated image plays |
| Chrome bar color | `chrome-background` | theme · auto (from wallpaper) · black · white |

**Behavior**

| Option | Config key | Notes |
|---|---|---|
| Scrollbar | `scrollbar` | hidden · auto · always |
| Scrollbar width | `scrollbar-width` | 2–40 px — the overlay scrollbar's thumb/track width |
| Bell | `bell` | off · visual · attention · both |
| Scrollback lines | `scrollback` | 0–100000 |
| Scrollback MB | `scrollback-bytes` | 0–1024 MB; 0 disables the byte cap |
| Copy on select | `copy-on-select` | on / off |
| Hide mouse while typing | `mouse-hide-while-typing` | on / off |
| Focus mode | `focus` | click · follows-mouse · system |
| Updates | `update-policy` | off · notify · install automatically (default: auto) |
| Update check (hours) | `update-check-interval-hours` | 1–720 h — how often the background check runs (default 24 = daily) |
| Vim menu navigation | `vim-menu-nav` | on / off — hjkl & friends in menus/overlays (see [Navigating](#navigating)) |

**Tabs** (v2.28.0)

| Option | Config key | Notes |
|---|---|---|
| Tab bar | `tab-bar` | off · auto (only with >1 tab) · always |
| Tab bar position | `tab-bar-position` | top · bottom (left/right vertical bars are config-only for now) |
| Min tab width | `tab-min-width` | 40–600 px — tabs fill the bar evenly; below this the bar overflows and scrolls |
| Scrollable tab bar | `scroll-tabbar` | on / off — `‹ ›` arrows + wheel scroll when tabs overflow |
| Close button on tabs | `close-button-on-tab` | on / off |
| Detachable tabs | `detachable-tabs` | on / off — drag a tab out into its own window |

**Graphics**

| Option | Config key | Notes |
|---|---|---|
| GPU preference | `gpu-power-preference` | automatic · low power / integrated · high performance. **Default: automatic** (platform/wgpu chooses). Pick `high` only when you want dedicated-GPU render headroom on hybrid hardware |
| GPU device | `gpu-device-id` + `gpu-vendor-id` + `gpu-name` | Pin a *specific* detected GPU, or **Automatic**. The list is the GPUs found on this machine |
| GPU backend | `gpu-backend` | automatic · DirectX 12 · Vulkan · Metal · OpenGL |
| Force software rendering | `gpu-force-software` | on / off — debugging fallback (slow) |

A footer line shows the **Active GPU** in use right now. GPU changes take effect
on the **next launch** (the GPU/surface graph can't hot-swap), so the panel shows
a *"⚠ restart kettle to apply"* hint after you change one. This is kettle's
cross-platform answer to the OS GPU picker, and unlike it, it persists per-app.

**Keybinds** — rebind common actions interactively. Each row shows the chord
currently bound to that action; press **Enter** on a row, then press the new
chord you want (any modifier combination). It binds immediately (replacing the
action's previous chord) and is saved to your config as a `keybind = …` line. Press **Esc** to cancel a capture. Covered
actions: split right/down, close pane, new/next/previous tab, search, command
palette, open settings, zoom pane, copy, paste.

## Beyond the panel

The panel covers the most-used options; kettle has many more config keys
(themes, colors, keybinds, shell command, SSH hosts, triggers, plugins, …).
For those, edit the config file directly — the full reference is in
[CONFIG.md](CONFIG.md). You can jump straight to it from kettle with
**right-click → Preferences ▸ Advanced… (open config in $EDITOR)**.

**Live theme preview (v2.24.0):** in **right-click → Theme**, hovering (or
arrowing over) a theme applies it instantly so you can browse all 500+ themes
live; moving off, pressing Esc, or clicking away reverts to your current theme,
and clicking a theme commits it.

> **Tip:** for keybinds beyond the curated list (or to unbind a default),
> edit the config file directly (`keybind = ctrl+shift+e = split_right`,
> `keybind = ctrl+shift+e = unbind`) and check your effective bindings any
> time with `kettle --list-keybinds`.

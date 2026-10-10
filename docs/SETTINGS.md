# Settings overlay

kettle has a built-in **Settings** panel so you can change the common options
without editing a config file. Open it with **`Ctrl + ,`** or
**right-click → Settings…**.

## Navigating

| Key | Action |
|---|---|
| ↑ / ↓ | Move between options (skips options that don't apply) |
| ← / → | Change the highlighted option |
| Space / Enter | Toggle / cycle the highlighted option; on the Theme row, open the theme picker |
| Tab / Shift+Tab | Next / previous category |
| Esc | Close |

The panel is also fully **mouse-driven**: **left-click** a row to cycle its
value forward, **right-click** to cycle back, **scroll-wheel** over a row to
adjust it, **click a category tab** to switch pages, and **click outside** the
panel to close. (A keybind row starts capture on click, the image-path row opens
an inline text prompt, and the Theme row opens the theme picker.)

With **`vim-menu-nav`** on (the default), the panel also takes vim keys:
`j`/`k` move between options, `h`/`l` change the highlighted option, `g`/`G`
jump to the first/last option, and `Ctrl+d`/`Ctrl+u` move half a page. The
same scheme works in the right-click context menu and the new-tab dropdown,
where `h` closes or pops a submenu and `l` drills in or activates. `y`/`n`
answer confirm dialogs. Text-input overlays with a selection (palette, search,
layout picker) step with `Ctrl+j`/`Ctrl+k` (or `Ctrl+n`/`Ctrl+p`) so plain
letters keep typing. Turn it off with `vim-menu-nav = false`.

Changes are written to your config file, shown by `kettle --config-path`. Most
apply immediately. GPU changes apply after restarting Kettle. Completion changes
apply to new shells. Opacity, background type, and blur may need a new window.
The footer shows dependencies for the focused row and pending changes. Notes
keep reserved space below scrolling fields in short windows.

Labels and display values use sentence case. Theme and GPU names retain their
spelling. Quantities use a space before the unit, for example `13 pt`, `6 px`,
`120 MB`, `24 h`, and `10 s`. Percentages use `99%`. Config tokens do not change.
All categories share a label column with a two-cell gap before values. Narrow
panels shorten labels and values separately with an ellipsis. Category names
have two spaces between them; an underline marks the selected category.

## What's in the panel

**Appearance**

| Option | Config key | Notes |
|---|---|---|
| Theme | `theme` | curated list of the most popular themes; ←/→ and the wheel live-preview each, stepping through the popular themes of the current theme's appearance (dark or light) from one look to the most similar next. Enter, Space or a click opens the [theme picker](#theme-picker), which searches the full 500+-theme bundle. `NextTheme`/`PrevTheme` and a `theme =` line in your config reach every theme too |
| Font size | `font-size` | 6–72 pt |
| Background opacity | `background-opacity` | 20–100% (stored as 0.0–1.0) |
| Window blur | `window-blur` | native backdrop blur where the window system supports it; changing the startup surface requires a new window |
| Window padding | `window-padding-x` | 0–40 px |
| Cursor shape | `cursor-style` | Block · Bar · Underline |
| Cursor blink | `cursor-blink` | On / Off |
| Stop blinking after | `cursor-blink-timeout` | 0–3600 s in 5 s steps; `0` never stops. Dimmed while blink is off |
| Show pane titlebars | `show-titlebar` | On / Off |

**Background** — options that don't apply to the chosen type are dimmed
and skipped; the page dims its backdrop so the **live** wallpaper previews around
the panel. See [BACKGROUNDS.md](BACKGROUNDS.md).

| Option | Config key | Notes |
|---|---|---|
| Background | `background-type` | Solid color · Image · Starfield (animated) · Transparent |
| Image file | `background-image` | the wallpaper path — **editable inline** here (Enter to open the prompt, type a path, Enter to save). Only for `image` |
| Animation | `background-animation` | Always (default) · When focused · Off — how a starfield / animated image plays |
| Interface bar color | `chrome-background` | Theme · Automatic (from wallpaper) · Black · White |

**Behavior**

| Option | Config key | Notes |
|---|---|---|
| Language | `language` | Automatic · English · Español; each language is named in itself. Applies when Kettle restarts, and the footer says so once the choice differs from the running language |
| Scrollbar | `scrollbar` | Hidden · Automatic · Always |
| Completion overlay | `completion-overlay` | Automatic · Off; applies to new shells |
| Scrollbar width | `scrollbar-width` | 2–40 px — the overlay scrollbar's thumb/track width |
| Bell | `bell` | Off · Visual flash · Attention · Visual flash and attention |
| Scrollback lines | `scrollback` | 0–100000 |
| Scrollback memory | `scrollback-bytes` | 0–1024 MB; 0 disables the byte cap |
| Copy on selection | `copy-on-select` | On / Off |
| Hide mouse while typing | `mouse-hide-while-typing` | On / Off |
| Focus mode | `focus` | Click to focus · Follows mouse · System default |
| Updates | `update-policy` | Off · Notify · Install automatically (config default: `auto`) |
| Update check interval | `update-check-interval-hours` | 1–720 h — how often the background check runs (default 24 = daily) |
| Vim menu navigation | `vim-menu-nav` | On / Off — hjkl & friends in menus/overlays (see [Navigating](#navigating)) |

**Search**

| Option | Config key | Notes |
|---|---|---|
| Wrap at boundaries | `search-wrap` | On / Off — when on, Next after the last result wraps to the first (and Previous wraps in reverse) |
| Case mode | `search-case-sensitive` | **Smart** (ignore case until uppercase) · **Match** (always case-sensitive) · **Ignore** (always case-insensitive) |
| Invert default direction | `invert-search` | On / Off — flips `Enter` to backward and `Shift+Enter` to forward; explicit Next / Previous do not change |

These are the same persistent controls shown in the `Ctrl+Shift+F` bottom bar.
The bar also has Previous, Next, and Close controls and a grapheme-aware editor.
It accepts strict Rust regular expressions up to 4096 UTF-8 bytes and reports a
compact state instead of an eager global match count. Valid expressions beyond
the engine-size ceiling show **Pattern too complex**. **Results limited** means
a pathological logical line, output-interrupted explicit navigation, or the
65,536-span nearby projection made ordering uncertain; ordinary work-budget
yields resume without that warning. See
[CONFIG.md#scrollback-search](CONFIG.md#scrollback-search) for shortcuts,
history-scan bounds, and TUI behavior.

**Tabs**

| Option | Config key | Notes |
|---|---|---|
| Tab bar | `tab-bar` | Hidden · Automatic (multiple tabs) · Always |
| Tab bar position | `tab-bar-position` | Top · Bottom (Left/Right vertical bars are config-only for now) |
| Minimum tab width | `tab-min-width` | 40–600 px — tabs fill the bar evenly; below this the bar overflows and scrolls |
| Scrollable tab bar | `scroll-tabbar` | On / Off — `‹ ›` arrows + wheel scroll when tabs overflow |
| Close button on tabs | `close-button-on-tab` | On / Off |
| Detachable tabs | `detachable-tabs` | On / Off — drag a tab out into its own window |

**Graphics**

| Option | Config key | Notes |
|---|---|---|
| GPU preference | `gpu-power-preference` | Automatic · Low power (integrated) · High performance. **Default: Automatic** (platform/wgpu chooses). Pick `high` only when you want dedicated-GPU render headroom on hybrid hardware |
| GPU device | `gpu-device-id` + `gpu-vendor-id` + `gpu-name` | Pin a *specific* detected GPU, or **Automatic**. The list is the GPUs found on this machine |
| GPU backend | `gpu-backend` | Automatic · DirectX 12 · Vulkan · Metal · OpenGL |
| Force software rendering | `gpu-force-software` | On / Off — debugging fallback (slow) |

A footer line shows the **Active GPU** in use right now. GPU changes take effect
on the **next launch** (the GPU/surface graph can't hot-swap), so the panel shows
"Restart Kettle or open a new window to apply pending changes." after an edit
that requires a restart or new window. The pending flag does not record the
cause, so selecting Graphics does not change that wording. GPU edits require a
full restart. The GPU picker persists the selection per application.

**Agents**

| Option | Config key | Notes |
|---|---|---|
| Agent previews | `agent-display` | On / Off — lets AI agents show media in Kettle without reading the screen or typing. Turning it on applies at once, also for agents already connected; turning it off applies when Kettle restarts |
| Codex previews | `agent-display-codex` | On / Off — zsh and fish started in a new pane define `codex`, which starts Codex with Kettle's display server. Needs agent previews; a `codex` you define still wins, and panes already open keep what they started with |
| Add kettle to PATH | `add-kettle-to-path` | Automatic / Off — a new pane on macOS or Linux whose `PATH` has no `kettle` gets this Kettle's folder appended, after your commands. Panes already open keep theirs |
| Claude Code previews | `agent-display-claude-code` | On / Off — Claude Code started in a new pane gets Kettle's plugin, so the images and diagrams it sends show as cards under the call. Needs agent previews; panes already open keep what they started with |

The footer explains the row's effect right now: what turning it on or off does;
that a restart is needed to turn previews off; that a launch option
(`--agent-display off`, or `--agent-server off` alone) keeps them off; that
`agent-server = full` already includes previews; or that Kettle could not start
its agent server. On the Claude Code row it says what the setting applies to,
that agent previews must be on, or why new panes get no plugin: Claude Code's
managed settings forbid plugins from the environment, Kettle is running from
a temporary translocated copy, Kettle could not write or check its plugin, or
the system cannot run the check yet. Agent control itself (`agent-server`)
stays in the config file, because it grants reading and typing and applies at
launch.

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
**right-click → Preferences ▸ Advanced… (open config with default app)**.
The pre-negotiation Enter fallback `modify-other-keys = auto|always|off` is one of
these config-only options; edits still reload immediately for every open pane.

### Theme picker

The theme picker lists every bundled theme. Type part of a name to filter
them, or move with ↑/↓ (and Tab); the window wears the selected theme while
you browse. Enter keeps it and writes it to your config file, and Esc puts
back the theme you started with. With nothing typed, the themes of your
current theme's appearance come first, each followed by the most similar one,
so the arrows never jump between a dark and a light palette until the end of
the list; each row says whether its theme is dark or light, and your current
theme is ticked. Open it from the Settings **Theme** row (Enter or a click),
**right-click → Theme…**, the command palette's **Choose theme…**, or the
bindable `open_theme_picker` action. Opened from Settings, it covers the panel
and returns to it when it closes.

> **Tip:** for keybinds beyond the curated list (or to unbind a default),
> edit the config file directly (`keybind = ctrl+shift+e = split_right`,
> `keybind = ctrl+shift+e = unbind`) and check your effective bindings any
> time with `kettle --list-keybinds`.

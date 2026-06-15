# Configuration reference

kettle uses the **Ghostty `key = value` grammar**: one entry per line, the
first `=` splits key and value, surrounding whitespace is trimmed, only
full-line `#` comments are allowed (a `#` inside a value is part of the value,
so hex colors work), and some keys may repeat.

**Type notes.** `bool` keys accept the standard aliases (case-insensitive):
`true` / `yes` / `on` / `1` / `enabled` / `enable` / `y` for true,
and `false` / `no` / `off` / `0` / `disabled` / `disable` / `n` for false.
Unrecognized values keep the current setting and surface in `--check-config`.
Numeric keys with documented ranges
(`font-size`, `background-opacity`, `unfocused-split-opacity`,
`scroll-multiplier`, `minimum-contrast`, `cursor-blink-interval`) clamp at
parse-time *and* flag out-of-range values via `--check-config`.

Config path: kettle first probes `$XDG_CONFIG_HOME/kettle/config` (honored on
every OS). Failing that, the fallback is per-OS: on Unix/macOS
`~/.config/kettle/config`, and on Windows `%APPDATA%\kettle\config`. On Windows a
stray `HOME` (Git Bash / MSYS / WSL-interop all export one) is **intentionally
ignored** so a Start-menu launch and a shell launch read the same config — set
`XDG_CONFIG_HOME` if you genuinely want `~/.config` on Windows. Always run
`kettle --config-path` for the authoritative resolved location, or
`kettle --check-config` to validate it (resolved settings + any unrecognized
keys). The file is **watched and reloaded live**.

## Keys

| Key | Type | Default | Notes |
|---|---|---|---|
| `theme` | string | `Catppuccin Mocha` | Any bundled theme (`kettle --list-themes`). Runtime theme changes — the Settings picker, the right-click Theme submenu, `next_theme`/`prev_theme`, light/dark toggle — are written back to this line, so a picked theme persists across launches |
| `font-family` | string | `JetBrainsMono Nerd Font` | Bundled; falls back to system fonts |
| `font-family-bold` / `-italic` / `-bold-italic` | string | — | Per-style family overrides (fall back to `font-family`) |
| `font-size` | float | `13` | |
| `text-renderer` | enum | `grid` | `grid` \| `legacy` (v2.25.0). `grid` (default) is cell-locked rendering: every glyph is pinned to its terminal cell (`col × cell_w`), the way Alacritty/kitty/WezTerm/Ghostty render — so fallback-font glyphs (CJK, color emoji, some symbols) and ligatures can't drift off the grid that selection / cursor / mouse hit-testing use. `legacy` restores the pre-2.25.0 continuous layout as a rollback escape hatch. Leave it on `grid` unless you are isolating a renderer regression |
| `background` / `foreground` | color | from theme | Hex/`#rgb`/`rgb:`/X11 name |
| `cursor-color` | color | from theme | The cursor BLOCK color. `cursor-bg-color` (Terminator `cursor_bg_color`) is an alias |
| `cursor-fg-color` | color | from theme | The color of the glyph UNDER the cursor (Terminator `cursor_fg_color`). A focused block cursor renders solid in the block color with the glyph recolored to this — the standard inverted-cursor model |
| `selection-background` / `selection-foreground` | color | from theme | |
| `palette` | `N=#RRGGBB` | from theme | Repeatable, `N` = 0..15 |
| `search-foreground` / `search-background` | color | from theme | Search-match + quick-select highlight. Default derives from the active theme (`search-background` → the theme's yellow `palette[3]`, `search-foreground` → the theme background), so it matches whatever theme is set; override with an explicit color |
| `scrollback` | int / `infinite` | `10000` | Lines of history; `0`, `infinite` or `unlimited` = effectively unbounded |
| `window-padding-x` / `window-padding-y` | float | `8` | Inner padding (px) |
| `background-opacity` | float | `1.0` | 0..1 |
| `cursor-style` | `block`\|`underline`\|`bar` (`beam`) | `block` | `beam` accepted as Alacritty-spelled alias for `bar` |
| `cursor-style-blink` (`cursor-blink`, `cursor_blink`) | bool | `true` | Cursor blinks while the window is focused. The short alias `cursor-blink` is the spelling the right-click Preferences submenu writes back |
| `bell` | `off`\|`visual`\|`attention`\|`both` | `both` | Visual flash and/or window-attention (taskbar/dock urgency) on `BEL` |
| `osc52` (`clipboard`) | `off`\|`copy`\|`paste`\|`both` | `copy` | OSC 52 clipboard policy. `copy` allows programs to set the clipboard but **not** read it (a remote read is a clipboard-exfiltration risk); `paste`/`both` enable read |
| `tab-bar` | `off`\|`auto`\|`always` | `always` | When the tab bar is shown (`auto` = only with >1 tab) |
| `tab-bar-position` (`tab-position`) | `top`\|`bottom`\|`left`\|`right`\|`hidden` | `top` | Where the tab bar sits. `left`/`right` render a **vertical** tab strip (its width is `tab-bar-width`); `hidden` forces the bar off regardless of `tab-bar` |
| `tab-bar-width` | float 40–600 px | `180` | Width of the vertical tab strip when `tab-bar-position = left`/`right`. Clamped `[40, 600]`; ignored for `top`/`bottom` bars |
| `unfocused-split-opacity` | float 0.1–1 | `0.7` | Dim level of unfocused split panes |
| `scroll-multiplier` (`mouse-scroll-multiplier`) | float 0.1–50 | `1.0` | Mouse-wheel scroll-speed multiplier (1.0 ≈ 3 lines/notch) |
| `disable-mousewheel-zoom` | bool | `false` | When `true`, Ctrl+wheel does NOT change the font size. Useful for users who accidentally scroll-zoom on a laptop touchpad. The keyboard IncreaseFontSize / DecreaseFontSize / ResetFontSize chords still work |
| `smart-copy` | bool | `true` | `true` (default + Terminator default): `Action::Copy` preserves the existing clipboard when there's no selection. `false`: clobber the clipboard with empty text on every Ctrl+Shift+C — for users who prefer "Copy means the clipboard now reflects the current selection, even when empty" over the smart heuristic. Distinct from `copy-on-select` (which controls auto-copy when text selection completes) |
| `menu-item` (repeatable, `menu-item = LABEL = CMD`) | string | none | Add a row to the right-click context menu that writes `CMD\n` to the focused pane's PTY when clicked. Repeatable — each `menu-item = …` line appends one row. Use `menu-item = Clear screen = clear`, `menu-item = Open editor = $EDITOR ~/.bashrc`, etc. For richer behavior (Lua callbacks, conditional rows) use `kettle.add_menu_item(label, callback)` from `init.lua` — see [`docs/examples/init.lua`](examples/init.lua) |
| `handle-size` | int -1–50 px | `-1` | Split-divider stroke width. `-1` = use the theme default (1 px). Higher values give a chunkier divider — useful on high-DPI displays where 1 px is hard to see |
| `geometry-hinting` | bool | `false` | When `true`, request that the window manager resize the kettle window in steps that match the font cell grid (so a resize always lands on integral rows/columns instead of mid-cell pixel offsets). Best-effort: respected by X11 + Windows window managers via winit; ignored on Wayland (compositor manages sizing) |
| `focus` | `click`\|`sloppy`\|`system` | `click` | Focus-follows-mouse policy. `click` (default) — focus on click. `sloppy` — focus on cursor movement; pane under the cursor becomes focused without clicking. `system` — kettle treats this as `click` (winit doesn't expose the OS-level focus policy, so the OS-managed mode falls back to explicit-click behavior) |
| `minimum-contrast` | float 0–21 | `0.0` | WCAG 2.0 minimum contrast ratio of cell text against its background; `0` = off. `4.5` ≈ WCAG AA, `7.0` ≈ AAA. Foreground is lifted toward white/black as needed |
| `window-title-format` (`title-format`) | string | `{title} — kettle` | OS window title template — placeholders `{title}` (active pane title), `{cwd}` (active pane cwd), `{tab}` (1-based tab index); `{{`/`}}` escape literal braces |
| `tab-format` (`tab-title-format`) | string | `{n}: {title}` | Per-tab label template — placeholders `{n}` (1-based tab index), `{title}` (focused pane title). The trailing `✕` close button is appended by the renderer |
| `scrollbar` | `never`\|`auto`\|`always` | `auto` | Per-pane scrollback scrollbar (`auto` = only while scrolled) |
| `split-divider-color` | color | theme `palette[8]` | Pane border/divider color for *inactive* panes |
| `focused-split-color` (`split-divider-color-focused`) | color | theme `palette[4]` | Border color for the *focused* pane — the "here am I" accent. While **broadcast mode** is on (`Super+G`), this is temporarily overridden by theme `palette[3]` (yellow) to signal the active state; the configured color is restored when broadcast turns off |
| `cursor-blink-interval` | int ms | `530` | Cursor blink half-period |
| `tab-silence-threshold-ms` (`tab-silence-threshold`) | int ms | `10000` | An inactive tab whose unseen output went quiet for this long transitions from the cyan `Output` dot to the dim `Silent` dot (Terminator's Silence Watcher). Clamped `[1000, 600_000]` |
| `command-notify-threshold-ms` (`command-notify-threshold`) | int ms | `5000` | Minimum command duration before kettle fires a desktop notification when an OSC 133 D (CommandEnd) event arrives **while the window is unfocused**. `0` disables. Requires shell integration (`kettle --shell-integration bash >> ~/.bashrc` or equivalent) — without OSC 133 the shell never emits the event. Clamped `[0, 86_400_000]` (0..1 day). Terminator parity: `command_notify.py` plugin |
| `copy-on-select` | bool | `true` | Auto-copy the selection to the clipboard on release |
| `update-check` (`check-for-updates`) | bool | `true` | Check GitHub at most once/day for a newer kettle release and show a dismissable notification (click the banner to open the release page). Notify-only — kettle never downloads or installs. Never runs on the first launch or in packaged builds; opt out with `false`. A one-shot `kettle --check-update` checks on demand |
| `restore-session` (`restore_session`) | bool | `false` | Reopen the previous session (tabs, splits, working dirs) on launch. **Off by default** — like every mainstream terminal, a new window/instance opens fresh (a single pane in the default cwd). The session is always *saved* on exit only when this is on (or `--restore` is passed), so a fresh window never clobbers a saved layout. `--restore` is the one-shot equivalent; `--layout NAME` restores a named workspace independently |
| `agent-server` | `off`\|`read-only`\|`full` | `off` | The agent control server mode. **Off by default.** When enabled, kettle starts a local-IPC control server that an AI agent / `kettle ctl` / `kettle mcp` can use to read the screen and drive panes (`read-only` reads / lists / subscribes; `full` also sends text + runs commands). Security: local-only — a Unix domain socket (mode `0600`) or a Windows named pipe (current-user DACL); no TCP. `--agent-server <mode>` is the per-launch override. See [docs/AGENT.md](AGENT.md) |
| `agent-badge` | string | `"[agent] "` | The per-pane titlebar prefix shown while an agent connection has the pane attached. Set to any glyph you like (`agent-badge = 🤖 `); empty disables it |
| `scroll-on-keystroke` (`scroll-on-input`) | bool | `true` | Jump back to the bottom when the user types while scrolled back (Alacritty `scrolling.history.scroll_on_input`) |
| `scroll-on-output` | bool | `false` | Jump back to the bottom when new output arrives while scrolled back. Off by default so reading old output isn't interrupted by a chatty background job (Alacritty `scrolling.history.scroll_on_output`) |
| `mouse-hide-while-typing` (`mouse-hide`) | bool | `true` | Hide the OS mouse cursor while the user is typing; re-shown on the next mouse movement (Alacritty `mouse.hide_when_typing`, kitty `hide_mouse_when_typing`) |
| `word-delimiters` (`selection-word-chars`, `semantic-escape-chars`) | string | engine default | Characters that delimit a "word" for double-click selection. Empty = engine default (`,│\`\|:\"' ()[]{}<>\t`). Override to e.g. `()[]{}` to make `/` part of a word so URLs/paths are picked up whole (Alacritty `selection.semantic_escape_chars`) |
| `font-feature` | string | — | OpenType feature(s), repeatable / comma-list. Forms: `liga`, `+calt`, `-liga`, `liga off`, `ss01`, `cv01=2`, `zero 1`. Applied on top of the ligature toggle |
| `command` / `shell` | string | `$SHELL` | Program to launch |
| `ssh-host` | `name=user@host` | — | Repeatable; named target for the `Ctrl+Shift+S` SSH launcher |
| `keybind` | `trigger=action` | Terminator set | Repeatable |
| `accent-color` | `auto` \| `theme` \| color | **`auto`** | The UI-chrome accent — active-tab strip, focused-pane border, per-pane titlebars, drag ghost, settings/menu highlights. **`auto` (the default since v2.18) is Peacock behavior, per *window***: each window claims a distinct hue from the theme's accent pool, seeded by the working directory (same project → same starting hue, stable across launches) and live-deduped against every other kettle window — including other kettle processes — so two open windows never share a hue while the pool has a free one. A theme switch keeps each window's pool slot. **`theme`** (also `off`/`none`) opts out: every window uses the theme's signature accent (Catppuccin Mocha's mauve `#cba6f7`, matching the app icon; `palette[4]` for themes without an `accent`). A `#rrggbb`/`#rgb`/`0xRRGGBB`/X11 color pins one color for every window (skips the dedupe). CLI `--accent COLOR` wins over the config. `palette[3]` broadcast yellow and the cursor are not affected by design |
| `status-bar` (`statusbar`) | `off\|top\|bottom` | `off` | iTerm2 / kitty parity — show a thin strip at the configured edge with `HH:MM:SS UTC · theme · focused pane title`. Disabled by default so the row isn't subtracted from the pane grid unless the user wants it. Aliases: `none` / `false` = off, `on` / `true` = bottom |
| `trigger` | regex \[`:: cmd args`\] | — | iTerm2 parity — repeatable. Each match against PTY output in an unfocused pane fires `window.request_user_attention(Critical)` (Wayland notification counter / X11 WM_HINTS urgency / macOS dock bounce / Windows taskbar flash). 2 s throttle so a build-script error storm pulses once, not 100×. Patterns are the whole value before an optional ` :: ` — alternation like `(BUILD SUCCESSFUL\|FAILED)` survives intact. With ` :: cmd args`, the command is spawned instead (argv form, **no shell**); since v2.20, `{0}`/`{1}`… in the argv substitute the match's capture groups (Terminator `run_cmd_on_match` parity — substitution can only change an argument's *value*, never add arguments) |
| `resize-overlay` (`resize_overlay`) | `always`\|`never`\|`after-first` | `after-first` | v2.20, Ghostty parity — a transient centered `cols×rows` chip while the window is being resized. `after-first` shows it on every resize except the initial window placement; `never` disables |
| `theme-mode` (`theme_mode`) | `explicit`\|`light`\|`dark`\|`auto` (`system`/`follow-system`) | `explicit` | How the active theme is picked. `explicit` uses `theme`; `light`/`dark` force `light-theme`/`dark-theme`; `auto` switches between them on a schedule (see `theme-schedule`) |
| `light-theme` / `dark-theme` | string | — (falls back to `theme`) | The two themes `theme-mode` switches between. Any bundled theme name (`kettle --list-themes`) |
| `theme-schedule` | string | — | Auto light/dark switch for `theme-mode = auto`. Either two `HH:MM <role>` entries (`role` = `dark`/`light`), comma-separated — e.g. `19:00 dark,07:00 light` — or `auto` (aliases `sunrise/sunset`, `solar`) for sunrise/sunset (needs `theme-schedule-lat`/`-long`) |
| `theme-schedule-lat` / `theme-schedule-long` | float | — | Latitude `[-90, 90]` / longitude `[-180, 180]` for `theme-schedule = auto` sunrise/sunset. Out-of-range values are discarded (the schedule stays unset) |
| `allow-bold` | bool | `true` | When `false`, the SGR bold attribute is suppressed — useful on fonts without a bold companion (Terminator `allow_bold`) |
| `bold-is-bright` | bool | `false` | When `true`, bold text using a palette 0–7 color is remapped to the bright 8–15 variant (xterm convention) |
| `clear-select-on-copy` | bool | `false` | When `true`, the selection highlight is cleared right after a copy (some users prefer the selection to disappear once captured) |
| `invert-search` | bool | `false` | When `true`, the in-pane search overlay opens at the bottom instead of the top |
| `search-wrap` | bool | `true` | Terminator parity. When `true` (default), scrollback search wraps around (past the last match → the first). When `false`, Next stops at the last match and Previous at the first |
| `vim-menu-nav` | bool | `true` | Vim-style navigation in kettle's menus/overlays. List overlays (context menu, new-tab dropdown, settings) take `j`/`k` (wrapping), `g`/`G`, `Ctrl+d`/`Ctrl+u` (half page); in the context menu / new-tab dropdown `h` = back/close and `l` = drill in/activate, while in the settings panel `h`/`l` step the highlighted row's value (same as `←`/`→`); confirm dialogs take `y`/`n`; text-input overlays with a selection (palette, search, layout picker) use `Ctrl+j`/`Ctrl+k` (or `Ctrl+n`/`Ctrl+p`) so letters keep typing. Menu mnemonics skip the nav letters while enabled; type-to-search keeps working. `false` restores plain arrow-key navigation |
| `backspace-binding` | `ascii-del`\|`control-h`\|`escape-sequence`\|`auto` | `ascii-del` | Byte(s) the Backspace key sends. `ascii-del` = `0x7f` (the modern default); `control-h` = `0x08` for hosts expecting the old binding |
| `delete-binding` | `ascii-del`\|`control-h`\|`escape-sequence`\|`auto` | `escape-sequence` | Byte(s) the Delete key sends. `escape-sequence` = the standard `CSI 3~` |
| `login-shell` | bool | `false` | Launch the shell as a login shell (`-l`). Ignored for `wsl.exe` (where `-l` means "list distros" — see the WSL note below) |
| `term` | string | `xterm-256color` | The `TERM` value exported to the PTY |
| `colorterm` | string | `truecolor` | The `COLORTERM` value exported to the PTY (advertises 24-bit color) |

### Auto light/dark theme switching

Set `theme-mode = auto` to follow a schedule. Either give an explicit
light window:

```ini
theme-mode     = auto
light-theme    = TokyoNight Day
dark-theme     = TokyoNight Night
theme-schedule = 19:00 dark,07:00 light   # dark at 19:00, light at 07:00
```

…or compute sunrise/sunset from your location:

```ini
theme-mode          = auto
light-theme         = TokyoNight Day
dark-theme          = TokyoNight Night
theme-schedule      = auto
theme-schedule-lat  = 33.77       # e.g. Long Beach, CA
theme-schedule-long = -118.19
```

### Launching WSL / Ubuntu as your shell (Windows)

On Windows you can point kettle at a WSL distribution instead of
PowerShell or `cmd.exe` by setting `command` to the `wsl.exe` launcher:

```ini
# Open your default WSL distro
command = wsl.exe

# Or pick a specific distro
command = wsl.exe -d Ubuntu

# Start in your Linux home directory (not the Windows cwd)
command = wsl.exe -d Ubuntu --cd ~
```

kettle runs `wsl.exe` over ConPTY just like any other shell, so colors,
resizing, UTF-8, and mouse reporting all work. Run `wsl -l -v` in
PowerShell to see your installed distro names.

> **Note:** `login-shell = true` is ignored for `wsl.exe` — passing `-l`
> to `wsl` means "list distributions", which would exit immediately
> instead of opening a shell. To get a Linux *login* shell, ask for it
> inside the distro: `command = wsl.exe -d Ubuntu -- bash -l`.

### Terminator-parity keys (cycles 331-410)

Both kebab-case (kettle convention) and underscore form (Terminator's
own form, e.g. `show_titlebar`) are accepted for every key in this
table. See [`docs/TERMINATOR-AUDIT.md`](TERMINATOR-AUDIT.md) for the
per-key audit against Terminator's source.

| Key | Type | Default | Notes |
|---|---|---|---|
| `window-state` | enum | `normal` | Launch state: `normal` \| `maximise` (`maximize`) \| `fullscreen` \| `hidden`. Honored by winit's `with_maximized` / `with_fullscreen` / `set_visible(false)` |
| `gpu-power-preference` | enum | `auto` | Which GPU policy wgpu uses at startup: `auto` (platform/wgpu chooses — **default**) \| `low` (low-power / usually integrated) \| `high` (high-performance / often discrete). Used as the policy/fallback when no specific GPU is pinned (`gpu-device-id` below). On a dual-GPU laptop `high` may wake the discrete GPU from its low-power state; use it only when you want that render headroom. Single-GPU machines usually resolve all three policies to the same adapter. Also surfaced in Settings → Graphics |
| `gpu-device-id` / `gpu-vendor-id` | hex u32 | `0` / `0` | Pin a *specific* GPU by its PCI device + vendor id (e.g. `0x2191` / `0x10de`). `0`/unset = use `gpu-power-preference`. Easiest set via Settings → Graphics → GPU device; the resolver falls back to the power-preference policy if the pinned GPU is absent (eGPU unplugged, driver swap) so a stale pin never fails startup. **Applies on next launch** |
| `gpu-name` | string | — | Display name of the pinned GPU; also a fallback match if the `(vendor,device)` pair no longer enumerates. Written by the settings picker |
| `gpu-backend` | enum | `auto` | Pin the graphics backend: `auto` \| `dx12` \| `vulkan` \| `metal` \| `gl`. Mainly disambiguates the same GPU exposed under multiple backends on Windows. **Applies on next launch** |
| `gpu-force-software` | bool | `false` | Force wgpu's software/fallback adapter (slow; for debugging GPU-driver issues). **Applies on next launch** |
| `borderless` | bool | `false` | Hide OS chrome (`winit::WindowAttributes::with_decorations(false)`). Useful for tiling WMs |
| `always-on-top` | bool | `false` | Keep window above others (`winit::Window::set_window_level(AlwaysOnTop)`) |
| `hide-on-lose-focus` | bool | `false` | Quake-style auto-hide. Wayland defers to compositor; Linux X11 + macOS + Windows hide directly |
| `show-titlebar` | bool | `true` | Per-pane titlebar; renders only when a tab has >1 pane (a single-pane tab uses the OS window title instead) |
| `title-at-bottom` | bool | `false` | Per-pane titlebar position |
| `title-hide-sizetext` | bool | `false` | Hide the `WxH` size annotation in the titlebar |
| `icon-bell` | bool | `true` | Render a bell glyph in the titlebar when the pane ringed BEL |
| `title-transmit-bg-color` / `-fg-color` | color | `#c80003` / `#ffffff` | Focused-pane (broadcast-source) titlebar colors |
| `title-receive-bg-color` / `-fg-color` | color | `#0076c9` / `#ffffff` | Broadcast-group-member titlebar colors |
| `title-inactive-bg-color` / `-fg-color` | color | `#c0bebf` / `#000000` | Idle-pane titlebar colors |
| `background-type` | enum | `solid` | `solid` \| `transparent` \| `image` \| `starfield` (v2.24.0 — a zero-config procedural GPU starfield; needs no `background-image`. A FIXED built-in example: its look is baked in, not tunable). Surfaced in Settings → Background. See **[BACKGROUNDS.md](BACKGROUNDS.md)** |
| `background-image` | path | — | Wallpaper image (for `background-type = image`). Supports PNG/JPEG/WebP/BMP/GIF, **animated GIF / APNG / animated WebP** (plays as a moving background — see `background-animation`). Tilde expansion supported. Editable inline in Settings → Background. Curated sources in **[BACKGROUNDS.md](BACKGROUNDS.md)** |
| `chrome-background` | enum | `theme` | When a wallpaper (`image` or `starfield`) is set, the opaque fill of the window chrome strips (tab bar, status bar) so the background never bleeds through them: `theme` (the theme's chrome color — default) \| `auto` (the background's average color, kept readable under the tab text; black over the starfield) \| `black` \| `white`. No effect without a wallpaper |
| `background-animation` | enum | `always` | How an animated background (a `starfield` or an animated `background-image`) plays: `always` (`on`/`true`, the v2.24.0 default — animate even when unfocused; still freezes when the window is minimized/occluded) \| `when-focused` (`focused`, animate only while focused, zero idle otherwise — battery-friendly) \| `off` (`static`/`false`, freeze on first frame). Surfaced in Settings → Background |
| `background-image-mode` | enum | `stretch_and_fill` | `stretch_and_fill` \| `tile` \| `center` \| `scale` (aspect-preserving fit) |
| `background-image-align-horiz` | enum | `center` | `left` \| `center` \| `right` (applies to `center` + `scale` modes) |
| `background-image-align-vert` | enum | `middle` | `top` \| `middle` \| `bottom` |
| `background-blur` | bool | `false` | CPU-side 3-pass separable box blur at decode (approximates Gaussian) |
| `background-darkness` | float 0..1 | `0.5` | Compose tint over the image (`1.0` = no tint, `0.0` = fully dark; default `0.5` = 50% tint, matching Terminator's `background_darkness`) |
| `exit-action` | enum | `close` | What happens when the shell exits: `close` (default) \| `hold` (keep dead-pane visible) \| `restart` (re-spawn shell — spawns the same argv + cwd in a new tab, deduped so alacritty's `Exit` + `ChildExit` emit pair counts once) |
| `force-no-bell` | bool | `false` | Terminator `force_no_bell` parity. Silences EVERY bell flavor regardless of the `bell` mode — visual flash, audible (none today), window-attention, and the `tab_bar.bell` activity dot. Use when running in a meeting / library / next-to-a-baby setup |
| `visible-bell` / `urgent-bell` | bool / bool | `—` | Terminator compat aliases for the unified `bell` key. Terminator splits the bell into two orthogonal bools; kettle's `bell = both` is `visible_bell + urgent_bell`, `bell = visual` is `visible_bell` alone, `bell = attention` is `urgent_bell` alone. The two arms compose at end-of-parse so file order doesn't matter. **Precedence:** if you set the canonical `bell = …` key explicitly, the Terminator aliases are ignored — canonical wins over alias on hybrid configs |
| `log-strip-ansi` | bool | `false` | Strip ANSI escape sequences from the per-pane session log (`Action::ToggleSessionLog`) before writing. `true` → log is plain-text (CSI / OSC / single-char ESC all stripped); `false` → raw stream is preserved (`cat`-replayable in a terminal) |
| `light-theme` / `dark-theme` | theme name | `""` (falls back to `theme`) | See the `light-theme` / `dark-theme` row in the **Keys** table above — same fields. Terminator `auto_theme` parity: `Action::ToggleLightDark` swaps between the two (case-insensitive bundled-name lookup; an empty value no-ops that side of the swap and falls back to `theme`) |
| `search-case-sensitive` | enum | `smart` | Terminator `case_sensitive` parity. Scrollback-search case-sensitivity. `smart` (default; ripgrep/vim: case-insensitive until any uppercase), `always` / `sensitive` (force sensitive even for lowercase patterns — matches Terminator's default), `never` / `insensitive` (force insensitive even for mixed-case). The Terminator-spelled `case-sensitive = true/false` is also accepted (`true` ⇒ always, `false` ⇒ never) |
| `link-single-click` | bool | `false` | Single-click opens URLs (default needs `Ctrl`/`Cmd`+click) |
| `disable-mouse-paste` | bool | `false` | Block middle-click paste |
| `putty-paste-style` | bool | `false` | Right-click pastes (PuTTY convention) |
| `close-button-on-tab` | bool | `true` | Show `✕` on tab segments |
| `new-tab-after-current-tab` | bool | `false` | Insert vs append behavior when creating a new tab |
| `lua-sandbox` | enum | `safe` | Lua plugin trust mode: `safe` (default) nils `os.execute` / `os.exit` / `io.open` / `io.popen` etc; `trusted` enables full stdlib. See [`docs/examples/init.lua`](examples/init.lua) for the `kettle.*` Lua API surface (URL handlers, event hooks, menu items) with Launchpad / APT URL handlers ported from Terminator's `url_handlers.py` |

### Terminator-parity config keys by disposition

The Terminator config grammar has a few dozen keys kettle's parser
accepts (so copying a Terminator config to kettle doesn't error
on unknown keys) but where the runtime behavior differs from
Terminator's. Each key falls into one of three buckets:

#### Effectively wired — kettle's behavior already matches the documented setting

| Key | Why it's a "no-op" but works |
|---|---|
| `detachable-tabs` | Terminator: a toggle to enable cross-window tab drag. kettle: cross-window detach drag is always available via `Action::MoveTabToNewWindow` + the drag-state machine in `crates/kettle-ui/src/detach.rs`. The config toggle isn't read, but the feature it gates is on by default |
| `homogeneous-tabbar` | Terminator: a toggle for equal-width tab segments. kettle: tab bar ALWAYS tiles segments equally (single-source-of-truth: `app.rs::tab_bar::seg_w = strip / n`). Setting the toggle has no effect because there's no inhomogeneous mode to disable |
| `sticky` (X11 _NET_WM_STATE_STICKY) | **macOS is fully wired** — `sticky = true` calls `app.rs::set_visible_on_all_spaces`, which sets `NSWindowCollectionBehavior::CanJoinAllSpaces \| Stationary` on the underlying NSWindow, pinning the window to every Space. **X11 / Wayland are not wired** — winit 0.30 exposes no portable "stick to all workspaces" API (the X11 `_NET_WM_STATE_STICKY` hint has no winit binding and Wayland has no equivalent), so on those platforms the key is logged (not silently dropped) and `always-on-top` is the closest available cross-platform substitute |
| `inactive-color-offset` | Kettle's `unfocused-split-opacity` is the implemented variant. The exact math differs (Terminator: two separate fg + bg offsets; kettle: single opacity blend), but the user-visible effect — dim unfocused panes — is equivalent |

#### Won't implement — by-design divergence from Terminator

| Key | Rationale |
|---|---|
| `cursor-color-default` | Terminator's two-key design (`cursor-color = X` + `cursor-color-default = true` overrides to ignore the X) is confusing. kettle's design: set `cursor-color = …` to override, REMOVE the line to revert to theme — no separate boolean needed |
| `http-proxy` | The kettle binary makes no HTTP requests, so a proxy setting is meaningless. (The install scripts `install-online.sh` use system curl — kettle the binary itself never fetches HTTP) |
| `broadcast-default` | Was previously mis-mapped to "startup broadcast state" — kettle no longer starts with broadcast on by default. Terminator's intent is "scope when broadcast IS on (all / group / off)" which presupposes named groups; kettle's per-tab broadcast model doesn't have group scoping today (Bucket D, see `docs/TERMINATOR-AUDIT.md`) |
| `putty-paste-style-source-clipboard` | Companion to `putty-paste-style` (right-click pastes); meaningful only when kettle wires `putty-paste-style` itself. Kettle currently surfaces right-click as the context menu — wiring putty-style would be a Bucket-C task that this companion key follows |

#### Genuine future work — parsed for forward-compat

The remaining keys parse cleanly but are not yet wired. A future cycle wiring any of them moves the row into the main `Terminator-parity keys` table above; the parser arm doesn't change.

| Key | What Terminator does with it | Why it's future-work |
|---|---|---|
| `ask-before-closing` | Close-confirmation dialog (always / multiple-terminals / never) | Needs modal dialog primitive |
| `always-split-with-profile` | New splits inherit the parent pane's profile | Needs the profile concept formalized first |
| `autoclean-groups` | Auto-remove empty broadcast groups | Needs named broadcast groups (Bucket D) |
| `cell-width` / `cell-height` | Font cell-grid pixel size overrides | Render-layer font-metric override |
| `extra-styling` | Render bold/italic with styled-font features even when palette lacks variants | Render glyph-attribute change |
| `hide-from-taskbar` | Suppress from OS taskbar | winit Windows-only natively; cross-platform requires per-platform extensions |
| `scroll-tabbar` | Horizontal-scroll across many tab segments | Needs scrollable tab-bar UI for many-tab cases |
| `split-to-group` | New splits join the parent's broadcast group | Needs named broadcast groups (Bucket D) |
| `title-font` / `title-use-system-font` / `use-system-font` / `use-theme-colors` | Per-pane titlebar font + theme-color overrides | Multi-cycle per-pane font system |

## Editing the config from inside kettle (Preferences submenu)

Most of the keys above can be toggled at runtime via right-click → **Preferences ▸**.
The submenu surfaces five common toggles + an `Advanced…` row that opens the
config file in `$EDITOR` for everything else:

| Submenu row | Config key written |
|---|---|
| Scrollbar (radio: always/auto/hidden) | `scrollbar` |
| Cursor blink (✓) | `cursor-blink` |
| Copy on select (✓) | `copy-on-select` |
| Bell (radio: off/visual/attention/both) | `bell` |
| Mouse-hide while typing (✓) | `mouse-hide-while-typing` |
| Font size + / − | (live-only; `font-size` not auto-persisted yet) |
| Advanced… | opens `~/.config/kettle/config` in `$EDITOR` |

Each click both mutates the running `Config` (the change takes effect
immediately) and atomically rewrites the matching line in the config file via
the `kettle_config::persist_config_toggle` helper. The atomic write preserves every
existing comment, blank line, and key order byte-for-byte — only the targeted
`key = value` line is replaced (or appended if it doesn't exist yet).

On the first toggle in any session, kettle saves a snapshot of the pre-edit
file at `~/.config/kettle/config.bak` so you can roll back to your hand-edited
state.

## Keybind grammar

`trigger` = `+`-joined modifiers and one key. Recognized modifier names:

- `shift`
- `ctrl` / `control`
- `alt` / `opt` / `option`
- `super` / `cmd` / `command` / `win` / `windows` / `meta` / `logo` —
  all aliases for the same Super-key bit, so a chord copied from a
  macOS / Windows / Linux config works without renaming.

Keys: any single printable character is a valid key — letters `a`..`z`,
**digits** `0`..`9` (e.g. `alt+1`..`alt+9` for `goto_tab:N`), and **punctuation**
(e.g. `ctrl+,` for `open_settings`). Plus the named keys: `f1`..`f12`,
`up`/`down`/`left`/`right`, `page_up`/`page_down` (aliases `pageup`/`pagedown`,
`prior`/`next`), `home`/`end`, `enter` (alias `return`), `tab`, and the symbolic
names `plus`/`minus`/`equal` for `+`/`-`/`=`.

A typo'd modifier (`cttrl+t`, `supre+t`) is rejected outright and
flagged by `kettle --check-config` — it doesn't silently degrade
into a bare-key binding.

`action` is one of:

**Tabs**: `new_tab`, `close_tab`, `next_tab`, `previous_tab`,
`move_tab_left`, `move_tab_right`, `goto_tab:N` (1-based, N is the tab
number — `goto_tab:1` is the first tab), `new_tab_shell_N` (1-based —
open the Nth entry of the new-tab `▾` dropdown; `Ctrl+Shift+1..9` by
default, Windows Terminal's profile shortcuts), `undo_close_tab` (also
`reopen_tab` / `restore_tab` — restore the most recently-closed tab
from a bounded LIFO ring of 10), `duplicate_tab` (clone the focused
pane's argv + cwd into a new tab — `ssh prod` clones to a second
`ssh prod`).

**Splits**: `new_split:right` (also `split_right` / `split_vert`),
`new_split:down` (also `split_down` / `split_horiz`), `split_auto`
(pick by aspect ratio), `close_pane` (also `close_surface` /
`close_term`), `duplicate_pane` (clone the focused pane's argv + cwd
into a right-side split).

**Focus + resize**: `focus_next`, `focus_prev`,
`goto_split:{up,down,left,right}`, `resize_{up,down,left,right}`,
`toggle_zoom` (also `toggle_split_zoom`), `rotate_cw` / `rotate_ccw`
(rotate the split layout).

**Window**: `new_window` (opens another window **in this process** since
v2.18 — tabs can move live between windows), `close_window`,
`toggle_fullscreen`, `move_tab_to_new_window` (tear the focused tab out
into its own window LIVE — running programs keep running; dragging a tab
past the tab bar does the same with the mouse, Chromium-style since
v2.19: the live window rides the pointer, and dropping it on another
kettle window's tab bar merges it there), `open_settings`
(`settings` — the Ctrl+, overlay), `layout_picker`, `about` (also
`show_about` — version, update status, GitHub link), `screenshot`
(`take_screenshot` / `terminalshot`).

**Editing**: `copy` (`copy_to_clipboard`), `paste`
(`paste_from_clipboard`).

**Search + jump**: `start_search` (`search`), `prev_prompt`
(`jump_to_prompt_prev`), `next_prompt` (`jump_to_prompt_next`).

**Scrollback**: `scroll_line_up`, `scroll_line_down`, `scroll_page_up`,
`scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`,
`clear_history` (also `clear_scrollback` / `clear_buffer` — wipes
scrollback only; keep the visible screen unlike `reset`).

**Broadcast / group input**: `broadcast_all` (`group_all`),
`broadcast_off` (`ungroup_all`), `group_tab` (broadcast to every pane in the
focused tab), `broadcast_group` (type to every pane in the focused pane's
named group).

**Font**: `increase_font_size` (`zoom_in`), `decrease_font_size`
(`zoom_out`), `reset_font_size` (`zoom_normal`).

**Themes**: `next_theme`, `prev_theme` (`previous_theme`), `toggle_light_dark`
(swap between `light-theme` and `dark-theme`).

**Titles**: `edit_window_title`, `edit_tab_title`, `edit_pane_title` (open the
inline rename overlay for the OS window / active tab / focused pane).

**Modals + UI**: `command_palette` (`palette`), `hint_mode` (`hints` /
`quick_select`), `new_ssh` (`ssh`), `context_menu`
(`open_context_menu` — the right-click menu — mouse-only by default
but bindable to a keyboard trigger if you want the menu opened at the
cursor position).

**Vi-mode**: `toggle_vi_mode` (`vi_mode` / `vi`) — enter keyboard-driven
copy/navigation mode (default `Ctrl+Shift+Space`): `h`/`j`/`k`/`l` move,
`0`/`$`/`g`/`G`/`H`/`M`/`L` jump, `v` starts a visual selection, `y` yanks it to
the clipboard, `Esc` exits. See `man kettle` for the full keymap.

**Misc**: `reset` (RIS — full terminal reset including engine state),
`reload_config`, `detach_tab` (Unix-only cross-window tab tear-off).

> This list covers the common actions. For the **complete, always-current**
> set of bindable action names (and every accepted alias), run
> `kettle --list-actions` — it prints straight from the parser, so it can't
> drift from the build.

The action `unbind` (also `none`, `null`, `false`, or an empty string) removes
the default binding for that trigger — useful when a default like
`Ctrl+Shift+C` collides with a chord your shell or another tool wants.
Example: `keybind = ctrl+shift+c=unbind`.

See [`kettle.example.config`](kettle.example.config).

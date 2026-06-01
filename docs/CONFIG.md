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

Config path: `$XDG_CONFIG_HOME/kettle/config` (Linux), the `~/.config`
fallback, or `%APPDATA%\kettle\config` on Windows. Run `kettle --config-path`
to print it, or `kettle --check-config` to validate it (resolved settings +
any unrecognized keys). The file is **watched and reloaded live**.

## Keys

| Key | Type | Default | Notes |
|---|---|---|---|
| `theme` | string | `TokyoNight Night` | Any bundled theme (`kettle --list-themes`) |
| `font-family` | string | `JetBrainsMono Nerd Font` | Bundled; falls back to system fonts |
| `font-family-bold` / `-italic` / `-bold-italic` | string | — | Per-style family overrides (fall back to `font-family`) |
| `font-size` | float | `13` | |
| `background` / `foreground` | color | from theme | Hex/`#rgb`/`rgb:`/X11 name |
| `cursor-color` | color | from theme | |
| `selection-background` / `selection-foreground` | color | from theme | |
| `palette` | `N=#RRGGBB` | from theme | Repeatable, `N` = 0..15 |
| `search-foreground` / `search-background` | color | amber on dark | Search highlight |
| `scrollback` | int / `infinite` | `10000` | Lines of history; `0`, `infinite` or `unlimited` = effectively unbounded |
| `window-padding-x` / `window-padding-y` | float | `8` | Inner padding (px) |
| `background-opacity` | float | `1.0` | 0..1 |
| `cursor-style` | `block`\|`underline`\|`bar` (`beam`) | `block` | `beam` accepted as Alacritty-spelled alias for `bar` |
| `cursor-style-blink` (`cursor-blink`, `cursor_blink`) | bool | `true` | Cursor blinks while the window is focused. The short alias `cursor-blink` is the spelling the right-click Preferences submenu writes back |
| `bell` | `off`\|`visual`\|`attention`\|`both` | `both` | Visual flash and/or window-attention (taskbar/dock urgency) on `BEL` |
| `osc52` (`clipboard`) | `off`\|`copy`\|`paste`\|`both` | `copy` | OSC 52 clipboard policy. `copy` allows programs to set the clipboard but **not** read it (a remote read is a clipboard-exfiltration risk); `paste`/`both` enable read |
| `tab-bar` | `off`\|`auto`\|`always` | `always` | When the tab bar is shown (`auto` = only with >1 tab) |
| `tab-bar-position` | `top`\|`bottom` | `top` | Where the tab bar sits |
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
| `focused-split-color` (`split-divider-color-focused`) | color | theme `palette[4]` | Border color for the *focused* pane — the "here am I" accent. While **broadcast mode** is on (`Ctrl+Shift+G`), this is temporarily overridden by theme `palette[3]` (yellow) to signal the active state; the configured color is restored when broadcast turns off |
| `cursor-blink-interval` | int ms | `530` | Cursor blink half-period |
| `tab-silence-threshold-ms` (`tab-silence-threshold`) | int ms | `10000` | An inactive tab whose unseen output went quiet for this long transitions from the cyan `Output` dot to the dim `Silent` dot (Terminator's Silence Watcher). Clamped `[1000, 600_000]` |
| `command-notify-threshold-ms` (`command-notify-threshold`) | int ms | `5000` | Minimum command duration before kettle fires a desktop notification when an OSC 133 D (CommandEnd) event arrives **while the window is unfocused**. `0` disables. Requires shell integration (`kettle --shell-integration bash >> ~/.bashrc` or equivalent) — without OSC 133 the shell never emits the event. Clamped `[0, 86_400_000]` (0..1 day). Terminator parity: `command_notify.py` plugin |
| `copy-on-select` | bool | `true` | Auto-copy the selection to the clipboard on release |
| `scroll-on-keystroke` (`scroll-on-input`) | bool | `true` | Jump back to the bottom when the user types while scrolled back (Alacritty `scrolling.history.scroll_on_input`) |
| `scroll-on-output` | bool | `false` | Jump back to the bottom when new output arrives while scrolled back. Off by default so reading old output isn't interrupted by a chatty background job (Alacritty `scrolling.history.scroll_on_output`) |
| `mouse-hide-while-typing` (`mouse-hide`) | bool | `true` | Hide the OS mouse cursor while the user is typing; re-shown on the next mouse movement (Alacritty `mouse.hide_when_typing`, kitty `hide_mouse_when_typing`) |
| `word-delimiters` (`selection-word-chars`, `semantic-escape-chars`) | string | engine default | Characters that delimit a "word" for double-click selection. Empty = engine default (`,│\`\|:\"' ()[]{}<>\t`). Override to e.g. `()[]{}` to make `/` part of a word so URLs/paths are picked up whole (Alacritty `selection.semantic_escape_chars`) |
| `font-feature` | string | — | OpenType feature(s), repeatable / comma-list. Forms: `liga`, `+calt`, `-liga`, `liga off`, `ss01`, `cv01=2`, `zero 1`. Applied on top of the ligature toggle |
| `command` / `shell` | string | `$SHELL` | Program to launch |
| `ssh-host` | `name=user@host` | — | Repeatable; named target for the `Ctrl+Shift+S` SSH launcher |
| `keybind` | `trigger=action` | Terminator set | Repeatable |
| `accent-color` | color | — | Peacock parity — when set, overrides the active tab segment's accent strip, focused pane border, and dragged-tab ghost so multi-window kettle setups are visually distinguishable. CLI override `--accent COLOR` wins over the config value. Accepts `#rrggbb`, `#rgb`, `0xRRGGBB`, X11 color names. `palette[3]` broadcast yellow and the cursor are not affected by design |
| `status-bar` (`statusbar`) | `off\|top\|bottom` | `off` | iTerm2 / kitty parity — show a thin strip at the configured edge with `HH:MM:SS UTC · theme · focused pane title`. Disabled by default so the row isn't subtracted from the pane grid unless the user wants it. Aliases: `none` / `false` = off, `on` / `true` = bottom |
| `trigger` | regex | — | iTerm2 parity — repeatable. Each match against PTY output in an unfocused pane fires `window.request_user_attention(Critical)` (Wayland notification counter / X11 WM_HINTS urgency / macOS dock bounce / Windows taskbar flash). 2 s throttle so a build-script error storm pulses once, not 100×. Patterns are the whole value — no `\|action` split, so alternation patterns like `(BUILD SUCCESSFUL\|FAILED)` survive intact |

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
| `borderless` | bool | `false` | Hide OS chrome (`winit::WindowAttributes::with_decorations(false)`). Useful for tiling WMs |
| `always-on-top` | bool | `false` | Keep window above others (`winit::Window::set_window_level(AlwaysOnTop)`) |
| `hide-on-lose-focus` | bool | `false` | Quake-style auto-hide. Wayland defers to compositor; Linux X11 + macOS + Windows hide directly |
| `show-titlebar` | bool | `true` | Per-pane titlebar; renders only when a tab has >1 pane (a single-pane tab uses the OS window title instead) |
| `title-at-bottom` | bool | `false` | Per-pane titlebar position |
| `title-hide-sizetext` | bool | `false` | Hide the `WxH` size annotation in the titlebar |
| `icon-bell` | bool | `false` | Render a bell glyph in the titlebar when the pane ringed BEL |
| `title-transmit-bg-color` / `-fg-color` | color | `#c80003` / `#ffffff` | Focused-pane (broadcast-source) titlebar colors |
| `title-receive-bg-color` / `-fg-color` | color | `#0076c9` / `#ffffff` | Broadcast-group-member titlebar colors |
| `title-inactive-bg-color` / `-fg-color` | color | `#c0bebf` / `#000000` | Idle-pane titlebar colors |
| `background-type` | enum | `solid` | `solid` \| `transparent` \| `image` |
| `background-image` | path | — | Wallpaper image. Supports PNG/JPEG/WebP/BMP/GIF. Tilde expansion supported |
| `background-image-mode` | enum | `stretch_and_fill` | `stretch_and_fill` \| `tile` \| `center` \| `scale` (aspect-preserving fit) |
| `background-image-align-horiz` | enum | `center` | `left` \| `center` \| `right` (applies to `center` + `scale` modes) |
| `background-image-align-vert` | enum | `middle` | `top` \| `middle` \| `bottom` |
| `background-blur` | bool | `false` | CPU-side 3-pass separable box blur at decode (approximates Gaussian) |
| `background-darkness` | float 0..1 | `0.5` | Compose tint over the image (`1.0` = no tint, `0.0` = fully dark; default `0.5` = 50% tint, matching Terminator's `background_darkness`) |
| `exit-action` | enum | `close` | What happens when the shell exits: `close` (default) \| `hold` (keep dead-pane visible) \| `restart` (re-spawn shell — spawns the same argv + cwd in a new tab, deduped so alacritty's `Exit` + `ChildExit` emit pair counts once) |
| `force-no-bell` | bool | `false` | Terminator `force_no_bell` parity. Silences EVERY bell flavor regardless of the `bell` mode — visual flash, audible (none today), window-attention, and the `tab_bar.bell` activity dot. Use when running in a meeting / library / next-to-a-baby setup |
| `visible-bell` / `urgent-bell` | bool / bool | `—` | Terminator compat aliases for the unified `bell` key. Terminator splits the bell into two orthogonal bools; kettle's `bell = both` is `visible_bell + urgent_bell`, `bell = visual` is `visible_bell` alone, `bell = attention` is `urgent_bell` alone. The two arms compose at end-of-parse so file order doesn't matter. **Precedence:** if you set the canonical `bell = …` key explicitly, the Terminator aliases are ignored — canonical wins over alias on hybrid configs |
| `log-strip-ansi` | bool | `false` | Strip ANSI escape sequences from the per-pane session log (`Action::ToggleSessionLog`) before writing. `true` → log is plain-text (CSI / OSC / single-char ESC all stripped); `false` → raw stream is preserved (`cat`-replayable in a terminal) |
| `light-theme` | theme name | `""` | Terminator `auto_theme` parity. Theme that `Action::ToggleLightDark` switches **to** when leaving the dark variant (and stays-on when no chord yet). Empty = action no-ops on the light side. Case-insensitive bundled-name lookup (stored as the canonical bundle name when matched, otherwise stored verbatim trimmed) |
| `dark-theme` | theme name | `""` | Terminator `auto_theme` parity. Theme that `Action::ToggleLightDark` switches **to** when leaving the light variant — and the default landing when current is a third-party theme. Empty = action no-ops on the dark side |
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
| `sticky` (X11 _NET_WM_STATE_STICKY) | Kettle's `always-on-top` is the closest cross-platform variant. winit doesn't expose "stick to all workspaces" portably (X11 hint only, no Wayland/macOS equivalent) — the kettle-style "above other windows" maps to a single config key that works everywhere |
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

Keys: `a`..`z`, `f1`..`f12`, `up`/`down`/`left`/`right`,
`page_up`/`page_down`, `home`/`end`, `enter`, `tab`, `plus`/`minus`/`equal`.

A typo'd modifier (`cttrl+t`, `supre+t`) is rejected outright and
flagged by `kettle --check-config` — it doesn't silently degrade
into a bare-key binding.

`action` is one of:

**Tabs**: `new_tab`, `close_tab`, `next_tab`, `previous_tab`,
`move_tab_left`, `move_tab_right`, `goto_tab:N` (1-based, N is the tab
number — `goto_tab:1` is the first tab), `undo_close_tab` (also
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
`toggle_zoom` (also `toggle_split_zoom`).

**Window**: `new_window`, `close_window`, `toggle_fullscreen`.

**Editing**: `copy` (`copy_to_clipboard`), `paste`
(`paste_from_clipboard`).

**Search + jump**: `start_search` (`search`), `prev_prompt`
(`jump_to_prompt_prev`), `next_prompt` (`jump_to_prompt_next`).

**Scrollback**: `scroll_line_up`, `scroll_line_down`, `scroll_page_up`,
`scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`,
`clear_history` (also `clear_scrollback` / `clear_buffer` — wipes
scrollback only; keep the visible screen unlike `reset`).

**Broadcast / group input**: `broadcast_all` (`group_all`),
`broadcast_off` (`ungroup_all`).

**Font**: `increase_font_size` (`zoom_in`), `decrease_font_size`
(`zoom_out`), `reset_font_size` (`zoom_normal`).

**Themes**: `next_theme`, `prev_theme` (`previous_theme`).

**Modals + UI**: `command_palette` (`palette`), `hint_mode` (`hints` /
`quick_select`), `new_ssh` (`ssh`), `context_menu`
(`open_context_menu` — the right-click menu — mouse-only by default
but bindable to a keyboard trigger if you want the menu opened at the
cursor position).

**Misc**: `reset` (RIS — full terminal reset including engine state),
`reload_config`.

The action `unbind` (also `none`, `null`, `false`, or an empty string) removes
the default binding for that trigger — useful when a default like
`Ctrl+Shift+C` collides with a chord your shell or another tool wants.
Example: `keybind = ctrl+shift+c=unbind`.

See [`kettle.example.config`](kettle.example.config).

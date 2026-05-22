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
| `cursor-style-blink` | bool | `true` | |
| `bell` | `off`\|`visual`\|`attention`\|`both` | `both` | Visual flash and/or window-attention (taskbar/dock urgency) on `BEL` |
| `osc52` (`clipboard`) | `off`\|`copy`\|`paste`\|`both` | `copy` | OSC 52 clipboard policy. `copy` allows programs to set the clipboard but **not** read it (a remote read is a clipboard-exfiltration risk); `paste`/`both` enable read |
| `tab-bar` | `off`\|`auto`\|`always` | `always` | When the tab bar is shown (`auto` = only with >1 tab) |
| `tab-bar-position` | `top`\|`bottom` | `top` | Where the tab bar sits |
| `unfocused-split-opacity` | float 0.1–1 | `0.7` | Dim level of unfocused split panes |
| `scroll-multiplier` (`mouse-scroll-multiplier`) | float 0.1–50 | `1.0` | Mouse-wheel scroll-speed multiplier (1.0 ≈ 3 lines/notch) |
| `minimum-contrast` | float 0–21 | `0.0` | WCAG 2.0 minimum contrast ratio of cell text against its background; `0` = off. `4.5` ≈ WCAG AA, `7.0` ≈ AAA. Foreground is lifted toward white/black as needed |
| `window-title-format` (`title-format`) | string | `{title} — kettle` | OS window title template — placeholders `{title}` (active pane title), `{cwd}` (active pane cwd), `{tab}` (1-based tab index); `{{`/`}}` escape literal braces |
| `tab-format` (`tab-title-format`) | string | `{n}: {title}` | Per-tab label template — placeholders `{n}` (1-based tab index), `{title}` (focused pane title). The trailing `✕` close button is appended by the renderer |
| `scrollbar` | `never`\|`auto`\|`always` | `auto` | Per-pane scrollback scrollbar (`auto` = only while scrolled) |
| `split-divider-color` | color | theme `palette[8]` | Pane border/divider color for *inactive* panes |
| `focused-split-color` (`split-divider-color-focused`) | color | theme `palette[4]` | Border color for the *focused* pane — the "here am I" accent. While **broadcast mode** is on (`Ctrl+Shift+G`), this is temporarily overridden by theme `palette[3]` (yellow) to signal the active state; the configured color is restored when broadcast turns off |
| `cursor-blink-interval` | int ms | `530` | Cursor blink half-period |
| `tab-silence-threshold-ms` (`tab-silence-threshold`) | int ms | `10000` | An inactive tab whose unseen output went quiet for this long transitions from the cyan `Output` dot to the dim `Silent` dot (Terminator's Silence Watcher). Clamped `[1000, 600_000]` |
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
| `background-darkness` | float 0..1 | `1.0` | Compose tint over the image (`1.0` = no tint, `0.0` = fully dark) |
| `exit-action` | enum | `close` | What happens when the shell exits: `close` (default) \| `hold` (keep dead-pane visible) \| `restart` (re-spawn shell) |
| `link-single-click` | bool | `false` | Single-click opens URLs (default needs `Ctrl`/`Cmd`+click) |
| `disable-mouse-paste` | bool | `false` | Block middle-click paste |
| `putty-paste-style` | bool | `false` | Right-click pastes (PuTTY convention) |
| `close-button-on-tab` | bool | `true` | Show `✕` on tab segments |
| `new-tab-after-current-tab` | bool | `false` | Insert vs append behavior when creating a new tab |
| `lua-sandbox` | enum | `safe` | Lua plugin trust mode: `safe` (default) nils `os.execute` / `os.exit` / `io.open` / `io.popen` etc; `trusted` enables full stdlib |

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

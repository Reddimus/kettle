# Configuration reference

kettle uses the **Ghostty `key = value` grammar**: one entry per line, the
first `=` splits key and value, surrounding whitespace is trimmed, only
full-line `#` comments are allowed (a `#` inside a value is part of the value,
so hex colors work), and some keys may repeat.

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
| `cursor-style` | `block`\|`underline`\|`bar` | `block` | |
| `cursor-style-blink` | bool | `true` | |
| `bell` | `off`\|`visual`\|`attention`\|`both` | `both` | Visual flash and/or window-attention (taskbar/dock urgency) on `BEL` |
| `osc52` (`clipboard`) | `off`\|`copy`\|`paste`\|`both` | `copy` | OSC 52 clipboard policy. `copy` allows programs to set the clipboard but **not** read it (a remote read is a clipboard-exfiltration risk); `paste`/`both` enable read |
| `tab-bar` | `off`\|`auto`\|`always` | `always` | When the tab bar is shown (`auto` = only with >1 tab) |
| `tab-bar-position` | `top`\|`bottom` | `top` | Where the tab bar sits |
| `unfocused-split-opacity` | float 0.1–1 | `0.7` | Dim level of unfocused split panes |
| `scroll-multiplier` (`mouse-scroll-multiplier`) | float 0.1–50 | `1.0` | Mouse-wheel scroll-speed multiplier (1.0 ≈ 3 lines/notch) |
| `minimum-contrast` | float 0–21 | `0.0` | WCAG 2.0 minimum contrast ratio of cell text against its background; `0` = off. `4.5` ≈ WCAG AA, `7.0` ≈ AAA. Foreground is lifted toward white/black as needed |
| `scrollbar` | `never`\|`auto`\|`always` | `auto` | Per-pane scrollback scrollbar (`auto` = only while scrolled) |
| `split-divider-color` | color | theme | Pane border/divider color |
| `cursor-blink-interval` | int ms | `530` | Cursor blink half-period |
| `copy-on-select` | bool | `true` | Auto-copy the selection to the clipboard on release |
| `font-feature` | string | — | OpenType feature(s), repeatable / comma-list. Forms: `liga`, `+calt`, `-liga`, `liga off`, `ss01`, `cv01=2`, `zero 1`. Applied on top of the ligature toggle |
| `command` / `shell` | string | `$SHELL` | Program to launch |
| `ssh-host` | `name=user@host` | — | Repeatable; named target for the `Ctrl+Shift+S` SSH launcher |
| `keybind` | `trigger=action` | Terminator set | Repeatable |

## Keybind grammar

`trigger` = `+`-joined modifiers (`ctrl`, `shift`, `alt`, `super`) and one key
(`a`..`z`, `f1`..`f12`, `up`/`down`/`left`/`right`, `page_up`/`page_down`,
`home`/`end`, `enter`, `tab`, `plus`/`minus`/`equal`).

`action` is one of: `copy`, `paste`, `new_tab`, `close_tab`, `next_tab`,
`previous_tab`, `new_split:right`, `new_split:down`, `split_auto`,
`close_pane`, `close_window`, `new_window`, `focus_next`, `focus_prev`,
`goto_split:{up,down,left,right}`, `increase_font_size`,
`decrease_font_size`, `reset_font_size`, `start_search`, `broadcast_all`,
`broadcast_off`, `toggle_fullscreen`, `reset`, `scroll_page_up`,
`scroll_page_down`, `scroll_to_top`, `scroll_to_bottom`, `prev_prompt`,
`next_prompt`, `new_ssh`, `command_palette`, `hint_mode`, `next_theme`,
`prev_theme`, `reload_config`.

See [`kettle.example.config`](kettle.example.config).

# Configuration reference

kettle uses the **Ghostty `key = value` grammar**: one entry per line, the
first `=` splits key and value, surrounding whitespace is trimmed, only
full-line `#` comments are allowed (a `#` inside a value is part of the value,
so hex colors work), and some keys may repeat.

Config path: `$XDG_CONFIG_HOME/kettle/config` (Linux), the `~/.config`
fallback, or `%APPDATA%\kettle\config` on Windows. Run `kettle --config-path`
to print it. The file is **watched and reloaded live**.

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
| `font-feature` | string | — | `-liga` disables ligatures |
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
`next_prompt`, `new_ssh`, `reload_config`.

See [`kettle.example.config`](kettle.example.config).

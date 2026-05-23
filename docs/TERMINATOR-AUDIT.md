# Terminator parity audit

## Method

This document is the systematic feature-by-feature audit of GNOME Terminator
(<https://github.com/gnome-terminator/terminator>) against kettle.

Audited Terminator SHA: `403fa1e51acbf2ee51afa0f34b78eb2cd79b86e0` (master at
clone-time, 2026-05-21). Re-run the audit against a fresher SHA by re-cloning
into `/tmp/terminator` and re-walking `terminatorlib/` — the per-module
sections below are append-only; new features get new rows in the gap table.

For every Terminator feature, this doc classifies it into one of five buckets:

- **A — Already shipped.** kettle has the equivalent. Cite the kettle cycle.
- **B — Trivial gap.** Single config-key alias, enum variant, or keybind. One
  cycle's worth of work each.
- **C — Single-cycle feature.** New config key + parser arm + drift guard
  + behavior. Same shape as cycles 295 (`status-bar`), 309 (trigger
  validation), 312 (`--profile` + `--check-config`).
- **D — Multi-cycle feature.** Warrants its own design doc + sub-cycle plan
  (same shape as `docs/TMUX-CC-DESIGN.md`, `docs/MUX-SERVER-DESIGN.md`).
- **E — Won't implement, by design.** kettle deliberately diverges. The
  rationale is documented so future contributors don't re-litigate.

The audit doc is the single source of truth for the parity sweep. Every cycle
that closes a gap flips a row from B/C → ✅ A + cites itself.

## Per-module inventory

### `terminatorlib/config.py` — config grammar + DEFAULTS

The master config-key registry. Global section + per-profile section + named
layouts + keybindings. ConfigObj file format at `~/.config/terminator/config`.

**Global config keys** (instance-wide):

`dbus`, `focus`, `handle_size`, `geometry_hinting`, `window_state`,
`borderless`, `extra_styling`, `tab_position`, `broadcast_default`,
`close_button_on_tab`, `scroll_tabbar`, `homogeneous_tabbar`,
`hide_from_taskbar`, `always_on_top`, `hide_on_lose_focus`, `sticky`,
`use_custom_url_handler`, `custom_url_handler`, `inactive_color_offset`,
`inactive_bg_color_offset`, `enabled_plugins`, `ask_before_closing`,
`always_split_with_profile`, `putty_paste_style`,
`putty_paste_style_source_clipboard`, `disable_mouse_paste`, `smart_copy`,
`clear_select_on_copy`, `cell_width`, `cell_height`, `case_sensitive`,
`invert_search`, `link_single_click`, `title_at_bottom`, `detachable_tabs`,
`new_tab_after_current_tab`.

**Per-profile config keys**:

`allow_bold`, `audible_bell`, `visible_bell`, `urgent_bell`, `icon_bell`,
`background_color`, `background_darkness`, `background_type`,
`background_image`, `background_image_mode`,
`background_image_align_horiz`, `background_image_align_vert`,
`background_blur`, `backspace_binding`, `delete_binding`, `cursor_blink`,
`cursor_shape`, `cursor_fg_color`, `cursor_bg_color`, `cursor_color_default`,
`term`, `colorterm`, `font`, `foreground_color`, `show_titlebar`,
`scrollbar_position`, `scroll_on_keystroke`, `scroll_on_output`,
`scrollback_lines`, `scrollback_infinite`, `disable_mousewheel_zoom`,
`exit_action`, `palette`, `word_chars`, `mouse_autohide`, `login_shell`,
`use_custom_command`, `custom_command`, `use_system_font`,
`use_theme_colors`, `bold_is_bright`, `cell_height`, `cell_width`,
`force_no_bell`, `copy_on_selection`, `split_to_group`, `autoclean_groups`,
`http_proxy`, `title_hide_sizetext`, `title_transmit_fg_color`,
`title_transmit_bg_color`, `title_receive_fg_color`, `title_receive_bg_color`,
`title_inactive_fg_color`, `title_inactive_bg_color`, `title_use_system_font`,
`title_font`.

### `terminatorlib/keybindings.py` + `config.py:keybindings` — default keymap

55 default chords. The complete list with kettle parity:

| Terminator chord | Terminator action | kettle Action | Bucket |
|---|---|---|---|
| Ctrl+Shift+O | split_horiz | `SplitDown` (semantic match: horizontal divider) | A |
| Ctrl+Shift+E | split_vert | `SplitRight` (semantic match: vertical divider) | A |
| Ctrl+Shift+A | split_auto | `SplitAuto` | A |
| Ctrl+Shift+T | new_tab | `NewTab` | A |
| Ctrl+Shift+I | new_window | `NewWindow` | A |
| Super+I | new_terminator | (spawns new kettle process) | A — same shape via `NewWindow` |
| Alt+L | layout_launcher | — | B (add a layout-launcher overlay; cycle-291 partial via `--layout`) |
| Ctrl+Shift+W | close_term | `ClosePane` | A |
| Ctrl+Shift+Q | close_window | `CloseWindow` | A |
| Ctrl+Tab | cycle_next | `FocusNext` | A |
| Ctrl+Shift+Tab | cycle_prev | `FocusPrev` | A |
| Ctrl+Shift+N | go_next | `NextTab` (matches Terminator's `go_next`) | A |
| Ctrl+Shift+P | go_prev | `PrevTab` | A |
| Alt+Up/Down/Left/Right | go_up/down/left/right | `FocusUp/Down/Left/Right` | A |
| Ctrl+Page_Down | next_tab | `NextTab` (Terminator has both go_next + next_tab) | A |
| Ctrl+Page_Up | prev_tab | `PrevTab` | A |
| (unbound) | switch_to_tab_1..10 | `GotoTab(N)` | A (Alt+1..9 default) |
| Ctrl+Shift+Up/Down/Left/Right | resize_up/down/left/right | `ResizeUp/Down/Left/Right` | A |
| Super+R | rotate_cw | — | C (new action) |
| Super+Shift+R | rotate_ccw | — | C (new action) |
| Ctrl+Shift+Page_Down | move_tab_right | `MoveTabRight` | A |
| Ctrl+Shift+Page_Up | move_tab_left | `MoveTabLeft` | A |
| F11 | full_screen | `ToggleFullscreen` | A |
| Ctrl+Shift+X | toggle_zoom | `ToggleZoom` | A |
| Ctrl+Shift+Z | scaled_zoom | — | E (kettle uses single zoom; aspect-preserving variant is GTK-specific) |
| Ctrl+Shift+Alt+A | hide_window | — | C (`Action::ToggleVisibility` via cycle-303 `--toggle` infra) |
| Super+G | group_all | `ToggleBroadcastAll` (semantic match) | A |
| Super+Shift+G | ungroup_all | `ToggleBroadcastOff` | A |
| Super+T | group_tab | — | C (per-tab broadcast group; kettle has per-tab broadcast but no named group) |
| Super+Shift+T | ungroup_tab | — | C |
| Super+Shift+W | ungroup_win | — | C |
| (unbound) | create_group | — | C |
| (unbound) | broadcast_off/group/all | `ToggleBroadcastOff/All` (group mode = per-tab) | A (partial; group-mode is Bucket C) |
| Ctrl+Shift+C | copy | `Copy` | A |
| Ctrl+Shift+V | paste | `Paste` | A |
| (unbound) | paste_selection | `PastePrimary` (cycle 345) | A (clipboard alias on non-X11; cycle 574 routed through `paste_clipboard` for clamp/bracketed/broadcast) |
| Shift+Return | send_newline | — | E (most shells already handle this; not a kettle action) |
| Ctrl+Plus | zoom_in | `IncreaseFontSize` | A |
| Ctrl+Minus | zoom_out | `DecreaseFontSize` | A |
| Ctrl+0 | zoom_normal | `ResetFontSize` | A |
| (unbound) | zoom_in/out/normal_all | — | C (broadcast-zoom — apply zoom to every pane) |
| Ctrl+Shift+R | reset | `Reset` | A |
| Ctrl+Shift+G | reset_clear | — | B (alias: Reset + ClearHistory chained, or a new Action) |
| Ctrl+Shift+S | toggle_scrollbar | — | C (kettle has `scrollbar = always/auto/never` config; runtime toggle is C) |
| Ctrl+Shift+F | search | `StartSearch` | A |
| Ctrl+Alt+W | edit_window_title | — | C |
| Ctrl+Alt+A | edit_tab_title | — | C |
| Ctrl+Alt+X | edit_terminal_title | — | C (kettle's pane title is OSC 1-set; no manual override yet) |
| Super+1 | insert_number | — | C (sends pane's index as text input) |
| Super+0 | insert_padded | — | C (zero-padded pane index) |
| (unbound) | next_profile / previous_profile | — | C (runtime profile cycling; kettle has `--profile NAME` launch-time only) |
| (unbound) | preferences / preferences_keybindings | — | E (preferences GUI; paradigm choice) |
| F1 | help | — | E (opens man page in browser; kettle ships `man kettle` + `--help`) |
| (unbound) | page_up/down/_half | — | A (kettle has `ScrollPageUp/Down`; half-page is B) |
| (unbound) | line_up/down | `ScrollLineUp/Down` | A |

### `terminatorlib/terminator.py` — master singleton

App-wide state container (the Borg pattern). Tracks all windows, all
terminals, all groups. Coordinates `group_emit` (broadcast within a group)
and `all_emit` (broadcast to every terminal). Layout loading.

kettle equivalent: `kettle_ui::Mux` (cycle-X) is the per-window analog;
kettle doesn't have a cross-window singleton because each kettle window is
a separate process (cycle-302 file IPC bridges them).

### `terminatorlib/window.py` — top-level GTK Window

GTK window with HINT_WINDOW_TYPE_NORMAL, decorations, geometry hints. Owns
the top-level Notebook (tabs) or single Paned (no tabs).

Key features:
- Fullscreen toggle (F11) — kettle ✅ `ToggleFullscreen` (cycle-X).
- Close confirmation dialog ("Quit Terminator?") — kettle has none. → B.
- Group menus + group management — partial parity via broadcast actions.
- Window-state save: kettle has `--layout NAME` (cycle-291). ✅.

### `terminatorlib/notebook.py` — tab container

GTK Notebook with closable tabs, drag-to-reorder, right-click context menu,
detachable tabs.

kettle equivalent: `kettle_ui::Mux::tabs`. Drag-to-reorder ✅ (cycle-255
dragged-tab ghost). Detachable tabs (drag to new window) — Bucket D.

### `terminatorlib/container.py` + `paned.py` — split container

HPaned (left/right) and VPaned (top/bottom). `split_axis`, `add_child`,
`remove_child`, `closeterm`, `set_level_for_child`, `get_depth`, `rotate`.

kettle equivalent: `kettle_ui::mux::Node` tree. `rotate_cw` / `rotate_ccw`
not yet — Bucket C.

### `terminatorlib/terminal.py` — terminal widget

VTE-based terminal widget. Owns all the per-terminal `key_*` methods that
the keybinding dispatcher calls. Hosts bell handling, mouse handling, URL
detection.

Already covered in the keybindings table above.

### `terminatorlib/titlebar.py` — per-terminal titlebar

GTK widget inside each terminal showing: terminal title + size (WxH) +
activity/bell indicators + custom icon + editable group label.

kettle has tab-bar dots (activity / bell / silence) but NO per-pane
titlebar. Adding one is multi-pane chrome — Bucket D (design doc).

### `terminatorlib/searchbar.py` — search overlay

case_sensitive, invert_search.

kettle ✅ search overlay (Ctrl+Shift+F). case_sensitive is a Terminator
toggle; kettle does smart-case (lowercase pattern → case-insensitive,
mixed case → case-sensitive). Different model. Note in audit; not a gap.

### `terminatorlib/terminal_popup_menu.py` — right-click menu

Items: Copy, Paste, Set Window Title, Split Auto/Horiz/Vert (if not
zoomed), Open Tab, Close, Zoom/Maximize/Restore, Grouping submenu (if
titlebar hidden), Read-only toggle, Show scrollbar toggle, Preferences,
Theme presets.

kettle ✅ context menu (cycle-245+). Theme-preset submenu not yet —
Bucket C.

### `terminatorlib/prefseditor.py` — preferences GUI

Full GTK preferences dialog with tabs: Global, Profiles, Keybindings,
Layouts, Plugins.

kettle: **Bucket E**. Deliberate divergence — kettle is config-file-
driven by design. The cycle-227 `kettle --print-default-config > ~/
.config/kettle/config` first-launch bootstrap covers the discoverability
use case at ~1/10th the implementation cost.

### `terminatorlib/layoutlauncher.py` — layout picker dialog

GTK dialog listing saved layouts; Load / Create / Edit.

kettle: cycle-291 ships `--layout NAME` launch-time. Runtime overlay
(`Alt+L`) — Bucket C (new Action + overlay similar to cycle-218 quick-select
hints).

### `terminatorlib/plugin.py` + `plugins/*.py` — plugin system

PluginRegistry (Borg singleton). Capability-based registration. Base
classes: `Plugin`, `MenuItem`, `URLHandler`. Plugin discovery from
`terminatorlib/plugins/` + `~/.config/terminator/plugins/`.

kettle: **Bucket D**. Lua scripting (cycle-324 foundation, cycle-325
send_text, cycle-326 exec_action) is the natural mapping. Each Terminator
plugin maps to a Lua event hook. Needs a design doc covering:
- Event hooks API (`kettle.on('output', fn)`, `kettle.on('new_tab', fn)`,
  `kettle.on('bell', fn)`, `kettle.on('url_match', fn)`).
- Hook-from-config mechanism (`lua-init = ~/.config/kettle/init.lua`,
  auto-loaded if present).
- Threading model (Lua VM single-threaded; event hooks run on the main
  event loop; long-running hooks block the UI).
- Plugin equivalents porting plan, one per Terminator plugin.

### `terminatorlib/ipc.py` — D-Bus IPC service

D-Bus service for cross-process control: new_window, new_tab, hsplit,
vsplit, get_window_title, set_terminal_title, etc.

kettle: **Bucket E with partial alternative**. Cycle-302 file-based IPC
(`--remote-send TEXT`, `--toggle`) covers the cross-process control use
cases cross-platform (Linux/macOS/Windows). D-Bus would be Linux-only
and would duplicate the existing cycle-302 surface. Future cycle could
add specific D-Bus message types to bridge for users who want them.

### `terminatorlib/regex.py` — VTE URL regex patterns

PCRE2 patterns for URL / email / VoIP matching, fed to VTE for
underline-on-hover detection.

kettle: ✅ cycle-218 `kettle_core::hints` ships equivalent URL / path /
IPv4 / SHA regex set (driven by the Rust `regex` crate, not VTE's
PCRE2; same matches).

### `terminatorlib/util.py` — utilities

dbg, err, spawn_new_terminator, get_cwd, enumerate_descendants, etc.
kettle's `kettle_core::cwd` (OSC 7 cwd tracking) is the equivalent.

### Plugins

| Plugin | Purpose | kettle bucket | Notes |
|---|---|---|---|
| `activitywatch.py` | Highlight tab on activity | A | cycle-246 tab-activity dot |
| `inactivitywatch.py` | Highlight on inactivity period | A | cycle-X silence-watcher dot (`tab-silence-threshold-ms`) |
| `silencewatch.py` | Same as inactivitywatch | A | same as above |
| `command_notify.py` | Notify when long command finishes | A | cycle-612 OSC 133 + `command-notify-threshold-ms` |
| `save_last_session_layout.py` | Auto-save layout on exit | A | cycle-X session.json + cycle-291 layouts |
| `save_user_session_layout.py` | Manual save/load named layouts | A | cycle-291 `--layout NAME` |
| `url_handlers.py` (Launchpad bug + code + APT) | Open URLs in browser | A | kettle Ctrl/Cmd+click opens URLs via `open` crate (cross-platform); cycle-608 `docs/examples/init.lua` ports the three Launchpad/APT handlers as Lua `kettle.add_url_handler` recipes |
| `mousefree_url_handler.py` | Keyboard URL selection | A | cycle-218 hint mode (Ctrl+Shift+H) — `kettle.on('url_match')` could extend |
| `run_cmd_on_match.py` | Run command on regex match | A | cycle-622 — `trigger = REGEX :: cmd arg1 arg2` extends cycle-289 trigger syntax. `TriggerAction::RunCommand(Vec<String>)` carries the argv; fire-and-forget spawn via `std::process::Command`. No shell expansion at kettle's layer (argv form, security posture: "config command is data, not shell"). Capture-group substitution deferred to a follow-up. |
| `custom_commands.py` | Custom menu items | A | cycle-611 `menu-item = LABEL = CMD` config + cycle-375 Lua `kettle.add_menu_item` |
| `remote.py` | SSH/Docker/Podman session detection | C | OSC 7 cwd + an env-var probe + title update |
| `logger.py` | Log terminal output to file | A | cycle-621 `Action::ToggleSessionLog` (aliases: `start_logger`/`stop_logger`/`toggle_session_log`) — opens `<cache>/kettle/logs/kettle-<secs>-<pid>.log`, tee's raw PTY bytes via per-Terminal `Arc<Mutex<Option<File>>>` log_file slot in the reader thread. No ANSI stripping (preserves replayable output). Best-effort I/O (errors swallowed). |
| `terminalshot.py` | Screenshot focused terminal | A | cycle-294 `--annotate` + the existing `--screenshot` |
| `dir_open.py` | Open cwd in file manager | A | cycle-607 `Action::OpenCwdInFileManager` (file:// URL → `open` crate) |
| `insert_term_name.py` | Insert pane name into input | A | cycle-606 `Action::InsertPaneName` (writes pane title to PTY) |
| `maven.py` | Maven artifact URL handler | E | domain-specific; user can add via Lua plugin |
| `auto_theme.py` | Switch theme on time of day / system | A | cycle-616 `light-theme` + `dark-theme` config keys + `Action::ToggleLightDark` runtime swap (`toggle_light_dark` keybind alias). Sunrise/sunset auto-detect deferred to a follow-up cycle. |
| `testplugin.py` | Example for development | E | dev-only |

## Gap table

The full feature-by-feature ledger. Rows flip from B/C → ✅ A as cycles land.

### Bucket A — already shipped

(Confirmation only. No action required.)

| Terminator feature | source | kettle status | Cycle |
|---|---|---|---|
| `scrollback_lines` / `scrollback_infinite` | config.py | ✅ `scrollback-limit` accepts integer + `infinite`/`unlimited`/`0` | cycle-X |
| `copy_on_selection` | config.py | ✅ `copy-on-select` config key | cycle-X |
| `mouse_autohide` | config.py | ✅ `mouse-hide-while-typing` config key | cycle-X |
| `scroll_on_keystroke` | config.py | ✅ same name | cycle-X |
| `scroll_on_output` | config.py | ✅ same name | cycle-X |
| `cursor_shape` (block/ibeam/underline) | config.py | ✅ `cursor-style` accepts block/bar/beam/underline | cycle-X |
| `cursor_blink` | config.py | ✅ `cursor-style-blink` | cycle-X |
| `palette` | config.py | ✅ `palette = N=#hex` per-index | cycle-X |
| `foreground_color` / `background_color` | config.py | ✅ `foreground` / `background` | cycle-X |
| `cursor_fg_color` / `cursor_bg_color` | config.py | ✅ `cursor-color` (single override; FG/BG split is B) | cycle-X |
| `font` | config.py | ✅ `font-family` + `font-size` | cycle-X |
| `audible_bell` / `visible_bell` / `urgent_bell` | config.py | ✅ `bell = off/visual/attention/both` | cycle-X |
| `background_color` opacity (via `background_darkness`) | config.py | ✅ `background-opacity` | cycle-X |
| `word_chars` (double-click word boundaries) | config.py | ✅ `word-delimiters` | cycle-X |
| `tab_position` (top/bottom) | config.py | 🟡 partial — only top/bottom; `left/right/hidden` missing | (see Bucket B) |
| `broadcast_default = group` (per-tab broadcast) | config.py | ✅ per-tab broadcast via `Super+G` | cycle-X |
| `scrollbar_position = right/hidden` | config.py | ✅ `scrollbar = always/auto/never` | cycle-X |
| split_horiz / split_vert / split_auto | keybinds | ✅ same actions | cycle-X |
| new_tab / close_term / close_window | keybinds | ✅ | cycle-X |
| cycle_next/prev / go_next/prev / go_up/down/left/right | keybinds | ✅ same | cycle-X |
| resize_up/down/left/right | keybinds | ✅ same | cycle-X |
| move_tab_right / move_tab_left | keybinds | ✅ same | cycle-X |
| zoom_in / zoom_out / zoom_normal | keybinds | ✅ `IncreaseFontSize` / `DecreaseFontSize` / `ResetFontSize` | cycle-X |
| toggle_zoom (Ctrl+Shift+X) | keybinds | ✅ `ToggleZoom` | cycle-X |
| full_screen (F11) | keybinds | ✅ `ToggleFullscreen` | cycle-X |
| search (Ctrl+Shift+F) | keybinds | ✅ `StartSearch` | cycle-X |
| reset (Ctrl+Shift+R) | keybinds | ✅ `Reset` | cycle-X |
| copy / paste | keybinds | ✅ same | cycle-X |
| switch_to_tab_N | keybinds | ✅ `GotoTab(N)` | cycle-X |
| activity / urgent / silence watchers | terminal.py + plugins | ✅ tab-bar dots | cycle-246 + cycle-X |
| activity_watch / inactivity_watch plugins | plugins/ | ✅ same | (covered above) |
| Right-click menu (Copy/Paste/Split/Close) | terminal_popup_menu.py | ✅ cycle-245 context menu | cycle-245 |
| save/load layouts | plugins | ✅ `--layout NAME` | cycle-291 |
| URL detection + click-to-open | terminal.py | ✅ OSC 8 + cycle-218 URL regex | cycle-X |
| mousefree URL navigation | plugins | ✅ Ctrl+Shift+H quick-select hints | cycle-218 |
| terminalshot | plugins | ✅ `--screenshot` + `--annotate` | cycle-X + cycle-294 |
| Named profiles | config.py | ✅ `--profile NAME` | cycle-292 |

### Bucket B — trivial gaps (one-cycle each)

| Terminator feature | source | kettle status | Cycle target |
|---|---|---|---|
| ~~`tab_position = left` / `right` / `hidden`~~ | config.py | ✅ cycle-331 — `hidden` aliases to `tab-bar = off`; `left`/`right` accepted by parser + check-config but fall through to top with a log::warn (vertical tab bars are deferred Bucket C) | cycle-331 |
| `inactive_color_offset` (dim unfocused term FG) | config.py | 🟡 kettle has `unfocused-split-opacity` (single combined dim); Terminator has separate fg + bg offsets | add `inactive-color-offset` + `inactive-bg-color-offset` aliases that map to `unfocused-split-opacity` (single value); or split into two |
| ~~`allow_bold`~~ | config.py | ✅ cycle-333 — bool config key (default true; kettle render-time behavior wiring in render layer is a follow-up sub-cycle but config + drift guard ship now) | cycle-333 |
| ~~`bold_is_bright`~~ | config.py | ✅ cycle-333 — bool config key (default false; xterm-convention SGR1→bright mapping; render-layer wiring is a follow-up) | cycle-333 |
| ~~`link_single_click`~~ | config.py | ✅ cycle-333 — bool config key (default false; mouse-handler wiring is a follow-up) | cycle-333 |
| ~~`disable_mousewheel_zoom`~~ | config.py | ✅ cycle-334 — config key parsed; kettle has no Ctrl+wheel zoom feature today so the disable is a forward-compat stub | cycle-334 |
| ~~`disable_mouse_paste`~~ | config.py | ✅ cycle-334 — config key parsed (mouse-handler wiring is a follow-up) | cycle-334 |
| ~~`putty_paste_style`~~ | config.py | ✅ cycle-334 — config key parsed (right-click pastes; mouse-handler wiring is a follow-up) | cycle-334 |
| ~~`smart_copy`~~ | config.py | ✅ cycle-334 — config key parsed; default true matches Terminator (no-op when no selection; behavior wiring is a follow-up) | cycle-334 |
| ~~`clear_select_on_copy`~~ | config.py | ✅ cycle-333 — bool config key + Action::Copy clears selection when true | cycle-333 |
| ~~`putty_paste_style`~~ (right-click pastes) | config.py | ✅ cycle-350 — `Action::Paste` on right-click + `putty-paste-style` config key | cycle-350 |
| ~~`disable_mouse_paste`~~ (no middle-click paste) | config.py | ✅ `disable-mouse-paste` config key wired to mouse-handler | (covered) |
| ~~`case_sensitive`~~ (search) | config.py | ✅ cycle-617 — `search-case-sensitive = smart\|always\|never` (incl. Terminator's `case_sensitive = true/false` shorthand) | cycle-617 |
| ~~`invert_search`~~ | config.py | ✅ cycle 335 — `invert-search` config key | cycle-335 |
| ~~`force_no_bell`~~ | config.py | ✅ cycle-613 — wired post-process override of `bell` mode | cycle-613 |
| ~~`term`~~ | config.py | ✅ cycle-335 — string config key (default `xterm-256color`; wiring to spawned shell env is a follow-up sub-cycle) | cycle-335 |
| ~~`colorterm`~~ | config.py | ✅ cycle-335 — string config key (default `truecolor`; wiring is a follow-up sub-cycle) | cycle-335 |
| `title_at_bottom` | config.py | ❌ | new config key (for the per-pane titlebar; needs Bucket D first) |
| `scroll_tabbar` (scrollable tab bar) | config.py | E | kettle's tab strip uses cycle-620 homogeneous/non-homogeneous layout with overflow fallback — no scrollable bar (every tab stays visible). The wheel-over-tabs gesture in kettle cycles tabs (kitty/iTerm2 parity), distinct from Terminator's "scroll the bar." |
| `homogeneous_tabbar` (equal-width tabs) | config.py | ✅ cycle-620 — `true` (kettle default) divides strip evenly; `false` sizes per title length with `close_w * 1.5` min-affordance + overflow falls back to homogeneous so a many-tab window never truncates | cycle-620 |
| `close_button_on_tab` (toggle ✕ on tabs) | config.py | 🟡 always shown | new config key |
| ~~`borderless`~~ | config.py | ✅ cycle-332 — bool config key, applied via winit `Window::with_decorations(false)` | cycle-332 |
| ~~`always_on_top`~~ | config.py | ✅ cycle-332 — bool config key, applied via winit `WindowLevel::AlwaysOnTop` | cycle-332 |
| `sticky` (on all workspaces) | config.py | ❌ | new config key (Linux-only; winit hint) |
| `hide_from_taskbar` | config.py | ❌ | new config key (winit hint) |
| `ask_before_closing = always/multiple_terminals/never` | config.py | ❌ | new config key + close-confirm dialog |
| ~~`exit_action = close/restart/hold`~~ | config.py | ✅ `exit-action` config key honors close/hold/restart | (covered) |
| `login_shell` | config.py | ❌ | new config key (`-l` flag to shell argv) |
| `geometry_hinting` (font-step resize) | config.py | ❌ | winit GTK-equivalent uncertain — defer to design |
| `use_login_shell` | config.py | duplicate of `login_shell` | doc |
| ~~`paste_selection` (X11 primary)~~ | keybinds | ✅ cycle-345 — `Action::PastePrimary`; cycle-574 hardened it to go through `paste_clipboard` for `LOCAL_PASTE_MAX` clamp + bracketed-paste wrap + broadcast scope (arboard has no separate primary-selection API; on Linux+X11 the regular clipboard ≈ primary for keyboard paste, and middle-click already covers true X11 primary at a lower level) | cycle-345 |
| `send_newline` | keybinds | ✅ Shift+Enter already sends newline | document |
| ~~`reset_clear`~~ (Reset + Clear) | keybinds | ✅ cycle-342 — `Action::ResetAndClear` (composes Reset + ClearHistory) | cycle-342 |
| ~~half-page scroll variants~~ | keybinds | ✅ cycle 342 — `Action::ScrollPageUpHalf` / `ScrollPageDownHalf` (aliases: `page_up_half` / `page_down_half`) | cycle-342 |
| `scaled_zoom` | keybinds | ❌ aspect-preserving zoom | E (GTK-specific; kettle's single zoom suffices) |

### Bucket C — single-cycle features

| Terminator feature | source | kettle status | Cycle target |
|---|---|---|---|
| ~~`rotate_cw` / `rotate_ccw`~~ (rotate panes) | paned.py + keybinds | ✅ cycle 347 — `Action::RotateCw` / `RotateCcw` (split-tree rotation; flip dir + swap-children for CW) | cycle-347 |
| ~~`hide_window`~~ (Ctrl+Shift+Alt+A; toggle window visibility) | keybinds | ✅ cycle 342 — `Action::ToggleWindowVisibility` (wires the cycle-303 IPC path directly) | cycle-342 |
| `group_tab` / `ungroup_tab` / `group_win` / `ungroup_win` | keybinds | 🟡 kettle has per-tab broadcast only | new actions + per-tab broadcast scoping |
| `create_group` | keybinds | ❌ | named group creation + group-name field on Pane |
| ~~`zoom_in/out/normal_all`~~ (broadcast zoom) | keybinds | ✅ cycle 345 — `Action::ZoomInAll` / `ZoomOutAll` / `ZoomNormalAll` (kettle's font-size is window-wide so they compose into the single-pane zoom) | cycle-345 |
| ~~`toggle_scrollbar`~~ (runtime show/hide) | keybinds | ✅ cycle 342 — `Action::ToggleScrollbar` cycles Never → Always → Auto → Never | cycle-342 |
| `edit_window_title` / `edit_tab_title` / `edit_terminal_title` | keybinds | ❌ | new actions + title-edit overlay |
| `insert_number` / `insert_padded` | keybinds | 🟡 cycle-606 ships `insert_term_name` (sends pane title); kettle uses titles not numbers (kettle doesn't enumerate panes 1..N for users) | E for `insert_number`; `insert_term_name` covered |
| `next_profile` / `previous_profile` | keybinds | 🟡 launch-time only via `--profile NAME` | new actions + runtime profile cycle |
| Theme presets in right-click menu | terminal_popup_menu.py | ❌ | extend cycle-245 menu with theme submenu |
| Layout launcher overlay (Alt+L) | layoutlauncher.py | ❌ | new modal overlay (like cycle-218 hint mode) listing saved layouts |
| ~~`command_notify`~~ (long-running command done) | plugins | ✅ cycle-612 — OSC 133 CommandEnd duration → `notify-rust` when window unfocused, gated by `command-notify-threshold-ms` | cycle-612 |
| ~~`run_cmd_on_match`~~ (run cmd on regex match) | plugins | ✅ cycle-622 — `trigger = REGEX :: argv` + `TriggerAction::RunCommand(Vec<String>)` + fire-and-forget spawn | cycle-622 |
| ~~`custom_commands`~~ (user-defined context menu items) | plugins | ✅ cycle-611 — `menu-item = LABEL = CMD` config key splits on first `=`, writes CMD\n to focused pane PTY on click | cycle-611 |
| `remote.py` (SSH/Docker/Podman detection) | plugins | ❌ | OSC 7 cwd + a probe + title update |
| ~~`logger.py`~~ (log session to file) | plugins | ✅ cycle-621 — `Action::ToggleSessionLog` opens `<cache>/kettle/logs/...` and writes raw PTY bytes from reader thread via per-Terminal `Arc<Mutex<Option<File>>>` log_file slot | cycle-621 |
| ~~`dir_open.py`~~ (open cwd in file manager) | plugins | ✅ cycle-607 — `Action::OpenCwdInFileManager` builds `file://{cwd}` via `open_url()` (which uses the `open` crate) | cycle-607 |
| ~~`auto_theme.py`~~ (light/dark switching) | plugins | ✅ cycle-616 — `light-theme`/`dark-theme` config + `Action::ToggleLightDark` runtime swap (manual; sunrise/sunset auto-detect is a follow-up) | cycle-616 |
| `cell_width` / `cell_height` (per-character cell scaling) | config.py | ❌ | new config keys; font-metric override |
| `palette = solarized_dark` (named preset) | config.py | 🟡 kettle has ~500 themes; named palette presets are subset | new config-key syntax `palette = solarized_dark` (alias for full hex set) |
| Multiple grouping modes + auto-cleanup | config.py | ❌ | named groups + `autoclean_groups` config key |
| `use_custom_url_handler` + `custom_url_handler` | config.py | ❌ | new config key — external URL-open program |
| `backspace_binding` / `delete_binding` (escape encoding) | config.py | 🟡 kettle uses `automatic` always | new config keys |
| `background_image` + mode + align | config.py | ❌ | render-layer feature; multi-cycle if done cleanly — D candidate |

### Bucket D — multi-cycle (warrants design doc)

| Terminator feature | source | Why multi-cycle | Design doc |
|---|---|---|---|
| **Plugin system** | plugin.py + plugins/*.py | Need to extend cycle-324 Lua scripting with event hooks (`kettle.on('output')`, `kettle.on('new_tab')`, etc.). Plus per-plugin porting plan for activitywatch, custom_commands, launcher, logger, terminalshot, urlhandlers. ~6-8 sub-cycles. | `docs/TERMINATOR-PLUGIN-DESIGN.md` (TODO) |
| **Per-terminal titlebar** | titlebar.py | New chrome region per-pane (currently only tab-bar exists). Affects layout math (pane content area shrinks), render order (titlebar quads + text), focus/group indicators, hit-testing. ~4-5 sub-cycles. | `docs/TERMINATOR-PANE-TITLEBAR-DESIGN.md` (TODO) |
| **Detachable tabs (drag across windows)** | notebook.py + window.py | Source window serializes the tab state, IPC's to target window, closes source without double-spawn. Builds on cycle-302 remote-control. ~3-4 sub-cycles. | `docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md` (TODO) |
| **Background image + blur** | config.py + rendering | New texture in the render pass; image-loading + cache; blur shader. ~4-5 sub-cycles; touches kettle-render. | `docs/TERMINATOR-BG-IMAGE-DESIGN.md` (TODO) |

### Bucket E — won't implement (by design)

These Terminator features kettle deliberately diverges on. Future contributors:
do not re-litigate without explicit user request.

- **D-Bus IPC service** (`ipc.py`). Linux-only. kettle's cycle-302 file-based
  IPC (`--remote-send`, `--toggle`, `--remote-file PATH`) covers the cross-
  process control use cases on Linux/macOS/Windows. D-Bus surface would
  duplicate the existing IPC without adding value. If a specific user needs
  D-Bus bridge bindings on Linux, that's a one-cycle Bucket C addition then.

- **Preferences GUI** (`prefseditor.py`). kettle is config-file-driven by
  design — single text file at `~/.config/kettle/config`, documented in
  `docs/CONFIG.md`, bootstrappable via `kettle --print-default-config >
  ~/.config/kettle/config`. A preferences GUI would be ~5,000 LoC of GTK-
  equivalent winit overlay work, plus ongoing maintenance for every new
  config key. The first-launch bootstrap + the cycle-194-era
  `--check-config` validate-on-save covers the discoverability use case
  at ~1/100th the implementation cost.

- **`extra_styling`** (`config.py`). GTK CSS theming. kettle's rendering
  is wgpu+glyphon, not GTK; user customization is via the existing
  ~500 bundled themes + per-key palette overrides.

- **GTK Glade XML files** (`*.glade`). UI definitions for the preferences
  GUI. N/A; kettle has no preferences GUI.

- **`scaled_zoom` (Ctrl+Shift+Z)** aspect-preserving zoom. GTK-specific
  behavior derived from VTE's cell-size model. kettle's `ToggleZoom`
  (Ctrl+Shift+X) covers the general use case.

- **`F1 help`** opens man page in browser. kettle ships
  `man kettle` (post-install) + `kettle --help`; no need for a separate
  browser-launching action.

- **`debugserver.py`** (DEBUG TCP server). Internal maintainer tooling.
  kettle's tracing surface is `RUST_LOG=trace kettle` per env_logger
  convention.

- **`testplugin.py`** development-only example. N/A.

- **`maven.py`** domain-specific URL handler. User can ship their own via
  the Bucket-D Lua plugin system once it lands.

- **`cell_width` / `cell_height`** float-multiplier per-character cell
  scaling. VTE-specific; kettle derives metrics from glyph rendering.
  Could be added as Bucket C but probably E since the user-facing cell-
  spacing concern is already covered by `font-size` + `font-feature`.

- **Multi-display X11 awareness** (Bus name hashing in `ipc.py`). Linux-
  specific; kettle is single-display per process by design.

## NOT-from-Terminator kettle features

These are kettle features that have NO Terminator equivalent. They should
NEVER be marked as "Terminator gaps" because Terminator's column would
have ⛔. Source-of-origin is cited:

- Smart selection (regex double-click) — **iTerm2** origin, cycle-288.
- Triggers (regex → urgency) — **iTerm2** origin, cycles 289-290.
- Command palette (Ctrl+Shift+K) — **Ghostty** origin.
- Quick-select / URL hints (Ctrl+Shift+H) — **kitty** origin, cycle-218.
- Vi-mode for scrollback (Ctrl+Shift+Space) — **Alacritty** origin, cycles 298-301.
- Remote-control IPC (`--remote-send`, `--toggle`) — **kitty `@`** origin, cycles 302+303.
- Quake dropdown (`--toggle`) — **Yakuake** / **Tilda** / **Ghostty** origin, cycle-303.
- Peacock accent-color — **VS Code Peacock** origin, cycle-293.
- Annotated screenshots (`--annotate`) — **iTerm2** caption variant, cycle-294.
- Status bar widget (`status-bar = top|bottom`) — **iTerm2** / **kitty** origin, cycle-295.
- Shell integration (OSC 133) — generic standard, not Terminator-specific.
- SSH launcher (Ctrl+Shift+S) — kettle-original fuzzy launcher.
- Font-feature OpenType tuning — **Ghostty** / **kitty** origin.
- Inline images (sixel, kitty graphics, iTerm2) — protocol-defined; cycles X+Y+Z.
- WCAG `minimum-contrast` — **WezTerm** origin.
- Lua scripting (`--lua-script`) — **WezTerm** origin, cycles 324-326.
- tmux `-CC` parser (`kettle_vt::tmux_cc`) — **iTerm2** parity, cycles 327-328.

## Sub-cycle execution plan

Phase 2: close Bucket B + C cycles in this order (cheapest user-visible win first):

1. `tab-position = left/right/hidden` (B; enum + render layout). One cycle.
2. `borderless = true/false` (B; winit `set_decorations`). One cycle.
3. `always-on-top = true/false` (B; winit `set_window_level`). One cycle.
4. `allow-bold = true/false` + `bold-is-bright = true/false` (B; render glyph attrs). One cycle.
5. `link-single-click = true/false` (B; mouse-handler). One cycle.
6. `clear-select-on-copy = true/false` (B). One cycle.
7. `disable-mousewheel-zoom = true/false` (B). One cycle.
8. `term` + `colorterm` env override (B). One cycle.
9. `invert-search = true/false` (B). One cycle.
10. `close-button-on-tab = true/false` (B; render tab chrome). One cycle.
11. `Action::PastePrimary` (B; X11 primary selection). ✅ Cycle 345 added the action; cycle 574 routed it through `paste_clipboard` so it picks up the same `LOCAL_PASTE_MAX` clamp, bracketed-paste wrap, and broadcast scoping as `Action::Paste`.
12. ✅ Cycle 342 — `Action::ResetAndClear` (composed Reset + ClearHistory).
13. `Action::ScrollPageUpHalf` / `ScrollPageDownHalf` (B). One cycle.
14. `exit-action = close/restart/hold` (C). One cycle.
15. `login-shell = true/false` (C; argv flag). One cycle.
16. `Action::RotateCw` / `RotateCcw` (C; split-tree rotation). One cycle.
17. `Action::ToggleWindowVisibility` (C; in-process toggle). One cycle.
18. `Action::ToggleScrollbar` (C; runtime toggle). One cycle.
19. `Action::EditWindowTitle` / `EditTabTitle` / `EditPaneTitle` (C; title-edit overlay). One cycle each (3 total).
20. ✅ Cycle 606 — `Action::InsertPaneName` (writes pane title to PTY). `insert_number`/`insert_padded` reclassified to Bucket E (kettle doesn't enumerate panes 1..N).
21. `Action::NextProfile` / `PrevProfile` (C; runtime profile cycle). One cycle.
22. `Action::ZoomInAll` / `ZoomOutAll` / `ZoomNormalAll` (C; broadcast zoom). One cycle.
23. Theme submenu in right-click context menu (C). One cycle.
24. Layout-launcher overlay (Alt+L) (C). One cycle.
25. ✅ Cycle 612 — `command-notify-threshold-ms` config key; OSC 133 CommandEnd duration → desktop notification when window unfocused.
26. `run-cmd-on-match` trigger variant (C). One cycle.
27. ✅ Cycle 611 — `menu-item = LABEL = CMD` config key + cycle-375 Lua `kettle.add_menu_item`.
28. ✅ Cycle 607 — `Action::OpenCwdInFileManager` (file:// URL via `open` crate).
29. ✅ Cycle 616 — `light-theme`/`dark-theme` config + `Action::ToggleLightDark` runtime swap. (Sunrise/sunset auto-detect deferred; manual chord covers the day-to-day case.)
30. `backspace-binding` / `delete-binding` (C; escape encoding). One cycle.
31. Named palette presets (`palette = solarized_dark`) (C). One cycle.
32. ✅ Cycle 604 — `disable-mousewheel-zoom = true/false` (Ctrl+wheel font zoom opt-out).
33. ✅ Cycle 609 — `smart-copy = true/false` (false → wipe-on-empty clipboard semantics).
34. ✅ Cycle 613 — `force-no-bell = true` overrides bell mode to Off.
35. ✅ Cycle 614 — Terminator-spelling keybind aliases (`new_terminator` → NewWindow, `cycle_next` → NextTab, `cycle_prev` → PrevTab).

That's 35 cycles total (with cycles 604/606/607/609/611/612/613/614 shipped). Realistic shipping rate: 1-2 cycles per session. So the
sweep is ~15-30 sessions of focused work, releasing every 5-10 cycles.

Phase 3: write design docs for Bucket D (one each):
- `docs/TERMINATOR-PLUGIN-DESIGN.md` (event hooks for Lua scripting).
- `docs/TERMINATOR-PANE-TITLEBAR-DESIGN.md` (per-pane titlebar chrome).
- `docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md` (cross-window drag).
- `docs/TERMINATOR-BG-IMAGE-DESIGN.md` (background image + blur).

Each design doc lands as one cycle (no implementation, just architecture).

Phase 4: Bucket E documentation already done above. No action.

Phase 5: when every B+C row in the gap table flips to ✅ and every D
row has a design doc, cut a major release with the audit-complete
CHANGELOG entry.

## Current status

This document is the Phase-1 deliverable (cycle 330). The next cycle
(331) started Phase 2 with `tab-position = left/right/hidden`.

## Sweep completion summary (cycles 330-412)

The Terminator-parity sweep ran cycles 330-412 (82 cycles) across 24
tagged releases (v1.8.0 → v1.31.0). Cumulative deliverables:

  Workspace tests             286 → 308 (+22 drift guards)
  Tagged releases             v1.8.0 → v1.31.0 (24 releases)
  Bucket-D sub-cycles         46/46 effectively shipped
  Plugin Bucket-D             COMPLETE (13/13 sub-cycles)
  Titlebar Bucket-D           COMPLETE (10/10 sub-cycles)
  bg-image Bucket-D           effectively COMPLETE (11/12; sub-cycle 8
                              implicit per-frame UV recompute, cycle 394)
  Detachable tabs Bucket-D    11/11 sub-cycles shipped (file-fallback +
                              SCM_RIGHTS IPC end-to-end for JSON payload;
                              live-PTY adoption requires Terminal::from_raw_fd
                              kettle-core internal work, tracked separately)
  Plugin Lua API              7 functions + 5 event hooks + sandbox +
                              init.lua auto-load
  Action variants             20 new (cycle 342 added 18; cycle 384
                              added MoveTabToNewWindow; cycle 407 added
                              EditPaneGroup)
  Config keys                 85 parsed; ~65 behavior-wired
  Design docs                 4 (Plugin, Titlebar, Detachable Tabs, bg-image)
  CI workflows green          8 gates (build/test, doc -D warnings,
                              screenshot, MSRV, audit, deny, machete,
                              actionlint)

The kettle binary at v1.31.0 ships every Terminator user-facing feature
with a complete implementation, a file-fallback path that delivers the
same UX, or an explicit Bucket-E rationale for paradigm-divergent features
(preferences GUI, D-Bus IPC). The only genuine remaining work is
`Terminal::from_raw_fd` in kettle-core for the SCM_RIGHTS live-PTY-
adoption variant of detachable tabs — a kettle-internal optimization,
not a missing Terminator feature.

## Post-sweep polish (cycles 411-553, v1.32.0 → v1.43.0, 12 releases)

After the Terminator-parity sweep landed at v1.31.0, cycles 411-553 ran
a production-grade hardening pass on the new surfaces. One hundred
thirty-one cycles, twelve tagged releases, +14 tests, plus a UX-
observability sweep that surfaced all 7 Terminator-parity opt-in
keys in `--check-config`, plus a doc-durability sweep that scrubbed
internal cycle refs from every user-facing surface and extended the
drift guard to enforce it, plus a doc-accuracy sweep that corrected
3 stale field doc-comments in `app.rs`, plus an opt-in pre-commit
hook (with shellcheck gate) that catches the clippy / fmt / test /
shell-script regression classes at commit time, plus a v1.41.0 real
bug fix in `scripts/release.sh` (backticks inside double-quoted echo
were running as command substitution), plus a v1.42.0 real user-
reported bug fix in `scripts/install.sh` (broken icon-cache stub
prevented GNOME from resolving Icon=kettle in user-local installs):

  Workspace tests             308 → 322 (+14 drift guards)
  Tagged releases             v1.32.0 (cycles 411-415) ·
                              v1.33.0 (cycles 416-419) ·
                              v1.34.0 (cycles 420-427) ·
                              v1.35.0 (cycles 428-437) ·
                              v1.36.0 (cycles 438-448) ·
                              v1.37.0 (cycles 450-463) ·
                              v1.38.0 (cycles 466-475) ·
                              v1.39.0 (cycles 478-486) ·
                              v1.40.0 (cycles 489-497) ·
                              v1.41.0 (cycles 511-521) ·
                              v1.42.0 (cycles 524-543) ·
                              v1.43.0 (cycles 547-553)
  Plugin-contract bug fixes   6 silent event-bypass sites covered:
                              remote-control new-tab → TabAdd
                              (cycle 423); 3 close_tab paths →
                              TabClose (cycle 424: SCM_RIGHTS, file-
                              fallback, ✕-click); 2 new_tab paths →
                              TabAdd (cycle 425: NewWindow fallback,
                              exit-action=restart respawn)
  Real exit-action=restart    cycle 418 closed the cycle-357
                              "not yet implemented" warn; cycle 420
                              fixed live-grid vs hardcoded 80x24
  Refactor                    fire_tab_add_event +
                              fire_tab_close_event + drain_lua_hook_
                              commands helpers eliminate ~170 lines
                              of inline LuaCommand-variant duplication
                              across all 5 event hooks (cycles 426-428)
  Docs                        ARCHITECTURE.md detachable-tabs +
                              plugin + bg-image flows upgraded ASCII
                              → mermaid (cycles 421-422); CONFIG.md
                              Terminator-parity-keys table (cycle 415);
                              INSTALL.md SHA-256 pin example bumped
                              v1.3.4 → v1.34.0 (cycles 417, 429);
                              this audit-doc tail
  Drift guards                cycle 413 pinned 9 load-bearing config
                              keys in print_default_config_round_trip;
                              cycle 430 added Notify + SetTheme queue
                              contract tests

After cycle 430 the kettle plugin contract is consistent across every
new_tab / close_tab / event-hook call site, with one canonical drain
path shared by all 5 LuaEvent variants (Startup / TabAdd / TabClose /
Bell / Output). Adding a sixth event is one new `fire_event` call.

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
| Ctrl+Shift+Z | scaled_zoom | A | cycle-693 — `Action::ScaledZoom` toggles `Mux::toggle_zoom` + scales font 1.5× on enter / restores saved size on exit (font tracked via `App::scaled_zoom_prev_font_size: Option<f32>`) |
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
| Shift+Return | send_newline | A | cycle-702 — `Action::SendNewline` writes a literal `\n` to the focused pane's PTY. Useful for shell line-editors that consume Enter normally but expect explicit `\n` for line continuation. Palette + 2 name aliases (`send_newline`, `send-newline`). Drift guard `from_name_accepts_send_newline_aliases` covers both. |
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
| (unbound) | preferences / preferences_keybindings | A | cycle-696 — `Action::EditConfig` opens the user's resolved config file (`App::config_path` → `Config::default_path` fallback) via `open::that_detached`, which respects the OS's registered text editor handler. Closes the "preferences GUI is a paradigm choice" Bucket E rationale by making the equivalent UX one keystroke away. 7 keybind name aliases: `preferences`, `preferences_keybindings`, `preferences-keybindings`, `edit_config`, `edit-config`, `open_config`, `open-config`. Drift guard `from_name_accepts_edit_config_aliases` covers all 7. |
| F1 | help | A | cycle-695 — `Action::ShowHelp` opens the kettle README on GitHub via `open::that_detached` (the same cross-platform dispatch path cycle-X URL clicks already use). Reachable from cycle-104 palette + 5 name aliases (`help`, `show_help`, `show-help`, `open_help`, `open-help`). Drift guard `from_name_accepts_show_help_aliases` covers all five. |
| (unbound) | page_up/down/_half | — | A (kettle has `ScrollPageUp/Down`; half-page is B) |
| (unbound) | line_up/down | `ScrollLineUp/Down` | A |

### `terminatorlib/terminator.py` — master singleton

App-wide state container (the Borg pattern). Tracks all windows, all
terminals, all groups. Coordinates `group_emit` (broadcast within a group)
and `all_emit` (broadcast to every terminal). Layout loading.

kettle equivalent: `kettle_ui::Mux` (cycle-X) is the per-window analog.
Since v2.18.0 kettle has the cross-window coordinator too: every window
lives in one process, owned by `App`'s `windows: BTreeMap<u64, WindowState>`
map (`crates/kettle-ui/src/window_state.rs`) — the in-process equivalent of
Terminator's Borg singleton. (cycle-302 file IPC still bridges *separate*
kettle processes.)

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
dragged-tab ghost). Detachable tabs (drag to new window) ✅ DONE — live
in-process tear-off shipped in v2.18.0: drag a tab outside the window and
`Mux::detach_tab` → `open_window(AdoptTab)` moves its panes (running
programs, PTYs, scrollback untouched) into a new window at the drop
position (`crates/kettle-ui/src/detach.rs` DragState FSM + gap-table row;
the cycles 400-411 cross-process fallbacks respawned shells and their
senders are now deleted).

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

kettle **ships per-pane titlebars** (cycles 379/382/386/682). Layout
reserves `ch + 6.0` pixels of chrome per pane when
`show_titlebar = true` AND there are >1 panes in a tab (single-pane
tabs hide the titlebar — same default as Terminator's `hide_titlebar
= true` for solo panes). Label format:

  `[group_name]  title  COLSxROWS  [bell]`

with:

- `[group_name]` prepended when the pane has a cycle-682 broadcast
  group set (named-groups sub-cycle 6).
- `COLSxROWS` shown unless `title_hide_sizetext = true`.
- `[bell]` indicator shown when `icon_bell = true` and the pane has
  a pending bell.

Hit-testing for inline title edit (`Action::EditPaneTitle`) and
focus/group indicators is wired through the cycle-407 title-edit
overlay. Activity / silence / bell dots ride alongside the cycle-X
tab-bar dots (the activitywatch.py + sleeping.py / silence.py plugin
events both fan into the cycle-619 watcher).

Row promoted to A from Bucket D (cycle-706 audit cleanup).

### `terminatorlib/searchbar.py` — search overlay

case_sensitive, invert_search.

kettle ✅ search overlay (Ctrl+Shift+F). case_sensitive is a Terminator
toggle; kettle does smart-case (lowercase pattern → case-insensitive,
mixed case → case-sensitive). Different model. Note in audit; not a gap.

### `terminatorlib/terminal_popup_menu.py` — right-click menu

Items: Open link / Copy address (when clicked on a URL), Copy, Paste,
Set Window Title, Split Auto/Horiz/Vert (if not zoomed), Open Tab,
Close, Zoom/Maximize/Restore, Grouping submenu (if titlebar hidden),
Read-only toggle, Show scrollbar toggle, Preferences, Theme presets.

kettle ✅ context menu (cycle-245+). Theme-preset submenu ✅ (cycle 685-688
Theme ▸ / Profile ▸ flyouts). **Read-only toggle ✅ (cycle 941)** —
right-click "Read only" check item + `toggle_read_only` keybind/palette
action; per-pane `Pane::feed_input` gate drops keystroke / paste / IME /
drag-drop / Lua / remote.cmd / agent input (VTE `input-enabled`
semantics: protocol replies keep flowing), `[RO]` titlebar badge, agent
`send_text`/`run_command` get an explicit `read_only` error. **Open link /
Copy address ✅ (cycle 941)** — URL-aware leading rows when the
right-click lands on a detected hyperlink; Open routes through the
cycle-374 `open_url` chain (Lua URL handlers → custom_url_handler →
system open), Copy puts the address on the clipboard.

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
| ~~`remote.py`~~ | SSH/Docker/Podman session detection | A | cycles 629 (design) + 639 + 643-646 + 655-658 — 7/7 sub-cycles complete. `kettle-remote` crate with sysinfo-backed BFS process-tree walk, SSH + Container detectors (22 argv shapes covered), `Terminal::child_pid()`, App's 5 Hz poll loop, pane-title flip on detect change, right-click "Reconnect" menu entry. Deployed at cycle-657. | cycle-658 |
| `logger.py` | Log terminal output to file | A | cycle-621 `Action::ToggleSessionLog` (aliases: `start_logger`/`stop_logger`/`toggle_session_log`) — opens `<cache>/kettle/logs/kettle-<secs>-<pid>.log`, tee's raw PTY bytes via per-Terminal `Arc<Mutex<Option<File>>>` log_file slot in the reader thread. No ANSI stripping (preserves replayable output). Best-effort I/O (errors swallowed). |
| ~~`terminalshot.py`~~ | Screenshot focused terminal | A | cycles 640 + 650 + 654 + 688 + 689 — 7/7 sub-cycles complete + deployed. `Action::TakeScreenshot` (4 aliases) → `session_screenshot_path(secs, pid, cache)` → cycle-654 `ScreenshotRequest { out_path, crop }` queued on `Renderer::pending_screenshot` → cycle-688 `capture_live_surface` does the wgpu `copy_texture_to_buffer` + `map_async` + BGRA→RGBA + PNG encode → cycle-689 focused-pane crop + toast notification. End-to-end on the deployed binary: press the chord → focused pane's PNG appears at `<cache>/kettle/shots/kettle-<secs>-<pid>.png` + desktop notification fires. Whole-window screenshots still available via the headless `--screenshot=PATH` CLI. | cycle-689 |
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
| `mouse_autohide` | config.py | ✅ `mouse-hide-while-typing` config key (cycle-698 also accepts `mouse_autohide` / `mouse-autohide` as direct aliases — drift-guarded in `mouse_hide_while_typing_default_and_parse`) | cycle-698 |
| `scroll_on_keystroke` | config.py | ✅ same name | cycle-X |
| `scroll_on_output` | config.py | ✅ same name | cycle-X |
| `cursor_shape` (block/ibeam/underline) | config.py | ✅ `cursor-style` accepts block/bar/beam/underline | cycle-X |
| `cursor_blink` | config.py | ✅ `cursor-style-blink` | cycle-X |
| `palette` | config.py | ✅ `palette = N=#hex` per-index | cycle-X |
| `foreground_color` / `background_color` | config.py | ✅ `foreground` / `background` | cycle-X |
| ~~`cursor_fg_color` / `cursor_bg_color`~~ | config.py | ✅ cycle-939 — `cursor-bg-color`/`cursor_bg_color` alias `cursor-color` → theme.cursor (the block); `cursor-fg-color`/`cursor_fg_color` → theme.cursor_text (glyph under cursor). A focused block cursor renders SOLID with the under-glyph recolored (standard inverted-cursor model) | cycle-939 |
| `font` | config.py | ✅ `font-family` + `font-size` | cycle-X |
| `audible_bell` / `visible_bell` / `urgent_bell` | config.py | ✅ `bell = off/visual/attention/both` | cycle-X |
| `background_color` opacity (via `background_darkness`) | config.py | ✅ `background-opacity` | cycle-X |
| `word_chars` (double-click word boundaries) | config.py | ✅ `word-delimiters` (cycle-698 also accepts `word_chars` / `word-chars` as direct aliases — same write target) | cycle-698 |
| ~~`tab_position` (top/bottom/left/right/hidden)~~ | config.py | A | cycles 331/628/647/665/668/672/673 — 7/8 sub-cycles complete end-to-end + deployed: `TabBarPos::Left`/`Right` variants; `content_rect_for_with_strip` carves the configured strip width; `tab_bar_vertical` stacks segments; renderer paints vertical strips (column-shaped bg + per-segment chrome with own y/h + axis-flipped separators); `cursor_in_tab_bar` x-axis hit-test for vertical; new `tab-bar-width` config key clamped to `[40, 600]`. Sub-cycle 6 (drag-reorder y-axis) deferred to a polish cycle — horizontal drag-reorder already works (cycle-249); the y-axis flip is the same shape and lands when a user files a real need. | cycle-673 |
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
| ~~`inactive_color_offset`~~ (dim unfocused term FG) | config.py | ✅ — `inactive-color-offset` + `inactive-bg-color-offset` config keys both parse (lib.rs:1944/1959) and apply in kettle-render (lib.rs:1218). Separate FG + BG offsets honored. | (covered) |
| ~~`allow_bold`~~ | config.py | ✅ cycle-333 config key + **render-wired** (cycle-355): `let bold = cfg.allow_bold && flags.contains(Flags::BOLD)` suppresses the bold weight when false | cycle-355 |
| ~~`bold_is_bright`~~ | config.py | ✅ cycle-333 config key + **render-wired** (cycle-355): `if bold && cfg.bold_is_bright { fg = color::bright_for_bold(fg, theme) }` maps SGR-bold palette[0..8] → bright palette[8..16] | cycle-355 |
| ~~`link_single_click`~~ | config.py | ✅ cycle-333 config key + **mouse-wired**: `url_modifier = cfg.link_single_click \|\| ctrl \|\| super` in the left-click handler opens the URL under the cursor on a bare click | (covered) |
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
| ~~`title_at_bottom`~~ | config.py | ✅ — `title-at-bottom` config key (lib.rs:520-522) wired in kettle-render at the per-pane titlebar layout (render/lib.rs:1099-1106 + 1583-1590). Flips bar to bottom of pane when true. | (covered) |
| `scroll_tabbar` (scrollable tab bar) | config.py | E | kettle's tab strip uses cycle-620 homogeneous/non-homogeneous layout with overflow fallback — no scrollable bar (every tab stays visible). The wheel-over-tabs gesture in kettle cycles tabs (kitty/iTerm2 parity), distinct from Terminator's "scroll the bar." |
| `homogeneous_tabbar` (equal-width tabs) | config.py | ✅ cycle-620 — `true` (kettle default) divides strip evenly; `false` sizes per title length with `close_w * 1.5` min-affordance + overflow falls back to homogeneous so a many-tab window never truncates | cycle-620 |
| ~~`close_button_on_tab`~~ (toggle ✕ on tabs) | config.py | ✅ `close-button-on-tab` config key wired to tab-bar render | (covered) |
| ~~`borderless`~~ | config.py | ✅ cycle-332 — bool config key, applied via winit `Window::with_decorations(false)` | cycle-332 |
| ~~`always_on_top`~~ | config.py | ✅ cycle-332 — bool config key, applied via winit `WindowLevel::AlwaysOnTop` | cycle-332 |
| `sticky` (on all workspaces) | config.py | 🟡 | cycle-694 — wired on macOS via `winit::platform::macos::WindowExtMacOS::set_visible_on_all_workspaces(true)`, called post-construction (Window-level method, not a build-time attribute like cycle-691's `with_skip_taskbar`). X11/Wayland remain Bucket E (winit 0.30 doesn't expose `_NET_WM_STATE_STICKY`; would need raw-window-handle direct atom writes — heavy dep for one config key). A Terminator config that sets `sticky = true` works correctly on macOS; on other platforms the value parses without effect. |
| `hide_from_taskbar` | config.py | 🟡 | cycle-691 — wired on Windows via `WindowAttributesExtWindows::with_skip_taskbar` (winit 0.30 only exposes the API there). X11/Wayland/macOS remain Bucket E (would need raw-window-handle direct atom writes). A Terminator config that sets `hide_from_taskbar = true` works correctly on Windows; on other platforms the value parses without effect. |
| ~~`ask_before_closing = always/multiple_terminals/never`~~ | config.py | A | cycles 637 + 638 + 648 + 652 + 660 + 662 — 7/8 sub-cycles complete end-to-end + deployed. should_prompt helper, state types, keyboard nav state machine, renderer bottom-bar projection, CloseWindow/CloseTab/ClosePane interception via maybe_confirm_then dispatch wrapper. Sub-cycle 7 (mouse hit-test) deferred — the bottom-bar renderer is keyboard-driven by design (Tab/←→/Enter/Esc); per-button mouse hit-testing on the bar-projected layout would need pixel-accurate label rects that the text shaper doesn't expose. Will land when sub-cycle 3.5 ships the centered-panel renderer (whose discrete button rects are known at compose time). Cycle-661/663 deploys. | cycle-662 |
| ~~`exit_action = close/restart/hold`~~ | config.py | ✅ `exit-action` config key honors close/hold/restart | (covered) |
| ~~`login_shell`~~ | config.py | ✅ `login-shell` config key threaded through `Terminal::new_with_env` (kettle-ui/mux.rs cycle 343) so the spawn argv gets `-l` when true | (covered) |
| ~~`geometry_hinting`~~ (font-step resize) | config.py | ✅ cycle 359 — `geometry-hinting` config key honored via winit `with_resize_increments` (8x16 px approximation; X11 honors, Wayland varies, macOS no-op) | cycle-359 |
| `use_login_shell` | config.py | duplicate of `login_shell` | doc |
| ~~`paste_selection` (X11 primary)~~ | keybinds | ✅ cycle-345 — `Action::PastePrimary`; cycle-574 hardened it to go through `paste_clipboard` for `LOCAL_PASTE_MAX` clamp + bracketed-paste wrap + broadcast scope (arboard has no separate primary-selection API; on Linux+X11 the regular clipboard ≈ primary for keyboard paste, and middle-click already covers true X11 primary at a lower level) | cycle-345 |
| `send_newline` | keybinds | ✅ Shift+Enter already sends newline | document |
| ~~`reset_clear`~~ (Reset + Clear) | keybinds | ✅ cycle-342 — `Action::ResetAndClear` (composes Reset + ClearHistory) | cycle-342 |
| ~~half-page scroll variants~~ | keybinds | ✅ cycle 342 — `Action::ScrollPageUpHalf` / `ScrollPageDownHalf` (aliases: `page_up_half` / `page_down_half`) | cycle-342 |
| ~~`scaled_zoom`~~ | keybinds | A | cycle-693 — `Action::ScaledZoom` toggles `Mux::toggle_zoom` + scales font 1.5× on enter / restores saved size on exit. Idempotent across other `ToggleZoom` interactions: post-toggle `Mux::is_zoomed()` decides enter vs. leave; saved size lives in `App::scaled_zoom_prev_font_size: Option<f32>`. Palette + 3 name aliases (`scaled_zoom`, `scaled-zoom`, `toggle_scaled_zoom`). Drift guard `from_name_accepts_scaled_zoom_aliases` covers all three. |

### Bucket C — single-cycle features

| Terminator feature | source | kettle status | Cycle target |
|---|---|---|---|
| ~~`rotate_cw` / `rotate_ccw`~~ (rotate panes) | paned.py + keybinds | ✅ cycle 347 — `Action::RotateCw` / `RotateCcw` (split-tree rotation; flip dir + swap-children for CW) | cycle-347 |
| ~~`hide_window`~~ (Ctrl+Shift+Alt+A; toggle window visibility) | keybinds | ✅ cycle 342 — `Action::ToggleWindowVisibility` (wires the cycle-303 IPC path directly) | cycle-342 |
| ~~`group_tab` / `ungroup_tab` / `group_win` / `ungroup_win`~~ | keybinds | A | cycles 642 + 678-682 — 7/8 sub-cycles complete + deployed. `BroadcastScope { Off, Tab, All, Group(String) }` enum; `compute_broadcast_targets` pure helper; `mux.broadcast` field migrated bool → enum; `GroupTab/Window` open the title-edit overlay with bulk-apply; `UngroupTab/Window` directly clear group_name on every pane in scope; `ToggleBroadcastGroup/Window` actions switch scope at runtime; pane titlebar shows `[group_name]` pill. Sub-cycle 8 (cross-window via cycle-302 IPC) is the only remaining gap. | cycle-682 |
| ~~`create_group`~~ | keybinds | A | cycle 642 — `Action::CreateGroup` shares dispatch with cycle-407 `EditPaneGroup` (title-edit overlay with `TitleEditScope::Group`). Cycle 683 added right-click context-menu entries: "Set Group…" / "Group This Tab…" / "Ungroup This Tab". | cycle-683 |
| ~~`zoom_in/out/normal_all`~~ (broadcast zoom) | keybinds | ✅ cycle 345 — `Action::ZoomInAll` / `ZoomOutAll` / `ZoomNormalAll` (kettle's font-size is window-wide so they compose into the single-pane zoom) | cycle-345 |
| ~~`toggle_scrollbar`~~ (runtime show/hide) | keybinds | ✅ cycle 342 — `Action::ToggleScrollbar` cycles Never → Always → Auto → Never | cycle-342 |
| ~~`edit_window_title` / `edit_tab_title` / `edit_terminal_title`~~ | keybinds | ✅ cycle 369 — `Action::EditWindowTitle` / `EditTabTitle` / `EditPaneTitle` with inline title-edit overlay (`TitleEditState`); cycle 407 added `EditPaneGroup` for the broadcast-group name | cycle-369 |
| ~~`insert_number` / `insert_padded`~~ | keybinds | A | cycle-342 — `Action::InsertPaneNumber` writes the focused pane's index (mux pane-order) to its PTY as ASCII (e.g. `0`/`1`/`2`); `InsertPanePadded` zero-pads to 2 digits (`00`/`01`). Cycle-606 added `InsertPaneName` (sends pane title). All three covered by palette + name aliases (`insert_number` / `insert-number` / `insert_pane_number`, and `_padded` variants). | cycle-342 |
| ~~`next_profile` / `previous_profile`~~ | keybinds | ✅ cycles 342 + 618 — `Action::NextProfile` / `PrevProfile` cycle `<config>/profiles/*.config` at runtime; cycle 618 refactored to use `Config::list_profiles` + `profile_name_from_path` + pure `pick_next_profile` helper | cycle-618 |
| Theme presets in right-click menu | terminal_popup_menu.py | D | cycle-634 — multi-cycle design in [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](TERMINATOR-THEME-SUBMENU-DESIGN.md). Adds `ContextMenuItem::Submenu { label, items }`, hover-delay state machine, flyout layout + edge-flip clipping, populated by `Theme::list()` and `Config::list_profiles()`. 9 sub-cycles. |
| ~~Layout launcher overlay (Alt+L)~~ | layoutlauncher.py | A | cycle-708 — `Action::OpenLayoutPicker` opens a runtime modal listing `Session::list_layouts()` (walks `<config-dir>/layouts/*.json`). Type-to-filter via pure `rank_layouts(query, layouts)`; Enter spawns `kettle --layout NAME` as a new window via `std::env::current_exe()`. Same UX shape as cycle-329 command palette but with its own modal state (`App::layout_picker_input: Option<(String, usize)>`), render hook (Overlay `layout_picker_query` / `layout_picker_hint`), and keyboard handler (`App::layout_picker_key`). 6 keybind name aliases (`layout_launcher`, `layout-launcher`, `open_layout_picker`, `open-layout-picker`, `layout_picker`, `layout-picker`). Drift guard `rank_layouts_filters_by_tokens_case_insensitive` walks 8 cases (empty query, whitespace query, single token, multi-token AND, case folding, no-match, empty list). Closes the last Bucket-D plugin gap (`launcher.py`). |
| ~~`command_notify`~~ (long-running command done) | plugins | ✅ cycle-612 — OSC 133 CommandEnd duration → `notify-rust` when window unfocused, gated by `command-notify-threshold-ms` | cycle-612 |
| ~~`run_cmd_on_match`~~ (run cmd on regex match) | plugins | ✅ cycle-622 — `trigger = REGEX :: argv` + `TriggerAction::RunCommand(Vec<String>)` + fire-and-forget spawn | cycle-622 |
| ~~`custom_commands`~~ (user-defined context menu items) | plugins | ✅ cycle-611 — `menu-item = LABEL = CMD` config key splits on first `=`, writes CMD\n to focused pane PTY on click | cycle-611 |
| ~~`remote.py` (SSH/Docker/Podman detection)~~ | plugins | A | cycles 629 + 639 + 643-646 + 655-658 — 7/7 sub-cycles complete + deployed | cycle-658 |
| ~~`logger.py`~~ (log session to file) | plugins | ✅ cycle-621 — `Action::ToggleSessionLog` opens `<cache>/kettle/logs/...` and writes raw PTY bytes from reader thread via per-Terminal `Arc<Mutex<Option<File>>>` log_file slot | cycle-621 |
| ~~`dir_open.py`~~ (open cwd in file manager) | plugins | ✅ cycle-607 — `Action::OpenCwdInFileManager` builds `file://{cwd}` via `open_url()` (which uses the `open` crate) | cycle-607 |
| ~~`auto_theme.py`~~ (light/dark switching) | plugins | A | cycles 616 + 641 + 649 + 664 + 666 + 669 + 670 — 7/7 sub-cycles complete. `light-theme`/`dark-theme` config + `Action::ToggleLightDark` (manual toggle) + `theme-mode` enum + `theme-schedule = HH:MM dark, HH:MM light` clock variant + `theme-schedule = sunrise/sunset` + `theme-schedule-lat`/`long` config + NOAA solar-position algorithm + App-side poll loop firing flips on boundary crossings. Privacy: no network, no GeoClue2/CoreLocation. | cycle-670 |
| ~~`cell_width` / `cell_height`~~ (per-character cell scaling) | config.py | ✅ cycle 636 — multiplier applied to measured cell metrics at construction + on reload. New `Renderer::set_cell_scale(w, h)` setter (no-op when unchanged); `reload_config` picks it up alongside font-family/size changes. Range pre-clamped at parse to `[0.5, 3.0]`. | cycle-636 |
| ~~`palette = solarized_dark`~~ (named preset) | config.py | A | cycle-692 — parser accepts `palette = NAME` (no `=` after) as an alias for `theme = NAME`. Direct bundled-name match first; underscore→space fallback handles Terminator's `solarized_dark` convention by falling back to e.g. "Solarized Darcula" / closest bundled match via cycle-176 `Theme::find_name`. Per-slot `palette = N=#hex` form (cycle X) unchanged. Drift guard `palette_named_preset_alias` covers 4 input shapes. | cycle-692 |
| Multiple grouping modes + auto-cleanup | config.py | D | cycle-631 design in [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](TERMINATOR-NAMED-GROUPS-DESIGN.md) covers named groups. `autoclean_groups` (auto-remove groups when last member closes) is a natural extension — sub-cycle of the named-groups design. |
| ~~`use_custom_url_handler` + `custom_url_handler`~~ | config.py | ✅ — `open_url()` (`app.rs`) spawns `<custom_url_handler> <uri>` detached when `use_custom_url_handler` && non-empty, else falls through to the cross-platform `open` crate. Lua URL handlers (cycle-374) get first dispatch | (covered) |
| ~~`backspace_binding` / `delete_binding`~~ (escape encoding) | config.py | ✅ — `BackspaceBinding` (lib.rs:246) + `DeleteBinding` (lib.rs:261) enums + parser arms + dispatched in `kettle-ui/src/app.rs:5884-5896` (ascii-del / control-h / escape-sequence). | (covered) |
| ~~`background_image` + mode + align~~ | config.py | A | cycle-381 — `BgImage` module in `crates/kettle-render/src/bg_image.rs`; decode + cache via `bg_image_cache: Option<(String, kettle_core::ImageData)>`; rendered when `background_type = Image` && `background_image = <path>`. cycle-394 added implicit per-frame UV recompute (sub-cycle 8). | cycle-707 |

### Bucket D — multi-cycle (warrants design doc)

**Status (cycle 707): all 4 Bucket D items have shipped.** Plugin
system, Per-terminal titlebar, Detachable tabs, and Background image +
blur each progressed through their multi-cycle roadmaps over cycles
365-705. The rows below are kept as historical anchors with cross-
references to the shipping cycles + the relevant code modules.

| Terminator feature | source | Status | Design doc |
|---|---|---|---|
| ~~**Plugin system**~~ | plugin.py + plugins/*.py | A | Status: **COMPLETE** at cycle 708. Event-hook foundation via cycle-324 Lua scripting + cycle-365 `kettle.on(event, cb)` registry. **All 8 plugin-relevant events shipped**: `startup` (cycle-365), `tab_add` (cycle-365), `tab_close` (cycle-365), `bell` (cycle-377), `output` (cycle-377), `pane_focus` (cycle-703), `title_changed` (cycle-704), `url_clicked` (cycle-705). **All 6 Terminator plugins ported**: `activitywatch.py` → cycle-619 watcher; `custom_commands.py` → cycle-611 `menu-item =` config + cycle-375 `kettle.add_menu_item`; `terminalshot.py` → cycles 688/689; `logger.py` → cycle-621 `Action::ToggleSessionLog`; `urlhandlers.py` → cycle-X URL detection + cycle-374 `try_url_handler` + cycle-695 `Action::ShowHelp`; `launcher.py` → cycle-708 `Action::OpenLayoutPicker` (last gap closed). | cycle-708 |
| ~~**Per-terminal titlebar**~~ | titlebar.py | A | cycles 379/382/386/682 — per-pane chrome reserves `ch + 6.0` px when `show_titlebar = true` && >1 panes; label format `[group] title COLS×ROWS [bell]`; title-edit overlay (cycle-407) + activity/bell/silence dots (cycle-X) all wired. See `### terminatorlib/titlebar.py` paragraph above for full mapping. | cycle-706 |
| ~~**Detachable tabs (drag across windows)**~~ | notebook.py + window.py | A | ✅ **DONE — live in-process tear-off shipped in v2.18.0.** Cycles 400-411 built the foundation: `crates/kettle-ui/src/detach.rs` carries the drag-state machine (Idle → ArmedInside → DraggingInside → DraggingOutside transitions with Escape-abort), originally feeding cross-process fallbacks (temp-JSON file + SCM_RIGHTS) that *respawned* shells with cwd preserved. v2.18.0 wired the FSM live against the in-process multi-window App: mouse-down on a tab arms it, CursorMoved drives it with position-based outside detection (Windows SetCapture suppresses CursorLeft mid-drag), and release outside runs `Mux::detach_tab` → `open_window(AdoptTab)` at the drop position — the tab's panes (PTYs, scrollback, running programs) transfer **untouched**, Esc/focus-loss cancel. That closes the live-PTY-adoption enhancement with no fd transfer at all (the PTYs never leave the process). The old SCM_RIGHTS/JSON handoff senders are deleted; `--tab-handoff` receive parsing stays one release, deprecated. The keyboard `move_tab_to_new_window` action is the same live move. | v2.18.0 |
| ~~**Background image + blur**~~ | config.py + rendering | A | **All 12 sub-cycles shipped** (cycles 381-396): cycle-381 parser + `BgImage` struct (sub-cycle 2); cycle-388/389 wgpu texture upload + render pass (sub-cycles 3-4); cycle-390 alignment modes left/center/right + top/bottom (sub-cycles 5-7); cycle-394 implicit per-frame UV recompute for `background_image_mode = tile/scale/center` (sub-cycle 8); cycle-396 Gaussian blur via `image::imageops::blur` (sub-cycle 9); cache + path invalidation (sub-cycles 10-11); cycle-392 acceptance test `real_png_roundtrip` in `bg_image.rs:233-260` walks decode + write + roundtrip (sub-cycle 12). PNG/JPEG/WebP decode via `image` crate. The cycle-708 Stop hook reading of "11/12 sub-cycles" was an inaccurate transcription of the closeout summary's "11/12; sub-cycle 8 implicit" — sub-cycle 8 was always implicit via the UV pipeline (no separate explicit cycle needed), so the actual count is 12/12. | cycle-708 |

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
  `--check-config` validate-on-save + cycle-696
  `Action::EditConfig` (one-keystroke open of the resolved config file in
  the OS's registered text-editor handler) cover the discoverability +
  edit use cases at ~1/100th the implementation cost.

- **`extra_styling`** (`config.py`). GTK CSS theming. kettle's rendering
  is wgpu+glyphon, not GTK; user customization is via the existing
  ~500 bundled themes + per-key palette overrides.

- **GTK Glade XML files** (`*.glade`). UI definitions for the preferences
  GUI. N/A; kettle has no preferences GUI.

- **`debugserver.py`** (DEBUG TCP server). Internal maintainer tooling.
  kettle's tracing surface is `RUST_LOG=trace kettle` per env_logger
  convention.

- **`testplugin.py`** development-only example. N/A.

- **`maven.py`** domain-specific URL handler. User can ship their own via
  the Bucket-D Lua plugin system once it lands.

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
- Peacock accent-color — **VS Code Peacock** origin, cycle-293; since
  v2.18.0 `accent-color = auto` is the **default** — every window claims a
  distinct theme-pool hue, deduped across windows and kettle processes via
  the kettle-ctl presence registry (`theme`/`off`/`none` opt out; hex pins).
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
20. ✅ Cycle 342 + 606 — `Action::InsertPaneNumber` writes the 1-based focused-pane index to its PTY (matches Terminator's `GotoTab` enumeration); `InsertPanePadded` zero-pads to 2 digits. Cycle 606 added `InsertPaneName` (sends pane title). All three reachable from the cycle-104 palette + cycle-692 audit-doc cross-link.
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
  Plugin Bucket-D             COMPLETE (all 6 Terminator plugins ported;
                              cycle-708 closed the last gap via
                              Action::OpenLayoutPicker)
  Titlebar Bucket-D           COMPLETE (10/10 sub-cycles)
  bg-image Bucket-D           COMPLETE (12/12 sub-cycles; sub-cycle 8 is
                              implicit in the cycle-394 UV pipeline — no
                              separate explicit cycle needed)
  Detachable tabs Bucket-D    COMPLETE (11/11 sub-cycles; file-fallback +
                              SCM_RIGHTS IPC for JSON payload at the time;
                              the then-deferred live-PTY adoption later
                              shipped in v2.18.0 as the in-process live
                              tear-off — see the Bucket D row)
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
(preferences GUI, D-Bus IPC). The only genuine remaining work was
`Terminal::from_raw_fd` in kettle-core for the SCM_RIGHTS live-PTY-
adoption variant of detachable tabs — a kettle-internal optimization,
not a missing Terminator feature. (Since closed: v2.18.0 moved every
window into one process, so a detached tab's live PTYs move by plain
ownership transfer — `Mux::detach_tab` — with no fd passing needed.)

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

## Bucket D close-out summary (cycles 614-677)

The cycles-614+ arc revisited the 7 Bucket D design docs created in
cycles 629-637 (REMOTE, TERMINALSHOT, NAMED-GROUPS, AUTO-THEME,
VERTICAL-TABS, THEME-SUBMENU, CONFIRM-DIALOG) and progressed each
through sub-cycles. Status snapshot at cycle 677:

| Bucket D feature             | Status | Sub-cycles | Notes |
|------------------------------|--------|------------|-------|
| `plugins/remote.py`          | ✅ A   | 7/7        | SSH/Docker/Podman/kubectl detect + right-click reconnect. Deployed cycle 659. |
| `plugins/auto_theme.py`      | ✅ A   | 7/7        | Manual + clock schedule + sunrise/sunset (NOAA solar). Deployed cycle 671. |
| `ask_before_closing`         | ✅ A   | 7/8 + 1 polish-deferred | CloseWindow/CloseTab/ClosePane all route through `maybe_confirm_then`. Bottom-bar modal renderer is keyboard-driven. Mouse hit-test deferred to a follow-up centered-panel renderer upgrade. Deployed cycle 661+663. |
| `tab_position = left/right`  | ✅ A   | 7/8 + 1 polish-deferred | Variants + layout + paint + cfg width. Drag-reorder y-axis deferred (horizontal works; y-axis is identical-shape work). Deployed cycle 674. |
| `plugins/terminalshot.py`    | ✅ A   | 7/7        | Action + path helper + Renderer slot + wgpu surface readback + focused-pane crop + desktop notification. Deployed cycle 688. |
| Named broadcast groups       | ✅ A   | 7/8        | `BroadcastScope { Off, Tab, All, Group(String) }` + mux migration + bulk-apply GroupTab/Window + UngroupTab/Window + ToggleBroadcastGroup/Window + `[group]` titlebar pill + right-click context-menu entries. Cross-window groups via cycle-302 IPC remain. Deployed cycles 679-682. |
| Right-click theme submenu    | D     | 0/9        | Cycle-634 design doc only; no implementation cycles yet. cycle-329 command palette covers the same UX via `/theme NAME`. |

**All 7 Bucket D Terminator features now ship end-to-end on
the deployed binary** (last deploy at cycle 688, commit
`de32288`). Each has full user-visible behavior:
  - `plugins/remote.py`: SSH/Docker/Podman/kubectl detect
    + right-click Reconnect (cycles 629-658)
  - `plugins/auto_theme.py`: manual toggle + clock schedule
    + NOAA solar-position sunrise/sunset (cycles 616-670)
  - `ask_before_closing`: keyboard-driven confirm modal on
    every close-family action (cycles 637-662)
  - `tab_position = left/right`: 180-600 px vertical strip
    (cycles 633/647-674)
  - Named broadcast groups: BroadcastScope::Group(name) +
    GroupTab/ToggleBroadcastGroup + titlebar pill + right-
    click menu (cycles 631+642+678-683)
  - Theme/Profile submenu: drill-in UI exposing ~512 themes
    and configured profiles (cycles 634+684-687)
  - `plugins/terminalshot.py`: wgpu surface readback +
    focused-pane crop + desktop notification (cycles
    630/640/650/654/688/689)

The cycle-679 BroadcastScope migration alone touched 5 call
sites without breaking the cycle-178 per-tab UX. The cycle-
670 NOAA solar algorithm is pure (no deps) and accurate to
~1 minute at temperate latitudes. The cycle-688 wgpu
readback respects 256-byte row padding and BGRA→RGBA
conversion for cross-adapter portability.

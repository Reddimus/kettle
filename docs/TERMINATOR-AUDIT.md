# Terminator parity audit

## Method

This document is the systematic feature-by-feature audit of GNOME Terminator
(<https://github.com/gnome-terminator/terminator>) against kettle.

Audited Terminator SHA: `403fa1e51acbf2ee51afa0f34b78eb2cd79b86e0` (master at
clone-time, 2026-05-21). Re-run the audit against a fresher SHA by re-cloning
into `/tmp/terminator` and re-walking `terminatorlib/` — the per-module
sections below are append-only; new features get new rows in the gap table.

Status: re-verified 2026-06-12 by the v2.20.0 Terminator + Ghostty deep-dive
cross-check — the parity claims held against source (residual gaps tracked in
`docs/UX-COMPARISON.md` § "v2.20.0 Terminator + Ghostty deep-dive").

Correction pass, 2026-08-03. A machine check of this document against both
sources — every Terminator option name it cites, checked against a clone of
the audited SHA; every kettle symbol it names, checked against the Rust
sources under `crates/` — found claims that no source supported:

- Options attributed to Terminator that Terminator does not have, at any SHA:
  `hide_titlebar` (the real key is `show_titlebar`, and the sense was
  inverted), `tab_max_width`, and `use_login_shell` (the real key is
  `login_shell`, which was already covered by its own row).
- A kettle function credited by name in two places, `maybe_confirm_then`,
  that exists in no Rust source. It is not a typo for something real: it was
  specified as pseudocode in
  [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](TERMINATOR-CONFIRM-DIALOG-DESIGN.md)
  and announced in `CHANGELOG.md`, and then the feature shipped a different
  way. Design and release prose recorded the plan; nothing checked that the
  plan is what landed, and this document repeated the name as evidence.
- `ask_before_closing` described as "complete end-to-end" while four close
  gestures bypassed the prompt entirely.

Those rows are corrected below. A claim in this document is worth only as
much as the source behind it, so prefer citing a file and symbol that can be
checked over an adjective that cannot.

For every Terminator feature, this doc classifies it into one of five buckets:

- **A — Already shipped.** kettle has the equivalent. Cite the kettle cycle.
- **B — Trivial gap.** Single config-key alias, enum variant, or keybind. One
  cycle's worth of work each.
- **C — Single-cycle feature.** New config key + parser arm + drift guard
  + behavior. Same shape as `status-bar`, trigger validation, and
  `--profile` + `--check-config`.
- **D — Multi-cycle feature.** Warrants its own design doc + phased plan
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
`close_button_on_tab`, `scroll_tabbar`,
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
| Alt+L | layout_launcher | — | B (add a layout-launcher overlay; `--layout NAME` launch-time loading is partial coverage) |
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
| Ctrl+Shift+Z | scaled_zoom | A | `Action::ScaledZoom` toggles `Mux::toggle_zoom` + scales font 1.5× on enter / restores saved size on exit (font tracked via `App::scaled_zoom_prev_font_size: Option<f32>`) |
| Ctrl+Shift+Alt+A | hide_window | — | C (`Action::ToggleVisibility` via the `--toggle` infra) |
| Super+G | group_all | `GroupAll` — puts every pane in the group named `All`. (This row previously claimed `ToggleBroadcastAll` was a "semantic match". It is not: grouping and broadcasting are different operations upstream, and the mapping meant one press armed input duplication.) | A |
| Super+Shift+G | ungroup_all | `UngroupAll` | A |
| Super+T | group_tab | — | C (per-tab broadcast group; kettle has per-tab broadcast but no named group) |
| Super+Shift+T | ungroup_tab | — | C |
| Super+Shift+W | ungroup_win | — | C |
| (unbound) | create_group | — | C |
| (unbound) | broadcast_off/group/all | `ToggleBroadcastOff/All` (group mode = per-tab) | A (partial; group-mode is Bucket C) |
| Ctrl+Shift+C | copy | `Copy` | A |
| Ctrl+Shift+V | paste | `Paste` | A |
| (unbound) | paste_selection | `PastePrimary` | A (PRIMARY-first on Linux with clipboard fallback elsewhere; shared clamp/bracketed/broadcast paste path) |
| Shift+Return | send_newline | A | `Action::SendNewline` writes a literal `\n` to the focused pane's PTY. Useful for shell line-editors that consume Enter normally but expect explicit `\n` for line continuation. Palette + 2 name aliases (`send_newline`, `send-newline`). Drift guard `from_name_accepts_send_newline_aliases` covers both. |
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
| (unbound) | preferences / preferences_keybindings | A | `Action::EditConfig` opens the user's resolved config file (`App::config_path` → `Config::default_path` fallback) via `open::that_detached`, which respects the OS's registered text editor handler. Closes the "preferences GUI is a paradigm choice" Bucket E rationale by making the equivalent UX one keystroke away. 7 keybind name aliases: `preferences`, `preferences_keybindings`, `preferences-keybindings`, `edit_config`, `edit-config`, `open_config`, `open-config`. Drift guard `from_name_accepts_edit_config_aliases` covers all 7. |
| F1 | help | A | `Action::ShowHelp` opens the kettle README on GitHub via `open::that_detached` (the same cross-platform dispatch path URL clicks already use). Reachable from the command palette + 5 name aliases (`help`, `show_help`, `show-help`, `open_help`, `open-help`). Drift guard `from_name_accepts_show_help_aliases` covers all five. |
| (unbound) | page_up/down/_half | — | A (kettle has `ScrollPageUp/Down`; half-page is B) |
| (unbound) | line_up/down | `ScrollLineUp/Down` | A |

### `terminatorlib/terminator.py` — master singleton

App-wide state container (the Borg pattern). Tracks all windows, all
terminals, all groups. Coordinates `group_emit` (broadcast within a group)
and `all_emit` (broadcast to every terminal). Layout loading.

kettle equivalent: `kettle_ui::Mux` is the per-window analog.
Since v2.18.0 kettle has the cross-window coordinator too: every window
lives in one process, owned by `App`'s `windows: BTreeMap<u64, WindowState>`
map (`crates/kettle-ui/src/window_state.rs`) — the in-process equivalent of
Terminator's Borg singleton. (The file-based IPC still bridges *separate*
kettle processes.) Bare GUI launches now elect a per-user primary over private
local IPC and ask it to open a new in-process window; explicit launches remain
separate, with `--new-process` as the default-launch escape hatch.

### `terminatorlib/window.py` — top-level GTK Window

GTK window with HINT_WINDOW_TYPE_NORMAL, decorations, geometry hints. Owns
the top-level Notebook (tabs) or single Paned (no tabs).

Key features:
- Fullscreen toggle (F11) — kettle ✅ `ToggleFullscreen`.
- Close confirmation dialog ("Quit Terminator?") — kettle has none. → B.
- Group menus + group management — partial parity via broadcast actions.
- Window-state save: kettle has `--layout NAME`. ✅.

### `terminatorlib/notebook.py` — tab container

GTK Notebook with closable tabs, drag-to-reorder, right-click context menu,
detachable tabs.

kettle equivalent: `kettle_ui::Mux::tabs`. Drag-to-reorder ✅ (dragged-tab ghost). Detachable tabs (drag to new window) ✅ DONE — live
in-process tear-off shipped in v2.18.0: drag a tab outside the window and
`Mux::detach_tab` → `open_window(AdoptTab)` moves its panes (running
programs, PTYs, scrollback untouched) into a new window at the drop
position (`crates/kettle-ui/src/detach.rs` DragState FSM + gap-table row;
the earlier cross-process fallbacks respawned shells and their
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

kettle **ships per-pane titlebars**. Layout
reserves `ch + 6.0` pixels of chrome per pane when
`show_titlebar = true` AND there are >1 panes in a tab (single-pane
tabs hide the titlebar). Terminator's own option is `show_titlebar`,
default `True` (`config.py:238`); there is no `hide_titlebar` anywhere in
Terminator's source, and an earlier revision of this document cited one.
Label format:

  `[group_name]  title  COLSxROWS  [bell]`

with:

- `[group_name]` prepended when the pane has a broadcast
  group set (phase 6 of `TERMINATOR-NAMED-GROUPS-DESIGN.md`).
- `COLSxROWS` shown unless `title_hide_sizetext = true`.
- `[bell]` indicator shown when `icon_bell = true` and the pane has
  a pending bell.

Hit-testing for inline title edit (`Action::EditPaneTitle`) and
focus/group indicators is wired through the title-edit
overlay. Activity / silence / bell dots ride alongside the
tab-bar dots (the activitywatch.py + sleeping.py / silence.py plugin
events both fan into the shared activity watcher).

Row promoted to A from Bucket D during an audit cleanup pass.

### `terminatorlib/searchbar.py` — search overlay

case_sensitive, invert_search.

kettle ✅ search overlay (`Ctrl+Shift+F`). The current bar follows
Terminator's compact bottom-lane model and exposes Previous, Next, Wrap,
Case, Invert, and Close beside a Unicode/grapheme-aware editor. Case cycles
**Smart** (lowercase pattern → insensitive, uppercase present → sensitive),
**Match** (always sensitive, Terminator's default), and **Ignore** (always
insensitive); `invert-search` flips the default Enter direction. The settings
persist through both the bar and Settings → Search.

Kettle intentionally differs in bounded ways: patterns are strict Rust regexes
compiled by `regex-automata`'s meta engine and capped at 4096 UTF-8 bytes; the
status does not claim a global count; and history work is incremental
(a nearby 1000-line range, then 500 ms idle traversal) with at most one bounded
core work slice per event-loop turn and nearby projection capped at 65,536
signed spans. Logical-line materialization is capped at 256 rows, 262,144
inspected cells, and 64 KiB. Each bounded call also has 64 KiB
aggregate text, 262,144-cell, and 256-logical-haystack work budgets; exact
continuations resume, while an in-line capacity barrier reports **Results
limited** instead of a possibly wrong ordering. Engine construction is capped
at 512 KiB NFA, 256 KiB one-pass/hybrid, and 40 KiB DFA. Continuous output
preserves chunk progress. Only non-navigation work gets a fresh verification
after 500 ms quiet; output-interrupted explicit navigation waits for a user
retry. These choices preserve responsive infinite/large scrollback while
fixing the old failure to highlight negative-line history and ordinary
soft-wrapped matches. See
[AUDIT-2026-07-22-SEARCH.md](AUDIT-2026-07-22-SEARCH.md) for the frame evidence
and baseline reproduction.

### `terminatorlib/terminal_popup_menu.py` — right-click menu

Items: Open link / Copy address (when clicked on a URL), Copy, Paste,
Set Window Title, Split Auto/Horiz/Vert (if not zoomed), Open Tab,
Close, Zoom/Maximize/Restore, Grouping submenu (if titlebar hidden),
Read-only toggle, Show scrollbar toggle, Preferences, Theme presets.

kettle ✅ context menu. Theme-preset submenu ✅ (Theme ▸ / Profile ▸
flyouts). **Read-only toggle ✅** — right-click "Read only" check item +
`toggle_read_only` keybind/palette action; per-pane `Pane::feed_input`
gate drops keystroke / paste / IME / drag-drop / Lua / remote.cmd /
agent input (VTE `input-enabled` semantics: protocol replies keep
flowing), `[RO]` titlebar badge, agent `send_text`/`run_command` get an
explicit `read_only` error. **Open link / Copy address ✅** — URL-aware
leading rows when the right-click lands on a detected hyperlink; Open
routes through the `open_url` chain (Lua URL handlers →
custom_url_handler → system open), Copy puts the address on the
clipboard.

### `terminatorlib/prefseditor.py` — preferences GUI

Full GTK preferences dialog with tabs: Global, Profiles, Keybindings,
Layouts, Plugins.

kettle: **Bucket E**. Deliberate divergence — kettle is config-file-
driven by design. The `kettle --print-default-config > ~/
.config/kettle/config` first-launch bootstrap covers the discoverability
use case at ~1/10th the implementation cost.

### `terminatorlib/layoutlauncher.py` — layout picker dialog

GTK dialog listing saved layouts; Load / Create / Edit.

kettle: `--layout NAME` ships launch-time. Runtime overlay
(`Alt+L`) — Bucket C (new Action + overlay similar to the quick-select
hints).

### `terminatorlib/plugin.py` + `plugins/*.py` — plugin system

PluginRegistry (Borg singleton). Capability-based registration. Base
classes: `Plugin`, `MenuItem`, `URLHandler`. Plugin discovery from
`terminatorlib/plugins/` + `~/.config/terminator/plugins/`.

kettle: **Bucket D**. Lua scripting (the scripting foundation, `send_text`,
and `exec_action` support) is the natural mapping. Each Terminator
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

kettle: **Bucket E with partial alternative**. The file-based IPC
(`--remote-send TEXT`, `--toggle`) covers the cross-process control use
cases cross-platform (Linux/macOS/Windows). D-Bus would be Linux-only
and would duplicate the existing IPC surface. A future addition could
add specific D-Bus message types to bridge for users who want them.

### `terminatorlib/regex.py` — VTE URL regex patterns

PCRE2 patterns for URL / email / VoIP matching, fed to VTE for
underline-on-hover detection.

kettle: ✅ `kettle_core::hints` ships equivalent URL / path /
IPv4 / SHA regex set (driven by the Rust `regex` crate, not VTE's
PCRE2; same matches).

### `terminatorlib/util.py` — utilities

dbg, err, spawn_new_terminator, get_cwd, enumerate_descendants, etc.
kettle's `kettle_core::cwd` (OSC 7 cwd tracking) is the equivalent.

### Plugins

| Plugin | Purpose | kettle bucket | Notes |
|---|---|---|---|
| `activitywatch.py` | Highlight tab on activity | A | tab-activity dot |
| `inactivitywatch.py` | Highlight on inactivity period | A | silence-watcher dot (`tab-silence-threshold-ms`) |
| `silencewatch.py` | Same as inactivitywatch | A | same as above |
| `command_notify.py` | Notify when long command finishes | A | OSC 133 + `command-notify-threshold-ms` |
| `save_last_session_layout.py` | Auto-save layout on exit | A | `session.json` + `--layout` support |
| `save_user_session_layout.py` | Manual save/load named layouts | A | `--layout NAME` |
| `url_handlers.py` (Launchpad bug + code + APT) | Open URLs in browser | A | kettle Ctrl/Cmd+click opens URLs via `open` crate (cross-platform); `docs/examples/init.lua` ports the three Launchpad/APT handlers as Lua `kettle.add_url_handler` recipes |
| `mousefree_url_handler.py` | Keyboard URL selection | A | hint mode (Ctrl+Shift+H) — `kettle.on('url_match')` could extend |
| `run_cmd_on_match.py` | Run command on regex match | A | `trigger = REGEX :: cmd arg1 arg2` extends the existing trigger syntax. `TriggerAction::RunCommand(Vec<String>)` carries the argv; fire-and-forget spawn via `std::process::Command`. No shell expansion at kettle's layer (argv form, security posture: "config command is data, not shell"). Capture-group substitution deferred to a follow-up. |
| `custom_commands.py` | Custom menu items | A | `menu-item = LABEL = CMD` config + Lua `kettle.add_menu_item` |
| ~~`remote.py`~~ | SSH/Docker/Podman session detection | A | Shipped across a phased rollout, from initial design through detector coverage. `kettle-remote` crate with sysinfo-backed BFS process-tree walk, SSH + Container detectors (22 argv shapes covered), `Terminal::child_pid()`, App's 5 Hz poll loop, pane-title flip on detect change, right-click "Reconnect" menu entry. Deployed. |
| `logger.py` | Log terminal output to file | A | `Action::ToggleSessionLog` (aliases: `start_logger`/`stop_logger`/`toggle_session_log`) — opens `<cache>/kettle/logs/kettle-<secs>-<pid>.log`, tee's raw PTY bytes via per-Terminal `Arc<Mutex<Option<File>>>` log_file slot in the reader thread. No ANSI stripping (preserves replayable output). Best-effort I/O (errors swallowed). |
| ~~`terminalshot.py`~~ | Screenshot focused terminal | A | `Action::TakeScreenshot` queues one bounded `ScreenshotRequest { out_path, crop }`. The frame copy is encoded in the normal render submission; a single lazy worker performs the finite GPU wait, mapping, BGRA→RGBA conversion, crop, PNG encode, and write so the winit/Wayland event loop never blocks on readback. A concurrent request receives an explicit busy error. The focused-pane PNG appears at `<cache>/kettle/shots/kettle-<secs>-<pid>.png`; whole-window screenshots remain available through `--screenshot=PATH`. Hardened in v2.36.1. |
| `dir_open.py` | Open cwd in file manager | A | `Action::OpenCwdInFileManager` (file:// URL → `open` crate) |
| `insert_term_name.py` | Insert pane name into input | A | `Action::InsertPaneName` (writes pane title to PTY) |
| `maven.py` | Maven artifact URL handler | E | domain-specific; user can add via Lua plugin |
| `auto_theme.py` | Switch theme on time of day / system | A | `light-theme` + `dark-theme` config keys + `Action::ToggleLightDark` runtime swap (`toggle_light_dark` keybind alias). Sunrise/sunset auto-detect deferred to a follow-up. |
| `testplugin.py` | Example for development | E | dev-only |

## Gap table

The full feature-by-feature ledger. Rows flip from B/C → ✅ A as cycles land.

### Bucket A — already shipped

(Confirmation only. No action required.)

| Terminator feature | source | kettle status |
|---|---|---|
| `scrollback_lines` / `scrollback_infinite` | config.py | ✅ `scrollback-limit` accepts integer + `infinite`/`unlimited`/`0` |
| `copy_on_selection` | config.py | ✅ `copy-on-select` config key |
| `mouse_autohide` | config.py | ✅ `mouse-hide-while-typing` config key (also accepts `mouse_autohide` / `mouse-autohide` as direct aliases — drift-guarded in `mouse_hide_while_typing_default_and_parse`) |
| `scroll_on_keystroke` | config.py | ✅ same name |
| `scroll_on_output` | config.py | ✅ same name |
| `cursor_shape` (block/ibeam/underline) | config.py | ✅ `cursor-style` accepts block/bar/beam/underline |
| `cursor_blink` | config.py | ✅ `cursor-style-blink` |
| `palette` | config.py | ✅ `palette = N=#hex` per-index |
| `foreground_color` / `background_color` | config.py | ✅ `foreground` / `background` |
| ~~`cursor_fg_color` / `cursor_bg_color`~~ | config.py | ✅ `cursor-bg-color`/`cursor_bg_color` alias `cursor-color` → theme.cursor (the block); `cursor-fg-color`/`cursor_fg_color` → theme.cursor_text (glyph under cursor). A focused block cursor renders SOLID with the under-glyph recolored (standard inverted-cursor model) |
| `font` | config.py | ✅ `font-family` + `font-size` |
| `audible_bell` / `visible_bell` / `urgent_bell` | config.py | ✅ `bell = off/visual/attention/both` |
| `background_color` opacity (via `background_darkness`) | config.py | ✅ `background-opacity` |
| `word_chars` (double-click word boundaries) | config.py | ✅ `word-delimiters` (also accepts `word_chars` / `word-chars` as direct aliases — same write target) |
| ~~`tab_position` (top/bottom/left/right/hidden)~~ | config.py | A — end-to-end and deployed: `TabBarPos::Left`/`Right` variants; `content_rect_for_with_strip` carves the configured strip width; `tab_bar_vertical` stacks segments; renderer paints vertical strips (column-shaped bg + per-segment chrome with own y/h + axis-flipped separators); `cursor_in_tab_bar` x-axis hit-test for vertical; new `tab-bar-width` config key clamped to `[40, 600]`. The drag-reorder y-axis phase is deferred as polish — horizontal drag-reorder already works; the y-axis flip is the same shape and lands when a user files a real need. |
| ~~`broadcast_default`~~ (`all` / `group` / `off`) | config.py | ✅ picks the scope the `Super+G` / `Ctrl+Shift+G` chord turns on: `group` → the active tab (kettle's long-standing behaviour and the default), `all` → the whole window, `off` → the chord cannot enable broadcast. Terminator stores it as the *initial* mode; kettle waits to be asked so a window never starts mirroring keystrokes everywhere. Its default behaves the same either way — Terminator's `group` mode with no groups assigned types only into the focused terminal. The chord is a toggle, matching Terminator's (unbound) `group_all_toggle`. |
| `scrollbar_position = right/hidden` | config.py | ✅ `scrollbar = always/auto/never` |
| split_horiz / split_vert / split_auto | keybinds | ✅ same actions |
| new_tab / close_term / close_window | keybinds | ✅ |
| cycle_next/prev / go_next/prev / go_up/down/left/right | keybinds | ✅ same |
| resize_up/down/left/right | keybinds | ✅ same |
| move_tab_right / move_tab_left | keybinds | ✅ same |
| zoom_in / zoom_out / zoom_normal | keybinds | ✅ `IncreaseFontSize` / `DecreaseFontSize` / `ResetFontSize` |
| toggle_zoom (Ctrl+Shift+X) | keybinds | ✅ `ToggleZoom` |
| full_screen (F11) | keybinds | ✅ `ToggleFullscreen` |
| search (Ctrl+Shift+F) | keybinds | ✅ `StartSearch`; Terminator-style bottom bar, strict regex, signed history highlights |
| reset (Ctrl+Shift+R) | keybinds | ✅ `Reset` |
| copy / paste | keybinds | ✅ same |
| switch_to_tab_N | keybinds | ✅ `GotoTab(N)` |
| activity / urgent / silence watchers | terminal.py + plugins | ✅ tab-bar dots |
| activity_watch / inactivity_watch plugins | plugins/ | ✅ same |
| Right-click menu (Copy/Paste/Split/Close) | terminal_popup_menu.py | ✅ context menu |
| save/load layouts | plugins | ✅ `--layout NAME` |
| URL detection + click-to-open | terminal.py | ✅ OSC 8 + URL regex |
| mousefree URL navigation | plugins | ✅ Ctrl+Shift+H quick-select hints |
| terminalshot | plugins | ✅ `--screenshot` + `--annotate` |
| Named profiles | config.py | ✅ `--profile NAME` |

### Bucket B — trivial gaps (one-cycle each)

| Terminator feature | source | kettle status |
|---|---|---|
| ~~`tab_position = left` / `right` / `hidden`~~ | config.py | ✅ `hidden` aliases to `tab-bar = off`; `left`/`right` accepted by parser + check-config but fall through to top with a log::warn (vertical tab bars are deferred Bucket C) |
| ~~`inactive_color_offset`~~ (dim unfocused term FG) | config.py | ✅ `inactive-color-offset` + `inactive-bg-color-offset` config keys both parse (lib.rs:1944/1959) and apply in kettle-render (lib.rs:1218). Separate FG + BG offsets honored. |
| ~~`allow_bold`~~ | config.py | ✅ config key, later render-wired: `let bold = cfg.allow_bold && flags.contains(Flags::BOLD)` suppresses the bold weight when false |
| ~~`bold_is_bright`~~ | config.py | ✅ config key, later render-wired: `if bold && cfg.bold_is_bright { fg = color::bright_for_bold(fg, theme) }` maps SGR-bold palette[0..8] → bright palette[8..16] |
| ~~`link_single_click`~~ | config.py | ✅ config key, mouse-wired: `url_modifier = cfg.link_single_click \|\| ctrl \|\| super` in the left-click handler opens the URL under the cursor on a bare click |
| ~~`disable_mousewheel_zoom`~~ | config.py | ✅ config key parsed; kettle has no Ctrl+wheel zoom feature today so the disable is a forward-compat stub |
| ~~`disable_mouse_paste`~~ | config.py | ✅ config key parsed and mouse-wired; middle-click paste uses the PRIMARY-first paste path unless disabled |
| ~~`putty_paste_style`~~ | config.py | ✅ config key parsed and mouse-wired; right-click pastes instead of opening the context menu |
| ~~`putty_paste_style_source_clipboard`~~ | config.py | ✅ when PuTTY right-click paste is enabled, `false` uses PRIMARY-first paste and `true` uses the regular clipboard source |
| ~~`smart_copy`~~ | config.py | ✅ config key parsed and `Action::Copy` wired; default true preserves the existing clipboard when no selection exists, false clobbers with empty text |
| ~~`clear_select_on_copy`~~ | config.py | ✅ bool config key + Action::Copy clears selection when true |
| ~~`case_sensitive`~~ (search) | config.py | ✅ `search-case-sensitive = smart\|always\|never` (incl. Terminator's `case_sensitive = true/false` shorthand) |
| ~~`invert_search`~~ | config.py | ✅ `invert-search` config key |
| ~~`force_no_bell`~~ | config.py | ✅ wired post-process override of `bell` mode |
| ~~`term`~~ | config.py | ✅ string config key (default `xterm-256color`) wired to spawned PTY env |
| ~~`colorterm`~~ | config.py | ✅ string config key (default `truecolor`) wired to spawned PTY env; WSL launches also propagate it via `WSLENV` |
| ~~`title_at_bottom`~~ | config.py | ✅ `title-at-bottom` flips the per-pane bar to the bottom. Terminal paint/clipping and UI pointer/native-IME projection share a title-position-aware grid origin, so the bottom mode does not leave a phantom top inset or shift cell hit testing. |
| ~~`scroll_tabbar` (scrollable tab bar)~~ | config.py | ✅ v2.26.0 — `scroll-tabbar` config key: when tabs overflow the bar width, the strip scrolls with ‹›arrows + the mouse wheel (Terminator's "scroll the bar"). With it off, the wheel-over-tabs gesture cycles tabs instead (kitty/iTerm2 parity). |
| `homogeneous_tabbar` | config.py | ✅ kettle always uses equal-width tabs across the available strip and does not expose the Terminator knob. (An earlier revision paired this with a `tab_max_width` option; Terminator has no such key.) The full segment is the active surface, hit target, drag target, and title budget. |
| ~~`close_button_on_tab`~~ (toggle ✕ on tabs) | config.py | ✅ `close-button-on-tab` config key wired to tab-bar render |
| ~~`borderless`~~ | config.py | ✅ bool config key, applied via winit `Window::with_decorations(false)` |
| ~~`always_on_top`~~ | config.py | ✅ bool config key, applied via winit `WindowLevel::AlwaysOnTop` |
| `sticky` (on all workspaces) | config.py | 🟡 wired on macOS via `winit::platform::macos::WindowExtMacOS::set_visible_on_all_workspaces(true)`, called post-construction (Window-level method, not a build-time attribute like `with_skip_taskbar`'s). X11/Wayland remain Bucket E (winit 0.30 doesn't expose `_NET_WM_STATE_STICKY`; would need raw-window-handle direct atom writes — heavy dep for one config key). A Terminator config that sets `sticky = true` works correctly on macOS; on other platforms the value parses without effect. |
| `hide_from_taskbar` | config.py | 🟡 wired on Windows via `WindowAttributesExtWindows::with_skip_taskbar` (winit 0.30 only exposes the API there). X11/Wayland/macOS remain Bucket E (would need raw-window-handle direct atom writes). A Terminator config that sets `hide_from_taskbar = true` works correctly on Windows; on other platforms the value parses without effect. |
| ~~`ask_before_closing = always/multiple_terminals/never`~~ | config.py | ✅ `should_prompt` helper, state types, keyboard nav state machine, renderer bottom-bar projection, and mouse hit-testing for the visible `[Cancel]` / `[Close]` buttons. Every close gesture routes through the `confirm_close` gate. **This row previously read "complete end-to-end" and credited a `maybe_confirm_then` dispatch wrapper — that name exists only in this feature's design pseudocode and its changelog entry, never in a Rust source, and the claim was wrong on the substance too: only the three close *actions* asked. The titlebar ✕, Alt+F4, the tab-bar ✕ button, and middle-clicking a tab all closed without prompting, under every setting including `always`. Fixed and drift-guarded since.** Centered-panel rendering remains optional polish. |
| ~~`exit_action = close/restart/hold`~~ | config.py | ✅ `exit-action` config key honors close/hold/restart |
| ~~`login_shell`~~ | config.py | ✅ `login-shell` config key threaded through `Terminal::new_with_env` (`kettle-ui/mux.rs`) so the spawn argv gets `-l` when true |
| ~~`geometry_hinting`~~ (font-step resize) | config.py | ✅ `geometry-hinting` config key honored via winit `with_resize_increments` (8x16 px approximation; X11 honors, Wayland varies, macOS no-op) |
| ~~`paste_selection` (X11 primary)~~ | keybinds | ✅ `Action::PastePrimary` uses X11 PRIMARY on Linux and falls back to the regular clipboard on Wayland/macOS/Windows; middle-click and PuTTY-style right-click share the same hardened paste paths |
| `send_newline` | keybinds | ✅ `Action::SendNewline` writes a literal LF when explicitly bound or selected. The default Shift+Enter is forwarded distinctly to the client (Kettle's fallback before negotiation, negotiated xterm or CSI-u afterward); it is not a hidden Kettle keybinding. |
| ~~`reset_clear`~~ (Reset + Clear) | keybinds | ✅ `Action::ResetAndClear` (composes Reset + ClearHistory) |
| ~~half-page scroll variants~~ | keybinds | ✅ `Action::ScrollPageUpHalf` / `ScrollPageDownHalf` (aliases: `page_up_half` / `page_down_half`) |
| ~~`scaled_zoom`~~ | keybinds | A — `Action::ScaledZoom` toggles `Mux::toggle_zoom` + scales font 1.5× on enter / restores saved size on exit. Idempotent across other `ToggleZoom` interactions: post-toggle `Mux::is_zoomed()` decides enter vs. leave; saved size lives in `App::scaled_zoom_prev_font_size: Option<f32>`. Palette + 3 name aliases (`scaled_zoom`, `scaled-zoom`, `toggle_scaled_zoom`). Drift guard `from_name_accepts_scaled_zoom_aliases` covers all three. |

### Bucket C — single-cycle features

| Terminator feature | source | kettle status | Status |
|---|---|---|---|
| ~~`rotate_cw` / `rotate_ccw`~~ (rotate panes) | paned.py + keybinds | ✅ `Action::RotateCw` / `RotateCcw` — turns the visible tab's whole layout a quarter turn, matching `rotate_recursive`: every split flips axis, and swaps children with a mirrored ratio where the rectangles demand it, so the two directions are exact inverses. Leaves zoom first, then resizes the PTYs and saves. | — |
| Drag a terminal to another position | paned.py drag-and-drop | A | Partial: the rearrangement itself ships as `move_split:{up,down,left,right}` (`Mux::move_pane_beside` -- lift the pane, collapsing whatever split it leaves, then graft it beside the target on the chosen side), reusing `goto_split`'s neighbour search so a pane lands where focus would have gone. The mouse GESTURE is not implemented: it needs drop-target hit-testing and a drag preview, which is interaction design rather than tree surgery. |
| ~~`hide_window`~~ (Ctrl+Shift+Alt+A; toggle window visibility) | keybinds | ✅ `Action::ToggleWindowVisibility` (wires the file-based IPC path directly) | — |
| ~~`group_tab` / `ungroup_tab` / `group_win` / `ungroup_win`~~ | keybinds | A | Complete end-to-end and deployed. `BroadcastScope { Off, Tab, All, Group(String) }` enum; `compute_broadcast_targets` pure helper; `mux.broadcast` field migrated bool → enum; `GroupTab/Window` open the title-edit overlay with bulk-apply; `UngroupTab/Window` directly clear group_name on every pane in scope; `ToggleBroadcastGroup/Window` actions switch scope at runtime; pane titlebar shows `[group_name]` pill. A named group now broadcasts across every window, matching Terminator's process-wide `self.terminator.terminals` scope -- `group_all` already spanned windows, so grouping panes in two of them and typing used to reach half the group with nothing on screen to explain it. Both user-input paths cross: keystrokes and paste. A SEPARATE kettle process is still its own broadcast domain, which is not a divergence -- Terminator is single-process and has no equivalent. |
| ~~`create_group`~~ | keybinds | A | `Action::CreateGroup` shares dispatch with `EditPaneGroup` (title-edit overlay with `TitleEditScope::Group`). A follow-up added right-click context-menu entries: "Set Group…" / "Group This Tab…" / "Ungroup This Tab". |
| ~~`zoom_in/out/normal_all`~~ (broadcast zoom) | keybinds | ✅ `Action::ZoomInAll` / `ZoomOutAll` / `ZoomNormalAll` (kettle's font-size is window-wide so they compose into the single-pane zoom) | — |
| ~~`toggle_scrollbar`~~ (runtime show/hide) | keybinds | ✅ `Action::ToggleScrollbar` cycles Never → Always → Auto → Never | — |
| ~~`edit_window_title` / `edit_tab_title` / `edit_terminal_title`~~ | keybinds | ✅ `Action::EditWindowTitle` / `EditTabTitle` / `EditPaneTitle` with inline title-edit overlay (`TitleEditState`); a follow-up added `EditPaneGroup` for the broadcast-group name | — |
| ~~`insert_number` / `insert_padded`~~ | keybinds | A | `Action::InsertPaneNumber` writes the focused pane's index (mux pane-order) to its PTY as ASCII (e.g. `0`/`1`/`2`); `InsertPanePadded` zero-pads to 2 digits (`00`/`01`). A follow-up added `InsertPaneName` (sends pane title). All three covered by palette + name aliases (`insert_number` / `insert-number` / `insert_pane_number`, and `_padded` variants). |
| ~~`next_profile` / `previous_profile`~~ | keybinds | ✅ `Action::NextProfile` / `PrevProfile` cycle `<config>/profiles/*.config` at runtime; a follow-up refactored to use `Config::list_profiles` + `profile_name_from_path` + pure `pick_next_profile` helper | — |
| Theme presets in right-click menu | terminal_popup_menu.py | D | Phased design in [`TERMINATOR-THEME-SUBMENU-DESIGN.md`](TERMINATOR-THEME-SUBMENU-DESIGN.md). Adds `ContextMenuItem::Submenu { label, items }`, hover-delay state machine, flyout layout + edge-flip clipping, populated by `Theme::list()` and `Config::list_profiles()`. Multiple phases planned. |
| ~~Layout launcher overlay (Alt+L)~~ | layoutlauncher.py | A | `Action::OpenLayoutPicker` opens a runtime modal listing `Session::list_layouts()` (walks `<config-dir>/layouts/*.json`). Type-to-filter via pure `rank_layouts(query, layouts)`; Enter spawns `kettle --layout NAME` as a new window via `std::env::current_exe()`. Same UX shape as the command palette but with its own modal state (`App::layout_picker_input: Option<(String, usize)>`), render hook (Overlay `layout_picker_query` / `layout_picker_hint`), and keyboard handler (`App::layout_picker_key`). 6 keybind name aliases (`layout_launcher`, `layout-launcher`, `open_layout_picker`, `open-layout-picker`, `layout_picker`, `layout-picker`). Drift guard `rank_layouts_filters_by_tokens_case_insensitive` walks 8 cases (empty query, whitespace query, single token, multi-token AND, case folding, no-match, empty list). Closes the last Bucket-D plugin gap (`launcher.py`). |
| ~~`command_notify`~~ (long-running command done) | plugins | ✅ OSC 133 CommandEnd duration → `notify-rust` when window unfocused, gated by `command-notify-threshold-ms` | Shipped |
| ~~`run_cmd_on_match`~~ (run cmd on regex match) | plugins | ✅ `trigger = REGEX :: argv` + `TriggerAction::RunCommand(Vec<String>)` + fire-and-forget spawn | Shipped |
| ~~`custom_commands`~~ (user-defined context menu items) | plugins | ✅ `menu-item = LABEL = CMD` config key splits on first `=`, writes CMD\n to focused pane PTY on click | Shipped |
| ~~`remote.py` (SSH/Docker/Podman detection)~~ | plugins | A | Shipped across a phased rollout, fully deployed | Shipped |
| ~~`logger.py`~~ (log session to file) | plugins | ✅ `Action::ToggleSessionLog` opens `<cache>/kettle/logs/...` and writes raw PTY bytes from reader thread via per-Terminal `Arc<Mutex<Option<File>>>` log_file slot | Shipped |
| ~~`dir_open.py`~~ (open cwd in file manager) | plugins | ✅ `Action::OpenCwdInFileManager` builds `file://{cwd}` via `open_url()` (which uses the `open` crate) | Shipped |
| ~~`auto_theme.py`~~ (light/dark switching) | plugins | A | Shipped in full. `light-theme`/`dark-theme` config + `Action::ToggleLightDark` (manual toggle) + `theme-mode` enum + `theme-schedule = HH:MM dark, HH:MM light` clock variant + `theme-schedule = sunrise/sunset` + `theme-schedule-lat`/`long` config + NOAA solar-position algorithm + App-side poll loop firing flips on boundary crossings. Privacy: no network, no GeoClue2/CoreLocation. | Shipped |
| ~~`cell_width` / `cell_height`~~ (per-character cell scaling) | config.py | ✅ Multiplier applied to measured cell metrics at construction + on reload. `Renderer::set_cell_scale(w, h)` setter (no-op when unchanged); `reload_config` picks it up alongside font-family/size changes. Range pre-clamped at parse to `[0.5, 3.0]`. | Shipped |
| ~~`palette = solarized_dark`~~ (named preset) | config.py | A | Parser accepts `palette = NAME` (no `=` after) as an alias for `theme = NAME`. Direct bundled-name match first; underscore→space fallback handles Terminator's `solarized_dark` convention by falling back to e.g. "Solarized Darcula" / closest bundled match via `Theme::find_name`. Per-slot `palette = N=#hex` form unchanged. Drift guard `palette_named_preset_alias` covers 4 input shapes. | Shipped |
| ~~Multiple grouping modes + auto-cleanup~~ | config.py | ✅ | Named groups shipped per [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](TERMINATOR-NAMED-GROUPS-DESIGN.md). `autoclean_groups` is wired (`Mux::hoover_groups`): kettle has no group registry to prune — a group is just the name its panes carry — so what it clears is a broadcast scope still aimed at a group with no members, which would otherwise keep the titlebar claiming it and capture the next pane given that name. `split_to_group` (`Mux::inherit_split_group`) puts a new split in its parent's group. |
| ~~`use_custom_url_handler` + `custom_url_handler`~~ | config.py | ✅ — `open_url()` (`app.rs`) spawns `<custom_url_handler> <uri>` detached when `use_custom_url_handler` && non-empty, else falls through to the cross-platform `open` crate. Lua URL handlers get first dispatch | (covered) |
| ~~`backspace_binding` / `delete_binding`~~ (escape encoding) | config.py | ✅ — `BackspaceBinding` (lib.rs:246) + `DeleteBinding` (lib.rs:261) enums + parser arms + dispatched in `kettle-ui/src/app.rs:5884-5896` (ascii-del / control-h / escape-sequence). | (covered) |
| ~~`background_image` + mode + align~~ | config.py | A | `BgImage` module in `crates/kettle-render/src/bg_image.rs`; decode + cache via `bg_image_cache: Option<(String, kettle_core::ImageData)>`; rendered when `background_type = Image` && `background_image = <path>`. A follow-up added implicit per-frame UV recompute. | Shipped |

### Bucket D — multi-cycle (warrants design doc)

**Status: all 4 Bucket D items have shipped.** Plugin
system, Per-terminal titlebar, Detachable tabs, and Background image +
blur each progressed through their own phased roadmaps. The rows below
are kept as historical anchors with cross-references to the relevant
code modules.

| Terminator feature | source | Status | Design doc |
|---|---|---|---|
| ~~**Plugin system**~~ | plugin.py + plugins/*.py | A | Status: **COMPLETE**. Event-hook foundation via Lua scripting + the `kettle.on(event, cb)` registry. **All 9 plugin-relevant events shipped**: `startup`, `tab_add`, `tab_close`, `bell`, `pane_close`, `output`, `pane_focus`, `title_changed`, `url_clicked`. **All 6 Terminator plugins ported**: `activitywatch.py` → watcher; `custom_commands.py` → `menu-item =` config + `kettle.add_menu_item`; `terminalshot.py` → screenshot action; `logger.py` → `Action::ToggleSessionLog`; `urlhandlers.py` → URL detection + `try_url_handler` + `Action::ShowHelp`; `launcher.py` → `Action::OpenLayoutPicker` (last gap closed). | Shipped |
| ~~**Per-terminal titlebar**~~ | titlebar.py | A | Per-pane chrome reserves `ch + 6.0` px when `show_titlebar = true` && >1 panes; label format `[group] title COLS×ROWS [bell]`; title-edit overlay + activity/bell/silence dots all wired. See `### terminatorlib/titlebar.py` paragraph above for full mapping. | Shipped |
| ~~**Detachable tabs (drag across windows)**~~ | notebook.py + window.py | A | ✅ **DONE — live in-process tear-off shipped in v2.18.0; the `detachable-tabs` runtime toggle is wired.** An earlier iteration built the foundation: `crates/kettle-ui/src/detach.rs` carries the drag-state machine (Idle → ArmedInside → DraggingInside → DraggingOutside transitions with Escape-abort), originally feeding cross-process fallbacks (temp-JSON file + SCM_RIGHTS) that *respawned* shells with cwd preserved. v2.18.0 wired the FSM live against the in-process multi-window App: mouse-down on a tab arms it, CursorMoved drives it with position-based outside detection (Windows SetCapture suppresses CursorLeft mid-drag), and release outside runs `Mux::detach_tab` → `open_window(AdoptTab)` at the drop position — the tab's panes (PTYs, scrollback, running programs) transfer **untouched**, Esc/focus-loss cancel. That closes the live-PTY-adoption enhancement with no fd transfer at all (the PTYs never leave the process). The old SCM_RIGHTS/JSON handoff senders are deleted; `--tab-handoff` receive parsing stays one release, deprecated. The keyboard `move_tab_to_new_window` action is the same live move. Setting `detachable-tabs = false` disables mouse tear-off and the keyboard/palette live move while keeping tab switching and in-window reorder active. | v2.18.0 |
| ~~**Background image + blur**~~ | config.py + rendering | A | **Fully shipped**: parser + `BgImage` struct; wgpu texture upload + render pass; alignment modes left/center/right + top/bottom; implicit per-frame UV recompute for `background_image_mode = tile/scale/center` (folded into the UV pipeline, not a separate step); Gaussian blur via `image::imageops::blur`; cache + path invalidation; acceptance test `real_png_roundtrip` in `bg_image.rs:233-260` walks decode + write + roundtrip. PNG/JPEG/WebP decode via `image` crate. | Shipped |

### Bucket E — won't implement (by design)

These Terminator features kettle deliberately diverges on. Future contributors:
do not re-litigate without explicit user request.

- **D-Bus IPC service** (`ipc.py`). Linux-only. kettle's file-based
  IPC (`--remote-send`, `--toggle`, `--remote-file PATH`) covers the cross-
  process control use cases on Linux/macOS/Windows. D-Bus surface would
  duplicate the existing IPC without adding value. If a specific user needs
  D-Bus bridge bindings on Linux, that's a small Bucket C addition then.

- **Preferences GUI** (`prefseditor.py`). kettle is config-file-driven by
  design — single text file at `~/.config/kettle/config`, documented in
  `docs/CONFIG.md`, bootstrappable via `kettle --print-default-config >
  ~/.config/kettle/config`. A preferences GUI would be ~5,000 LoC of GTK-
  equivalent winit overlay work, plus ongoing maintenance for every new
  config key. The first-launch bootstrap + `--check-config`
  validate-on-save + `Action::EditConfig` (one-keystroke open of the
  resolved config file in the OS's registered text-editor handler)
  cover the discoverability + edit use cases at ~1/100th the
  implementation cost.

- **`extra_styling`** (`config.py`). GTK CSS theming. kettle's rendering
  is wgpu+glyphon, not GTK; user customization is via the existing
  ~500 bundled themes + per-key palette overrides.

- **GTK Glade XML files** (`*.glade`). UI definitions for the preferences
  GUI. N/A; kettle has no preferences GUI.

- **`debugserver.py`** (DEBUG TCP server). Internal maintainer tooling.
  kettle's tracing surface is `RUST_LOG=trace kettle`; one
  `tracing-subscriber` filter receives both `log` and `tracing` events, including
  winit backend errors.

- **`testplugin.py`** development-only example. N/A.

- **`maven.py`** domain-specific URL handler. User can ship their own via
  the Bucket-D Lua plugin system once it lands.

- **Multi-display X11 awareness** (Bus name hashing in `ipc.py`). Linux-
  specific; kettle is single-display per process by design.

## NOT-from-Terminator kettle features

These are kettle features that have NO Terminator equivalent. They should
NEVER be marked as "Terminator gaps" because Terminator's column would
have ⛔. Source-of-origin is cited:

- Smart selection (regex double-click) — **iTerm2** origin.
- Triggers (regex → urgency) — **iTerm2** origin.
- Command palette (Ctrl+Shift+K) — **Ghostty** origin.
- Quick-select / URL hints (Ctrl+Shift+H) — **kitty** origin.
- Vi-mode for scrollback (Ctrl+Shift+Space) — **Alacritty** origin.
- Remote-control IPC (`--remote-send`, `--toggle`) — **kitty `@`** origin.
- Quake dropdown (`--toggle`) — **Yakuake** / **Tilda** / **Ghostty** origin.
- Peacock accent-color — **VS Code Peacock** origin; since
  v2.18.0 `accent-color = auto` is the **default** — every window claims a
  distinct theme-pool hue, deduped across windows and kettle processes via
  the kettle-ctl presence registry (`theme`/`off`/`none` opt out; hex pins).
- Annotated screenshots (`--annotate`) — **iTerm2** caption variant.
- Status bar widget (`status-bar = top|bottom`) — **iTerm2** / **kitty** origin.
- Shell integration (OSC 133) — generic standard, not Terminator-specific.
- SSH launcher (Ctrl+Shift+S) — kettle-original fuzzy launcher.
- Font-feature OpenType tuning — **Ghostty** / **kitty** origin.
- Inline images (sixel, kitty graphics, iTerm2) — protocol-defined.
- WCAG `minimum-contrast` — **WezTerm** origin.
- Lua scripting (`--lua-script`) — **WezTerm** origin.
- tmux `-CC` parser — **iTerm2** parity; the unwired
  `kettle_vt::tmux_cc` scaffold was **removed in v2.26.0** (see TMUX-CC-DESIGN.md).

## Bucket B/C closure plan

Phase 2: close Bucket B + C cycles in this order (cheapest user-visible win first):

1. `tab-position = left/right/hidden` (B; enum + render layout). One cycle.
2. `borderless = true/false` (B; winit `set_decorations`). One cycle.
3. `always-on-top = true/false` (B; winit `set_window_level`). One cycle.
4. `allow-bold = true/false` + `bold-is-bright = true/false` (B; render glyph attrs). One cycle.
5. `link-single-click = true/false` (B; mouse-handler). One cycle.
6. `clear-select-on-copy = true/false` (B). One cycle.
7. `disable-mousewheel-zoom = true/false` (B). One cycle.
8. ✅ `term` + `colorterm` env override (B). Wired both to PTY spawn; a follow-up propagates terminal identity vars through Windows→WSL via `WSLENV`.
9. `invert-search = true/false` (B). One cycle.
10. `close-button-on-tab = true/false` (B; render tab chrome). One cycle.
11. `Action::PastePrimary` (B; X11 primary selection). ✅ Added the action; reads X11 PRIMARY on Linux with clipboard fallback elsewhere; middle-click and PuTTY-style right-click route through the correct source while keeping the shared `LOCAL_PASTE_MAX` clamp, bracketed-paste wrap, and broadcast scoping.
12. ✅ `Action::ResetAndClear` (composed Reset + ClearHistory).
13. `Action::ScrollPageUpHalf` / `ScrollPageDownHalf` (B). One cycle.
14. `exit-action = close/restart/hold` (C). One cycle.
15. `login-shell = true/false` (C; argv flag). One cycle.
16. `Action::RotateCw` / `RotateCcw` (C; split-tree rotation). One cycle.
17. `Action::ToggleWindowVisibility` (C; in-process toggle). One cycle.
18. `Action::ToggleScrollbar` (C; runtime toggle). One cycle.
19. `Action::EditWindowTitle` / `EditTabTitle` / `EditPaneTitle` (C; title-edit overlay). One cycle each (3 total).
20. ✅ `Action::InsertPaneNumber` writes the 1-based focused-pane index to its PTY (matches Terminator's `GotoTab` enumeration); `InsertPanePadded` zero-pads to 2 digits. A follow-up added `InsertPaneName` (sends pane title). All three reachable from the command palette + this audit doc's cross-link.
21. `Action::NextProfile` / `PrevProfile` (C; runtime profile cycle). One cycle.
22. `Action::ZoomInAll` / `ZoomOutAll` / `ZoomNormalAll` (C; broadcast zoom). One cycle.
23. Theme submenu in right-click context menu (C). One cycle.
24. Layout-launcher overlay (Alt+L) (C). One cycle.
25. ✅ `command-notify-threshold-ms` config key; OSC 133 CommandEnd duration → desktop notification when window unfocused.
26. `run-cmd-on-match` trigger variant (C). One cycle.
27. ✅ `menu-item = LABEL = CMD` config key + Lua `kettle.add_menu_item`.
28. ✅ `Action::OpenCwdInFileManager` (file:// URL via `open` crate).
29. ✅ `light-theme`/`dark-theme` config + `Action::ToggleLightDark` runtime swap. (Sunrise/sunset auto-detect deferred; manual chord covers the day-to-day case.)
30. `backspace-binding` / `delete-binding` (C; escape encoding). One cycle.
31. Named palette presets (`palette = solarized_dark`) (C). One cycle.
32. ✅ `disable-mousewheel-zoom = true/false` (Ctrl+wheel font zoom opt-out).
33. ✅ `smart-copy = true/false` (false → wipe-on-empty clipboard semantics).
34. ✅ `force-no-bell = true` overrides bell mode to Off.
35. ✅ Terminator-spelling keybind aliases (`new_terminator` → NewWindow, `cycle_next` → NextTab, `cycle_prev` → PrevTab).

That's 35 cycles total (several already shipped, marked ✅ above). Realistic shipping rate: 1-2 cycles per session. So the
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

This document is the Phase-1 deliverable. Phase 2 started with
`tab-position = left/right/hidden`.

## Sweep completion summary

The Terminator-parity sweep ran across 24 tagged releases (v1.8.0 →
v1.31.0). Cumulative deliverables:

  Workspace tests             286 → 308 (+22 drift guards)
  Tagged releases             v1.8.0 → v1.31.0 (24 releases)
  Bucket-D increments         46/46 effectively shipped
  Plugin Bucket-D             COMPLETE (all 6 Terminator plugins ported;
                              the last gap closed via
                              Action::OpenLayoutPicker)
  Titlebar Bucket-D           COMPLETE
  bg-image Bucket-D           COMPLETE (the per-frame UV recompute step
                              was implicit in the UV pipeline, not a
                              separate step)
  Detachable tabs Bucket-D    COMPLETE (file-fallback + SCM_RIGHTS IPC
                              for JSON payload at the time; the then-
                              deferred live-PTY adoption later shipped
                              in v2.18.0 as the in-process live
                              tear-off — see the Bucket D row)
  Plugin Lua API              7 functions + 5 event hooks + sandbox +
                              init.lua auto-load
  Action variants             20 new (including MoveTabToNewWindow
                              and EditPaneGroup)
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

## Post-sweep polish (v1.32.0 → v1.43.0, 12 releases)

After the Terminator-parity sweep landed at v1.31.0, a production-grade
hardening pass ran on the new surfaces across twelve tagged releases:
+14 tests, plus a UX-observability sweep that surfaced all 7
Terminator-parity opt-in keys in `--check-config`, plus a doc-durability
sweep that scrubbed internal dev-log references from every user-facing
surface and extended the drift guard to enforce it, plus a doc-accuracy
sweep that corrected 3 stale field doc-comments in `app.rs`, plus an
opt-in pre-commit hook (with shellcheck gate) that catches the clippy /
fmt / test / shell-script regression classes at commit time, plus a
v1.41.0 real bug fix in `scripts/release.sh` (backticks inside
double-quoted echo were running as command substitution), plus a
v1.42.0 real user-reported bug fix in `scripts/install.sh` (broken
icon-cache stub prevented GNOME from resolving Icon=kettle in
user-local installs):

  Workspace tests             308 → 322 (+14 drift guards)
  Tagged releases             v1.32.0 · v1.33.0 · v1.34.0 · v1.35.0 ·
                              v1.36.0 · v1.37.0 · v1.38.0 · v1.39.0 ·
                              v1.40.0 · v1.41.0 · v1.42.0 · v1.43.0
  Plugin-contract bug fixes   6 silent event-bypass sites covered:
                              remote-control new-tab → TabAdd;
                              3 close_tab paths → TabClose
                              (SCM_RIGHTS, file-fallback, ✕-click);
                              2 new_tab paths → TabAdd (NewWindow
                              fallback, exit-action=restart respawn)
  Real exit-action=restart    Closed the "not yet implemented" warn;
                              fixed live-grid vs hardcoded 80x24
  Refactor                    fire_tab_add_event +
                              fire_tab_close_event + drain_lua_hook_
                              commands helpers eliminate ~170 lines
                              of inline LuaCommand-variant duplication
                              across all 5 event hooks
  Docs                        ARCHITECTURE.md detachable-tabs +
                              plugin + bg-image flows upgraded ASCII
                              → mermaid; CONFIG.md
                              Terminator-parity-keys table;
                              INSTALL.md SHA-256 pin example bumped
                              v1.3.4 → v1.34.0;
                              this audit-doc tail
  Drift guards                Pinned 9 load-bearing config
                              keys in print_default_config_round_trip;
                              added Notify + SetTheme queue
                              contract tests

The kettle plugin contract is consistent across every
new_tab / close_tab / event-hook call site, with one canonical drain
path shared by all 5 LuaEvent variants (Startup / TabAdd / TabClose /
Bell / Output). Adding a sixth event is one new `fire_event` call.

## Bucket D close-out summary

This arc revisited the 7 Bucket D design docs (REMOTE, TERMINALSHOT,
NAMED-GROUPS, AUTO-THEME, VERTICAL-TABS, THEME-SUBMENU, CONFIRM-DIALOG)
and progressed each through implementation. Status snapshot:

| Bucket D feature             | Status | Progress | Notes |
|------------------------------|--------|------------|-------|
| `plugins/remote.py`          | ✅ A   | 7/7        | SSH/Docker/Podman/kubectl detect + right-click reconnect. Deployed. |
| `plugins/auto_theme.py`      | ✅ A   | 7/7        | Manual + clock schedule + sunrise/sunset (NOAA solar). Deployed. |
| `ask_before_closing`         | ✅ A   | Complete; centered-panel polish deferred | Every close gesture — the three close actions, the titlebar ✕, Alt+F4, the tab-bar ✕, and middle-click — routes through the shared `confirm_close` gate, pinned by `every_close_gesture_asks_before_closing`. Bottom-bar modal renderer supports keyboard navigation and mouse clicks on the visible `[Cancel]` / `[Close]` buttons. |
| `tab_position = left/right`  | ✅ A   | 7/8 + 1 polish-deferred | Variants + layout + paint + cfg width. Drag-reorder y-axis deferred (horizontal works; y-axis is identical-shape work). Deployed. |
| `plugins/terminalshot.py`    | ✅ A   | 7/7        | Action + path helper + Renderer slot + wgpu surface readback + focused-pane crop + desktop notification. Deployed. |
| Named broadcast groups       | ✅ A   | 7/8        | `BroadcastScope { Off, Tab, All, Group(String) }` + mux migration + bulk-apply GroupTab/Window + UngroupTab/Window + ToggleBroadcastGroup/Window + `[group]` titlebar pill + right-click context-menu entries. Cross-window groups via the file-based IPC remain. Deployed. |
| Right-click theme submenu    | D     | 0/9        | Design doc only; no implementation yet. The command palette covers the same UX via `/theme NAME`. |

**All 7 Bucket D Terminator features now ship end-to-end on
the deployed binary** (commit `de32288`). Each has full
user-visible behavior:
  - `plugins/remote.py`: SSH/Docker/Podman/kubectl detect
    + right-click Reconnect
  - `plugins/auto_theme.py`: manual toggle + clock schedule
    + NOAA solar-position sunrise/sunset
  - `ask_before_closing`: keyboard + mouse confirm modal on
    every close-family action
  - `tab_position = left/right`: 180-600 px vertical strip
  - Named broadcast groups: BroadcastScope::Group(name) +
    GroupTab/ToggleBroadcastGroup + titlebar pill + right-
    click menu
  - Theme/Profile submenu: drill-in UI exposing ~512 themes
    and configured profiles
  - `plugins/terminalshot.py`: wgpu surface readback +
    focused-pane crop + desktop notification

The BroadcastScope migration alone touched 5 call
sites without breaking the existing per-tab broadcast UX. The NOAA
solar algorithm is pure (no deps) and accurate to
~1 minute at temperate latitudes. The wgpu
readback respects 256-byte row padding and BGRA→RGBA
conversion for cross-adapter portability.

//! Keybinding model: actions, triggers, Ghostty `keybind = trigger=action`
//! parsing, and the Terminator-compatible default set.

use std::collections::HashMap;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct Mods: u8 {
        const SHIFT = 1;
        const CTRL  = 2;
        const ALT   = 4;
        const SUPER = 8;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Tab,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Trigger {
    pub mods: Mods,
    pub key: Key,
}

impl Trigger {
    pub fn new(mods: Mods, key: Key) -> Self {
        Self { mods, key }
    }

    /// Human-readable label, e.g. `Ctrl+Shift+E`, `Alt+Left`, `F5`.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.mods.contains(Mods::CTRL) {
            parts.push("Ctrl".into());
        }
        if self.mods.contains(Mods::ALT) {
            parts.push("Alt".into());
        }
        if self.mods.contains(Mods::SHIFT) {
            parts.push("Shift".into());
        }
        if self.mods.contains(Mods::SUPER) {
            parts.push("Super".into());
        }
        parts.push(match self.key {
            // Char punctuation that the parser accepts as a *named* token
            // (`plus`/`minus`/`equal`, line 354-356) should round-trip
            // through the label the same way — otherwise
            // `kettle --list-keybinds` shows the default `Ctrl++` as
            // literally `Ctrl++` (three `+` characters: separator + key)
            // and the user can't tell whether the second `+` is the
            // separator's repetition or the key itself. Cycle 170:
            // emit `Plus`/`Minus`/`Equal` so the row reads
            // `Ctrl+Plus  IncreaseFontSize`, matching how the user
            // would type the chord in their config file.
            Key::Char('+') => "Plus".into(),
            Key::Char('-') => "Minus".into(),
            Key::Char('=') => "Equal".into(),
            Key::Char(c) => c.to_ascii_uppercase().to_string(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::PageUp => "PageUp".into(),
            Key::PageDown => "PageDown".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::Enter => "Enter".into(),
            Key::Tab => "Tab".into(),
            Key::F(n) => format!("F{n}"),
        });
        parts.join("+")
    }
}

/// User-facing label for one `Action`. Most variants use Rust's `Debug`
/// derive (`Copy`, `NewTab`, `SplitRight`, …) — short enough and already
/// matches the binding-table heading style. The exception is parametric
/// variants like `GotoTab(0)` whose Debug form leaks the 0-based internal
/// index; we render the 1-based human form so `kettle --list-keybinds`
/// reads "Goto tab 1" instead of "GotoTab(0)".
pub fn action_label(a: &Action) -> String {
    match a {
        Action::GotoTab(i) => format!("Goto tab {}", i + 1),
        other => format!("{other:?}"),
    }
}

/// Human-readable lines for the default keymap, sorted by trigger label —
/// powers `kettle --list-keybinds` (no config) so the binding set is
/// discoverable without reading the source.
pub fn describe_defaults() -> Vec<String> {
    describe(&defaults())
}

/// Human-readable lines for an arbitrary binding map, sorted by trigger
/// label. Used by `kettle --list-keybinds` together with `--config FILE`
/// so the user can introspect their *effective* keymap (defaults +
/// user overrides + unbinds applied) without restarting kettle and
/// inspecting it by hand.
pub fn describe(bindings: &Bindings) -> Vec<String> {
    let mut lines: Vec<(String, String)> = bindings
        .iter()
        .map(|(t, a)| (t.label(), action_label(a)))
        .collect();
    lines.sort();
    // Column width = longest trigger label, with a floor of 16 so the
    // common shorter-default case still has breathing room. Without
    // this, `Ctrl+Shift+PageDown` (19 chars; move-tab-right) and
    // `Ctrl+Shift+PageUp` (17 chars; move-tab-left) overflowed the
    // hard-coded 16-char padding, so their action column landed one
    // or three columns to the right of every other row in
    // `--list-keybinds` — visually jarring even though every row
    // had a trigger+action pair. Same shape as `format_ssh_hosts`
    // in cycle 105.
    let width = lines
        .iter()
        .map(|(t, _)| t.len())
        .max()
        .unwrap_or(0)
        .max(16);
    lines
        .into_iter()
        .map(|(t, a)| format!("{t:<width$}  {a}"))
        .collect()
}

/// Every action kettle can bind. Names match Ghostty actions where they exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Copy,
    Paste,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    MoveTabLeft,
    MoveTabRight,
    SplitRight,
    SplitDown,
    SplitAuto,
    ClosePane,
    CloseWindow,
    NewWindow,
    FocusNext,
    FocusPrev,
    FocusUp,
    FocusDown,
    FocusLeft,
    FocusRight,
    ResizeUp,
    ResizeDown,
    ResizeLeft,
    ResizeRight,
    ToggleZoom,
    /// Cycle 695 Terminator parity (`key_help`).
    /// Terminator's F1 opens its HTML manual via `open_url`
    /// (xdg-open). kettle opens its README at the canonical
    /// GitHub URL via the `open` crate — the same dispatch path
    /// cycle-X URL clicks already use, so it works on
    /// Linux/macOS/Windows without spawning a per-platform helper.
    ShowHelp,
    /// Cycle 708 Terminator parity
    /// (`terminatorlib/layoutlauncher.py`). Open the runtime
    /// layout picker — an overlay modal that lists saved
    /// layouts from `<config-dir>/layouts/*.json` (via
    /// `Session::list_layouts`). Type-to-filter; Enter spawns
    /// `kettle --layout NAME` as a new window. Same shape as
    /// the cycle-329 command palette; uses
    /// `App::layout_picker_input: Option<(String, usize)>`.
    /// Closes the last Bucket-D plugin gap
    /// (`launcher.py` → layout overlay).
    OpenLayoutPicker,
    /// Cycle 702 Terminator parity (`key_send_newline` /
    /// Shift+Return). Writes a literal `\n` to the focused
    /// pane's PTY. Mostly useful for inserting a newline into a
    /// shell line-editor that's otherwise consuming Enter
    /// (e.g. multi-line readline prompts that submit on Enter
    /// but expect explicit `\n` for line continuation). Bucket E
    /// rationale removed since shipping this 4-line dispatch
    /// arm closes the row outright.
    SendNewline,
    /// Cycle 696 Terminator parity (`key_preferences` /
    /// `key_preferences_keybindings`). Terminator's GUI
    /// Preferences dialog is config-file-driven for kettle, so
    /// the preferences keybind opens the user's config file in
    /// $EDITOR (fallback: `open::that_detached` lets the OS pick
    /// the default text editor). Closes the "preferences GUI is
    /// a paradigm choice" Bucket E rationale by making the
    /// equivalent UX one keystroke away. Writes the path of the
    /// active config file to that pane's PTY too in case the
    /// user wants to switch editors mid-session.
    EditConfig,
    /// Cycle 756: open the in-app **Settings overlay** — a keyboard-navigable
    /// panel of the most-used config keys (font size, theme, scrollbar, bell,
    /// cursor, opacity, …) that persists changes live. Distinct from
    /// `EditConfig` (which opens the raw config file in `$EDITOR` for the long
    /// tail). This is the "settings menu for non-technical users" surface.
    OpenSettings,
    /// Cycle 717 (Preferences submenu, C8): runtime-mutable
    /// toggles that the Preferences ▸ right-click submenu wires
    /// through `Config::persist_config_toggle` (cycle 716) so a
    /// click both updates `self.cfg` AND writes the change back
    /// to `~/.config/kettle/config` atomically. Each variant
    /// targets one specific value; the submenu builder emits
    /// radio-style rows (one variant per option) for enum
    /// settings and toggle rows (one variant for the boolean)
    /// for bools.
    SetScrollbarAlways,
    SetScrollbarAuto,
    SetScrollbarNever,
    ToggleCursorBlink,
    ToggleCopyOnSelect,
    SetBellOff,
    SetBellVisual,
    SetBellAttention,
    SetBellBoth,
    ToggleMouseHide,
    /// Cycle 693 Terminator parity (`key_scaled_zoom`).
    /// Terminator's "scaled zoom" maximizes the active pane AND
    /// scales the font proportionally so text fills the larger
    /// area. Kettle pairs `Mux::toggle_zoom` with a 1.5× font-size
    /// bump on enter / restore on exit (saved size lives in
    /// `App::scaled_zoom_prev_font_size`).
    ScaledZoom,
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,
    StartSearch,
    ToggleBroadcastAll,
    ToggleBroadcastOff,
    /// Cycle 681 (Terminator parity, named-groups sub-cycle 5 of
    /// [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
    /// docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)): toggle broadcast
    /// scope to `Group(focused_pane.group_name)`. When the focused
    /// pane has no group, log + no-op. Pressing again with the
    /// same group already set switches to Off (toggle semantics).
    /// Distinct from `ToggleBroadcastAll` (which sets Tab scope).
    ToggleBroadcastGroup,
    /// Cycle 681: window-wide broadcast — every pane in every tab
    /// receives input. Terminator's true `broadcast_all`. Distinct
    /// from the misnamed cycle-178 `ToggleBroadcastAll` which is
    /// actually per-tab.
    ToggleBroadcastWindow,
    ToggleFullscreen,
    Reset,
    /// Clear scrollback only, NOT the visible screen — `CSI 3 J`
    /// (ANSI `ED 3`). Distinct from `Reset` (RIS, `\e c`) which
    /// wipes the engine to factory defaults. Same chord shape as
    /// kitty / iTerm2 / WezTerm's "Clear Buffer" / "clear_scrollback".
    ClearHistory,
    ScrollPageUp,
    ScrollPageDown,
    ScrollLineUp,
    ScrollLineDown,
    ScrollToTop,
    ScrollToBottom,
    JumpPrevPrompt,
    JumpNextPrompt,
    /// Toggle vi-mode for the focused pane's scrollback (Alacritty
    /// parity). When on, kettle intercepts keyboard input for
    /// vi-style navigation (h/j/k/l + 0/$ + g/G + visual + yank).
    /// Foundation cycle ships the entry + visible block cursor +
    /// Esc exit; movement + visual / yank come in follow-up
    /// sub-cycles.
    ToggleViMode,
    /// Cycle 342 Terminator parity (terminatorlib/terminal.py:key_rotate_cw):
    /// rotate the split tree clockwise.
    RotateCw,
    /// Cycle 342 Terminator parity: rotate the split tree counter-clockwise.
    RotateCcw,
    /// Cycle 342 Terminator parity (key_toggle_scrollbar): runtime
    /// show/hide of the scrollbar without editing config.
    ToggleScrollbar,
    /// Cycle 342 Terminator parity (key_edit_window_title): open an
    /// inline overlay to edit the window title (OSC 0/2 equivalent).
    EditWindowTitle,
    /// Cycle 342 Terminator parity (key_edit_tab_title): edit the
    /// active tab's title.
    EditTabTitle,
    /// Cycle 342 Terminator parity (key_edit_terminal_title): edit
    /// the focused pane's title.
    EditPaneTitle,
    /// Cycle 342 Terminator parity (key_insert_number): send the
    /// focused pane's index as text input.
    InsertPaneNumber,
    /// Cycle 342 Terminator parity (key_insert_padded): send the
    /// focused pane's index zero-padded.
    InsertPanePadded,
    /// Cycle 606 Terminator parity (`insert_term_name.py` plugin):
    /// send the focused pane's title (Pane::title — what the chrome
    /// shows in the per-pane titlebar) as text input. Useful for
    /// scripts that want to label their output by which pane it
    /// came from, or for keyboard-driven copy-the-current-title
    /// workflows.
    InsertPaneName,
    /// Cycle 607 Terminator parity (`dir_open.py` plugin →
    /// `CurrDirOpen` menu item): open the focused pane's current
    /// working directory in the OS file manager. Builds a
    /// `file://<cwd>` URI and routes through the existing
    /// `Action::OpenUrl` machinery (cycle 374) so the
    /// `is_safe_url` allowlist + custom-url-handler + Lua hook
    /// path all apply consistently — exactly like clicking a
    /// `file://...` hyperlink in pane output.
    OpenCwdInFileManager,
    /// Cycle 342 Terminator parity (key_next_profile): cycle to the
    /// next named profile at runtime.
    NextProfile,
    /// Cycle 342 Terminator parity (key_previous_profile): cycle to
    /// the previous named profile.
    PrevProfile,
    /// Cycle 342 Terminator parity (key_zoom_in_all): increase font
    /// size on every pane (broadcast variant of IncreaseFontSize).
    ZoomInAll,
    /// Cycle 342 Terminator parity (key_zoom_out_all): decrease font
    /// size on every pane.
    ZoomOutAll,
    /// Cycle 342 Terminator parity (key_zoom_normal_all): reset font
    /// size on every pane.
    ZoomNormalAll,
    /// Cycle 342 Terminator parity (key_reset_clear): Reset (RIS)
    /// + ClearHistory composed.
    ResetAndClear,
    /// Cycle 616 Terminator parity (`plugins/auto_theme.py`):
    /// runtime toggle between the configured `light-theme` and
    /// `dark-theme`. If the current theme matches `dark_theme`,
    /// switches to `light_theme`; otherwise switches to `dark_theme`.
    /// If neither config key is set the action no-ops (logged at
    /// `warn`). Distinct from `NextTheme` / `PrevTheme` which walk
    /// the full bundled list.
    ToggleLightDark,
    /// Cycle 621 Terminator parity (`plugins/logger.py`):
    /// toggle the focused pane's per-pane session log. When off,
    /// opens a new file at `<cache>/kettle/logs/kettle-<secs>-<pid>.log`
    /// and starts tee-ing raw PTY bytes to it (no ANSI stripping —
    /// the log preserves exact terminal output for later replay).
    /// When on, closes the file. Per-pane state (per-tab and
    /// per-window). No-op + warn when the cache dir can't be created.
    ToggleSessionLog,
    /// Cycle 640 Terminator parity (`plugins/terminalshot.py`,
    /// sub-cycle 1 of [`TERMINATOR-TERMINALSHOT-DESIGN.md`](
    /// docs/TERMINATOR-TERMINALSHOT-DESIGN.md)): trigger a live-
    /// window screenshot of the focused pane. Fully wired since
    /// cycle 688 (wgpu surface readback + BGRA→RGBA conversion +
    /// row-padding strip + image::ImageBuffer save) and cycle 689
    /// (per-pane crop via focused-pane rect + toast notification
    /// via `fire_notify`). PNG lands at the cycle-650
    /// `session_screenshot_path` (`<cache_dir>/<unix>-<pid>.png`,
    /// with the angle-bracket placeholders inside a code span so
    /// rustdoc doesn't read them as HTML tags).
    TakeScreenshot,
    /// Cycle 642 Terminator parity (sub-cycle 1 of
    /// [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
    /// docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)).
    /// `create_group` is Terminator's name for "prompt for a
    /// group name + assign it to the focused pane." Already wired
    /// since cycle 407 as `Action::EditPaneGroup`; `CreateGroup`
    /// is the Terminator-spelled alias.
    CreateGroup,
    /// Cycle 642 Terminator parity. Assign every pane in the
    /// focused tab to a named broadcast group. Wired since cycle
    /// 679 — opens the cycle-407 title-edit overlay with
    /// `TitleEditScope::Group` + bulk-apply on confirm. cycle-683
    /// also surfaces the action via the right-click context menu.
    GroupTab,
    /// Cycle 642 Terminator parity. Assign every pane in the
    /// focused window to a named broadcast group. Same wiring as
    /// `GroupTab` (cycle 679/683) but with a window-wide scope.
    GroupWindow,
    /// Cycle 642 Terminator parity. Bulk-clear the group on every
    /// pane in the focused tab. Wired since cycle 679 — walks
    /// `mux.panes_in_focused_tab()` and clears `pane.group_name`
    /// on each. Same dispatch surface as the cycle-683 right-click
    /// "Ungroup This Tab" entry.
    UngroupTab,
    /// Cycle 642 Terminator parity. Bulk-clear the group on every
    /// pane in the focused window.
    UngroupWindow,
    /// Cycle 342 Terminator parity (key_page_up_half): scroll up
    /// half a page.
    ScrollPageUpHalf,
    /// Cycle 342 Terminator parity (key_page_down_half): scroll down
    /// half a page.
    ScrollPageDownHalf,
    /// Cycle 342 Terminator parity (key_paste_selection): paste the
    /// X11 primary selection (Linux-only; no-op on macOS/Windows).
    PastePrimary,
    /// Cycle 342 Terminator parity (key_hide_window): toggle window
    /// visibility in-process. Same effect as `kettle --toggle` (cycle
    /// 303) via the remote-control IPC; this is the in-process keybind
    /// equivalent for users who don't want to set up a global hotkey.
    ToggleWindowVisibility,
    /// Cycle 384 (Terminator parity, detachable-tabs Bucket-D
    /// Wayland-fallback per docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md
    /// sub-cycle 10): move the focused tab to a new kettle window.
    /// Keyboard-driven alternative for Wayland (where cross-window
    /// cursor drag isn't feasible without global tracking). New
    /// window inherits cwd; running shells stay in the source tab
    /// (cross-process PTY transfer needs SCM_RIGHTS — multi-cycle
    /// full impl thread).
    MoveTabToNewWindow,
    /// Cycle 407 (Terminator parity, titlebar Bucket-D sub-cycle 8):
    /// open the edit overlay for the focused pane's broadcast
    /// group name. Same shape as EditPaneTitle but writes to
    /// pane.group_name. Enter empty input → clear the group.
    EditPaneGroup,
    OpenSsh,
    ReloadConfig,
    CommandPalette,
    HintMode,
    NextTheme,
    PrevTheme,
    /// Open the right-click context menu (Copy / Paste / Split Right /
    /// Split Down / Close Pane / New Tab) anchored at the click point.
    /// Bound to bare right-click in cycle 245 — replacing the cycle-49
    /// silent no-op that left first-time users confused. Shift+right-
    /// click still extends the selection (xterm convention preserved).
    OpenContextMenu,
    /// Cycle 247: restore the most-recently-closed tab (WezTerm /
    /// browser convention). Pops the most recent entry from
    /// `Mux::closed_tabs` (bounded ring of 10) and re-spawns the same
    /// argv + OSC-7 cwd at the same tab index. No-op when the ring is
    /// empty. Bound to `Ctrl+Shift+T` by default — same chord
    /// WezTerm / Chrome / Firefox use for "reopen closed tab."
    UndoCloseTab,
    /// Cycle 248: clone the focused pane's argv + OSC-7 cwd into a
    /// new tab (iTerm2's "Duplicate Tab"). An `ssh box` tab clones to
    /// another `ssh box` tab; a `kettle -e vim file` tab clones to a
    /// second vim. Empty argv falls back to the configured shell.
    DuplicateTab,
    /// Cycle 248: clone the focused pane's argv + OSC-7 cwd into a
    /// right-side split of itself. Same logic as `DuplicateTab` but
    /// the new program lives in the same tab.
    DuplicatePane,
    /// Cycle 809 (audit): open the release page for the pending update
    /// banner (cycle 794) and dismiss it — the keyboard equivalent of
    /// left-clicking the banner. No-op (debug-logged) when no banner is
    /// showing. Unbound by default: the banner is non-modal so grabbing a
    /// bare key (Enter/Esc) would steal it from the terminal, so keyboard
    /// access is opt-in via this bindable action instead.
    OpenUpdate,
    /// Cycle 809 (audit): dismiss the pending update banner without opening
    /// the release page — the keyboard equivalent of right-clicking the
    /// banner. No-op (debug-logged) when no banner is showing. Unbound by
    /// default (see `OpenUpdate`).
    DismissUpdate,
    GotoTab(u8),
}

/// Every accepted action token, in the canonical form the user types in
/// `keybind = …`. One name per row; alias rows are present too (so users
/// who learned Terminator's `go_next` see it here alongside Ghostty's
/// `focus_next`). Sorted for stable output. Followed by a one-line
/// `goto_tab:N` blurb — the parametric form can't be enumerated.
///
/// Powers `kettle --list-actions`, the inverse of `Action::from_name`.
/// A `--check-config` cycle (cycle 85) catches typos at validation time;
/// `--list-actions` is the *forward* discovery for users writing a new
/// keybind from scratch.
///
/// Kept in sync with `Action::from_name` via the test
/// `action_names_round_trip_through_from_name`.
pub fn action_names() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vec![
        "copy",
        "copy_to_clipboard",
        "paste",
        "paste_from_clipboard",
        "new_tab",
        "close_tab",
        "next_tab",
        "previous_tab",
        "prev_tab",
        // Cycle 614: Terminator names (config.py:133-134).
        "cycle_next",
        "cycle_prev",
        "move_tab_left",
        "move_tab_right",
        "new_split:right",
        "split_right",
        "split_vert",
        "new_split:down",
        "split_down",
        "split_horiz",
        "split_auto",
        "close_surface",
        "close_pane",
        "close_term",
        "close_window",
        "new_window",
        // Cycle 614: Terminator name (config.py:195).
        "new_terminator",
        "focus_next",
        "go_next",
        "focus_prev",
        "go_prev",
        "goto_split:up",
        "go_up",
        "goto_split:down",
        "go_down",
        "goto_split:left",
        "go_left",
        "goto_split:right",
        "go_right",
        "resize_up",
        "resize_down",
        "resize_left",
        "resize_right",
        "toggle_split_zoom",
        "toggle_zoom",
        "scaled_zoom",
        "help",
        "show_help",
        "send_newline",
        "layout_launcher",
        "open_layout_picker",
        "preferences",
        "edit_config",
        "settings",
        "open_settings",
        "set_scrollbar_always",
        "set_scrollbar_auto",
        "set_scrollbar_never",
        "toggle_cursor_blink",
        "toggle_copy_on_select",
        "set_bell_off",
        "set_bell_visual",
        "set_bell_attention",
        "set_bell_both",
        "toggle_mouse_hide",
        "increase_font_size",
        "zoom_in",
        "decrease_font_size",
        "zoom_out",
        "reset_font_size",
        "zoom_normal",
        "start_search",
        "search",
        "broadcast_all",
        "group_all",
        "group_all_toggle",
        "group_tab_toggle",
        "group_win_toggle",
        "broadcast_off",
        "ungroup_all",
        // Cycle 681 — named-groups runtime broadcast scope.
        "broadcast_group",
        "broadcast-group",
        "toggle_broadcast_group",
        "toggle-broadcast-group",
        "broadcast_window",
        "broadcast-window",
        "toggle_broadcast_window",
        "toggle-broadcast-window",
        "toggle_fullscreen",
        "full_screen",
        "reset",
        "clear_history",
        "clear_scrollback",
        "clear_buffer",
        "scroll_page_up",
        "scroll_page_down",
        "scroll_line_up",
        "scroll_line_down",
        "scroll_to_top",
        "scroll_to_bottom",
        "jump_to_prompt_prev",
        "prev_prompt",
        "jump_to_prompt_next",
        "next_prompt",
        "new_ssh",
        "ssh",
        "toggle_vi_mode",
        "vi_mode",
        "vi",
        "scrollback_vi",
        // Cycle 342 Terminator-parity action names.
        "rotate_cw",
        "rotate_ccw",
        "toggle_scrollbar",
        "edit_window_title",
        "edit_tab_title",
        "edit_terminal_title",
        "edit_pane_title",
        "insert_number",
        "insert_padded",
        "insert_pane_number",
        "insert_pane_padded",
        "next_profile",
        "previous_profile",
        "prev_profile",
        "zoom_in_all",
        "zoom_out_all",
        "zoom_normal_all",
        "reset_zoom_all",
        "reset_clear",
        "reset_and_clear",
        // Cycle 616 — auto_theme.py runtime toggle.
        "toggle_light_dark",
        "toggle-light-dark",
        "toggle_theme_variant",
        "toggle-theme-variant",
        // Cycle 621 — logger.py runtime tap.
        "toggle_session_log",
        "toggle-session-log",
        "start_logger",
        "start-logger",
        "stop_logger",
        "stop-logger",
        // Cycle 640 — terminalshot.py runtime trigger.
        "take_screenshot",
        "take-screenshot",
        "terminalshot",
        "screenshot",
        // Cycle 642 — named broadcast groups (action surface).
        "create_group",
        "create-group",
        "group_tab",
        "group-tab",
        "group_win",
        "group-win",
        "group_window",
        "group-window",
        "ungroup_tab",
        "ungroup-tab",
        "ungroup_win",
        "ungroup-win",
        "ungroup_window",
        "ungroup-window",
        "page_up_half",
        "page_down_half",
        "scroll_page_up_half",
        "scroll_page_down_half",
        "paste_selection",
        "paste_primary",
        "hide_window",
        "toggle_window",
        "toggle_window_visibility",
        // Cycle 384.
        "move_tab_to_new_window",
        "detach_tab",
        "edit_pane_group",
        "edit_group",
        "command_palette",
        "palette",
        "hint_mode",
        "hints",
        "quick_select",
        "context_menu",
        "open_context_menu",
        "undo_close_tab",
        "reopen_tab",
        "restore_tab",
        "duplicate_tab",
        "duplicate_pane",
        "next_theme",
        "prev_theme",
        "previous_theme",
        "reload_config",
        // Cycle 809 (audit) — keyboard access to the update banner.
        "open_update",
        "open-update",
        "dismiss_update",
        "dismiss-update",
        // Cycle 918: these two bindable actions had `from_name` aliases + tests
        // but were omitted from the discovery list, so `kettle --list-actions`
        // silently hid them. The reverse-coverage drift guard below now catches
        // any future omission.
        "insert_name",
        "insert_pane_name",
        "insert_term_name",
        "open_cwd",
        "open_cwd_in_file_manager",
    ];
    v.sort_unstable();
    v
}

impl Action {
    /// Cycle 326: opened to `pub` so kettle-ui's Lua engine can
    /// translate `kettle.exec_action(name)` strings into Action
    /// variants at drain time. The set of accepted names + their
    /// aliases is the same as the keybind grammar.
    pub fn from_name(s: &str) -> Option<Action> {
        use Action::*;
        // Cycle 147: lowercase before matching so `keybind =
        // ctrl+shift+c = Copy` resolves the same as `... = copy`.
        // Pre-fix the capitalized spelling silently dropped (cycle
        // 88's malformed-value check flagged it, but the runtime
        // still didn't bind anything). Same shape as cycle 146's
        // enum-key case-insensitivity.
        let lowered = s.trim().to_ascii_lowercase();
        Some(match lowered.as_str() {
            "copy_to_clipboard" | "copy" => Copy,
            "paste_from_clipboard" | "paste" => Paste,
            "new_tab" => NewTab,
            "close_tab" => CloseTab,
            // Cycle 614: `cycle_next` / `cycle_prev` are
            // Terminator's names for "cycle to the next / previous
            // tab" (config.py:133-134, bound to Ctrl+Tab /
            // Ctrl+Shift+Tab). Equivalent semantics to kettle's
            // NextTab / PrevTab. Accept both spellings.
            "next_tab" | "cycle_next" | "cycle-next" => NextTab,
            "previous_tab" | "prev_tab" | "cycle_prev" | "cycle-prev" => PrevTab,
            "move_tab_left" => MoveTabLeft,
            "move_tab_right" => MoveTabRight,
            // Terminator semantics: split_horiz = horizontal divider
            // (panes top/bottom) = our SplitDown; split_vert = vertical
            // divider (panes left/right) = our SplitRight.
            "new_split:right" | "split_right" | "split_vert" => SplitRight,
            "new_split:down" | "split_down" | "split_horiz" => SplitDown,
            "split_auto" => SplitAuto,
            "close_surface" | "close_pane" | "close_term" => ClosePane,
            "close_window" => CloseWindow,
            // Cycle 614 Terminator parity: `new_terminator` is
            // Terminator's name for "spawn a new top-level
            // window/instance" (config.py line 195, bound to
            // <Super>i by default). Kettle's `NewWindow` action
            // does the same thing — accept the Terminator spelling
            // so a `keybind = super+i = new_terminator` copied from
            // a Terminator config Just Works.
            "new_window" | "new_terminator" | "new-terminator" => NewWindow,
            "focus_next" | "go_next" => FocusNext,
            "focus_prev" | "go_prev" => FocusPrev,
            "goto_split:up" | "go_up" => FocusUp,
            "goto_split:down" | "go_down" => FocusDown,
            "goto_split:left" | "go_left" => FocusLeft,
            "goto_split:right" | "go_right" => FocusRight,
            "resize_up" => ResizeUp,
            "resize_down" => ResizeDown,
            "resize_left" => ResizeLeft,
            "resize_right" => ResizeRight,
            "toggle_split_zoom" | "toggle_zoom" => ToggleZoom,
            "scaled_zoom" | "scaled-zoom" | "toggle_scaled_zoom" => ScaledZoom,
            "help" | "show_help" | "show-help" | "open_help" | "open-help" => ShowHelp,
            "send_newline" | "send-newline" => SendNewline,
            "layout_launcher" | "layout-launcher" | "open_layout_picker" | "open-layout-picker"
            | "layout_picker" | "layout-picker" => OpenLayoutPicker,
            "preferences"
            | "preferences_keybindings"
            | "preferences-keybindings"
            | "edit_config"
            | "edit-config"
            | "open_config"
            | "open-config" => EditConfig,
            "settings" | "open_settings" | "open-settings" => OpenSettings,
            "set_scrollbar_always" | "set-scrollbar-always" | "scrollbar_always" => {
                SetScrollbarAlways
            }
            "set_scrollbar_auto" | "set-scrollbar-auto" | "scrollbar_auto" => SetScrollbarAuto,
            "set_scrollbar_never" | "set-scrollbar-never" | "scrollbar_never" => SetScrollbarNever,
            "toggle_cursor_blink" | "toggle-cursor-blink" | "cursor_blink_toggle" => {
                ToggleCursorBlink
            }
            "toggle_copy_on_select" | "toggle-copy-on-select" | "copy_on_select_toggle" => {
                ToggleCopyOnSelect
            }
            "set_bell_off" | "set-bell-off" | "bell_off" => SetBellOff,
            "set_bell_visual" | "set-bell-visual" | "bell_visual" => SetBellVisual,
            "set_bell_attention" | "set-bell-attention" | "bell_attention" => SetBellAttention,
            "set_bell_both" | "set-bell-both" | "bell_both" => SetBellBoth,
            "toggle_mouse_hide" | "toggle-mouse-hide" | "mouse_hide_toggle" => ToggleMouseHide,
            "increase_font_size" | "zoom_in" => IncreaseFontSize,
            "decrease_font_size" | "zoom_out" => DecreaseFontSize,
            "reset_font_size" | "zoom_normal" => ResetFontSize,
            "start_search" | "search" => StartSearch,
            "broadcast_all" | "group_all" => ToggleBroadcastAll,
            "broadcast_off" | "ungroup_all" => ToggleBroadcastOff,
            "broadcast_group"
            | "broadcast-group"
            | "toggle_broadcast_group"
            | "toggle-broadcast-group"
            | "group_tab_toggle"
            | "group-tab-toggle" => ToggleBroadcastGroup,
            "broadcast_window"
            | "broadcast-window"
            | "toggle_broadcast_window"
            | "toggle-broadcast-window"
            | "group_win_toggle"
            | "group-win-toggle" => ToggleBroadcastWindow,
            // Cycle 700 Terminator parity
            // (terminatorlib/keybindings DEFAULTS):
            // `group_all_toggle` is Terminator's spelling for
            // "toggle group-all". Reuses the existing
            // ToggleBroadcastAll dispatch.
            "group_all_toggle" | "group-all-toggle" => ToggleBroadcastAll,
            "toggle_fullscreen" | "full_screen" => ToggleFullscreen,
            "reset" => Reset,
            "clear_history" | "clear_scrollback" | "clear_buffer" => ClearHistory,
            // Terminator spells these `page_up`/`page_down`/`line_up`/
            // `line_down` (terminatorlib keybindings); accept both so a verbatim
            // Terminator keybinding config imports cleanly.
            "scroll_page_up" | "page_up" | "page-up" => ScrollPageUp,
            "scroll_page_down" | "page_down" | "page-down" => ScrollPageDown,
            "scroll_line_up" | "line_up" | "line-up" => ScrollLineUp,
            "scroll_line_down" | "line_down" | "line-down" => ScrollLineDown,
            "scroll_to_top" => ScrollToTop,
            "scroll_to_bottom" => ScrollToBottom,
            "jump_to_prompt_prev" | "prev_prompt" => JumpPrevPrompt,
            "jump_to_prompt_next" | "next_prompt" => JumpNextPrompt,
            "toggle_vi_mode" | "vi_mode" | "vi" | "scrollback_vi" => ToggleViMode,
            // Cycle 342 Terminator-parity actions. Names match
            // terminatorlib/terminal.py:key_<name> + the kebab-case
            // alias.
            "rotate_cw" | "rotate-cw" => RotateCw,
            "rotate_ccw" | "rotate-ccw" => RotateCcw,
            "toggle_scrollbar" | "toggle-scrollbar" => ToggleScrollbar,
            "edit_window_title" | "edit-window-title" => EditWindowTitle,
            "edit_tab_title" | "edit-tab-title" => EditTabTitle,
            "edit_terminal_title"
            | "edit-terminal-title"
            | "edit_pane_title"
            | "edit-pane-title" => EditPaneTitle,
            "insert_number" | "insert-number" | "insert_pane_number" | "insert-pane-number" => {
                InsertPaneNumber
            }
            "insert_padded" | "insert-padded" | "insert_pane_padded" | "insert-pane-padded" => {
                InsertPanePadded
            }
            "insert_name" | "insert-name" | "insert_pane_name" | "insert-pane-name"
            | "insert_term_name" | "insert-term-name" => InsertPaneName,
            "open_cwd" | "open-cwd" | "open_cwd_in_file_manager" | "open-cwd-in-file-manager" => {
                OpenCwdInFileManager
            }
            "next_profile" | "next-profile" => NextProfile,
            "previous_profile" | "previous-profile" | "prev_profile" | "prev-profile" => {
                PrevProfile
            }
            "zoom_in_all" | "zoom-in-all" => ZoomInAll,
            "zoom_out_all" | "zoom-out-all" => ZoomOutAll,
            "zoom_normal_all" | "zoom-normal-all" | "reset_zoom_all" => ZoomNormalAll,
            "reset_clear" | "reset-clear" | "reset_and_clear" | "reset-and-clear" => ResetAndClear,
            "toggle_light_dark"
            | "toggle-light-dark"
            | "toggle_theme_variant"
            | "toggle-theme-variant" => ToggleLightDark,
            "toggle_session_log" | "toggle-session-log" | "start_logger" | "start-logger"
            | "stop_logger" | "stop-logger" => ToggleSessionLog,
            "take_screenshot" | "take-screenshot" | "terminalshot" | "screenshot" => TakeScreenshot,
            "create_group" | "create-group" => CreateGroup,
            "group_tab" | "group-tab" => GroupTab,
            "group_win" | "group-win" | "group_window" | "group-window" => GroupWindow,
            "ungroup_tab" | "ungroup-tab" => UngroupTab,
            "ungroup_win" | "ungroup-win" | "ungroup_window" | "ungroup-window" => UngroupWindow,
            "page_up_half" | "page-up-half" | "scroll_page_up_half" => ScrollPageUpHalf,
            "page_down_half" | "page-down-half" | "scroll_page_down_half" => ScrollPageDownHalf,
            "paste_selection" | "paste-selection" | "paste_primary" => PastePrimary,
            "hide_window"
            | "hide-window"
            | "toggle_window"
            | "toggle-window"
            | "toggle_window_visibility" => ToggleWindowVisibility,
            "move_tab_to_new_window" | "move-tab-to-new-window" | "detach_tab" | "detach-tab" => {
                MoveTabToNewWindow
            }
            "edit_pane_group" | "edit-pane-group" | "edit_group" | "edit-group" => EditPaneGroup,
            "new_ssh" | "ssh" => OpenSsh,
            "command_palette" | "palette" => CommandPalette,
            "hint_mode" | "hints" | "quick_select" => HintMode,
            "context_menu" | "open_context_menu" => OpenContextMenu,
            "undo_close_tab" | "reopen_tab" | "restore_tab" => UndoCloseTab,
            "duplicate_tab" => DuplicateTab,
            "duplicate_pane" => DuplicatePane,
            "next_theme" => NextTheme,
            "prev_theme" | "previous_theme" => PrevTheme,
            "reload_config" => ReloadConfig,
            // Cycle 809 (audit): keyboard access to the cycle-794 update banner.
            "open_update" | "open-update" => OpenUpdate,
            "dismiss_update" | "dismiss-update" => DismissUpdate,
            // `goto_tab:N` where N is 1-based (Terminator / kitty syntax —
            // "Alt+1 = first tab" is the user mental model). Internally we
            // store the zero-based index so the handler can clamp against
            // `tabs.len()` without an off-by-one dance.
            other => {
                if let Some(rest) = other.strip_prefix("goto_tab:")
                    && let Ok(n) = rest.parse::<u8>()
                    && n >= 1
                {
                    return Some(GotoTab(n - 1));
                }
                // Terminator's `switch_to_tab_N` (1-based, N = 1..=10) — accept
                // it as an alias of `goto_tab:N` so a verbatim Terminator config
                // imports. `switch-to-tab-N` (kebab) is accepted too.
                if let Some(rest) = other
                    .strip_prefix("switch_to_tab_")
                    .or_else(|| other.strip_prefix("switch-to-tab-"))
                    && let Ok(n) = rest.parse::<u8>()
                    && n >= 1
                {
                    return Some(GotoTab(n - 1));
                }
                return None;
            }
        })
    }
}

fn parse_key(s: &str) -> Option<Key> {
    let l = s.to_ascii_lowercase();
    Some(match l.as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "page_up" | "pageup" | "prior" => Key::PageUp,
        "page_down" | "pagedown" | "next" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "enter" | "return" => Key::Enter,
        "tab" => Key::Tab,
        "plus" => Key::Char('+'),
        "minus" => Key::Char('-'),
        "equal" => Key::Char('='),
        _ => {
            // Cycle 857 (audit): only F1..=F12 are real. The winit→Key bridge
            // (app.rs) maps F1..F12 only; F0 and F13+ can never arrive, so a
            // binding to them was silently dead. Reject out-of-range so the
            // user's typo surfaces instead of binding to nothing. (`f13` then
            // falls through and fails the single-char arm → None.)
            if let Some(n) = l.strip_prefix('f')
                && let Ok(num) = n.parse::<u8>()
                && (1..=12).contains(&num)
            {
                return Some(Key::F(num));
            }
            let mut ch = l.chars();
            let c = ch.next()?;
            if ch.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    })
}

/// Parse a Ghostty trigger such as `ctrl+shift+o`.
pub fn parse_trigger(s: &str) -> Option<Trigger> {
    let mut mods = Mods::empty();
    let mut key: Option<Key> = None;
    let parts: Vec<&str> = s.split('+').collect();
    let last_idx = parts.len().checked_sub(1)?;
    for (i, part) in parts.iter().enumerate() {
        let lower = part.trim().to_ascii_lowercase();
        // Modifier aliases. The Super-key family is over-covered on
        // purpose: it has different names on every OS / WM / config
        // ecosystem (`super` X11, `cmd`/`command` macOS, `win`/
        // `windows` PC keyboards, `meta` historical X11 / Emacs,
        // `logo` Qt's spelling), and a user copying a chord from
        // their old config shouldn't have to relearn the name kettle
        // happens to call it.
        let added_mod = match lower.as_str() {
            "shift" => {
                mods |= Mods::SHIFT;
                true
            }
            "ctrl" | "control" => {
                mods |= Mods::CTRL;
                true
            }
            "alt" | "opt" | "option" => {
                mods |= Mods::ALT;
                true
            }
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => {
                mods |= Mods::SUPER;
                true
            }
            _ => false,
        };
        if !added_mod {
            // Strict-mode rejection: a non-modifier in any but the
            // last `+`-separated slot is a typo. The pre-cycle
            // implementation `parse_key(other)`'d every non-modifier
            // and overwrote `key` each loop iteration, so a typo'd
            // modifier (`cttrl+t`, or `win+t` before cycle 163 added
            // the alias) silently degraded to "plain key with no
            // modifiers" — `keybind = win+t = new_tab` rebound plain
            // `t` to new_tab and the user got new tabs while typing
            // normally. Now the typo returns None and `--check-config`
            // (which already gates triggers via `parse_trigger.is_some()`
            // in `detect_malformed_values`) surfaces the bad line.
            if i != last_idx {
                return None;
            }
            key = parse_key(&lower);
        }
    }
    Some(Trigger::new(mods, key?))
}

pub type Bindings = HashMap<Trigger, Action>;

/// Terminator-compatible defaults (see plan / terminatorlib/config.py).
pub fn defaults() -> Bindings {
    defaults_audit().0
}

/// `defaults()` plus the *ordered* list of every trigger the builder
/// called `bind()` on (including duplicates). The bindings map already
/// has cardinality `<= bind_calls.len()` by HashMap semantics; the
/// test `defaults_has_no_shadow_collisions` asserts equality so a
/// future binding that silently shadows an earlier one (the cycle-110
/// bug — Ctrl+Shift+Up/Down landed on top of the Resize quartet) fails
/// CI instead of going unnoticed. Pure, allocates one extra Vec; not
/// on the hot path.
pub fn defaults_audit() -> (Bindings, Vec<Trigger>) {
    use Action::*;
    use Key::*;
    let c = Mods::CTRL;
    let cs = Mods::CTRL | Mods::SHIFT;
    let a = Mods::ALT;
    let su = Mods::SUPER;
    let sus = Mods::SUPER | Mods::SHIFT;
    let mut m = Bindings::new();
    let mut triggers: Vec<Trigger> = Vec::new();
    let mut bind = |mods: Mods, k: Key, act: Action| {
        let t = Trigger::new(mods, k);
        triggers.push(t);
        m.insert(t, act);
    };
    // Terminator parity: Ctrl+Shift+O = split horizontally (top/bottom),
    // Ctrl+Shift+E = split vertically (left/right).
    bind(cs, Char('o'), SplitDown);
    bind(cs, Char('e'), SplitRight);
    bind(cs, Char('a'), SplitAuto);
    bind(cs, Char('t'), NewTab);
    bind(cs, Char('w'), ClosePane);
    bind(cs, Char('q'), CloseWindow);
    bind(cs, Char('i'), NewWindow);
    bind(cs, Char('n'), FocusNext);
    bind(cs, Char('p'), FocusPrev);
    bind(a, Up, FocusUp);
    bind(a, Down, FocusDown);
    bind(a, Left, FocusLeft);
    bind(a, Right, FocusRight);
    // Resize splits with Shift+Arrows only — cycle 110 took
    // `Ctrl+Shift+Up/Down` for `ScrollLineUp/Down`, so binding
    // `Ctrl+Shift+Left/Right` to Resize alone would have given an
    // inconsistent four-direction map (Up/Down scroll, Left/Right
    // resize). Drop the Ctrl+Shift+Arrows resize quartet entirely;
    // Shift+Arrows is the canonical Terminator-default chord. The
    // README and keybind table reflect this from cycle 115 onward.
    // Terminator-style Shift+Arrow split resize.
    let sh = Mods::SHIFT;
    bind(sh, Up, ResizeUp);
    bind(sh, Down, ResizeDown);
    bind(sh, Left, ResizeLeft);
    bind(sh, Right, ResizeRight);
    bind(c, PageDown, NextTab);
    bind(c, PageUp, PrevTab);
    bind(cs, PageDown, MoveTabRight);
    bind(cs, PageUp, MoveTabLeft);
    bind(cs, Char('c'), Copy);
    bind(cs, Char('v'), Paste);
    bind(cs, Char('f'), StartSearch);
    // Font-size zoom: every variant of "Ctrl + the plus/minus area" maps
    // to the same action. The `+` glyph on a US layout *is* `Shift+=` —
    // winit reports the chord as `mods = Ctrl+Shift, key = '+'`, which
    // wouldn't match a bare `Ctrl+Plus` binding. Cover all four sensible
    // combinations so muscle memory works regardless of layout / whether
    // the user thinks of it as `Ctrl++`, `Ctrl+=`, or `Ctrl+Shift+=`.
    bind(c, Char('+'), IncreaseFontSize);
    bind(c, Char('='), IncreaseFontSize);
    bind(cs, Char('+'), IncreaseFontSize);
    bind(cs, Char('='), IncreaseFontSize);
    bind(c, Char('-'), DecreaseFontSize);
    // `Ctrl+_` (== `Ctrl+Shift+-` on US) — same logic as Ctrl+Plus above.
    bind(cs, Char('-'), DecreaseFontSize);
    bind(cs, Char('_'), DecreaseFontSize);
    bind(c, Char('0'), ResetFontSize);
    bind(cs, Char('x'), ToggleZoom);
    bind(cs, Char('r'), Reset);
    bind(su, Char('g'), ToggleBroadcastAll);
    bind(sus, Char('g'), ToggleBroadcastOff);
    bind(Mods::empty(), F(11), ToggleFullscreen);
    bind(cs, Char('m'), ReloadConfig);
    // Cycle 756: Ctrl+, opens the Settings overlay (VS Code / common
    // convention). Ctrl+, is otherwise unused by shells, and the overlay is
    // also reachable from the right-click menu.
    bind(c, Char(','), OpenSettings);
    bind(c, Up, JumpPrevPrompt);
    bind(c, Down, JumpNextPrompt);
    bind(cs, Char('s'), OpenSsh);
    bind(cs, Char('k'), CommandPalette);
    bind(cs, Char('h'), HintMode);
    // Ctrl+Shift+Space toggles vi-mode (Alacritty default). Foundation
    // sub-cycle ships the entry + visible block cursor + Esc exit;
    // h/j/k/l movement + visual selection + yank come in follow-up
    // sub-cycles.
    bind(cs, Char(' '), ToggleViMode);
    bind(Mods::SHIFT, PageUp, ScrollPageUp);
    bind(Mods::SHIFT, PageDown, ScrollPageDown);
    // Ctrl+Shift+Up/Down for line-by-line scrollback. Matches Alacritty's
    // `Ctrl+Shift+Up/Down → ScrollLineUp/Down` and the same chord on kitty
    // (`shift+up/down`, but ctrl-shift conflicts less with shell history
    // navigation than plain shift) and WezTerm (`Ctrl+Shift+UpArrow →
    // ScrollByLine(-1)`).
    bind(cs, Up, ScrollLineUp);
    bind(cs, Down, ScrollLineDown);
    bind(Mods::SHIFT, Home, ScrollToTop);
    bind(Mods::SHIFT, End, ScrollToBottom);
    // Alt+1..9 jumps to tab 1..9 (kitty / Terminator / Ghostty parity).
    // No-op when the requested tab doesn't exist — the app-side handler
    // already clamps against `tabs.len()`.
    for n in 1u8..=9 {
        bind(a, Char((b'0' + n) as char), GotoTab(n - 1));
    }
    (m, triggers)
}

/// Apply a `keybind = trigger=action` line on top of an existing map.
///
/// The action text `unbind` (also `none`, or empty after the `=`) **removes**
/// the trigger from the map rather than inserting — that's the only way for
/// a user to remove a default like `Ctrl+Shift+C` they want their shell or
/// another tool to receive instead. Matches Ghostty's `unbind` and WezTerm's
/// `DisableDefaultAssignment` / Alacritty's empty-action behavior.
pub fn apply_keybind(map: &mut Bindings, value: &str) {
    if value.is_empty() {
        return;
    }
    // Cycle 832 (audit): split on the LAST `=`, not the first. The trigger can
    // BE the `=` key (a shipped default binding), so `ctrl+==increase_font_size`
    // must parse as trigger `ctrl+=` / action `increase_font_size`. Action names
    // are `[a-z0-9_:-]` and never contain `=`, so the final `=` is unambiguously
    // the separator. `split_once` cut at the first `=` → trigger `ctrl+` (a
    // trailing-empty chord parse_trigger rejects), silently dropping the rebind.
    let Some((trig, act)) = value.rsplit_once('=') else {
        return;
    };
    let Some(t) = parse_trigger(trig) else {
        return;
    };
    let act_trim = act.trim();
    if is_unbind_token(act_trim) {
        map.remove(&t);
        return;
    }
    if let Some(a) = Action::from_name(act_trim) {
        map.insert(t, a);
    }
}

/// Recognize the unbind sentinels: empty, `unbind`, `none`, `null`, `false`.
/// Pure so `detect_malformed_values` and `apply_keybind` agree on what's
/// valid action text (a sentinel here means "remove", not "malformed").
pub(crate) fn is_unbind_token(s: &str) -> bool {
    matches!(
        s.to_ascii_lowercase().as_str(),
        "" | "unbind" | "none" | "null" | "false"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycle 857: only F1..=F12 are real keys (the winit→Key bridge maps no
    /// others), so `parse_key` must reject F0 and F13+ rather than accept a
    /// binding that can never fire.
    #[test]
    fn parse_key_rejects_out_of_range_fkeys() {
        assert_eq!(parse_key("f1"), Some(Key::F(1)));
        assert_eq!(parse_key("f12"), Some(Key::F(12)));
        assert_eq!(parse_key("f0"), None, "F0 is not a real key");
        assert_eq!(parse_key("f13"), None, "F13+ never arrives from winit");
        assert_eq!(parse_key("f255"), None);
    }

    /// Cycle 766: `Trigger::label()` must round-trip through `parse_trigger`
    /// for every `Key` variant — the interactive keybind editor uses `label()`
    /// as the config-file serializer (`keybind = <label>=<action>`), so a
    /// non-round-tripping label would persist a binding the parser can't read
    /// back. Covers modifiers, letters, punctuation aliases, named keys, F-keys.
    #[test]
    fn trigger_label_round_trips_through_parse() {
        let cs = Mods::CTRL | Mods::SHIFT;
        let cases = [
            Trigger::new(cs, Key::Char('e')),
            Trigger::new(Mods::CTRL, Key::Char(',')),
            Trigger::new(Mods::ALT, Key::Left),
            Trigger::new(Mods::SUPER | Mods::SHIFT, Key::Char('g')),
            Trigger::new(Mods::CTRL, Key::Char('+')),
            Trigger::new(Mods::CTRL, Key::Char('-')),
            Trigger::new(Mods::CTRL, Key::Char('=')),
            Trigger::new(Mods::empty(), Key::F(5)),
            Trigger::new(cs, Key::PageUp),
            Trigger::new(Mods::ALT, Key::Char('1')),
            Trigger::new(cs, Key::Enter),
            Trigger::new(Mods::CTRL, Key::Up),
        ];
        for t in cases {
            let label = t.label();
            assert_eq!(
                parse_trigger(&label),
                Some(t),
                "label {label:?} did not round-trip back to {t:?}"
            );
        }
    }

    #[test]
    fn trigger_label_formats() {
        let t = Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('e'));
        assert_eq!(t.label(), "Ctrl+Shift+E");
        assert_eq!(Trigger::new(Mods::ALT, Key::Left).label(), "Alt+Left");
        assert_eq!(Trigger::new(Mods::empty(), Key::F(5)).label(), "F5");
    }

    #[test]
    fn trigger_label_uses_named_tokens_for_plus_minus_equal() {
        // Cycle 170: the parser accepts `ctrl+plus` / `ctrl+minus` /
        // `ctrl+equal` as named tokens for the punctuation keys
        // (line 354-356). `label()` should mirror that, otherwise
        // `kettle --list-keybinds` shows the default `Ctrl++`
        // binding (font zoom-in) as the literal string `Ctrl++`
        // — two adjacent `+` make it ambiguous whether the second
        // one is the separator's repetition or the key. Same for
        // `Ctrl+-` (zoom out, looks like a trailing dash) and
        // `Ctrl+=` (also zoom in, looks like an assignment).
        // Both kitty and Ghostty render these as `Plus`/`Minus`/
        // `Equal` in their printed keymaps for the same reason.
        let c = Mods::CTRL;
        assert_eq!(Trigger::new(c, Key::Char('+')).label(), "Ctrl+Plus");
        assert_eq!(Trigger::new(c, Key::Char('-')).label(), "Ctrl+Minus");
        assert_eq!(Trigger::new(c, Key::Char('=')).label(), "Ctrl+Equal");
        // Other punctuation that isn't a named-parse token still uses
        // the raw char (uppercased where applicable).
        assert_eq!(Trigger::new(c, Key::Char(',')).label(), "Ctrl+,");
        assert_eq!(Trigger::new(c, Key::Char('/')).label(), "Ctrl+/");
        // Plain letters unchanged — regression for the existing test.
        assert_eq!(Trigger::new(c, Key::Char('a')).label(), "Ctrl+A");
    }

    #[test]
    fn parse_trigger_accepts_super_aliases_and_rejects_typos() {
        // The Super key has different names in different worlds —
        // `super` (X11), `cmd`/`command` (macOS), `win`/`windows`
        // (Windows), `meta` (historical X11 / Emacs), `logo` (Qt).
        // All map to the same Mods::SUPER bit. Before cycle 163,
        // only `super`/`cmd`/`command` were recognized; anything
        // else fell to `parse_key(other)` and silently degraded
        // the chord to a plain-key binding.
        let s_t = Trigger::new(Mods::SUPER, Key::Char('t'));
        assert_eq!(parse_trigger("super+t"), Some(s_t));
        assert_eq!(parse_trigger("cmd+t"), Some(s_t));
        assert_eq!(parse_trigger("command+t"), Some(s_t));
        assert_eq!(parse_trigger("win+t"), Some(s_t));
        assert_eq!(parse_trigger("windows+t"), Some(s_t));
        assert_eq!(parse_trigger("meta+t"), Some(s_t));
        assert_eq!(parse_trigger("logo+t"), Some(s_t));
        // Case-insensitive (`Ctrl` / `WIN` / `Cmd`).
        assert_eq!(parse_trigger("WIN+T"), Some(s_t));
        // Multi-modifier still works (regression).
        assert_eq!(
            parse_trigger("ctrl+shift+c"),
            Some(Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('c'))),
        );
        // Modifiers can appear in any order before the key.
        assert_eq!(
            parse_trigger("win+ctrl+t"),
            Some(Trigger::new(Mods::SUPER | Mods::CTRL, Key::Char('t'))),
        );
        // Strict rejection: a typo'd modifier name in a non-final
        // position used to silently degrade to plain `t` (the key
        // slot got overwritten by the typo's `parse_key` attempt,
        // then by the real key). Now the parse fails outright so
        // `--check-config` flags the line as malformed instead of
        // letting a "secret" plain-key binding stomp on normal
        // typing.
        assert_eq!(parse_trigger("cttrl+t"), None);
        assert_eq!(parse_trigger("contorl+t"), None);
        assert_eq!(parse_trigger("supre+t"), None);
        // Bare key (no modifiers) still parses — keybinds like `f5`
        // are legitimate.
        assert_eq!(
            parse_trigger("f5"),
            Some(Trigger::new(Mods::empty(), Key::F(5))),
        );
    }

    #[test]
    fn action_from_name_is_case_insensitive() {
        // Cycle 147: same pattern as cycle 146's enum-key case-
        // insensitivity. A user writing `keybind = ctrl+shift+c =
        // Copy` (capitalized) used to silently drop the binding —
        // `from_name` returned None on the unrecognized case
        // variant, and apply_keybind's silent-skip path swallowed
        // it. Now lowercased before matching.
        use Action::*;
        assert_eq!(Action::from_name("Copy"), Some(Copy));
        assert_eq!(Action::from_name("COPY"), Some(Copy));
        assert_eq!(Action::from_name("copy"), Some(Copy));
        // Multi-word names with underscores too.
        assert_eq!(
            Action::from_name("INCREASE_FONT_SIZE"),
            Some(IncreaseFontSize)
        );
        assert_eq!(Action::from_name("Goto_Split:Up"), Some(FocusUp));
        // Parametric form still works with case variants.
        assert!(matches!(Action::from_name("GOTO_TAB:1"), Some(GotoTab(0))));
        // Surrounding whitespace also trimmed.
        assert_eq!(Action::from_name("  paste  "), Some(Paste));
        // Real typos still return None.
        assert!(Action::from_name("Cpy").is_none());
        // Cycle 809 (audit): the update-banner actions resolve from both the
        // underscore and hyphen spellings, case-insensitively.
        assert_eq!(Action::from_name("open_update"), Some(OpenUpdate));
        assert_eq!(Action::from_name("OPEN-UPDATE"), Some(OpenUpdate));
        assert_eq!(Action::from_name("dismiss_update"), Some(DismissUpdate));
        assert_eq!(Action::from_name("Dismiss-Update"), Some(DismissUpdate));
    }

    #[test]
    fn readme_documented_chords_are_actually_bound() {
        // Cycle 125 promoted 9 default bindings into the README
        // keybind table (SSH launcher, command palette, hint mode,
        // jump-prompt, move-tab, zoom-pane, new-window, split-auto,
        // goto-tab). The README is documentation, not source-of-
        // truth, but a user reading it deserves to find that chord
        // doing what the row claims. Pin each one so a future
        // unbind / rebind catches the docs-drift here.
        use Action::*;
        let d = defaults();
        let c = Mods::CTRL;
        let cs = Mods::CTRL | Mods::SHIFT;
        let pairs: &[(Mods, Key, Action)] = &[
            (cs, Key::Char('s'), OpenSsh),
            (cs, Key::Char('k'), CommandPalette),
            (cs, Key::Char('h'), HintMode),
            (cs, Key::Char('a'), SplitAuto),
            (cs, Key::Char('i'), NewWindow),
            (cs, Key::Char('x'), ToggleZoom),
            (c, Key::Up, JumpPrevPrompt),
            (c, Key::Down, JumpNextPrompt),
            (cs, Key::PageUp, MoveTabLeft),
            (cs, Key::PageDown, MoveTabRight),
        ];
        for (mods, k, want) in pairs {
            let trig = Trigger::new(*mods, *k);
            assert_eq!(
                d.get(&trig),
                Some(want),
                "README claims {} → {want:?}; bound to {:?} instead",
                trig.label(),
                d.get(&trig)
            );
        }
    }

    #[test]
    fn defaults_has_no_shadow_collisions() {
        // Cycle-116 systemic guard. Cycle 115 caught a single shadow
        // collision (Ctrl+Shift+Up/Down both bound to Resize *and*
        // ScrollLine — second one silently wins). The class of bug is
        // easy to reintroduce because `bind()` is `HashMap::insert()`
        // which doesn't warn on duplicates. `defaults_audit()` returns
        // both the final map AND the ordered list of every trigger
        // the builder bound; map.len() < triggers.len() iff some
        // trigger appears twice. Pin equality so any future
        // duplicate-bind shows up here, with a useful error naming
        // the offender(s).
        let (m, triggers) = defaults_audit();
        if m.len() != triggers.len() {
            // Build the duplicate set so the failure message tells
            // the next developer exactly which trigger shadowed.
            use std::collections::HashMap;
            let mut seen: HashMap<Trigger, usize> = HashMap::new();
            for t in &triggers {
                *seen.entry(*t).or_insert(0) += 1;
            }
            let dups: Vec<String> = seen
                .into_iter()
                .filter(|(_, n)| *n > 1)
                .map(|(t, n)| format!("{} (×{n})", t.label()))
                .collect();
            panic!(
                "shadow collision in defaults(): {} bind() calls but \
                 only {} unique triggers — duplicates: [{}]",
                triggers.len(),
                m.len(),
                dups.join(", ")
            );
        }
    }

    #[test]
    fn scroll_line_up_down_bound_to_ctrl_shift_arrows() {
        // Cycle-110 bindings: Alacritty / kitty / WezTerm all bind a
        // chord for line-by-line scrollback navigation, but kettle
        // shipped only PageUp/PageDown (Shift) and Top/Bottom (Shift
        // Home/End). Ctrl+Shift+Up/Down fills the gap with the most
        // commonly-used chord across modern terminals.
        //
        // Cycle 115 added a regression guard: this binding collided
        // with the previous Ctrl+Shift+Arrows → Resize quartet, so
        // the Resize-via-Ctrl+Shift+Arrows defaults were dropped
        // entirely. Shift+Arrows is now the canonical resize chord
        // (the README and example config match).
        let d = defaults();
        let cs = Mods::CTRL | Mods::SHIFT;
        assert_eq!(
            d.get(&Trigger::new(cs, Key::Up)),
            Some(&Action::ScrollLineUp)
        );
        assert_eq!(
            d.get(&Trigger::new(cs, Key::Down)),
            Some(&Action::ScrollLineDown)
        );
        // Page-scroll and jump-to-prompt still bound (regression guard;
        // earlier cycles relied on these existing).
        assert!(d.contains_key(&Trigger::new(Mods::SHIFT, Key::PageUp)));
        assert_eq!(
            d.get(&Trigger::new(Mods::CTRL, Key::Up)),
            Some(&Action::JumpPrevPrompt),
            "JumpPrev/Next (Ctrl+Up/Down) must coexist with Ctrl+Shift+Up/Down"
        );
        // Cycle-115 guards: the Ctrl+Shift+Arrows → Resize quartet is
        // GONE (would silently shadow ScrollLineUp/Down). Shift+Arrows
        // is the only resize chord now.
        assert!(
            !d.contains_key(&Trigger::new(cs, Key::Left)),
            "Ctrl+Shift+Left must NOT be bound (avoid Resize/Scroll inconsistency)"
        );
        assert!(
            !d.contains_key(&Trigger::new(cs, Key::Right)),
            "Ctrl+Shift+Right must NOT be bound (avoid Resize/Scroll inconsistency)"
        );
        for k in [Key::Up, Key::Down, Key::Left, Key::Right] {
            let trig = Trigger::new(Mods::SHIFT, k);
            assert!(
                matches!(
                    d.get(&trig),
                    Some(Action::ResizeUp)
                        | Some(Action::ResizeDown)
                        | Some(Action::ResizeLeft)
                        | Some(Action::ResizeRight)
                ),
                "Shift+{k:?} should map to a Resize action: {:?}",
                d.get(&trig)
            );
        }
    }

    #[test]
    fn font_size_binds_cover_us_layout_shift_variants() {
        // On a US keyboard the "Ctrl+Plus" chord is actually
        // `Ctrl+Shift+=` (Shift held because `+` lives on `=`). winit
        // reports it as `mods = Ctrl+Shift, key = '+'` — without a
        // Ctrl+Shift+Plus binding the chord did nothing. Same family
        // for Ctrl+Shift+= and Ctrl+Shift+- (== Ctrl+_).
        let d = defaults();
        let c = Mods::CTRL;
        let cs = Mods::CTRL | Mods::SHIFT;
        for (mods, k, expected) in [
            (c, '+', Action::IncreaseFontSize),
            (c, '=', Action::IncreaseFontSize),
            (cs, '+', Action::IncreaseFontSize),
            (cs, '=', Action::IncreaseFontSize),
            (c, '-', Action::DecreaseFontSize),
            (cs, '-', Action::DecreaseFontSize),
            (cs, '_', Action::DecreaseFontSize),
        ] {
            let t = Trigger::new(mods, Key::Char(k));
            assert_eq!(
                d.get(&t),
                Some(&expected),
                "{t:?} should map to {expected:?}"
            );
        }
    }

    #[test]
    fn action_names_round_trip_through_from_name() {
        // Drift guard: every name `action_names()` lists must parse
        // back to Some(Action) via `from_name`. If a new action lands
        // in `from_name` but no row is added to `action_names`,
        // `kettle --list-actions` will silently omit it; if a row in
        // `action_names` typos a name, `from_name` will return None
        // and the published list ships a useless token. Either way
        // this test fails.
        let names = action_names();
        // Sanity: enumeration is non-trivial. If this drops to 0, the
        // bug is in `action_names`, not in `from_name`.
        assert!(
            names.len() >= 40,
            "expected at least 40 documented actions; got {}",
            names.len()
        );
        for n in &names {
            assert!(
                Action::from_name(n).is_some(),
                "action_names returned {n:?} but from_name rejects it"
            );
        }
        // Cycle 918 reverse guard: these two bindable actions were accepted by
        // `from_name` (each with a round-trip test) yet missing from
        // `action_names()`, so `kettle --list-actions` silently hid them. Pin
        // them so the omission can't recur. (The general reverse-coverage check —
        // every Action variant has a listed spelling — would need an Action
        // iterator/strum; this targeted guard covers the gap that actually bit.)
        for must in ["insert_pane_name", "open_cwd_in_file_manager"] {
            assert!(
                names.contains(&must),
                "{must:?} is bindable (from_name accepts it) but absent from \
                 action_names() — `--list-actions` would hide it"
            );
            assert!(Action::from_name(must).is_some());
        }
        // Also pin `goto_tab:N` (the only parametric form). It isn't
        // in `action_names` because N is unbounded, but it must parse.
        assert!(Action::from_name("goto_tab:1").is_some());
        // And `unbind` is intentionally NOT a listed action — it's a
        // sentinel for `apply_keybind`, not a real Action variant.
        assert!(!names.contains(&"unbind"));
        assert!(Action::from_name("unbind").is_none());
    }

    #[test]
    fn describe_reflects_user_overrides_and_unbinds() {
        // Cycle-103 contract: `--list-keybinds --config FILE` should
        // show the *effective* keymap, not the built-in defaults. The
        // pure `describe(&Bindings)` is what powers it; here we drive
        // it directly with a hand-built map to confirm: overrides
        // appear with the override action, unbound triggers don't
        // appear at all, and the output is sorted.
        let mut m = defaults();
        // Override a default to a different action.
        apply_keybind(&mut m, "ctrl+shift+t=close_tab");
        // Unbind another default.
        apply_keybind(&mut m, "ctrl+shift+c=unbind");

        let lines = describe(&m);
        // Override surfaces with the new action label.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Ctrl+Shift+T") && l.contains("CloseTab")),
            "override should appear with the new action: {lines:?}"
        );
        // No line should still show Ctrl+Shift+T → NewTab.
        assert!(
            !lines
                .iter()
                .any(|l| l.starts_with("Ctrl+Shift+T") && l.contains("NewTab")),
            "stale default should not coexist with override: {lines:?}"
        );
        // Unbound trigger is gone entirely (no Ctrl+Shift+C line).
        assert!(
            !lines.iter().any(|l| l.starts_with("Ctrl+Shift+C")),
            "unbound trigger should not appear: {lines:?}"
        );
        // Output remains sorted by trigger label.
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted, "describe must return sorted lines");
        // And `describe_defaults` is still just `describe(&defaults())`.
        assert_eq!(describe_defaults(), describe(&defaults()));
    }

    #[test]
    fn describe_column_width_grows_to_fit_longest_trigger() {
        // Cycle 165: `Ctrl+Shift+PageDown` (19 chars; move-tab-right) and
        // `Ctrl+Shift+PageUp` (17 chars; move-tab-left) used to overflow
        // the hard-coded 16-char padding, so their action column landed
        // one or three columns to the right of every other row — the
        // alignment that's supposed to make `--list-keybinds` scannable
        // was the one thing visibly wrong.
        //
        // Locating the column from output bytes is tricky because a
        // short trigger like `Ctrl+C` (6 chars) gets padded with the
        // remainder as spaces, so the first `  ` (two consecutive
        // spaces) in that line lands *inside* the padding, not at the
        // separator. So check the format contract directly: padded
        // trigger of `width` chars + 2-char separator + action label.
        // Then for every row, byte `longest` is either inside-padding
        // (a space) or the separator's first space, byte `longest+1`
        // is the separator's second space, and byte `longest+2` is
        // the first action char (never a space).
        let lines = describe(&defaults());
        // Default keymap really has `Ctrl+Shift+PageDown` (19 chars).
        let long_row = lines
            .iter()
            .find(|l| l.starts_with("Ctrl+Shift+PageDown "))
            .expect("default has Ctrl+Shift+PageDown");
        let longest = 19usize;
        assert_eq!(
            &long_row[longest..longest + 2],
            "  ",
            "two-space separator should follow the unpadded longest trigger: {long_row:?}"
        );
        for l in &lines {
            // Every row's separator's second space sits at the same byte.
            assert_eq!(
                &l[longest + 1..longest + 2],
                " ",
                "row's column {} should be the separator's second space: {l:?}",
                longest + 1,
            );
            // And every row's action label starts at byte longest+2
            // with no leading space — that's the alignment contract.
            assert_ne!(
                &l[longest + 2..longest + 3],
                " ",
                "row's action column should start at byte {} (no leading space): {l:?}",
                longest + 2,
            );
        }
        // Floor of 16 keeps the short-default case readable. A map with
        // only `Ctrl+C` (6 chars) should still pad to 16, so the action
        // lands at byte 18.
        let short = describe(
            &[(Trigger::new(Mods::CTRL, Key::Char('c')), Action::Copy)]
                .into_iter()
                .collect(),
        );
        assert_eq!(&short[0][16..18], "  ", "short trigger padded to 16");
        assert_eq!(&short[0][18..19], "C", "action 'Copy' at byte 18");
    }

    #[test]
    fn alt_digit_keys_go_to_tab() {
        let d = defaults();
        // Alt+1 → GotoTab(0), …, Alt+9 → GotoTab(8). Every modern terminal
        // binds these (kitty / Terminator / iTerm2 / Ghostty). User mental
        // model is 1-based ("Alt+1 = first tab"); internally we store the
        // zero-based index the handler indexes into `tabs`.
        for n in 1u8..=9 {
            let t = Trigger::new(Mods::ALT, Key::Char((b'0' + n) as char));
            match d.get(&t) {
                Some(Action::GotoTab(i)) => assert_eq!(*i, n - 1, "Alt+{n} → tab {}", n - 1),
                other => panic!("Alt+{n} not bound to GotoTab: {other:?}"),
            }
        }
        // Alt+0 is *not* bound (browsers use Cmd+9 for "last tab" but kitty
        // / Terminator don't bind 0, so neither do we — kept free for the
        // user to bind manually if they want "last tab" semantics).
        let t0 = Trigger::new(Mods::ALT, Key::Char('0'));
        assert!(!d.contains_key(&t0), "Alt+0 should be free");
    }

    #[test]
    fn apply_keybind_unbind_removes_default() {
        // Default map ships with Ctrl+Shift+C → Copy. A user whose shell
        // wants Ctrl+Shift+C for itself (e.g. some readline kits) had no
        // way to remove it — `apply_keybind` only ever *inserted*. Now
        // `keybind = ctrl+shift+c = unbind` removes it; aliases are
        // `none` / `null` / `false` / empty (all map to "no action").
        let mut m = defaults();
        let trig = Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('c'));
        assert_eq!(m.get(&trig), Some(&Action::Copy), "ships bound by default");

        apply_keybind(&mut m, "ctrl+shift+c=unbind");
        assert!(!m.contains_key(&trig), "unbind removes the entry");

        // Re-bind to confirm map state is still healthy after a removal.
        apply_keybind(&mut m, "ctrl+shift+c=copy");
        assert_eq!(m.get(&trig), Some(&Action::Copy));

        // Every documented alias.
        for tok in ["unbind", "none", "null", "false", ""] {
            let mut mm = defaults();
            apply_keybind(&mut mm, &format!("ctrl+shift+c={tok}"));
            assert!(!mm.contains_key(&trig), "alias {tok:?} should also unbind");
        }

        // Unbind on an *unbound* trigger is a no-op (not an error).
        let mut mm = defaults();
        let unused = Trigger::new(Mods::CTRL | Mods::ALT, Key::Char('§'));
        let before = mm.len();
        apply_keybind(&mut mm, "ctrl+alt+§=unbind");
        assert_eq!(mm.len(), before, "unbinding a free trigger is a no-op");
        let _ = unused;
    }

    /// Cycle 832 (audit): the `=` key can itself be a trigger (a shipped
    /// default), so a rebind line splits on the LAST `=`, not the first.
    #[test]
    fn apply_keybind_rebinds_the_equals_key() {
        let mut m = defaults();
        let eq = Trigger::new(Mods::CTRL, Key::Char('='));
        // ctrl+= ships as IncreaseFontSize; rebind it to a different action.
        apply_keybind(&mut m, "ctrl+==reset_font_size");
        assert_eq!(
            m.get(&eq),
            Some(&Action::ResetFontSize),
            "ctrl+==reset_font_size must split on the LAST = (trigger ctrl+=)"
        );
        // ctrl+shift+= too (multi-modifier chord ending in the = key).
        apply_keybind(&mut m, "ctrl+shift+==decrease_font_size");
        let eqs = Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('='));
        assert_eq!(m.get(&eqs), Some(&Action::DecreaseFontSize));
        // And unbinding it works through the same split.
        apply_keybind(&mut m, "ctrl+==unbind");
        assert!(!m.contains_key(&eq));
    }

    #[test]
    fn is_unbind_token_recognizes_aliases() {
        for tok in ["unbind", "Unbind", "UNBIND", "none", "null", "false", ""] {
            assert!(is_unbind_token(tok), "{tok:?} should be unbind");
        }
        // Anything else is a real action name (or a typo `from_name`
        // will reject — not unbind).
        for tok in ["copy", "no_action", "disabled", "off", "x"] {
            assert!(!is_unbind_token(tok), "{tok:?} should NOT be unbind");
        }
    }

    /// Cycle 614 drift guard. Terminator-spelling aliases for
    /// kettle actions: a config copied verbatim from Terminator
    /// should bind without unknown-key warnings.
    ///   - `new_terminator` → `NewWindow`   (config.py:195)
    ///   - `cycle_next`     → `NextTab`     (config.py:133)
    ///   - `cycle_prev`     → `PrevTab`     (config.py:134)
    #[test]
    fn from_name_accepts_terminator_spelling_aliases() {
        for s in ["new_window", "new_terminator", "new-terminator"] {
            assert!(
                matches!(Action::from_name(s), Some(Action::NewWindow)),
                "alias {s:?} should parse to NewWindow"
            );
        }
        for s in ["next_tab", "cycle_next", "cycle-next"] {
            assert!(
                matches!(Action::from_name(s), Some(Action::NextTab)),
                "alias {s:?} should parse to NextTab"
            );
        }
        for s in ["previous_tab", "prev_tab", "cycle_prev", "cycle-prev"] {
            assert!(
                matches!(Action::from_name(s), Some(Action::PrevTab)),
                "alias {s:?} should parse to PrevTab"
            );
        }
    }

    /// Cycle 940 drift guard: Terminator's scroll/tab action spellings parse so
    /// a verbatim Terminator keybinding config imports cleanly.
    #[test]
    fn from_name_accepts_terminator_scroll_and_tab_aliases() {
        assert!(matches!(
            Action::from_name("page_up"),
            Some(Action::ScrollPageUp)
        ));
        assert!(matches!(
            Action::from_name("page_down"),
            Some(Action::ScrollPageDown)
        ));
        assert!(matches!(
            Action::from_name("line_up"),
            Some(Action::ScrollLineUp)
        ));
        assert!(matches!(
            Action::from_name("line_down"),
            Some(Action::ScrollLineDown)
        ));
        // switch_to_tab_N (1-based) == goto_tab:N == GotoTab(N-1).
        assert!(matches!(
            Action::from_name("switch_to_tab_1"),
            Some(Action::GotoTab(0))
        ));
        assert!(matches!(
            Action::from_name("switch_to_tab_9"),
            Some(Action::GotoTab(8))
        ));
        assert!(matches!(
            Action::from_name("switch-to-tab-3"),
            Some(Action::GotoTab(2))
        ));
        // switch_to_tab_0 is invalid (tabs are 1-based, like goto_tab:N).
        assert!(Action::from_name("switch_to_tab_0").is_none());
    }

    /// Cycle 640 drift guard: every alias for `take_screenshot`
    /// parses to `Action::TakeScreenshot`. Terminator's
    /// `terminalshot.py` is the source of the `terminalshot` spelling;
    /// `screenshot` and `take-screenshot` are kettle-style short
    /// forms.
    #[test]
    fn from_name_accepts_take_screenshot_aliases() {
        for s in [
            "take_screenshot",
            "take-screenshot",
            "terminalshot",
            "screenshot",
        ] {
            assert!(
                matches!(Action::from_name(s), Some(Action::TakeScreenshot)),
                "alias {s:?} should parse to TakeScreenshot"
            );
        }
    }

    /// Cycle 607 drift guard: each alias for the open-cwd-in-file-
    /// manager action round-trips. Terminator's `dir_open.py`
    /// plugin only exposes a menu item (no keybind name), so the
    /// kettle-native `open_cwd` short form is the canonical spelling
    /// — the longer forms exist for explicitness in config files
    /// and a `--list-keybinds`-style display.
    #[test]
    fn from_name_accepts_open_cwd_in_file_manager_aliases() {
        for s in [
            "open_cwd",
            "open-cwd",
            "open_cwd_in_file_manager",
            "open-cwd-in-file-manager",
        ] {
            assert!(
                matches!(Action::from_name(s), Some(Action::OpenCwdInFileManager)),
                "alias {s:?} should parse to OpenCwdInFileManager"
            );
        }
    }

    /// Cycle 606 drift guard: every alias the parser accepts for
    /// `insert_pane_name` round-trips to the same Action variant.
    /// Terminator emits the `insert-term-name` signal (with hyphens
    /// AND `term`); kettle's preferred spelling is `insert_pane_name`.
    /// The aliases here let a Terminator-style config keybind work
    /// without renaming.
    #[test]
    fn from_name_accepts_insert_pane_name_aliases() {
        for s in [
            "insert_pane_name",
            "insert-pane-name",
            "insert_name",
            "insert-name",
            "insert_term_name",
            "insert-term-name",
        ] {
            assert!(
                matches!(Action::from_name(s), Some(Action::InsertPaneName)),
                "alias {s:?} should parse to InsertPaneName"
            );
        }
    }

    /// Cycle 700 drift guard. Terminator's `*_toggle` broadcast
    /// keybind names map onto kettle's existing broadcast-scope
    /// actions.
    #[test]
    fn from_name_accepts_terminator_group_toggle_aliases() {
        for (s, want) in [
            ("group_all_toggle", Action::ToggleBroadcastAll),
            ("group-all-toggle", Action::ToggleBroadcastAll),
            ("group_tab_toggle", Action::ToggleBroadcastGroup),
            ("group-tab-toggle", Action::ToggleBroadcastGroup),
            ("group_win_toggle", Action::ToggleBroadcastWindow),
            ("group-win-toggle", Action::ToggleBroadcastWindow),
        ] {
            assert_eq!(
                Action::from_name(s).as_ref(),
                Some(&want),
                "alias {s:?} should parse to {want:?}"
            );
        }
    }

    /// Cycle 696 drift guard. Terminator's `key_preferences`
    /// and `key_preferences_keybindings` both resolve to
    /// `Action::EditConfig` via the documented aliases.
    #[test]
    fn from_name_accepts_edit_config_aliases() {
        for s in [
            "preferences",
            "preferences_keybindings",
            "preferences-keybindings",
            "edit_config",
            "edit-config",
            "open_config",
            "open-config",
        ] {
            assert!(
                matches!(Action::from_name(s), Some(Action::EditConfig)),
                "alias {s:?} should parse to EditConfig"
            );
        }
    }

    /// Cycle 702 drift guard. Terminator's `send_newline`
    /// keybind name resolves to `Action::SendNewline`.
    #[test]
    fn from_name_accepts_send_newline_aliases() {
        for s in ["send_newline", "send-newline"] {
            assert!(
                matches!(Action::from_name(s), Some(Action::SendNewline)),
                "alias {s:?} should parse to SendNewline"
            );
        }
    }

    /// Cycle 695 drift guard. Terminator's F1 `key_help`
    /// resolves to `Action::ShowHelp` via the documented aliases.
    #[test]
    fn from_name_accepts_show_help_aliases() {
        for s in ["help", "show_help", "show-help", "open_help", "open-help"] {
            assert!(
                matches!(Action::from_name(s), Some(Action::ShowHelp)),
                "alias {s:?} should parse to ShowHelp"
            );
        }
    }

    /// Cycle 693 drift guard. Terminator's `scaled_zoom` keybind
    /// name (and the spelled aliases) parse to the new
    /// `Action::ScaledZoom` variant — zoom + 1.5× font scale.
    #[test]
    fn from_name_accepts_scaled_zoom_aliases() {
        for s in ["scaled_zoom", "scaled-zoom", "toggle_scaled_zoom"] {
            assert!(
                matches!(Action::from_name(s), Some(Action::ScaledZoom)),
                "alias {s:?} should parse to ScaledZoom"
            );
        }
        // Make sure the new variant didn't accidentally swallow
        // the bare `toggle_zoom` (which must still resolve to the
        // non-font-scaling variant).
        assert!(matches!(
            Action::from_name("toggle_zoom"),
            Some(Action::ToggleZoom)
        ));
    }

    #[test]
    fn from_name_parses_goto_tab_one_based() {
        // `goto_tab:1` is the first tab — 1-based to match the keybind
        // intuition. Internally → GotoTab(0).
        assert!(matches!(
            Action::from_name("goto_tab:1"),
            Some(Action::GotoTab(0))
        ));
        assert!(matches!(
            Action::from_name("goto_tab:9"),
            Some(Action::GotoTab(8))
        ));
        // Zero is rejected (user mental model is 1-based; refuse the
        // ambiguity rather than silently mapping it to GotoTab(0)).
        assert!(Action::from_name("goto_tab:0").is_none());
        // Garbage values → None so unknown-key reporting still kicks in.
        assert!(Action::from_name("goto_tab:abc").is_none());
        assert!(Action::from_name("goto_tab:").is_none());
    }

    #[test]
    fn action_label_renders_goto_tab_with_one_based_index() {
        // `--list-keybinds` reads these labels; Debug-derived
        // `GotoTab(0)` leaks the 0-based internal index — a user looking
        // at the listing would think Alt+1 → tab 0. Use the 1-based
        // human form. Other variants use the Debug derive verbatim so
        // the existing labels (Copy, NewTab, SplitRight, …) don't
        // change.
        assert_eq!(action_label(&Action::GotoTab(0)), "Goto tab 1");
        assert_eq!(action_label(&Action::GotoTab(8)), "Goto tab 9");
        assert_eq!(action_label(&Action::Copy), "Copy");
        assert_eq!(action_label(&Action::SplitRight), "SplitRight");
    }

    #[test]
    fn describe_defaults_is_sorted_and_complete() {
        let lines = describe_defaults();
        assert_eq!(lines.len(), defaults().len(), "one line per binding");
        // Sorted by the (label, action) key.
        let mut sorted = lines.clone();
        sorted.sort();
        assert_eq!(lines, sorted);
        // A known Terminator binding is present and labelled.
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Ctrl+Shift+E") && l.contains("SplitRight")),
            "expected the Ctrl+Shift+E split binding, got {lines:?}"
        );
    }
}

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
    /// Edit keys. macOS binds `Cmd+Backspace` to delete-to-line-start by
    /// default; before these existed the chord could not be written in a
    /// config file at all, which is why `⌘⌫` did nothing.
    Backspace,
    Delete,
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
            // separator's repetition or the key itself, so we
            // emit `Plus`/`Minus`/`Equal` so the row reads
            // `Ctrl+Plus  IncreaseFontSize`, matching how the user
            // would type the chord in their config file.
            Key::Char('+') => "Plus".into(),
            Key::Char('-') => "Minus".into(),
            Key::Char('=') => "Equal".into(),
            Key::Char(' ') => "Space".into(),
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
            Key::Backspace => "Backspace".into(),
            Key::Delete => "Delete".into(),
            Key::F(n) => format!("F{n}"),
        });
        parts.join("+")
    }
}

/// Maximum decoded payload for a `text:` binding. Keystroke-sized on purpose:
/// this stands in for a chord, not a paste, and the bounded-input rule applies
/// to anything on its way to a PTY.
pub(crate) const MAX_SEND_TEXT_BYTES: usize = 256;

/// Decode the payload of a `text:` action. Ghostty's spelling and its escapes.
///
/// Four rules are load-bearing rather than cosmetic:
///
/// * A raw `=` is rejected in favour of `\x3d`. `apply_keybind` splits a
///   `keybind =` line on its LAST `=` and relies on action names never
///   containing one; `apply_exclusive_keybind` and `detect_malformed_values`
///   share that assumption. Requiring the escape keeps all three correct.
/// * `\xHH` stops at `7f`. The payload is a `String`, so admitting a lone
///   `\x80` would put invalid UTF-8 on the way to the PTY. Non-ASCII is
///   written literally instead and encoded as UTF-8.
/// * An unknown escape is an error rather than a literal backslash, so
///   `--check-config` names the bad line instead of quietly sending something
///   the user did not write. Raw control bytes are rejected for the same
///   reason: the config tokenizer trims the value, so they cannot round-trip.
/// * An empty payload returns `None`, leaving `is_unbind_token` to own the
///   empty case and keeping the bare `text:` prefix out of the `from_name`
///   literal census that backs `--list-actions`.
fn parse_send_text_payload(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        let decoded = match c {
            '=' => return None,
            c if (c as u32) < 0x20 || c == '\x7f' => return None,
            '\\' => match chars.next()? {
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                'e' => '\x1b',
                'a' => '\x07',
                'b' => '\x08',
                'f' => '\x0c',
                'v' => '\x0b',
                '0' => '\0',
                'x' => {
                    let hi = chars.next()?.to_digit(16)?;
                    let lo = chars.next()?.to_digit(16)?;
                    let value = hi * 16 + lo;
                    if value > 0x7f {
                        return None;
                    }
                    char::from_u32(value)?
                }
                _ => return None,
            },
            c => c,
        };
        out.push(decoded);
        if out.len() > MAX_SEND_TEXT_BYTES {
            return None;
        }
    }
    (!out.is_empty()).then_some(out)
}

/// Render a `text:` payload back in the spelling the user would type it, so
/// that no label, `--list-keybinds` row or conflict dialog ever puts a raw
/// control byte on the user's own terminal.
fn escape_send_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x1b' => out.push_str("\\e"),
            '=' => out.push_str("\\x3d"),
            c if (c as u32) < 0x20 || c == '\x7f' => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
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
        Action::NewTabShell(i) => format!("New tab: dropdown shell {}", i + 1),
        // Never `{:?}` this one: the label reaches `--list-keybinds`, `describe`
        // and the Settings conflict dialog, and a payload like `\x15` must not
        // arrive there as a raw control byte.
        Action::SendText(text) => format!("Send text \"{}\"", escape_send_text(text)),
        other => format!("{other:?}"),
    }
}

/// Dropdown parity: the display label of a trigger bound to `action`
/// in the LIVE map (defaults + user overrides), for right-aligned shortcut
/// hints in menus — a rebind shows the user's actual chord, never a
/// hardcoded string. Deterministic despite the map's iteration order:
/// prefer a trigger whose key is a bare alphanumeric (`Ctrl+Shift+1` over
/// its US-shifted twin `Ctrl+Shift+!`), then the shortest label, then
/// lexicographic. `None` when the action is unbound.
pub fn hint_label(bindings: &Bindings, action: &Action) -> Option<String> {
    bindings
        .iter()
        .filter(|(_, a)| *a == action)
        .map(|(t, _)| t.label())
        .min_by_key(|l| {
            let alnum = l
                .rsplit('+')
                .next()
                .is_some_and(|k| k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()));
            (!alnum, l.len(), l.clone())
        })
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
    // had a trigger+action pair. Same shape as `format_ssh_hosts`.
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
    /// Select the entire scrollback + screen (keyboard / palette select-all).
    SelectAll,
    /// Extend the selection up to the first line / top of the buffer (Shift+Home).
    SelectToTop,
    /// Extend the selection down to the last line / last cell (Shift+End).
    SelectToBottom,
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    MoveTabLeft,
    MoveTabRight,
    SplitRight,
    SplitDown,
    SplitAuto,
    /// Terminator parity: move the focused pane to sit beside its neighbour in
    /// that direction -- the keyboard route to the rearrangement Terminator
    /// offers by dragging a terminal.
    MovePaneLeft,
    MovePaneRight,
    MovePaneUp,
    MovePaneDown,
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
    /// v2.20.0 (Ghostty `equalize_splits` / Terminator parity): rebalance
    /// every split in the focused tab so each leaf pane gets equal area —
    /// each split node's ratio becomes `leaves(a) / (leaves(a)+leaves(b))`.
    EqualizeSplits,
    ToggleZoom,
    /// Terminator parity (`key_help`).
    /// Terminator's F1 opens its HTML manual via `open_url`
    /// (xdg-open). kettle opens its README at the canonical
    /// GitHub URL via the `open` crate — the same
    /// `open::that_detached` dispatch path URL clicks already use,
    /// so it works on Linux/macOS/Windows without spawning a
    /// per-platform helper.
    ShowHelp,
    /// Terminator parity
    /// (`terminatorlib/layoutlauncher.py`). Open the runtime
    /// layout picker — an overlay modal that lists saved
    /// layouts from `<config-dir>/layouts/*.json` (via
    /// `Session::list_layouts`). Type-to-filter; Enter spawns
    /// `kettle --layout NAME` as a new window. Same shape as
    /// the `CommandPalette` overlay; uses
    /// `App::layout_picker_input: Option<(String, usize)>`.
    /// Closes the last Bucket-D plugin gap
    /// (`launcher.py` → layout overlay).
    OpenLayoutPicker,
    /// Terminator parity (`key_send_newline`). Writes a literal `\n` to the focused
    /// pane's PTY. Mostly useful for inserting a newline into a
    /// shell line-editor that's otherwise consuming Enter
    /// (e.g. multi-line readline prompts that submit on Enter
    /// but expect explicit `\n` for line continuation). Bucket E
    /// rationale removed since shipping this 4-line dispatch
    /// arm closes the row outright.
    SendNewline,
    /// Terminator parity (`key_preferences` /
    /// `key_preferences_keybindings`). Terminator's GUI
    /// Preferences dialog is config-file-driven for kettle, so
    /// the preferences keybind opens the user's config file with
    /// the OS-registered application. Closes the "preferences GUI is
    /// a paradigm choice" Bucket E rationale by making the
    /// equivalent UX one keystroke away.
    EditConfig,
    /// Open the in-app **Settings overlay** — a keyboard-navigable
    /// panel of the most-used config keys (font size, theme, scrollbar, bell,
    /// cursor, opacity, …) that persists changes live. Distinct from
    /// `EditConfig` (which opens the raw config file with its default app for the long
    /// tail). This is the "settings menu for non-technical users" surface.
    OpenSettings,
    /// Preferences submenu (C8): runtime-mutable
    /// toggles that the Preferences ▸ right-click submenu wires
    /// through `Config::persist_config_toggle` so a
    /// click both updates `self.cfg` AND writes the change back
    /// to `~/.config/kettle/config` atomically. Each variant
    /// targets one specific value; the submenu builder emits
    /// radio-style rows (one variant per option) for enum
    /// settings and toggle rows (one variant for the boolean)
    /// for bools.
    SetScrollbarAlways,
    SetScrollbarAuto,
    SetScrollbarNever,
    /// Close-confirmation policy radio. The close bar answers the CURRENT
    /// close; standing policy belongs somewhere reversible in the same place
    /// it was set, which is Preferences — not a third button on a destructive
    /// one-line prompt.
    SetAskBeforeClosingAlways,
    SetAskBeforeClosingMultiple,
    SetAskBeforeClosingNever,
    ToggleCursorBlink,
    ToggleCopyOnSelect,
    SetBellOff,
    SetBellVisual,
    SetBellAttention,
    SetBellBoth,
    ToggleMouseHide,
    /// Terminator parity (`key_scaled_zoom`).
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
    /// Terminator parity, phase 5 of
    /// [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
    /// docs/TERMINATOR-NAMED-GROUPS-DESIGN.md): toggle broadcast
    /// scope to `Group(focused_pane.group_name)`. When the focused
    /// pane has no group, log + no-op. Pressing again with the
    /// same group already set switches to Off (toggle semantics).
    /// Distinct from `ToggleBroadcastAll` (which sets Tab scope).
    ToggleBroadcastGroup,
    /// Window-wide broadcast — every pane in every tab
    /// receives input. Terminator's true `broadcast_all`. Distinct
    /// from the misnamed `ToggleBroadcastAll` which is
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
    /// This change ships the entry + visible block cursor +
    /// Esc exit; movement + visual / yank come in a follow-up.
    ToggleViMode,
    /// Terminator parity (terminatorlib/terminal.py:key_rotate_cw):
    /// rotate the split tree clockwise.
    RotateCw,
    /// Terminator parity: rotate the split tree counter-clockwise.
    RotateCcw,
    /// Terminator parity (key_toggle_scrollbar): runtime
    /// show/hide of the scrollbar without editing config.
    ToggleScrollbar,
    /// Terminator parity (terminal_popup_menu.py "Read only"): toggle
    /// the focused pane's read-only state — user input is dropped while on.
    TogglePaneReadOnly,
    /// Terminator parity (key_edit_window_title): open an
    /// inline overlay to edit the window title (OSC 0/2 equivalent).
    EditWindowTitle,
    /// Terminator parity (key_edit_tab_title): edit the
    /// active tab's title.
    EditTabTitle,
    /// Terminator parity (key_edit_terminal_title): edit
    /// the focused pane's title.
    EditPaneTitle,
    /// Terminator parity (key_insert_number): send the
    /// focused pane's index as text input.
    InsertPaneNumber,
    /// Terminator parity (key_insert_padded): send the
    /// focused pane's index zero-padded.
    InsertPanePadded,
    /// Terminator parity (`insert_term_name.py` plugin):
    /// send the focused pane's title (Pane::title — what the chrome
    /// shows in the per-pane titlebar) as text input. Useful for
    /// scripts that want to label their output by which pane it
    /// came from, or for keyboard-driven copy-the-current-title
    /// workflows.
    InsertPaneName,
    /// Terminator parity (`dir_open.py` plugin →
    /// `CurrDirOpen` menu item): open the focused pane's current
    /// working directory in the OS file manager. Builds a
    /// `file://<cwd>` URI and routes through the existing
    /// `Action::OpenUrl` machinery so the
    /// `is_safe_url` allowlist + custom-url-handler + Lua hook
    /// path all apply consistently — exactly like clicking a
    /// `file://...` hyperlink in pane output.
    OpenCwdInFileManager,
    /// Terminator parity (key_next_profile): cycle to the
    /// next named profile at runtime.
    NextProfile,
    /// Terminator parity (key_previous_profile): cycle to
    /// the previous named profile.
    PrevProfile,
    /// Terminator parity (key_zoom_in_all): increase font
    /// size on every pane (broadcast variant of IncreaseFontSize).
    ZoomInAll,
    /// Terminator parity (key_zoom_out_all): decrease font
    /// size on every pane.
    ZoomOutAll,
    /// Terminator parity (key_zoom_normal_all): reset font
    /// size on every pane.
    ZoomNormalAll,
    /// Terminator parity (key_reset_clear): Reset (RIS)
    /// + ClearHistory composed.
    ResetAndClear,
    /// Terminator parity (`plugins/auto_theme.py`):
    /// runtime toggle between the configured `light-theme` and
    /// `dark-theme`. If the current theme matches `dark_theme`,
    /// switches to `light_theme`; otherwise switches to `dark_theme`.
    /// If neither config key is set the action no-ops (logged at
    /// `warn`). Distinct from `NextTheme` / `PrevTheme` which walk
    /// the full bundled list.
    ToggleLightDark,
    /// Terminator parity (`plugins/logger.py`):
    /// toggle the focused pane's per-pane session log. When off,
    /// opens a new file at `<cache>/kettle/logs/kettle-<secs>-<pid>.log`
    /// and starts tee-ing raw PTY bytes to it (no ANSI stripping —
    /// the log preserves exact terminal output for later replay).
    /// When on, closes the file. Per-pane state (per-tab and
    /// per-window). No-op + warn when the cache dir can't be created.
    ToggleSessionLog,
    /// Terminator parity (`plugins/terminalshot.py`,
    /// phase 1 of [`TERMINATOR-TERMINALSHOT-DESIGN.md`](
    /// docs/TERMINATOR-TERMINALSHOT-DESIGN.md)): trigger a live-
    /// window screenshot of the focused pane. Wired end-to-end: wgpu
    /// surface readback + BGRA→RGBA conversion + row-padding strip +
    /// image::ImageBuffer save, plus per-pane crop via focused-pane
    /// rect and a toast notification via `fire_notify`. PNG lands at
    /// `session_screenshot_path` (`<cache_dir>/<unix>-<pid>.png`,
    /// with the angle-bracket placeholders inside a code span so
    /// rustdoc doesn't read them as HTML tags).
    TakeScreenshot,
    /// Terminator parity (phase 1 of
    /// [`TERMINATOR-NAMED-GROUPS-DESIGN.md`](
    /// docs/TERMINATOR-NAMED-GROUPS-DESIGN.md)).
    /// `create_group` is Terminator's name for "prompt for a
    /// group name + assign it to the focused pane." Already wired
    /// as `Action::EditPaneGroup`; `CreateGroup`
    /// is the Terminator-spelled alias.
    CreateGroup,
    /// Terminator parity. Assign every pane in the
    /// focused tab to a named broadcast group. Opens the
    /// title-edit overlay (`EditPaneGroup`) with
    /// `TitleEditScope::Group` + bulk-apply on confirm; also
    /// surfaced via the right-click context menu.
    GroupTab,
    /// Terminator parity. Assign every pane in the
    /// focused window to a named broadcast group. Same wiring as
    /// `GroupTab` but with a window-wide scope.
    GroupWindow,
    /// Terminator parity. Bulk-clear the group on every
    /// pane in the focused tab. Walks
    /// `mux.panes_in_focused_tab()` and clears `pane.group_name`
    /// on each. Same dispatch surface as the right-click
    /// "Ungroup This Tab" entry.
    UngroupTab,
    /// Terminator parity. Bulk-clear the group on every
    /// pane in the focused window.
    UngroupWindow,
    /// Terminator parity (`group_all`, window.py:933): put every pane into the
    /// group named `All`.
    ///
    /// Distinct from [`Action::GroupWindow`], which prompts for a name — this
    /// one is Terminator's fixed-name bulk grouping and needs no input.
    /// Distinct too from [`Action::ToggleBroadcastAll`], which is what the
    /// importer used to map this to: grouping is not broadcasting. In
    /// Terminator you group terminals and then choose to broadcast to the
    /// group, so mapping `group_all` onto a broadcast toggle armed input
    /// duplication the user never asked for.
    GroupAll,
    /// Terminator parity (`ungroup_all`, window.py:947): clear the group on
    /// every pane. The partner to [`Action::GroupAll`].
    UngroupAll,
    /// Terminator parity (`group_all_toggle`, window.py:940): group every pane
    /// as `All`, or ungroup them if they already are.
    ToggleGroupAll,
    /// Terminator parity (`group_tab_toggle`, window.py:987): group every pane
    /// in this tab under a generated `Tab N` name, or ungroup them if they
    /// already carry one.
    ///
    /// Distinct from [`Action::GroupTab`], which prompts for a name — and
    /// distinct in the type system for a second reason: two Terminator names
    /// that both resolved to `GroupTab` made an imported `group_tab` line and
    /// an imported `group_tab_toggle` line fight over the same action, so the
    /// second silently unbound the first.
    ToggleGroupTab,
    /// Terminator parity (`group_win_toggle`, window.py:959): the same for
    /// every pane in the window, under a generated `Window group N`.
    ToggleGroupWindow,
    /// Terminator parity (key_page_up_half): scroll up
    /// half a page.
    ScrollPageUpHalf,
    /// Terminator parity (key_page_down_half): scroll down
    /// half a page.
    ScrollPageDownHalf,
    /// Terminator parity (key_paste_selection): paste the
    /// X11 primary selection (Linux-only; no-op on macOS/Windows).
    PastePrimary,
    /// Terminator parity (key_hide_window): toggle window
    /// visibility in-process. Same effect as `kettle --toggle`
    /// via the remote-control IPC; this is the in-process keybind
    /// equivalent for users who don't want to set up a global hotkey.
    ToggleWindowVisibility,
    /// Terminator parity, detachable-tabs: move the focused tab
    /// to a new kettle window. With multi-window support (C5) this is a
    /// LIVE in-process move — the tab's panes (PTYs, scrollback, running
    /// programs) transfer untouched to the new window; nothing respawns.
    /// Keyboard-driven equivalent of the drag tear-off (C6), and the only
    /// route on Wayland (no global cursor tracking). No-op on a 1-tab
    /// window.
    MoveTabToNewWindow,
    /// Terminator parity, titlebar Bucket-D: open the edit overlay
    /// for the focused pane's broadcast group name. Same shape as
    /// EditPaneTitle but writes to pane.group_name. Enter empty
    /// input → clear the group.
    EditPaneGroup,
    OpenSsh,
    ReloadConfig,
    CommandPalette,
    HintMode,
    NextTheme,
    PrevTheme,
    /// Open the right-click context menu (Copy / Paste / Split Right /
    /// Split Down / Close Pane / New Tab) anchored at the click point.
    /// Bound to bare right-click — replacing the earlier silent no-op
    /// that left first-time users confused. Shift+right-
    /// click still extends the selection (xterm convention preserved).
    OpenContextMenu,
    /// Restore the most-recently-closed tab (WezTerm /
    /// browser convention). Pops the most recent entry from
    /// `Mux::closed_tabs` (bounded ring of 10) and re-spawns the same
    /// argv + OSC-7 cwd at the same tab index. No-op when the ring is
    /// empty. Bound to `Ctrl+Shift+T` by default — same chord
    /// WezTerm / Chrome / Firefox use for "reopen closed tab."
    UndoCloseTab,
    /// Clone the focused pane's argv + OSC-7 cwd into a
    /// new tab (iTerm2's "Duplicate Tab"). An `ssh box` tab clones to
    /// another `ssh box` tab; a `kettle -e vim file` tab clones to a
    /// second vim. Empty argv falls back to the configured shell.
    DuplicateTab,
    /// Clone the focused pane's argv + OSC-7 cwd into a
    /// right-side split of itself. Same logic as `DuplicateTab` but
    /// the new program lives in the same tab.
    DuplicatePane,
    /// Open the release page for the pending update
    /// banner and dismiss it — the keyboard equivalent of
    /// left-clicking the banner. No-op (debug-logged) when no banner is
    /// showing. Unbound by default: the banner is non-modal so grabbing a
    /// bare key (Enter/Esc) would steal it from the terminal, so keyboard
    /// access is opt-in via this bindable action instead.
    OpenUpdate,
    /// Dismiss the pending update banner without opening
    /// the release page — the keyboard equivalent of right-clicking the
    /// banner. No-op (debug-logged) when no banner is showing. Unbound by
    /// default (see `OpenUpdate`).
    DismissUpdate,
    GotoTab(u8),
    /// Dropdown parity: open the Nth entry of the new-tab `▾` dropdown
    /// (0-based internally; the `new_tab_shell_N` config form is 1-based like
    /// `goto_tab:N`). Windows Terminal's Ctrl+Shift+1..9 profile shortcuts.
    NewTabShell(u8),
    /// Dropdown parity: open the About panel (version + git hash,
    /// update status, GitHub link) — the dropdown's bottom row, mirroring
    /// Windows Terminal's; also reachable from the command palette.
    About,
    /// Write a literal byte string to the focused pane, as though the user had
    /// typed it — which means broadcast carries it to every pane in scope, and
    /// a payload holding `\r` submits a line in each of them. Deliberate: this
    /// action stands in for a keystroke, so it delivers where a keystroke
    /// would. Ghostty's `text:` action, and the only way to give a chord the
    /// Super key holds meaning for: Super has no legacy PTY encoding at all
    /// (`docs/TERMINAL-CLIENT-COMPATIBILITY.md`), so `⌘⌫` reached applications
    /// as nothing until something claimed it. Bound to `\x15` on macOS.
    SendText(String),
}

/// Every accepted action token, in the canonical form the user types in
/// `keybind = …`. One name per row; alias rows are present too (so users
/// who learned Terminator's `go_next` see it here alongside Ghostty's
/// `focus_next`). Sorted for stable output. Followed by a one-line
/// `goto_tab:N` blurb — the parametric form can't be enumerated.
///
/// Powers `kettle --list-actions`, the inverse of `Action::from_name`.
/// A `--check-config` pass catches typos at validation time;
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
        "select_all",
        "select_to_top",
        "select_to_bottom",
        "new_tab",
        "close_tab",
        "next_tab",
        "previous_tab",
        "prev_tab",
        // Terminator names (config.py:133-134).
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
        // Terminator name (config.py:195).
        "new_terminator",
        "focus_next",
        "go_next",
        "focus_prev",
        "go_prev",
        "move_split:left",
        "move_split:right",
        "move_split:up",
        "move_split:down",
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
        "equalize_splits",
        "balance_splits",
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
        "set_ask_before_closing_always",
        "set_ask_before_closing_multiple",
        "set_ask_before_closing_never",
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
        "broadcast_tab",
        "broadcast-tab",
        "group_all_toggle",
        "group_tab_toggle",
        "group_win_toggle",
        "broadcast_off",
        "ungroup_all",
        // Named-groups runtime broadcast scope.
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
        // Terminator-parity action names.
        "rotate_cw",
        "rotate_ccw",
        "toggle_scrollbar",
        "toggle_read_only",
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
        // auto_theme.py runtime toggle.
        "toggle_light_dark",
        "toggle-light-dark",
        "toggle_theme_variant",
        "toggle-theme-variant",
        // logger.py runtime tap.
        "toggle_session_log",
        "toggle-session-log",
        "start_logger",
        "start-logger",
        "stop_logger",
        "stop-logger",
        // terminalshot.py runtime trigger.
        "take_screenshot",
        "take-screenshot",
        "terminalshot",
        "screenshot",
        // Named broadcast groups (action surface).
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
        // Keyboard access to the update banner.
        "open_update",
        "open-update",
        "dismiss_update",
        "dismiss-update",
        // Dropdown parity — the About panel.
        "about",
        "show_about",
        "show-about",
        // These two bindable actions had `from_name` aliases + tests
        // but were omitted from the discovery list, so `kettle --list-actions`
        // silently hid them.
        "insert_name",
        "insert_pane_name",
        "insert_term_name",
        "open_cwd",
        "open_cwd_in_file_manager",
        // Twenty-seven more aliases that `from_name` has always accepted while
        // this list hid them, found once the reverse-coverage guard below
        // stopped pinning two hand-written names and started deriving the set
        // from `from_name` itself. Each shares an arm with a canonical name
        // that was already listed, so users who learned the short spelling from
        // Terminator saw `--list-actions` deny an action that works.
        "bell_attention",
        "bell_both",
        "bell_off",
        "bell_visual",
        "copy_on_select_toggle",
        "cursor_blink_toggle",
        "layout_picker",
        "line_down",
        "line_up",
        "mouse_hide_toggle",
        "move_pane_down",
        "move_pane_left",
        "move_pane_right",
        "move_pane_up",
        "open_config",
        "open_help",
        "page_down",
        "page_up",
        "preferences_keybindings",
        "read_only",
        "scrollbar_always",
        "scrollbar_auto",
        "scrollbar_never",
        "select_to_first_line",
        "select_to_last_line",
        "toggle_pane_read_only",
        "toggle_scaled_zoom",
    ];
    v.sort_unstable();
    v
}

impl Action {
    /// Opened to `pub` so kettle-ui's Lua engine can
    /// translate `kettle.exec_action(name)` strings into Action
    /// variants at drain time. The set of accepted names + their
    /// aliases is the same as the keybind grammar.
    pub fn from_name(s: &str) -> Option<Action> {
        use Action::*;
        // Lowercase before matching so `keybind =
        // ctrl+shift+c = Copy` resolves the same as `... = copy`.
        // Before this fix the capitalized spelling was silently
        // dropped — an earlier malformed-value check flagged it, but
        // the runtime still didn't bind anything. Same pattern as
        // `enum_keys_are_case_insensitive`.
        //
        // Hyphens fold to underscores for the same reason the config
        // tokenizer folds them the other way: an action name reaches here in
        // whichever spelling its source uses, and hand-maintaining a dual
        // alias per action does not hold. Several were missing, and the
        // tokenizer's own folding turned Terminator's `new_tab` into
        // `new-tab` — a spelling no arm listed — so every line in a copied
        // `[keybindings]` section resolved to nothing. No action is spelled
        // with a hyphen and no underscore twin (pinned by
        // `every_action_name_resolves_in_both_spellings`), so folding this
        // direction cannot shadow one.
        //
        // `text:` is the exception to all of that. Its payload is data, not a
        // name: the folding below would rewrite `text:Hello-World` into
        // `text:hello_world`, so the prefix has to be claimed first and the
        // payload passed through verbatim.
        let trimmed = s.trim();
        if let Some(head) = trimmed.get(..5)
            && head.eq_ignore_ascii_case("text:")
        {
            return parse_send_text_payload(&trimmed[5..]).map(SendText);
        }
        let lowered = trimmed.to_ascii_lowercase().replace('-', "_");
        Some(match lowered.as_str() {
            "copy_to_clipboard" | "copy" => Copy,
            "paste_from_clipboard" | "paste" => Paste,
            "select_all" | "select-all" => SelectAll,
            "select_to_top" | "select-to-top" | "select_to_first_line" => SelectToTop,
            "select_to_bottom" | "select-to-bottom" | "select_to_last_line" => SelectToBottom,
            "new_tab" => NewTab,
            "close_tab" => CloseTab,
            // `cycle_next` / `cycle_prev` are
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
            // Terminator parity: `new_terminator` is
            // Terminator's name for "spawn a new top-level
            // window/instance" (config.py line 195, bound to
            // <Super>i by default). Kettle's `NewWindow` action
            // does the same thing — accept the Terminator spelling
            // so a `keybind = super+i = new_terminator` copied from
            // a Terminator config Just Works.
            "new_window" | "new_terminator" | "new-terminator" => NewWindow,
            "focus_next" | "go_next" => FocusNext,
            "focus_prev" | "go_prev" => FocusPrev,
            "move_split:left" | "move_pane_left" => MovePaneLeft,
            "move_split:right" | "move_pane_right" => MovePaneRight,
            "move_split:up" | "move_pane_up" => MovePaneUp,
            "move_split:down" | "move_pane_down" => MovePaneDown,
            "goto_split:up" | "go_up" => FocusUp,
            "goto_split:down" | "go_down" => FocusDown,
            "goto_split:left" | "go_left" => FocusLeft,
            "goto_split:right" | "go_right" => FocusRight,
            "resize_up" => ResizeUp,
            "resize_down" => ResizeDown,
            "resize_left" => ResizeLeft,
            "resize_right" => ResizeRight,
            "equalize_splits" | "equalize-splits" | "balance_splits" | "balance-splits" => {
                EqualizeSplits
            }
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
            "set_ask_before_closing_always" | "set-ask-before-closing-always" => {
                SetAskBeforeClosingAlways
            }
            "set_ask_before_closing_multiple" | "set-ask-before-closing-multiple" => {
                SetAskBeforeClosingMultiple
            }
            "set_ask_before_closing_never" | "set-ask-before-closing-never" => {
                SetAskBeforeClosingNever
            }
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
            // Terminator's `broadcast_all` is `set_groupsend('all')`
            // (terminal.py:2193-2195) — EVERY terminal, not just the current
            // tab's. It previously aliased to `ToggleBroadcastAll`, whose
            // dispatch sets `BroadcastScope::Tab`, so a user who bound
            // `broadcast_all` and typed a command believed it reached every
            // pane while it reached only the focused tab's. Narrowing the
            // blast radius silently is the dangerous direction for a broadcast
            // feature. `ToggleBroadcastWindow` is the window-wide scope, which
            // this crate's own docs already called "Terminator's true
            // broadcast_all".
            "broadcast_all" => ToggleBroadcastWindow,
            // `group_all` GROUPS every terminal (window.py:933); it does not
            // arm broadcasting. `group_all_toggle` is Terminator's toggling
            // partner and maps to the same bulk grouping here, since kettle
            // treats re-grouping an already-grouped set as idempotent.
            "group_all" => GroupAll,
            "group_all_toggle" => ToggleGroupAll,
            // Kept reachable under an honest name: this is the per-tab scope.
            // NOT spelled `group_tab` — that is Terminator's "group every
            // terminal in this tab" action and already maps to `GroupTab`.
            "broadcast_tab" | "broadcast-tab" => ToggleBroadcastAll,
            "broadcast_off" => ToggleBroadcastOff,
            "ungroup_all" => UngroupAll,
            "broadcast_group"
            | "broadcast-group"
            | "toggle_broadcast_group"
            | "toggle-broadcast-group" => ToggleBroadcastGroup,
            "broadcast_window"
            | "broadcast-window"
            | "toggle_broadcast_window"
            | "toggle-broadcast-window" => ToggleBroadcastWindow,
            // Terminator's `*_toggle` names toggle GROUPING (window.py:959,
            // :987), not broadcasting. kettle's grouping actions prompt for the
            // group name where Terminator generates one, so these bind the
            // grouping half — a prompt the user did not expect, where the old
            // broadcast mapping was a different feature entirely that silently
            // duplicated every keystroke across every pane.
            "group_tab_toggle" => ToggleGroupTab,
            "group_win_toggle" => ToggleGroupWindow,

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
            // Terminator-parity actions. Names match
            // terminatorlib/terminal.py:key_<name> + the kebab-case
            // alias.
            "rotate_cw" | "rotate-cw" => RotateCw,
            "rotate_ccw" | "rotate-ccw" => RotateCcw,
            "toggle_scrollbar" | "toggle-scrollbar" => ToggleScrollbar,
            "toggle_read_only"
            | "toggle-read-only"
            | "read_only"
            | "read-only"
            | "toggle_pane_read_only" => TogglePaneReadOnly,
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
            // Keyboard access to the update banner.
            "open_update" | "open-update" => OpenUpdate,
            "dismiss_update" | "dismiss-update" => DismissUpdate,
            // Dropdown parity: the About panel.
            "about" | "show_about" | "show-about" => About,
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
                // Dropdown parity: `new_tab_shell_N` (1-based) opens the
                // Nth new-tab `▾` dropdown entry — Windows Terminal's
                // Ctrl+Shift+N profile shortcuts. Kebab accepted too.
                if let Some(rest) = other
                    .strip_prefix("new_tab_shell_")
                    .or_else(|| other.strip_prefix("new-tab-shell-"))
                    && let Ok(n) = rest.parse::<u8>()
                    && n >= 1
                {
                    return Some(NewTabShell(n - 1));
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
        "backspace" | "bs" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "plus" => Key::Char('+'),
        "minus" => Key::Char('-'),
        "equal" => Key::Char('='),
        "space" => Key::Char(' '),
        _ => {
            // Only F1..=F12 are real. The winit→Key bridge
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
/// Rewrite a GTK accelerator into kettle's `+`-separated trigger form.
///
/// Terminator writes every binding as a GTK accelerator —
/// `<Control><Shift>t`, `<Alt>1` — with angle-bracketed modifiers and no
/// separator before the key. kettle splits on `+`, so such a string arrived as
/// one unrecognised token and the binding was dropped. Every keybinding line in
/// a real Terminator config therefore imported as nothing, which also made the
/// ~79 Terminator action-name aliases unreachable from a copied file.
///
/// Borrows the input back untouched when it carries no `<`, so kettle's own
/// `ctrl+shift+t` spelling takes exactly the path it always did and costs
/// nothing extra — this runs for every trigger in every config, and all but
/// the imported ones are already in kettle's spelling.
fn normalize_gtk_accelerator(s: &str) -> Option<std::borrow::Cow<'_, str>> {
    if !s.contains('<') {
        return Some(std::borrow::Cow::Borrowed(s));
    }
    let mut parts: Vec<String> = Vec::new();
    let mut rest = s.trim();
    while let Some(open) = rest.find('<') {
        // Anything before a `<` is stray text; GTK accelerators put modifiers
        // first, so treat it as part of the key tail rather than guessing.
        if open > 0 {
            break;
        }
        let Some(close) = rest.find('>') else {
            // No closing `>`, so this is not a GTK accelerator at all — it is
            // kettle's own grammar, where `<` is a perfectly ordinary key.
            // `keybind = <=copy` binds it, and refusing here broke that.
            // Whatever remains becomes the key; a genuine typo like `<Control`
            // then fails at the key parser, which is where it should fail.
            break;
        };
        let modifier = &rest[open + 1..close];
        if modifier.is_empty() {
            // `<>t` is malformed. Dropping the empty group silently turned it
            // into the bare key `t` — so a typo in a config quietly bound an
            // ordinary letter, and typing that letter fired the action instead
            // of reaching the shell. A malformed accelerator must bind
            // NOTHING; the unknown-value diagnostic then names the line.
            return None;
        }
        parts.push(modifier.to_string());
        rest = &rest[close + 1..];
    }
    let key = rest.trim();
    if key.is_empty() {
        // Modifiers with no key (`<Control><Shift>`) is not a chord.
        return None;
    }
    parts.push(key.to_string());
    Some(std::borrow::Cow::Owned(parts.join("+")))
}

pub fn parse_trigger(s: &str) -> Option<Trigger> {
    let s = normalize_gtk_accelerator(s)?;
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
            // GTK's accelerator parser accepts `Ctl` as well as `Ctrl` and
            // `Control`, and Terminator configs are written by GTK.
            //
            // `Primary` is GTK's portable spelling, and it is what its own
            // documentation tells people to write. On the Linux desktops
            // Terminator runs on it means Control, so that is what a chord
            // copied out of one of those configs meant when the user wrote it.
            // Mapping it to Control everywhere keeps the binding they had,
            // rather than moving it to a different key on one platform.
            "ctrl" | "control" | "ctl" | "primary" => {
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
            // last `+`-separated slot is a typo. The earlier
            // implementation `parse_key(other)`'d every non-modifier
            // and overwrote `key` each loop iteration, so a typo'd
            // modifier (`cttrl+t`, or `win+t` before the `win` alias
            // was added) silently degraded to "plain key with no
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
/// future binding that silently shadows an earlier one (as once
/// happened when Ctrl+Shift+Up/Down landed on top of the Resize
/// quartet) fails CI instead of going unnoticed. Pure, allocates one extra Vec; not
/// on the hot path.
pub fn defaults_audit() -> (Bindings, Vec<Trigger>) {
    use Action::*;
    use Key::*;
    let c = Mods::CTRL;
    let cs = Mods::CTRL | Mods::SHIFT;
    let a = Mods::ALT;
    // `su` (Super alone) is used by the non-Windows defaults below. On
    // Windows the broadcast chord is Ctrl+Shift+G (Game Bar owns Win+G), so
    // gate the binding to avoid an unused-variable warning.
    #[cfg(not(windows))]
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
    #[cfg(not(target_os = "macos"))]
    {
        bind(a, Up, FocusUp);
        bind(a, Down, FocusDown);
        bind(a, Left, FocusLeft);
        bind(a, Right, FocusRight);
    }
    #[cfg(target_os = "macos")]
    {
        let ctrl_cmd = Mods::CTRL | Mods::SUPER;
        bind(ctrl_cmd, Up, FocusUp);
        bind(ctrl_cmd, Down, FocusDown);
        bind(ctrl_cmd, Left, FocusLeft);
        bind(ctrl_cmd, Right, FocusRight);
    }
    // Resize splits with Shift+Arrows only — `Ctrl+Shift+Up/Down` is
    // taken for `ScrollLineUp/Down`, so binding
    // `Ctrl+Shift+Left/Right` to Resize alone would have given an
    // inconsistent four-direction map (Up/Down scroll, Left/Right
    // resize). Drop the Ctrl+Shift+Arrows resize quartet entirely;
    // Shift+Arrows is the canonical Terminator-default chord. The
    // README and keybind table reflect this.
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
    // Broadcast toggle. Windows uses Ctrl+Shift+G because Game Bar owns Win+G.
    // macOS uses Ctrl+Cmd+B: Cmd+G / Cmd+Shift+G are Find Next / Previous, and
    // accidentally enabling broadcast from Find would duplicate later input
    // into every pane. Ctrl+Cmd+B has no standard macOS system meaning, keeps
    // the mnemonic, and is free in this map.
    #[cfg(windows)]
    bind(cs, Char('g'), ToggleBroadcastAll);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    bind(su, Char('g'), ToggleBroadcastAll);
    #[cfg(target_os = "macos")]
    bind(Mods::CTRL | Mods::SUPER, Char('b'), ToggleBroadcastAll);
    #[cfg(not(target_os = "macos"))]
    bind(sus, Char('g'), ToggleBroadcastOff);
    #[cfg(target_os = "macos")]
    bind(
        Mods::CTRL | Mods::SHIFT | Mods::SUPER,
        Char('b'),
        ToggleBroadcastOff,
    );
    bind(Mods::empty(), F(11), ToggleFullscreen);
    bind(cs, Char('m'), ReloadConfig);
    // Ctrl+, opens the Settings overlay (VS Code / common
    // convention). Ctrl+, is otherwise unused by shells, and the overlay is
    // also reachable from the right-click menu.
    bind(c, Char(','), OpenSettings);
    bind(c, Up, JumpPrevPrompt);
    bind(c, Down, JumpNextPrompt);
    bind(cs, Char('s'), OpenSsh);
    bind(cs, Char('k'), CommandPalette);
    bind(cs, Char('h'), HintMode);
    #[cfg(target_os = "macos")]
    {
        // Native macOS chords are additive: the portable Ctrl+Shift map above
        // remains available for users sharing one config across platforms.
        bind(su, Char('c'), Copy);
        bind(su, Char('v'), Paste);
        bind(su, Char('t'), NewTab);
        bind(su, Char('n'), NewWindow);
        // Apple Terminal semantics, not kettle's portable ones: Cmd+W is Close
        // Tab and Shift+Cmd+D is Close Split Pane. Binding Cmd+W to ClosePane
        // would silently close only the focused split of a split tab, which is
        // not what the standard close chord means on this platform.
        bind(su, Char('w'), CloseTab);
        bind(sus, Char('d'), ClosePane);
        bind(su, Char('f'), StartSearch);
        // Cmd+K clears the scrollback in Apple Terminal and iTerm2; the command
        // palette keeps its portable Ctrl+Shift+K and gains Shift+Cmd+P, the
        // chord every VS Code user already has in their fingers.
        bind(su, Char('k'), ClearHistory);
        bind(sus, Char('p'), CommandPalette);
        bind(su, Char(','), OpenSettings);
        bind(su, Char('='), IncreaseFontSize);
        bind(su, Char('+'), IncreaseFontSize);
        bind(sus, Char('='), IncreaseFontSize);
        bind(sus, Char('+'), IncreaseFontSize);
        bind(su, Char('-'), DecreaseFontSize);
        bind(su, Char('0'), ResetFontSize);
        bind(su, Up, JumpPrevPrompt);
        bind(su, Down, JumpNextPrompt);
        // Cmd+Backspace deletes to the start of the line in every native macOS
        // text field, and Super reaches a terminal application through nothing
        // but the Kitty protocol — so without a binding the chord was simply
        // dead. `\x15` is what Ghostty ships and what iTerm2's Natural Text
        // Editing preset sends: `unix-line-discard` in readline,
        // `kill-whole-line` in zsh's emacs keymap, `i_CTRL-U` in nvim insert
        // mode. A binding rather than an encoder fallback on purpose: it fires
        // whatever the client negotiated, where a fallback would defer to the
        // Kitty protocol and go dead in exactly the TUIs that negotiate it.
        // `keybind = cmd+backspace=unbind` turns it off.
        bind(su, Backspace, SendText("\x15".into()));
        for n in 1u8..=9 {
            bind(su, Char((b'0' + n) as char), GotoTab(n - 1));
        }
    }
    // Ctrl+Shift+Space toggles vi-mode (Alacritty default). This change
    // ships the entry + visible block cursor + Esc exit;
    // h/j/k/l movement + visual selection + yank come in a follow-up.
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
    // Shift+Home/End extend the text selection to the top / bottom of the buffer
    // (the AskUbuntu "select all in terminator" gesture). Scroll-to-extremes moved
    // to Ctrl+Home / Ctrl+End so both behaviors stay reachable. (These chords were
    // already keybinds — never forwarded to the PTY — so apps lose nothing.)
    bind(Mods::SHIFT, Home, SelectToTop);
    bind(Mods::SHIFT, End, SelectToBottom);
    bind(c, Home, ScrollToTop);
    bind(c, End, ScrollToBottom);
    // Alt+1..9 jumps to tab 1..9 (kitty / Terminator / Ghostty parity).
    // No-op when the requested tab doesn't exist — the app-side handler
    // already clamps against `tabs.len()`.
    for n in 1u8..=9 {
        bind(a, Char((b'0' + n) as char), GotoTab(n - 1));
    }
    // Dropdown parity (Windows Terminal's Ctrl+Shift+1..9 profile
    // shortcuts): open the Nth new-tab `▾` dropdown entry. Shift+digit
    // arrives as the US-shifted SYMBOL from winit (see the font-zoom
    // rationale above), so bind both spellings of each chord. None of
    // `! @ # $ % ^ & * (` collides with an existing Ctrl+Shift default.
    const US_SHIFTED_DIGITS: [char; 9] = ['!', '@', '#', '$', '%', '^', '&', '*', '('];
    for n in 1u8..=9 {
        bind(cs, Char((b'0' + n) as char), NewTabShell(n - 1));
        bind(
            cs,
            Char(US_SHIFTED_DIGITS[(n - 1) as usize]),
            NewTabShell(n - 1),
        );
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
    // Split on the LAST `=`, not the first. The trigger can
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

/// Bind `value` (`trigger=action`) as the ONLY chord for that action, dropping
/// any chord already bound to it.
///
/// This is Terminator's `[keybindings]` semantics, and it is deliberately NOT
/// what [`apply_keybind`] does. kettle's own `keybind = trigger=action` grammar
/// is additive on purpose — a user can give one action several chords.
/// Terminator's grammar is the inverse, `action = accelerator`: one accelerator
/// per action, and writing one means "this is the key for this", not "here is
/// another key for this".
///
/// Treating an imported line as additive left kettle's stock chord live
/// alongside the imported one. Someone rebinding `new_tab` precisely BECAUSE
/// Ctrl+Shift+T collides with tmux, AstroNvim, or an agent CLI found the chord
/// still captured after the import — the rebind looked like it worked and the
/// collision it was meant to resolve was still there.
pub fn apply_exclusive_keybind(map: &mut Bindings, value: &str) {
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
    let Some(a) = Action::from_name(act_trim) else {
        return;
    };
    // Drop every OTHER chord for this action first. Parameterized actions
    // (`goto_tab:3`) compare by value, so rebinding one tab's chord leaves the
    // other tabs' chords alone.
    map.retain(|existing, bound| *existing == t || *bound != a);
    map.insert(t, a);
}

/// Remove every chord bound to `action_name`.
///
/// Terminator's `[keybindings]` grammar disables a shortcut by giving it an
/// empty accelerator — its shipped defaults contain several, and its
/// preferences UI writes one when a binding is cleared. Ignoring those lines
/// left kettle's own default chord live, so a config that deliberately freed a
/// chord (to hand it back to tmux, AstroNvim, or an agent CLI) did not free it.
pub fn unbind_action(map: &mut Bindings, action_name: &str) {
    let Some(a) = Action::from_name(action_name) else {
        return;
    };
    map.retain(|_, bound| *bound != a);
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

    /// The production source of this file, excluding test-only items.
    fn production_source() -> String {
        let production = kettle_test_support::production_source(include_str!("keybinds.rs"));
        assert!(
            !production.contains("fn production_source()"),
            "the production slice retained its own helper"
        );
        assert!(
            !production.contains("#[test]"),
            "the production slice retained a test function"
        );
        assert!(
            !production.contains("#[cfg(test)]"),
            "the production slice retained a test-only item"
        );
        production
    }

    /// Only F1..=F12 are real keys (the winit→Key bridge maps no
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

    /// `Trigger::label()` must round-trip through `parse_trigger`
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
            Trigger::new(cs, Key::Char(' ')),
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
        // The parser accepts `ctrl+plus` / `ctrl+minus` /
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
        assert_eq!(
            Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char(' ')).label(),
            "Ctrl+Shift+Space"
        );
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
        // All map to the same Mods::SUPER bit. Earlier,
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
        // GTK's portable spelling, which is what its own docs tell people to
        // write and what a Terminator config copied off a Linux desktop
        // contains. It used to fall through to parse_key and silently degrade
        // `<Primary>t` to a bare `t`.
        let c_t = Trigger::new(Mods::CTRL, Key::Char('t'));
        assert_eq!(parse_trigger("primary+t"), Some(c_t));
        assert_eq!(parse_trigger("<Primary>t"), Some(c_t));
        assert_eq!(
            parse_trigger("<Primary><Shift>t"),
            Some(Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('t')))
        );
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
        // Same pattern as `enum_keys_are_case_insensitive`. A user
        // writing `keybind = ctrl+shift+c =
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
        // The update-banner actions resolve from both the
        // underscore and hyphen spellings, case-insensitively.
        assert_eq!(Action::from_name("open_update"), Some(OpenUpdate));
        assert_eq!(Action::from_name("OPEN-UPDATE"), Some(OpenUpdate));
        assert_eq!(Action::from_name("dismiss_update"), Some(DismissUpdate));
        assert_eq!(Action::from_name("Dismiss-Update"), Some(DismissUpdate));
    }

    #[test]
    fn unbound_v_modifier_chords_remain_available_to_child() {
        let defaults = defaults();
        assert!(!defaults.contains_key(&Trigger::new(Mods::CTRL, Key::Char('v'))));
        assert!(!defaults.contains_key(&Trigger::new(Mods::ALT, Key::Char('v'))));
        assert!(!defaults.contains_key(&Trigger::new(Mods::CTRL | Mods::ALT, Key::Char('v'))));
        assert_eq!(
            defaults.get(&Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('v'))),
            Some(&Action::Paste),
            "Kettle's own paste action remains Ctrl+Shift+V"
        );
    }

    #[test]
    fn readme_documented_chords_are_actually_bound() {
        // These 9 default bindings are documented in the README
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

        // The list above is portable by construction, so on its own it would
        // still pass if every macOS Cmd default vanished. Pin the documented
        // macOS chords on the leg that actually has them.
        #[cfg(target_os = "macos")]
        {
            let su = Mods::SUPER;
            let sus = Mods::SUPER | Mods::SHIFT;
            let ctrl_cmd = Mods::CTRL | Mods::SUPER;
            let mac_pairs: &[(Mods, Key, Action)] = &[
                (su, Key::Char('c'), Copy),
                (su, Key::Char('v'), Paste),
                (su, Key::Char('t'), NewTab),
                (su, Key::Char('n'), NewWindow),
                (su, Key::Char('w'), CloseTab),
                (sus, Key::Char('d'), ClosePane),
                (su, Key::Char('f'), StartSearch),
                (su, Key::Char('k'), ClearHistory),
                (sus, Key::Char('p'), CommandPalette),
                (su, Key::Char(','), OpenSettings),
                (su, Key::Char('0'), ResetFontSize),
                (su, Key::Char('1'), GotoTab(0)),
                (su, Key::Char('9'), GotoTab(8)),
                (ctrl_cmd, Key::Char('b'), ToggleBroadcastAll),
                (ctrl_cmd, Key::Left, FocusLeft),
            ];
            for (mods, k, want) in mac_pairs {
                let trig = Trigger::new(*mods, *k);
                assert_eq!(
                    d.get(&trig),
                    Some(want),
                    "README claims {} → {want:?} on macOS; bound to {:?} instead",
                    trig.label(),
                    d.get(&trig)
                );
            }

            // Additive, not replacing: a config shared with Linux/Windows must
            // keep working on this machine too.
            assert_eq!(
                d.get(&Trigger::new(cs, Key::Char('c'))),
                Some(&Copy),
                "the portable Ctrl+Shift+C must stay bound on macOS"
            );
            // Cmd+G is Find Next on macOS; broadcast must never sit there.
            assert!(
                !d.contains_key(&Trigger::new(su, Key::Char('g'))),
                "Cmd+G is the system Find Next chord and must stay unbound"
            );
        }
    }

    #[test]
    fn defaults_has_no_shadow_collisions() {
        // Systemic guard against shadow collisions. An earlier fix
        // caught a single shadow collision (Ctrl+Shift+Up/Down both
        // bound to Resize *and* ScrollLine — second one silently
        // wins). The class of bug is
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
    fn broadcast_toggle_default_is_platform_correct() {
        // Win+G is captured by the Windows Game Bar before kettle sees it, and
        // macOS reserves Cmd+G for Find Next. Pin all three platform choices so
        // a future edit cannot silently put broadcast back on a system chord.
        let d = defaults();
        #[cfg(not(target_os = "macos"))]
        let cs = Mods::CTRL | Mods::SHIFT;
        let su = Mods::SUPER;
        #[cfg(target_os = "macos")]
        let ctrl_cmd = Mods::CTRL | Mods::SUPER;
        #[cfg(windows)]
        {
            assert_eq!(
                d.get(&Trigger::new(cs, Key::Char('g'))),
                Some(&Action::ToggleBroadcastAll),
                "on Windows broadcast toggle must be Ctrl+Shift+G (Win+G is Game Bar)"
            );
            assert_eq!(
                d.get(&Trigger::new(su, Key::Char('g'))),
                None,
                "Super+G must NOT be bound on Windows (Game Bar swallows it)"
            );
        }
        #[cfg(all(not(windows), not(target_os = "macos")))]
        {
            assert_eq!(
                d.get(&Trigger::new(su, Key::Char('g'))),
                Some(&Action::ToggleBroadcastAll),
                "off Windows broadcast toggle stays on Super+G"
            );
            assert_eq!(
                d.get(&Trigger::new(cs, Key::Char('g'))),
                None,
                "Ctrl+Shift+G is only the Windows fallback"
            );
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(
                d.get(&Trigger::new(ctrl_cmd, Key::Char('b'))),
                Some(&Action::ToggleBroadcastAll),
                "macOS broadcast toggle must avoid the system Find chords"
            );
            assert_eq!(
                d.get(&Trigger::new(su, Key::Char('g'))),
                None,
                "Cmd+G must remain available for Find Next"
            );
            assert_eq!(
                d.get(&Trigger::new(Mods::SUPER | Mods::SHIFT, Key::Char('g'))),
                None,
                "Cmd+Shift+G must remain available for Find Previous"
            );
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            d.get(&Trigger::new(Mods::SUPER | Mods::SHIFT, Key::Char('g'))),
            Some(&Action::ToggleBroadcastOff),
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            d.get(&Trigger::new(
                Mods::CTRL | Mods::SHIFT | Mods::SUPER,
                Key::Char('b'),
            )),
            Some(&Action::ToggleBroadcastOff),
        );
    }

    #[test]
    fn scroll_line_up_down_bound_to_ctrl_shift_arrows() {
        // Alacritty / kitty / WezTerm all bind a
        // chord for line-by-line scrollback navigation, but kettle
        // shipped only PageUp/PageDown (Shift) and Top/Bottom (Shift
        // Home/End). Ctrl+Shift+Up/Down fills the gap with the most
        // commonly-used chord across modern terminals.
        //
        // This binding collided with the previous Ctrl+Shift+Arrows
        // → Resize quartet, so the Resize-via-Ctrl+Shift+Arrows
        // defaults were dropped entirely as a regression guard.
        // Shift+Arrows is now the canonical resize chord
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
        // other tests rely on these existing).
        assert!(d.contains_key(&Trigger::new(Mods::SHIFT, Key::PageUp)));
        assert_eq!(
            d.get(&Trigger::new(Mods::CTRL, Key::Up)),
            Some(&Action::JumpPrevPrompt),
            "JumpPrev/Next (Ctrl+Up/Down) must coexist with Ctrl+Shift+Up/Down"
        );
        // Guards against the Ctrl+Shift+Arrows → Resize quartet
        // returning (would silently shadow ScrollLineUp/Down).
        // Shift+Arrows is the only resize chord now.
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
    fn shift_home_end_select_and_scroll_moves_to_ctrl() {
        // Keyboard text-selection feature: Shift+Home/End now extend the
        // selection to the top / bottom of the buffer; scroll-to-extremes
        // relocated to Ctrl+Home / Ctrl+End so both behaviors stay reachable.
        let d = defaults();
        assert_eq!(
            d.get(&Trigger::new(Mods::SHIFT, Key::Home)),
            Some(&Action::SelectToTop)
        );
        assert_eq!(
            d.get(&Trigger::new(Mods::SHIFT, Key::End)),
            Some(&Action::SelectToBottom)
        );
        assert_eq!(
            d.get(&Trigger::new(Mods::CTRL, Key::Home)),
            Some(&Action::ScrollToTop)
        );
        assert_eq!(
            d.get(&Trigger::new(Mods::CTRL, Key::End)),
            Some(&Action::ScrollToBottom)
        );
        // SelectAll is bindable but intentionally has no default chord.
        assert!(!d.values().any(|a| *a == Action::SelectAll));
        // The new tokens parse back through from_name (round-trip).
        assert_eq!(Action::from_name("select_all"), Some(Action::SelectAll));
        assert_eq!(
            Action::from_name("select_to_top"),
            Some(Action::SelectToTop)
        );
        assert_eq!(
            Action::from_name("select-to-bottom"),
            Some(Action::SelectToBottom)
        );
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

    /// Dropdown parity: `new_tab_shell_N` parses like the established
    /// `switch_to_tab_N` shape — 1-based config form, 0-based variant, kebab
    /// alias accepted, 0 rejected.
    #[test]
    fn new_tab_shell_parses_like_switch_to_tab() {
        assert_eq!(
            Action::from_name("new_tab_shell_1"),
            Some(Action::NewTabShell(0))
        );
        assert_eq!(
            Action::from_name("new-tab-shell-9"),
            Some(Action::NewTabShell(8))
        );
        assert_eq!(Action::from_name("new_tab_shell_0"), None);
        assert_eq!(Action::from_name("new_tab_shell_"), None);
        assert_eq!(Action::from_name("about"), Some(Action::About));
        assert_eq!(Action::from_name("show-about"), Some(Action::About));
    }

    /// Dropdown parity: Ctrl+Shift+1..9 must work on a US layout where
    /// winit reports the SHIFTED symbol (the font-zoom precedent) — both the
    /// digit and its symbol map to the same NewTabShell entry.
    #[test]
    fn ctrl_shift_digit_binds_cover_us_layout_shift_variants() {
        let d = defaults();
        let cs = Mods::CTRL | Mods::SHIFT;
        let shifted = ['!', '@', '#', '$', '%', '^', '&', '*', '('];
        for n in 0u8..9 {
            for k in [(b'1' + n) as char, shifted[n as usize]] {
                let t = Trigger::new(cs, Key::Char(k));
                assert_eq!(
                    d.get(&t),
                    Some(&Action::NewTabShell(n)),
                    "Ctrl+Shift+{k:?} should open dropdown shell {}",
                    n + 1
                );
            }
        }
    }

    /// Dropdown parity: `hint_label` reverse-lookup — prefers the
    /// alphanumeric spelling of a chord and follows user rebinds.
    #[test]
    fn hint_label_prefers_alphanumeric_trigger_and_follows_rebinds() {
        let d = defaults();
        // Both Ctrl+Shift+1 and Ctrl+Shift+! are bound; the hint shows the
        // digit spelling.
        assert_eq!(
            hint_label(&d, &Action::NewTabShell(0)).as_deref(),
            Some("Ctrl+Shift+1")
        );
        assert_eq!(
            hint_label(&d, &Action::OpenSettings).as_deref(),
            Some("Ctrl+,")
        );
        // An unbound action yields no hint.
        assert_eq!(hint_label(&d, &Action::OpenUpdate), None);
        // A rebind takes over once the defaults are unbound.
        let mut m = d.clone();
        apply_keybind(&mut m, "ctrl+shift+1=unbind");
        apply_keybind(&mut m, "ctrl+shift+!=unbind");
        apply_keybind(&mut m, "ctrl+f9=new_tab_shell_1");
        assert_eq!(
            hint_label(&m, &Action::NewTabShell(0)).as_deref(),
            Some("Ctrl+F9")
        );
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
        // Reverse guard. This used to pin two hand-written names, which is a
        // guard that cannot enforce what `docs/CONFIG.md` promises: that
        // `--list-actions` prints *every* accepted alias. Deriving the set from
        // `from_name`'s own arms is what makes that promise checkable — when it
        // first ran it found twenty-seven aliases this list had been hiding.
        let listed: std::collections::BTreeSet<String> =
            names.iter().map(|n| n.replace('-', "_")).collect();
        let body = from_name_body();
        let mut accepted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut hidden: Vec<String> = Vec::new();
        for lit in body.split('"').skip(1).step_by(2) {
            // Parametric prefixes (`goto_tab:`), sentinels (`unbind`) and
            // non-name literals all fail `from_name`, which is the filter.
            if Action::from_name(lit).is_none() {
                continue;
            }
            let normalized = lit.replace('-', "_");
            accepted.insert(normalized.clone());
            if !listed.contains(&normalized) && !hidden.contains(&normalized) {
                hidden.push(normalized);
            }
        }
        // Count DISTINCT names, not literal occurrences. Most arms carry both
        // the underscore and hyphen spelling, so an occurrence count runs ~40%
        // ahead of the real set and a floor set against it would still be met
        // after the extraction silently lost a third of the table. The floor
        // sits just under the current distinct count for the same reason.
        assert!(
            accepted.len() >= 210,
            "only {} distinct accepted names found in from_name — the body \
             extraction is wrong, so this guard would pass vacuously",
            accepted.len()
        );
        // Truncation check. `from_name_body` matches braces without lexing, so
        // a future `// }}` comment could close the body early; the count floor
        // would still pass because the lost arms are at the end. Pin a name
        // from the last arm instead, which no early close can leave in view.
        assert!(
            accepted.contains("toggle_pane_read_only"),
            "the extracted from_name body is missing its final arms, so every \
             alias past the truncation point is invisible to this guard"
        );
        assert!(
            hidden.is_empty(),
            "from_name accepts {} name(s) that action_names() omits, so \
             `--list-actions` hides them: {hidden:?}",
            hidden.len()
        );
        // The three parametric forms cannot be listed (N is unbounded), so
        // `--list-actions` prints each as a trailing note instead. Pin all
        // three: `switch_to_tab_N` parsed but appeared in no output at all,
        // which is what made the documented "complete set" claim false.
        assert!(Action::from_name("goto_tab:1").is_some());
        assert!(Action::from_name("switch_to_tab_1").is_some());
        assert!(Action::from_name("new_tab_shell_1").is_some());
        // And `unbind` is intentionally NOT a listed action — it's a
        // sentinel for `apply_keybind`, not a real Action variant.
        assert!(!names.contains(&"unbind"));
        assert!(Action::from_name("unbind").is_none());
    }

    #[test]
    fn describe_reflects_user_overrides_and_unbinds() {
        // Contract: `--list-keybinds --config FILE` should
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
        // `Ctrl+Shift+PageDown` (19 chars; move-tab-right) and
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cmd_digit_keys_go_to_tab() {
        let d = defaults();
        for n in 1u8..=9 {
            let trigger = Trigger::new(Mods::SUPER, Key::Char((b'0' + n) as char));
            assert_eq!(
                d.get(&trigger),
                Some(&Action::GotoTab(n - 1)),
                "Cmd+{n} must select tab {n}"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_option_arrows_reach_the_pty_and_directional_focus_stays_bound() {
        let d = defaults();
        for key in [Key::Up, Key::Down, Key::Left, Key::Right] {
            let trigger = Trigger::new(Mods::ALT, key);
            assert!(
                !d.contains_key(&trigger),
                "bare Option+{} must remain available for terminal word motion",
                trigger.label()
            );
        }
        let ctrl_cmd = Mods::CTRL | Mods::SUPER;
        for (key, action) in [
            (Key::Up, Action::FocusUp),
            (Key::Down, Action::FocusDown),
            (Key::Left, Action::FocusLeft),
            (Key::Right, Action::FocusRight),
        ] {
            assert_eq!(d.get(&Trigger::new(ctrl_cmd, key)), Some(&action));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_default_map_has_no_shadow_collisions() {
        let (bindings, triggers) = defaults_audit();
        assert_eq!(
            bindings.len(),
            triggers.len(),
            "every macOS default trigger must be unique"
        );
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

    /// The `=` key can itself be a trigger (a shipped
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

    /// Drift guard. Terminator-spelling aliases for
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

    /// Every action name must resolve in BOTH spellings.
    ///
    /// The config tokenizer folds `_` to `-` so that Terminator's key
    /// spellings match kettle's hyphenated arms. That fold also rewrites the
    /// action names in a `[keybindings]` section — `new_tab` arrives here as
    /// `new-tab` — and this table is written almost entirely in underscores,
    /// so the section imported as nothing at all. The reverse fold in
    /// `from_name` closes it; this walks the table to prove there is no name
    /// that works in one spelling only, in either direction.
    /// `from_name`'s body, delimited by brace matching rather than by the next
    /// `pub fn` — the loose bound runs past the end of the method and sweeps in
    /// literals from the key-name parser (`ctrl`, `home`, `up`), which the
    /// reverse-coverage guard would then demand `--list-actions` publish as
    /// actions. Asserted below rather than assumed.
    fn from_name_body() -> String {
        let src = production_source();
        let start = src.find("    pub fn from_name(").expect("from_name");
        let open = src[start..].find('{').expect("from_name body") + start;
        let mut depth = 0usize;
        let mut end = None;
        for (offset, ch) in src[open..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + offset + ch.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        let body = src[start..end.expect("from_name body is brace-balanced")].to_owned();
        // The key-name parser lives outside this method; if either literal is
        // in the slice the bound ran past `from_name` and every count below is
        // measuring the wrong text.
        assert!(
            !body.contains("\"ctrl\"") && !body.contains("\"pageup\""),
            "from_name body extraction over-ran into the key-name parser"
        );
        body
    }

    #[test]
    fn every_action_name_resolves_in_both_spellings() {
        let body = from_name_body();
        let body = body.as_str();

        let mut checked = 0usize;
        for lit in body.split('"').skip(1).step_by(2) {
            // Only the action-name literals: lowercase words joined by `_`
            // or `-`, which is every arm pattern in the table.
            if lit.is_empty()
                || !lit
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
            {
                continue;
            }
            let Some(expected) = Action::from_name(lit) else {
                continue;
            };
            checked += 1;
            let underscored = lit.replace('-', "_");
            let hyphenated = lit.replace('_', "-");
            assert_eq!(
                Action::from_name(&underscored),
                Some(expected.clone()),
                "{lit:?} must also resolve as {underscored:?}"
            );
            assert_eq!(
                Action::from_name(&hyphenated),
                Some(expected),
                "{lit:?} must also resolve as {hyphenated:?} — the config \
                 tokenizer hands action names over in this spelling"
            );
        }
        assert!(
            checked > 100,
            "expected to walk the whole action table, only saw {checked} names"
        );
    }

    /// Drift guard: Terminator's scroll/tab action spellings parse so
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

    /// Drift guard: every alias for `take_screenshot`
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

    /// Drift guard: each alias for the open-cwd-in-file-
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

    /// Drift guard: every alias the parser accepts for
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

    /// Terminator's `*_toggle` names toggle GROUPING, not broadcasting.
    ///
    /// `window.py:940/959/987` each flip a group assignment; broadcasting to a
    /// group is a separate, later choice. This test previously asserted the
    /// broadcast mapping, so it pinned the wrong behavior in place: importing
    /// a Terminator config bound a grouping key to a broadcast toggle, and one
    /// press sent everything the user typed to every pane at once.
    ///
    /// Each toggle is its OWN action rather than an alias of the non-toggling
    /// one. Two Terminator names resolving to a single kettle action made an
    /// imported config containing both silently unbind the first, since an
    /// imported binding is exclusive per action.
    #[test]
    fn from_name_accepts_terminator_group_toggle_aliases() {
        for (s, want) in [
            ("group_all_toggle", Action::ToggleGroupAll),
            ("group-all-toggle", Action::ToggleGroupAll),
            ("group_tab_toggle", Action::ToggleGroupTab),
            ("group-tab-toggle", Action::ToggleGroupTab),
            ("group_win_toggle", Action::ToggleGroupWindow),
            ("group-win-toggle", Action::ToggleGroupWindow),
        ] {
            assert_eq!(
                Action::from_name(s).as_ref(),
                Some(&want),
                "alias {s:?} should parse to {want:?}"
            );
        }
    }

    /// Drift guard. Terminator's `key_preferences`
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

    /// Drift guard. Terminator's `send_newline`
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

    /// Drift guard. Terminator's F1 `key_help`
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

    /// Drift guard. Terminator's `scaled_zoom` keybind
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

    /// `⌘⌫` was dead because the chord could not be written down: `Key` had no
    /// Backspace variant, so `parse_trigger` returned `None` and every
    /// `cmd+backspace` line in a config file was a malformed value.
    #[test]
    fn backspace_and_delete_are_bindable_triggers() {
        let sb = Trigger::new(Mods::SUPER, Key::Backspace);
        assert_eq!(parse_trigger("cmd+backspace"), Some(sb));
        assert_eq!(parse_trigger("super+bs"), Some(sb));
        assert_eq!(parse_trigger("CMD+Backspace"), Some(sb));
        let cd = Trigger::new(Mods::CTRL, Key::Delete);
        assert_eq!(parse_trigger("ctrl+delete"), Some(cd));
        assert_eq!(parse_trigger("ctrl+del"), Some(cd));
        // Both keys round-trip through the label, like every other trigger.
        for t in [sb, cd] {
            assert_eq!(parse_trigger(&t.label()), Some(t), "label {}", t.label());
        }
    }

    /// The payload of a `text:` binding is data, not an action name. Three of
    /// these rules exist to keep a payload from breaking something else:
    /// `=` is the `keybind =` separator, `\xHH` past 7f would be invalid UTF-8
    /// on the way to a PTY, and an unrecognized escape has to surface in
    /// `--check-config` rather than be sent as a literal backslash.
    #[test]
    fn send_text_payload_decodes_and_rejects() {
        let text = |s: &str| Action::from_name(s);
        assert_eq!(text("text:\\x15"), Some(Action::SendText("\x15".into())));
        assert_eq!(text("text:\\e[A"), Some(Action::SendText("\x1b[A".into())));
        assert_eq!(
            text("text:\\n\\r\\t\\a\\b\\f\\v\\0\\\\"),
            Some(Action::SendText("\n\r\t\x07\x08\x0c\x0b\0\\".into()))
        );
        // Case and hyphens survive: the folding that normalizes action names
        // must not reach the payload.
        assert_eq!(
            text("text:Hello-World"),
            Some(Action::SendText("Hello-World".into()))
        );
        // Non-ASCII is written literally and encoded as UTF-8.
        assert_eq!(text("text:é"), Some(Action::SendText("é".into())));
        // `TEXT:` is the same prefix; only the payload is case-sensitive.
        assert_eq!(text("TEXT:Ab"), Some(Action::SendText("Ab".into())));
        // `=` must be escaped, because `apply_keybind` splits on the last one.
        assert_eq!(text("text:a=b"), None);
        assert_eq!(text("text:a\\x3db"), Some(Action::SendText("a=b".into())));
        // Rejections.
        assert_eq!(text("text:"), None, "empty payload is not an action");
        assert_eq!(text("text:\\q"), None, "unknown escape");
        assert_eq!(text("text:\\x80"), None, "would be invalid UTF-8");
        assert_eq!(text("text:\\xzz"), None, "not hex");
        assert_eq!(text("text:\\"), None, "truncated escape");
        assert_eq!(text("text:\\x1"), None, "truncated hex escape");
        let long = format!("text:{}", "a".repeat(MAX_SEND_TEXT_BYTES));
        assert!(text(&long).is_some(), "at the cap");
        let too_long = format!("text:{}", "a".repeat(MAX_SEND_TEXT_BYTES + 1));
        assert_eq!(text(&too_long), None, "over the cap");
    }

    /// Two `text:` bindings with different payloads are different actions, so
    /// rebinding one does not silently unbind the other. Same property
    /// `GotoTab(N)` relies on in `apply_exclusive_keybind`.
    #[test]
    fn send_text_payloads_are_independent_bindings() {
        let mut m: Bindings = Bindings::new();
        apply_keybind(&mut m, "ctrl+shift+y=text:\\x01");
        apply_keybind(&mut m, "ctrl+shift+u=text:\\x05");
        assert_eq!(
            m.get(&Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('y'))),
            Some(&Action::SendText("\x01".into()))
        );
        assert_eq!(
            m.get(&Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('u'))),
            Some(&Action::SendText("\x05".into()))
        );
    }

    /// The label reaches `--list-keybinds`, `describe` and the Settings
    /// conflict dialog — all of which print to the user's own terminal. A
    /// `Debug`-derived label would have put a raw `0x15` there.
    #[test]
    fn send_text_label_never_emits_a_raw_control_byte() {
        let label = action_label(&Action::SendText("\x15".into()));
        assert_eq!(label, r#"Send text "\x15""#);
        for a in [
            Action::SendText("\x1b[A".into()),
            Action::SendText("a\nb\tc\\d=e".into()),
            Action::SendText("\x7f".into()),
        ] {
            let label = action_label(&a);
            assert!(
                !label.chars().any(|c| (c as u32) < 0x20 || c == '\x7f'),
                "label leaked a control byte: {label:?}"
            );
        }
        // And the escaped form is what the user would type back.
        assert_eq!(
            action_label(&Action::SendText("\x1b[A".into())),
            r#"Send text "\e[A""#
        );
    }

    /// `⌘⌫` is documented in the README's Common keys table, and in this repo a
    /// documented chord is a pinned chord.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_binds_command_backspace_to_delete_to_line_start() {
        assert_eq!(
            defaults().get(&Trigger::new(Mods::SUPER, Key::Backspace)),
            Some(&Action::SendText("\x15".into())),
            "Cmd+Backspace must send ^U, as Ghostty and iTerm2's natural-text \
             preset do"
        );
    }

    /// The chord is macOS-native, so it must not appear anywhere else.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_defaults_leave_backspace_alone() {
        assert!(
            defaults()
                .keys()
                .all(|t| !matches!(t.key, Key::Backspace | Key::Delete)),
            "Backspace/Delete defaults are macOS-only"
        );
    }
}

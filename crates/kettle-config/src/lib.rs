//! kettle configuration: Ghostty-compatible `key = value` config, the bundled
//! Ghostty theme set (TokyoNight Night default), the embedded Nerd Font,
//! Terminator-compatible keybindings, and the fuzzy matcher / command-palette
//! infrastructure the SSH launcher (Ctrl+Shift+S) and command palette
//! (Ctrl+Shift+K) reuse.
//!
//! Modules (all `pub` except `theme_filter`):
//! - [`parse`] — Ghostty-syntax tokenizer: one `key = value` per line,
//!   first `=` splits, full-line `#` comments only, BOM-strip, empty-
//!   value-resets semantics. The single source of truth for *what* a
//!   config file is.
//! - [`color`] — `Rgb` + parser accepting `#rrggbb` / `#rgb` / `0xRRGGBB`
//!   / X11 color names.
//! - [`theme`] — bundled-theme set baked in at build time via the
//!   `theme_filter` skip list; `Theme::by_name` for case-insensitive
//!   lookup, `Theme::find_name` for canonical-form rewriting,
//!   `Theme::cycle` for runtime forward/back navigation.
//! - [`keybinds`] — `Action` enum, `Trigger` (modifiers + key), parser
//!   (accepts `win`/`meta`/`logo` / `cmd` / `super` Super-key aliases
//!   and rejects typo'd modifiers), default Terminator-compatible
//!   bindings, `apply_keybind` for user overrides, `describe` for
//!   `--list-keybinds`.
//! - [`palette`] — command-palette registry: friendly label + Action
//!   pairs the UI fuzzy-ranks via [`fuzzy`].
//! - [`fuzzy`] — dependency-free subsequence-with-bonuses ranker;
//!   `score(pattern, candidate)` + `best(pattern, items, key)`. Used by
//!   palette and SSH launcher.
//! - [`font`] — embedded JetBrains Mono Nerd Font (`FAMILY` + `all()`
//!   font-face bytes for `cosmic-text` loading).
//! - [`template`] — `{title}` / `{cwd}` / `{tab}` placeholder substitution
//!   for `window-title-format` / `tab-format`.
//! - `theme_filter` (private) — filter for what counts as a real theme
//!   file under `assets/themes/`; shared between this crate and
//!   `build.rs` via `include!`.

pub mod color;
pub mod font;
pub mod fuzzy;
pub mod keybinds;
pub mod palette;
pub mod parse;
pub mod template;
pub mod theme;
mod theme_filter;

use std::path::{Path, PathBuf};

pub use color::Rgb;
pub use keybinds::{Action, Bindings, Key, Mods, Trigger};
pub use theme::Theme;

/// Practical stand-in for "infinite" scrollback: ~10M lines (keeps memory
/// bounded while never realistically clipping history).
pub const INFINITE_SCROLLBACK: usize = 10_000_000;

/// Parse the standard true/false aliases. Cycle 138 introduces this
/// because every previous boolean config used `e.value != "false"`,
/// which silently treats "no", "off", "0", and "disabled" as `true`.
/// Case-insensitive; whitespace already trimmed by the line tokenizer.
/// Returns `None` on an unrecognized value so callers keep the prior
/// state (rather than silently flipping) and `detect_malformed_values`
/// can surface the typo.
pub(crate) fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" | "enabled" | "enable" | "y" => Some(true),
        "false" | "no" | "off" | "0" | "disabled" | "disable" | "n" => Some(false),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}

/// How the terminal reacts to `BEL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BellMode {
    Off,
    /// Brief full-surface flash.
    Visual,
    /// Request window attention (taskbar/dock urgency) when unfocused.
    Attention,
    /// Both visual flash and window attention.
    Both,
}

/// Cycle 617 (Terminator parity, terminatorlib/config.py:117
/// `case_sensitive`): scrollback-search case-sensitivity mode.
///
/// kettle defaults to `Smart` (ripgrep/vim semantics: case-
/// insensitive until the pattern contains any uppercase letter).
/// `Always` forces case-sensitive even for all-lowercase patterns
/// (matches Terminator's default), `Never` forces case-insensitive
/// even for mixed-case patterns. Maps to `kettle_core::search::
/// CaseSensitivity` at search time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchCaseSensitivity {
    #[default]
    Smart,
    Always,
    Never,
}

impl BellMode {
    pub fn visual(self) -> bool {
        matches!(self, BellMode::Visual | BellMode::Both)
    }
    pub fn attention(self) -> bool {
        matches!(self, BellMode::Attention | BellMode::Both)
    }
    /// Cycle 619: compose two bell flavors (used by the Terminator-
    /// compat `urgent_bell` + `visible_bell` arms which compose into
    /// kettle's unified `BellMode`). Idempotent: `compose(x, x) == x`.
    /// Pure.
    pub fn compose(self, other: BellMode) -> BellMode {
        match (
            self.visual() || other.visual(),
            self.attention() || other.attention(),
        ) {
            (true, true) => BellMode::Both,
            (true, false) => BellMode::Visual,
            (false, true) => BellMode::Attention,
            (false, false) => BellMode::Off,
        }
    }
}

/// OSC 52 clipboard policy. The **read** path lets a (possibly remote)
/// program read your system clipboard, so it is denied by default —
/// `Copy` allows programs to *set* the clipboard but not read it
/// (xterm/kitty-style safe default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Osc52 {
    /// Ignore OSC 52 entirely.
    Off,
    /// Allow clipboard *writes* only (default).
    Copy,
    /// Allow clipboard *reads* (paste-back) only.
    Paste,
    /// Allow both.
    Both,
}

impl Osc52 {
    /// May a program set the clipboard via OSC 52?
    pub fn can_copy(self) -> bool {
        matches!(self, Osc52::Copy | Osc52::Both)
    }
    /// May a program read the clipboard via OSC 52 (`?` query)?
    pub fn can_paste(self) -> bool {
        matches!(self, Osc52::Paste | Osc52::Both)
    }
}

/// When the tab bar is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarMode {
    /// Never shown.
    Off,
    /// Shown only when there is more than one tab.
    Auto,
    /// Always shown.
    Always,
}

/// Where the tab bar sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarPos {
    Top,
    Bottom,
}

/// Cycle 295 (iTerm2 / kitty parity): status-bar position. A thin
/// strip showing HH:MM:SS · theme · focused pane title. Disabled by
/// default — turning it on subtracts one row from each pane's grid,
/// so chatty users with 80x24 budgets stay in control. Future cycle
/// adds CPU / MEM widgets via `sysinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusBarMode {
    #[default]
    Off,
    Top,
    Bottom,
}

/// Cycle 341 (Terminator parity, terminatorlib/config.py:118
/// `background_type`): background style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundType {
    /// Solid color (Terminator + kettle default).
    #[default]
    Solid,
    /// Use the `background_image` file.
    Image,
    /// Transparent (uses `background_darkness` to dim).
    Transparent,
}

/// Cycle 376 (Terminator plugin parity, plugin sub-cycle 12): Lua
/// sandbox level. `Safe` is the default — Lua plugins can still
/// access the kettle.* APIs but the dangerous parts of the Lua
/// stdlib (os.execute, io.open, os.exit, package.loadlib) are nil'd
/// out. `Trusted` exposes everything; user explicitly opts in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LuaSandbox {
    #[default]
    Safe,
    Trusted,
}

/// Cycle 339 (Terminator parity, terminatorlib/config.py:73
/// `focus`): focus mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusMode {
    /// Click to focus (Terminator + kettle default).
    #[default]
    Click,
    /// Focus follows mouse cursor.
    Sloppy,
    /// Use the OS / desktop-environment default.
    System,
}

/// Cycle 339 (Terminator parity, terminatorlib/config.py:75
/// `window_state`): initial window state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowState {
    /// Standard windowed (kettle + Terminator default).
    #[default]
    Normal,
    /// Maximize at launch.
    Maximise,
    /// Fullscreen at launch (no chrome).
    Fullscreen,
    /// Launch hidden — useful for Quake-style dropdown setups
    /// where `kettle --toggle` brings it up.
    Hidden,
}

/// Cycle 338 (Terminator parity, terminatorlib/config.py:107
/// `backspace_binding`): how Backspace key is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackspaceBinding {
    /// Send ASCII DEL (0x7f) — VTE convention + kettle default.
    #[default]
    AsciiDel,
    /// Send Ctrl-H (0x08).
    ControlH,
    /// Send the escape sequence `\e[3~`.
    EscapeSequence,
    /// Automatic per the TERM database.
    Automatic,
}

/// Cycle 338 (Terminator parity, terminatorlib/config.py:108
/// `delete_binding`): how Delete key is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeleteBinding {
    /// Send ASCII DEL (0x7f).
    AsciiDel,
    /// Send Ctrl-H.
    ControlH,
    /// Send the escape sequence `\e[3~` — VTE convention + kettle default.
    #[default]
    EscapeSequence,
    /// Automatic per the TERM database.
    Automatic,
}

/// Cycle 338 (Terminator parity, terminatorlib/config.py:71
/// `broadcast_default`): default broadcast scope when none set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BroadcastDefault {
    /// Broadcast to every pane in every window (Terminator's
    /// most-permissive mode).
    All,
    /// Broadcast within a per-tab group (kettle default — matches
    /// the cycle-178 per-tab broadcast model).
    #[default]
    Group,
    /// Don't broadcast (each pane gets its own input).
    Off,
}

/// Cycle 336 (Terminator parity, terminatorlib/config.py:118
/// `exit_action`): what to do when the shell process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExitAction {
    /// Close the pane (kettle default + Terminator default).
    #[default]
    Close,
    /// Re-spawn the shell. Useful for long-running session windows.
    Restart,
    /// Keep the pane open with the dead shell visible (so the user
    /// can read final output / scrollback before manually closing).
    Hold,
}

/// Cycle 336 (Terminator parity, terminatorlib/config.py:79
/// `ask_before_closing`): when to show the close-confirmation dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AskBeforeClosing {
    /// Always show the dialog, even with one pane (Terminator's
    /// most-cautious mode).
    Always,
    /// Show the dialog only when ≥2 panes/tabs would be killed.
    /// Terminator's default.
    #[default]
    MultipleTerminals,
    /// Never show the dialog. Close immediately.
    Never,
}

/// When the per-pane scrollback scrollbar is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarMode {
    Never,
    /// Only while scrolled back into history.
    Auto,
    Always,
}

/// One OpenType feature override: a 4-byte tag (space-padded, e.g. `liga`,
/// `calt`, `ss01`, `zero`, `cv01`) and its value (`0` = off, `1` = on, or a
/// font-specific alternate index).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FontFeature {
    pub tag: [u8; 4],
    pub value: u32,
}

impl FontFeature {
    /// Parse one `font-feature` token. Accepts the common dialects:
    /// `liga` / `+liga` / `liga on` / `liga=1` (enable),
    /// `-liga` / `liga off` / `liga=0` (disable), `cv01=2` / `ss05 3`
    /// (explicit value). Tag = 1–4 ASCII alphanumerics, right-padded with
    /// spaces. Returns `None` if it isn't a well-formed feature token.
    pub fn parse(tok: &str) -> Option<FontFeature> {
        let tok = tok.trim();
        if tok.is_empty() {
            return None;
        }
        let (mut name, mut value): (&str, Option<u32>) = (tok, None);
        // Leading +/- sign form.
        if let Some(rest) = tok.strip_prefix('-') {
            name = rest;
            value = Some(0);
        } else if let Some(rest) = tok.strip_prefix('+') {
            name = rest;
            value = Some(1);
        }
        // `tag=N`, `tag N`, `tag on`, `tag off` forms.
        if value.is_none() {
            let split = name
                .split_once('=')
                .or_else(|| name.split_once(char::is_whitespace));
            if let Some((n, v)) = split {
                name = n.trim();
                let v = v.trim();
                value = Some(match v {
                    "on" | "true" | "yes" => 1,
                    "off" | "false" | "no" => 0,
                    _ => v.parse().ok()?,
                });
            }
        }
        let name = name.trim();
        if name.is_empty() || name.len() > 4 || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return None;
        }
        // OpenType feature tags are case-sensitive: the spec defines every
        // standard tag in *lowercase* (`liga`, `clig`, `calt`, `cv01`,
        // `ss05`…). A user writing `font-feature = LIGA on` would otherwise
        // store an uppercase tag, which (a) fails `is_ligature()` so the
        // coarse `font_ligatures` config flag stays stale, and (b) wouldn't
        // be recognized by the cosmic-text / harfbuzz shaper downstream.
        // Lowercase here so both checks see the canonical form. (Numeric
        // chars like `cv01` are unaffected by the lowercasing.)
        let mut tag = [b' '; 4];
        tag[..name.len()].copy_from_slice(name.as_bytes());
        for b in tag.iter_mut() {
            b.make_ascii_lowercase();
        }
        Some(FontFeature {
            tag,
            value: value.unwrap_or(1),
        })
    }

    /// Whether this token toggles a ligature-class feature (so the coarse
    /// `font_ligatures` flag stays consistent with explicit settings).
    pub fn is_ligature(&self) -> bool {
        matches!(&self.tag, b"liga" | b"clig" | b"calt" | b"dlig")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub font_family: String,
    /// Per-style family overrides (fall back to `font_family`).
    pub font_family_bold: Option<String>,
    pub font_family_italic: Option<String>,
    pub font_family_bold_italic: Option<String>,
    pub font_size: f32,
    pub theme_name: String,
    pub theme: Theme,
    pub scrollback: usize,
    pub padding_x: f32,
    pub padding_y: f32,
    pub background_opacity: f32,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub bell: BellMode,
    /// OSC 52 clipboard policy (default: writes only).
    pub osc52: Osc52,
    pub tab_bar: TabBarMode,
    pub tab_bar_pos: TabBarPos,
    /// Cycle 295: status-bar mode. See [`StatusBarMode`].
    pub status_bar: StatusBarMode,
    /// Cycle 332 (Terminator parity, terminatorlib/config.py:75
    /// `borderless`): hide OS window decorations. Useful for tiling
    /// window managers + Quake-style dropdown setups where the host
    /// chrome is redundant.
    pub borderless: bool,
    /// Cycle 332 (Terminator parity, terminatorlib/config.py:78
    /// `always_on_top`): keep the kettle window above other
    /// windows. Best-effort per OS (Wayland respects compositor
    /// rules; X11 + macOS + Windows mostly honor it).
    pub always_on_top: bool,
    /// Cycle 333 (Terminator parity, terminatorlib/config.py:111
    /// `allow_bold`): when false, suppress bold text rendering
    /// (everything renders plain regardless of SGR 1). Useful on
    /// monospace fonts that lack a bold companion.
    pub allow_bold: bool,
    /// Cycle 333 (Terminator parity, terminatorlib/config.py:130
    /// `bold_is_bright`): when true, SGR 1 (bold) for indices
    /// 0-7 maps to the bright variant (8-15). xterm convention.
    pub bold_is_bright: bool,
    /// Cycle 333 (Terminator parity, terminatorlib/config.py:120
    /// `link_single_click`): when true, single-click on a URL
    /// opens it (kettle default: Ctrl+click). PuTTY/iTerm2-style.
    pub link_single_click: bool,
    /// Cycle 333 (Terminator parity, terminatorlib/config.py:91
    /// `clear_select_on_copy`): when true, the selection is
    /// deselected after Copy (default: keep selected, so user can
    /// re-copy).
    pub clear_select_on_copy: bool,
    /// Cycle 334 (Terminator parity, terminatorlib/config.py:128
    /// `disable_mousewheel_zoom`): when true, Ctrl+wheel doesn't
    /// change font size (lets terminal-based scroll-wheel users
    /// avoid accidental zooms).
    pub disable_mousewheel_zoom: bool,
    /// Cycle 334 (Terminator parity, terminatorlib/config.py:88
    /// `disable_mouse_paste`): when true, middle-click paste is
    /// disabled. Useful for terminal-of-last-resort use cases
    /// where you don't want clipboard content to leak in via
    /// accidental middle-clicks.
    pub disable_mouse_paste: bool,
    /// Cycle 334 (Terminator parity, terminatorlib/config.py:89
    /// `putty_paste_style`): when true, right-click pastes (PuTTY/
    /// Windows convention). When false, right-click opens the
    /// context menu (kettle default + Linux convention).
    pub putty_paste_style: bool,
    /// Cycle 334 (Terminator parity, terminatorlib/config.py:90
    /// `smart_copy`): when true, Ctrl+Shift+C is a no-op when no
    /// selection exists (passes through to the shell). When false,
    /// the key is always consumed.
    pub smart_copy: bool,
    /// Cycle 335 (Terminator parity, terminatorlib/config.py:93
    /// `invert_search`): when true, scrollback search goes from
    /// the bottom up (newest matches first) instead of the default
    /// top-down (oldest first).
    pub invert_search: bool,
    /// Cycle 617 (Terminator parity, terminatorlib/config.py:117
    /// `case_sensitive`): scrollback-search case-sensitivity
    /// override. kettle's default is `smart` (ripgrep/vim:
    /// insensitive until the pattern has any uppercase). `always`
    /// forces sensitive (Terminator's default), `never` forces
    /// insensitive.
    pub search_case_sensitive: SearchCaseSensitivity,
    /// Cycle 335 (Terminator parity, terminatorlib/config.py:114
    /// `term`): TERM environment variable for spawned shells.
    /// Default `xterm-256color` matches kettle's pre-cycle-335
    /// hardcoded value + Terminator's own default.
    pub term: String,
    /// Cycle 335 (Terminator parity, terminatorlib/config.py:115
    /// `colorterm`): COLORTERM environment variable. Default
    /// `truecolor` signals 24-bit color support to programs that
    /// honor it (vim, nvim, tmux, ...).
    pub colorterm: String,
    /// Cycle 336 (Terminator parity, terminatorlib/config.py:122
    /// `login_shell`): when true, spawn the shell with `-l` (login
    /// shell semantics — reads /etc/profile, ~/.profile,
    /// ~/.bash_profile, ...). Default false matches Terminator.
    pub login_shell: bool,
    /// Cycle 336 (Terminator parity, terminatorlib/config.py:118
    /// `exit_action`): what to do when the shell process exits.
    pub exit_action: ExitAction,
    /// Cycle 336 (Terminator parity, terminatorlib/config.py:79
    /// `ask_before_closing`): when to show the close-confirmation
    /// dialog on window close.
    ///
    /// NOTE (cycle 563): parsed but currently no-op — kettle-ui
    /// doesn't consume this field yet. Users setting
    /// `ask-before-closing = always` see the same behavior as
    /// `never`. Field kept for forward-compat; a future cycle
    /// wiring the confirm-on-close dialog reads it here.
    pub ask_before_closing: AskBeforeClosing,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:81
    /// `close_button_on_tab`): show ✕ on tabs.
    pub close_button_on_tab: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:97
    /// `new_tab_after_current_tab`): insert new tab after the
    /// currently-active tab (vs at the end).
    pub new_tab_after_current_tab: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:95
    /// `title_at_bottom`): per-pane titlebar position. No-op until
    /// the per-pane titlebar Bucket-D lands; config accepted now.
    pub title_at_bottom: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:82
    /// `scroll_tabbar`): scrollable tab bar for many-tabs windows.
    pub scroll_tabbar: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:83
    /// `homogeneous_tabbar`): equal-width tabs.
    pub homogeneous_tabbar: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:77
    /// `hide_on_lose_focus`): hide window when it loses focus.
    /// Quake-style behavior. winit hint; partial OS support.
    pub hide_on_lose_focus: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:78
    /// `sticky`): show window on every workspace (X11 only;
    /// no-op on Wayland and most other platforms).
    pub sticky: bool,
    /// Cycle 337 (Terminator parity, terminatorlib/config.py:76
    /// `hide_from_taskbar`): hide kettle from the OS taskbar.
    /// Linux-specific (X11 + Wayland support varies).
    pub hide_from_taskbar: bool,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:107
    /// `backspace_binding`): how Backspace key is encoded.
    pub backspace_binding: BackspaceBinding,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:108
    /// `delete_binding`): how Delete key is encoded.
    pub delete_binding: DeleteBinding,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:71
    /// `broadcast_default`): default broadcast scope.
    pub broadcast_default: BroadcastDefault,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:86
    /// `use_custom_url_handler`): use an external program for
    /// URL clicks instead of the OS default.
    pub use_custom_url_handler: bool,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:87
    /// `custom_url_handler`): path to the custom URL handler.
    /// No-op unless `use_custom_url_handler` is true.
    pub custom_url_handler: String,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:84
    /// `inactive_color_offset`): float 0.0-1.0 — unfocused-pane
    /// FG color dimming. kettle's `unfocused-split-opacity`
    /// applies to the whole pane; this is a separate FG-only
    /// dim. No-op until wired into the render layer.
    pub inactive_color_offset: f32,
    /// Cycle 338 (Terminator parity, terminatorlib/config.py:85
    /// `inactive_bg_color_offset`): BG-only dim for unfocused
    /// panes. No-op until wired into the render layer.
    pub inactive_bg_color_offset: f32,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:99
    /// `split_to_group`): new splits inherit the parent's broadcast
    /// group.
    pub split_to_group: bool,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:100
    /// `autoclean_groups`): remove empty broadcast groups
    /// automatically.
    pub autoclean_groups: bool,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:80
    /// `always_split_with_profile`): new splits inherit the
    /// parent pane's profile.
    pub always_split_with_profile: bool,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:73
    /// `focus`): focus mode — click (default), sloppy (focus
    /// follows mouse), system (use the desktop's focus mode).
    ///
    /// NOTE (cycle 563): parsed but currently no-op — kettle-ui
    /// uses click-focus exclusively. Sloppy / system modes
    /// aren't wired yet. Field kept for forward-compat; a future
    /// cycle wiring focus-follows-mouse reads it here.
    pub focus: FocusMode,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:74
    /// `handle_size`): split-divider grab width in px. -1 means
    /// "use the GTK/winit theme default."
    pub handle_size: i32,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:75
    /// `window_state`): initial window state at launch.
    pub window_state: WindowState,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:75
    /// `geometry_hinting`): resize in font-step increments.
    pub geometry_hinting: bool,
    /// Cycle 339 (Terminator parity, terminatorlib/config.py:75
    /// `extra_styling`): load extra GTK CSS per-theme.
    /// kettle is wgpu+glyphon, not GTK — this is a no-op stub
    /// for config compatibility.
    pub extra_styling: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:103
    /// `force_no_bell`): suppress every bell flavor. Same as
    /// kettle's `bell = off` but as a separate bool flag.
    pub force_no_bell: bool,
    /// Cycle 625 (Terminator parity extension, `plugins/logger.py`):
    /// when true, the per-pane session log strips ANSI escape
    /// sequences (CSI / OSC / single-char ESC) before writing.
    /// Default false preserves cycle-621 raw-stream behavior
    /// (the log is `cat`-replayable in a terminal).
    pub log_strip_ansi: bool,
    /// Cycle 616 (Terminator parity, `plugins/auto_theme.py`):
    /// theme name to switch to on `Action::ToggleLightDark`
    /// when the current theme matches `dark_theme`. Empty
    /// string = unset (action is a no-op).
    pub light_theme: String,
    /// Cycle 616 (Terminator parity, `plugins/auto_theme.py`):
    /// theme name to switch to on `Action::ToggleLightDark`
    /// when the current theme matches `light_theme`. Empty
    /// string = unset (action is a no-op). If neither is
    /// set, the toggle keybind silently no-ops.
    pub dark_theme: String,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:101
    /// `icon_bell`): show the bell icon in the per-pane titlebar
    /// when a bell rings. No-op until the Bucket-D titlebar lands.
    pub icon_bell: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:96
    /// `show_titlebar`): show the per-pane titlebar widget. The
    /// titlebar itself is Bucket-D in docs/TERMINATOR-AUDIT.md.
    pub show_titlebar: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:131
    /// `title_hide_sizetext`): hide the WxH size annotation in
    /// the per-pane titlebar.
    pub title_hide_sizetext: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:142
    /// `title_use_system_font`): use the system font for the
    /// per-pane titlebar text.
    pub title_use_system_font: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:143
    /// `title_font`): explicit font (when title_use_system_font
    /// is false). Default `Sans 9`.
    pub title_font: String,
    /// Cycle 340 title-color triplets (terminatorlib/config.py:132-141).
    pub title_transmit_fg_color: Option<Rgb>,
    pub title_transmit_bg_color: Option<Rgb>,
    pub title_receive_fg_color: Option<Rgb>,
    pub title_receive_bg_color: Option<Rgb>,
    pub title_inactive_fg_color: Option<Rgb>,
    pub title_inactive_bg_color: Option<Rgb>,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:127
    /// `cursor_color_default`): when true, the cursor uses the
    /// theme's foreground color (default kettle behavior).
    pub cursor_color_default: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:117
    /// `use_system_font`): use the OS system font.
    pub use_system_font: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:116
    /// `use_theme_colors`): use OS theme colors.
    pub use_theme_colors: bool,
    /// Cycle 340 (Terminator parity, terminatorlib/config.py:144
    /// `http_proxy`): HTTP proxy URL for plugin HTTP requests.
    /// No-op until the plugin Bucket-D lands.
    pub http_proxy: String,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:118
    /// `background_type`): background style.
    pub background_type: BackgroundType,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:117
    /// `background_image`): path to background image. No-op
    /// until Bucket-D bg-image render lands.
    pub background_image: String,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:119
    /// `background_image_mode`): tiling mode.
    pub background_image_mode: String,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:120
    /// `background_image_align_horiz`): horizontal alignment.
    pub background_image_align_horiz: String,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:121
    /// `background_image_align_vert`): vertical alignment.
    pub background_image_align_vert: String,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:122
    /// `background_blur`): blur the background image.
    pub background_blur: bool,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:106
    /// `background_darkness`): background image opacity (0.0 fully
    /// dark .. 1.0 untinted).
    pub background_darkness: f32,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:93
    /// `cell_height`): vertical cell scaling (default 1.0).
    /// kettle's font metrics derive from glyph rendering; this
    /// is a no-op stub for config compatibility (Bucket E in
    /// audit doc — VTE-specific behavior).
    pub cell_height: f32,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:94
    /// `cell_width`): horizontal cell scaling. No-op stub.
    pub cell_width: f32,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:124
    /// `detachable_tabs`): allow dragging tabs between windows.
    /// No-op until Bucket-D detachable-tabs lands.
    pub detachable_tabs: bool,
    /// Cycle 341 (Terminator parity, terminatorlib/config.py:96
    /// `putty_paste_style_source_clipboard`): when `putty_paste_
    /// style` is true, also source from the system clipboard
    /// (not just the X11 primary).
    pub putty_paste_style_source_clipboard: bool,
    /// Cycle 376 (Terminator plugin parity, plugin sub-cycle 12):
    /// Lua sandbox level.
    pub lua_sandbox: LuaSandbox,
    /// Opacity of unfocused split panes (1.0 = no dim).
    pub unfocused_split_opacity: f32,
    /// Mouse-wheel scroll speed multiplier (1.0 = ~3 lines per notch).
    pub scroll_multiplier: f32,
    /// WCAG minimum contrast ratio (text vs background); `0.0` = off.
    pub minimum_contrast: f32,
    /// Template for the OS window title. Placeholders: `{title}` (the
    /// active pane's OSC title or "kettle"), `{cwd}` (active pane's cwd
    /// or empty), `{tab}` (1-based active tab index).
    pub window_title_format: String,
    /// Template for each tab segment in the tab bar. Placeholders:
    /// `{n}` (1-based tab index), `{title}` (focused pane's title).
    pub tab_format: String,
    pub scrollbar: ScrollbarMode,
    /// Explicit split-divider/border color for *inactive* panes (else
    /// theme `palette[8]`, the dim color).
    pub split_divider_color: Option<Rgb>,
    /// Explicit border color for the *focused* pane (else theme
    /// `palette[4]`, the blue accent). Lets users tune the
    /// here-am-I indicator without re-themeing the whole palette.
    pub focused_split_color: Option<Rgb>,
    /// Cycle 293 peacock parity. When set, this color overrides
    /// every "kettle accent" surface — active tab segment's accent
    /// strip, focused-pane border (unless `focused-split-color` is
    /// also set; that wins for backward-compat), the cycle-255
    /// dragged-tab ghost strip. Lets a user run multiple kettle
    /// windows (`--profile dev` + `--profile ops`) and tell them
    /// apart at a glance. `palette[4]` and `palette[3]` (broadcast
    /// warning) are *not* overridden — broadcast stays yellow so
    /// the high-priority state isn't confused with a workspace
    /// identity color.
    pub accent_color: Option<Rgb>,
    /// Cursor blink half-period in milliseconds.
    pub cursor_blink_interval: u64,
    /// Cycle 252: an inactive tab whose unseen output went quiet for
    /// at least this many milliseconds transitions from the
    /// `Output` indicator (cyan) to `Silent` (dim chrome). Default
    /// 10 s — long enough that a slow `cargo build` doesn't oscillate
    /// the indicator between strokes, short enough that a `tail -f`
    /// going quiet is noticed before the user switches tabs. Clamped
    /// `[1000, 600_000]` (1 s..10 min) at parse time.
    pub tab_silence_threshold_ms: u64,
    /// Cycle 612 (Terminator parity, `command_notify.py` plugin):
    /// minimum command duration in ms before kettle fires a desktop
    /// notification on OSC 133 D (CommandEnd). The notification fires
    /// only when the kettle window doesn't have focus at the moment
    /// the command finishes — so a foreground command you're
    /// watching doesn't pop a noise. `0` disables command-end
    /// notifications entirely. Default 5_000 (5 s) — long enough
    /// that quick `ls` / `cd` don't fire, short enough that a `make`
    /// you switched away from notifies promptly. Clamped at parse
    /// time to `[0, 86_400_000]` (0..1 day).
    pub command_notify_threshold_ms: u64,
    /// Auto-copy the selection to the clipboard on release.
    pub copy_on_select: bool,
    /// When the user types while scrolled back in scrollback, jump back to
    /// the bottom of the screen (Alacritty `scrolling.history.scroll_on_input`,
    /// xterm `scrollKey`). Default `true`.
    pub scroll_on_keystroke: bool,
    /// When new output arrives while the user is scrolled back, jump to the
    /// bottom of the screen (Alacritty `scrolling.history.scroll_on_output`,
    /// xterm `scrollTtyOutput`). Default `false` so reading old output isn't
    /// interrupted by a chatty background process.
    pub scroll_on_output: bool,
    /// Hide the OS mouse cursor while the user is typing; show it again on
    /// the next mouse movement. Defaults to `true` (matches Alacritty /
    /// kitty / WezTerm so the mouse pointer doesn't sit over the text
    /// you're editing). Disable to keep the cursor visible at all times.
    pub mouse_hide_while_typing: bool,
    /// Characters that delimit a "word" for double-click word selection
    /// (and the matching jump-to-prompt search). When empty, the engine
    /// default is used (Alacritty `selection.semantic_escape_chars`:
    /// `,│`|:\"' ()[]{}<>\t`). Set to e.g. ` "'` to make `/` part of a
    /// word so URLs/paths are picked up whole.
    pub word_delimiters: String,
    pub font_ligatures: bool,
    /// Explicit OpenType feature overrides (`font-feature`, repeatable),
    /// applied on top of the ligature toggle. Later entries win.
    pub font_features: Vec<FontFeature>,
    pub search_foreground: Rgb,
    pub search_background: Rgb,
    pub keybinds: Bindings,
    /// Shell override; `None` uses `$SHELL` / platform default.
    pub shell: Option<String>,
    /// Named SSH targets: `ssh-host = name=user@host` (repeatable).
    pub ssh_hosts: Vec<(String, String)>,
    /// Cycle 289 triggers (iTerm2 parity). Each entry is a regex
    /// pattern matched against PTY output; when it fires while the
    /// pane is unfocused, the action runs. Repeatable via
    /// `trigger = REGEX` config lines (default action: Urgency).
    /// Stored as strings; kettle-core compiles them to
    /// `regex::Regex` at pane-spawn time so a malformed regex on
    /// one trigger doesn't sink the whole config load — invalid
    /// patterns are logged via `log::warn!` and dropped.
    pub triggers: Vec<OutputTrigger>,
    /// Cycle 611 (Terminator parity, `terminatorlib/plugins/
    /// custom_commands.py` → "Custom Commands" menu). User-defined
    /// right-click menu entries: each `menu-item = LABEL = CMD`
    /// config line appends one row. Clicking the row writes
    /// `CMD\n` to the focused pane's PTY (same shape as
    /// `kettle.send_text` from cycle 325). Strictly additive on
    /// top of the built-in context menu items.
    ///
    /// Distinct from the cycle-375 `kettle.add_menu_item(label,
    /// callback)` Lua API: that one takes a callback for
    /// arbitrary Lua-side behavior; this one is config-file-only
    /// and sends literal text. Use this entry for plain
    /// "type this command" rows; use Lua for anything richer.
    pub menu_items: Vec<MenuItem>,
}

/// Cycle 611: a user-defined right-click menu entry from
/// `menu-item = LABEL = CMD`. The label is shown in the menu;
/// the command is sent as PTY input + `\n` when the row is
/// clicked. Plain text, no shell expansion or env substitution
/// at parse time — the user's shell does that when the
/// command arrives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    pub label: String,
    pub command: String,
}

/// Cycle 289: one configured output-trigger rule. Plain-string
/// `pattern` (compiled to `Regex` by kettle-core) + an action describing
/// what should happen when output matches.
///
/// Named `OutputTrigger` (not just `Trigger`) to disambiguate from the
/// existing `keybinds::Trigger`, which is a modifier+key combo — a
/// different concept entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTrigger {
    /// Rust-`regex`-syntax pattern, matched against PTY-printable
    /// bytes (escape sequences stripped). E.g.
    ///
    /// ```text
    /// trigger = error.*panic
    /// trigger = (BUILD SUCCESSFUL|FAILED)
    /// ```
    pub pattern: String,
    /// What kettle does on a match. v1 ships `Urgency` only — the
    /// window taskbar/dock entry pulses to alert the user. Future
    /// additions: `Bell`, `TabTitle(template)`, `Notify(text)`.
    pub action: TriggerAction,
}

/// Cycle 289 trigger action. One enum so the config parser can grow
/// new variants without rippling through every call site. v1 ships
/// the minimum:
///
/// - `Urgency` — `window.request_user_attention(Critical)`. The OS
///   handles the rest (Wayland: foot animation, GNOME notification
///   counter; X11: WM_HINTS urgency; macOS: dock bounce; Windows:
///   taskbar flash).

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TriggerAction {
    #[default]
    Urgency,
    /// Cycle 622 (Terminator parity, `plugins/run_cmd_on_match.py`):
    /// spawn an external program when the trigger pattern matches.
    /// Argv form (no shell expansion at kettle's layer) — security
    /// posture is "treat the configured command as data, not a
    /// shell string." Capture groups are NOT substituted in v1
    /// (a `$1`-substitution path is the natural next sub-cycle).
    ///
    /// The spawn is fire-and-forget: kettle does not wait for the
    /// child to exit, doesn't capture its stdout/stderr, and
    /// doesn't track its lifetime. Same shape as a shell `&`
    /// background launch. If the command can't be spawned (binary
    /// missing, perm denied) a warn is logged + the trigger is
    /// otherwise ignored.
    RunCommand(Vec<String>),
}

/// Cycle 622 helper: split a `trigger = REGEX :: cmd arg1 arg2`
/// value into `(pattern, argv)`. Returns `None` when there's no
/// `::` separator (caller treats the whole value as a plain
/// Urgency trigger, preserving cycle-289 behavior).
///
/// Argv is whitespace-split with no quote-escaping in v1 — kettle
/// doesn't try to mimic the shell's quoting rules. A user who
/// needs spaces in an arg should symlink the binary to a path
/// without spaces, or wait for the v2 quoted-arg syntax.
///
/// Pure: no env, no disk, no clock.
pub fn parse_trigger_with_command(value: &str) -> Option<(String, Vec<String>)> {
    let (pat, cmd) = value.split_once("::")?;
    let pattern = pat.trim().to_string();
    let argv: Vec<String> = cmd.split_whitespace().map(|s| s.to_string()).collect();
    if pattern.is_empty() || argv.is_empty() {
        return None;
    }
    Some((pattern, argv))
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_family: font::FAMILY.to_string(),
            font_family_bold: None,
            font_family_italic: None,
            font_family_bold_italic: None,
            font_size: 13.0,
            theme_name: "TokyoNight Night".to_string(),
            theme: Theme::by_name("TokyoNight Night"),
            scrollback: 10_000,
            padding_x: 8.0,
            padding_y: 8.0,
            background_opacity: 1.0,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            bell: BellMode::Both,
            osc52: Osc52::Copy,
            tab_bar: TabBarMode::Always,
            tab_bar_pos: TabBarPos::Top,
            status_bar: StatusBarMode::Off,
            borderless: false,
            always_on_top: false,
            allow_bold: true,
            bold_is_bright: false,
            link_single_click: false,
            clear_select_on_copy: false,
            disable_mousewheel_zoom: false,
            disable_mouse_paste: false,
            putty_paste_style: false,
            smart_copy: true,
            invert_search: false,
            search_case_sensitive: SearchCaseSensitivity::Smart,
            term: "xterm-256color".to_string(),
            colorterm: "truecolor".to_string(),
            login_shell: false,
            exit_action: ExitAction::Close,
            ask_before_closing: AskBeforeClosing::MultipleTerminals,
            close_button_on_tab: true,
            new_tab_after_current_tab: false,
            title_at_bottom: false,
            scroll_tabbar: false,
            homogeneous_tabbar: true,
            hide_on_lose_focus: false,
            sticky: false,
            hide_from_taskbar: false,
            backspace_binding: BackspaceBinding::AsciiDel,
            delete_binding: DeleteBinding::EscapeSequence,
            broadcast_default: BroadcastDefault::Group,
            use_custom_url_handler: false,
            custom_url_handler: String::new(),
            inactive_color_offset: 1.0,
            inactive_bg_color_offset: 1.0,
            split_to_group: false,
            autoclean_groups: true,
            always_split_with_profile: false,
            focus: FocusMode::Click,
            handle_size: -1,
            window_state: WindowState::Normal,
            geometry_hinting: false,
            extra_styling: true,
            force_no_bell: false,
            log_strip_ansi: false,
            light_theme: String::new(),
            dark_theme: String::new(),
            icon_bell: true,
            show_titlebar: true,
            title_hide_sizetext: false,
            title_use_system_font: true,
            title_font: "Sans 9".to_string(),
            title_transmit_fg_color: None,
            title_transmit_bg_color: None,
            title_receive_fg_color: None,
            title_receive_bg_color: None,
            title_inactive_fg_color: None,
            title_inactive_bg_color: None,
            cursor_color_default: true,
            use_system_font: true,
            use_theme_colors: false,
            http_proxy: String::new(),
            background_type: BackgroundType::Solid,
            background_image: String::new(),
            background_image_mode: "stretch_and_fill".to_string(),
            background_image_align_horiz: "center".to_string(),
            background_image_align_vert: "middle".to_string(),
            background_blur: false,
            background_darkness: 0.5,
            cell_height: 1.0,
            cell_width: 1.0,
            detachable_tabs: true,
            putty_paste_style_source_clipboard: false,
            lua_sandbox: LuaSandbox::Safe,
            unfocused_split_opacity: 0.7,
            scroll_multiplier: 1.0,
            minimum_contrast: 0.0,
            window_title_format: "{title} — kettle".to_string(),
            tab_format: "{n}: {title}".to_string(),
            scrollbar: ScrollbarMode::Auto,
            split_divider_color: None,
            focused_split_color: None,
            accent_color: None,
            cursor_blink_interval: 530,
            tab_silence_threshold_ms: 10_000,
            command_notify_threshold_ms: 5_000,
            copy_on_select: true,
            scroll_on_keystroke: true,
            scroll_on_output: false,
            mouse_hide_while_typing: true,
            word_delimiters: String::new(),
            font_ligatures: true,
            font_features: Vec::new(),
            search_foreground: Rgb::new(0x1a, 0x1b, 0x26),
            search_background: Rgb::new(0xe0, 0xaf, 0x68),
            keybinds: keybinds::defaults(),
            shell: None,
            ssh_hosts: Vec::new(),
            triggers: Vec::new(),
            menu_items: Vec::new(),
        }
    }
}

impl Config {
    /// Font family to use for a given style, falling back to `font_family`.
    pub fn family_for(&self, bold: bool, italic: bool) -> &str {
        let pick = match (bold, italic) {
            (true, true) => self.font_family_bold_italic.as_deref(),
            (true, false) => self.font_family_bold.as_deref(),
            (false, true) => self.font_family_italic.as_deref(),
            (false, false) => None,
        };
        pick.unwrap_or(&self.font_family)
    }

    /// Standard config path: `$XDG_CONFIG_HOME/kettle/config` (or the platform
    /// equivalent).
    pub fn default_path() -> Option<PathBuf> {
        Self::default_path_from(|k| std::env::var_os(k))
    }

    /// Cycle 292: resolve a named-profile config path. Returns
    /// `<config-dir>/profiles/<sanitized>.config`. Name is sanitized to
    /// `[A-Za-z0-9._-]` so a `--profile ../../etc/passwd` can't
    /// traverse out of the profiles directory. Returns `None` if the
    /// config dir isn't resolvable.
    ///
    /// Used by the kettle binary's `--profile NAME` flag: when set,
    /// kettle loads the named-profile config file instead of the
    /// default `<config-dir>/config`. Distinct from cycle-291's
    /// `--layout` which switches the *session* file (tab tree)
    /// while keeping the same config.
    pub fn path_for_profile(name: &str) -> Option<PathBuf> {
        let safe: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if safe.is_empty() {
            return None;
        }
        Self::default_path().and_then(|p| {
            p.parent()
                .map(|d| d.join("profiles").join(format!("{safe}.config")))
        })
    }

    /// Cycle 618 (Terminator parity, terminatorlib/terminator.py:
    /// `key_next_profile` / `key_previous_profile`): enumerate the
    /// available profile files in `<config-dir>/profiles/`, sorted
    /// ascii-then-bytewise so the cycle order is deterministic
    /// across runs. Returned names are the *bare* profile names
    /// (no `.config` extension and no parent dirs), so callers
    /// can round-trip via `path_for_profile`.
    ///
    /// Returns an empty Vec when:
    ///   - the config dir can't be located (CI / no $HOME)
    ///   - the `profiles/` subdir doesn't exist
    ///   - the directory exists but has no `*.config` files
    ///
    /// In all three cases, `Action::NextProfile` / `PrevProfile`
    /// will no-op rather than panic.
    pub fn list_profiles() -> Vec<String> {
        let Some(default_p) = Self::default_path() else {
            return Vec::new();
        };
        let Some(parent) = default_p.parent() else {
            return Vec::new();
        };
        let profiles_dir = parent.join("profiles");
        let Ok(rd) = std::fs::read_dir(&profiles_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if !p.is_file() {
                    return None;
                }
                let name = p.file_name()?.to_str()?;
                name.strip_suffix(".config").map(|s| s.to_string())
            })
            .collect();
        names.sort_by(|a, b| {
            a.to_lowercase()
                .cmp(&b.to_lowercase())
                .then_with(|| a.cmp(b))
        });
        names
    }

    /// Cycle 618. Companion to `path_for_profile`: extract the profile
    /// name from a config file path, if that path is shaped like one
    /// returned by `path_for_profile` (`<config-dir>/profiles/<name>.config`).
    /// Returns `None` for paths outside `profiles/` (e.g. the default
    /// `<config-dir>/config`, or a user-supplied --config FILE).
    pub fn profile_name_from_path(p: &std::path::Path) -> Option<String> {
        let parent_is_profiles =
            p.parent().and_then(|d| d.file_name()) == Some(std::ffi::OsStr::new("profiles"));
        if !parent_is_profiles {
            return None;
        }
        let stem = p.file_name()?.to_str()?.strip_suffix(".config")?;
        Some(stem.to_string())
    }

    /// Inner of `default_path` parameterized on the env-var lookup so
    /// the probe order + empty-value filter are unit-testable without
    /// mutating the real process env (which would race against the
    /// rest of the parallel suite).
    ///
    /// Empty env-var values are treated as unset and the probe
    /// continues to the next variable. Pre-cycle-181,
    /// `XDG_CONFIG_HOME=""` (rare but possible in stripped CI
    /// containers or after a misconfigured `unset`/`export X=`)
    /// returned `Some(PathBuf::from(""))` from the first arm, and
    /// the final path became `"kettle/config"` — a *relative* path
    /// that could pick up a stray `kettle/config` file in whatever
    /// directory the user launched kettle from. Same shape as
    /// cycle 180 (`home_dir_fallback`), applied here to the
    /// config-path probe.
    pub(crate) fn default_path_from(
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Option<PathBuf> {
        let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
        let base = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(|| var("APPDATA").map(PathBuf::from))?;
        Some(base.join("kettle").join("config"))
    }

    pub fn load() -> Config {
        match Self::default_path() {
            Some(p) if p.exists() => Self::load_from(&p),
            _ => Config::default(),
        }
    }

    pub fn load_from(path: &Path) -> Config {
        let (cfg, unknown, malformed) = Self::load_from_with_diagnostics(path);
        if !unknown.is_empty() {
            log::warn!(
                "{}: unrecognized config keys: {}",
                path.display(),
                unknown.join(", ")
            );
        }
        if !malformed.is_empty() {
            log::warn!(
                "{}: malformed values (ignored): {}",
                path.display(),
                malformed.join(", ")
            );
        }
        cfg
    }

    /// Parse the config at `path` and also return the unknown-keys and
    /// malformed-values diagnostics. `load_from` wraps this with a
    /// `log::warn!` for each; callers that want to render the diagnostics
    /// (e.g. a future in-window banner on reload, the existing
    /// `--check-config` flow) can use this directly. Missing file or read
    /// error → `(default(), [], [])`, same fallthrough as `load_from`,
    /// since the user already gets the error logged by `load_from`.
    ///
    /// Cycle 586: bound the read at 1 MiB. Real configs top out around 50
    /// KB (the bundled `docs/kettle.example.config` is 10 KB); 1 MiB is
    /// a ~20× margin over the bundled example and ~100× over typical
    /// user configs while staying small enough to detect a swap-attack
    /// blob before any allocation. Same defense-in-depth shape as cycle
    /// 585 (session.json) and cycle 584 (bg-image).
    pub fn load_from_with_diagnostics(path: &Path) -> (Config, Vec<String>, Vec<String>) {
        const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
        if let Ok(meta) = std::fs::metadata(path)
            && meta.len() > MAX_CONFIG_BYTES
        {
            log::warn!(
                "config file {} is {} bytes (cap {MAX_CONFIG_BYTES}); \
                 refusing to load — using defaults",
                path.display(),
                meta.len()
            );
            return (Config::default(), Vec::new(), Vec::new());
        }
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let (cfg, unknown) = Self::parse_collect(&text);
                let malformed = Self::detect_malformed_values(&text);
                (cfg, unknown, malformed)
            }
            Err(e) => {
                log::warn!("could not read config {}: {e}", path.display());
                (Config::default(), Vec::new(), Vec::new())
            }
        }
    }

    pub fn parse_text(text: &str) -> Config {
        Self::parse_collect(text).0
    }

    /// Scan a config text for keys whose value doesn't parse to the
    /// expected numeric / enum form — those would silently fall back to
    /// the default in `parse_collect`, leaving the user thinking their
    /// `font-size = 14px` (or `scrollback = lots`, etc.) took effect.
    /// Returns `"<key> = <value>"` strings; surfaced by `kettle
    /// --check-config` so the user sees the typo. Keeps the scan
    /// independent of the apply loop so adding a new validated key is
    /// one line here, not a touch on every parse arm.
    pub fn detect_malformed_values(text: &str) -> Vec<String> {
        let mut bad = Vec::new();
        // Strip the leading UTF-8 BOM (cycle 155 fixed it in
        // `parse::parse`; this function does its own raw scan so it
        // needs the same strip independently — otherwise a
        // BOM-prefixed config that's missing `=` on the first key
        // would surface `missing `=` separator: "\u{feff}theme"`
        // with the invisible BOM mangling the diagnostic).
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        // Tokenizer drops every non-`#`/non-empty line that lacks an `=`
        // (parse.rs:21). A typo like `font-family Jetbrains Mono` (missing
        // `=`) therefore disappears with no user-visible warning — same
        // shape as the value-typo bugs `detect_malformed_values` already
        // catches, but caught before parsing rather than after. Surface
        // the offending line verbatim so the user can see exactly which
        // one is wrong. Comment lines (`#`) and blanks are skipped using
        // the same rules `parse::parse` applies internally.
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if !line.contains('=') {
                bad.push(format!("missing `=` separator: {line:?}"));
            }
        }
        for e in parse::parse(text) {
            let v = &e.value;
            // Cycle 158: empty values are documented (parse.rs) as
            // "reset to default" semantics. Cycle 121/122 made the
            // string-keyed paths honor that explicitly; the bool /
            // enum / numeric arms all also fall through to defaults
            // on empty. Skip the per-key validity check for empty
            // values so the diagnostic doesn't disagree with the
            // runtime ("malformed value: theme = \"\"" while the
            // runtime quietly used the default → confusing).
            if v.trim().is_empty() {
                continue;
            }
            let ok = match e.key.as_str() {
                // Padding: parse-only (no fixed runtime clamp). Big
                // pads just shrink the rendered body area — the
                // cycle-119 `cap_axis_cells` keeps screenshots safe.
                "padding-x" | "window-padding-x" | "padding-y" | "window-padding-y" => {
                    v.parse::<f32>().is_ok()
                }
                // Numerics with a *runtime clamp* — parse AND land
                // inside the clamp range, otherwise the user's
                // `--check-config` value disagreed with what the
                // runtime actually used. Cycle 131 caught this for
                // `font-size`; cycle 132 extends to every other
                // clamped numeric so the diagnostic surface is
                // consistent. The runtime still clamps cleanly —
                // the warning just stops the silent mismatch.
                "font-size" => v.parse::<f32>().is_ok_and(|n| (5.0..=72.0).contains(&n)),
                "background-opacity" => v.parse::<f32>().is_ok_and(|n| (0.0..=1.0).contains(&n)),
                "unfocused-split-opacity" => {
                    v.parse::<f32>().is_ok_and(|n| (0.1..=1.0).contains(&n))
                }
                "scroll-multiplier" | "mouse-scroll-multiplier" => {
                    v.parse::<f32>().is_ok_and(|n| (0.1..=50.0).contains(&n))
                }
                "minimum-contrast" => v.parse::<f32>().is_ok_and(|n| (0.0..=21.0).contains(&n)),
                // Special: scrollback accepts unlimited/infinite/0 as
                // "no cap" plus any non-negative integer up to
                // `INFINITE_SCROLLBACK` (10 M lines). Values above
                // would have allocated >100 GB of history rows
                // (cycle 133 clamps them at parse, but flag the
                // diagnostic too).
                "scrollback" => {
                    v.eq_ignore_ascii_case("infinite")
                        || v.eq_ignore_ascii_case("unlimited")
                        || v.parse::<usize>().is_ok_and(|n| n <= INFINITE_SCROLLBACK)
                }
                // Same shape as the float-range checks above:
                // parse_collect clamps to [50, 5000] (cycle X), so
                // `cursor-blink-interval = 99999` silently becomes
                // 5000 — surface it now so the user's diagnostic
                // matches their runtime.
                "cursor-blink-interval" => v.parse::<u64>().is_ok_and(|n| (50..=5000).contains(&n)),
                // Color keys: `Rgb::parse` accepts `#RRGGBB`, `rgb:RR/GG/BB`,
                // X11 names ("red"), etc. Bad values otherwise silently
                // keep the default — same trap as the numeric keys.
                "background"
                | "foreground"
                | "cursor-color"
                | "selection-background"
                | "selection-foreground"
                | "search-foreground"
                | "search-background"
                | "split-divider-color"
                | "focused-split-color"
                | "split-divider-color-focused" => Rgb::parse(v).is_some(),
                // `keybind = <trigger>=<action>` — both halves have to
                // parse (same predicate `apply_keybind` uses, just split
                // so we know which half failed). A user typo on either
                // side silently drops the binding without this guard.
                // The action half also accepts the unbind sentinels
                // (`unbind`, `none`, `null`, `false`, empty) — those
                // mean "remove this default trigger", not "malformed".
                "keybind" => v.split_once('=').is_some_and(|(t, a)| {
                    let act = a.trim();
                    keybinds::parse_trigger(t.trim()).is_some()
                        && (keybinds::is_unbind_token(act) || Action::from_name(act).is_some())
                }),
                // `theme = …` falls back to TokyoNight Night silently on
                // an unknown name. Surface the typo so a user copying a
                // theme name from another terminal's config sees that
                // it's not in the bundled set (~512 themes including
                // every Ghostty default). Case-insensitive match matches
                // `Theme::by_name`'s resolution. (Empty value is
                // pre-filtered by cycle 158's global skip above.)
                "theme" => {
                    let want = v.trim().to_ascii_lowercase();
                    Theme::list().iter().any(|n| n.to_ascii_lowercase() == want)
                }
                // Enum-typed config values: each apply arm above has a
                // `_ => DefaultVariant` fallthrough, so a typo (`bell =
                // loud`, `cursor-style = wibble`, `scrollbar = sometimes`)
                // silently falls back to the default without any user-
                // visible warning. Pin the documented variants here so
                // `--check-config` flags unknown values; the list mirrors
                // the apply arms exactly.
                // `cursor-style = beam` is the Alacritty spelling for
                // the same vertical-bar cursor kettle calls `bar`.
                // Cycle 142 accepts it as an alias so a user copying
                // their Alacritty config doesn't get a silent
                // fallback to Block. Cycle 146: case-insensitive so
                // `Block` / `BLOCK` etc. also pass (matching the
                // parse_collect behavior).
                "cursor-style" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "block" | "underline" | "bar" | "beam"
                ),
                // Cycle 146: enum keys are case-insensitive so
                // `bell = OFF` validates the same as `bell = off`.
                // Mirrors the parse_collect change so the diagnostic
                // and runtime agree.
                "bell" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off" | "none" | "false" | "visual" | "flash" | "attention" | "urgent" | "both"
                ),
                "osc52" | "clipboard" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off"
                        | "none"
                        | "disabled"
                        | "false"
                        | "paste"
                        | "read"
                        | "both"
                        | "all"
                        | "true"
                        | "copy"
                ),
                // Boolean keys: accept the same alias set `parse_bool`
                // recognizes (cycle 138). Pre-cycle, any non-"false"
                // string silently meant "true", so typos like
                // `cursor-style-blink = no` quietly enabled the blink.
                // Surface unrecognized values now so the user sees
                // their typo in --check-config.
                "cursor-style-blink"
                | "copy-on-select"
                | "scroll-on-keystroke"
                | "scroll-on-input"
                | "scroll-on-output"
                | "mouse-hide-while-typing"
                | "mouse-hide" => parse_bool(v).is_some(),
                "tab-bar" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off" | "none" | "false" | "auto" | "always"
                ),
                "tab-bar-position" | "tab-position" | "tab_position" => {
                    // Cycle 331 + cycle 628 (Terminator parity,
                    // terminatorlib/config.py:144 `tab_position` accepts
                    // top/left/right/bottom/hidden). kettle accepts top +
                    // bottom natively, treats `hidden` as the well-known
                    // alias for `tab-bar = off` (the separate visibility
                    // key). `left`/`right` would require a vertical-tab-bar
                    // render-layer change (Bucket C in docs/TERMINATOR-AUDIT.md);
                    // accepted here so a config copied from Terminator doesn't
                    // fail --check-config, but the runtime falls through to
                    // top with a log::warn. Cycle 628 added the Terminator-
                    // spelled `tab-position` / `tab_position` aliases (kettle
                    // canonical is `tab-bar-position`).
                    matches!(
                        v.to_ascii_lowercase().as_str(),
                        "top" | "bottom" | "hidden" | "left" | "right"
                    )
                }
                "scrollbar" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "never" | "off" | "false" | "auto" | "always"
                ),
                // `font-feature` is comma-separated; every token must
                // parse via the documented `FontFeature::parse` shape
                // (`liga`, `+calt`, `cv01=2`, etc.). One bad token in
                // the list is enough to flag — that token's silently
                // dropped while the rest apply, leaving the user with
                // a half-applied feature set.
                "font-feature" => v.split(',').all(|tok| FontFeature::parse(tok).is_some()),
                // `ssh-host = name=user@host` — requires the `=`
                // separator; without it the entry is silently dropped
                // (no `name` to bind via the Ctrl+Shift+S launcher).
                "ssh-host" => v
                    .split_once('=')
                    .is_some_and(|(n, t)| !n.trim().is_empty() && !t.trim().is_empty()),
                // `palette = N=#hex` — both halves have to parse.
                // `palette = N=#hex`: both halves must parse, AND N
                // must fit the implementation's 0..=15 range. Cycle
                // 124: indices 16..=255 are documented as belonging
                // to the xterm 256-color extension but the runtime
                // apply path only writes `theme.palette[0..16]`. A
                // user writing `palette = 200=#ff0000` (expecting
                // their override to land on the bright-red 256-color
                // cube slot) silently saw no effect; this surfaces
                // the limit so they at least know the override was
                // ignored. The fix is one-sided (docs+diagnostic);
                // adding runtime support for 16..255 means a much
                // bigger Theme/renderer refactor.
                "palette" => v.split_once('=').is_some_and(|(i, h)| {
                    i.trim().parse::<usize>().is_ok_and(|n| n < 16)
                        && Rgb::parse(h.trim()).is_some()
                }),
                // Cycle 309: trigger patterns must be valid regex.
                // Without this check, a malformed pattern like
                // `trigger = [unclosed` parses (the config layer
                // stores it as a plain string), `--check-config`
                // reports OK, then at runtime `compile_triggers`
                // fails `Regex::new` and the trigger silently never
                // fires (only a log::warn that the user often
                // doesn't see). Now: surface the malformed regex at
                // check-config time so users see the issue before
                // an event they expected to fire never does.
                "trigger" => regex::Regex::new(v.trim()).is_ok(),
                "menu-item" | "menu_item" => v
                    .split_once('=')
                    .is_some_and(|(l, c)| !l.trim().is_empty() && !c.trim().is_empty()),
                _ => true,
            };
            if !ok {
                bad.push(format!("{} = {:?}", e.key, v));
            }
        }
        bad
    }

    /// Parse, also returning any unrecognized config keys (typo guard,
    /// surfaced by `kettle --check-config` and a startup `log::warn`).
    pub fn parse_collect(text: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut explicit_palette: Vec<(usize, Rgb)> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        // Cycle 619: Terminator splits bell into two orthogonal
        // bools (`visible_bell`, `urgent_bell`). Track them through
        // the parse loop and compose into `cfg.bell` at end-of-parse
        // so the result doesn't OR with kettle's default `Both`.
        let mut terminator_visible_bell: Option<bool> = None;
        let mut terminator_urgent_bell: Option<bool> = None;
        // Track whether the canonical `bell =` key was explicitly set
        // so that an explicit kettle-style mode wins over compat
        // aliases — same precedence rule kettle has elsewhere for
        // canonical key vs Terminator-spelled alias.
        let mut explicit_canonical_bell = false;
        for e in parse::parse(text) {
            match e.key.as_str() {
                // Empty `font-family =` (and the per-style variants)
                // silently emptied the family string, breaking the
                // renderer's `measure_cell` (cosmic-text falls back
                // to *some* font but the cell metrics drift and
                // glyphs render unpredictably). The parser docstring
                // already promised "empty value resets the key" —
                // honor that here by skipping the assignment so the
                // default (or a previous valid override on the same
                // key) stays in place. Same shape for the per-style
                // overrides.
                "font-family" => {
                    if !e.value.trim().is_empty() {
                        cfg.font_family = e.value.clone();
                    }
                }
                "font-family-bold" => {
                    if !e.value.trim().is_empty() {
                        cfg.font_family_bold = Some(e.value.clone());
                    } else {
                        cfg.font_family_bold = None;
                    }
                }
                "font-family-italic" => {
                    if !e.value.trim().is_empty() {
                        cfg.font_family_italic = Some(e.value.clone());
                    } else {
                        cfg.font_family_italic = None;
                    }
                }
                "font-family-bold-italic" => {
                    if !e.value.trim().is_empty() {
                        cfg.font_family_bold_italic = Some(e.value.clone());
                    } else {
                        cfg.font_family_bold_italic = None;
                    }
                }
                "font-size" => {
                    // Clamp at parse so `cfg.font_size` matches what the
                    // renderer will actually use (the cycle-118
                    // `clamp_font_size` is downstream). Without this,
                    // `--check-config` echoed e.g. `font: ... 500pt`
                    // while the runtime rendered at 72pt — confusing
                    // diagnostics. Cycle-131 surfaces out-of-range as
                    // a warning; cycle 139 makes the stored value
                    // match reality too. Parse-fail keeps the default.
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.font_size = v.clamp(5.0, 72.0);
                    }
                }
                "theme" => {
                    // Empty `theme =` same as the font-family case:
                    // keep the previously-resolved theme (default or
                    // an earlier override on the same key) rather
                    // than blanking the name string and falling back
                    // to whatever `Theme::by_name("")` returns.
                    //
                    // Unknown name (typo, copy-paste from another
                    // terminal's theme set, etc.): `Theme::by_name`
                    // silently falls back to TokyoNight Night. The
                    // cycle-176 fix keeps `cfg.theme_name` in sync
                    // with that fallback — store the *canonical*
                    // bundled name (with original casing) when found,
                    // and leave the previous value (the default
                    // "TokyoNight Night" on first hit) untouched when
                    // the lookup misses. Otherwise `--check-config`
                    // would echo `theme: TokyoNitght Night` while the
                    // runtime used a different palette — same shape
                    // as cycle 139 (font-size clamp matches runtime).
                    // The malformed-value diagnostic still flags the
                    // typo so the user sees their mistake.
                    if !e.value.trim().is_empty() {
                        cfg.theme = Theme::by_name(&e.value);
                        if let Some(canonical) = Theme::find_name(&e.value) {
                            cfg.theme_name = canonical.to_string();
                        }
                    }
                }
                "background" | "background-color" | "background_color" => {
                    // Cycle 623 (Terminator parity): kettle's canonical key
                    // is `background`; `background-color` + `background_color`
                    // are accepted as compatibility aliases so a Terminator
                    // config copies in without rename. Same for `foreground`
                    // and the cursor / fullscreen keys below.
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.theme.background = c;
                    }
                }
                "foreground" | "foreground-color" | "foreground_color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.theme.foreground = c;
                    }
                }
                "cursor-color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.theme.cursor = c;
                    }
                }
                "selection-background" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.theme.selection_background = c;
                    }
                }
                "selection-foreground" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.theme.selection_foreground = c;
                    }
                }
                "palette" => {
                    if let Some((i, h)) = e.value.split_once('=')
                        && let (Ok(i), Some(c)) = (i.trim().parse(), Rgb::parse(h.trim()))
                    {
                        explicit_palette.push((i, c));
                    }
                }
                "search-foreground" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.search_foreground = c;
                    }
                }
                "search-background" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.search_background = c;
                    }
                }
                "scrollback-limit" | "scrollback" => {
                    let v = e.value.trim().to_ascii_lowercase();
                    // `0` / `infinite` / `unlimited` => effectively unbounded
                    // history (capped high to keep memory bounded).
                    if v == "infinite" || v == "unlimited" || v == "0" {
                        cfg.scrollback = INFINITE_SCROLLBACK;
                    } else if let Ok(n) = v.parse::<usize>() {
                        // Clamp at `INFINITE_SCROLLBACK` (cycle 133): a
                        // user with `scrollback = 100000000` would have
                        // allocated ~250 GB of history rows on the
                        // first PTY. The docstring on the constant
                        // calls 10 M lines "practical stand-in for
                        // infinite"; any higher value is asking for an
                        // OOM. detect_malformed_values surfaces it as
                        // a diagnostic so the user sees the clamp.
                        cfg.scrollback = n.min(INFINITE_SCROLLBACK);
                    }
                }
                "window-padding-x" => {
                    if let Ok(v) = e.value.parse() {
                        cfg.padding_x = v;
                    }
                }
                "window-padding-y" => {
                    if let Ok(v) = e.value.parse() {
                        cfg.padding_y = v;
                    }
                }
                "background-opacity" => {
                    // Clamp at parse so out-of-range values can't reach
                    // wgpu's `Color { a: ... }` (alpha < 0 or > 1
                    // produces undefined visual artifacts on some
                    // backends). detect_malformed_values warns the
                    // user but we still want the runtime safe even if
                    // they ignore the warning. Matches the
                    // already-clamped siblings (`unfocused-split-
                    // opacity`, `scroll-multiplier`, `minimum-contrast`,
                    // `cursor-blink-interval`).
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.background_opacity = v.clamp(0.0, 1.0);
                    }
                }
                "cursor-style" | "cursor-shape" | "cursor_shape" => {
                    // Cycle 623 (Terminator parity): Terminator's
                    // `cursor_shape` is kettle's `cursor-style`. Same
                    // enum values (`block` / `underline` / `bar`).
                    cfg.cursor_style = match e.value.to_ascii_lowercase().as_str() {
                        "underline" => CursorStyle::Underline,
                        // `beam` is Alacritty's name for the same
                        // vertical-bar cursor; cycle 142 added the
                        // alias so Alacritty refugees don't get a
                        // silent Block fallback.
                        "bar" | "beam" | "ibeam" | "i-beam" => CursorStyle::Bar,
                        _ => CursorStyle::Block,
                    }
                }
                // Cycle 138: every boolean config key used `e.value !=
                // "false"`, which silently treated "no" / "off" / "0" /
                // "disabled" as `true` (because they're not the literal
                // string "false"). A user writing `cursor-style-blink =
                // no` expecting to disable the blink got blink ON
                // anyway. Route through the shared `parse_bool` helper
                // so all five bool keys (`cursor-style-blink`,
                // `copy-on-select`, `scroll-on-keystroke`,
                // `scroll-on-output`, `mouse-hide-while-typing`) accept
                // the standard true/false aliases. Bad values keep the
                // current value (no silent flip).
                "cursor-style-blink" | "cursor-blink" | "cursor_blink" => {
                    // Cycle 623 (Terminator parity, config.py:165
                    // `cursor_blink`): Terminator's bool maps to
                    // kettle's `cursor-style-blink`. Default true
                    // matches both.
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.cursor_blink = b;
                    }
                }
                "bell" => {
                    // Cycle 146: lowercase the value so `bell = OFF`
                    // matches `bell = off`. Pre-fix any non-lowercase
                    // spelling silently fell into the catchall (→
                    // BellMode::Both). Same shape applied to the four
                    // enum keys (bell / osc52 / tab-bar /
                    // tab-bar-position / scrollbar / cursor-style)
                    // and the bool parser already had it via
                    // `parse_bool`.
                    explicit_canonical_bell = true;
                    cfg.bell = match e.value.to_ascii_lowercase().as_str() {
                        "off" | "none" | "false" => BellMode::Off,
                        "visual" | "flash" => BellMode::Visual,
                        "attention" | "urgent" => BellMode::Attention,
                        _ => BellMode::Both,
                    }
                }
                "osc52" | "clipboard" => {
                    cfg.osc52 = match e.value.to_ascii_lowercase().as_str() {
                        "off" | "none" | "disabled" | "false" => Osc52::Off,
                        "paste" | "read" => Osc52::Paste,
                        "both" | "all" | "true" => Osc52::Both,
                        _ => Osc52::Copy,
                    }
                }
                "tab-bar" => {
                    cfg.tab_bar = match e.value.to_ascii_lowercase().as_str() {
                        "off" | "none" | "false" => TabBarMode::Off,
                        "auto" => TabBarMode::Auto,
                        _ => TabBarMode::Always,
                    }
                }
                "tab-bar-position" | "tab-position" | "tab_position" => {
                    // Cycle 331 + cycle 628 (Terminator parity,
                    // terminatorlib/config.py:144 `tab_position`). Terminator
                    // accepts top/left/right/bottom/hidden. kettle:
                    //   - `top` / `bottom`: native (cycle-X).
                    //   - `hidden`: alias to `tab-bar = off` (the kettle
                    //     visibility-vs-position split — different keys).
                    //   - `left` / `right`: vertical tab bars require a
                    //     render-layer change (Bucket C in audit doc).
                    //     Accept the value so --check-config doesn't flag it
                    //     as malformed on a copied Terminator config, but
                    //     fall through to top + log::warn so the user knows
                    //     it didn't take effect.
                    let lowered = e.value.to_ascii_lowercase();
                    match lowered.as_str() {
                        "bottom" => cfg.tab_bar_pos = TabBarPos::Bottom,
                        "hidden" => cfg.tab_bar = TabBarMode::Off,
                        "left" | "right" => {
                            log::warn!(
                                "tab-bar-position = {lowered} requested but vertical \
                                 tab bars aren't yet implemented; falling through to top \
                                 (see docs/TERMINATOR-AUDIT.md Bucket C)"
                            );
                            cfg.tab_bar_pos = TabBarPos::Top;
                        }
                        _ => cfg.tab_bar_pos = TabBarPos::Top,
                    }
                }
                "status-bar" | "statusbar" => {
                    cfg.status_bar = match e.value.to_ascii_lowercase().as_str() {
                        "off" | "false" | "none" => StatusBarMode::Off,
                        "top" => StatusBarMode::Top,
                        "bottom" | "true" | "on" => StatusBarMode::Bottom,
                        _ => StatusBarMode::Off,
                    }
                }
                "borderless" => {
                    // Cycle 332 (Terminator parity).
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.borderless = b;
                    }
                }
                "always-on-top" | "always_on_top" => {
                    // Cycle 332 (Terminator parity).
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.always_on_top = b;
                    }
                }
                "allow-bold" | "allow_bold" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.allow_bold = b;
                    }
                }
                "bold-is-bright" | "bold_is_bright" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.bold_is_bright = b;
                    }
                }
                "link-single-click" | "link_single_click" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.link_single_click = b;
                    }
                }
                "clear-select-on-copy" | "clear_select_on_copy" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.clear_select_on_copy = b;
                    }
                }
                "disable-mousewheel-zoom" | "disable_mousewheel_zoom" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.disable_mousewheel_zoom = b;
                    }
                }
                "disable-mouse-paste" | "disable_mouse_paste" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.disable_mouse_paste = b;
                    }
                }
                "putty-paste-style" | "putty_paste_style" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.putty_paste_style = b;
                    }
                }
                "smart-copy" | "smart_copy" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.smart_copy = b;
                    }
                }
                "invert-search" | "invert_search" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.invert_search = b;
                    }
                }
                "search-case-sensitive"
                | "search_case_sensitive"
                | "case-sensitive"
                | "case_sensitive" => {
                    // Accept the three named modes, plus the Terminator
                    // bool form (true ⇒ Always, false ⇒ Never) since
                    // `case_sensitive = True` is the Terminator config.py
                    // default.
                    let v = e.value.trim().to_ascii_lowercase();
                    cfg.search_case_sensitive = match v.as_str() {
                        "smart" | "auto" => SearchCaseSensitivity::Smart,
                        "always" | "sensitive" => SearchCaseSensitivity::Always,
                        "never" | "insensitive" => SearchCaseSensitivity::Never,
                        _ => match parse_bool(&e.value) {
                            Some(true) => SearchCaseSensitivity::Always,
                            Some(false) => SearchCaseSensitivity::Never,
                            None => cfg.search_case_sensitive, // keep current; unknown value
                        },
                    };
                }
                "term" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.term = v.to_string();
                    }
                }
                "colorterm" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.colorterm = v.to_string();
                    }
                }
                "login-shell" | "login_shell" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.login_shell = b;
                    }
                }
                "exit-action" | "exit_action" => {
                    cfg.exit_action = match e.value.to_ascii_lowercase().as_str() {
                        "restart" => ExitAction::Restart,
                        "hold" => ExitAction::Hold,
                        _ => ExitAction::Close,
                    };
                }
                "ask-before-closing" | "ask_before_closing" => {
                    cfg.ask_before_closing = match e.value.to_ascii_lowercase().as_str() {
                        "always" => AskBeforeClosing::Always,
                        "never" => AskBeforeClosing::Never,
                        _ => AskBeforeClosing::MultipleTerminals,
                    };
                }
                "close-button-on-tab" | "close_button_on_tab" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.close_button_on_tab = b;
                    }
                }
                "new-tab-after-current-tab" | "new_tab_after_current_tab" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.new_tab_after_current_tab = b;
                    }
                }
                "title-at-bottom" | "title_at_bottom" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.title_at_bottom = b;
                    }
                }
                "scroll-tabbar" | "scroll_tabbar" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.scroll_tabbar = b;
                    }
                }
                "homogeneous-tabbar" | "homogeneous_tabbar" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.homogeneous_tabbar = b;
                    }
                }
                "hide-on-lose-focus" | "hide_on_lose_focus" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.hide_on_lose_focus = b;
                    }
                }
                "sticky" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.sticky = b;
                    }
                }
                "hide-from-taskbar" | "hide_from_taskbar" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.hide_from_taskbar = b;
                    }
                }
                "backspace-binding" | "backspace_binding" => {
                    cfg.backspace_binding = match e.value.to_ascii_lowercase().as_str() {
                        "control-h" | "ctrl-h" | "control_h" => BackspaceBinding::ControlH,
                        "escape-sequence" | "escape_sequence" => BackspaceBinding::EscapeSequence,
                        "automatic" | "auto" => BackspaceBinding::Automatic,
                        _ => BackspaceBinding::AsciiDel,
                    };
                }
                "delete-binding" | "delete_binding" => {
                    cfg.delete_binding = match e.value.to_ascii_lowercase().as_str() {
                        "ascii-del" | "ascii_del" => DeleteBinding::AsciiDel,
                        "control-h" | "ctrl-h" | "control_h" => DeleteBinding::ControlH,
                        "automatic" | "auto" => DeleteBinding::Automatic,
                        _ => DeleteBinding::EscapeSequence,
                    };
                }
                "broadcast-default" | "broadcast_default" => {
                    cfg.broadcast_default = match e.value.to_ascii_lowercase().as_str() {
                        "all" => BroadcastDefault::All,
                        "off" | "none" => BroadcastDefault::Off,
                        _ => BroadcastDefault::Group,
                    };
                }
                "use-custom-url-handler" | "use_custom_url_handler" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.use_custom_url_handler = b;
                    }
                }
                "custom-url-handler" | "custom_url_handler" => {
                    cfg.custom_url_handler = e.value.trim().to_string();
                }
                "inactive-color-offset" | "inactive_color_offset" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.inactive_color_offset = v.clamp(0.0, 1.0);
                    }
                }
                "inactive-bg-color-offset" | "inactive_bg_color_offset" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.inactive_bg_color_offset = v.clamp(0.0, 1.0);
                    }
                }
                "split-to-group" | "split_to_group" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.split_to_group = b;
                    }
                }
                "autoclean-groups" | "autoclean_groups" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.autoclean_groups = b;
                    }
                }
                "always-split-with-profile" | "always_split_with_profile" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.always_split_with_profile = b;
                    }
                }
                "focus" => {
                    cfg.focus = match e.value.to_ascii_lowercase().as_str() {
                        "sloppy" => FocusMode::Sloppy,
                        "system" => FocusMode::System,
                        _ => FocusMode::Click,
                    };
                }
                "handle-size" | "handle_size" => {
                    if let Ok(v) = e.value.parse::<i32>() {
                        cfg.handle_size = v.clamp(-1, 50);
                    }
                }
                "window-state" | "window_state" => {
                    cfg.window_state = match e.value.to_ascii_lowercase().as_str() {
                        "maximise" | "maximize" => WindowState::Maximise,
                        "fullscreen" => WindowState::Fullscreen,
                        "hidden" => WindowState::Hidden,
                        _ => WindowState::Normal,
                    };
                }
                "full-screen" | "full_screen" => {
                    // Cycle 623 (Terminator parity, config.py:159
                    // `full_screen`): Terminator splits "should start
                    // fullscreen" into its own bool while kettle uses
                    // `window-state = fullscreen`. Compat alias: when
                    // `full_screen = true`, override window_state to
                    // Fullscreen. `false` is a no-op (doesn't override
                    // a separately-set window-state).
                    if let Some(true) = parse_bool(&e.value) {
                        cfg.window_state = WindowState::Fullscreen;
                    }
                }
                "geometry-hinting" | "geometry_hinting" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.geometry_hinting = b;
                    }
                }
                "extra-styling" | "extra_styling" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.extra_styling = b;
                    }
                }
                "force-no-bell" | "force_no_bell" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.force_no_bell = b;
                    }
                }
                "log-strip-ansi" | "log_strip_ansi" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.log_strip_ansi = b;
                    }
                }
                "audible-bell" | "audible_bell" => {
                    // Cycle 626 (Terminator parity, config.py:214
                    // `audible_bell`): kettle ships no audio bell
                    // surface yet (visual + window-attention only),
                    // so this key parses but is otherwise a Bucket E
                    // documented no-op. Accepting it keeps a
                    // Terminator config file copy-clean (no
                    // unknown-key warning at --check-config time).
                    // If a user wants the bell to fire, they should
                    // set `bell = attention` / `bell = visual` or
                    // the cycle-619 `urgent_bell` / `visible_bell`
                    // compat aliases.
                    let _ = parse_bool(&e.value);
                }
                "visible-bell" | "visible_bell" => {
                    // Cycle 619 (Terminator parity, config.py:215).
                    // Terminator splits bell into two orthogonal
                    // bools while kettle uses a unified enum; track
                    // the Terminator pair separately and compose at
                    // end-of-parse so the result doesn't depend on
                    // kettle's default `bell = Both`.
                    if let Some(b) = parse_bool(&e.value) {
                        terminator_visible_bell = Some(b);
                    }
                }
                "urgent-bell" | "urgent_bell" => {
                    // Cycle 619 (Terminator parity, config.py:216).
                    if let Some(b) = parse_bool(&e.value) {
                        terminator_urgent_bell = Some(b);
                    }
                }
                "light-theme" | "light_theme" => {
                    if let Some(canonical) = Theme::find_name(&e.value) {
                        cfg.light_theme = canonical.to_string();
                    } else if !e.value.trim().is_empty() {
                        cfg.light_theme = e.value.trim().to_string();
                    }
                }
                "dark-theme" | "dark_theme" => {
                    if let Some(canonical) = Theme::find_name(&e.value) {
                        cfg.dark_theme = canonical.to_string();
                    } else if !e.value.trim().is_empty() {
                        cfg.dark_theme = e.value.trim().to_string();
                    }
                }
                "icon-bell" | "icon_bell" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.icon_bell = b;
                    }
                }
                "show-titlebar" | "show_titlebar" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.show_titlebar = b;
                    }
                }
                "title-hide-sizetext" | "title_hide_sizetext" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.title_hide_sizetext = b;
                    }
                }
                "title-use-system-font" | "title_use_system_font" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.title_use_system_font = b;
                    }
                }
                "title-font" | "title_font" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.title_font = v.to_string();
                    }
                }
                "title-transmit-fg-color" | "title_transmit_fg_color" => {
                    cfg.title_transmit_fg_color = Rgb::parse(&e.value);
                }
                "title-transmit-bg-color" | "title_transmit_bg_color" => {
                    cfg.title_transmit_bg_color = Rgb::parse(&e.value);
                }
                "title-receive-fg-color" | "title_receive_fg_color" => {
                    cfg.title_receive_fg_color = Rgb::parse(&e.value);
                }
                "title-receive-bg-color" | "title_receive_bg_color" => {
                    cfg.title_receive_bg_color = Rgb::parse(&e.value);
                }
                "title-inactive-fg-color" | "title_inactive_fg_color" => {
                    cfg.title_inactive_fg_color = Rgb::parse(&e.value);
                }
                "title-inactive-bg-color" | "title_inactive_bg_color" => {
                    cfg.title_inactive_bg_color = Rgb::parse(&e.value);
                }
                "cursor-color-default" | "cursor_color_default" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.cursor_color_default = b;
                    }
                }
                "use-system-font" | "use_system_font" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.use_system_font = b;
                    }
                }
                "use-theme-colors" | "use_theme_colors" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.use_theme_colors = b;
                    }
                }
                "http-proxy" | "http_proxy" => {
                    cfg.http_proxy = e.value.trim().to_string();
                }
                "background-type" | "background_type" => {
                    cfg.background_type = match e.value.to_ascii_lowercase().as_str() {
                        "image" => BackgroundType::Image,
                        "transparent" => BackgroundType::Transparent,
                        _ => BackgroundType::Solid,
                    };
                }
                "background-image" | "background_image" => {
                    cfg.background_image = e.value.trim().to_string();
                }
                "background-image-mode" | "background_image_mode" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.background_image_mode = v.to_string();
                    }
                }
                "background-image-align-horiz" | "background_image_align_horiz" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.background_image_align_horiz = v.to_string();
                    }
                }
                "background-image-align-vert" | "background_image_align_vert" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.background_image_align_vert = v.to_string();
                    }
                }
                "background-blur" | "background_blur" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.background_blur = b;
                    }
                }
                "background-darkness" | "background_darkness" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.background_darkness = v.clamp(0.0, 1.0);
                    }
                }
                "cell-height" | "cell_height" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.cell_height = v.clamp(0.5, 3.0);
                    }
                }
                "cell-width" | "cell_width" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.cell_width = v.clamp(0.5, 3.0);
                    }
                }
                "detachable-tabs" | "detachable_tabs" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.detachable_tabs = b;
                    }
                }
                "putty-paste-style-source-clipboard" | "putty_paste_style_source_clipboard" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.putty_paste_style_source_clipboard = b;
                    }
                }
                "lua-sandbox" | "lua_sandbox" => {
                    cfg.lua_sandbox = match e.value.to_ascii_lowercase().as_str() {
                        "trusted" | "unsafe" => LuaSandbox::Trusted,
                        _ => LuaSandbox::Safe,
                    };
                }
                "unfocused-split-opacity" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.unfocused_split_opacity = v.clamp(0.1, 1.0);
                    }
                }
                "scroll-multiplier" | "mouse-scroll-multiplier" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.scroll_multiplier = v.clamp(0.1, 50.0);
                    }
                }
                "minimum-contrast" => {
                    if let Ok(v) = e.value.parse::<f32>() {
                        cfg.minimum_contrast = v.clamp(0.0, 21.0);
                    }
                }
                "window-title-format" | "title-format" => {
                    if !e.value.trim().is_empty() {
                        cfg.window_title_format = e.value.clone();
                    }
                }
                "tab-format" | "tab-title-format" => {
                    if !e.value.trim().is_empty() {
                        cfg.tab_format = e.value.clone();
                    }
                }
                "scrollbar" => {
                    cfg.scrollbar = match e.value.to_ascii_lowercase().as_str() {
                        "never" | "off" | "false" => ScrollbarMode::Never,
                        "always" => ScrollbarMode::Always,
                        _ => ScrollbarMode::Auto,
                    }
                }
                "split-divider-color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.split_divider_color = Some(c);
                    }
                }
                "focused-split-color" | "split-divider-color-focused" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.focused_split_color = Some(c);
                    }
                }
                "accent-color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.accent_color = Some(c);
                    }
                }
                "cursor-blink-interval" => {
                    if let Ok(v) = e.value.parse::<u64>() {
                        cfg.cursor_blink_interval = v.clamp(50, 5000);
                    }
                }
                "tab-silence-threshold-ms" | "tab-silence-threshold" => {
                    if let Ok(v) = e.value.parse::<u64>() {
                        cfg.tab_silence_threshold_ms = v.clamp(1_000, 600_000);
                    }
                }
                "command-notify-threshold-ms"
                | "command-notify-threshold"
                | "command_notify_threshold_ms"
                | "command_notify_threshold" => {
                    if let Ok(v) = e.value.parse::<u64>() {
                        // 0 = disable; positive clamps to 1 day.
                        cfg.command_notify_threshold_ms = v.min(86_400_000);
                    }
                }
                "copy-on-select" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.copy_on_select = b;
                    }
                }
                "scroll-on-keystroke" | "scroll-on-input" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.scroll_on_keystroke = b;
                    }
                }
                "scroll-on-output" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.scroll_on_output = b;
                    }
                }
                "mouse-hide-while-typing" | "mouse-hide" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.mouse_hide_while_typing = b;
                    }
                }
                "word-delimiters" | "selection-word-chars" | "semantic-escape-chars" => {
                    cfg.word_delimiters = e.value.clone();
                }
                "font-feature" => {
                    for tok in e.value.split(',') {
                        if let Some(f) = FontFeature::parse(tok) {
                            if f.is_ligature() {
                                cfg.font_ligatures = f.value != 0;
                            }
                            cfg.font_features.push(f);
                        }
                    }
                }
                "command" | "shell" => {
                    // Empty `command =` is "reset to default" (same
                    // shape as cycle 121's font-family fix). Without
                    // this, `Some("")` slips through to `shell_argv`
                    // which would spawn an empty program name and
                    // either fail with an unclear error or — worse —
                    // leave the user with no shell at all.
                    cfg.shell = if e.value.trim().is_empty() {
                        None
                    } else {
                        Some(e.value.clone())
                    };
                }
                "ssh-host" => {
                    // Filter empty name / target halves at parse time
                    // so they don't sneak into the runtime list and
                    // surface as an empty launcher row or a connection
                    // to "". `--check-config` already FLAGS these
                    // (detect_malformed_values, cycle 88), but the
                    // bad entries were still being pushed — the
                    // diagnostic and the runtime state disagreed.
                    if let Some((name, target)) = e.value.split_once('=') {
                        let (n, t) = (name.trim(), target.trim());
                        if !n.is_empty() && !t.is_empty() {
                            cfg.ssh_hosts.push((n.to_string(), t.to_string()));
                        }
                    }
                }
                "trigger" => {
                    // Cycle 289 base: `trigger = REGEX` fires Urgency.
                    // Cycle 622 (Terminator parity, run_cmd_on_match.py):
                    // `trigger = REGEX :: cmd arg1 arg2` extends the
                    // syntax with a `::` separator (two colons —
                    // chosen over `|` because pipe is a regex
                    // metacharacter, and over `:` because IPv6
                    // patterns would split mid-address). The RHS
                    // is whitespace-split into an argv (no shell
                    // expansion at kettle's layer); spawned
                    // fire-and-forget when the pattern matches.
                    let raw = e.value.trim();
                    if !raw.is_empty() {
                        if let Some((pat, cmd)) = parse_trigger_with_command(raw) {
                            cfg.triggers.push(OutputTrigger {
                                pattern: pat,
                                action: TriggerAction::RunCommand(cmd),
                            });
                        } else {
                            cfg.triggers.push(OutputTrigger {
                                pattern: raw.to_string(),
                                action: TriggerAction::Urgency,
                            });
                        }
                    }
                }
                "menu-item" | "menu_item" => {
                    // Cycle 611 (Terminator parity, custom_commands.py):
                    // `menu-item = LABEL = CMD` appends a right-click
                    // menu entry that writes `CMD\n` to the focused
                    // PTY on click. The cycle-X line-parser already
                    // consumed the first `=` to split key/value, so
                    // here we split `e.value` on the FIRST `=` again
                    // to get label vs command. A label with no `=`
                    // is rejected (parser arm logs nothing — the
                    // line just doesn't materialize a row).
                    if let Some((label, cmd)) = e.value.split_once('=') {
                        let label = label.trim();
                        let cmd = cmd.trim();
                        if !label.is_empty() && !cmd.is_empty() {
                            cfg.menu_items.push(MenuItem {
                                label: label.to_string(),
                                command: cmd.to_string(),
                            });
                        }
                    }
                }
                "keybind" => keybinds::apply_keybind(&mut cfg.keybinds, &e.value),
                other => unknown.push(other.to_string()),
            }
        }
        for (i, c) in explicit_palette {
            if i < 16 {
                cfg.theme.palette[i] = c;
            }
        }
        // Cycle 619 (Terminator parity, config.py:215-216):
        // compose `visible_bell` + `urgent_bell` into kettle's
        // unified `BellMode` when EITHER appears in the config
        // AND the canonical `bell =` was NOT explicitly set
        // (canonical kettle key wins on precedence). Default
        // values (None) compose to Off as a base, so
        // `visible_bell = true` alone yields Visual — matching
        // Terminator's two-bool semantics where the unset bool
        // is False.
        if !explicit_canonical_bell
            && (terminator_visible_bell.is_some() || terminator_urgent_bell.is_some())
        {
            let v = if terminator_visible_bell.unwrap_or(false) {
                BellMode::Visual
            } else {
                BellMode::Off
            };
            let u = if terminator_urgent_bell.unwrap_or(false) {
                BellMode::Attention
            } else {
                BellMode::Off
            };
            cfg.bell = v.compose(u);
        }
        // Cycle 613 (Terminator parity, terminatorlib/config.py
        // `force_no_bell`): post-process override. When
        // `force_no_bell = true`, force the bell mode to Off
        // regardless of the `bell` config key. Equivalent to
        // setting `bell = off` but uses Terminator's own key
        // name — copying a Terminator config that sets
        // `force_no_bell = True` now actually silences the
        // bell instead of the previous behavior (parsed but
        // never read).
        if cfg.force_no_bell {
            cfg.bell = BellMode::Off;
        }
        unknown.sort();
        unknown.dedup();
        (cfg, unknown)
    }
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn example_config_in_docs_uncommented_parses_with_zero_diagnostics() {
        // Cycle-100: docs/kettle.example.config used to document 9 of the
        // ~35 settable keys. After the expansion it documents every key
        // the parser knows about; this test catches docs drift by
        // strip-commenting every `# key = value` line in the example and
        // running it through the same diagnostic pipeline `kettle
        // --check-config` uses. If a new key lands but the example
        // doesn't add it, or if a typo creeps in, this test fails.
        //
        // Strategy: take each line, drop a leading `# ` if present, keep
        // anything that looks like a `key = value`, drop everything else
        // (section headers, pure prose). Then parse_collect + detect_
        // malformed_values both come back empty.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/kettle.example.config");
        let text = std::fs::read_to_string(&path).expect("example config exists");
        let activated: String = text
            .lines()
            .filter_map(|raw| {
                let l = raw.trim_start();
                let stripped = l.strip_prefix("# ").or_else(|| l.strip_prefix("#"))?;
                // After uncommenting, only keep lines that look like real
                // `key = value` rows (drop section headers like `─── Fonts ───`).
                let s = stripped.trim();
                if s.is_empty() {
                    return None;
                }
                // A real config line has lowercase-alnum-dash key, `=`, value.
                let eq = s.find('=')?;
                let key = s[..eq].trim();
                if key.is_empty()
                    || !key
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                {
                    return None;
                }
                Some(s.to_string())
            })
            .collect::<Vec<_>>()
            .join("\n");
        // Sanity: we extracted *something*. If this hits zero, the regex
        // logic broke and the rest of the test would pass vacuously.
        assert!(
            activated.lines().count() >= 20,
            "expected the example to document at least 20 keys; got\n{activated}"
        );
        let (_cfg, unknown) = Config::parse_collect(&activated);
        let malformed = Config::detect_malformed_values(&activated);
        assert!(
            unknown.is_empty(),
            "example config has unknown keys (docs drift): {unknown:?}"
        );
        assert!(
            malformed.is_empty(),
            "example config has malformed values (docs drift): {malformed:?}"
        );
    }

    #[test]
    fn user_facing_docs_have_no_internal_cycle_refs() {
        // Cycle 168 caught the audit-trail-in-doc-string issue on the
        // clap CLI surface (`kettle --help` was emitting "(cycle 103)"
        // / "(cycle 106)" parentheticals — mysterious to end users).
        // Cycle 172 extends the drift guard to the user-facing markdown
        // docs the README links to: CONFIG.md (config reference) and
        // INSTALL.md (per-OS install + from-source). README itself
        // mentions the word "cycle" in legitimate prose ("cycle the
        // themes at runtime", "the audit-cycle pattern"), so the
        // check has to be tighter than "contains 'cycle '" — match
        // the internal-ref shape `cycle <digit>` instead.
        //
        // TESTING.md and ROADMAP.md are intentionally exempt — they're
        // contributor-leaning docs where cycle refs serve as anchors
        // to specific CHANGELOG entries, the same way they do in code
        // comments. CONTRIBUTING.md documents the cycle-N pattern
        // itself, so a literal reference there is part of the content.
        //
        // Cycle 179 also flags hardcoded `<N> workspace tests` /
        // `<N> tests across` claims — these go stale every cycle
        // (TESTING.md / ARCHITECTURE.md / INSTALL.md each had one
        // that was off by 30-120 tests at the time of the audit).
        // Range-stable phrasings ("230+ tests", "an extensive
        // suite") don't drift; this guard fails the next time a
        // contributor hardcodes a count.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.join("../..");
        for rel in [
            "README.md",
            "docs/CONFIG.md",
            "docs/INSTALL.md",
            // Cycle 474: the example config is user-facing through
            // `kettle --print-default-config > ~/.config/kettle/config`.
            // Cycle refs inside it would leak into every user's
            // bootstrap file — same drift-guard reasoning as the
            // markdown docs above.
            "docs/kettle.example.config",
            // Cycle 475: the man page is user-facing via `man kettle`
            // (cycle 282 + cycle 414 + cycle 436 land entries). Same
            // reasoning — internal cycle refs leak into user-visible
            // documentation.
            "packaging/linux/kettle.1",
            // Cycle 596: SECURITY.md is user-facing via GitHub's
            // /security tab + the repo root listing. It already uses
            // the hyphenated `cycle-NNN` form (per cycles 583 + 588's
            // resource-cap documentation pass), which passes the
            // space-digit scan below — adding the doc to the scan
            // list makes future drift explicit. Past contributors
            // shouldn't have to remember "SECURITY.md is user-facing,
            // don't write `cycle 583` there".
            "SECURITY.md",
        ] {
            let path = repo_root.join(rel);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("missing user-facing doc {}: {e}", path.display()));
            let lower = text.to_ascii_lowercase();
            // Scan for "cycle <space> <digit>". `windows(7)` over bytes
            // — keep it dependency-free; the docs are small enough
            // that the linear scan is negligible.
            let needle: &[u8] = b"cycle ";
            for (i, w) in lower.as_bytes().windows(needle.len()).enumerate() {
                if w == needle
                    && let Some(&b) = lower.as_bytes().get(i + needle.len())
                    && b.is_ascii_digit()
                {
                    panic!(
                        "internal `cycle N` ref leaked into user-facing doc {}: \
                         offset {} (`{}`)",
                        rel,
                        i,
                        &text[i..(i + 12).min(text.len())]
                    );
                }
            }
            // Hardcoded test-count claims drift every cycle. Detect
            // `<digit> workspace tests` and `<digit> tests across`
            // — the "230+" / "an extensive suite" phrasings don't
            // trigger because they don't have a digit immediately
            // before the substring.
            for stale in [" workspace tests", " tests across"] {
                let stale_l = stale.to_ascii_lowercase();
                let needle = stale_l.as_bytes();
                for (i, w) in lower.as_bytes().windows(needle.len()).enumerate() {
                    if w == needle && i > 0 && lower.as_bytes()[i - 1].is_ascii_digit() {
                        // Walk back to find the start of the integer.
                        let mut start = i;
                        while start > 0 && lower.as_bytes()[start - 1].is_ascii_digit() {
                            start -= 1;
                        }
                        panic!(
                            "hardcoded test count in user-facing doc {} \
                             (drifts every cycle — reword as `230+ tests` \
                             or `an extensive suite`): `{}`",
                            rel,
                            &text[start..(i + stale.len()).min(text.len())],
                        );
                    }
                }
            }
        }
    }

    // ────────────────────────────────────────────────────────────
    // Cycle 235: consolidated drift guards for user-facing markdown.
    //
    // Cycles 223/224 (image guard) and 232/233 (link guard) had
    // near-identical byte-walking scanners that differed only in
    // which kind of `[…](path)` they matched. Cycle 233 added
    // backtick-awareness to the link scanner; cycle 234 propagated
    // the same fix to the image scanner. With both behaviorally
    // identical except for the `!` prefix, consolidating into one
    // shared callback-driven walker is a clean refactor.
    //
    // `walk_md_refs` does the byte walking and calls `visit` for
    // every well-formed `[…](path)` reference (image-prefixed `!`
    // marked via `kind`). Each test filters and asserts on its own
    // kind so the failure messages stay specific to the guard
    // that was tripped.
    //
    // Backtick-awareness: we flip an `in_code` flag on every `` ` ``
    // and skip the link-parsing branch while inside (a real markdown
    // renderer treats `\`[label](path)\`` as inline code, not a
    // link — CHANGELOG.md has these as textual examples).
    // ────────────────────────────────────────────────────────────

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MdRefKind {
        /// `![alt](path)` — image embed.
        Image,
        /// `[label](path)` — text link.
        Link,
    }

    /// Walk a markdown document and call `visit` for every well-formed
    /// `[…](path)` reference (or `![…](path)` image embed). Caller
    /// gets the raw path string (with any trailing title / anchor
    /// fragment still attached); peels off the title / anchor on
    /// their side so this stays a pure scanner.
    fn walk_md_refs(text: &str, mut visit: impl FnMut(MdRefKind, &str)) {
        let bytes = text.as_bytes();
        let mut i = 0usize;
        let mut in_code = false;
        while i + 2 < bytes.len() {
            if bytes[i] == b'`' {
                in_code = !in_code;
                i += 1;
                continue;
            }
            if in_code {
                i += 1;
                continue;
            }
            if bytes[i] != b'[' {
                i += 1;
                continue;
            }
            // Image vs. text-link: `![` vs. `[`.
            let kind = if i > 0 && bytes[i - 1] == b'!' {
                MdRefKind::Image
            } else {
                MdRefKind::Link
            };
            let alt_close = match text[i + 1..].find(']') {
                Some(j) => i + 1 + j,
                None => break,
            };
            if alt_close + 1 >= bytes.len() || bytes[alt_close + 1] != b'(' {
                i = alt_close + 1;
                continue;
            }
            let path_start = alt_close + 2;
            let path_end = match text[path_start..].find(')') {
                Some(j) => path_start + j,
                None => break,
            };
            let raw = &text[path_start..path_end];
            // Strip the optional ` "title"` after the path.
            let path = raw.split_whitespace().next().unwrap_or(raw);
            visit(kind, path);
            i = path_end + 1;
        }
    }

    /// Return every `.md` file under the repo that should pass the
    /// drift guards: README + every `docs/*.md` + top-level
    /// `CHANGELOG.md` and `CONTRIBUTING.md`. Used by both guards so
    /// they stay in scope-sync.
    fn user_facing_md_files(repo_root: &std::path::Path) -> Vec<(std::path::PathBuf, String)> {
        let mut out: Vec<(std::path::PathBuf, String)> = Vec::new();
        out.push((repo_root.join("README.md"), "README.md".to_string()));
        let docs_dir = repo_root.join("docs");
        if docs_dir.is_dir() {
            let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&docs_dir)
                .expect("read docs/")
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
                .collect();
            entries.sort();
            for path in entries {
                let rel = format!("docs/{}", path.file_name().unwrap().to_string_lossy());
                out.push((path, rel));
            }
        }
        for top in ["CHANGELOG.md", "CONTRIBUTING.md"] {
            let p = repo_root.join(top);
            if p.exists() {
                out.push((p, top.to_string()));
            }
        }
        out
    }

    #[test]
    fn user_facing_doc_images_exist() {
        // See `walk_md_refs` for the rationale. Cycle 223 introduced
        // the README guard; 224 extended to docs/*.md; 234 added
        // backtick-awareness; 235 consolidated with the link guard.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.join("../..");
        let mut readme_image_count = 0usize;
        for (file_abs, file_rel) in user_facing_md_files(&repo_root) {
            let text = std::fs::read_to_string(&file_abs)
                .unwrap_or_else(|e| panic!("missing {file_rel}: {e}"));
            let parent = file_abs.parent().expect("doc has a parent dir");
            walk_md_refs(&text, |kind, path| {
                if kind != MdRefKind::Image
                    || path.starts_with("http://")
                    || path.starts_with("https://")
                {
                    return;
                }
                let abs = parent.join(path);
                assert!(
                    abs.exists(),
                    "{file_rel} references image `{path}` but `{}` does \
                     not exist (cycle 223/224 drift guard)",
                    abs.display()
                );
                if file_rel == "README.md" {
                    readme_image_count += 1;
                }
            });
        }
        // Cycle 223's contract: README has at least one image embed.
        assert!(
            readme_image_count >= 1,
            "expected ≥ 1 image embed in README; found {readme_image_count} \
             — walker likely regressed"
        );
    }

    #[test]
    fn user_facing_doc_md_cross_links_resolve() {
        // See `walk_md_refs`. Cycle 232 introduced this; 233 made it
        // backtick-aware; 235 consolidated with the image guard.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.join("../..");
        let mut readme_link_count = 0usize;
        for (file_abs, file_rel) in user_facing_md_files(&repo_root) {
            let text = std::fs::read_to_string(&file_abs)
                .unwrap_or_else(|e| panic!("missing {file_rel}: {e}"));
            let parent = file_abs.parent().expect("doc has a parent dir");
            walk_md_refs(&text, |kind, path| {
                if kind != MdRefKind::Link {
                    return;
                }
                let no_frag = path.split('#').next().unwrap_or(path);
                if no_frag.is_empty()
                    || no_frag.starts_with("http://")
                    || no_frag.starts_with("https://")
                    || no_frag.starts_with('#')
                    || !no_frag.ends_with(".md")
                {
                    return;
                }
                let abs = parent.join(no_frag);
                assert!(
                    abs.exists(),
                    "{file_rel} links to `{path}` but `{}` does not exist \
                     (cycle 232 drift guard)",
                    abs.display()
                );
                if file_rel == "README.md" {
                    readme_link_count += 1;
                }
            });
        }
        // Cycle 232's contract: README has ≥ 3 .md cross-links.
        assert!(
            readme_link_count >= 3,
            "expected ≥ 3 .md cross-links in README; found {readme_link_count} \
             — walker likely regressed"
        );
    }

    #[test]
    fn workspace_metadata_policy() {
        // Cycle 226 (extends cycles 213/218/225): the workspace
        // pins one source of truth for every `[package]` field
        // shared across crates, and the cycle-218 description
        // override has its own rule. This guard prevents a
        // "tidying" cycle from accidentally inverting either
        // shape — every libary inherits, binary inherits except
        // description, every shared field actually inherits.
        //
        // Without this, a contributor could write `version =
        // "1.0.0"` directly in one crate and the others would
        // drift to `1.0.2` on the next bump — `cargo metadata`
        // would still build but `cargo publish` of that crate
        // would publish a stale version. Same shape for the
        // other workspace.package fields.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.join("../..");

        // 1) workspace Cargo.toml has all the workspace.package fields
        //    we expect to share.
        let root = std::fs::read_to_string(repo_root.join("Cargo.toml"))
            .expect("workspace Cargo.toml must exist");
        for field in [
            "version = ",
            "edition = ",
            "rust-version = ",
            "license = ",
            "repository = ",
            "authors = ",
            "description = ",
        ] {
            assert!(
                root.contains(field),
                "workspace.package is missing the `{field}` line — \
                 cycle 226 contract requires every shared metadata \
                 field to live in workspace.package"
            );
        }

        // 2) every crate inherits each field via `.workspace = true`.
        // Description is the exception (library override; see cycle 218).
        let crates = [
            ("kettle-config", true),
            ("kettle-core", true),
            ("kettle-vt", true),
            ("kettle-render", true),
            ("kettle-ui", true),
            ("kettle", false), // binary: inherits description too
        ];
        for (name, is_library) in crates {
            let path = repo_root.join("crates").join(name).join("Cargo.toml");
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("missing {}: {e}", path.display()));
            for inherit in [
                "version.workspace = true",
                "edition.workspace = true",
                "rust-version.workspace = true",
                "license.workspace = true",
                "repository.workspace = true",
                "authors.workspace = true",
            ] {
                assert!(
                    text.contains(inherit),
                    "{name}: missing `{inherit}` — cycle 226 contract \
                     requires every crate to inherit this field from \
                     workspace.package"
                );
            }
            if is_library {
                // Library: override description with its own.
                assert!(
                    text.contains("\ndescription = \"kettle: "),
                    "{name}: library must override description with \
                     `description = \"kettle: …\"` (cycle 218)"
                );
                assert!(
                    !text.contains("description.workspace = true"),
                    "{name}: library must NOT inherit description \
                     (cycle 218 — would emit binary's blurb)"
                );
            } else {
                // Binary: inherits description.
                assert!(
                    text.contains("description.workspace = true"),
                    "kettle (binary): keeps `description.workspace = true` \
                     so the workspace blurb stays the single source of \
                     truth for the binary's blurb (cycle 218)"
                );
            }
        }
    }

    #[test]
    fn default_is_tokyonight_night() {
        let c = Config::default();
        assert_eq!(c.theme_name, "TokyoNight Night");
        assert_eq!(c.theme.background, Rgb::new(0x1a, 0x1b, 0x26));
        assert_eq!(c.theme.foreground, Rgb::new(0xc0, 0xca, 0xf5));
        assert_eq!(c.theme.palette[4], Rgb::new(0x7a, 0xa2, 0xf7));
        assert_eq!(c.font_family, font::FAMILY);
    }

    #[test]
    fn default_path_falls_through_empty_env_vars() {
        // Cycle 181 (sibling to cycle 180): `XDG_CONFIG_HOME=""`
        // (stripped CI container, misconfigured unset/export) used to
        // return `Some(PathBuf::from(""))` from the first arm, and the
        // final path became `"kettle/config"` — a *relative* path that
        // could pick up a stray `kettle/config` file in whatever
        // directory the user launched kettle from. Now empty values
        // are filtered as if unset and the probe continues.
        use std::ffi::OsString;
        use std::path::PathBuf;

        fn from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
            move |k| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == k)
                    .map(|(_, v)| OsString::from(*v))
            }
        }

        // XDG_CONFIG_HOME set normally → wins, joins `kettle/config`.
        // Build the expected value via PathBuf::join so the assertion
        // uses the platform separator and works on Windows CI too.
        assert_eq!(
            Config::default_path_from(from(&[("XDG_CONFIG_HOME", "/x")])),
            Some(PathBuf::from("/x").join("kettle").join("config")),
        );
        // XDG_CONFIG_HOME empty, HOME set → HOME-based path
        // ($HOME/.config/kettle/config), absolute.
        assert_eq!(
            Config::default_path_from(from(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")])),
            Some(
                PathBuf::from("/h")
                    .join(".config")
                    .join("kettle")
                    .join("config"),
            ),
        );
        // Both empty, APPDATA set (Windows) → APPDATA-based path.
        // PathBuf::join uses the platform separator (`/` on Linux/Mac,
        // `\` on Windows); build the expected value the same way
        // rather than hardcoding either form so the assertion holds on
        // every CI runner.
        assert_eq!(
            Config::default_path_from(from(&[
                ("XDG_CONFIG_HOME", ""),
                ("HOME", ""),
                ("APPDATA", r"C:\u\AppData\Roaming"),
            ])),
            Some(
                PathBuf::from(r"C:\u\AppData\Roaming")
                    .join("kettle")
                    .join("config"),
            ),
        );
        // All three empty → None (rather than the pre-cycle relative
        // `"kettle/config"`).
        assert_eq!(
            Config::default_path_from(from(&[
                ("XDG_CONFIG_HOME", ""),
                ("HOME", ""),
                ("APPDATA", ""),
            ])),
            None,
        );
        // Nothing set at all → None (same outcome).
        assert_eq!(Config::default_path_from(from(&[])), None);
    }

    #[test]
    fn theme_name_matches_the_actually_loaded_palette() {
        // Cycle 176: pre-fix, `parse_collect` did
        //   cfg.theme_name = e.value.clone();      // typo preserved
        //   cfg.theme = Theme::by_name(&e.value);  // silent fallback
        // so a typo'd theme name had `--check-config` print
        // `theme: TokyoNitght Night` while the runtime used
        // TokyoNight Night's palette. Same docs/runtime mismatch shape
        // as cycle 139 (font-size). Now: store the canonical bundled
        // name (with original casing) when the lookup matches; leave
        // `theme_name` at the prior default when it misses.
        //
        // Case-insensitive input → canonical-casing output.
        let c = Config::parse_text("theme = tokyonight night\n");
        assert_eq!(c.theme_name, "TokyoNight Night");
        assert_eq!(c.theme.background, Rgb::new(0x1a, 0x1b, 0x26));
        // Different real theme — verify by_name + name agree.
        let c = Config::parse_text("theme = Dracula\n");
        assert_eq!(c.theme_name, "Dracula");
        // Case-insensitive match returns canonical casing.
        let c = Config::parse_text("theme = dracula\n");
        assert_eq!(c.theme_name, "Dracula", "case-insensitive → canonical case");
        // Typo: name doesn't match any bundled theme. cfg.theme falls
        // back to TokyoNight Night (Theme::default()); cfg.theme_name
        // ALSO stays at "TokyoNight Night" so the diagnostic agrees
        // with the runtime palette. The malformed-value warning still
        // surfaces the typo separately so the user notices.
        let c = Config::parse_text("theme = TokyoNitght Night\n");
        assert_eq!(c.theme_name, "TokyoNight Night");
        assert_eq!(c.theme.background, Rgb::new(0x1a, 0x1b, 0x26));
        // And the diagnostic still flags it.
        let malformed = Config::detect_malformed_values("theme = TokyoNitght Night\n");
        assert!(
            malformed.iter().any(|m| m.contains("TokyoNitght")),
            "typo'd theme should surface as malformed: {malformed:?}"
        );
    }

    #[test]
    fn empty_value_resets_string_keys_to_their_default() {
        // Cycle-121 contract. parse.rs's docstring promised "empty
        // value resets the key" but parse_collect unconditionally
        // assigned `cfg.font_family = e.value.clone()`, so a
        // `font-family =` line silently emptied the font name and
        // the renderer's measure_cell drifted into whatever
        // cosmic-text falls back to. Same for `font-family-bold` /
        // `-italic` / `-bold-italic` and `theme`. Now: empty values
        // skip the assignment (or, for Option-valued per-style
        // families, reset to None so the main font-family is the
        // fallback).
        let dflt = Config::default();

        // Empty font-family: keep default.
        let c = Config::parse_text("font-family =\n");
        assert_eq!(
            c.font_family, dflt.font_family,
            "empty font-family should keep default; got {:?}",
            c.font_family
        );

        // Per-style overrides: setting then clearing.
        let c = Config::parse_text(
            "font-family-bold = SomeBold\n\
             font-family-bold =\n\
             font-family-italic = SomeItalic\n",
        );
        assert!(
            c.font_family_bold.is_none(),
            "second empty assignment should clear bold-family override"
        );
        assert_eq!(c.font_family_italic.as_deref(), Some("SomeItalic"));

        // Empty theme: keep default theme.
        let c = Config::parse_text("theme =\n");
        assert_eq!(c.theme_name, dflt.theme_name);

        // Set-then-empty for theme: keep the set value (per-key,
        // last *non-empty* wins).
        let c = Config::parse_text("theme = Dracula\ntheme =\n");
        assert_eq!(
            c.theme_name, "Dracula",
            "empty theme reverts to default by leaving the previous override in place"
        );
        // Hmm — actually our semantics is "empty = skip", so the
        // first `theme = Dracula` is preserved. That's distinct
        // from a strict "empty = reset to compile-time default"
        // interpretation; the docstring is ambiguous and the
        // skip-form is cheaper to implement and harder to mis-use.
        // (Users wanting a reset can simply remove the line.)

        // Cycle 122 additions:
        // `command =` (empty) clears the override to None so the
        // engine falls back to the user's $SHELL — previously
        // Some("") slipped through to shell_argv and produced an
        // unspawnable empty argv.
        let c = Config::parse_text("command = /usr/bin/fish\ncommand =\n");
        assert!(
            c.shell.is_none(),
            "empty command should clear override; got {:?}",
            c.shell
        );

        // ssh-host with an empty name or empty target is silently
        // dropped (matches detect_malformed_values — cycle 88 — which
        // flagged these for --check-config but the runtime list
        // still contained them).
        let c = Config::parse_text(
            "ssh-host = good=me@host\n\
             ssh-host = =onlytarget\n\
             ssh-host = onlyname=\n\
             ssh-host = bothbad=\n",
        );
        assert_eq!(
            c.ssh_hosts.len(),
            1,
            "only the well-formed ssh-host entry should survive; got {:?}",
            c.ssh_hosts
        );
        assert_eq!(c.ssh_hosts[0].0, "good");
    }

    #[test]
    fn ghostty_syntax_overrides_and_repeats() {
        let c = Config::parse_text(
            "# comment\nfont-size = 16\nbackground = #102030\n\
             palette = 1=#abcdef\nscrollback = infinite\n\
             ssh-host = box=me@example.com\nssh-host = gpu=root@10.0.0.2\n\
             keybind = ctrl+shift+f=start_search\n",
        );
        assert_eq!(c.font_size, 16.0);
        assert_eq!(c.theme.background, Rgb::new(0x10, 0x20, 0x30));
        assert_eq!(c.theme.palette[1], Rgb::new(0xab, 0xcd, 0xef));
        assert_eq!(c.scrollback, INFINITE_SCROLLBACK);
        assert_eq!(c.ssh_hosts.len(), 2);
        assert_eq!(c.ssh_hosts[0], ("box".into(), "me@example.com".into()));
    }

    #[test]
    fn bundled_theme_set_is_large_and_named() {
        let list = Theme::list();
        assert!(list.len() > 400, "expected the full Ghostty set");
        assert!(list.contains(&"TokyoNight Night"));
    }

    #[test]
    fn terminator_default_keybinds() {
        let b = keybinds::defaults();
        let t = |m: Mods, k: Key| b.get(&Trigger::new(m, k)).cloned();
        // Terminator parity: Ctrl+Shift+O = split horizontally (top/bottom
        // = SplitDown); Ctrl+Shift+E = split vertically (left/right
        // = SplitRight).
        assert_eq!(
            t(Mods::CTRL | Mods::SHIFT, Key::Char('o')),
            Some(Action::SplitDown)
        );
        assert_eq!(
            t(Mods::CTRL | Mods::SHIFT, Key::Char('e')),
            Some(Action::SplitRight)
        );
        assert_eq!(
            t(Mods::CTRL | Mods::SHIFT, Key::Char('f')),
            Some(Action::StartSearch)
        );
        assert_eq!(t(Mods::SHIFT, Key::Left), Some(Action::ResizeLeft));
        assert_eq!(t(Mods::CTRL, Key::Up), Some(Action::JumpPrevPrompt));
    }

    #[test]
    fn trigger_parsing() {
        let tr = keybinds::parse_trigger("ctrl+shift+o").unwrap();
        assert_eq!(tr, Trigger::new(Mods::CTRL | Mods::SHIFT, Key::Char('o')));
        assert!(keybinds::parse_trigger("alt+up").is_some());
    }

    #[test]
    fn per_style_font_families() {
        let c = Config::parse_text(
            "font-family = Main\nfont-family-italic = Cursive\n\
             font-feature = -liga\n",
        );
        assert_eq!(c.family_for(false, false), "Main");
        assert_eq!(c.family_for(true, false), "Main"); // falls back
        assert_eq!(c.family_for(false, true), "Cursive");
        assert!(!c.font_ligatures, "-liga disables ligatures");
        // `-liga` is also recorded as an explicit liga=0 feature.
        assert_eq!(
            c.font_features,
            vec![FontFeature {
                tag: *b"liga",
                value: 0
            }]
        );
    }

    #[test]
    fn font_feature_token_parsing() {
        let p = FontFeature::parse;
        assert_eq!(
            p("liga"),
            Some(FontFeature {
                tag: *b"liga",
                value: 1
            })
        );
        assert_eq!(
            p("+calt"),
            Some(FontFeature {
                tag: *b"calt",
                value: 1
            })
        );
        assert_eq!(
            p("-liga"),
            Some(FontFeature {
                tag: *b"liga",
                value: 0
            })
        );
        assert_eq!(
            p("zero=1"),
            Some(FontFeature {
                tag: *b"zero",
                value: 1
            })
        );
        assert_eq!(
            p("ss05 3"),
            Some(FontFeature {
                tag: *b"ss05",
                value: 3
            })
        );
        assert_eq!(
            p("calt off"),
            Some(FontFeature {
                tag: *b"calt",
                value: 0
            })
        );
        // 3-char tag is right-padded with a space; bogus tokens rejected.
        assert_eq!(
            p("cv1"),
            Some(FontFeature {
                tag: *b"cv1 ",
                value: 1
            })
        );
        assert_eq!(p(""), None);
        assert_eq!(p("toolongtag"), None);
        assert_eq!(p("ss01=x"), None);
        assert!(
            FontFeature {
                tag: *b"calt",
                value: 0
            }
            .is_ligature()
        );
        assert!(
            !FontFeature {
                tag: *b"zero",
                value: 1
            }
            .is_ligature()
        );
    }

    #[test]
    fn font_feature_tag_is_lowercased() {
        // Cycle 169: OpenType feature tags are case-sensitive per spec and
        // every standard tag is lowercase (`liga`, `clig`, `calt`, `cv01`,
        // `ss05`…). A user writing `font-feature = LIGA on` had their tag
        // stored verbatim as uppercase. Two consequences flowed from
        // there:
        // 1. `is_ligature()` matched only `b"liga"` (lowercase), so the
        //    coarse `cfg.font_ligatures` flag wasn't flipped — the
        //    user's "LIGA on" didn't tell the rest of the renderer
        //    that ligatures were re-enabled.
        // 2. The uppercase tag was passed verbatim to the cosmic-text
        //    shaper, which uses the standard case-sensitive lookup
        //    and silently ignores it.
        // Net effect pre-fix: `font-feature = LIGA on` did nothing.
        // Now `parse` lowercases the tag bytes so both checks see the
        // canonical form.
        let p = FontFeature::parse;
        assert_eq!(
            p("LIGA"),
            Some(FontFeature {
                tag: *b"liga",
                value: 1
            })
        );
        assert_eq!(
            p("CV01=2"),
            Some(FontFeature {
                tag: *b"cv01",
                value: 2
            })
        );
        assert_eq!(
            p("Ss05 3"),
            Some(FontFeature {
                tag: *b"ss05",
                value: 3
            })
        );
        // And the downstream ligature-tracking path agrees now.
        let c = Config::parse_text("font-feature = -LIGA\n");
        assert!(
            !c.font_ligatures,
            "uppercase -LIGA disables like lowercase -liga"
        );
        let c = Config::parse_text("font-feature = +CLIG\n");
        assert!(
            c.font_ligatures,
            "uppercase +CLIG enables like lowercase +clig"
        );
    }

    #[test]
    fn font_features_collected_and_ligature_flag_tracks() {
        // Comma-separated list; explicit +liga re-enables, ss01 added.
        let c = Config::parse_text("font-feature = +liga, ss01, zero=1\n");
        assert!(c.font_ligatures, "+liga keeps ligatures on");
        assert_eq!(
            c.font_features,
            vec![
                FontFeature {
                    tag: *b"liga",
                    value: 1
                },
                FontFeature {
                    tag: *b"ss01",
                    value: 1
                },
                FontFeature {
                    tag: *b"zero",
                    value: 1
                },
            ]
        );
        // Default config carries no explicit features.
        assert!(Config::default().font_features.is_empty());
    }

    #[test]
    fn bell_mode_parsing() {
        assert_eq!(Config::default().bell, BellMode::Both);
        assert_eq!(Config::parse_text("bell = off").bell, BellMode::Off);
        assert_eq!(Config::parse_text("bell = visual").bell, BellMode::Visual);
        assert_eq!(
            Config::parse_text("bell = attention").bell,
            BellMode::Attention
        );
        assert!(BellMode::Both.visual() && BellMode::Both.attention());
        assert!(!BellMode::Off.visual() && !BellMode::Off.attention());
    }

    #[test]
    fn tab_format_default_and_parse() {
        let d = Config::default();
        assert_eq!(d.tab_format, "{n}: {title}");
        let c = Config::parse_text("tab-format = [{n}] {title}");
        assert_eq!(c.tab_format, "[{n}] {title}");
        let c2 = Config::parse_text("tab-title-format = {title}");
        assert_eq!(c2.tab_format, "{title}");
        // Empty value keeps the default.
        let c3 = Config::parse_text("tab-format =   ");
        assert_eq!(c3.tab_format, d.tab_format);
    }

    #[test]
    fn window_title_format_default_and_parse() {
        let d = Config::default();
        assert_eq!(d.window_title_format, "{title} — kettle");
        // Non-empty overrides take effect; alias `title-format` accepted.
        let c = Config::parse_text("window-title-format = [{tab}] {title}");
        assert_eq!(c.window_title_format, "[{tab}] {title}");
        let c2 = Config::parse_text("title-format = {cwd} - {title}");
        assert_eq!(c2.window_title_format, "{cwd} - {title}");
        // Empty value keeps the default (avoids an unusable blank title).
        let c3 = Config::parse_text("window-title-format =   ");
        assert_eq!(c3.window_title_format, d.window_title_format);
    }

    #[test]
    fn minimum_contrast_default_and_clamps() {
        assert_eq!(Config::default().minimum_contrast, 0.0);
        assert_eq!(
            Config::parse_text("minimum-contrast = 4.5").minimum_contrast,
            4.5
        );
        assert_eq!(
            Config::parse_text("minimum-contrast = -1").minimum_contrast,
            0.0,
            "clamped to 0"
        );
        assert_eq!(
            Config::parse_text("minimum-contrast = 99").minimum_contrast,
            21.0,
            "clamped to 21:1"
        );
    }

    #[test]
    fn scroll_multiplier_default_and_clamps() {
        assert_eq!(Config::default().scroll_multiplier, 1.0);
        // Both names accepted, value clamped to a sane range.
        assert_eq!(
            Config::parse_text("scroll-multiplier = 2.5").scroll_multiplier,
            2.5
        );
        assert_eq!(
            Config::parse_text("mouse-scroll-multiplier = 0.5").scroll_multiplier,
            0.5
        );
        assert_eq!(
            Config::parse_text("scroll-multiplier = 0").scroll_multiplier,
            0.1,
            "clamped to >= 0.1"
        );
        assert_eq!(
            Config::parse_text("scroll-multiplier = 9999").scroll_multiplier,
            50.0,
            "clamped to <= 50"
        );
    }

    #[test]
    fn osc52_policy_parsing_and_safe_default() {
        // Default allows writes, denies reads (clipboard exfil guard).
        let d = Config::default().osc52;
        assert_eq!(d, Osc52::Copy);
        assert!(d.can_copy() && !d.can_paste());

        assert_eq!(Config::parse_text("osc52 = off").osc52, Osc52::Off);
        assert_eq!(Config::parse_text("osc52 = paste").osc52, Osc52::Paste);
        assert_eq!(Config::parse_text("osc52 = both").osc52, Osc52::Both);
        // `clipboard` alias + unknown value falls back to the safe default.
        assert_eq!(Config::parse_text("clipboard = read").osc52, Osc52::Paste);
        assert_eq!(Config::parse_text("osc52 = bogus").osc52, Osc52::Copy);
        assert!(!Osc52::Off.can_copy() && !Osc52::Off.can_paste());
        assert!(Osc52::Both.can_copy() && Osc52::Both.can_paste());
    }

    #[test]
    fn tab_bar_config() {
        let d = Config::default();
        assert_eq!(d.tab_bar, TabBarMode::Always);
        assert_eq!(d.tab_bar_pos, TabBarPos::Top);
        assert_eq!(
            Config::parse_text("tab-bar = auto").tab_bar,
            TabBarMode::Auto
        );
        assert_eq!(Config::parse_text("tab-bar = off").tab_bar, TabBarMode::Off);
        assert_eq!(
            Config::parse_text("tab-bar-position = bottom").tab_bar_pos,
            TabBarPos::Bottom
        );
    }

    #[test]
    fn borderless_and_always_on_top_parse() {
        // Cycle 332 drift guard. Terminator's `borderless` +
        // `always_on_top` config keys (terminatorlib/config.py:75
        // + 78). kettle accepts both true/false + the standard
        // `parse_bool` truthy/falsy aliases.
        assert!(!Config::default().borderless);
        assert!(!Config::default().always_on_top);
        assert!(Config::parse_text("borderless = true").borderless);
        assert!(!Config::parse_text("borderless = false").borderless);
        assert!(Config::parse_text("always-on-top = true").always_on_top);
        assert!(Config::parse_text("always_on_top = true").always_on_top);
    }

    #[test]
    fn render_and_copy_bools_parse() {
        // Cycle 333 drift guard. allow_bold defaults true (Terminator
        // default), others default false.
        let d = Config::default();
        assert!(d.allow_bold);
        assert!(!d.bold_is_bright);
        assert!(!d.link_single_click);
        assert!(!d.clear_select_on_copy);
        assert!(!Config::parse_text("allow-bold = false").allow_bold);
        assert!(!Config::parse_text("allow_bold = false").allow_bold);
        assert!(Config::parse_text("bold-is-bright = true").bold_is_bright);
        assert!(Config::parse_text("bold_is_bright = true").bold_is_bright);
        assert!(Config::parse_text("link-single-click = true").link_single_click);
        assert!(Config::parse_text("link_single_click = true").link_single_click);
        assert!(Config::parse_text("clear-select-on-copy = true").clear_select_on_copy);
        assert!(Config::parse_text("clear_select_on_copy = true").clear_select_on_copy);
    }

    #[test]
    fn background_cell_detachable_parse() {
        // Cycle 341 drift guard. Closes the remaining config-key
        // surface (background image + cell metrics + detachable
        // tabs + putty source).
        let d = Config::default();
        assert_eq!(d.background_type, BackgroundType::Solid);
        assert_eq!(d.background_image, "");
        assert_eq!(d.background_image_mode, "stretch_and_fill");
        assert_eq!(d.background_image_align_horiz, "center");
        assert_eq!(d.background_image_align_vert, "middle");
        assert!(!d.background_blur);
        assert!((d.background_darkness - 0.5).abs() < 1e-6);
        assert!((d.cell_height - 1.0).abs() < 1e-6);
        assert!((d.cell_width - 1.0).abs() < 1e-6);
        assert!(d.detachable_tabs);
        assert!(!d.putty_paste_style_source_clipboard);
        // Enum parsing.
        assert_eq!(
            Config::parse_text("background-type = image").background_type,
            BackgroundType::Image
        );
        assert_eq!(
            Config::parse_text("background_type = transparent").background_type,
            BackgroundType::Transparent
        );
        // String paths.
        assert_eq!(
            Config::parse_text("background-image = /tmp/wp.jpg").background_image,
            "/tmp/wp.jpg"
        );
        // Floats clamp.
        assert!(
            (Config::parse_text("background-darkness = 0.75").background_darkness - 0.75).abs()
                < 1e-6
        );
        assert!((Config::parse_text("cell-height = 1.5").cell_height - 1.5).abs() < 1e-6);
        assert!((Config::parse_text("cell-width = 99.0").cell_width - 3.0).abs() < 1e-6);
        // Bools.
        assert!(!Config::parse_text("detachable-tabs = false").detachable_tabs);
        assert!(!Config::parse_text("detachable_tabs = false").detachable_tabs);
        assert!(
            Config::parse_text("putty-paste-style-source-clipboard = true")
                .putty_paste_style_source_clipboard
        );
        assert!(
            Config::parse_text("putty_paste_style_source_clipboard = true")
                .putty_paste_style_source_clipboard
        );
    }

    #[test]
    fn bell_titlebar_misc_keys_parse() {
        // Cycle 340 drift guard. Bell sub-flag aliases + per-pane
        // titlebar color/font keys + system-font/theme-colors stubs
        // + http_proxy. All defaults match Terminator's defaults.
        let d = Config::default();
        assert!(!d.force_no_bell);
        assert!(d.icon_bell);
        assert!(d.show_titlebar);
        assert!(!d.title_hide_sizetext);
        assert!(d.title_use_system_font);
        assert_eq!(d.title_font, "Sans 9");
        assert!(d.title_transmit_fg_color.is_none());
        assert!(d.title_inactive_bg_color.is_none());
        assert!(d.cursor_color_default);
        assert!(d.use_system_font);
        assert!(!d.use_theme_colors);
        assert_eq!(d.http_proxy, "");
        // Parsing samples.
        assert!(Config::parse_text("force-no-bell = true").force_no_bell);
        assert!(Config::parse_text("force_no_bell = true").force_no_bell);
        assert!(!Config::parse_text("icon-bell = false").icon_bell);
        assert!(!Config::parse_text("show-titlebar = false").show_titlebar);
        assert!(Config::parse_text("title-hide-sizetext = true").title_hide_sizetext);
        assert!(!Config::parse_text("title-use-system-font = false").title_use_system_font);
        assert_eq!(
            Config::parse_text("title-font = Inter 11").title_font,
            "Inter 11"
        );
        let c = Config::parse_text("title-transmit-fg-color = #abcdef");
        assert!(c.title_transmit_fg_color.is_some());
        assert!(!Config::parse_text("cursor-color-default = false").cursor_color_default);
        assert!(!Config::parse_text("use-system-font = false").use_system_font);
        assert!(Config::parse_text("use-theme-colors = true").use_theme_colors);
        assert_eq!(
            Config::parse_text("http-proxy = http://proxy.example:8080").http_proxy,
            "http://proxy.example:8080"
        );
    }

    #[test]
    fn group_focus_handle_window_state_parse() {
        // Cycle 339 drift guard.
        let d = Config::default();
        assert!(!d.split_to_group);
        assert!(d.autoclean_groups);
        assert!(!d.always_split_with_profile);
        assert_eq!(d.focus, FocusMode::Click);
        assert_eq!(d.handle_size, -1);
        assert_eq!(d.window_state, WindowState::Normal);
        assert!(!d.geometry_hinting);
        assert!(d.extra_styling);
        // FocusMode parsing.
        assert_eq!(
            Config::parse_text("focus = sloppy").focus,
            FocusMode::Sloppy
        );
        assert_eq!(
            Config::parse_text("focus = system").focus,
            FocusMode::System
        );
        // window-state alias.
        assert_eq!(
            Config::parse_text("window-state = maximise").window_state,
            WindowState::Maximise
        );
        assert_eq!(
            Config::parse_text("window_state = maximize").window_state,
            WindowState::Maximise
        );
        assert_eq!(
            Config::parse_text("window-state = fullscreen").window_state,
            WindowState::Fullscreen
        );
        assert_eq!(
            Config::parse_text("window-state = hidden").window_state,
            WindowState::Hidden
        );
        // handle_size clamp.
        assert_eq!(Config::parse_text("handle-size = 99").handle_size, 50);
        // group bools both forms.
        assert!(Config::parse_text("split-to-group = true").split_to_group);
        assert!(Config::parse_text("split_to_group = true").split_to_group);
        assert!(!Config::parse_text("autoclean-groups = false").autoclean_groups);
        assert!(!Config::parse_text("autoclean_groups = false").autoclean_groups);
        assert!(Config::parse_text("always-split-with-profile = true").always_split_with_profile);
        assert!(Config::parse_text("always_split_with_profile = true").always_split_with_profile);
    }

    #[test]
    fn key_encoding_broadcast_url_offsets_parse() {
        // Cycle 338 drift guard.
        let d = Config::default();
        assert_eq!(d.backspace_binding, BackspaceBinding::AsciiDel);
        assert_eq!(d.delete_binding, DeleteBinding::EscapeSequence);
        assert_eq!(d.broadcast_default, BroadcastDefault::Group);
        assert!(!d.use_custom_url_handler);
        assert_eq!(d.custom_url_handler, "");
        assert!((d.inactive_color_offset - 1.0).abs() < 1e-6);
        assert!((d.inactive_bg_color_offset - 1.0).abs() < 1e-6);
        // Key-encoding parse arms.
        assert_eq!(
            Config::parse_text("backspace-binding = control-h").backspace_binding,
            BackspaceBinding::ControlH
        );
        assert_eq!(
            Config::parse_text("delete_binding = ascii-del").delete_binding,
            DeleteBinding::AsciiDel
        );
        // Broadcast default.
        assert_eq!(
            Config::parse_text("broadcast-default = all").broadcast_default,
            BroadcastDefault::All
        );
        assert_eq!(
            Config::parse_text("broadcast_default = off").broadcast_default,
            BroadcastDefault::Off
        );
        // Custom URL handler.
        let c = Config::parse_text(
            "use-custom-url-handler = true\ncustom-url-handler = firefox-developer-edition",
        );
        assert!(c.use_custom_url_handler);
        assert_eq!(c.custom_url_handler, "firefox-developer-edition");
        // Inactive offset clamps.
        assert!(
            (Config::parse_text("inactive-color-offset = 0.5").inactive_color_offset - 0.5).abs()
                < 1e-6
        );
        assert!(
            (Config::parse_text("inactive_bg_color_offset = 999.0").inactive_bg_color_offset - 1.0)
                .abs()
                < 1e-6,
            "should clamp to 1.0 max"
        );
    }

    #[test]
    fn tab_ux_and_window_state_bools_parse() {
        // Cycle 337 drift guard. 8 bool config keys from
        // terminatorlib/config.py:75-97.
        let d = Config::default();
        assert!(d.close_button_on_tab);
        assert!(!d.new_tab_after_current_tab);
        assert!(!d.title_at_bottom);
        assert!(!d.scroll_tabbar);
        assert!(d.homogeneous_tabbar);
        assert!(!d.hide_on_lose_focus);
        assert!(!d.sticky);
        assert!(!d.hide_from_taskbar);
        // Each accepts kebab + underscore form. Probe via specific
        // field reads (Config doesn't derive PartialEq).
        assert!(!Config::parse_text("close-button-on-tab = false").close_button_on_tab);
        assert!(!Config::parse_text("close_button_on_tab = false").close_button_on_tab);
        assert!(Config::parse_text("new-tab-after-current-tab = true").new_tab_after_current_tab);
        assert!(Config::parse_text("new_tab_after_current_tab = true").new_tab_after_current_tab);
        assert!(Config::parse_text("title-at-bottom = true").title_at_bottom);
        assert!(Config::parse_text("title_at_bottom = true").title_at_bottom);
        assert!(Config::parse_text("scroll-tabbar = true").scroll_tabbar);
        assert!(Config::parse_text("scroll_tabbar = true").scroll_tabbar);
        assert!(Config::parse_text("hide-on-lose-focus = true").hide_on_lose_focus);
        assert!(Config::parse_text("hide_on_lose_focus = true").hide_on_lose_focus);
        assert!(Config::parse_text("hide-from-taskbar = true").hide_from_taskbar);
        assert!(Config::parse_text("hide_from_taskbar = true").hide_from_taskbar);
        // sticky is single-form (no underscore variant — it's
        // already a single word).
        assert!(Config::parse_text("sticky = true").sticky);
        // homogeneous-tabbar default true, parse_text confirms it
        // accepts override to false.
        assert!(!Config::parse_text("homogeneous-tabbar = false").homogeneous_tabbar);
    }

    #[test]
    fn shell_exec_and_close_behavior_parse() {
        // Cycle 336 drift guard.
        let d = Config::default();
        assert!(!d.login_shell);
        assert_eq!(d.exit_action, ExitAction::Close);
        assert_eq!(d.ask_before_closing, AskBeforeClosing::MultipleTerminals);
        assert!(Config::parse_text("login-shell = true").login_shell);
        assert!(Config::parse_text("login_shell = true").login_shell);
        assert_eq!(
            Config::parse_text("exit-action = restart").exit_action,
            ExitAction::Restart
        );
        assert_eq!(
            Config::parse_text("exit-action = hold").exit_action,
            ExitAction::Hold
        );
        assert_eq!(
            Config::parse_text("exit_action = close").exit_action,
            ExitAction::Close
        );
        assert_eq!(
            Config::parse_text("ask-before-closing = always").ask_before_closing,
            AskBeforeClosing::Always
        );
        assert_eq!(
            Config::parse_text("ask_before_closing = never").ask_before_closing,
            AskBeforeClosing::Never
        );
        assert_eq!(
            Config::parse_text("exit-action = wat").exit_action,
            ExitAction::Close
        );
    }

    #[test]
    fn invert_search_and_env_strings_parse() {
        // Cycle 335 drift guard.
        let d = Config::default();
        assert!(!d.invert_search);
        assert_eq!(d.term, "xterm-256color");
        assert_eq!(d.colorterm, "truecolor");
        assert!(Config::parse_text("invert-search = true").invert_search);
        assert!(Config::parse_text("invert_search = true").invert_search);
        assert_eq!(
            Config::parse_text("term = screen-256color").term,
            "screen-256color"
        );
        assert_eq!(Config::parse_text("colorterm = 24bit").colorterm, "24bit");
        // Empty value preserves the default (avoids breaking shells
        // that rely on a non-empty TERM).
        assert_eq!(Config::parse_text("term =").term, "xterm-256color");
    }

    #[test]
    fn mouse_and_paste_bools_parse() {
        // Cycle 334 drift guard. smart_copy defaults true (Terminator
        // default); others default false.
        let d = Config::default();
        assert!(!d.disable_mousewheel_zoom);
        assert!(!d.disable_mouse_paste);
        assert!(!d.putty_paste_style);
        assert!(d.smart_copy);
        assert!(Config::parse_text("disable-mousewheel-zoom = true").disable_mousewheel_zoom);
        assert!(Config::parse_text("disable_mousewheel_zoom = true").disable_mousewheel_zoom);
        assert!(Config::parse_text("disable-mouse-paste = true").disable_mouse_paste);
        assert!(Config::parse_text("disable_mouse_paste = true").disable_mouse_paste);
        assert!(Config::parse_text("putty-paste-style = true").putty_paste_style);
        assert!(Config::parse_text("putty_paste_style = true").putty_paste_style);
        assert!(!Config::parse_text("smart-copy = false").smart_copy);
        assert!(!Config::parse_text("smart_copy = false").smart_copy);
    }

    #[test]
    fn tab_bar_position_terminator_aliases() {
        // Cycle 331 drift guard. Terminator's `tab_position` accepts
        // top/left/right/bottom/hidden; kettle maps them as:
        //   - top/bottom: native.
        //   - hidden: alias to `tab-bar = off`.
        //   - left/right: accepted by --check-config (so a copied
        //     Terminator config doesn't fail), but the runtime falls
        //     through to top with a log::warn (vertical tab bars
        //     are Bucket C in docs/TERMINATOR-AUDIT.md).
        let hidden = Config::parse_text("tab-bar-position = hidden");
        assert_eq!(hidden.tab_bar, TabBarMode::Off);
        let left = Config::parse_text("tab-bar-position = left");
        assert_eq!(left.tab_bar_pos, TabBarPos::Top);
        let right = Config::parse_text("tab-bar-position = right");
        assert_eq!(right.tab_bar_pos, TabBarPos::Top);
        // detect_malformed_values must NOT flag any of the five
        // Terminator values — that's what guarantees Terminator
        // users can copy their config without --check-config errors.
        for value in &["top", "bottom", "hidden", "left", "right"] {
            let bad = Config::detect_malformed_values(&format!("tab-bar-position = {value}\n"));
            assert!(
                bad.iter().all(|b| !b.contains("tab-bar-position")),
                "value {value:?} flagged as malformed: {bad:?}"
            );
        }
        // A truly bogus value still gets flagged.
        let bad = Config::detect_malformed_values("tab-bar-position = sideways\n");
        assert!(bad.iter().any(|b| b.contains("tab-bar-position")));
    }

    #[test]
    fn ux_backlog_config() {
        let d = Config::default();
        assert_eq!(d.unfocused_split_opacity, 0.7);
        assert_eq!(d.scrollbar, ScrollbarMode::Auto);
        assert_eq!(d.cursor_blink_interval, 530);
        assert!(d.copy_on_select);
        assert!(d.split_divider_color.is_none());
        assert!(d.focused_split_color.is_none());
        let c = Config::parse_text(
            "unfocused-split-opacity = 0.5\nscrollbar = always\n\
             split-divider-color = #ff8800\ncursor-blink-interval = 800\n\
             copy-on-select = false\n",
        );
        assert_eq!(c.unfocused_split_opacity, 0.5);
        assert_eq!(c.scrollbar, ScrollbarMode::Always);
        assert_eq!(c.split_divider_color, Some(Rgb::new(0xff, 0x88, 0x00)));
        // `focused-split-color` (canonical) + `split-divider-color-focused`
        // (alias) both populate the focused-pane border override.
        assert_eq!(
            Config::parse_text("focused-split-color = #00ff00").focused_split_color,
            Some(Rgb::new(0x00, 0xff, 0x00))
        );
        assert_eq!(
            Config::parse_text("split-divider-color-focused = #0088ff").focused_split_color,
            Some(Rgb::new(0x00, 0x88, 0xff))
        );
        assert_eq!(c.cursor_blink_interval, 800);
        assert!(!c.copy_on_select);
        // Clamping.
        assert_eq!(
            Config::parse_text("unfocused-split-opacity = 5").unfocused_split_opacity,
            1.0
        );
    }

    #[test]
    fn scroll_behavior_defaults_and_parse() {
        // Defaults match Alacritty's: keystroke yanks you to the bottom
        // (so typing into a scrolled-back history is never confusing),
        // output does not (a chatty background process won't tear you
        // away from the page you're reading).
        let d = Config::default();
        assert!(d.scroll_on_keystroke);
        assert!(!d.scroll_on_output);
        let c = Config::parse_text("scroll-on-keystroke = false\nscroll-on-output = true\n");
        assert!(!c.scroll_on_keystroke);
        assert!(c.scroll_on_output);
        // Alacritty's `scroll-on-input` alias is honored too.
        assert!(!Config::parse_text("scroll-on-input = false").scroll_on_keystroke);
    }

    #[test]
    fn detect_malformed_values_catches_typos_silently_swallowed_by_parse() {
        // Each of these was silently falling through to the default
        // before — `parse_collect` would skip the `if let Ok(v) =
        // parse()` arm and the user thought their setting took effect.
        let bad = Config::detect_malformed_values(
            "font-size = not_a_number\n\
             padding-x = 4px\n\
             background-opacity = high\n\
             scroll-multiplier = fast\n\
             cursor-blink-interval = forever\n\
             scrollback = lots\n",
        );
        assert_eq!(bad.len(), 6, "all six should be flagged: {bad:?}");
        assert!(bad.iter().any(|b| b.contains("font-size")));
        assert!(bad.iter().any(|b| b.contains("scrollback")));
        // Valid values pass cleanly.
        let ok = Config::detect_malformed_values(
            "font-size = 14\n\
             padding-x = 6.5\n\
             background-opacity = 0.8\n\
             scrollback = infinite\n\
             scrollback = 0\n\
             cursor-blink-interval = 800\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
        // Unknown keys aren't *malformed* — that's a separate diagnostic
        // (caught by `parse_collect`'s `unknown` Vec). detect_malformed
        // intentionally returns empty for them so the two lists don't
        // duplicate.
        assert!(Config::detect_malformed_values("totally-unknown = x").is_empty());
    }

    #[test]
    fn load_from_with_diagnostics_surfaces_both_unknown_and_malformed() {
        // Cycle-99 contract: a reload via `Action::ReloadConfig` should
        // give the user *some* signal that their typo wasn't applied.
        // `load_from` used to only `log::warn!` on unknown keys; bad
        // values silently dropped. The diagnostics variant returns both
        // lists so chrome callers can render them (the public log path
        // wraps it).
        let dir = std::env::temp_dir().join(format!(
            "kettle-load-from-diag-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("kettle.conf");
        std::fs::write(
            &path,
            "font-size = wrong\n\
             totally-not-a-key = whatever\n\
             theme = TokyoNight Night\n\
             font-family Jetbrains Mono\n",
        )
        .expect("write");
        let (cfg, unknown, malformed) = Config::load_from_with_diagnostics(&path);
        // Cfg parsed cleanly past the typos (`theme` set, others defaulted).
        assert_eq!(cfg.theme_name, "TokyoNight Night");
        // Unknown keys: `totally-not-a-key`.
        assert!(
            unknown.iter().any(|k| k == "totally-not-a-key"),
            "unknown: {unknown:?}"
        );
        // Malformed: bad `font-size` value AND the missing-= line.
        assert!(
            malformed.iter().any(|m| m.contains("font-size")),
            "malformed: {malformed:?}"
        );
        assert!(
            malformed
                .iter()
                .any(|m| m.contains("missing `=` separator")),
            "malformed: {malformed:?}"
        );
        // Cleanup; ignore failures (race-safe).
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// Cycle 586 drift guard: an oversized config file (swap-attack
    /// scenario; out of strict SECURITY.md scope but defense-in-depth)
    /// must be refused via the metadata pre-check rather than read
    /// into RAM. Asserts the 1 MiB cap is enforced and the function
    /// falls through to Config::default() rather than allocating.
    #[test]
    fn load_from_with_diagnostics_rejects_oversize_config() {
        let dir = std::env::temp_dir().join(format!(
            "kettle-load-from-diag-oversize-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let path = dir.join("kettle.conf");
        // Write a 2 MiB file (1 MiB over the cap). Content is
        // valid-looking config lines so the test verifies the size
        // gate fires BEFORE any parsing happens — even a perfectly
        // legitimate config payload past the cap is refused.
        let line = "font-size = 14\n";
        let copies = (2 * 1024 * 1024) / line.len() + 1;
        let oversize: String = line.repeat(copies);
        std::fs::write(&path, &oversize).expect("write oversize config");
        let (cfg, unknown, malformed) = Config::load_from_with_diagnostics(&path);
        // Defaults returned; no parsing happened.
        let default_cfg = Config::default();
        assert_eq!(cfg.font_size, default_cfg.font_size);
        assert!(unknown.is_empty(), "no diagnostics past the cap");
        assert!(malformed.is_empty(), "no diagnostics past the cap");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn detect_malformed_values_skips_empty_values() {
        // Cycle 158: empty values are documented as "reset to
        // default" in parse.rs and are honored by parse_collect.
        // The diagnostic used to disagree with the runtime —
        // theme = "" surfaced as "malformed value: theme = \"\""
        // while the runtime quietly defaulted. Same shape for
        // any other key on the empty-value path. Skip the
        // per-key validity check for empty values entirely so
        // the two sources agree.
        let ok = Config::detect_malformed_values(
            "theme =\n\
             font-family =\n\
             cursor-style =\n\
             cursor-style-blink =\n\
             bell =\n\
             scrollbar =\n\
             font-size =\n\
             background-opacity =\n",
        );
        assert!(ok.is_empty(), "empty values should never flag: {ok:?}");
        // Whitespace-only also counts as empty.
        let ok = Config::detect_malformed_values("theme =    \n");
        assert!(
            ok.is_empty(),
            "whitespace-only value should not flag: {ok:?}"
        );
        // Real typos with non-empty values still flag (regression
        // guard for cycle 86/87/etc — empty skip mustn't swallow
        // typos).
        let bad = Config::detect_malformed_values("theme = NoSuchTheme\n");
        assert_eq!(bad.len(), 1, "unknown theme still flagged: {bad:?}");
    }

    #[test]
    fn detect_malformed_values_strips_bom_before_scanning() {
        // Cycle 156 (sibling to cycle 155): a BOM-prefixed config
        // with a missing-`=` typo on the first key used to surface
        // the diagnostic with the BOM character mangled in (looks
        // like an unintended invisible-char in the user-facing
        // output). Now `detect_malformed_values` also strips the
        // leading BOM, mirroring parse::parse.
        let bad = Config::detect_malformed_values("\u{feff}font-family\n");
        assert_eq!(bad.len(), 1);
        // The flagged line should NOT contain the BOM character.
        assert!(
            !bad[0].contains('\u{feff}'),
            "diagnostic should be BOM-free: {bad:?}"
        );
        assert!(bad[0].contains("font-family"));
        // A clean BOM-prefixed config (with a valid first line) is
        // not flagged at all.
        let ok = Config::detect_malformed_values("\u{feff}font-family = Hack\n");
        assert!(ok.is_empty(), "valid BOM-prefixed config: {ok:?}");
    }

    #[test]
    fn detect_malformed_values_flags_lines_missing_equals() {
        // parse.rs:21 silently `continue`s on any non-comment, non-empty
        // line without `=`. A user typo like `font-family Jetbrains Mono`
        // (forgot the `=`) used to disappear with no warning at all —
        // their font config was effectively a no-op and `--check-config`
        // happily printed "status: OK — no issues". Surface it here.
        let bad = Config::detect_malformed_values(
            "font-family\n\
             font-size 13\n\
             theme = TokyoNight Night\n\
             scrollback 5000\n",
        );
        // Three offenders: the two `=`-less lines plus the standalone
        // `font-family` (also no `=`). The `theme =` line is fine.
        assert_eq!(bad.len(), 3, "three missing-= lines: {bad:?}");
        assert!(bad.iter().all(|b| b.contains("missing `=` separator")));
        assert!(bad.iter().any(|b| b.contains("font-family")));
        assert!(bad.iter().any(|b| b.contains("font-size 13")));
        assert!(bad.iter().any(|b| b.contains("scrollback 5000")));
        // Comments and blanks are not flagged.
        let ok = Config::detect_malformed_values(
            "# this is a comment line\n\
             \n\
             font-family = JetBrainsMono\n",
        );
        assert!(
            ok.is_empty(),
            "comments and blanks are not malformed: {ok:?}"
        );
    }

    #[test]
    fn detect_malformed_values_catches_bad_font_feature_and_ssh_host() {
        // `font-feature` silently drops bad tokens (the parser returns
        // None and the apply loop just skips). A comma-list with one
        // bad entry leaves the user with a partly-applied set and no
        // warning. Now flagged.
        let bad = Config::detect_malformed_values(
            "font-feature = liga,!@#,calt\n\
             font-feature = no-such-tag-too-long\n\
             ssh-host = box-with-no-equals\n\
             ssh-host = =empty-name\n\
             ssh-host = empty-target=\n",
        );
        assert_eq!(
            bad.len(),
            5,
            "all five bad lines should be flagged: {bad:?}"
        );
        assert!(
            bad.iter()
                .any(|b| b.contains("font-feature") && b.contains("!@#"))
        );
        assert!(
            bad.iter()
                .any(|b| b.contains("ssh-host") && b.contains("no-equals"))
        );
        // Valid `font-feature` syntaxes (plain, +/-, on/off, =N) and
        // `ssh-host` lines pass cleanly.
        let ok = Config::detect_malformed_values(
            "font-feature = liga\n\
             font-feature = -calt\n\
             font-feature = +ss01\n\
             font-feature = cv01=2\n\
             font-feature = zero on,ss05 3\n\
             ssh-host = box=me@example.com\n\
             ssh-host = gpu=root@10.0.0.2\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
    }

    #[test]
    fn detect_malformed_values_catches_unknown_enum_values() {
        // Each enum config has an `_ => Default` arm — a typo silently
        // falls back to the default. Now flagged for every documented
        // enum key.
        let bad = Config::detect_malformed_values(
            "cursor-style = wibble\n\
             bell = loud\n\
             osc52 = sometimes\n\
             clipboard = maybe\n\
             tab-bar = sticky\n\
             tab-bar-position = side\n\
             scrollbar = occasionally\n",
        );
        assert_eq!(
            bad.len(),
            7,
            "all seven bad enum values should be flagged: {bad:?}"
        );
        assert!(bad.iter().any(|b| b.contains("cursor-style")));
        assert!(bad.iter().any(|b| b.contains("tab-bar-position")));
        // Every documented variant (and alias) passes cleanly.
        let ok = Config::detect_malformed_values(
            "cursor-style = block\n\
             cursor-style = underline\n\
             cursor-style = bar\n\
             bell = off\n\
             bell = none\n\
             bell = false\n\
             bell = visual\n\
             bell = flash\n\
             bell = attention\n\
             bell = urgent\n\
             bell = both\n\
             osc52 = off\n\
             osc52 = none\n\
             osc52 = paste\n\
             osc52 = read\n\
             osc52 = both\n\
             osc52 = all\n\
             osc52 = true\n\
             osc52 = copy\n\
             clipboard = off\n\
             tab-bar = auto\n\
             tab-bar = always\n\
             tab-bar = off\n\
             tab-bar-position = top\n\
             tab-bar-position = bottom\n\
             scrollbar = never\n\
             scrollbar = auto\n\
             scrollbar = always\n",
        );
        assert!(ok.is_empty(), "all valid enum variants: {ok:?}");
    }

    #[test]
    fn detect_malformed_values_catches_unknown_theme_name() {
        // `Theme::by_name` silently falls back to TokyoNight Night on
        // unknown names — a user copying a theme name from another
        // terminal's config (Alacritty's `colors.theme = my-theme`) got
        // no warning their theme wasn't bundled. Now flagged.
        let bad = Config::detect_malformed_values(
            "theme = NonExistentTheme\n\
             theme = also fake\n",
        );
        assert_eq!(bad.len(), 2, "both unknown themes flagged: {bad:?}");
        assert!(bad.iter().any(|b| b.contains("NonExistentTheme")));
        // Bundled themes pass (case-insensitive — `by_name` is too).
        let ok = Config::detect_malformed_values(
            "theme = TokyoNight Night\n\
             theme = tokyonight night\n\
             theme = Dracula\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
    }

    #[test]
    fn detect_malformed_values_catches_bad_keybind_lines() {
        // `apply_keybind` silently drops on bad trigger or unknown
        // action — a typo like `ctrl+shift+typo=copy` or
        // `ctrl+shift+a=garbage_action` produced no binding and no
        // warning. Now flagged in `--check-config` so the user sees
        // which line was dropped.
        let bad = Config::detect_malformed_values(
            "keybind = ctrl+shift+nope=copy\n\
             keybind = ctrl+shift+a=garbage_action\n\
             keybind = no_separator_at_all\n",
        );
        assert_eq!(
            bad.len(),
            3,
            "all three bad keybinds should be flagged: {bad:?}"
        );
        assert!(bad.iter().any(|b| b.contains("ctrl+shift+nope")));
        assert!(bad.iter().any(|b| b.contains("garbage_action")));
        // Valid keybinds — including aliases, `goto_tab:N` parametric,
        // and the unbind sentinels (`unbind` / `none` / `null` /
        // `false` / empty action means "remove this default") — pass
        // cleanly.
        let ok = Config::detect_malformed_values(
            "keybind = ctrl+shift+c=copy\n\
             keybind = alt+5=goto_tab:5\n\
             keybind = f11=toggle_fullscreen\n\
             keybind = ctrl+shift+o=split_horiz\n\
             keybind = ctrl+shift+c=unbind\n\
             keybind = ctrl+shift+v=none\n\
             keybind = ctrl+shift+w=\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
    }

    #[test]
    fn enum_keys_are_case_insensitive() {
        // Cycle 146: bell / osc52 / tab-bar / tab-bar-position /
        // scrollbar / cursor-style all parsed `e.value.as_str()`
        // verbatim. So `bell = OFF` fell into the catchall →
        // BellMode::Both, with --check-config flagging it as
        // malformed. Now all enum keys lowercase before matching;
        // the diagnostic agrees.

        // bell: uppercase OFF maps to Off, not the catchall Both.
        let c = Config::parse_text("bell = OFF");
        assert_eq!(c.bell, BellMode::Off);
        let c = Config::parse_text("bell = Visual");
        assert_eq!(c.bell, BellMode::Visual);

        // osc52
        let c = Config::parse_text("osc52 = NONE");
        assert_eq!(c.osc52, Osc52::Off);
        let c = Config::parse_text("osc52 = Both");
        assert_eq!(c.osc52, Osc52::Both);

        // tab-bar
        let c = Config::parse_text("tab-bar = OFF");
        assert_eq!(c.tab_bar, TabBarMode::Off);
        let c = Config::parse_text("tab-bar = Auto");
        assert_eq!(c.tab_bar, TabBarMode::Auto);

        // tab-bar-position
        let c = Config::parse_text("tab-bar-position = BOTTOM");
        assert_eq!(c.tab_bar_pos, TabBarPos::Bottom);

        // scrollbar
        let c = Config::parse_text("scrollbar = NEVER");
        assert_eq!(c.scrollbar, ScrollbarMode::Never);
        let c = Config::parse_text("scrollbar = Always");
        assert_eq!(c.scrollbar, ScrollbarMode::Always);

        // cursor-style (cycle 142's `beam` works with any case too).
        let c = Config::parse_text("cursor-style = Underline");
        assert_eq!(c.cursor_style, CursorStyle::Underline);
        let c = Config::parse_text("cursor-style = BEAM");
        assert_eq!(c.cursor_style, CursorStyle::Bar);

        // --check-config no longer flags the case-variant spellings.
        let ok = Config::detect_malformed_values(
            "bell = OFF\n\
             osc52 = NONE\n\
             tab-bar = OFF\n\
             tab-bar-position = BOTTOM\n\
             scrollbar = NEVER\n\
             cursor-style = BEAM\n",
        );
        assert!(ok.is_empty(), "case variants should not flag: {ok:?}");
    }

    #[test]
    fn cursor_style_accepts_beam_as_alacritty_alias_for_bar() {
        // Cycle 142: a user copying their Alacritty config writes
        // `cursor-style = beam`. Pre-fix, the catchall mapped that to
        // Block (since `beam` wasn't matched), and --check-config
        // flagged it as malformed. Now `beam` is an explicit alias
        // for `bar` — same vertical-stroke cursor — and parses
        // cleanly.
        let c = Config::parse_text("cursor-style = beam");
        assert_eq!(c.cursor_style, CursorStyle::Bar);
        // `bar` still works (regression guard).
        let c = Config::parse_text("cursor-style = bar");
        assert_eq!(c.cursor_style, CursorStyle::Bar);
        // The other two values are unchanged.
        let c = Config::parse_text("cursor-style = underline");
        assert_eq!(c.cursor_style, CursorStyle::Underline);
        let c = Config::parse_text("cursor-style = block");
        assert_eq!(c.cursor_style, CursorStyle::Block);
        // `beam` no longer flagged by --check-config.
        let bad = Config::detect_malformed_values("cursor-style = beam\n");
        assert!(bad.is_empty(), "beam should now be accepted: {bad:?}");
        // Real typos still flag.
        let bad = Config::detect_malformed_values("cursor-style = bream\n");
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn bool_keys_accept_yes_no_off_on_0_1_aliases() {
        // Cycle 138. Pre-fix `cursor-style-blink = no` silently meant
        // `true` because the parser compared against literal "false"
        // and treated everything else as on. Same for every other
        // bool key. Now every standard alias works on both sides.
        let truthy = ["true", "TRUE", "yes", "YES", "on", "1", "enabled", "y"];
        let falsy = ["false", "FALSE", "no", "off", "0", "disabled", "n"];
        for v in truthy {
            let c = Config::parse_text(&format!("cursor-style-blink = {v}"));
            assert!(c.cursor_blink, "{v:?} should mean true; got false");
        }
        for v in falsy {
            let c = Config::parse_text(&format!("cursor-style-blink = {v}"));
            assert!(!c.cursor_blink, "{v:?} should mean false; got true");
        }
        // Unrecognized: silently keep the default (cursor_blink = true)
        // instead of silently flipping to true on every garbage value
        // (pre-cycle behavior).
        let c = Config::parse_text("cursor-style-blink = wat");
        assert!(c.cursor_blink, "default (true) preserved on unrecognized");
        // And `--check-config` surfaces the typo:
        let bad = Config::detect_malformed_values("cursor-style-blink = wat\n");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].contains("cursor-style-blink"));

        // Quick spot-check that all five bool keys route through
        // parse_bool — set each to "off" and confirm.
        let c = Config::parse_text(
            "copy-on-select = off\n\
             scroll-on-keystroke = off\n\
             scroll-on-output = off\n\
             mouse-hide-while-typing = off\n\
             cursor-style-blink = off\n",
        );
        assert!(!c.copy_on_select);
        assert!(!c.scroll_on_keystroke);
        assert!(!c.scroll_on_output);
        assert!(!c.mouse_hide_while_typing);
        assert!(!c.cursor_blink);
    }

    #[test]
    fn scrollback_clamps_at_infinite_and_flags_above() {
        // Cycle 133: a user with `scrollback = 100000000` (100 M
        // lines) used to land that value into cfg verbatim, which
        // alacritty_terminal would honor by reserving rows for an
        // ~250 GB history buffer on the first PTY spawn. Clamp at
        // `INFINITE_SCROLLBACK` (10 M, the documented "practical
        // stand-in for infinite") and flag the over-cap as a
        // --check-config diagnostic.
        let c = Config::parse_text(&format!("scrollback = {}", INFINITE_SCROLLBACK + 1));
        assert_eq!(c.scrollback, INFINITE_SCROLLBACK, "clamped at cap");
        let c = Config::parse_text("scrollback = 100000000");
        assert_eq!(
            c.scrollback, INFINITE_SCROLLBACK,
            "200x the cap → still clamped"
        );
        let c = Config::parse_text("scrollback = 10000");
        assert_eq!(c.scrollback, 10000, "in-range value untouched");

        // Diagnostic: above-cap surfaces as malformed.
        let bad =
            Config::detect_malformed_values(&format!("scrollback = {}", INFINITE_SCROLLBACK + 1));
        assert_eq!(bad.len(), 1, "above-cap flagged: {bad:?}");
        assert!(bad[0].contains("scrollback"));

        // Documented escape hatches still work and don't flag.
        let ok = Config::detect_malformed_values(
            "scrollback = infinite\n\
             scrollback = unlimited\n\
             scrollback = 0\n\
             scrollback = 10000\n",
        );
        assert!(ok.is_empty(), "documented forms still pass: {ok:?}");
    }

    #[test]
    fn detect_malformed_values_flags_clamped_numerics_out_of_range() {
        // Cycle 132. Same shape as cycle 131's `font-size` fix,
        // extended to the other clamped numeric fields:
        //
        //   background-opacity        [0.0, 1.0]
        //   unfocused-split-opacity   [0.1, 1.0]
        //   scroll-multiplier         [0.1, 50.0]
        //   minimum-contrast          [0.0, 21.0]
        //   cursor-blink-interval     [50,  5000]
        //
        // All clamp silently at parse or render time, so the
        // user's --check-config echo disagreed with the runtime
        // for out-of-range values. Surface as diagnostics.
        let bad = Config::detect_malformed_values(
            "background-opacity = 2.0\n\
             background-opacity = -0.5\n\
             unfocused-split-opacity = 0.05\n\
             scroll-multiplier = 0.01\n\
             scroll-multiplier = 999\n\
             minimum-contrast = 50\n\
             minimum-contrast = -1\n\
             cursor-blink-interval = 10\n\
             cursor-blink-interval = 99999\n",
        );
        assert_eq!(bad.len(), 9, "all nine should flag: {bad:?}");

        // In-range / boundary values pass cleanly.
        let ok = Config::detect_malformed_values(
            "background-opacity = 0.0\n\
             background-opacity = 1.0\n\
             background-opacity = 0.8\n\
             unfocused-split-opacity = 0.1\n\
             unfocused-split-opacity = 1.0\n\
             scroll-multiplier = 0.1\n\
             scroll-multiplier = 50\n\
             scroll-multiplier = 2.5\n\
             minimum-contrast = 0\n\
             minimum-contrast = 21\n\
             minimum-contrast = 4.5\n\
             cursor-blink-interval = 50\n\
             cursor-blink-interval = 5000\n\
             cursor-blink-interval = 530\n",
        );
        assert!(ok.is_empty(), "all in-range pass: {ok:?}");

        // Runtime clamp on background-opacity (cycle 132 added) —
        // even with the warning ignored, wgpu sees a safe alpha.
        let c = Config::parse_text("background-opacity = 2.5");
        assert_eq!(c.background_opacity, 1.0);
        let c = Config::parse_text("background-opacity = -0.5");
        assert_eq!(c.background_opacity, 0.0);
    }

    #[test]
    fn detect_malformed_values_flags_font_size_out_of_renderer_range() {
        // Cycle 131: font-size = 500 silently clamps to 72 at the
        // renderer (cycle 118's `clamp_font_size`); --check-config
        // used to echo the raw value with no hint of the clamp.
        // Surface out-of-range values as malformed so the docs/UI
        // ("500pt") and the runtime ("72pt") finally agree.
        let bad = Config::detect_malformed_values(
            "font-size = 500\n\
             font-size = 0\n\
             font-size = -4\n\
             font-size = 72.5\n",
        );
        // 500, 0, -4 are all out of [5.0, 72.0]; 72.5 too.
        assert_eq!(bad.len(), 4, "all four out-of-range: {bad:?}");
        assert!(bad.iter().all(|b| b.contains("font-size")));
        // In-range values + bounds still pass cleanly.
        let ok = Config::detect_malformed_values(
            "font-size = 13\n\
             font-size = 5\n\
             font-size = 72\n\
             font-size = 13.5\n",
        );
        assert!(ok.is_empty(), "all in-range: {ok:?}");
        // Non-numeric still goes through the parse-fail path (cycle 70).
        let bad = Config::detect_malformed_values("font-size = abc\n");
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn detect_malformed_values_flags_palette_index_out_of_range() {
        // Cycle 124: documented as 0..=255 but the runtime apply path
        // only writes the 0..=15 ANSI palette. Flag 16+ so the user's
        // typo doesn't silently no-op (the diagnostic is on the same
        // surface as cycle 88's `ssh-host = name=` half-empty flag).
        let bad = Config::detect_malformed_values(
            "palette = 200=#ff0000\n\
             palette = 16=#abcdef\n\
             palette = 999=#cafe00\n",
        );
        assert_eq!(bad.len(), 3, "all three should be flagged: {bad:?}");
        assert!(bad.iter().all(|b| b.contains("palette")));
        // The 0..=15 range still parses cleanly.
        let ok = Config::detect_malformed_values(
            "palette = 0=#000000\n\
             palette = 4=#7aa2f7\n\
             palette = 15=#ffffff\n",
        );
        assert!(ok.is_empty(), "all in-range: {ok:?}");
        // Verify the runtime apply still works for the in-range case
        // (regression guard for the same field).
        let c = Config::parse_text("palette = 4=#ff0000");
        assert_eq!(c.theme.palette[4], Rgb::new(0xff, 0x00, 0x00));
    }

    #[test]
    fn detect_malformed_values_catches_bad_color_keys() {
        // Same trap as the numeric keys — `Rgb::parse(&value)` returns
        // None and the apply arm silently keeps the default. A user
        // writing `cursor-color = #not-a-color` saw a clean `--check-
        // config` while their color was being ignored. Now flagged.
        // (`Rgb::parse` does accept 3-char hex shorthand like `#bad`,
        // which expands to `#bbaadd` — that's intentional X11 behavior,
        // so the bad-value test uses values with no parseable form.)
        let bad = Config::detect_malformed_values(
            "background = #not-a-color\n\
             cursor-color = whatever\n\
             selection-foreground = oops\n\
             split-divider-color = ???\n\
             focused-split-color = nope\n\
             palette = junk\n",
        );
        assert_eq!(
            bad.len(),
            6,
            "all six bad colors should be flagged: {bad:?}"
        );
        assert!(bad.iter().any(|b| b.contains("background")));
        assert!(bad.iter().any(|b| b.contains("focused-split-color")));
        assert!(bad.iter().any(|b| b.contains("palette")));
        // Valid colors / palettes pass cleanly. Includes the X11 3-char
        // hex shorthand to pin that accepted form.
        let ok = Config::detect_malformed_values(
            "background = #1a1b26\n\
             cursor-color = #c0caf5\n\
             selection-foreground = red\n\
             focused-split-color = #00ff00\n\
             palette = 1=#ff0000\n\
             palette = 0=red\n\
             palette = 2=#bad\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
    }

    #[test]
    fn word_delimiters_default_empty_and_aliases() {
        // Default is empty so the engine uses its own default set —
        // means "no override," not "everything is a word".
        assert!(Config::default().word_delimiters.is_empty());
        // Canonical + two aliases — same field in different terminals'
        // configs. Value is taken verbatim after the `=` (with the usual
        // surrounding-whitespace trim).
        assert_eq!(
            Config::parse_text("word-delimiters = /").word_delimiters,
            "/"
        );
        assert_eq!(
            Config::parse_text("selection-word-chars = ,;").word_delimiters,
            ",;"
        );
        assert_eq!(
            Config::parse_text("semantic-escape-chars = ()[]{}").word_delimiters,
            "()[]{}"
        );
    }

    #[test]
    fn mouse_hide_while_typing_default_and_parse() {
        // Default is `true` — matches every modern terminal (Alacritty
        // `mouse.hide_when_typing`, kitty `hide_mouse_when_typing`,
        // WezTerm `hide_mouse_cursor_when_typing`).
        assert!(Config::default().mouse_hide_while_typing);
        assert!(!Config::parse_text("mouse-hide-while-typing = false").mouse_hide_while_typing);
        // Short `mouse-hide` alias also works.
        assert!(!Config::parse_text("mouse-hide = false").mouse_hide_while_typing);
    }

    #[test]
    fn unknown_keys_are_reported_not_fatal() {
        let (cfg, unknown) = Config::parse_collect(
            "font-size = 14\nfont-szie = 99\ntheme = TokyoNight Night\nbogus = x\n",
        );
        // Valid keys still applied.
        assert_eq!(cfg.font_size, 14.0);
        // Typo'd / unknown keys collected, sorted + deduped.
        assert_eq!(unknown, vec!["bogus".to_string(), "font-szie".to_string()]);
    }

    #[test]
    fn status_bar_parses_off_top_bottom_with_aliases() {
        // Cycle 295 drift guard. Default is Off (no row stolen from
        // the pane grid). Three explicit modes (off / top / bottom)
        // plus permissive aliases (`statusbar` no-dash, `true` / `on`
        // for bottom, `false` / `none` for off). Unknown values fall
        // back to Off so a future kettle adding a new mode doesn't
        // surprise-enable on an old config typo.
        assert_eq!(Config::default().status_bar, StatusBarMode::Off);
        assert_eq!(
            Config::parse_text("status-bar = top").status_bar,
            StatusBarMode::Top
        );
        assert_eq!(
            Config::parse_text("status-bar = bottom").status_bar,
            StatusBarMode::Bottom
        );
        assert_eq!(
            Config::parse_text("statusbar = bottom").status_bar,
            StatusBarMode::Bottom
        );
        assert_eq!(
            Config::parse_text("status-bar = true").status_bar,
            StatusBarMode::Bottom
        );
        assert_eq!(
            Config::parse_text("status-bar = off").status_bar,
            StatusBarMode::Off
        );
        assert_eq!(
            Config::parse_text("status-bar = none").status_bar,
            StatusBarMode::Off
        );
        // Unknown value → safe fallback (Off, not bottom).
        assert_eq!(
            Config::parse_text("status-bar = funky").status_bar,
            StatusBarMode::Off
        );
    }

    #[test]
    fn trigger_parses_pattern_and_repeats() {
        // Cycle 289 drift guard. `trigger = REGEX` accumulates into
        // `Config::triggers` with action defaulting to Urgency. The
        // whole value is the pattern — no in-band action separator
        // (pipe `|` is a regex metacharacter, so alternation patterns
        // like `(BUILD SUCCESSFUL|FAILED)` need to be passed through
        // intact). v1 only ships the Urgency action; future syntax
        // for multi-action would use a separate `trigger-action = …`
        // key or a non-regex-meta delimiter.
        let cfg = Config::parse_text(
            "trigger = error.*panic\n\
             trigger = (BUILD SUCCESSFUL|FAILED)\n\
             trigger = stack overflow\n",
        );
        assert_eq!(cfg.triggers.len(), 3);
        assert_eq!(cfg.triggers[0].pattern, "error.*panic");
        assert_eq!(cfg.triggers[0].action, TriggerAction::Urgency);
        // Regex alternation passes through intact — load-bearing.
        assert_eq!(cfg.triggers[1].pattern, "(BUILD SUCCESSFUL|FAILED)");
        assert_eq!(cfg.triggers[2].pattern, "stack overflow");
        // Default config has no triggers.
        assert!(Config::default().triggers.is_empty());
        // Empty / whitespace patterns are dropped at parse time so a
        // typo'd line doesn't fire on every byte.
        assert!(Config::parse_text("trigger =\n").triggers.is_empty());
        assert!(Config::parse_text("trigger =   \n").triggers.is_empty());
    }

    /// Cycle 611 drift guard. `menu-item = LABEL = CMD` accumulates
    /// `MenuItem` rows used by the chrome to extend the right-click
    /// context menu (Terminator parity, `custom_commands.py`). The
    /// FIRST `=` in the line splits key/value (consumed by the
    /// cycle-X line parser); the FIRST `=` of the value splits
    /// label/command. Subsequent `=` in the command are preserved
    /// (so `cmd = foo=bar` is a label "cmd" + command "foo=bar").
    /// Malformed lines (missing the value-side `=`, empty label,
    /// empty command) are silently dropped at parse time.
    #[test]
    fn menu_item_parses_label_and_command() {
        let cfg = Config::parse_text(
            "menu-item = Clear screen = clear\n\
             menu-item = Open editor = $EDITOR ~/.bashrc\n\
             # subsequent `=` in the command survive the split:\n\
             menu-item = Set FOO = export FOO=bar\n",
        );
        assert_eq!(cfg.menu_items.len(), 3);
        assert_eq!(cfg.menu_items[0].label, "Clear screen");
        assert_eq!(cfg.menu_items[0].command, "clear");
        assert_eq!(cfg.menu_items[1].label, "Open editor");
        assert_eq!(cfg.menu_items[1].command, "$EDITOR ~/.bashrc");
        assert_eq!(cfg.menu_items[2].label, "Set FOO");
        assert_eq!(cfg.menu_items[2].command, "export FOO=bar");
        // Default config has no entries.
        assert!(Config::default().menu_items.is_empty());
        // Malformed forms: silently dropped (the check-config malformed
        // diagnostic surfaces them; the parser doesn't materialize a row).
        assert!(
            Config::parse_text("menu-item = onlylabel\n")
                .menu_items
                .is_empty(),
            "missing the value-side `=` separator"
        );
        assert!(
            Config::parse_text("menu-item =  = cmd\n")
                .menu_items
                .is_empty(),
            "empty label"
        );
        assert!(
            Config::parse_text("menu-item = label = \n")
                .menu_items
                .is_empty(),
            "empty command"
        );
        // `menu_item` (underscore) is accepted as an alias of
        // `menu-item` (kebab) — same convention as the rest of the
        // grammar (cycle 175 underscore-vs-kebab cleanup).
        let alias_cfg = Config::parse_text("menu_item = test = ls\n");
        assert_eq!(alias_cfg.menu_items.len(), 1);
        assert_eq!(alias_cfg.menu_items[0].label, "test");
    }

    /// Cycle 611 drift guard for the check-config malformed-value
    /// surfacing. A `menu-item` line without a second `=` (or with
    /// empty label / command) should show up in the malformed list
    /// so the user sees the issue at `kettle --check-config` time
    /// rather than silently getting no menu row.
    ///
    /// Cycle 612: doc continues after a paragraph break to satisfy
    /// clippy's `doc_list_item_without_indent` lint that fires
    /// when consecutive `///` lines after a single `#[test]` look
    /// like a list continuation.
    #[test]
    fn command_notify_threshold_parses_and_clamps() {
        // body intentionally not changed by cycle 613; the test
        // continues below.
        assert_eq!(Config::default().command_notify_threshold_ms, 5_000);
        for alias in [
            "command-notify-threshold-ms",
            "command-notify-threshold",
            "command_notify_threshold_ms",
            "command_notify_threshold",
        ] {
            let cfg = Config::parse_text(&format!("{alias} = 12000\n"));
            assert_eq!(cfg.command_notify_threshold_ms, 12_000, "alias {alias}");
        }
        let cfg = Config::parse_text("command-notify-threshold-ms = 0\n");
        assert_eq!(cfg.command_notify_threshold_ms, 0);
        let cfg = Config::parse_text("command-notify-threshold-ms = 999999999\n");
        assert_eq!(cfg.command_notify_threshold_ms, 86_400_000);
    }

    /// Cycle 613 drift guard. `force-no-bell = true` overrides the
    /// `bell` config key — Terminator-parity hard-off for users
    /// who want to silence every bell flavor with one key.
    /// Pre-cycle-613 the key parsed but was a documented no-op.
    #[test]
    fn force_no_bell_overrides_bell_mode_to_off() {
        // `force-no-bell = true` alone → BellMode::Off.
        let cfg = Config::parse_text("force-no-bell = true\n");
        assert!(cfg.force_no_bell);
        assert_eq!(cfg.bell, BellMode::Off);
        // `force-no-bell = true` AFTER `bell = both` still wins
        // (the override is post-process, regardless of line order).
        let cfg = Config::parse_text(
            "bell = both\n\
             force-no-bell = true\n",
        );
        assert_eq!(cfg.bell, BellMode::Off);
        // The reverse order: `bell = both` after force-no-bell.
        let cfg = Config::parse_text(
            "force-no-bell = true\n\
             bell = both\n",
        );
        assert_eq!(cfg.bell, BellMode::Off);
        // `force-no-bell = false` is the default and leaves bell alone.
        let cfg = Config::parse_text("bell = visual\n");
        assert!(!cfg.force_no_bell);
        assert_eq!(cfg.bell, BellMode::Visual);
    }

    /// Cycle 616 drift guard. `light-theme` / `dark-theme` config
    /// keys store the *canonical* bundled name when the user-typed
    /// value matches one (so the toggle action can do exact-name
    /// matching against `cfg.theme_name`); both kebab + underscore
    /// spellings are accepted; an empty / whitespace-only value is
    /// ignored (so commenting-out via `light-theme = ` doesn't
    /// stick a stray empty string).
    #[test]
    fn search_case_sensitive_parses_terminator_and_named_forms() {
        use SearchCaseSensitivity::*;
        // Default is Smart (ripgrep semantics) — kettle's pre-617 behavior.
        assert_eq!(Config::default().search_case_sensitive, Smart);
        // Named modes (kettle convention).
        for (input, want) in [
            ("search-case-sensitive = smart", Smart),
            ("search-case-sensitive = auto", Smart),
            ("search-case-sensitive = always", Always),
            ("search-case-sensitive = sensitive", Always),
            ("search-case-sensitive = never", Never),
            ("search-case-sensitive = insensitive", Never),
            // Terminator bool form (config.py default `case_sensitive = True`).
            ("case-sensitive = true", Always),
            ("case-sensitive = false", Never),
            // Underscore spelling (Terminator convention).
            ("case_sensitive = true", Always),
            ("case_sensitive = false", Never),
            // search-case-sensitive bool form (kettle spelling).
            ("search-case-sensitive = true", Always),
            ("search-case-sensitive = false", Never),
        ] {
            let cfg = Config::parse_text(&format!("{input}\n"));
            assert_eq!(
                cfg.search_case_sensitive, want,
                "input {input:?} should produce {want:?}"
            );
        }
        // Garbage value leaves the field at the default.
        let cfg = Config::parse_text("search-case-sensitive = banana\n");
        assert_eq!(cfg.search_case_sensitive, Smart);
    }

    /// Cycle 628 drift guard. Terminator's `tab_position` is kettle's
    /// `tab-bar-position`. A Terminator config that says
    /// `tab_position = bottom` (or `= hidden`) should now bind cleanly.
    #[test]
    fn tab_position_alias_parses() {
        // Terminator-spelled value applies the expected mode.
        let cfg = Config::parse_text("tab-position = bottom\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Bottom);
        // Underscore spelling.
        let cfg = Config::parse_text("tab_position = bottom\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Bottom);
        // `hidden` value flips the visibility (not the position).
        let cfg = Config::parse_text("tab-position = hidden\n");
        assert_eq!(cfg.tab_bar, TabBarMode::Off);
        // Top default preserved when not set.
        let cfg = Config::parse_text("\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Top);
        // left/right accepted (parser-side) but log::warn'd at runtime.
        // detect_malformed_values should NOT flag these (they're known
        // values even if unimplemented at render time).
        let bad = Config::detect_malformed_values("tab-position = left\n");
        assert!(
            !bad.iter().any(|m| m.contains("tab-position")),
            "tab-position = left should be accepted at parse time (got: {bad:?})"
        );
        let bad = Config::detect_malformed_values("tab_position = right\n");
        assert!(
            !bad.iter().any(|m| m.contains("tab_position")),
            "tab_position = right should be accepted at parse time (got: {bad:?})"
        );
    }

    /// Cycle 626 drift guard. Terminator's `audible_bell` doesn't
    /// map to anything kettle ships (no audio surface yet), so the
    /// parser accepts the key without setting anything. The drift
    /// guard locks in two outcomes:
    ///   - the key is recognized (no unknown-key warning would
    ///     appear in cycle-179's user-doc-drift surface), and
    ///   - the rest of the config is unaffected (no spillover
    ///     into the unified `bell` mode).
    #[test]
    fn audible_bell_parses_as_documented_noop() {
        // Default bell mode is Both; audible-bell shouldn't change it.
        let cfg = Config::parse_text("audible-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Both);
        // Underscore spelling also accepted.
        let cfg = Config::parse_text("audible_bell = false\n");
        assert_eq!(cfg.bell, BellMode::Both);
        // Combined with the bell key, the canonical `bell =` arm
        // wins (audible-bell is a documented no-op).
        let cfg = Config::parse_text("bell = visual\naudible-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Visual);
        // The cycle-296 unknown-key surface should NOT flag this
        // key. (We test by asking detect_malformed_values for the
        // diagnostic list — `audible-bell` should not appear.)
        let bad = Config::detect_malformed_values("audible-bell = true\n");
        assert!(
            !bad.iter().any(|m| m.contains("audible-bell")),
            "audible-bell shouldn't trip --check-config (got: {bad:?})"
        );
    }

    /// Cycle 623 drift guard. Terminator-spelling aliases for kettle's
    /// canonical color / cursor / fullscreen keys. A user copying a
    /// Terminator config should bind these without rename.
    #[test]
    fn terminator_color_cursor_aliases_parse() {
        // Background + foreground aliases.
        let cfg = Config::parse_text(
            "background-color = #112233\n\
             foreground-color = #ddeeff\n",
        );
        assert_eq!(
            cfg.theme.background,
            Rgb {
                r: 0x11,
                g: 0x22,
                b: 0x33
            }
        );
        assert_eq!(
            cfg.theme.foreground,
            Rgb {
                r: 0xdd,
                g: 0xee,
                b: 0xff
            }
        );
        // Underscore form also works.
        let cfg = Config::parse_text(
            "background_color = #001100\n\
             foreground_color = #ffeedd\n",
        );
        assert_eq!(
            cfg.theme.background,
            Rgb {
                r: 0x00,
                g: 0x11,
                b: 0x00
            }
        );
        assert_eq!(
            cfg.theme.foreground,
            Rgb {
                r: 0xff,
                g: 0xee,
                b: 0xdd
            }
        );
        // cursor-shape (Terminator) maps to cursor_style (kettle).
        let cfg = Config::parse_text("cursor-shape = ibeam\n");
        assert_eq!(cfg.cursor_style, CursorStyle::Bar);
        let cfg = Config::parse_text("cursor_shape = underline\n");
        assert_eq!(cfg.cursor_style, CursorStyle::Underline);
        let cfg = Config::parse_text("cursor-shape = block\n");
        assert_eq!(cfg.cursor_style, CursorStyle::Block);
        // cursor-blink (Terminator) maps to cursor_blink (kettle).
        let cfg = Config::parse_text("cursor-blink = false\n");
        assert!(!cfg.cursor_blink);
        let cfg = Config::parse_text("cursor_blink = true\n");
        assert!(cfg.cursor_blink);
        // full-screen = true → WindowState::Fullscreen.
        let cfg = Config::parse_text("full-screen = true\n");
        assert_eq!(cfg.window_state, WindowState::Fullscreen);
        let cfg = Config::parse_text("full_screen = true\n");
        assert_eq!(cfg.window_state, WindowState::Fullscreen);
        // full-screen = false is a no-op (doesn't override existing).
        let cfg = Config::parse_text("window-state = maximise\nfull-screen = false\n");
        assert_eq!(cfg.window_state, WindowState::Maximise);
    }

    /// Cycle 622 drift guard. `parse_trigger_with_command` is the
    /// pure helper that splits a `trigger = REGEX :: CMD ARGS`
    /// value. Verify:
    ///   - `::` separator parsed (pattern + argv both non-empty)
    ///   - whitespace-split argv preserves order + collapses runs
    ///   - missing separator → None (caller falls back to Urgency)
    ///   - empty pattern after split → None (sentinel for malformed)
    ///   - empty argv after split → None
    #[test]
    fn parse_trigger_with_command_splits_on_double_colon() {
        use super::parse_trigger_with_command;
        // Happy path.
        let (pat, cmd) = parse_trigger_with_command("error.*panic :: notify-send oops").unwrap();
        assert_eq!(pat, "error.*panic");
        assert_eq!(cmd, vec!["notify-send", "oops"]);
        // Multiple-argument argv, tabs + multispace collapsed.
        let (pat, cmd) =
            parse_trigger_with_command("warn\\b ::   /usr/bin/say  -v Alex  warning").unwrap();
        assert_eq!(pat, "warn\\b");
        assert_eq!(cmd, vec!["/usr/bin/say", "-v", "Alex", "warning"]);
        // No separator → None (caller falls back to Urgency).
        assert!(parse_trigger_with_command("just a regex").is_none());
        // Empty pattern side → None.
        assert!(parse_trigger_with_command(" :: notify-send oops").is_none());
        // Empty argv side → None.
        assert!(parse_trigger_with_command("pattern ::").is_none());
        // Both empty → None.
        assert!(parse_trigger_with_command("::").is_none());
        // IPv6-like pattern with a single `:` does NOT split (sep
        // is `::` specifically). This guards against a footgun
        // where a user-typed IPv4-vs-IPv6 alternation pattern
        // would accidentally activate the cmd path.
        let (pat, cmd) =
            parse_trigger_with_command("from 2001:db8::1 :: logger ipv6-seen").unwrap();
        assert_eq!(pat, "from 2001:db8");
        // `::` is the split's stop sigil — `split_once` consumes only
        // the first occurrence; remaining `::` after the first match
        // survive as plain whitespace-separated argv tokens.
        assert_eq!(cmd, vec!["1", "::", "logger", "ipv6-seen"]);
        // ^ Note: the user-typed pattern DOES contain `::` so it
        // does split. This is the documented limitation of the
        // syntax; a v2 escape like `\::` could be added but in
        // practice users who need bare `::` in a regex can write
        // `:[:]` or `\x3a\x3a` to dodge the parser.
    }

    /// Cycle 619 drift guard. Terminator splits the bell into two
    /// orthogonal bools (`visible_bell`, `urgent_bell`); kettle uses
    /// a unified `bell = off | visual | attention | both`. The
    /// compatibility parser arms compose the two Terminator-style
    /// bools into the right unified mode. Idempotent; order-independent.
    #[test]
    fn visible_bell_and_urgent_bell_compose_into_bell_mode() {
        // Both true → Both.
        let cfg = Config::parse_text("visible-bell = true\nurgent-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Both);
        // Only visible.
        let cfg = Config::parse_text("visible-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Visual);
        // Only urgent.
        let cfg = Config::parse_text("urgent-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Attention);
        // Underscore spelling works too (Terminator convention).
        let cfg = Config::parse_text("visible_bell = true\nurgent_bell = true\n");
        assert_eq!(cfg.bell, BellMode::Both);
        // false values leave the bell alone (idempotent default Off).
        let cfg = Config::parse_text("visible-bell = false\nurgent-bell = false\n");
        assert_eq!(cfg.bell, BellMode::Off);
        // Precedence rule (cycle 619): an explicit canonical
        // `bell = <mode>` wins over Terminator-spelled compat
        // aliases REGARDLESS of file order. Mixing both spellings
        // is the rare hybrid-config case; the canonical key takes
        // precedence so the user gets the explicit kettle mode.
        let cfg = Config::parse_text("bell = visual\nurgent-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Visual);
        let cfg = Config::parse_text("visible-bell = true\nbell = attention\n");
        assert_eq!(cfg.bell, BellMode::Attention);
        // force-no-bell still overrides everything (cycle 613).
        let cfg = Config::parse_text(
            "visible-bell = true\n\
             urgent-bell = true\n\
             force-no-bell = true\n",
        );
        assert_eq!(cfg.bell, BellMode::Off);
    }

    /// Cycle 619 drift guard. The `BellMode::compose` helper is the
    /// pure semantics behind the urgent/visible-bell arms. Round-trip
    /// every input pair, and verify idempotency.
    #[test]
    fn bellmode_compose_is_idempotent_and_or_like() {
        use BellMode::*;
        let modes = [Off, Visual, Attention, Both];
        // Idempotent: compose(x, x) == x.
        for m in modes {
            assert_eq!(m.compose(m), m, "{m:?}.compose({m:?})");
        }
        // Identity: compose(x, Off) == x; compose(Off, x) == x.
        for m in modes {
            assert_eq!(m.compose(Off), m);
            assert_eq!(Off.compose(m), m);
        }
        // OR-like: Visual + Attention = Both (both directions).
        assert_eq!(Visual.compose(Attention), Both);
        assert_eq!(Attention.compose(Visual), Both);
        // Both absorbs everything.
        for m in modes {
            assert_eq!(Both.compose(m), Both);
            assert_eq!(m.compose(Both), Both);
        }
    }

    /// Cycle 618 drift guard. `profile_name_from_path` is the inverse of
    /// `path_for_profile`: it should recover the bare profile name from
    /// a `<config-dir>/profiles/<name>.config` path, and return `None`
    /// for paths shaped any other way (default config, --config FILE
    /// outside `profiles/`, missing `.config` suffix, etc.). Used by
    /// `Action::NextProfile` to compute the current profile from
    /// `App::config_path` so the cycle starts at the right index.
    #[test]
    fn profile_name_from_path_inverts_path_for_profile() {
        use std::path::PathBuf;
        // Round-trip through path_for_profile.
        if let Some(p) = Config::path_for_profile("dev") {
            assert_eq!(Config::profile_name_from_path(&p).as_deref(), Some("dev"));
        }
        // Plain "kettle/config" (the default path) is NOT inside profiles/.
        let p = PathBuf::from("/home/u/.config/kettle/config");
        assert!(Config::profile_name_from_path(&p).is_none());
        // A path with a different parent dir is rejected.
        let p = PathBuf::from("/etc/kettle.d/dev.config");
        assert!(Config::profile_name_from_path(&p).is_none());
        // Inside profiles/ but missing the .config suffix is rejected.
        let p = PathBuf::from("/home/u/.config/kettle/profiles/dev");
        assert!(Config::profile_name_from_path(&p).is_none());
        // Verbatim shape: parent=profiles, filename ending in .config.
        let p = PathBuf::from("/anywhere/profiles/something.config");
        assert_eq!(
            Config::profile_name_from_path(&p).as_deref(),
            Some("something")
        );
    }

    #[test]
    fn light_and_dark_theme_parse_canonical_and_aliases() {
        // Lowercased input for a bundled theme → stored *canonical*
        // (so the runtime toggle's case-sensitive equality match
        // against `cfg.theme_name` works).
        let cfg = Config::parse_text(
            "light-theme = tokyonight day\n\
             dark-theme = tokyonight night\n",
        );
        assert_eq!(cfg.light_theme, "TokyoNight Day");
        assert_eq!(cfg.dark_theme, "TokyoNight Night");
        // Underscore spelling (Terminator convention) works too.
        let cfg = Config::parse_text(
            "light_theme = TokyoNight Day\n\
             dark_theme = TokyoNight Night\n",
        );
        assert_eq!(cfg.light_theme, "TokyoNight Day");
        assert_eq!(cfg.dark_theme, "TokyoNight Night");
        // Unknown user-typed theme name stored verbatim (trimmed).
        // (kettle's runtime fallback in `Theme::by_name` will land
        // on the default theme, but storing the user's literal
        // string preserves the surface for --check-config diagnostics.)
        let cfg = Config::parse_text("light-theme =   my-custom-fork  \n");
        assert_eq!(cfg.light_theme, "my-custom-fork");
        // Empty/whitespace-only value leaves the field at default
        // (empty string), so a future `light-theme = ` doesn't
        // override a previously set value to nothing.
        let cfg = Config::parse_text(
            "light-theme = TokyoNight Day\n\
             light-theme =   \n",
        );
        assert_eq!(cfg.light_theme, "TokyoNight Day");
    }

    /// Cycle 611 drift guard for the --check-config malformed-value
    /// surface. Missing label-side `=`, empty label, or empty
    /// command should each show up in the diagnostic list so the
    /// user sees the issue at `kettle --check-config` time rather
    /// than silently getting no menu row.
    #[test]
    fn detect_malformed_values_flags_invalid_menu_item() {
        let cases = [
            "menu-item = no-separator",
            "menu-item =  = cmd",
            "menu-item = label = ",
        ];
        for case in cases {
            let bad = Config::detect_malformed_values(&format!("{case}\n"));
            assert!(
                bad.iter().any(|m| m.contains("menu-item")),
                "expected malformed-value diagnostic for {case:?}, got {bad:?}"
            );
        }
        // Well-formed line: no diagnostic.
        assert!(Config::detect_malformed_values("menu-item = ok = ls\n").is_empty());
    }

    #[test]
    fn detect_malformed_values_flags_invalid_trigger_regex() {
        // Cycle 309 drift guard. A malformed regex pattern like
        // `trigger = [unclosed` parses (the config layer stores it
        // as a plain string), `--check-config` USED to report OK,
        // then at runtime `compile_triggers` failed `Regex::new`
        // and the trigger silently never fired (only a log::warn
        // the user usually doesn't see). Now: surface invalid
        // regex at check-config time.
        let bad = Config::detect_malformed_values(
            "trigger = [unclosed\n\
             trigger = (mismatched\n\
             trigger = good.*pattern\n",
        );
        // Both bad lines surface; the good one doesn't.
        assert_eq!(
            bad.iter().filter(|b| b.contains("trigger")).count(),
            2,
            "expected 2 malformed-trigger entries, got: {bad:?}"
        );
        // Valid alternation patterns must NOT be flagged (load-
        // bearing — the cycle-289 docs explicitly tell users to
        // write `(BUILD SUCCESSFUL|FAILED)`).
        let ok = Config::detect_malformed_values("trigger = (BUILD SUCCESSFUL|FAILED)\n");
        assert!(
            ok.iter().all(|b| !b.contains("trigger")),
            "valid alternation flagged as malformed: {ok:?}"
        );
    }
}

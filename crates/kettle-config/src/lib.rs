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

use std::io::Read as _;

pub use color::Rgb;
pub use keybinds::{Action, Bindings, Key, Mods, Trigger};
pub use theme::Theme;

/// Practical stand-in for "infinite" scrollback: ~10M lines (keeps memory
/// bounded while never realistically clipping history).
pub const INFINITE_SCROLLBACK: usize = 10_000_000;

/// Per-pane scrollback byte budget. `0` disables the byte cap and leaves the
/// line-count `scrollback` key as the only history limit.
pub const DEFAULT_SCROLLBACK_BYTES: usize = 10_000_000;
pub const MAX_SCROLLBACK_BYTES: usize = 1 << 40;

/// Startup window geometry is specified in terminal cells, not pixels. Bounds
/// keep accidental huge/silly configs diagnostic instead of silently spawning
/// an unusable window.
pub const WINDOW_WIDTH_MIN: u32 = 20;
pub const WINDOW_WIDTH_MAX: u32 = 400;
pub const WINDOW_HEIGHT_MIN: u32 = 8;
pub const WINDOW_HEIGHT_MAX: u32 = 200;

/// Parse the standard true/false aliases. This exists because every
/// previous boolean config used `e.value != "false"`,
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

pub(crate) fn parse_byte_size(s: &str) -> Option<usize> {
    let value = s.trim().replace('_', "");
    if value.is_empty() {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    let (digits, multiplier): (&str, u128) = if let Some(v) = lower.strip_suffix("kib") {
        (v, 1024)
    } else if let Some(v) = lower.strip_suffix("kb") {
        (v, 1000)
    } else if let Some(v) = lower.strip_suffix('k') {
        (v, 1000)
    } else if let Some(v) = lower.strip_suffix("mib") {
        (v, 1024 * 1024)
    } else if let Some(v) = lower.strip_suffix("mb") {
        (v, 1000 * 1000)
    } else if let Some(v) = lower.strip_suffix('m') {
        (v, 1000 * 1000)
    } else if let Some(v) = lower.strip_suffix("gib") {
        (v, 1024 * 1024 * 1024)
    } else if let Some(v) = lower.strip_suffix("gb") {
        (v, 1000 * 1000 * 1000)
    } else if let Some(v) = lower.strip_suffix('g') {
        (v, 1000 * 1000 * 1000)
    } else {
        (lower.as_str(), 1)
    };
    let n = digits.parse::<u128>().ok()?;
    let bytes = n.checked_mul(multiplier)?;
    usize::try_from(bytes).ok()
}

/// Parse a `u32` written either as hex (`0x10de`, `10DE`) or decimal (`4318`).
/// Used for the GPU `gpu-vendor-id` / `gpu-device-id` pins, which the in-app
/// picker writes in hex (matching how PCI ids are conventionally displayed) but
/// which a hand-edited config may give in either form. `None` on garbage.
pub(crate) fn parse_hex_or_dec_u32(s: &str) -> Option<u32> {
    let t = s.trim();
    if let Some(hex) = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .or_else(|| t.strip_prefix("#"))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    // Bare value: try decimal first, then hex (so `10de` still resolves).
    t.parse::<u32>()
        .ok()
        .or_else(|| u32::from_str_radix(t, 16).ok())
}

/// Parse a portable `KEY=VALUE` environment assignment. The value may be empty
/// and may itself contain `=`; the name is intentionally restricted to the
/// POSIX/Windows-compatible subset so a checked config behaves the same on
/// Linux, Windows, and through WSLENV.
pub(crate) fn parse_env_assignment(s: &str) -> Option<(String, String)> {
    let (name, value) = s.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return None;
    }
    if !chars.all(|c| c == '_' || c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((name.to_string(), value.to_string()))
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

/// Terminator parity (`plugins/auto_theme.py`, phase 1
/// of [`TERMINATOR-AUTO-THEME-DESIGN.md`](docs/TERMINATOR-AUTO-THEME-DESIGN.md)):
/// theme-mode policy.
///
/// - `Explicit` — use the `theme = …` value as kettle has always
///   done. Default; unchanged from kettle's original explicit-theme behavior.
/// - `Light` — always use `light-theme`.
/// - `Dark` — always use `dark-theme`.
/// - `Auto` — follow the OS dark-mode preference when winit reports
///   one; if `theme-schedule` is configured, the schedule owns the
///   light/dark switch instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeMode {
    #[default]
    Explicit,
    Light,
    Dark,
    Auto,
}

/// Terminator parity (terminatorlib/config.py:117
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
    /// Compose two bell flavors (used by the Terminator-
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

/// Kettle's modified-Enter fallback before keyboard-protocol negotiation.
///
/// Xterm levels remain application-owned so enabling the fallback cannot turn
/// Ctrl+I into a distinct sequence for clients which still expect Tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ModifyOtherKeysMode {
    /// Preserve modified Enter until an application negotiates a protocol.
    #[default]
    Enter,
    /// Do not add Kettle's fallback; negotiated protocols are still honored.
    Off,
}

impl ModifyOtherKeysMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Off => "off",
        }
    }
}

/// Policy for pasting OS file references — a file copied in the file manager
/// (`CF_HDROP` on Windows, `text/uri-list` elsewhere) — into the focused pane
/// as a shell-quoted path. `On` (default) pastes the path(s); `Off` disables
/// the clipboard file-paste branch so a copied file behaves as before. Explicit
/// drag-and-drop still pastes a path regardless of this policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteFiles {
    /// Ignore a clipboard file list (only text is pasted).
    Off,
    /// Paste clipboard file path(s) as shell-quoted text (default).
    On,
}

impl PasteFiles {
    /// Should a clipboard file list be pasted as path text?
    pub fn enabled(self) -> bool {
        matches!(self, PasteFiles::On)
    }
}

/// Policy for pasting a clipboard BITMAP — a screenshot from Win+Shift+S, the
/// Snipping Tool, macOS Cmd+Shift+4, GNOME Screenshot — into the focused pane.
/// Unlike a copied file, a captured screenshot puts raw pixels on the clipboard
/// with no file and no text behind it, so there is no path to quote until one is
/// materialized. `On` (default) writes the bitmap to a temporary PNG and pastes
/// that path; `Off` disables the branch, leaving a bitmap clipboard to fall
/// through to the (usually empty) text paste.
///
/// Distinct from [`PasteFiles`] because the privacy profile differs: pasting a
/// file only references bytes the user already stored, whereas this WRITES
/// captured screen content to disk. The files are owner-only, bounded, and
/// deleted when kettle exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteImages {
    /// Ignore a clipboard bitmap.
    Off,
    /// Write it to a temporary PNG and paste that path (default).
    On,
}

impl PasteImages {
    /// Should a clipboard bitmap be materialized and pasted as a path?
    pub fn enabled(self) -> bool {
        matches!(self, PasteImages::On)
    }
}

/// Whether the GUI session recorder is armed at launch (`record = on`). `Off`
/// (the default) records nothing; `On` starts an asciicast recording for the
/// window session, written into the configured `record-dir`. Recording captures
/// on-screen output verbatim — a terminal cannot tell a secret from normal
/// output — so review a trace before sharing it. The one-shot `--record` /
/// `--record-dir` launch flags override this policy for a single launch. See
/// docs/RECORDING.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordMode {
    /// Do not record automatically (default).
    #[default]
    Off,
    /// Record the session to the configured recording directory.
    On,
}

impl RecordMode {
    /// Should recording start automatically at launch?
    pub fn enabled(self) -> bool {
        matches!(self, RecordMode::On)
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
///
/// Phase 1 of [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](
/// docs/TERMINATOR-VERTICAL-TABS-DESIGN.md)
/// added `Left` and `Right` variants. The parser already accepted
/// the values from earlier work but routed them to a
/// `log::warn` + fallback to `Top`. Now they store the user's
/// chosen orientation; the render-layer change to actually draw
/// the strip vertically lands in later phases of the vertical-
/// tabs design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TabBarPos {
    Top,
    Bottom,
    Left,
    Right,
}

impl TabBarPos {
    /// Is this a vertical-strip orientation?
    /// Helper for the upcoming `App::content_rect` branch + the
    /// `paint_tab_bar` orientation dispatch.
    pub fn is_vertical(self) -> bool {
        matches!(self, TabBarPos::Left | TabBarPos::Right)
    }
}

/// iTerm2 / kitty parity: status-bar position. A thin
/// strip showing HH:MM:SS · theme · focused pane title. Disabled by
/// default — turning it on subtracts one row from each pane's grid,
/// so chatty users with 80x24 budgets stay in control. A future update
/// adds CPU / MEM widgets via `sysinfo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StatusBarMode {
    #[default]
    Off,
    Top,
    Bottom,
}

/// Terminator parity (terminatorlib/config.py:118
/// `background_type`): background style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundType {
    /// Solid color (Terminator + kettle default).
    #[default]
    Solid,
    /// Use the `background_image` file.
    Image,
    /// Procedural GPU starfield (v2.24.0): a slow forward-flight field of
    /// soft-glowing, subtly-colored stars rendered by a WGSL fragment shader —
    /// true-color, perfectly looping, ~zero memory (no decoded frames). Needs no
    /// `background_image`. v2.24.1: a FIXED built-in example — the look (slow
    /// drift, center-invisible cubic fade-in) is baked into the shader, not
    /// config-tunable (the `starfield-speed` / `-density` / `-glow` knobs were
    /// removed; an old config still carrying them just warns "unknown key").
    Starfield,
    /// Transparent (uses `background_darkness` to dim).
    Transparent,
}

/// `text-renderer` (v2.25.0): how pane (terminal grid) text is rasterized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextRenderer {
    /// Cell-locked: every glyph is pinned to its grid cell (`col * cell_w`), the
    /// way Alacritty / kitty / WezTerm / Ghostty render. Fixes fallback-glyph
    /// drift — em-dashes, middle-dots, smart quotes, Nerd icons, CJK and
    /// ligature clusters whose advance ≠ the cell width used to shift the rest
    /// of a row off the grid that selection highlights, the block cursor and
    /// mouse hit-testing assume ("misaligned text" / "selection off by one
    /// letter"). The default.
    #[default]
    Grid,
    /// Legacy: the pre-2.25.0 continuous glyphon layout (each row shaped as one
    /// advance-positioned run). A rollback escape hatch kept for one release in
    /// case a font/emoji/ligature regression surfaces; slated for removal.
    Legacy,
}

/// `background-animation`: how an ANIMATED background (a `Starfield`, or an
/// animated GIF / APNG / animated WebP `background-image`) plays. Unlike
/// Ghostty's custom GLSL shaders — which pin the GPU to a high frame rate even
/// when idle — kettle advances on the media's own timestamps (or the starfield's
/// ~10 fps cap) and freezes when the window is minimized or fully occluded, so a
/// hidden window costs nothing. A still image ignores this entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundAnimation {
    /// Always animate, even when the window is unfocused (the v2.24.0 default —
    /// a wallpaper that only moves while focused felt broken). Still freezes
    /// when the window is minimized or occluded (it can't be seen).
    #[default]
    Always,
    /// Animate only while the window is focused; freeze (zero idle cost) when it
    /// isn't. The battery-friendly choice.
    WhenFocused,
    /// Never animate — freeze on the first frame (the pre-v2.21.x behavior).
    Off,
}

/// `chrome-background`: the fill color of the window chrome strips (tab bar,
/// status bar, new-tab button) **when a `background-image` is in use**. Without
/// an image, chrome always uses the theme as before — this only governs how the
/// chrome reads against a wallpaper. v2.23.0 already draws the chrome opaquely
/// over the wallpaper (so the animation no longer bleeds through); this picks
/// what that opaque color is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChromeBackground {
    /// Use the theme's chrome color (`palette[8]`) — the default, matches the
    /// no-wallpaper look.
    #[default]
    Theme,
    /// Sample the wallpaper's average color, dimmed for text contrast, so the
    /// chrome feels "inspired by" the background. Recomputed per frame change.
    Auto,
    /// Solid black.
    Black,
    /// Solid white.
    White,
}

/// Terminator plugin parity: Lua
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

/// Terminator parity (terminatorlib/config.py:73
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

/// Terminator parity (terminatorlib/config.py:75
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

/// `gpu-power-preference`: which GPU wgpu should request the adapter from.
/// Default is `Auto`: let wgpu / the platform pick the adapter unless the user
/// pins a specific GPU. This is the least surprising cross-platform policy:
/// single-GPU machines show their only adapter, hybrid laptops avoid claiming a
/// discrete GPU is required, and users can still opt into `High` for dedicated
/// GPU headroom or `Low` for integrated/battery-friendly startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuPowerPreference {
    /// Prefer the low-power (typically integrated) adapter — fastest cold start
    /// on a dual-GPU laptop.
    Low,
    /// Prefer the high-performance adapter. On hybrid laptops this usually
    /// means the discrete/dedicated GPU; on single-GPU machines it may resolve
    /// to the only integrated adapter.
    High,
    /// No preference; let wgpu pick.
    #[default]
    Auto,
}

/// `gpu-backend` (v2.23.0): which wgpu graphics backend to request. `Auto`
/// uses Kettle's deterministic native order (DX12 on Windows, Metal on macOS,
/// Vulkan elsewhere). An explicit backend applies independently of a physical
/// GPU pin. If it is unavailable, Kettle logs the fallback and uses native
/// order so a stale cross-platform config cannot prevent startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuBackend {
    /// Use Kettle's deterministic native-first backend order.
    #[default]
    Auto,
    /// Direct3D 12 (Windows).
    Dx12,
    /// Vulkan (Windows / Linux).
    Vulkan,
    /// Metal (macOS).
    Metal,
    /// OpenGL / GLES (fallback).
    Gl,
}

/// Terminator parity (terminatorlib/config.py:107
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

/// Terminator parity (terminatorlib/config.py:108
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

/// Terminator parity (terminatorlib/config.py:71
/// `broadcast_default`): default broadcast scope when none set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BroadcastDefault {
    /// Broadcast to every pane in every window (Terminator's
    /// most-permissive mode).
    All,
    /// Broadcast within a per-tab group (kettle default — matches
    /// kettle's per-tab broadcast model).
    #[default]
    Group,
    /// Don't broadcast (each pane gets its own input).
    Off,
}

/// Terminator parity (terminatorlib/config.py:118
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

/// Whether kettle exposes its agent control server,
/// and at what privilege. OFF by default — the server is a local-IPC surface
/// that lets another process read the screen and (in `Full`) drive the panes,
/// so it must be opt-in. See docs/AGENT.md for the threat model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentServer {
    /// No control server is started (default).
    #[default]
    Off,
    /// Read-only methods only (`get_state`, `list_*`, `read_screen`,
    /// `subscribe`). Mutating methods are rejected with `read_only`.
    ReadOnly,
    /// All methods, including `send_text` and `run_command`.
    Full,
}

impl AgentServer {
    /// Whether any server should be started.
    pub fn is_enabled(self) -> bool {
        !matches!(self, AgentServer::Off)
    }
    /// Whether mutating methods are permitted.
    pub fn allows_mutation(self) -> bool {
        matches!(self, AgentServer::Full)
    }
}

/// Terminator parity (terminatorlib/config.py:79
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

/// Phase 4 of [`TERMINATOR-AUTO-THEME-DESIGN.md`](
/// docs/TERMINATOR-AUTO-THEME-DESIGN.md): theme-schedule policy
/// for the no-geolocation case.
///
/// `Clock { dark_at, light_at }` flips between dark and light at
/// the wall-clock times (local). E.g.
/// `theme-schedule = 18:00 dark, 06:00 light` reads as: dark from
/// 18:00 to 06:00 the next day, light from 06:00 to 18:00.
///
/// The sunrise/sunset variant (lat/long-driven) is a phase 5
/// follow-up — needs a small solar-position computation that the
/// `sunrise` crate handles, plus explicit-lat/long config keys.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ThemeSchedule {
    Clock {
        dark_at: (u8, u8),
        light_at: (u8, u8),
    },
    /// Phase 6 of the auto-theme design: switch theme
    /// at sunrise (→ light) and sunset (→ dark) computed from
    /// configured lat/long. Privacy posture: kettle does NOT
    /// do IP-geo or OS-location lookups — `theme-schedule-lat`
    /// and `theme-schedule-long` are supplied explicitly.
    /// Solar-position math lands in phase 7.
    SunriseSunset { lat: f64, long: f64 },
}

/// Pure decision for `ThemeSchedule::Clock`.
///
/// Given the current wall-clock time (hour, minute), returns:
///   - `Some(true)` — should be dark right now
///   - `Some(false)` — should be light right now
///
/// The schedule "dark at H1:M1, light at H2:M2" means: dark when
/// current time is in `[H1:M1, H2:M2)` (where the range can wrap
/// past midnight; e.g. `18:00 → 06:00` wraps).
///
/// Pure — entirely a function of `now_hm` + the schedule. Drift
/// guard `schedule_decision_clock_walks_boundaries` covers the
/// 4 representative shapes (normal day, wrap past midnight,
/// exactly-on-boundary, dawn = dusk degenerate).
pub fn schedule_decision_clock(now_hm: (u8, u8), schedule: ThemeSchedule) -> bool {
    // The SunriseSunset variant has its own decision helper
    // (phase 7); this helper only handles Clock. Defensive
    // default-to-light for non-Clock to keep the caller pure.
    let ThemeSchedule::Clock { dark_at, light_at } = schedule else {
        return false;
    };
    let now = (now_hm.0 as u32) * 60 + (now_hm.1 as u32);
    let dark = (dark_at.0 as u32) * 60 + (dark_at.1 as u32);
    let light = (light_at.0 as u32) * 60 + (light_at.1 as u32);
    if dark == light {
        // Degenerate: dawn equals dusk. Default to light (the
        // less-disruptive choice for a presumably-misconfigured
        // schedule).
        return false;
    }
    if dark < light {
        // Same-day window: dark in [dark, light).
        now >= dark && now < light
    } else {
        // Wraps past midnight: dark in [dark, 24:00) ∪ [00:00, light).
        now >= dark || now < light
    }
}

/// Parse a `theme-schedule = HH:MM dark, HH:MM light`
/// value into `Option<ThemeSchedule>`. Either tag-order is OK
/// (`HH:MM light, HH:MM dark` works too); whitespace is flexible.
///
/// Returns `None` on any malformed input — invalid HH:MM, missing
/// comma, missing role tag, hour > 23, minute > 59. Strict parse
/// keeps `--check-config` flagging real misconfigurations.
///
/// Pure — string-in, optional-schedule-out. Unit-testable.
pub fn parse_theme_schedule(value: &str) -> Option<ThemeSchedule> {
    let value = value.trim();
    // `theme-schedule = sunrise/sunset` is the
    // sunrise-mode trigger. The actual lat/long live in their
    // own config keys (read by parse_collect); the value here
    // is a placeholder (0.0/0.0 until the caller patches them in).
    // Phase 7 reconciles the post-parse lat/long override.
    let lowered = value.to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "sunrise/sunset" | "sunrise-sunset" | "sunrise_sunset" | "solar" | "auto"
    ) {
        return Some(ThemeSchedule::SunriseSunset {
            lat: 0.0,
            long: 0.0,
        });
    }
    let (a, b) = value.split_once(',')?;
    let parse_entry = |s: &str| -> Option<((u8, u8), &'static str)> {
        let s = s.trim();
        let (time_part, role) = s.rsplit_once(' ')?;
        let role = match role.trim().to_ascii_lowercase().as_str() {
            "dark" => "dark",
            "light" => "light",
            _ => return None,
        };
        let (h, m) = time_part.trim().split_once(':')?;
        let h: u8 = h.trim().parse().ok()?;
        let m: u8 = m.trim().parse().ok()?;
        if h > 23 || m > 59 {
            return None;
        }
        Some(((h, m), role))
    };
    let (entry_a_time, entry_a_role) = parse_entry(a)?;
    let (entry_b_time, entry_b_role) = parse_entry(b)?;
    if entry_a_role == entry_b_role {
        return None;
    }
    let (dark_at, light_at) = if entry_a_role == "dark" {
        (entry_a_time, entry_b_time)
    } else {
        (entry_b_time, entry_a_time)
    };
    Some(ThemeSchedule::Clock { dark_at, light_at })
}

/// Phase 7 of [`TERMINATOR-AUTO-THEME-DESIGN.md`](
/// docs/TERMINATOR-AUTO-THEME-DESIGN.md): compute UTC sunrise +
/// sunset (seconds-of-day) for a given `day_of_year` (1..=366) +
/// latitude + longitude. Uses the well-known NOAA simplified
/// algorithm — accurate to ~1 minute at temperate latitudes,
/// degrades near the poles where the sun may not rise/set on
/// some days (returns `None` for polar-day or polar-night).
///
/// Pure — no env, no clock, no dep. Unit-testable against
/// known fixtures.
///
/// Returns `Some((sunrise_secs, sunset_secs))` in UTC seconds-of-day.
pub fn sunrise_sunset_utc_secs(
    day_of_year: u16,
    lat_deg: f64,
    long_deg: f64,
) -> Option<(u32, u32)> {
    let n = day_of_year as f64;
    // Solar declination (simplified — Spencer's formula).
    // Range: ±23.45° over the year.
    let gamma = 2.0 * std::f64::consts::PI * (n - 1.0) / 365.0;
    let decl = 0.006918 - 0.399912 * gamma.cos() + 0.070257 * gamma.sin()
        - 0.006758 * (2.0 * gamma).cos()
        + 0.000907 * (2.0 * gamma).sin()
        - 0.002697 * (3.0 * gamma).cos()
        + 0.001480 * (3.0 * gamma).sin();
    // Equation of time (minutes).
    let eot_min = 229.18
        * (0.000075 + 0.001868 * gamma.cos()
            - 0.032077 * gamma.sin()
            - 0.014615 * (2.0 * gamma).cos()
            - 0.040849 * (2.0 * gamma).sin());
    // Hour angle for sunrise (when zenith = 90.833° to account
    // for atmospheric refraction).
    let lat_rad = lat_deg.to_radians();
    let zenith = (90.833_f64).to_radians();
    let cos_h = (zenith.cos() - lat_rad.sin() * decl.sin()) / (lat_rad.cos() * decl.cos());
    if !(-1.0..=1.0).contains(&cos_h) {
        // Polar day (sun never sets) or polar night (sun never
        // rises). Caller's policy decides what to do.
        return None;
    }
    let h_deg = cos_h.acos().to_degrees();
    // Solar noon in UTC minutes: 720 - 4*longitude - eot_min.
    let solar_noon_min = 720.0 - 4.0 * long_deg - eot_min;
    let h_min = 4.0 * h_deg;
    let sunrise_min = solar_noon_min - h_min;
    let sunset_min = solar_noon_min + h_min;
    // Wrap into [0, 1440) — sun events can fall outside the
    // calendar day in UTC (e.g. east longitudes push sunrise
    // before UTC midnight).
    let wrap = |m: f64| -> u32 {
        let mut m = m;
        while m < 0.0 {
            m += 1440.0;
        }
        while m >= 1440.0 {
            m -= 1440.0;
        }
        (m * 60.0) as u32
    };
    Some((wrap(sunrise_min), wrap(sunset_min)))
}

/// Phase 7 of the auto-theme design: pure decision
/// helper for `ThemeSchedule::SunriseSunset`. Returns
/// `true` = should be dark, `false` = should be light.
///
/// Wraps `sunrise_sunset_utc_secs`:
///   - sunrise has not occurred yet → dark
///   - between sunrise and sunset → light
///   - past sunset → dark
///   - polar day → light (default)
///   - polar night → dark (default)
///
/// Pure — input is wall-clock seconds + lat/long + day_of_year.
pub fn schedule_decision_sunrise(
    now_secs_of_day_utc: u32,
    day_of_year: u16,
    lat_deg: f64,
    long_deg: f64,
) -> bool {
    let Some((sunrise, sunset)) = sunrise_sunset_utc_secs(day_of_year, lat_deg, long_deg) else {
        // Polar regions: default to dark when the sun's below
        // the horizon all day, light when it's above. Heuristic:
        // northern hemisphere winter (Nov-Feb) at high latitude
        // == polar night == dark; summer == polar day == light.
        // We approximate via day-of-year: day 80..266 ≈ light.
        let summer = (80..=266).contains(&day_of_year);
        return if lat_deg.abs() > 66.5 {
            // Within polar circle.
            !((summer && lat_deg > 0.0) || (!summer && lat_deg < 0.0))
        } else {
            // Shouldn't happen — non-polar latitudes always have
            // sunrise/sunset. Default to light defensively.
            false
        };
    };
    // Handle the case where sunrise > sunset (sun event crosses
    // UTC midnight). In that case the *light* window wraps.
    if sunrise <= sunset {
        !(now_secs_of_day_utc >= sunrise && now_secs_of_day_utc < sunset)
    } else {
        // Light wraps past midnight: [sunrise, 24:00) ∪ [00:00, sunset).
        !(now_secs_of_day_utc >= sunrise || now_secs_of_day_utc < sunset)
    }
}

/// Phase 2 of [`TERMINATOR-AUTO-THEME-DESIGN.md`](
/// docs/TERMINATOR-AUTO-THEME-DESIGN.md): pure helper that picks
/// the right theme name given the current `ThemeMode`, the
/// configured `light_theme` / `dark_theme` / `theme_name` triple,
/// and the detected OS dark-mode preference (Some(true)=dark,
/// Some(false)=light, None=can't tell).
///
/// Returns the new theme name to apply, or `None` if no change
/// is needed.
///
/// Modes:
///
/// - `Explicit`: returns `None` (cfg.theme is the authority).
/// - `Light`: returns `Some(light_theme)` when non-empty.
/// - `Dark`: returns `Some(dark_theme)` when non-empty.
/// - `Auto` with `Some(is_dark)`: returns dark/light based on flag.
/// - `Auto` with `None`: returns `None` (can't decide).
///
/// Pure — no `&self`, no env, no clock; entirely a function of its
/// 5 inputs. Phase 6 of the auto-theme design will call this
/// from the App on `ThemeModeEvent::AutoUpdated`.
pub fn resolve_theme_for_mode(
    mode: ThemeMode,
    current: &str,
    light: &str,
    dark: &str,
    os_dark: Option<bool>,
) -> Option<String> {
    let pick = |target: &str| -> Option<String> {
        let target = target.trim();
        if target.is_empty() || target.eq_ignore_ascii_case(current) {
            None
        } else {
            Some(target.to_string())
        }
    };
    match mode {
        ThemeMode::Explicit => None,
        ThemeMode::Light => pick(light),
        ThemeMode::Dark => pick(dark),
        ThemeMode::Auto => match os_dark {
            Some(true) => pick(dark),
            Some(false) => pick(light),
            None => None,
        },
    }
}

impl AskBeforeClosing {
    /// Terminator parity (phase 1 of
    /// [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)):
    /// pure-decision helper — does a Close action with `scope_count`
    /// panes/tabs about to die need the confirm dialog?
    ///
    /// Matrix:
    ///   - `Never`              → never prompt
    ///   - `Always`             → always prompt
    ///   - `MultipleTerminals`  → prompt iff scope_count > 1
    ///
    /// Later phases wire this to the `Action::CloseWindow` /
    /// `CloseTab` / `ClosePane` dispatch. Pure — no `&self` shape
    /// needed; just the enum + count.
    pub fn should_prompt(self, scope_count: usize) -> bool {
        match self {
            AskBeforeClosing::Never => false,
            AskBeforeClosing::Always => true,
            AskBeforeClosing::MultipleTerminals => scope_count > 1,
        }
    }
}

/// When the per-pane scrollback scrollbar is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbarMode {
    Never,
    /// v2.26.0: visible whenever the pane has scrollback history (not only
    /// while scrolled back). Drawn dim at rest and brighter while the pointer
    /// hovers the bar, the view is scrolled back, or the thumb is being
    /// dragged — an overlay-scrollbar look that stays out of the way but is
    /// always grabbable with the mouse.
    Auto,
    /// Like `Auto`, but the track gutter is drawn even when there is no
    /// scrollback yet (a permanent right-edge channel).
    Always,
}

/// Stable-channel update behavior for official installer-owned builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdatePolicy {
    /// Never contact the release feed automatically.
    Off,
    /// Check at most once per day and show a passive notification.
    Notify,
    /// Check at most once per day and install an authenticated update in the
    /// background. The running process is never restarted automatically.
    Auto,
}

/// v2.20.0 (Ghostty `resize-overlay` parity): when the transient
/// `cols×rows` chip is shown during a live window resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeOverlayMode {
    /// Every resize, including the initial window placement.
    Always,
    Never,
    /// Every resize EXCEPT the first one after window creation (the
    /// initial placement isn't a user action) — Ghostty's default.
    AfterFirst,
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
    pub scrollback_bytes: usize,
    pub padding_x: f32,
    pub padding_y: f32,
    pub background_opacity: f32,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    pub bell: BellMode,
    /// OSC 52 clipboard policy (default: writes only).
    pub osc52: Osc52,
    /// Kettle's modified-Enter fallback before a client negotiates key encoding.
    pub modify_other_keys: ModifyOtherKeysMode,
    /// Paste a clipboard file list (e.g. a file copied in Explorer) as a
    /// shell-quoted path (default: on). See [`PasteFiles`].
    pub paste_files: PasteFiles,
    /// Paste a clipboard BITMAP (screenshot) as a temporary-PNG path.
    pub paste_images: PasteImages,
    /// Arm the GUI session recorder at launch (`record = on`). Off by default.
    /// Recording captures on-screen output verbatim; typed keystrokes are
    /// redacted to tokens unless [`Config::record_raw_input`] is on. The window
    /// title carries `[REC]` while active. See [`RecordMode`] and
    /// docs/RECORDING.md.
    pub record: RecordMode,
    /// Directory that `record = on` writes per-session asciicast traces into.
    /// Unset resolves to `<config-dir>/recordings`. Ignored when recording is
    /// off or an explicit `--record PATH` is given for the launch.
    pub record_dir: Option<PathBuf>,
    /// Capture RAW typed characters (including passwords) instead of redacted
    /// key tokens while recording. A SEPARATE, explicit opt-in from `record`;
    /// off by default. The window title shows `[REC RAW]` while active so
    /// literal-keystroke capture is never silent.
    pub record_raw_input: bool,
    pub tab_bar: TabBarMode,
    pub tab_bar_pos: TabBarPos,
    /// Status-bar mode. See [`StatusBarMode`].
    pub status_bar: StatusBarMode,
    /// Terminator parity (terminatorlib/config.py:75
    /// `borderless`): hide OS window decorations. Useful for tiling
    /// window managers + Quake-style dropdown setups where the host
    /// chrome is redundant.
    pub borderless: bool,
    /// Terminator parity (terminatorlib/config.py:78
    /// `always_on_top`): keep the kettle window above other
    /// windows. Best-effort per OS (Wayland respects compositor
    /// rules; X11 + macOS + Windows mostly honor it).
    pub always_on_top: bool,
    /// Terminator parity (terminatorlib/config.py:111
    /// `allow_bold`): when false, suppress bold text rendering
    /// (everything renders plain regardless of SGR 1). Useful on
    /// monospace fonts that lack a bold companion.
    pub allow_bold: bool,
    /// Terminator parity (terminatorlib/config.py:130
    /// `bold_is_bright`): when true, SGR 1 (bold) for indices
    /// 0-7 maps to the bright variant (8-15). xterm convention.
    pub bold_is_bright: bool,
    /// Terminator parity (terminatorlib/config.py:120
    /// `link_single_click`): when true, single-click on a URL
    /// opens it (kettle default: Ctrl+click). PuTTY/iTerm2-style.
    pub link_single_click: bool,
    /// Terminator parity (terminatorlib/config.py:91
    /// `clear_select_on_copy`): when true, the selection is
    /// deselected after Copy (default: keep selected, so user can
    /// re-copy).
    pub clear_select_on_copy: bool,
    /// Terminator parity (terminatorlib/config.py:128
    /// `disable_mousewheel_zoom`): when true, Ctrl+wheel doesn't
    /// change font size (lets terminal-based scroll-wheel users
    /// avoid accidental zooms).
    pub disable_mousewheel_zoom: bool,
    /// Terminator parity (terminatorlib/config.py:88
    /// `disable_mouse_paste`): when true, middle-click paste is
    /// disabled. Useful for terminal-of-last-resort use cases
    /// where you don't want clipboard content to leak in via
    /// accidental middle-clicks.
    pub disable_mouse_paste: bool,
    /// Confirm risky multi-line pastes when the target pane has not enabled
    /// bracketed paste. Single-line pastes and editor/agent panes that opt into
    /// bracketed paste continue without a prompt.
    pub clipboard_paste_protection: bool,
    /// Terminator parity (terminatorlib/config.py:89
    /// `putty_paste_style`): when true, right-click pastes (PuTTY/
    /// Windows convention). When false, right-click opens the
    /// context menu (kettle default + Linux convention).
    pub putty_paste_style: bool,
    /// Terminator parity (terminatorlib/config.py:90
    /// `smart_copy`): when true, Ctrl+Shift+C is a no-op when no
    /// selection exists (passes through to the shell). When false,
    /// the key is always consumed.
    pub smart_copy: bool,
    /// Terminator parity (terminatorlib/config.py:93
    /// `invert_search`): when true, scrollback search goes from
    /// the bottom up (newest matches first) instead of the default
    /// top-down (oldest first).
    pub invert_search: bool,
    /// Terminator parity: when true (default), scrollback search
    /// wraps around — advancing past the last match returns to the first. When
    /// false, Next stops at the last match and Previous stops at the first.
    pub search_wrap: bool,
    /// v2.20.0: vim-style navigation in kettle's menus and overlays
    /// (default ON). List overlays — context menu, new-tab dropdown,
    /// settings panel — take `j`/`k` (down/up, wrapping), `g`/`G`
    /// (first/last), `Ctrl+d`/`Ctrl+u` (half page); in the context menu
    /// and new-tab dropdown `h` goes back/closes and `l` drills in /
    /// activates, while in the settings panel `h`/`l` step the
    /// highlighted row's value (same as `←`/`→`); confirm dialogs take
    /// `y`/`n`. Text-input
    /// overlays with a selection list (palette, search, layout picker) keep
    /// letters for typing and use `Ctrl+j`/`Ctrl+k` (plus the
    /// `Ctrl+n`/`Ctrl+p` telescope/fzf idiom) to move it. When enabled, the
    /// context menu's mnemonic auto-assignment skips the nav letters so no
    /// row silently loses its hotkey; all other mnemonics and the
    /// type-to-search prefix keep working. Set `false` to restore plain
    /// arrow-key navigation everywhere.
    pub vim_menu_nav: bool,
    /// Terminator parity (terminatorlib/config.py:117
    /// `case_sensitive`): scrollback-search case-sensitivity
    /// override. kettle's default is `smart` (ripgrep/vim:
    /// insensitive until the pattern has any uppercase). `always`
    /// forces sensitive (Terminator's default), `never` forces
    /// insensitive.
    pub search_case_sensitive: SearchCaseSensitivity,
    /// Terminator parity (terminatorlib/config.py:114
    /// `term`): TERM environment variable for spawned shells.
    /// Default `xterm-256color` matches kettle's original
    /// hardcoded value + Terminator's own default.
    pub term: String,
    /// Terminator parity (terminatorlib/config.py:115
    /// `colorterm`): COLORTERM environment variable. Default
    /// `truecolor` signals 24-bit color support to programs that
    /// honor it (vim, nvim, tmux, ...).
    pub colorterm: String,
    /// User-defined environment variables for spawned panes. Repeatable
    /// `env = KEY=VALUE`; applied before Kettle's terminal identity vars so
    /// `term` / `colorterm` remain the authoritative way to change those.
    pub env: Vec<(String, String)>,
    /// Terminator parity (terminatorlib/config.py:122
    /// `login_shell`): when true, spawn the shell with `-l` (login
    /// shell semantics — reads /etc/profile, ~/.profile,
    /// ~/.bash_profile, ...). Default false matches Terminator.
    pub login_shell: bool,
    /// v2.29.1: auto-load kettle's shell integration into the DEFAULT shell
    /// kettle launches, so the shell reports its working directory (OSC 7) +
    /// prompt marks (OSC 133) with zero `$PROFILE` setup — this is what makes the
    /// tab track `cd` for a stock PowerShell (whose `Set-Location` does NOT update
    /// the OS process cwd, so it cannot be read from outside the process).
    /// Injects for pwsh/powershell via `-NoExit -EncodedCommand <kettle.ps1>`
    /// (the user's `$PROFILE` still loads first; kettle only wraps the resulting
    /// prompt). cmd.exe needs no injection (its process cwd tracks `cd`, read by
    /// the native poll). Default `true`; `shell-integration = off` launches the
    /// shell untouched (rely on a manually-sourced `$PROFILE` integration).
    pub shell_integration: bool,
    /// Terminator parity (terminatorlib/config.py:118
    /// `exit_action`): what to do when the shell process exits.
    pub exit_action: ExitAction,
    /// The agent control server mode
    /// (`off` | `read-only` | `full`). Default `off`. When enabled, kettle
    /// starts a local-IPC control server an AI agent (or `kettle ctl`/`kettle
    /// mcp`) can use to read the screen and drive panes. See docs/AGENT.md.
    pub agent_server: AgentServer,
    /// Terminator parity (terminatorlib/config.py:79
    /// `ask_before_closing`): when to show the close-confirmation
    /// dialog on window close.
    /// Consumed by kettle-ui for close-window, close-tab, and close-pane
    /// confirmation prompts.
    pub ask_before_closing: AskBeforeClosing,
    /// Terminator parity (terminatorlib/config.py:81
    /// `close_button_on_tab`): show ✕ on tabs.
    pub close_button_on_tab: bool,
    /// Terminator parity (terminatorlib/config.py:97
    /// `new_tab_after_current_tab`): insert new tab after the
    /// currently-active tab (vs at the end).
    pub new_tab_after_current_tab: bool,
    /// Terminator parity (terminatorlib/config.py:95
    /// `title_at_bottom`): per-pane titlebar position. No-op until
    /// the per-pane titlebar Bucket-D lands; config accepted now.
    pub title_at_bottom: bool,
    /// Terminator parity (terminatorlib/config.py:82
    /// `scroll_tabbar`): scrollable tab bar for many-tabs windows.
    pub scroll_tabbar: bool,
    /// v2.26.0: minimum width (logical px) of a horizontal tab segment. Tabs
    /// divide the bar evenly and fill it (v2.28.0: no maximum — they always
    /// maximize width); once they would shrink below `tab_min_width` the bar
    /// overflows and — when `scroll_tabbar` (the default) — scrolls with `‹ ›`
    /// arrows + the mouse wheel. Clamped at parse.
    pub tab_min_width: f32,
    /// Terminator parity (terminatorlib/config.py:77
    /// `hide_on_lose_focus`): hide window when it loses focus.
    /// Quake-style behavior. winit hint; partial OS support.
    pub hide_on_lose_focus: bool,
    /// Terminator parity (terminatorlib/config.py:78
    /// `sticky`): show window on every workspace (X11 only;
    /// no-op on Wayland and most other platforms).
    pub sticky: bool,
    /// Terminator parity (terminatorlib/config.py:76
    /// `hide_from_taskbar`): hide kettle from the OS taskbar.
    /// Linux-specific (X11 + Wayland support varies).
    pub hide_from_taskbar: bool,
    /// Terminator parity (terminatorlib/config.py:107
    /// `backspace_binding`): how Backspace key is encoded.
    pub backspace_binding: BackspaceBinding,
    /// Terminator parity (terminatorlib/config.py:108
    /// `delete_binding`): how Delete key is encoded.
    pub delete_binding: DeleteBinding,
    /// Terminator parity (terminatorlib/config.py:71
    /// `broadcast_default`): default broadcast scope.
    pub broadcast_default: BroadcastDefault,
    /// Terminator parity (terminatorlib/config.py:86
    /// `use_custom_url_handler`): use an external program for
    /// URL clicks instead of the OS default.
    pub use_custom_url_handler: bool,
    /// Terminator parity (terminatorlib/config.py:87
    /// `custom_url_handler`): path to the custom URL handler.
    /// No-op unless `use_custom_url_handler` is true.
    pub custom_url_handler: String,
    /// Terminator parity (terminatorlib/config.py
    /// `use_custom_command`): when false, ignore any
    /// `custom_command` / `command` / `shell` value and fall
    /// back to the user's $SHELL. Lets a Terminator profile
    /// keep `custom_command` defined but disabled. Applied at
    /// parse-finalize so the order of `command =` and
    /// `use_custom_command =` in the file doesn't matter.
    pub use_custom_command: bool,
    /// Terminator parity (terminatorlib/config.py:84
    /// `inactive_color_offset`): float 0.0-1.0 — unfocused-pane
    /// FG color dimming. kettle's `unfocused-split-opacity`
    /// applies to the whole pane; this is a separate FG-only
    /// dim. No-op until wired into the render layer.
    pub inactive_color_offset: f32,
    /// Terminator parity (terminatorlib/config.py:85
    /// `inactive_bg_color_offset`): BG-only dim for unfocused
    /// panes. No-op until wired into the render layer.
    pub inactive_bg_color_offset: f32,
    /// Terminator parity (terminatorlib/config.py:99
    /// `split_to_group`): new splits inherit the parent's broadcast
    /// group. Parsed and validated for forward-compat (see
    /// `docs/CONFIG.md`'s "genuine future work" table) but **not yet
    /// read anywhere** — it needs kettle's named broadcast groups
    /// (Bucket D) formalized first, so wiring it lives in
    /// `kettle-ui`'s pane-split path alongside `BroadcastScope::Group`,
    /// not here.
    pub split_to_group: bool,
    /// Terminator parity (terminatorlib/config.py:100
    /// `autoclean_groups`): remove empty broadcast groups
    /// automatically. Same forward-compat-stub status as
    /// `split_to_group` above — parsed, not yet consumed.
    pub autoclean_groups: bool,
    /// Terminator parity (terminatorlib/config.py:80
    /// `always_split_with_profile`): new splits inherit the
    /// parent pane's profile. Parsed for forward-compat only — needs
    /// the profile concept formalized in the pane-split path first
    /// (see `docs/CONFIG.md`); not yet read anywhere.
    pub always_split_with_profile: bool,
    /// Terminator parity (terminatorlib/config.py:73
    /// `focus`): focus mode — click (default), sloppy (focus
    /// follows mouse), system (use the desktop's focus mode).
    ///
    /// NOTE: `sloppy` (focus-follows-mouse) **is**
    /// wired — the pane under the cursor is focused on every cursor move
    /// (kettle-ui `app.rs`). `system` is treated like `click`
    /// because winit doesn't expose the OS-level focus policy. Surfaced as
    /// an editable option in the Settings overlay (Behavior ▸ Focus mode).
    /// (An earlier "no-op / not wired yet" note here was stale by the
    /// time `sloppy` was actually wired up.)
    pub focus: FocusMode,
    /// Terminator parity (terminatorlib/config.py:74
    /// `handle_size`): split-divider grab width in px. -1 means
    /// "use the GTK/winit theme default."
    pub handle_size: i32,
    /// Terminator parity (terminatorlib/config.py:75
    /// `window_state`): initial window state at launch.
    pub window_state: WindowState,
    /// `window-width` / `window_width`: initial terminal width in cells.
    /// `None` means let the platform/window manager choose kettle's default.
    pub window_width: Option<u32>,
    /// `window-height` / `window_height`: initial terminal height in cells.
    /// `None` means let the platform/window manager choose kettle's default.
    pub window_height: Option<u32>,
    /// `window-position-x` / `window_position_x`: initial window x position in
    /// physical pixels. Negative coordinates are valid on multi-monitor setups.
    pub window_position_x: Option<i32>,
    /// `window-position-y` / `window_position_y`: initial window y position in
    /// physical pixels. Negative coordinates are valid on multi-monitor setups.
    pub window_position_y: Option<i32>,
    /// `gpu-power-preference`: which adapter wgpu requests at startup.
    /// Defaults to `Auto`: let wgpu / the platform choose unless the user pins
    /// a specific GPU below or explicitly asks for low/high power preference.
    pub gpu_power_preference: GpuPowerPreference,
    /// `gpu-backend` (v2.23.0): pin the wgpu backend (DX12/Vulkan/Metal/GL)
    /// independently of the physical GPU pin, or use deterministic native
    /// `Auto` order (default). See [`GpuBackend`].
    pub gpu_backend: GpuBackend,
    /// `gpu-vendor-id` (v2.23.0): PCI vendor id of the pinned GPU (0 = unset →
    /// use `gpu-power-preference`). Set by the in-app GPU picker. Hex in the
    /// config file (e.g. `0x8086` Intel, `0x10de` NVIDIA, `0x1002` AMD).
    pub gpu_vendor_id: u32,
    /// `gpu-device-id` (v2.23.0): PCI device id of the pinned GPU (0 = unset).
    /// Paired with `gpu-vendor-id` for a robust, name-independent match.
    pub gpu_device_id: u32,
    /// `gpu-name` (v2.23.0): display name of the pinned GPU. Used for the
    /// settings label and as a fallback match if the (vendor,device) pair no
    /// longer enumerates (e.g. eGPU unplugged, driver swap). Empty = unset.
    pub gpu_name: String,
    /// `gpu-force-software` (v2.23.0): force wgpu's software/fallback adapter
    /// (`force_fallback_adapter`). Slow; for debugging GPU-driver issues.
    pub gpu_force_software: bool,
    /// Terminator parity (terminatorlib/config.py:75
    /// `geometry_hinting`): resize in font-step increments.
    pub geometry_hinting: bool,
    /// Terminator parity (terminatorlib/config.py:75
    /// `extra_styling`): in Terminator, loads extra GTK CSS per-theme
    /// — moot in kettle (wgpu+glyphon, not GTK). Reinterpreted per
    /// `docs/CONFIG.md`'s "genuine future work" table as "render
    /// bold/italic with styled-font features even when the palette
    /// lacks variants" — a real, not-yet-implemented render feature,
    /// so kept parsed for forward-compat rather than dropped; not yet
    /// read anywhere.
    pub extra_styling: bool,
    /// Terminator parity (terminatorlib/config.py:103
    /// `force_no_bell`): suppress every bell flavor. Same as
    /// kettle's `bell = off` but as a separate bool flag.
    pub force_no_bell: bool,
    /// Terminator parity extension (`plugins/logger.py`):
    /// when true, the per-pane session log strips ANSI escape
    /// sequences (CSI / OSC / single-char ESC) before writing.
    /// Default false preserves the original raw-stream behavior
    /// (the log is `cat`-replayable in a terminal).
    pub log_strip_ansi: bool,
    /// Terminator parity (`plugins/auto_theme.py`):
    /// theme-mode policy. Default `Explicit` preserves the original
    /// "use the `theme = …` value" behavior. `Light` / `Dark` /
    /// `Auto` are the Terminator AutoTheme modes. `Auto` follows the
    /// OS light/dark preference when winit reports one; an explicit
    /// `theme-schedule` overrides OS following.
    pub theme_mode: ThemeMode,
    /// Phase 4 of the auto-theme design: wall-clock
    /// schedule for switching between `light_theme` and `dark_theme`.
    /// `None` means no schedule (the default; user's `theme_mode`
    /// alone governs the choice). When `Some(Clock { dark_at,
    /// light_at })`, the App's poll loop (a phase 5 follow-up)
    /// will flip the theme on minute boundaries.
    ///
    /// `Some(SunriseSunset { lat, long })`
    /// is the sunrise/sunset-driven variant; the actual lat/long
    /// come from `theme_schedule_lat` + `theme_schedule_long`
    /// fields below (post-process patches the variant once both
    /// halves of the config are parsed).
    pub theme_schedule: Option<ThemeSchedule>,
    /// Latitude for sunrise/sunset-based theme schedule.
    /// Range `[-90.0, 90.0]`; outside this range parses as None +
    /// triggers a `--check-config` malformed-value diagnostic.
    pub theme_schedule_lat: Option<f64>,
    /// Longitude for sunrise/sunset-based theme schedule.
    /// Range `[-180.0, 180.0]`.
    pub theme_schedule_long: Option<f64>,
    /// Phase 7 of the vertical-tabs design: width of
    /// the vertical tab strip in pixels for `tab-bar-position =
    /// left`/`right`. Default 180.0 (Firefox-style sidebar).
    /// Range `[40.0, 600.0]`. No effect on horizontal layouts.
    pub tab_bar_width: f32,
    /// Terminator parity (`plugins/auto_theme.py`):
    /// theme name to switch to on `Action::ToggleLightDark`
    /// when the current theme matches `dark_theme`. Empty
    /// string = unset (action is a no-op).
    pub light_theme: String,
    /// Terminator parity (`plugins/auto_theme.py`):
    /// theme name to switch to on `Action::ToggleLightDark`
    /// when the current theme matches `light_theme`. Empty
    /// string = unset (action is a no-op). If neither is
    /// set, the toggle keybind silently no-ops.
    pub dark_theme: String,
    /// Terminator parity (terminatorlib/config.py:101
    /// `icon_bell`): show the bell icon in the per-pane titlebar
    /// when a bell rings. No-op until the Bucket-D titlebar lands.
    pub icon_bell: bool,
    /// Terminator parity (terminatorlib/config.py:96
    /// `show_titlebar`): show the per-pane titlebar widget. The
    /// titlebar itself is Bucket-D in docs/TERMINATOR-AUDIT.md.
    pub show_titlebar: bool,
    /// Terminator parity (terminatorlib/config.py:131
    /// `title_hide_sizetext`): hide the WxH size annotation in
    /// the per-pane titlebar.
    pub title_hide_sizetext: bool,
    /// Terminator parity (terminatorlib/config.py:142
    /// `title_use_system_font`): use the system font for the
    /// per-pane titlebar text.
    pub title_use_system_font: bool,
    /// Terminator parity (terminatorlib/config.py:143
    /// `title_font`): explicit font (when title_use_system_font
    /// is false). Default `Sans 9`.
    pub title_font: String,
    /// Terminator parity title-color triplets (terminatorlib/config.py:132-141).
    pub title_transmit_fg_color: Option<Rgb>,
    pub title_transmit_bg_color: Option<Rgb>,
    pub title_receive_fg_color: Option<Rgb>,
    pub title_receive_bg_color: Option<Rgb>,
    pub title_inactive_fg_color: Option<Rgb>,
    pub title_inactive_bg_color: Option<Rgb>,
    /// Terminator parity (terminatorlib/config.py:127
    /// `cursor_color_default`): when true, the cursor uses the
    /// theme's foreground color (default kettle behavior). Parsed and
    /// validated so a Terminator config round-trips without erroring,
    /// but permanently a no-op by design (see `docs/CONFIG.md`'s
    /// "won't implement" table): Terminator's two-key design
    /// (`cursor-color = X` plus this flag to opt back out of it) is
    /// confusing, so kettle's equivalent is simply setting/removing
    /// `cursor-color = …` — no separate boolean is ever consulted.
    pub cursor_color_default: bool,
    /// Terminator parity (terminatorlib/config.py:117
    /// `use_system_font`): use the OS system font. Parsed for
    /// forward-compat only (see `docs/CONFIG.md`'s "genuine future
    /// work" table, alongside `title_font`/`title_use_system_font`) —
    /// needs a multi-cycle per-pane font system before it's read
    /// anywhere.
    pub use_system_font: bool,
    /// Terminator parity (terminatorlib/config.py:116
    /// `use_theme_colors`): use OS theme colors. Same forward-compat-
    /// stub status as `use_system_font` above — parsed, not yet
    /// consumed.
    pub use_theme_colors: bool,
    /// Terminator parity (terminatorlib/config.py:144
    /// `http_proxy`): HTTP proxy URL for plugin HTTP requests.
    /// Permanently a no-op by design (see `docs/CONFIG.md`'s "won't
    /// implement" table), not merely "not yet": kettle has no
    /// plugin-HTTP layer, and the self-updater's HTTPS requests are
    /// process-wide and intentionally don't inherit a per-profile
    /// terminal setting. Kept parse-only so a Terminator config
    /// doesn't trip the unknown-key diagnostic.
    pub http_proxy: String,
    /// Terminator parity (terminatorlib/config.py:118
    /// `background_type`): background style.
    pub background_type: BackgroundType,
    /// `text-renderer` (v2.25.0): cell-locked grid rendering (default) vs the
    /// legacy continuous glyphon layout. See [`TextRenderer`].
    pub text_renderer: TextRenderer,
    /// Terminator parity (terminatorlib/config.py:117
    /// `background_image`): path to background image. No-op
    /// until Bucket-D bg-image render lands.
    pub background_image: String,
    /// Terminator parity (terminatorlib/config.py:119
    /// `background_image_mode`): tiling mode.
    pub background_image_mode: String,
    /// Terminator parity (terminatorlib/config.py:120
    /// `background_image_align_horiz`): horizontal alignment.
    pub background_image_align_horiz: String,
    /// Terminator parity (terminatorlib/config.py:121
    /// `background_image_align_vert`): vertical alignment.
    pub background_image_align_vert: String,
    /// Terminator parity (terminatorlib/config.py:122
    /// `background_blur`): blur the background image.
    pub background_blur: bool,
    /// `background-animation`: how an animated background (a `Starfield` or an
    /// animated `background-image`) plays. Defaults to `Always` (v2.24.0). See
    /// [`BackgroundAnimation`].
    pub background_animation: BackgroundAnimation,
    /// `chrome-background` (v2.23.0): the opaque chrome strip color used when a
    /// `background-image` or `Starfield` is set. Defaults to `Theme`. See
    /// [`ChromeBackground`].
    pub chrome_background: ChromeBackground,
    /// Terminator parity (terminatorlib/config.py:106
    /// `background_darkness`): background image opacity (0.0 fully
    /// dark .. 1.0 untinted).
    pub background_darkness: f32,
    /// Terminator parity (terminatorlib/config.py:93
    /// `cell_height`): vertical cell scaling (default 1.0).
    /// Applied by kettle-render as a multiplier on measured cell height.
    pub cell_height: f32,
    /// Terminator parity (terminatorlib/config.py:94
    /// `cell_width`): horizontal cell scaling. Applied by kettle-render as a
    /// multiplier on measured cell width.
    pub cell_width: f32,
    /// Terminator parity (terminatorlib/config.py:124
    /// `detachable_tabs`): allow dragging tabs between windows.
    /// When false, cross-window tab tear-off and the
    /// `move_tab_to_new_window` action are disabled; in-window tab switching
    /// and reordering remain available.
    pub detachable_tabs: bool,
    /// Terminator parity (terminatorlib/config.py:96
    /// `putty_paste_style_source_clipboard`): when `putty_paste_
    /// style` is true, source right-click paste from the regular
    /// system clipboard instead of the X11 PRIMARY selection.
    pub putty_paste_style_source_clipboard: bool,
    /// Terminator plugin parity:
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
    /// Prefix shown on a per-pane titlebar while an
    /// agent control connection has the pane attached. Default `"[agent] "`
    /// (ASCII, no font-fallback risk). Empty disables the badge.
    pub agent_badge: String,
    /// Template for each tab segment in the tab bar. Placeholders:
    /// `{n}` (1-based tab index), `{title}` (focused pane's title).
    pub tab_format: String,
    pub scrollbar: ScrollbarMode,
    /// v2.26.0: width in logical px of the scrollbar track + thumb. The thumb
    /// fills this width; the track gutter is the same width at a lower opacity.
    /// Clamped to `[2, 40]`. Default `14` (Terminator-like — wide enough to grab
    /// with the mouse, unlike the old 3 px hairline).
    pub scrollbar_width: f32,
    /// v2.20.0 (Ghostty parity): when to show the transient `cols×rows`
    /// chip during a live window resize. Default `after-first` (every
    /// resize except the initial window placement).
    pub resize_overlay: ResizeOverlayMode,
    /// Explicit split-divider/border color for *inactive* panes (else
    /// theme `palette[8]`, the dim color).
    pub split_divider_color: Option<Rgb>,
    /// Explicit border color for the *focused* pane (else theme
    /// `palette[4]`, the blue accent). Lets users tune the
    /// here-am-I indicator without re-themeing the whole palette.
    pub focused_split_color: Option<Rgb>,
    /// Peacock parity. When set, this color overrides
    /// every "kettle accent" surface — active tab segment's accent
    /// strip, focused-pane border (unless `focused-split-color` is
    /// also set; that wins for backward-compat), the
    /// dragged-tab ghost strip. Lets a user run multiple kettle
    /// windows (`--profile dev` + `--profile ops`) and tell them
    /// apart at a glance. `palette[4]` and `palette[3]` (broadcast
    /// warning) are *not* overridden — broadcast stays yellow so
    /// the high-priority state isn't confused with a workspace
    /// identity color.
    pub accent_color: Option<Rgb>,
    /// Peacock parity: `accent-color = auto` — derive a distinct
    /// chrome accent per *working directory* AND per window, so a new kettle
    /// window in a different project is a visually different color (VS Code
    /// Peacock style) while a given project stays consistent across launches;
    /// two live windows never share a hue while the theme's pool has a free
    /// one. ON by default since multi-window support landed —
    /// opt out with `accent-color = theme` (or `off`/`none`); an explicit
    /// `accent-color = <hex>` / `--accent` always wins and pins every window.
    pub accent_auto: bool,
    /// Runtime-only seed for `accent_auto` (a hash of the window's
    /// startup working directory). Set by the App at launch, NOT parsed from the
    /// config file; default 0 (the theme's signature accent for the home/seedless
    /// case). Kept on `Config` so the renderer can resolve the accent from `cfg`
    /// + `theme` alone without threading a separate parameter.
    pub accent_seed: u64,
    /// Cursor blink half-period in milliseconds.
    pub cursor_blink_interval: u64,
    /// An inactive tab whose unseen output went quiet for
    /// at least this many milliseconds transitions from the
    /// `Output` indicator (cyan) to `Silent` (dim chrome). Default
    /// 10 s — long enough that a slow `cargo build` doesn't oscillate
    /// the indicator between strokes, short enough that a `tail -f`
    /// going quiet is noticed before the user switches tabs. Clamped
    /// `[1000, 600_000]` (1 s..10 min) at parse time.
    pub tab_silence_threshold_ms: u64,
    /// Terminator parity (`command_notify.py` plugin):
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
    /// Stable update policy. `auto` is the default (keep Kettle current in the
    /// background, oh-my-zsh style); the first launch only stamps the throttle
    /// and performs no network request. Set `update-policy = off` to opt out.
    pub update_policy: UpdatePolicy,
    /// How often the background update check may contact the release feed, in
    /// hours (default 24 = daily). Floored at 1 to avoid hammering the feed.
    /// `update-policy = off` disables checking entirely regardless of this.
    pub update_check_interval_hours: u32,
    /// Restore the previous session's tabs/splits/working-dirs on
    /// launch. OFF by default — like every mainstream terminal (GNOME Terminal,
    /// Windows Terminal, kitty, Alacritty, WezTerm, iTerm2), a new window/instance
    /// opens FRESH (a single pane in the default cwd). Opt in with
    /// `restore-session = true` (or the `--restore` flag) to continue where you
    /// left off. The session is always SAVED so opt-in restore has state to load.
    pub restore_session: bool,
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
    /// `None` = derive from the active theme (so the search /
    /// quick-select highlight matches whatever theme is set, incl. the Catppuccin
    /// Mocha default — `search_background` falls back to `theme.palette[3]`, the
    /// theme's yellow; `search_foreground` to `theme.background`). An explicit
    /// `search-foreground`/`search-background` config value overrides.
    pub search_foreground: Option<Rgb>,
    pub search_background: Option<Rgb>,
    pub keybinds: Bindings,
    /// Shell override; `None` uses `$SHELL` / platform default.
    pub shell: Option<String>,
    /// Named SSH targets: `ssh-host = name=user@host` (repeatable).
    pub ssh_hosts: Vec<(String, String)>,
    /// iTerm2-parity triggers. Each entry is a regex
    /// pattern matched against PTY output; when it fires while the
    /// pane is unfocused, the action runs. Repeatable via
    /// `trigger = REGEX` config lines (default action: Urgency).
    /// Stored as strings; kettle-core compiles them to
    /// `regex::Regex` at pane-spawn time so a malformed regex on
    /// one trigger doesn't sink the whole config load — invalid
    /// patterns are logged via `log::warn!` and dropped.
    pub triggers: Vec<OutputTrigger>,
    /// Terminator parity (`terminatorlib/plugins/
    /// custom_commands.py` → "Custom Commands" menu). User-defined
    /// right-click menu entries: each `menu-item = LABEL = CMD`
    /// config line appends one row. Clicking the row writes
    /// `CMD\n` to the focused pane's PTY (same shape as
    /// `kettle.send_text`). Strictly additive on
    /// top of the built-in context menu items.
    ///
    /// Distinct from the `kettle.add_menu_item(label,
    /// callback)` Lua API: that one takes a callback for
    /// arbitrary Lua-side behavior; this one is config-file-only
    /// and sends literal text. Use this entry for plain
    /// "type this command" rows; use Lua for anything richer.
    pub menu_items: Vec<MenuItem>,
}

/// A user-defined right-click menu entry from
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

/// One configured output-trigger rule. Plain-string
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

/// Trigger action. One enum so the config parser can grow
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
    /// Terminator parity (`plugins/run_cmd_on_match.py`):
    /// spawn an external program when the trigger pattern matches.
    /// Argv form (no shell expansion at kettle's layer) — security
    /// posture is "treat the configured command as data, not a
    /// shell string." Capture groups are NOT substituted in v1
    /// (a `$1`-substitution path is the natural next increment).
    ///
    /// The spawn is fire-and-forget: kettle does not wait for the
    /// child to exit, doesn't capture its stdout/stderr, and
    /// doesn't track its lifetime. Same shape as a shell `&`
    /// background launch. If the command can't be spawned (binary
    /// missing, perm denied) a warn is logged + the trigger is
    /// otherwise ignored.
    RunCommand(Vec<String>),
}

/// Splits a `trigger = REGEX :: cmd arg1 arg2`
/// value into `(pattern, argv)`. Returns `None` when there's no
/// `::` separator (caller treats the whole value as a plain
/// Urgency trigger, preserving that behavior).
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

/// Terminator menu UX (C7): atomic write-back for the
/// in-menu Preferences toggles. Persists a `key = value` line to
/// the user's config file with these contracts:
///
///   1. **In-place edit**: if the file already has a line matching
///      `key` (allowing `-` / `_` equivalence + leading/trailing
///      whitespace), only that line is replaced — every other line
///      including comments + blanks + ordering survives byte-for-
///      byte.
///   2. **Append on miss**: if no matching line exists, the new
///      `key = value` is appended with a leading blank line for
///      readability.
///   3. **Atomic + durable**: stage a collision-safe private sibling, sync it,
///      atomically replace the target, and sync the parent directory.
///   4. **First-write backup**: if `<path>.bak` doesn't exist yet,
///      save an encoding-preserving copy of the pre-edit bytes there. Subsequent
///      writes don't touch the backup — it's a "what did my config
///      look like before I started clicking toggles?" forensic
///      snapshot.
///   5. **Pre-commit validation**: before replacement, the candidate is
///      scanned with `Config::detect_malformed_values`.
///      Because this helper only ever rewrites a single line, any
///      *additional* diagnostic compared with the pre-edit content
///      means the new value is malformed, so it never becomes visible.
///   6. **Symlink preservation**: a symlinked config is resolved and its regular
///      target is replaced, leaving the link intact for dotfile managers.
///   7. **Concurrency + encoding**: a per-target advisory lock serializes all
///      Kettle editors, a final byte comparison refuses external-editor races,
///      and UTF-8 BOM / UTF-16 LE / UTF-16 BE input keeps its encoding.
///
/// Returns the path of the backup (created or pre-existing) on
/// success so the caller can surface it to the user. Pure-modulo-
/// the-filesystem so the contract is unit-tested with a tempdir
/// fixture.
pub fn persist_config_toggle(path: &Path, key: &str, new_value: &str) -> std::io::Result<PathBuf> {
    validate_single_line("config key", key)?;
    validate_single_line("config value", new_value)?;
    let candidate_line = format!("{key} = {new_value}\n");
    if !Config::detect_malformed_values(&candidate_line).is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("refusing to persist `{key} = {new_value}`: it is not a valid config value"),
        ));
    }
    let edit = ConfigEdit::begin(path)?;
    let existing = &edit.document.text;
    // Build the new text by walking lines. A line matches if its
    // first non-whitespace token, normalized to underscore form,
    // equals `key` (also normalized). Comments (`#` or `//` lines)
    // and blank lines pass through untouched.
    let needle = normalize_key(key);
    let line_ending = preferred_line_ending(existing);
    let mut out = String::with_capacity(existing.len() + candidate_line.len() + 2);
    let mut replaced = false;
    for segment in existing.split_inclusive('\n') {
        let (line, ending) = split_line_ending(segment);
        if let Some(line_key) = parse_line_key(line)
            && normalize_key(line_key) == needle
        {
            // Only the FIRST matching line becomes the new
            // value; any further duplicate lines for the same key are
            // dropped. Previously every match was rewritten, so a file
            // that already had two `cursor-blink = …` lines (or repeated
            // UI toggles that somehow doubled up) accumulated identical
            // lines forever. The parser is last-wins so behavior was
            // always correct — this keeps the on-disk file de-duplicated
            // to a single line, matching `append_keybind`'s drop-old
            // semantics.
            if !replaced {
                out.push_str(&format!("{key} = {new_value}"));
                out.push_str(ending);
                replaced = true;
            }
            continue;
        }
        out.push_str(segment);
    }
    if !replaced {
        ensure_blank_line_before_append(&mut out, line_ending);
        out.push_str(&format!("{key} = {new_value}"));
    }
    // Ensure trailing newline so the file is well-formed.
    if !out.ends_with('\n') {
        out.push_str(line_ending);
    }
    // Validate before the atomic replacement. A malformed candidate never
    // becomes visible, so there is no rollback window after the commit.
    let before_bad = Config::detect_malformed_values(existing).len();
    let after_bad = Config::detect_malformed_values(&out).len();
    if after_bad > before_bad {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "refusing to persist `{key} = {new_value}`: it would introduce a \
                 malformed config value"
            ),
        ));
    }
    edit.commit(&out)
}

/// Append a `keybind = <trigger>=<action>` line to the user's config
/// file, atomically and with the same first-write `.bak` backup as
/// `persist_config_toggle`. `keybind` is *repeatable* (unlike the single-valued
/// keys `persist_config_toggle` handles), so this appends rather than replaces —
/// the interactive keybind editor uses it to add a binding live + persist it.
/// Any prior `keybind` line that maps the SAME trigger is dropped first so the
/// file doesn't accumulate stale duplicates for a re-rebound chord.
pub fn append_keybind(path: &Path, trigger: &str, action: &str) -> std::io::Result<PathBuf> {
    validate_single_line("keybind trigger", trigger)?;
    validate_single_line("keybind action", action)?;
    let candidate_line = format!("keybind = {trigger}={action}\n");
    if !Config::detect_malformed_values(&candidate_line).is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "refusing to persist `keybind = {trigger}={action}`: it is not a valid key binding"
            ),
        ));
    }
    let edit = ConfigEdit::begin(path)?;
    let existing = &edit.document.text;
    // Drop any existing `keybind` line whose trigger is the SAME chord — a
    // re-rebind should overwrite, not stack. Compare
    // SEMANTICALLY via `parse_trigger` (and split the value on the LAST `=`, like
    // apply_keybind + the keybind malformed-value diagnostic in
    // `detect_malformed_values`), so the editor's canonical
    // `Ctrl+Equal` and a hand-written `ctrl+=` count as the same trigger, and a
    // literal `=` chord (`ctrl+==action`) de-dups correctly. The old first-`=`
    // string compare missed both and accumulated a stale duplicate line.
    let want_trig = keybinds::parse_trigger(trigger.trim());
    let line_ending = preferred_line_ending(existing);
    let mut out = String::with_capacity(existing.len() + candidate_line.len() + 2);
    for segment in existing.split_inclusive('\n') {
        let (line, _) = split_line_ending(segment);
        let drop = parse_line_key(line).is_some_and(|k| normalize_key(k) == "keybind")
            && want_trig.is_some()
            && line
                .split_once('=')
                .and_then(|(_, v)| v.rsplit_once('='))
                .and_then(|(t, _)| keybinds::parse_trigger(t.trim()))
                == want_trig;
        if !drop {
            out.push_str(segment);
        }
    }
    ensure_blank_line_before_append(&mut out, line_ending);
    out.push_str(&format!("keybind = {trigger}={action}"));
    out.push_str(line_ending);
    if Config::detect_malformed_values(&out).len() > Config::detect_malformed_values(existing).len()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "refusing to persist `keybind = {trigger}={action}`: it would introduce a malformed config value"
            ),
        ));
    }
    edit.commit(&out)
}

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

fn preferred_line_ending(text: &str) -> &'static str {
    match text.as_bytes().iter().position(|byte| *byte == b'\n') {
        Some(index) if index > 0 && text.as_bytes()[index - 1] == b'\r' => "\r\n",
        _ => "\n",
    }
}

fn ensure_blank_line_before_append(output: &mut String, line_ending: &str) {
    if output.is_empty() {
        return;
    }
    if !output.ends_with('\n') {
        output.push_str(line_ending);
    }
    let mut before_final_ending = &output[..output.len() - 1];
    if let Some(without_carriage_return) = before_final_ending.strip_suffix('\r') {
        before_final_ending = without_carriage_return;
    }
    if !before_final_ending.ends_with('\n') {
        output.push_str(line_ending);
    }
}

/// Extract the key from a `KEY = VALUE` config line.
/// Returns `None` for blanks, comments, or malformed lines.
fn parse_line_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
        return None;
    }
    let (k, _) = trimmed.split_once('=')?;
    let k = k.trim_end();
    if k.is_empty() { None } else { Some(k) }
}

#[derive(Clone, Copy)]
enum ConfigEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
}

struct ConfigDocument {
    bytes: Vec<u8>,
    text: String,
    encoding: ConfigEncoding,
}

const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

fn invalid_config_file(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("config path is not a regular file: {}", path.display()),
    )
}

fn set_config_read_flags(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    #[cfg(not(any(unix, windows)))]
    let _ = options;
}

fn read_config_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(invalid_config_file(path));
        }
        Ok(_) => {}
        Err(error) => return Err(error),
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    set_config_read_flags(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) => {
            #[cfg(unix)]
            if error.raw_os_error() == Some(libc::ELOOP) {
                return Err(invalid_config_file(path));
            }
            return Err(error);
        }
    };
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(invalid_config_file(path));
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "config file {} is {} bytes (cap {MAX_CONFIG_BYTES})",
                path.display(),
                metadata.len()
            ),
        ));
    }

    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "config file {} exceeds the {MAX_CONFIG_BYTES}-byte cap",
                path.display()
            ),
        ));
    }
    Ok(bytes)
}

impl ConfigDocument {
    fn read(path: &Path) -> std::io::Result<Self> {
        match read_config_bytes(path) {
            Ok(bytes) => Self::decode(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self {
                bytes: Vec::new(),
                text: String::new(),
                encoding: ConfigEncoding::Utf8,
            }),
            Err(error) => Err(error),
        }
    }

    fn decode(bytes: Vec<u8>) -> std::io::Result<Self> {
        let (encoding, text) = if let Some(payload) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
            (ConfigEncoding::Utf8Bom, decode_utf8_strict(payload)?)
        } else if let Some(payload) = bytes.strip_prefix(&[0xff, 0xfe]) {
            (ConfigEncoding::Utf16Le, decode_utf16_strict(payload, true)?)
        } else if let Some(payload) = bytes.strip_prefix(&[0xfe, 0xff]) {
            (
                ConfigEncoding::Utf16Be,
                decode_utf16_strict(payload, false)?,
            )
        } else {
            (ConfigEncoding::Utf8, decode_utf8_strict(&bytes)?)
        };
        Ok(Self {
            bytes,
            text,
            encoding,
        })
    }

    fn encode(&self, text: &str) -> Vec<u8> {
        match self.encoding {
            ConfigEncoding::Utf8 => text.as_bytes().to_vec(),
            ConfigEncoding::Utf8Bom => {
                let mut bytes = vec![0xef, 0xbb, 0xbf];
                bytes.extend_from_slice(text.as_bytes());
                bytes
            }
            ConfigEncoding::Utf16Le | ConfigEncoding::Utf16Be => {
                let little_endian = matches!(self.encoding, ConfigEncoding::Utf16Le);
                let mut bytes = if little_endian {
                    vec![0xff, 0xfe]
                } else {
                    vec![0xfe, 0xff]
                };
                for unit in text.encode_utf16() {
                    let encoded = if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    };
                    bytes.extend_from_slice(&encoded);
                }
                bytes
            }
        }
    }
}

struct ConfigEdit {
    path: PathBuf,
    backup: PathBuf,
    document: ConfigDocument,
    _lock: kettle_state::ExclusiveFileLock,
}

impl ConfigEdit {
    fn begin(requested_path: &Path) -> std::io::Result<Self> {
        if requested_path.components().any(|c| c.as_os_str() == "..") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "refusing path with `..` component: {}",
                    requested_path.display()
                ),
            ));
        }
        let parent = requested_path.parent().ok_or_else(|| {
            std::io::Error::other(format!(
                "config path has no parent: {}",
                requested_path.display()
            ))
        })?;
        std::fs::create_dir_all(parent)?;

        // Editing a symlink updates its resolved regular-file target instead
        // of replacing the link itself. This preserves dotfile-manager setups.
        let path = match std::fs::symlink_metadata(requested_path) {
            Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
                std::fs::canonicalize(requested_path)?
            }
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "config path is not a regular file: {}",
                        requested_path.display()
                    ),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let canonical_parent = std::fs::canonicalize(parent)?;
                let name = requested_path.file_name().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("config path has no file name: {}", requested_path.display()),
                    )
                })?;
                canonical_parent.join(name)
            }
            Err(error) => return Err(error),
        };
        let lock_path = path.with_extension(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map_or_else(
                    || "lock".to_string(),
                    |extension| format!("{extension}.lock"),
                ),
        );
        let lock = kettle_state::ExclusiveFileLock::acquire(&lock_path)?;
        let document = ConfigDocument::read(&path)?;
        let backup = path.with_extension(
            path.extension()
                .and_then(|extension| extension.to_str())
                .map_or_else(|| "bak".to_string(), |extension| format!("{extension}.bak")),
        );
        Ok(Self {
            path,
            backup,
            document,
            _lock: lock,
        })
    }

    fn commit(self, text: &str) -> std::io::Result<PathBuf> {
        // Editors outside Kettle do not honor our advisory lock. Refuse to
        // overwrite a change made since this edit began.
        let current = match read_config_bytes(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        if current != self.document.bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                format!(
                    "config changed while it was being edited: {}",
                    self.path.display()
                ),
            ));
        }
        kettle_state::atomic_create_new(
            &self.backup,
            &self.document.bytes,
            kettle_state::AtomicWriteOptions::PRIVATE,
        )?;
        kettle_state::atomic_replace(
            &self.path,
            &self.document.encode(text),
            kettle_state::AtomicWriteOptions::PRESERVE_PERMISSIONS,
        )?;
        Ok(self.backup)
    }
}

fn decode_utf8_strict(bytes: &[u8]) -> std::io::Result<String> {
    String::from_utf8(bytes.to_vec()).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("config file is not valid UTF-8: {}", error.utf8_error()),
        )
    })
}

fn decode_utf16_strict(bytes: &[u8], little_endian: bool) -> std::io::Result<String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "UTF-16 config has an incomplete code unit",
        ));
    }
    let units = bytes.chunks_exact(2).map(|pair| {
        let pair = [pair[0], pair[1]];
        if little_endian {
            u16::from_le_bytes(pair)
        } else {
            u16::from_be_bytes(pair)
        }
    });
    std::char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("config file contains invalid UTF-16: {error}"),
            )
        })
}

fn validate_single_line(label: &str, value: &str) -> std::io::Result<()> {
    if value.contains(['\n', '\r', '\0']) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} must contain exactly one text line"),
        ));
    }
    Ok(())
}

/// Normalize a config-key name for comparison: lowercase
/// the value and treat `-` as equivalent to `_`. So `cursor-blink`,
/// `cursor_blink`, and `Cursor-Blink` all hash to the same key.
fn normalize_key(k: &str) -> String {
    k.trim().to_ascii_lowercase().replace('-', "_")
}

/// Split a Pango font description — Terminator's `font = Mono 10` — into a
/// family and an optional size.
///
/// Pango's grammar is `FAMILY [STYLE-OPTIONS] [SIZE]`, so the size is a
/// trailing number and everything before it is the family (families may
/// contain spaces: `DejaVu Sans Mono 11`). Returns `None` for an empty value so
/// the caller leaves the existing settings alone.
///
/// Style words (`Bold`, `Italic`, …) are deliberately kept as part of the
/// family string rather than parsed out: kettle selects styles per-run from the
/// terminal's own attributes, so a style baked into the base family would fight
/// that. Leaving them in means `Mono Bold 10` resolves through the same
/// family-matching path as any other family name.
fn parse_font_description(value: &str) -> Option<(String, Option<f32>)> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (head, size) = match trimmed.rsplit_once(char::is_whitespace) {
        // A trailing token that parses as a positive size is the Pango size.
        Some((family, last)) => match last.parse::<f32>() {
            Ok(size) if size.is_finite() && size > 0.0 => (family.trim(), Some(size)),
            _ => (trimmed, None),
        },
        // Single token: a bare family with no size.
        None => (trimmed, None),
    };
    // Pango's grammar is FAMILY-LIST, comma-separated, as a fallback chain.
    // kettle resolves one family, so take the first — and this is also what
    // sheds the separator itself, which would otherwise ride along into the
    // family name (`Arial Black, 12` → the family `Arial Black,`).
    let family = strip_pango_style_options(head)
        .split(',')
        .map(str::trim)
        .find(|f| !f.is_empty())
        .unwrap_or(head)
        .to_string();
    Some((family, size))
}

/// Every Pango style keyword, lowercased and with separators removed.
///
/// Pango's grammar is `FAMILY [STYLE-OPTIONS] [SIZE]`, where the style options
/// are a whitespace-separated run of these words. Matching is done on a
/// normalized form so `Ultra-Light`, `ultra light`, and `UltraLight` all
/// compare equal, which is the spread GTK's own font chooser produces.
const PANGO_STYLE_OPTIONS: &[&str] = &[
    // Style. `regular` is what GTK's font chooser writes for a default face,
    // so omitting it made the most common description of all
    // (`DejaVu Sans Mono Regular 13`) request a family no system has.
    "normal",
    "regular",
    "book",
    "roman",
    "oblique",
    "italic", // Variant.
    "smallcaps",
    "allsmallcaps",
    "petitecaps",
    "allpetitecaps",
    "unicase",
    "titlecaps",
    // Weight.
    "thin",
    "ultralight",
    "extralight",
    "light",
    "semilight",
    "demilight",
    "book",
    "medium",
    "semibold",
    "demibold",
    "bold",
    "ultrabold",
    "extrabold",
    "heavy",
    "black",
    "ultraheavy",
    "extraheavy",
    "ultrablack",
    "extrablack", // Stretch.
    "ultracondensed",
    "extracondensed",
    "condensed",
    "semicondensed",
    "semiexpanded",
    "expanded",
    "extraexpanded",
    "ultraexpanded", // Gravity.
    "notrotated",
    "south",
    "upsidedown",
    "north",
    "rotatedleft",
    "east",
    "rotatedright",
    "west",
];

/// Drop the trailing Pango style options from a font description, leaving the
/// family.
///
/// `font = DejaVu Sans Mono Bold 13` asks for the BOLD FACE of the family
/// `DejaVu Sans Mono`. Keeping the whole string meant requesting a family
/// literally named "DejaVu Sans Mono Bold", which no system has, so the whole
/// font silently fell back — the user got a different typeface than the one
/// their config named.
///
/// The style itself is not carried anywhere: kettle picks faces per cell from
/// the terminal's own bold/italic attributes (with `font-family-bold` and
/// friends as explicit overrides), and has no notion of a base weight for a
/// style option to set. Dropping it is what makes the family resolve;
/// `docs/CONFIG.md` says so rather than leaving it to be discovered.
///
/// Never strips every token — a family whose real name ends in a style word
/// keeps at least its first — and stops at the first token that is not a style
/// option, so `Fira Code Light Retina` keeps `Retina` and everything before it.
fn strip_pango_style_options(head: &str) -> &str {
    fn normalize(t: &str) -> String {
        t.chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect()
    }
    fn is_style(tokens: &str) -> bool {
        // A comma is Pango's family-LIST separator, so a token carrying one is
        // part of the family, never a style option. Normalizing punctuation
        // away turned `Arial Black, 12` into family `Arial`, because the comma
        // made `Black,` look like the style word `black`.
        if tokens.contains(',') {
            return false;
        }
        PANGO_STYLE_OPTIONS.contains(&normalize(tokens).as_str())
    }
    // Split off the last `n` whitespace-separated tokens, if that leaves a
    // non-empty family behind.
    fn split_tail(s: &str, n: usize) -> Option<(&str, &str)> {
        let mut rest = s;
        for _ in 0..n {
            rest = rest.rsplit_once(char::is_whitespace)?.0.trim_end();
        }
        (!rest.is_empty()).then(|| (rest, s[rest.len()..].trim_start()))
    }

    let mut end = head.trim_end();
    loop {
        // Two tokens first: Pango spells the compound options either way
        // (`Semi-Bold`, `Semi Bold`, `SemiBold`), and taking one token at a
        // time would strip `Bold` and then stop at `Semi`, leaving it stuck to
        // the family.
        let two = split_tail(end, 2).filter(|(_, tail)| is_style(tail));
        let one = split_tail(end, 1).filter(|(_, tail)| is_style(tail));
        match two.or(one) {
            Some((rest, _)) => end = rest,
            None => return end,
        }
    }
}

/// Parse Terminator's colon-separated palette list into ordered colours.
///
/// Terminator writes `palette = "#2e3436:#cc0000:…"` — the whole palette in one
/// value, slots in order. Returns `None` when the value is not a colon list or
/// any element is not a colour, so the caller can fall through to kettle's
/// `N=#hex` and named-preset forms rather than half-applying a malformed list.
///
/// Accepts 8 or 16 entries: Terminator itself writes 16, but an 8-entry list
/// (the normal-intensity half) appears in older configs and hand-written ones,
/// and applying it to slots 0..8 is exactly right.
fn parse_colon_palette(value: &str) -> Option<Vec<Rgb>> {
    let trimmed = value.trim();
    if !trimmed.contains(':') {
        return None;
    }
    let parts: Vec<&str> = trimmed.split(':').map(str::trim).collect();
    if !matches!(parts.len(), 8 | 16) {
        return None;
    }
    parts.into_iter().map(Rgb::parse).collect()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            font_family: font::FAMILY.to_string(),
            font_family_bold: None,
            font_family_italic: None,
            font_family_bold_italic: None,
            font_size: 13.0,
            // v2.28.0 (user-requested): TokyoNight Night is the shipped default
            // theme (it is also the unknown-theme-name fallback, so default ==
            // fallback). Was Catppuccin Mocha.
            theme_name: "TokyoNight Night".to_string(),
            theme: Theme::by_name("TokyoNight Night"),
            scrollback: 10_000,
            scrollback_bytes: DEFAULT_SCROLLBACK_BYTES,
            padding_x: 8.0,
            padding_y: 8.0,
            background_opacity: 1.0,
            cursor_style: CursorStyle::Block,
            cursor_blink: true,
            bell: BellMode::Both,
            osc52: Osc52::Copy,
            modify_other_keys: ModifyOtherKeysMode::Enter,
            paste_files: PasteFiles::On,
            paste_images: PasteImages::On,
            record: RecordMode::Off,
            record_dir: None,
            record_raw_input: false,
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
            clipboard_paste_protection: true,
            putty_paste_style: false,
            smart_copy: true,
            invert_search: false,
            search_wrap: true,
            vim_menu_nav: true,
            search_case_sensitive: SearchCaseSensitivity::Smart,
            term: "xterm-256color".to_string(),
            colorterm: "truecolor".to_string(),
            env: Vec::new(),
            login_shell: false,
            shell_integration: true,
            exit_action: ExitAction::Close,
            agent_server: AgentServer::Off,
            ask_before_closing: AskBeforeClosing::MultipleTerminals,
            close_button_on_tab: true,
            new_tab_after_current_tab: false,
            title_at_bottom: false,
            scroll_tabbar: true,
            tab_min_width: 120.0,
            hide_on_lose_focus: false,
            sticky: false,
            hide_from_taskbar: false,
            backspace_binding: BackspaceBinding::AsciiDel,
            delete_binding: DeleteBinding::EscapeSequence,
            broadcast_default: BroadcastDefault::Group,
            use_custom_url_handler: false,
            custom_url_handler: String::new(),
            use_custom_command: true,
            inactive_color_offset: 1.0,
            inactive_bg_color_offset: 1.0,
            split_to_group: false,
            autoclean_groups: true,
            always_split_with_profile: false,
            focus: FocusMode::Click,
            handle_size: -1,
            window_state: WindowState::Normal,
            window_width: None,
            window_height: None,
            window_position_x: None,
            window_position_y: None,
            gpu_power_preference: GpuPowerPreference::default(),
            gpu_backend: GpuBackend::default(),
            gpu_vendor_id: 0,
            gpu_device_id: 0,
            gpu_name: String::new(),
            gpu_force_software: false,
            geometry_hinting: false,
            extra_styling: true,
            force_no_bell: false,
            log_strip_ansi: false,
            theme_mode: ThemeMode::Explicit,
            theme_schedule: None,
            theme_schedule_lat: None,
            theme_schedule_long: None,
            tab_bar_width: 180.0,
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
            text_renderer: TextRenderer::default(),
            background_animation: BackgroundAnimation::default(),
            chrome_background: ChromeBackground::default(),
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
            agent_badge: "[agent] ".to_string(),
            tab_format: "{n}: {title}".to_string(),
            scrollbar: ScrollbarMode::Auto,
            scrollbar_width: 6.0,
            resize_overlay: ResizeOverlayMode::AfterFirst,
            split_divider_color: None,
            focused_split_color: None,
            accent_color: None,
            // Peacock accents are the default — each
            // window gets a distinct theme hue (`accent-color = theme` opts
            // back into the single static accent).
            accent_auto: true,
            accent_seed: 0,
            cursor_blink_interval: 530,
            tab_silence_threshold_ms: 10_000,
            command_notify_threshold_ms: 5_000,
            copy_on_select: true,
            update_policy: UpdatePolicy::Auto,
            update_check_interval_hours: 24,
            restore_session: false, // fresh-by-default; opt in to restore
            scroll_on_keystroke: true,
            scroll_on_output: false,
            mouse_hide_while_typing: true,
            word_delimiters: String::new(),
            font_ligatures: true,
            font_features: Vec::new(),
            search_foreground: None, // derive from theme.background
            search_background: None, // derive from theme.palette[3]
            keybinds: keybinds::defaults(),
            shell: None,
            ssh_hosts: Vec::new(),
            triggers: Vec::new(),
            menu_items: Vec::new(),
        }
    }
}

/// Decode raw config-file bytes into text, honoring a leading
/// byte-order mark. PowerShell 5.1's `>` redirect writes UTF-16 LE with a
/// BOM, which plain UTF-8 reads reject — so a config created via the
/// documented `kettle --print-default-config > config` one-liner in 5.1 was
/// silently ignored. Detects the UTF-16 LE/BE BOMs and decodes them; UTF-8
/// (with or without a BOM) is decoded lossily so one stray byte can't drop
/// the whole file. A UTF-8 BOM is left in place for `parse()` to strip.
fn decode_config_text(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        [0xFE, 0xFF, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Peacock: the theme's deduped pool of
/// distinct accent hues — the candidate set `peacock_accent` indexes, public
/// so the multi-window LIVE dedupe can walk it (same project → same color,
/// but two windows never share a hue while the pool has a free one).
///
/// The spread covers the theme's most distinct hues, signature accent first.
/// Dedup preserves order: for a theme without an explicit
/// `accent` line `accent == palette[4]`, which would double blue's share and
/// shrink the pool; palettes that repeat hues (magenta == bright magenta)
/// collapse too. Pure; never empty.
pub fn peacock_pool(theme: &Theme) -> Vec<crate::color::Rgb> {
    let raw = [
        theme.accent,      // the signature (mauve on Mocha)
        theme.palette[4],  // blue
        theme.palette[2],  // green
        theme.palette[3],  // yellow
        theme.palette[1],  // red
        theme.palette[6],  // cyan/teal
        theme.palette[5],  // magenta/pink
        theme.palette[13], // bright magenta
    ];
    let mut candidates: Vec<crate::color::Rgb> = Vec::with_capacity(raw.len());
    for c in raw {
        if !candidates.contains(&c) {
            candidates.push(c);
        }
    }
    candidates
}

/// Peacock: pick a distinct, theme-appropriate accent for `seed`
/// (a hash of the window's working directory) from [`peacock_pool`]. Pure.
fn peacock_accent(theme: &Theme, seed: u64) -> crate::color::Rgb {
    let candidates = peacock_pool(theme);
    candidates[(seed % candidates.len() as u64) as usize]
}

impl Config {
    /// The effective UI-chrome accent (focus border, active tab,
    /// titlebars, menu/settings highlights), resolved in precedence:
    ///   1. an explicit `accent-color = <hex>` / `--accent` (`accent_color`),
    ///   2. `accent-color = auto` → a Peacock color varied by `accent_seed`
    ///      (a hash of the window's working directory), so a window in a
    ///      different project is a different color while one project stays
    ///      consistent across launches,
    ///   3. the THEME's signature accent (`theme.accent` — Catppuccin Mocha's
    ///      mauve, matching the app icon; `palette[4]` for themes without one).
    ///
    /// Pure + theme-aware; the renderer calls this with the active theme.
    pub fn resolved_accent(&self, theme: &Theme) -> crate::color::Rgb {
        if let Some(c) = self.accent_color {
            return c;
        }
        if self.accent_auto {
            return peacock_accent(theme, self.accent_seed);
        }
        theme.accent
    }

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

    /// Resolve a named-profile config path. Returns
    /// `<config-dir>/profiles/<sanitized>.config`. Name is sanitized to
    /// `[A-Za-z0-9._-]` so a `--profile ../../etc/passwd` can't
    /// traverse out of the profiles directory. Returns `None` if the
    /// config dir isn't resolvable.
    ///
    /// Used by the kettle binary's `--profile NAME` flag: when set,
    /// kettle loads the named-profile config file instead of the
    /// default `<config-dir>/config`. Distinct from
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

    /// Terminator parity (terminatorlib/terminator.py:
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

    /// Companion to `path_for_profile`: extract the profile
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
    /// continues to the next variable. Previously,
    /// `XDG_CONFIG_HOME=""` (rare but possible in stripped CI
    /// containers or after a misconfigured `unset`/`export X=`)
    /// returned `Some(PathBuf::from(""))` from the first arm, and
    /// the final path became `"kettle/config"` — a *relative* path
    /// that could pick up a stray `kettle/config` file in whatever
    /// directory the user launched kettle from. Same shape as
    /// `home_dir_fallback`, applied here to the
    /// config-path probe.
    pub(crate) fn default_path_from(
        lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Option<PathBuf> {
        let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
        // `XDG_CONFIG_HOME` is the explicit cross-platform override on every OS.
        // Config split-brain: the per-OS fallback then differs.
        // On Windows the canonical per-user dir is `%APPDATA%\kettle` — a stray
        // `HOME` (git-bash / MSYS / WSL-interop all export one) must NOT redirect
        // the GUI to `~/.config`, or a Start-menu launch (no HOME) and a shell
        // launch (HOME set) read DIFFERENT config + session files (the user hit
        // exactly this: a `~/.config/kettle/session.json` with a stale theme while
        // `%APPDATA%` had the right one). On Unix, `HOME/.config` is the standard
        // XDG fallback. A Windows user who genuinely wants `~/.config` sets
        // `XDG_CONFIG_HOME` (honored above on all platforms).
        let os_fallback = || {
            if cfg!(windows) {
                var("APPDATA").map(PathBuf::from)
            } else {
                var("HOME").map(|h| PathBuf::from(h).join(".config"))
            }
        };
        let base = var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(os_fallback)?;
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
    /// Reads through the same hardened `read_config_bytes` helper the
    /// edit path (`ConfigDocument::read`, used by `persist_config_toggle` /
    /// `append_keybind`) uses, rather than a raw `std::fs::metadata` +
    /// `std::fs::read`. That matters here specifically because this is the
    /// path every normal startup and every `ReloadConfig` goes through: a
    /// separate `metadata()` size check followed by a plain `read()` is a
    /// classic TOCTOU (the file can grow between the two calls, so the cap
    /// below was only advisory) and gave no protection if the resolved path
    /// is, or is swapped for, a FIFO or other special file — `std::fs::read`
    /// would then block or stream unbounded instead of erroring. Bound the
    /// read at 1 MiB. Real configs top out around 50 KB (the bundled
    /// `docs/kettle.example.config` is 10 KB); 1 MiB is a ~20× margin over
    /// the bundled example and ~100× over typical user configs while
    /// staying small enough to detect a swap-attack blob before any
    /// allocation. Same defense-in-depth shape as `MAX_SESSION_BYTES`
    /// (session.json) and `MAX_BG_IMAGE_BYTES` (bg-image).
    pub fn load_from_with_diagnostics(path: &Path) -> (Config, Vec<String>, Vec<String>) {
        // Decode by BOM rather than `read_to_string`, which hard-fails on a
        // non-UTF-8 file. A Windows user who runs the documented `kettle
        // --print-default-config > config` in **PowerShell 5.1** gets a
        // UTF-16-LE-with-BOM file (5.1's `>` default encoding); `read_to_string`
        // rejected it as invalid UTF-8, so the config was silently dropped and
        // the user's settings just "didn't apply" with no visible reason.
        // `decode_config_text` honors the UTF-16 LE/BE BOMs and otherwise
        // decodes UTF-8 lossily (more forgiving than a hard fail on a single
        // stray byte).
        //
        // Resolve a symlinked config to its regular-file target first, exactly
        // as `ConfigEdit::begin` does before writing. Dotfile managers (GNU
        // Stow, chezmoi, a manual `ln -s`) routinely make
        // `$XDG_CONFIG_HOME/kettle/config` a symlink into a tracked repo; the
        // low-level `read_config_bytes` reader is deliberately `O_NOFOLLOW`, so
        // without this the read and write paths would disagree — edits would
        // land on the real target while startup silently loaded all defaults.
        // Fall back to the raw path when canonicalization fails (a missing
        // config, the common case, then yields the NotFound handled below as
        // defaults; a non-regular target is rejected by the reader itself).
        let read_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        match read_config_bytes(&read_path) {
            Ok(bytes) => {
                let text = decode_config_text(&bytes);
                let (cfg, unknown) = Self::parse_collect(&text);
                let malformed = Self::detect_malformed_values(&text);
                (cfg, unknown, malformed)
            }
            // `read_config_bytes` folds the size + cap into the error
            // message itself (`InvalidData`), so surface it as-is rather
            // than re-deriving the byte count from a second `metadata()`
            // call.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                log::warn!("{e}; using defaults");
                (Config::default(), Vec::new(), Vec::new())
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
    /// Every boolean config key (both kebab- and snake-case spellings) that
    /// `parse_collect` routes through `parse_bool`. The
    /// `--check-config` diagnostic used to validate only 8 of these, so a typo
    /// in any of the other ~90 (`borderless = treu`, `login-shell = yse`)
    /// passed validation and then silently kept the default at runtime. Kept in
    /// lockstep with `parse_collect` by `bool_and_enum_typos_are_all_flagged`.
    const BOOL_KEYS: &'static [&'static str] = &[
        "allow-bold",
        "allow_bold",
        "always-on-top",
        "always-split-with-profile",
        "always_on_top",
        "always_split_with_profile",
        "audible-bell",
        "audible_bell",
        "autoclean-groups",
        "autoclean_groups",
        "gpu-force-software",
        "gpu_force_software",
        "background-blur",
        "background_blur",
        "bold-is-bright",
        "bold_is_bright",
        "borderless",
        "check-for-updates",
        "clear-select-on-copy",
        "clear_select_on_copy",
        "clipboard-paste-protection",
        "clipboard_paste_protection",
        "close-button-on-tab",
        "close_button_on_tab",
        "record-raw-input",
        "record_raw_input",
        "copy-on-select",
        "copy-on-selection",
        "copy_on_select",
        "copy_on_selection",
        "cursor-blink",
        "cursor-color-default",
        "cursor-style-blink",
        "cursor_blink",
        "cursor_color_default",
        "detachable-tabs",
        "detachable_tabs",
        "disable-mouse-paste",
        "disable-mousewheel-zoom",
        "disable_mouse_paste",
        "disable_mousewheel_zoom",
        "extra-styling",
        "extra_styling",
        "force-no-bell",
        "force_no_bell",
        "full-screen",
        "full_screen",
        "geometry-hinting",
        "geometry_hinting",
        "hide-from-taskbar",
        "hide-on-lose-focus",
        "hide_from_taskbar",
        "hide_on_lose_focus",
        "icon-bell",
        "icon_bell",
        "invert-search",
        "invert_search",
        "link-single-click",
        "link_single_click",
        "log-strip-ansi",
        "log_strip_ansi",
        "login-shell",
        "login_shell",
        "shell-integration",
        "shell_integration",
        "mouse-autohide",
        "mouse-hide",
        "mouse-hide-while-typing",
        "mouse_autohide",
        "new-tab-after-current-tab",
        "new_tab_after_current_tab",
        "putty-paste-style",
        "putty-paste-style-source-clipboard",
        "putty_paste_style",
        "putty_paste_style_source_clipboard",
        "scroll-on-input",
        "scroll-on-keystroke",
        "scroll-on-output",
        "scrollback-infinite",
        "scrollback_infinite",
        "scroll-tabbar",
        "scroll_tabbar",
        "search-wrap",
        "search_wrap",
        "show-titlebar",
        "show_titlebar",
        "smart-copy",
        "smart_copy",
        "split-to-group",
        "split_to_group",
        "sticky",
        "title-at-bottom",
        "title-hide-sizetext",
        "title-use-system-font",
        "title_at_bottom",
        "title_hide_sizetext",
        "title_use_system_font",
        "update-check",
        "restore-session",
        "restore_session",
        "urgent-bell",
        "urgent_bell",
        "use-custom-command",
        "use-custom-url-handler",
        "use-system-font",
        "use-theme-colors",
        "use_custom_command",
        "use_custom_url_handler",
        "use_system_font",
        "use_theme_colors",
        "vim-menu-nav",
        "vim_menu_nav",
        "visible-bell",
        "visible_bell",
    ];

    pub fn detect_malformed_values(text: &str) -> Vec<String> {
        let mut bad = Vec::new();
        // Strip the leading UTF-8 BOM (also handled in
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
            // An INI section header is structure, not a malformed assignment.
            // Terminator's config is sectioned, so scanning for a bare `=` on
            // every line reported a wall of errors on a file that is perfectly
            // well-formed — telling the user to fix lines that are supposed to
            // be there. Recognized with the same helper the tokenizer uses, so
            // the two cannot disagree about what a header looks like.
            if parse::section_header(line).is_some() {
                continue;
            }
            if !line.contains('=') {
                bad.push(format!("missing `=` separator: {line:?}"));
            }
        }
        for e in parse::parse(text) {
            let v = &e.value;
            // Empty values are documented (parse.rs) as
            // "reset to default" semantics. The
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
                // `cap_axis_cells` helper keeps screenshots safe.
                // Require a FINITE value. The apply arm
                // (`v.is_finite()`) rejects `inf`/`nan`, but `"inf".parse::
                // <f32>()` succeeds, so the diagnostic said OK while the
                // runtime silently kept the default — the exact mismatch
                // the clamped-numeric arms exist to prevent.
                "padding-x"
                | "padding_x"
                | "window-padding-x"
                | "window_padding_x"
                | "padding-y"
                | "padding_y"
                | "window-padding-y"
                | "window_padding_y" => v.parse::<f32>().is_ok_and(|n| n.is_finite()),
                // Numerics with a *runtime clamp* — parse AND land
                // inside the clamp range, otherwise the user's
                // `--check-config` value disagreed with what the
                // runtime actually used. An earlier fix caught this
                // for `font-size`; this extends to every other
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
                "scrollbar-width" | "scrollbar_width" => {
                    v.parse::<f32>().is_ok_and(|n| (2.0..=40.0).contains(&n))
                }
                "tab-min-width" | "tab_min_width" => {
                    v.parse::<f32>().is_ok_and(|n| (40.0..=600.0).contains(&n))
                }
                "minimum-contrast" => v.parse::<f32>().is_ok_and(|n| (0.0..=21.0).contains(&n)),
                // Special: scrollback accepts unlimited/infinite/0 as
                // "no cap" plus any non-negative integer up to
                // `INFINITE_SCROLLBACK` (10 M lines). Values above
                // would have allocated >100 GB of history rows
                // (clamped at parse, but flag the
                // diagnostic too).
                // The apply arm accepts the
                // `scrollback-limit` alias too, so the diagnostic must
                // recognise it — otherwise `scrollback-limit = 99999999999`
                // bypassed the malformed-value warning entirely.
                "scrollback" | "scrollback-limit" | "scrollback-lines" => {
                    v.eq_ignore_ascii_case("infinite")
                        || v.eq_ignore_ascii_case("unlimited")
                        || v.parse::<usize>().is_ok_and(|n| n <= INFINITE_SCROLLBACK)
                }
                "scrollback-bytes" | "scrollback-byte-limit" | "scrollback-memory" => {
                    parse_byte_size(v).is_some_and(|n| n <= MAX_SCROLLBACK_BYTES)
                }
                // Same shape as the float-range checks above:
                // parse_collect clamps to [50, 5000], so
                // `cursor-blink-interval = 99999` silently becomes
                // 5000 — surface it now so the user's diagnostic
                // matches their runtime.
                "cursor-blink-interval" => v.parse::<u64>().is_ok_and(|n| (50..=5000).contains(&n)),
                // The notification thresholds are clamped at
                // parse (`tab-silence` to [1000, 600000]; `command-notify` to
                // [0, 86_400_000] with 0 = disable) but had no diagnostic, so an
                // out-of-range value silently became something else. Bounds
                // mirror the apply-arm clamps exactly.
                "tab-silence-threshold-ms" | "tab-silence-threshold" => {
                    v.parse::<u64>().is_ok_and(|n| (1_000..=600_000).contains(&n))
                }
                "command-notify-threshold-ms"
                | "command-notify-threshold"
                | "command_notify_threshold_ms"
                | "command_notify_threshold" => {
                    v.parse::<u64>().is_ok_and(|n| n <= 86_400_000)
                }
                // The remaining clamped/range-checked
                // numerics. parse_collect clamps (or, for lat/long, silently
                // discards) an out-of-range value, so without these the
                // diagnostic said OK while the runtime used something else —
                // the exact silent-fallback trap the font-size clamp
                // diagnostic (above) set out to close. Bounds mirror the parse_collect clamp/range arms.
                "handle-size" | "handle_size" => {
                    v.parse::<i32>().is_ok_and(|n| (-1..=50).contains(&n))
                }
                "window-width" | "window_width" => v
                    .parse::<u32>()
                    .is_ok_and(|n| (WINDOW_WIDTH_MIN..=WINDOW_WIDTH_MAX).contains(&n)),
                "window-height" | "window_height" => v
                    .parse::<u32>()
                    .is_ok_and(|n| (WINDOW_HEIGHT_MIN..=WINDOW_HEIGHT_MAX).contains(&n)),
                "window-position-x" | "window_position_x" | "window-position-y"
                | "window_position_y" => v.parse::<i32>().is_ok(),
                "tab-bar-width" | "tab_bar_width" => {
                    v.parse::<f32>().is_ok_and(|n| (40.0..=600.0).contains(&n))
                }
                "background-darkness" | "background_darkness" => {
                    v.parse::<f32>().is_ok_and(|n| (0.0..=1.0).contains(&n))
                }
                "cell-height" | "cell_height" | "cell-width" | "cell_width" => {
                    v.parse::<f32>().is_ok_and(|n| (0.5..=3.0).contains(&n))
                }
                "inactive-color-offset"
                | "inactive_color_offset"
                | "inactive-bg-color-offset"
                | "inactive_bg_color_offset" => {
                    v.parse::<f32>().is_ok_and(|n| (0.0..=1.0).contains(&n))
                }
                // theme-schedule-lat/long are *range-checked* (out-of-range is
                // discarded, leaving the schedule unset) — the doc at the
                // `theme_schedule_lat` field even promises this diagnostic.
                "theme-schedule-lat" | "theme_schedule_lat" => {
                    v.parse::<f64>().is_ok_and(|n| (-90.0..=90.0).contains(&n))
                }
                "theme-schedule-long"
                | "theme_schedule_long"
                | "theme-schedule-lon"
                | "theme_schedule_lon"
                | "theme-schedule-longitude"
                | "theme_schedule_longitude" => {
                    v.parse::<f64>().is_ok_and(|n| (-180.0..=180.0).contains(&n))
                }
                // Color keys: `Rgb::parse` accepts `#RRGGBB`, `rgb:RR/GG/BB`,
                // X11 names ("red"), etc. Bad values otherwise silently
                // keep the default — same trap as the numeric keys.
                "background"
                | "foreground"
                // The apply path accepts the Terminator
                // `background-color`/`foreground-color` (and `_color`) aliases,
                // so a bad color under those spellings must be diagnosed too —
                // otherwise `background-color = notacolor` silently kept the
                // theme default and passed `--check-config`.
                | "background-color"
                | "background_color"
                | "foreground-color"
                | "foreground_color"
                | "cursor-color"
                | "cursor-bg-color"
                | "cursor_bg_color"
                | "cursor-fg-color"
                | "cursor_fg_color"
                | "selection-background"
                | "selection-foreground"
                | "search-foreground"
                | "search-background"
                | "split-divider-color"
                | "focused-split-color"
                | "split-divider-color-focused"
                // Accent + per-pane titlebar colors were
                // silently keeping the default on a typo too.
                | "title-transmit-bg-color"
                | "title_transmit_bg_color"
                | "title-receive-bg-color"
                | "title_receive_bg_color"
                | "title-inactive-bg-color"
                | "title_inactive_bg_color"
                | "title-transmit-fg-color"
                | "title_transmit_fg_color"
                | "title-receive-fg-color"
                | "title_receive_fg_color"
                | "title-inactive-fg-color"
                | "title_inactive_fg_color" => Rgb::parse(v).is_some(),
                // `accent-color` accepts a hex color, `auto`
                // (Peacock — vary by working directory + window; the
                // default), or `theme`/`off`/`none` (static theme accent).
                "accent-color" | "accent_color" => {
                    let t = v.trim();
                    t.eq_ignore_ascii_case("auto")
                        || t.eq_ignore_ascii_case("theme")
                        || t.eq_ignore_ascii_case("off")
                        || t.eq_ignore_ascii_case("none")
                        || Rgb::parse(t).is_some()
                }
                // `keybind = <trigger>=<action>` — both halves have to
                // parse (same predicate `apply_keybind` uses, just split
                // so we know which half failed). A user typo on either
                // side silently drops the binding without this guard.
                // The action half also accepts the unbind sentinels
                // (`unbind`, `none`, `null`, `false`, empty) — those
                // mean "remove this default trigger", not "malformed".
                // Split on the LAST `=` to agree with
                // apply_keybind — else rebinding the `=` key (`ctrl+==…`) was
                // both dropped AND flagged here as a false-positive "malformed".
                "keybind" => v.rsplit_once('=').is_some_and(|(t, a)| {
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
                // pre-filtered by the empty-value skip above.)
                "theme" => {
                    // `find_name` compares with
                    // `eq_ignore_ascii_case` — no per-name `to_ascii_lowercase`
                    // String alloc over the ~512 bundled themes (the sibling
                    // light/dark arm already does this; an earlier alloc-avoidance
                    // pass missed this mirror).
                    Theme::find_name(v.trim()).is_some()
                }
                // A malformed `theme-schedule` (bad HH:MM, bad
                // mode word, missing comma) makes `parse_theme_schedule` return
                // None and the schedule is silently unset — but the lat/long
                // sub-keys WERE diagnosed, so omitting the schedule string itself
                // was inconsistent.
                "theme-schedule" | "theme_schedule" => parse_theme_schedule(v).is_some(),
                // `ask-before-closing` typo silently fell back
                // to the default with no warning.
                "ask-before-closing" | "ask_before_closing" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "always" | "never" | "multiple" | "multiple-terminals" | "multiple_terminals"
                ),
                // Light/dark-theme must name a real bundled
                // theme (used by auto-theme); a typo silently kept the prior.
                "light-theme" | "light_theme" | "dark-theme" | "dark_theme" => {
                    Theme::find_name(v.trim()).is_some()
                }
                // Enum keys whose apply arm has a
                // `_ => DefaultVariant` fallthrough — a typo silently took the
                // default with no warning. Validate against the explicit variant
                // set plus the default's conventional spelling.
                "status-bar" | "statusbar" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off" | "false" | "none" | "top" | "bottom" | "true" | "on"
                ),
                "exit-action" | "exit_action" => {
                    matches!(v.to_ascii_lowercase().as_str(), "close" | "restart" | "hold")
                }
                "agent-server" | "agent_server" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off" | "read-only" | "read_only" | "readonly" | "full"
                ),
                "backspace-binding" | "backspace_binding" | "delete-binding"
                | "delete_binding" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "ascii-del"
                        | "ascii_del"
                        | "control-h"
                        | "ctrl-h"
                        | "control_h"
                        | "escape-sequence"
                        | "escape_sequence"
                        | "automatic"
                        | "auto"
                ),
                "broadcast-default" | "broadcast_default" => {
                    matches!(v.to_ascii_lowercase().as_str(), "all" | "off" | "none" | "group")
                }
                "theme-mode" | "theme_mode" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "light" | "dark" | "auto" | "system" | "follow-system" | "follow_system"
                        | "explicit"
                ),
                "background-type" | "background_type" => {
                    matches!(
                        v.to_ascii_lowercase().as_str(),
                        "solid" | "image" | "starfield" | "transparent"
                    )
                }
                // The three background-image placement enums
                // were stored verbatim and consumed by a renderer `match` with a
                // silent `_ =>` fallback (mode falls back to stretch_and_fill;
                // align falls back to center/middle) — a typo like `mode = tyle`
                // passed `--check-config` yet placed the image wrong. Pin the
                // accepted value set (matching kettle-render's match + the docs).
                "background-image-mode" | "background_image_mode" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "tile" | "center" | "scale" | "stretch_and_fill"
                ),
                "background-image-align-horiz" | "background_image_align_horiz" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "left" | "center" | "right"
                ),
                "background-image-align-vert" | "background_image_align_vert" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "top" | "middle" | "bottom"
                ),
                "text-renderer" | "text_renderer" => {
                    matches!(v.to_ascii_lowercase().as_str(), "grid" | "legacy")
                }
                "background-animation" | "background_animation" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "when-focused"
                        | "when_focused"
                        | "focused"
                        | "always"
                        | "true"
                        | "on"
                        | "yes"
                        | "off"
                        | "false"
                        | "no"
                        | "none"
                        | "static"
                ),
                "chrome-background" | "chrome_background" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "theme" | "auto" | "black" | "white"
                ),
                "lua-sandbox" | "lua_sandbox" => {
                    matches!(v.to_ascii_lowercase().as_str(), "safe" | "trusted" | "unsafe")
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
                // Accepts it as an alias so a user copying
                // their Alacritty config doesn't get a silent
                // fallback to Block. Case-insensitive so
                // `Block` / `BLOCK` etc. also pass (matching the
                // parse_collect behavior).
                // Include the `cursor-shape`/`cursor_shape`
                // Terminator aliases (the apply arm accepts them) so a typo
                // under those spellings is diagnosed instead of silently
                // becoming Block; and add `ibeam`/`i-beam`, which the apply
                // arm accepts but the diagnostic previously flagged as
                // malformed — a false positive that failed `--check-config`
                // on a valid value.
                "cursor-style" | "cursor-shape" | "cursor_shape" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "block" | "underline" | "bar" | "beam" | "ibeam" | "i-beam"
                ),
                // Enum keys are case-insensitive so
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
                "modify-other-keys" | "modify_other_keys" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "enter" | "off"
                ),
                "paste-files" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off" | "none" | "disabled" | "false" | "0" | "on" | "enabled" | "true" | "1"
                ),
                "record" => matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "off" | "none" | "disabled" | "false" | "0" | "on" | "enabled" | "true" | "1"
                        | "yes"
                ),
                // Boolean keys: accept the same alias set `parse_bool`
                // recognizes. Previously, any non-"false"
                // string silently meant "true", so typos like
                // `cursor-style-blink = no` quietly enabled the blink.
                // Validate the WHOLE bool-key set (was only
                // 8 of ~100), so `borderless = treu` etc. are caught too.
                // Compare in the parser's folded spelling. `BOOL_KEYS` lists
                // both spellings of many keys for documentation and coverage
                // asserts, and the tokenizer now yields only the hyphenated
                // form — a raw `contains` would silently stop validating every
                // underscore-spelled entry, so a typo'd `allow_bold = treu`
                // would go back to being accepted in silence.
                k if Self::BOOL_KEYS
                    .iter()
                    .any(|listed| listed.replace('_', "-") == k) =>
                {
                    parse_bool(v).is_some()
                }
                // Enum keys that previously fell through to
                // `_ => true` (silently kept their default on a typo).
                "focus" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "sloppy" | "system" | "click"
                ),
                "window-state" | "window_state" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "maximise" | "maximize" | "fullscreen" | "hidden" | "normal"
                ),
                "gpu-power-preference" | "gpu_power_preference" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "low"
                        | "low-power"
                        | "integrated"
                        | "high"
                        | "high-performance"
                        | "discrete"
                        | "auto"
                        | "automatic"
                        | "none"
                ),
                "gpu-backend" | "gpu_backend" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "auto" | "dx12" | "d3d12" | "directx12" | "vulkan" | "vk" | "metal" | "gl"
                        | "opengl" | "gles"
                ),
                "gpu-vendor-id" | "gpu_vendor_id" | "gpu-device-id" | "gpu_device_id" => {
                    parse_hex_or_dec_u32(v).is_some()
                }
                "search-case-sensitive"
                | "search_case_sensitive"
                | "case-sensitive"
                | "case_sensitive" => {
                    matches!(
                        v.to_ascii_lowercase().as_str(),
                        "smart" | "auto" | "always" | "sensitive" | "never" | "insensitive"
                    ) || parse_bool(v).is_some()
                }
                "tab-bar" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "off" | "none" | "false" | "auto" | "always"
                ),
                "tab-bar-position" | "tab-position" | "tab_position" => {
                    // Terminator parity (terminatorlib/config.py:144
                    // `tab_position` accepts
                    // top/left/right/bottom/hidden). kettle accepts top +
                    // bottom natively, treats `hidden` as the well-known
                    // alias for `tab-bar = off` (the separate visibility
                    // key). `left`/`right` would require a vertical-tab-bar
                    // render-layer change (Bucket C in docs/TERMINATOR-AUDIT.md);
                    // accepted here so a config copied from Terminator doesn't
                    // fail --check-config, but the runtime falls through to
                    // top with a log::warn. Also added the Terminator-
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
                "update-policy" | "update_policy" => matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "off" | "notify" | "auto"
                ),
                "update-check-interval-hours" | "update_check_interval_hours" => {
                    // Mirror the apply-arm `.max(1)` clamp: `0` is silently
                    // floored to 1 at runtime, so flag it rather than letting
                    // `--check-config` disagree with the effective value.
                    v.trim().parse::<u32>().is_ok_and(|n| n >= 1)
                }
                "resize-overlay" | "resize_overlay" => matches!(
                    v.to_ascii_lowercase().as_str(),
                    "never" | "off" | "false" | "always" | "on" | "true" | "after-first"
                        | "after_first"
                ),
                // `font-feature` is comma-separated; every token must
                // parse via the documented `FontFeature::parse` shape
                // (`liga`, `+calt`, `cv01=2`, etc.). One bad token in
                // the list is enough to flag — that token's silently
                // dropped while the rest apply, leaving the user with
                // a half-applied feature set. Skip
                // empty/whitespace tokens so a trailing comma (`liga,`)
                // or `liga, , calt` isn't a false-positive — the apply
                // path already tolerates them (it `if let Some`-skips).
                "font-feature" => v
                    .split(',')
                    .filter(|t| !t.trim().is_empty())
                    .all(|tok| FontFeature::parse(tok).is_some()),
                // `ssh-host = name=user@host` — requires the `=`
                // separator; without it the entry is silently dropped
                // (no `name` to bind via the Ctrl+Shift+S launcher).
                "ssh-host" => v
                    .split_once('=')
                    .is_some_and(|(n, t)| !n.trim().is_empty() && !t.trim().is_empty()),
                // `palette = N=#hex` — both halves have to parse.
                // `palette = N=#hex`: both halves must parse, AND N
                // must fit the implementation's 0..=15 range.
                // Indices 16..=255 are documented as belonging
                // to the xterm 256-color extension but the runtime
                // apply path only writes `theme.palette[0..16]`. A
                // user writing `palette = 200=#ff0000` (expecting
                // their override to land on the bright-red 256-color
                // cube slot) silently saw no effect; this surfaces
                // the limit so they at least know the override was
                // ignored. The fix is one-sided (docs+diagnostic);
                // adding runtime support for 16..255 means a much
                // bigger Theme/renderer refactor.
                "palette" => {
                    // parse_collect accepts BOTH
                    // `palette = N=#hex` (single-slot override) AND `palette =
                    // NAME` (a Terminator-style named palette via Theme::find_name
                    // with `_`->` ` fallback). The diagnostic only knew the first
                    // form, so a valid bare name was falsely flagged malformed —
                    // the inverse of the silent-fallback trap this check exists for.
                    if let Some((i, h)) = v.split_once('=') {
                        i.trim().parse::<usize>().is_ok_and(|n| n < 16)
                            && Rgb::parse(h.trim()).is_some()
                    } else if v.contains(':') {
                        // Terminator writes its palette as one colon-separated
                        // list of 8 or 16 colours, and `parse_collect` accepts
                        // exactly that. The diagnostic did not, so the palette
                        // out of a real Terminator config applied correctly at
                        // runtime while `--check-config` called it malformed —
                        // sending the user to fix a line that was already
                        // right. Validate with the same parser that applies it,
                        // so the two cannot disagree.
                        parse_colon_palette(v).is_some()
                    } else {
                        let name = v.trim();
                        Theme::find_name(name).is_some()
                            || Theme::find_name(&name.replace('_', " ")).is_some()
                    }
                }
                // Trigger patterns must be valid regex.
                // Without this check, a malformed pattern like
                // `trigger = [unclosed` parses (the config layer
                // stores it as a plain string), `--check-config`
                // reports OK, then at runtime `compile_triggers`
                // fails `Regex::new` and the trigger silently never
                // fires (only a log::warn that the user often
                // doesn't see). Now: surface the malformed regex at
                // check-config time so users see the issue before
                // an event they expected to fire never does.
                //
                // Mirror the apply path's split. A valid
                // trigger is `REGEX :: command` where ONLY
                // the LHS before the `::` separator is the regex — the
                // command can contain regex metacharacters freely.
                // Validating the WHOLE value falsely flagged a line like
                // `foo :: grep bar[baz` (the `[baz` is in the command,
                // not the pattern). Use `parse_trigger_with_command` so
                // the diagnostic and the runtime agree on what's a regex.
                "trigger" => match parse_trigger_with_command(v.trim()) {
                    Some((pat, _)) => regex::Regex::new(&pat).is_ok(),
                    None => regex::Regex::new(v.trim()).is_ok(),
                },
                "env" => parse_env_assignment(v).is_some(),
                "menu-item" | "menu_item" => v
                    .split_once('=')
                    .is_some_and(|(l, c)| !l.trim().is_empty() && !c.trim().is_empty()),
                _ => true,
            };
            if !ok {
                // Echo the user's own spelling. The parser folds `_`→`-`
                // internally, but a diagnostic that renames their key sends
                // them looking for a line their file does not contain.
                bad.push(format!("{} = {:?}", e.raw_key, v));
            }
        }
        bad
    }

    /// Parse, also returning any unrecognized config keys (typo guard,
    /// surfaced by `kettle --check-config` and a startup `log::warn`).
    pub fn parse_collect(text: &str) -> (Config, Vec<String>) {
        let mut cfg = Config::default();
        let mut explicit_palette: Vec<(usize, Rgb)> = Vec::new();
        // Collected during the loop, applied after it — see the arm below.
        let mut scrollback_infinite: Option<bool> = None;
        // Explicit single-color overrides
        // (background / foreground / cursor block + glyph / selection
        // bg + fg) are stashed here during the parse loop and applied
        // AFTER the theme / `explicit_palette` re-apply block below, so
        // an explicit color always wins over a `theme =` regardless of
        // line order. Before this, `background = #ff0000` followed by
        // `theme = Dracula` lost the red because applying the theme
        // overwrote `cfg.theme.background` in place. Precedence:
        // explicit single-color line > palette override > theme.
        let mut explicit_background: Option<Rgb> = None;
        let mut explicit_foreground: Option<Rgb> = None;
        let mut explicit_cursor: Option<Rgb> = None;
        let mut explicit_cursor_text: Option<Rgb> = None;
        let mut explicit_selection_bg: Option<Rgb> = None;
        let mut explicit_selection_fg: Option<Rgb> = None;
        let mut unknown: Vec<String> = Vec::new();
        // Terminator splits bell into two orthogonal
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
        let entries = parse::parse(text);
        // A canonical policy wins regardless of line order. This matters for
        // migrated configs that still carry the legacy boolean below it.
        let has_update_policy = entries
            .iter()
            .any(|entry| matches!(entry.key.as_str(), "update-policy" | "update_policy"));
        for e in entries {
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
                // Terminator's `font = Mono 10` — a Pango font description
                // carrying BOTH family and size, and the single most-set
                // profile key. kettle only had `font-family` + `font-size`, so
                // the whole line was an unrecognised key and both values were
                // lost. Split the trailing size off the description; an
                // explicit `font-family` / `font-size` still wins by
                // precedence, since those arms assign unconditionally and this
                // one only fills what the description carried.
                "font" => {
                    if let Some((family, size)) = parse_font_description(&e.value) {
                        if !family.is_empty() {
                            cfg.font_family = family;
                        }
                        if let Some(size) = size {
                            cfg.font_size = size;
                        }
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
                    // renderer will actually use (`clamp_font_size` is
                    // downstream). Without this,
                    // `--check-config` echoed e.g. `font: ... 500pt`
                    // while the runtime rendered at 72pt — confusing
                    // diagnostics. Surfaces out-of-range as
                    // a warning, and makes the stored value
                    // match reality too. Parse-fail keeps the default.
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
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
                    // falls back to `Theme::default()` — the
                    // self-contained Catppuccin Mocha safety net. Keep
                    // `cfg.theme_name` in sync with the loaded palette:
                    // store the *canonical* bundled name when found, else
                    // "Catppuccin Mocha" to match the fallback. (The
                    // SHIPPED default for a fresh config is TokyoNight
                    // Night via `Config::default`; an INVALID name
                    // resolves to the safety net, not the default.)
                    // Otherwise `--check-config` would echo
                    // `theme: TokyoNitght Night` while the runtime used a
                    // different palette — same shape as the font-size
                    // clamp above (stored value matches runtime). The
                    // malformed-value diagnostic still flags the typo.
                    if !e.value.trim().is_empty() {
                        cfg.theme = Theme::by_name(&e.value);
                        match Theme::find_name(&e.value) {
                            Some(canonical) => cfg.theme_name = canonical.to_string(),
                            None => cfg.theme_name = "Catppuccin Mocha".to_string(),
                        }
                    }
                }
                "background" | "background-color" | "background_color" => {
                    // Terminator parity: kettle's canonical key
                    // is `background`; `background-color` + `background_color`
                    // are accepted as compatibility aliases so a Terminator
                    // config copies in without rename. Same for `foreground`
                    // and the cursor / fullscreen keys below.
                    //
                    // Stash instead of writing
                    // `cfg.theme.background` in place so a later `theme =`
                    // line can't clobber it (applied post-theme below).
                    if let Some(c) = Rgb::parse(&e.value) {
                        explicit_background = Some(c);
                    }
                }
                "foreground" | "foreground-color" | "foreground_color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        explicit_foreground = Some(c);
                    }
                }
                // `cursor-color` / Terminator's `cursor_bg_color` set the cursor
                // BLOCK color; `cursor-fg-color` / `cursor_fg_color` set the
                // color of the glyph UNDER the cursor (theme.cursor_text). The
                // focused block cursor renders solid in the block color with the
                // under-glyph recolored — the standard terminal model.
                "cursor-color" | "cursor-bg-color" | "cursor_bg_color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        explicit_cursor = Some(c);
                    }
                }
                "cursor-fg-color" | "cursor_fg_color" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        explicit_cursor_text = Some(c);
                    }
                }
                "selection-background" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        explicit_selection_bg = Some(c);
                    }
                }
                "selection-foreground" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        explicit_selection_fg = Some(c);
                    }
                }
                "palette" => {
                    if let Some((i, h)) = e.value.split_once('=')
                        && let (Ok(i), Some(c)) = (i.trim().parse(), Rgb::parse(h.trim()))
                    {
                        explicit_palette.push((i, c));
                    } else if let Some(colors) = parse_colon_palette(&e.value) {
                        // Terminator's own spelling — `palette =
                        // "#2e3436:#cc0000:…"`, a colon-separated list of
                        // slots in order. This is the key most Terminator
                        // users customise, and it previously fell into the
                        // named-preset branch below (a colon list contains no
                        // `=`), failed `Theme::find_name`, and applied
                        // nothing: the whole palette silently discarded.
                        for (i, c) in colors.into_iter().enumerate() {
                            explicit_palette.push((i, c));
                        }
                    } else if !e.value.contains('=') {
                        // Terminator parity (palette
                        // named-preset alias): Terminator accepts
                        // `palette = solarized_dark` as a named
                        // preset that picks the whole 16-slot
                        // palette + cursor + selection colors at
                        // once. kettle ships ~512 themes which
                        // are a strict superset of those presets,
                        // so we treat `palette = NAME` as a
                        // shorthand for `theme = NAME` (best-
                        // effort: `solarized_dark` → `Solarized
                        // Darcula` or the closest bundled match
                        // via the case-insensitive
                        // find_name).
                        let v = e.value.trim();
                        // Try direct match first, then underscore
                        // → space (Terminator uses `_`; kettle
                        // bundled names use spaces).
                        let candidate = Theme::find_name(v).or_else(|| {
                            let spaced = v.replace('_', " ");
                            Theme::find_name(&spaced)
                        });
                        if let Some(name) = candidate {
                            cfg.theme_name = name.to_string();
                            cfg.theme = Theme::by_name(name);
                        }
                    }
                }
                "search-foreground" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.search_foreground = Some(c);
                    }
                }
                "search-background" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.search_background = Some(c);
                    }
                }
                // Terminator names it `scrollback_lines` (config.py:242), and
                // `scrollback_infinite` (config.py:243) is its unbounded flag,
                // which kettle expresses as the same key set to `infinite`.
                // Neither had an arm, so the scrollback size out of a copied
                // config was an unknown key that quietly did nothing.
                "scrollback-limit" | "scrollback" | "scrollback-lines" => {
                    let v = e.value.trim().to_ascii_lowercase();
                    // `0` / `infinite` / `unlimited` => effectively unbounded
                    // history (capped high to keep memory bounded).
                    if v == "infinite" || v == "unlimited" || v == "0" {
                        cfg.scrollback = INFINITE_SCROLLBACK;
                    } else if let Ok(n) = v.parse::<usize>() {
                        // Clamp at `INFINITE_SCROLLBACK`: a
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
                // Terminator's unbounded-history flag. kettle expresses the
                // same thing as `scrollback = infinite`. Terminator treats the
                // boolean as an override of the line count, so it must not
                // depend on which line comes first in the file — applied after
                // the loop for that reason.
                "scrollback-infinite" => {
                    if let Some(b) = parse_bool(&e.value) {
                        scrollback_infinite = Some(b);
                    }
                }
                "scrollback-bytes" | "scrollback-byte-limit" | "scrollback-memory" => {
                    if let Some(n) = parse_byte_size(&e.value) {
                        cfg.scrollback_bytes = n.min(MAX_SCROLLBACK_BYTES);
                    }
                }
                // Accept the bare `padding-x`/`-y` spellings
                // as aliases. The malformed-value diagnostic already listed them
                // as valid keys, so without these aliases a bare `padding-x`
                // both passed `--check-config` AND warned "unrecognized key"
                // while doing nothing — a contradictory diagnostic.
                "window-padding-x" | "padding-x" | "padding_x" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.padding_x = v;
                    }
                }
                "window-padding-y" | "padding-y" | "padding_y" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
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
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.background_opacity = v.clamp(0.0, 1.0);
                    }
                }
                "cursor-style" | "cursor-shape" | "cursor_shape" => {
                    // Terminator parity: Terminator's
                    // `cursor_shape` is kettle's `cursor-style`. Same
                    // enum values (`block` / `underline` / `bar`).
                    cfg.cursor_style = match e.value.to_ascii_lowercase().as_str() {
                        "underline" => CursorStyle::Underline,
                        // `beam` is Alacritty's name for the same
                        // vertical-bar cursor; the
                        // alias was added so Alacritty refugees don't get a
                        // silent Block fallback.
                        "bar" | "beam" | "ibeam" | "i-beam" => CursorStyle::Bar,
                        _ => CursorStyle::Block,
                    }
                }
                // Every boolean config key used `e.value !=
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
                    // Terminator parity (config.py:165
                    // `cursor_blink`): Terminator's bool maps to
                    // kettle's `cursor-style-blink`. Default true
                    // matches both.
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.cursor_blink = b;
                    }
                }
                "bell" => {
                    // Lowercase the value so `bell = OFF`
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
                "modify-other-keys" | "modify_other_keys" => {
                    cfg.modify_other_keys = match e.value.to_ascii_lowercase().as_str() {
                        "off" => ModifyOtherKeysMode::Off,
                        _ => ModifyOtherKeysMode::Enter,
                    }
                }
                "paste-files" => {
                    cfg.paste_files = match e.value.to_ascii_lowercase().as_str() {
                        "off" | "none" | "disabled" | "false" | "0" => PasteFiles::Off,
                        _ => PasteFiles::On,
                    }
                }
                "paste-images" | "paste-image" => {
                    cfg.paste_images = match e.value.to_ascii_lowercase().as_str() {
                        "off" | "none" | "disabled" | "false" | "0" => PasteImages::Off,
                        _ => PasteImages::On,
                    }
                }
                "record" => {
                    cfg.record = match e.value.trim().to_ascii_lowercase().as_str() {
                        "on" | "true" | "1" | "enabled" | "yes" => RecordMode::On,
                        _ => RecordMode::Off,
                    }
                }
                "record-dir" | "record_dir" => {
                    let dir = e.value.trim();
                    cfg.record_dir = if dir.is_empty() {
                        None
                    } else {
                        Some(PathBuf::from(dir))
                    };
                }
                "record-raw-input" | "record_raw_input" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.record_raw_input = b;
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
                    // Terminator parity (terminatorlib/config.py:144
                    // `tab_position`). Terminator
                    // accepts top/left/right/bottom/hidden. kettle:
                    //   - `top` / `bottom`: native.
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
                        "left" => cfg.tab_bar_pos = TabBarPos::Left,
                        "right" => cfg.tab_bar_pos = TabBarPos::Right,
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
                    // Terminator parity.
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.borderless = b;
                    }
                }
                "always-on-top" | "always_on_top" => {
                    // Terminator parity.
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
                "clipboard-paste-protection" | "clipboard_paste_protection" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.clipboard_paste_protection = b;
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
                "search-wrap" | "search_wrap" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.search_wrap = b;
                    }
                }
                "vim-menu-nav" | "vim_menu_nav" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.vim_menu_nav = b;
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
                "env" => {
                    if let Some(pair) = parse_env_assignment(&e.value) {
                        cfg.env.push(pair);
                    }
                }
                "login-shell" | "login_shell" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.login_shell = b;
                    }
                }
                "shell-integration" | "shell_integration" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.shell_integration = b;
                    }
                }
                "exit-action" | "exit_action" => {
                    cfg.exit_action = match e.value.to_ascii_lowercase().as_str() {
                        "restart" => ExitAction::Restart,
                        "hold" => ExitAction::Hold,
                        _ => ExitAction::Close,
                    };
                }
                "agent-server" | "agent_server" => {
                    cfg.agent_server = match e.value.to_ascii_lowercase().as_str() {
                        "full" => AgentServer::Full,
                        "read-only" | "read_only" | "readonly" => AgentServer::ReadOnly,
                        // Explicit so `--check-config` distinguishes a typo from
                        // the default (the validator above pins the value set).
                        _ => AgentServer::Off,
                    };
                }
                "ask-before-closing" | "ask_before_closing" => {
                    cfg.ask_before_closing = match e.value.to_ascii_lowercase().as_str() {
                        "always" => AskBeforeClosing::Always,
                        "never" => AskBeforeClosing::Never,
                        // Explicit so `--check-config` can tell a real value from
                        // a typo instead of silently mapping
                        // everything to the default.
                        "multiple" | "multiple-terminals" | "multiple_terminals" => {
                            AskBeforeClosing::MultipleTerminals
                        }
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
                "tab-min-width" | "tab_min_width" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.tab_min_width = v.clamp(40.0, 600.0);
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
                "use-custom-command" | "use_custom_command" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.use_custom_command = b;
                    }
                }
                // Terminator parity
                // (terminatorlib/config.py `enabled_plugins`):
                // VTE plugin list. kettle's plugin model is
                // Lua (loaded from `~/.config/kettle/
                // kettle.lua` + per-profile `*.lua` siblings) +
                // menu-item config keys. The Terminator
                // key is accepted without effect so a copied
                // config doesn't trigger `--check-config` warnings.
                "enabled-plugins" | "enabled_plugins" => {}
                "inactive-color-offset" | "inactive_color_offset" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.inactive_color_offset = v.clamp(0.0, 1.0);
                    }
                }
                "inactive-bg-color-offset" | "inactive_bg_color_offset" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
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
                "window-width" | "window_width" => {
                    if let Ok(v) = e.value.parse::<u32>() {
                        cfg.window_width = Some(v.clamp(WINDOW_WIDTH_MIN, WINDOW_WIDTH_MAX));
                    }
                }
                "window-height" | "window_height" => {
                    if let Ok(v) = e.value.parse::<u32>() {
                        cfg.window_height = Some(v.clamp(WINDOW_HEIGHT_MIN, WINDOW_HEIGHT_MAX));
                    }
                }
                "window-position-x" | "window_position_x" => {
                    if let Ok(v) = e.value.parse::<i32>() {
                        cfg.window_position_x = Some(v);
                    }
                }
                "window-position-y" | "window_position_y" => {
                    if let Ok(v) = e.value.parse::<i32>() {
                        cfg.window_position_y = Some(v);
                    }
                }
                "gpu-power-preference" | "gpu_power_preference" => {
                    cfg.gpu_power_preference = match e.value.to_ascii_lowercase().as_str() {
                        "high" | "high-performance" | "discrete" => GpuPowerPreference::High,
                        "low" | "low-power" | "integrated" => GpuPowerPreference::Low,
                        "auto" | "automatic" | "none" => GpuPowerPreference::Auto,
                        _ => GpuPowerPreference::Auto,
                    };
                }
                "gpu-backend" | "gpu_backend" => {
                    cfg.gpu_backend = match e.value.to_ascii_lowercase().as_str() {
                        "dx12" | "d3d12" | "directx12" => GpuBackend::Dx12,
                        "vulkan" | "vk" => GpuBackend::Vulkan,
                        "metal" => GpuBackend::Metal,
                        "gl" | "opengl" | "gles" => GpuBackend::Gl,
                        _ => GpuBackend::Auto,
                    };
                }
                "gpu-vendor-id" | "gpu_vendor_id" => {
                    if let Some(v) = parse_hex_or_dec_u32(&e.value) {
                        cfg.gpu_vendor_id = v;
                    }
                }
                "gpu-device-id" | "gpu_device_id" => {
                    if let Some(v) = parse_hex_or_dec_u32(&e.value) {
                        cfg.gpu_device_id = v;
                    }
                }
                "gpu-name" | "gpu_name" => {
                    cfg.gpu_name = e.value.trim().to_string();
                }
                "gpu-force-software" | "gpu_force_software" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.gpu_force_software = b;
                    }
                }
                "full-screen" | "full_screen" => {
                    // Terminator parity (config.py:159
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
                    // Terminator parity (config.py:214
                    // `audible_bell`): kettle ships no audio bell
                    // surface yet (visual + window-attention only),
                    // so this key parses but is otherwise a Bucket E
                    // documented no-op. Accepting it keeps a
                    // Terminator config file copy-clean (no
                    // unknown-key warning at --check-config time).
                    // If a user wants the bell to fire, they should
                    // set `bell = attention` / `bell = visual` or
                    // the `urgent_bell` / `visible_bell`
                    // compat aliases.
                    let _ = parse_bool(&e.value);
                }
                "visible-bell" | "visible_bell" => {
                    // Terminator parity (config.py:215).
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
                    // Terminator parity (config.py:216).
                    if let Some(b) = parse_bool(&e.value) {
                        terminator_urgent_bell = Some(b);
                    }
                }
                "theme-mode" | "theme_mode" => {
                    // Terminator parity (`plugins/auto_theme.py`).
                    // `system` / `follow-system` are accepted aliases because
                    // winit's window theme event is now the OS-following path.
                    cfg.theme_mode = match e.value.trim().to_ascii_lowercase().as_str() {
                        "light" => ThemeMode::Light,
                        "dark" => ThemeMode::Dark,
                        "auto" | "system" | "follow-system" | "follow_system" => ThemeMode::Auto,
                        _ => ThemeMode::Explicit,
                    };
                }
                "theme-schedule" | "theme_schedule" => {
                    // Phase 4 of the auto-theme design:
                    // `theme-schedule = HH:MM dark, HH:MM light`
                    // (whitespace flexible). The dark + light are
                    // role tags; either can come first. Garbage
                    // values leave theme_schedule as None.
                    //
                    // Phase 6 of the auto-theme design: `theme-schedule =
                    // sunrise/sunset` enables the lat/long-driven
                    // variant. The lat/long come from the
                    // theme-schedule-lat + theme-schedule-long
                    // keys and are patched in at end-of-parse.
                    cfg.theme_schedule = parse_theme_schedule(&e.value);
                }
                "theme-schedule-lat" | "theme_schedule_lat" => {
                    if let Ok(v) = e.value.trim().parse::<f64>()
                        && (-90.0..=90.0).contains(&v)
                    {
                        cfg.theme_schedule_lat = Some(v);
                    }
                }
                "theme-schedule-long"
                | "theme_schedule_long"
                | "theme-schedule-lon"
                | "theme_schedule_lon"
                | "theme-schedule-longitude"
                | "theme_schedule_longitude" => {
                    if let Ok(v) = e.value.trim().parse::<f64>()
                        && (-180.0..=180.0).contains(&v)
                    {
                        cfg.theme_schedule_long = Some(v);
                    }
                }
                "tab-bar-width" | "tab_bar_width" => {
                    // Phase 7 of the vertical-tabs design.
                    if let Ok(v) = e.value.trim().parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.tab_bar_width = v.clamp(40.0, 600.0);
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
                        "starfield" => BackgroundType::Starfield,
                        "transparent" => BackgroundType::Transparent,
                        _ => BackgroundType::Solid,
                    };
                }
                "text-renderer" | "text_renderer" => {
                    cfg.text_renderer = match e.value.to_ascii_lowercase().as_str() {
                        "legacy" => TextRenderer::Legacy,
                        _ => TextRenderer::Grid,
                    };
                }
                "background-animation" | "background_animation" => {
                    cfg.background_animation = match e.value.to_ascii_lowercase().as_str() {
                        "when-focused" | "when_focused" | "focused" => {
                            BackgroundAnimation::WhenFocused
                        }
                        "off" | "false" | "no" | "none" | "static" => BackgroundAnimation::Off,
                        // Unknown / always / on / true → the v2.24.0 default.
                        _ => BackgroundAnimation::Always,
                    };
                }
                "chrome-background" | "chrome_background" => {
                    cfg.chrome_background = match e.value.to_ascii_lowercase().as_str() {
                        "auto" => ChromeBackground::Auto,
                        "black" => ChromeBackground::Black,
                        "white" => ChromeBackground::White,
                        _ => ChromeBackground::Theme,
                    };
                }
                "background-image" | "background_image" => {
                    cfg.background_image = e.value.trim().to_string();
                }
                // These three are enum-valued strings, and the renderer matches
                // them case-SENSITIVELY (`match cfg.background_image_mode
                // .as_str()`) while `--check-config` validates them
                // case-INSENSITIVELY. So `background_image_mode = Tile` passed
                // the check and then fell through the renderer's match to the
                // default arm: the setting reported as valid and did nothing.
                // Fold on the way in, where the validator has already decided
                // that case does not carry meaning here.
                "background-image-mode" | "background_image_mode" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.background_image_mode = v.to_ascii_lowercase();
                    }
                }
                "background-image-align-horiz" | "background_image_align_horiz" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.background_image_align_horiz = v.to_ascii_lowercase();
                    }
                }
                "background-image-align-vert" | "background_image_align_vert" => {
                    let v = e.value.trim();
                    if !v.is_empty() {
                        cfg.background_image_align_vert = v.to_ascii_lowercase();
                    }
                }
                "background-blur" | "background_blur" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.background_blur = b;
                    }
                }
                "background-darkness" | "background_darkness" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.background_darkness = v.clamp(0.0, 1.0);
                    }
                }
                "cell-height" | "cell_height" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.cell_height = v.clamp(0.5, 3.0);
                    }
                }
                "cell-width" | "cell_width" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
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
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.unfocused_split_opacity = v.clamp(0.1, 1.0);
                    }
                }
                "scroll-multiplier" | "mouse-scroll-multiplier" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.scroll_multiplier = v.clamp(0.1, 50.0);
                    }
                }
                "minimum-contrast" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
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
                "agent-badge" | "agent_badge" => {
                    // Allow empty (disables the badge); take the value verbatim.
                    cfg.agent_badge = e.value.clone();
                }
                "scrollbar" => {
                    cfg.scrollbar = match e.value.to_ascii_lowercase().as_str() {
                        "never" | "off" | "false" => ScrollbarMode::Never,
                        "always" => ScrollbarMode::Always,
                        _ => ScrollbarMode::Auto,
                    }
                }
                "scrollbar-width" | "scrollbar_width" => {
                    if let Ok(v) = e.value.parse::<f32>()
                        && v.is_finite()
                    {
                        cfg.scrollbar_width = v.clamp(2.0, 40.0);
                    }
                }
                "resize-overlay" | "resize_overlay" => {
                    cfg.resize_overlay = match e.value.to_ascii_lowercase().as_str() {
                        "never" | "off" | "false" => ResizeOverlayMode::Never,
                        "always" | "on" | "true" => ResizeOverlayMode::Always,
                        _ => ResizeOverlayMode::AfterFirst,
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
                "accent-color" | "accent_color" => {
                    // `auto` = Peacock: vary the accent per working directory
                    // and per window (the default since multi-window support
                    // landed). `theme` / `off` / `none` opt OUT — every window
                    // uses the theme's static signature accent. A hex pins an
                    // explicit color (and skips the live dedupe).
                    let v = e.value.trim();
                    if v.eq_ignore_ascii_case("auto") {
                        cfg.accent_auto = true;
                        cfg.accent_color = None;
                    } else if v.eq_ignore_ascii_case("theme")
                        || v.eq_ignore_ascii_case("off")
                        || v.eq_ignore_ascii_case("none")
                    {
                        cfg.accent_auto = false;
                        cfg.accent_color = None;
                    } else if let Some(c) = Rgb::parse(v) {
                        cfg.accent_color = Some(c);
                        cfg.accent_auto = false;
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
                // Terminator parity
                // (terminatorlib/config.py `copy_on_selection`):
                // VTE per-profile "auto-copy selection to PRIMARY
                // clipboard". Maps 1:1 onto kettle's existing
                // `copy_on_select`.
                "copy-on-select" | "copy_on_selection" | "copy-on-selection" | "copy_on_select" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.copy_on_select = b;
                    }
                }
                "update-policy" | "update_policy" => {
                    cfg.update_policy = match e.value.trim().to_ascii_lowercase().as_str() {
                        "off" => UpdatePolicy::Off,
                        "auto" => UpdatePolicy::Auto,
                        "notify" => UpdatePolicy::Notify,
                        // Unrecognized value: fall back to the conservative
                        // notify-only behavior rather than silently
                        // auto-installing on a typo. Flagged by
                        // `detect_malformed_values`.
                        _ => UpdatePolicy::Notify,
                    };
                }
                "update-check-interval-hours" | "update_check_interval_hours" => {
                    if let Ok(hours) = e.value.trim().parse::<u32>() {
                        cfg.update_check_interval_hours = hours.max(1);
                    }
                }
                // Backward compatibility for pre-v2.35 boolean configs. The
                // canonical key above wins even when this alias appears later.
                "update-check" | "check-for-updates" => {
                    if !has_update_policy && let Some(b) = parse_bool(&e.value) {
                        cfg.update_policy = if b {
                            UpdatePolicy::Notify
                        } else {
                            UpdatePolicy::Off
                        };
                    }
                }
                // Opt IN to restoring the last session on launch
                // (off by default — fresh windows, mainstream behavior).
                "restore-session" | "restore_session" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.restore_session = b;
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
                // Terminator parity
                // (terminatorlib/config.py:249 `mouse_autohide`):
                // VTE auto-hides the pointer while typing. kettle's
                // existing `mouse_hide_while_typing` semantics
                // match exactly, so the Terminator key is accepted
                // as an alias.
                "mouse-hide-while-typing" | "mouse-hide" | "mouse_autohide" | "mouse-autohide" => {
                    if let Some(b) = parse_bool(&e.value) {
                        cfg.mouse_hide_while_typing = b;
                    }
                }
                // `word-delimiters` (Alacritty / WezTerm) = the characters that
                // BREAK words. v2.26.0 (audit): VTE/Terminator's `word_chars` is
                // the INVERSE concept (characters that ARE part of a word), so
                // aliasing it here produced exactly-inverted double-click
                // selection. `word_chars`/`word-chars` are intentionally NOT
                // accepted (they surface as unknown keys) rather than silently
                // doing the opposite of what the user asked.
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
                // Terminator parity (terminatorlib/
                // config.py `custom_command` + `use_custom_command`):
                // VTE per-profile "run a specific command instead
                // of the user's default shell". kettle's existing
                // `command` / `shell` config keys cover this — the
                // Terminator `use_custom_command = false` gate is
                // unnecessary because an empty `command =` falls
                // back to $SHELL.
                "command" | "shell" | "custom_command" | "custom-command" => {
                    // Empty `command =` is "reset to default" (same
                    // shape as the font-family empty-value handling
                    // above). Without
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
                    // (detect_malformed_values), but the
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
                    // Base: `trigger = REGEX` fires Urgency.
                    // Terminator parity (run_cmd_on_match.py):
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
                    // Terminator parity (custom_commands.py):
                    // `menu-item = LABEL = CMD` appends a right-click
                    // menu entry that writes `CMD\n` to the focused
                    // PTY on click. The config line-parser already
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
                // A line keyed by a bare ACTION name is Terminator's
                // `[keybindings]` grammar: `new_tab = <Control><Shift>t`.
                // kettle's own grammar is the inverse — `keybind =
                // <trigger>=<action>` — so every such line was an unknown key
                // and the whole section imported as nothing. Rewrite it into
                // kettle's form when the key names a real action AND the value
                // parses as a trigger; anything else still falls through to
                // the unknown-key warning rather than being silently eaten.
                // An EMPTY accelerator is Terminator's "this shortcut is
                // disabled" (its own defaults ship several, and its
                // preferences UI writes one when you clear a binding with
                // Backspace). Ignoring the line left kettle's default chord
                // live, so a config that deliberately freed Ctrl+Shift+T
                // did not free it.
                other
                    if keybinds::Action::from_name(other).is_some()
                        && e.value.trim().is_empty() =>
                {
                    keybinds::unbind_action(&mut cfg.keybinds, other);
                }
                other
                    if keybinds::Action::from_name(other).is_some()
                        && keybinds::parse_trigger(&e.value).is_some() =>
                {
                    // Terminator's grammar is `action = accelerator` — one
                    // accelerator per action — so an imported line REPLACES
                    // the action's chord rather than adding to it. See
                    // `apply_exclusive_keybind`.
                    keybinds::apply_exclusive_keybind(
                        &mut cfg.keybinds,
                        &format!("{}={}", e.value, e.raw_key),
                    );
                }
                // Report the spelling the user actually wrote, not the folded
                // one — otherwise the warning names a line that does not exist
                // in their file.
                _ => unknown.push(e.raw_key.clone()),
            }
        }
        // Terminator's `scrollback_infinite` overrides the line count rather
        // than racing it: `scrollback_infinite = True` above
        // `scrollback_lines = 100` must still mean unbounded, and it did not
        // when both simply assigned in file order.
        if scrollback_infinite == Some(true) {
            cfg.scrollback = INFINITE_SCROLLBACK;
        }
        for (i, c) in explicit_palette {
            if i < 16 {
                // A DERIVED accent (a theme without an
                // explicit `accent` line snapshots `palette[4]` at parse
                // time) must follow a config-level `palette = 4=#hex`
                // override; an explicit theme accent (different from
                // palette[4]) stays put. Equality is the derivation marker —
                // and if a theme explicitly set accent == palette[4],
                // following the override is indistinguishable from derived.
                if i == 4 && cfg.theme.accent == cfg.theme.palette[4] {
                    cfg.theme.accent = c;
                }
                cfg.theme.palette[i] = c;
            }
        }
        // Apply explicit single-color overrides AFTER the
        // theme + palette re-apply above so they're order-independent —
        // `background = #ff0000` keeps the red whether the `theme =`
        // line comes before or after it. Precedence (highest first):
        // explicit single-color line > `palette = N=#hex` > `theme =`.
        if let Some(c) = explicit_background {
            cfg.theme.background = c;
        }
        if let Some(c) = explicit_foreground {
            cfg.theme.foreground = c;
        }
        if let Some(c) = explicit_cursor {
            cfg.theme.cursor = c;
        }
        if let Some(c) = explicit_cursor_text {
            cfg.theme.cursor_text = c;
        }
        if let Some(c) = explicit_selection_bg {
            cfg.theme.selection_background = c;
        }
        if let Some(c) = explicit_selection_fg {
            cfg.theme.selection_foreground = c;
        }
        // Terminator parity (config.py:215-216):
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
        // Terminator parity (terminatorlib/config.py
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
        // Terminator parity (terminatorlib/config.py
        // `use_custom_command`): when false, ignore any
        // `custom_command` / `command` / `shell` value — Terminator
        // semantics let you keep `custom_command` defined in the
        // profile but disabled. Order-independent because applied
        // here at parse-finalize.
        if !cfg.use_custom_command {
            cfg.shell = None;
        }
        // Patch the SunriseSunset variant with the
        // parsed lat/long now that both halves of the config
        // are read. If lat OR long is missing, downgrade the
        // schedule to None (sunrise mode needs both halves).
        if let Some(ThemeSchedule::SunriseSunset { .. }) = cfg.theme_schedule {
            match (cfg.theme_schedule_lat, cfg.theme_schedule_long) {
                (Some(lat), Some(long)) => {
                    cfg.theme_schedule = Some(ThemeSchedule::SunriseSunset { lat, long });
                }
                _ => cfg.theme_schedule = None,
            }
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
    fn decode_config_text_handles_utf16_and_utf8_boms() {
        // PowerShell 5.1's `>` writes UTF-16 LE with a BOM. Build that for a
        // one-line config and confirm it decodes (and then parses) correctly,
        // rather than being rejected as invalid UTF-8 and silently dropped.
        let line = "update-check = false\n";
        let mut le = vec![0xFFu8, 0xFE];
        for u in line.encode_utf16() {
            le.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(decode_config_text(&le), line);
        assert_eq!(
            Config::parse_text(&decode_config_text(&le)).update_policy,
            UpdatePolicy::Off
        );

        // UTF-16 BE (FE FF) also decodes.
        let mut be = vec![0xFEu8, 0xFF];
        for u in line.encode_utf16() {
            be.extend_from_slice(&u.to_be_bytes());
        }
        assert_eq!(decode_config_text(&be), line);

        // Plain UTF-8 is unchanged; a UTF-8 BOM is preserved for parse() to
        // strip (so a Notepad-saved file still works as before).
        assert_eq!(decode_config_text(line.as_bytes()), line);
        let utf8_bom = [&[0xEFu8, 0xBB, 0xBF], line.as_bytes()].concat();
        assert_eq!(decode_config_text(&utf8_bom), format!("\u{feff}{line}"));
        assert_eq!(
            Config::parse_text(&decode_config_text(&utf8_bom)).update_policy,
            UpdatePolicy::Off
        );
    }

    #[test]
    fn update_policy_migrates_legacy_boolean_and_canonical_key_wins() {
        assert_eq!(
            Config::parse_text("update-check = true").update_policy,
            UpdatePolicy::Notify
        );
        assert_eq!(
            Config::parse_text("update-check = false").update_policy,
            UpdatePolicy::Off
        );
        assert_eq!(
            Config::parse_text("update-policy = auto\nupdate-check = false").update_policy,
            UpdatePolicy::Auto
        );
        assert_eq!(
            Config::parse_text("update-check = false\nupdate-policy = notify").update_policy,
            UpdatePolicy::Notify
        );
        assert!(
            Config::detect_malformed_values("update-policy = sometimes")
                .iter()
                .any(|issue| issue.contains("update-policy"))
        );
    }

    #[test]
    fn example_config_in_docs_uncommented_parses_with_zero_diagnostics() {
        // docs/kettle.example.config used to document 9 of the
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

    /// Every config key documented in `docs/CONFIG.md`'s
    /// Keys table must be a key the parser actually
    /// recognizes — otherwise the docs claim a key that does nothing. Feeds a
    /// valid `key = value` for each and asserts the parser reports no unknown
    /// keys. (The example config has its own broader round-trip guard above.)
    #[test]
    fn newly_documented_config_keys_are_recognized() {
        let sample = "\
theme-mode = dark\n\
light-theme = TokyoNight Day\n\
dark-theme = TokyoNight Night\n\
theme-schedule = 19:00 dark,07:00 light\n\
theme-schedule-lat = 33.77\n\
theme-schedule-long = -118.19\n\
allow-bold = false\n\
bold-is-bright = true\n\
clear-select-on-copy = true\n\
invert-search = true\n\
modify-other-keys = enter\n\
backspace-binding = ascii-del\n\
delete-binding = escape-sequence\n\
login-shell = true\n\
term = xterm-256color\n\
colorterm = truecolor\n\
env = EDITOR=nvim\n\
window-width = 120\n\
window-height = 36\n\
window-position-x = -1920\n\
window-position-y = 80\n\
tab-bar-position = left\n\
tab-bar-width = 200\n\
ask-before-closing = multiple-terminals\n\
cell-width = 1.1\n\
cell-height = 1.2\n";
        let (_cfg, unknown) = Config::parse_collect(sample);
        assert!(
            unknown.is_empty(),
            "CONFIG.md documents keys the parser doesn't recognize: {unknown:?}"
        );
        let malformed = Config::detect_malformed_values(sample);
        assert!(
            malformed.is_empty(),
            "the documented sample values must validate clean: {malformed:?}"
        );
    }

    /// User-facing config docs must not regress shipped keys
    /// back into the future-work table, and the main Keys table should not
    /// duplicate a primary row name.
    #[test]
    fn config_reference_table_has_no_stale_or_duplicate_rows() {
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let config_md =
            std::fs::read_to_string(manifest.join("../../docs/CONFIG.md")).expect("docs/CONFIG.md");

        let future = config_md
            .split("#### Genuine future work")
            .nth(1)
            .and_then(|s| s.split("## Editing the config").next())
            .expect("future-work table");
        for shipped in ["ask-before-closing", "cell-width", "cell-height"] {
            assert!(
                !future.contains(shipped),
                "{shipped} is shipped and must not be documented as future work"
            );
        }

        let keys_table = config_md
            .split("## Keys")
            .nth(1)
            .and_then(|s| s.split("### Auto light/dark").next())
            .expect("main Keys table");
        let mut seen = std::collections::BTreeMap::<String, usize>::new();
        for line in keys_table.lines() {
            let Some(rest) = line.strip_prefix("| `") else {
                continue;
            };
            let Some((key, _)) = rest.split_once('`') else {
                continue;
            };
            *seen.entry(key.to_string()).or_insert(0) += 1;
        }
        let duplicates: Vec<_> = seen
            .into_iter()
            .filter_map(|(key, count)| (count > 1).then_some((key, count)))
            .collect();
        assert!(
            duplicates.is_empty(),
            "main CONFIG.md Keys table has duplicate primary rows: {duplicates:?}"
        );

        // Keep the Terminator-parity reference as one Markdown table. Prose
        // inserted between rows silently closes a CommonMark table, leaving
        // every later `| key | ... |` line rendered as plain text.
        let parity = config_md
            .split("### Terminator-parity keys")
            .nth(1)
            .and_then(|s| {
                s.split("### Terminator-parity config keys by disposition")
                    .next()
            })
            .expect("Terminator-parity keys section");
        let mut table_started = false;
        let mut table_closed = false;
        for line in parity.lines() {
            if line.starts_with('|') {
                assert!(
                    !table_closed,
                    "CONFIG.md Terminator-parity table resumes after prose: {line}"
                );
                table_started = true;
            } else if table_started {
                table_closed = true;
            }
        }
        assert!(
            table_started,
            "CONFIG.md Terminator-parity table is missing"
        );
    }

    #[test]
    fn update_checker_is_documented_for_users() {
        // The automatic checker is a shipped network feature. Pin the explicit
        // command, canonical privacy policy, and migration alias so future
        // documentation edits cannot make those controls undiscoverable.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let readme = std::fs::read_to_string(manifest.join("../../README.md")).expect("README");
        assert!(
            readme.contains("--check-update"),
            "README must document the --check-update flag"
        );
        assert!(
            readme.contains("update-policy"),
            "README must document the update-policy control"
        );
        let example = std::fs::read_to_string(manifest.join("../../docs/kettle.example.config"))
            .expect("example config");
        assert!(
            example.contains("update-policy"),
            "example config must document the update-policy key"
        );
        assert!(
            example.contains("update-check"),
            "example config must document the legacy update-check key"
        );
    }

    #[test]
    fn user_facing_docs_have_no_internal_cycle_refs() {
        // This guard catches the audit-trail-in-doc-string issue on the
        // clap CLI surface (`kettle --help` was emitting internal
        // parenthetical annotations — mysterious to end users).
        // The drift guard also covers the user-facing markdown
        // docs the README links to: CONFIG.md (config reference) and
        // INSTALL.md (per-OS install + from-source). README itself
        // mentions the word "cycle" in legitimate prose (e.g. "cycle
        // the themes at runtime"), so the check has to be tighter
        // than "contains 'cycle '" — match the internal-ref shape
        // `cycle <digit>` instead.
        //
        // TESTING.md and ROADMAP.md are intentionally exempt — they're
        // contributor-leaning docs where internal reference markers
        // serve as anchors to specific CHANGELOG entries, the same way
        // they do in code comments. CONTRIBUTING.md originally worked
        // the same way, since it documented that marker format
        // directly and a literal instance there was part of the
        // content.
        //
        // This guard also flags hardcoded `<N> workspace tests` /
        // `<N> tests across` claims — these go stale as the suite
        // grows (TESTING.md / ARCHITECTURE.md / INSTALL.md each had one
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
            // The example config is user-facing through
            // `kettle --print-default-config > ~/.config/kettle/config`.
            // Internal reference markers inside it would leak into every
            // user's bootstrap file — same drift-guard reasoning as the
            // markdown docs above.
            "docs/kettle.example.config",
            // The man page is user-facing via `man kettle`. Same
            // reasoning — internal reference markers leak into
            // user-visible documentation.
            "packaging/linux/kettle.1",
            // SECURITY.md is user-facing via GitHub's
            // /security tab + the repo root listing. It already uses
            // the hyphenated `cycle-NNN` form, which passes the
            // space-digit scan below — adding the doc to the scan
            // list makes future drift explicit. Past contributors
            // shouldn't have to remember "SECURITY.md is user-facing,
            // don't write `cycle NNN` there".
            "SECURITY.md",
            // docs/ARCHITECTURE.md + CONTRIBUTING.md were
            // outside the scanned set when this guard was
            // first introduced — they were considered developer-
            // facing. After a later doc cleanup pass both files
            // were re-scrubbed to leave only proper-noun hyphenated
            // refs (pointing at test names like
            // `palette_includes_every_user_facing_action`), which
            // pass the space-digit scan. Adding them to the scan list
            // makes future regressions explicit at PR review time so
            // a stray internal-reference parenthetical doesn't drift
            // back into the prose.
            "docs/ARCHITECTURE.md",
            "CONTRIBUTING.md",
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
            // Hardcoded test-count claims drift over time. Detect
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
                             (drifts over time — reword as `230+ tests` \
                             or `an extensive suite`): `{}`",
                            rel,
                            &text[start..(i + stale.len()).min(text.len())],
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn no_internal_cycle_refs_anywhere() {
        // The repo's early development wove numbered audit bookkeeping
        // markers through code comments, docs, scripts, and build
        // files. Those markers were removed wholesale — provenance
        // lives in git history and CHANGELOG.md, whose historical
        // entries are the record the old markers pointed into (and
        // which is therefore the one deliberate exemption below).
        // This guard keeps the shapes from drifting back into any
        // living source: it walks every workspace Rust file plus the
        // doc/script/packaging text surfaces and fails on the numbered
        // form (the marker word + space or hyphen + digits), the
        // placeholder form (marker word + space or hyphen + a
        // standalone `x`), and the `sub-`-prefixed form.
        //
        // The marker word is assembled at runtime so this test's own
        // source cannot match itself.
        let word_owned: String = ["cy", "cle"].concat();
        let word = word_owned.as_bytes();

        fn line_of(text: &[u8], off: usize) -> usize {
            1 + text[..off].iter().filter(|&&b| b == b'\n').count()
        }

        let scan = |path: &std::path::Path, offenders: &mut Vec<String>| {
            let Ok(raw) = std::fs::read(path) else {
                return;
            };
            let lower: Vec<u8> = raw.iter().map(|b| b.to_ascii_lowercase()).collect();
            for (i, w) in lower.windows(word.len()).enumerate() {
                if w != word {
                    continue;
                }
                // The plural form ("<word>s 331-410") is banned the
                // same as the singular, so skip an optional `s`
                // before looking at the separator.
                let mut after = &lower[i + word.len()..];
                if after.first() == Some(&b's') {
                    after = &after[1..];
                }
                let numbered_or_placeholder = match (after.first(), after.get(1)) {
                    (Some(&s), Some(&c)) if s == b' ' || s == b'-' => {
                        c.is_ascii_digit()
                            || (c == b'x'
                                && !after.get(2).is_some_and(|b| b.is_ascii_alphanumeric()))
                    }
                    _ => false,
                };
                let sub_prefixed = i >= 4 && &lower[i - 4..i] == b"sub-";
                // A contiguous leading dash is command-line option syntax for
                // the PowerShell schedule-count parameter, not a prose audit
                // label. Spaced list labels and numeric suffix forms still
                // match. Do not exempt the independently banned sub-prefix
                // shape.
                let option_identifier = i > 0 && lower[i - 1] == b'-' && !sub_prefixed;
                if (numbered_or_placeholder && !option_identifier) || sub_prefixed {
                    let end = (i + 40).min(raw.len());
                    offenders.push(format!(
                        "{}:{}: {}",
                        path.display(),
                        line_of(&raw, i),
                        String::from_utf8_lossy(&raw[i.saturating_sub(4)..end]).trim()
                    ));
                }
            }
        };

        fn walk(dir: &std::path::Path, visit: &mut dyn FnMut(&std::path::Path)) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if name != "target" && name != ".git" {
                        walk(&p, visit);
                    }
                } else {
                    visit(&p);
                }
            }
        }

        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo_root = manifest.join("../..");
        let mut offenders: Vec<String> = Vec::new();

        // Binary assets are the only skip class — everything textual
        // is fair game, including extensionless files like the
        // pre-commit hook and templated packaging inputs.
        let is_binary = |p: &std::path::Path| {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            matches!(
                ext.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "ico"
                    | "icns"
                    | "bmp"
                    | "ttf"
                    | "otf"
                    | "woff"
                    | "woff2"
                    | "zip"
                    | "gz"
                    | "tar"
                    | "bin"
                    | "exe"
                    | "dll"
            )
        };

        // Every Rust source and manifest in the workspace, including
        // build scripts and integration tests.
        walk(&repo_root.join("crates"), &mut |p| {
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext == "rs" || ext == "toml" {
                scan(p, &mut offenders);
            }
        });

        // Living text surfaces. CHANGELOG.md is deliberately absent.
        for rel in [
            "README.md",
            "CONTRIBUTING.md",
            "SECURITY.md",
            "CODE_OF_CONDUCT.md",
            "AGENTS.md",
            "CLAUDE.md",
            "Justfile",
            "flake.nix",
            "Cargo.toml",
            "deny.toml",
            ".gitignore",
            ".gitattributes",
        ] {
            let p = repo_root.join(rel);
            if p.exists() {
                scan(&p, &mut offenders);
            }
        }
        for dir in ["docs", "scripts", "packaging", ".github", ".githooks"] {
            walk(&repo_root.join(dir), &mut |p| {
                if !is_binary(p) {
                    scan(p, &mut offenders);
                }
            });
        }

        assert!(
            offenders.is_empty(),
            "internal audit-marker references must not reappear outside \
             CHANGELOG.md; reword the rationale in timeless prose (see \
             CONTRIBUTING.md):\n{}",
            offenders.join("\n")
        );
    }

    // ────────────────────────────────────────────────────────────
    // Consolidated drift guards for user-facing markdown.
    //
    // The image guard and the link guard had
    // near-identical byte-walking scanners that differed only in
    // which kind of `[…](path)` they matched. An earlier pass added
    // backtick-awareness to the link scanner; a follow-up propagated
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
        // See `walk_md_refs` for the rationale. An earlier pass introduced
        // the README guard, later extended to docs/*.md, then added
        // backtick-awareness, and finally consolidated with the link guard.
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
                     not exist",
                    abs.display()
                );
                if file_rel == "README.md" {
                    readme_image_count += 1;
                }
            });
        }
        // Contract: README has at least one image embed.
        assert!(
            readme_image_count >= 1,
            "expected ≥ 1 image embed in README; found {readme_image_count} \
             — walker likely regressed"
        );
    }

    #[test]
    fn user_facing_doc_md_cross_links_resolve() {
        // See `walk_md_refs`. This guard was introduced for links, later
        // made backtick-aware, then consolidated with the image guard.
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
                    "{file_rel} links to `{path}` but `{}` does not exist",
                    abs.display()
                );
                if file_rel == "README.md" {
                    readme_link_count += 1;
                }
            });
        }
        // Contract: README has ≥ 3 .md cross-links.
        assert!(
            readme_link_count >= 3,
            "expected ≥ 3 .md cross-links in README; found {readme_link_count} \
             — walker likely regressed"
        );
    }

    #[test]
    fn workspace_metadata_policy() {
        // The workspace pins one source of truth for every `[package]` field
        // shared across crates, and the description
        // override below has its own rule. This guard prevents a
        // "tidying" pass from accidentally inverting either
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
                 this contract requires every shared metadata \
                 field to live in workspace.package"
            );
        }

        // 2) every crate inherits each field via `.workspace = true`.
        // Description is the exception (library override, verified below).
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
                    "{name}: missing `{inherit}` — this contract \
                     requires every crate to inherit this field from \
                     workspace.package"
                );
            }
            if is_library {
                // Library: override description with its own.
                assert!(
                    text.contains("\ndescription = \"kettle: "),
                    "{name}: library must override description with \
                     `description = \"kettle: …\"`"
                );
                assert!(
                    !text.contains("description.workspace = true"),
                    "{name}: library must NOT inherit description \
                     (would otherwise emit the binary's blurb)"
                );
            } else {
                // Binary: inherits description.
                assert!(
                    text.contains("description.workspace = true"),
                    "kettle (binary): keeps `description.workspace = true` \
                     so the workspace blurb stays the single source of \
                     truth for the binary's blurb"
                );
            }
        }
    }

    /// Session restore is OPT-IN (fresh windows by default, like
    /// mainstream terminals). Pin the default + that the bool key parses both
    /// spellings, so a regression to always-restore is caught.
    #[test]
    fn restore_session_defaults_off_and_parses() {
        assert!(
            !Config::default().restore_session,
            "session restore must be OFF by default (fresh windows)"
        );
        assert!(Config::parse_text("restore-session = true\n").restore_session);
        assert!(Config::parse_text("restore_session = true\n").restore_session);
        assert!(!Config::parse_text("restore-session = false\n").restore_session);
        // It is in BOOL_KEYS so `--check-config` validates it.
        assert!(Config::BOOL_KEYS.contains(&"restore-session"));
    }

    /// v2.20.0 (Ghostty parity): `resize-overlay` defaults to `after-first`,
    /// parses all three modes (+ bool courtesy spellings), and flags typos
    /// via `--check-config`.
    #[test]
    fn resize_overlay_parses_and_flags_typos() {
        assert_eq!(
            Config::default().resize_overlay,
            ResizeOverlayMode::AfterFirst
        );
        assert_eq!(
            Config::parse_text("resize-overlay = never\n").resize_overlay,
            ResizeOverlayMode::Never
        );
        assert_eq!(
            Config::parse_text("resize_overlay = always\n").resize_overlay,
            ResizeOverlayMode::Always
        );
        assert_eq!(
            Config::parse_text("resize-overlay = after-first\n").resize_overlay,
            ResizeOverlayMode::AfterFirst
        );
        let bad = Config::detect_malformed_values("resize-overlay = sometimes\n");
        assert!(
            bad.iter().any(|m| m.contains("resize-overlay")),
            "typo'd resize-overlay value must be flagged: {bad:?}"
        );
        assert!(Config::detect_malformed_values("resize-overlay = after-first\n").is_empty());
    }

    /// v2.20.0: vim-style menu navigation ships ON by default (the explicit
    /// ask), with `vim-menu-nav = false` as the documented opt-out. Pin the
    /// default + both key spellings + BOOL_KEYS coverage.
    #[test]
    fn vim_menu_nav_defaults_on_and_parses() {
        assert!(
            Config::default().vim_menu_nav,
            "vim-menu-nav must default ON"
        );
        assert!(!Config::parse_text("vim-menu-nav = false\n").vim_menu_nav);
        assert!(!Config::parse_text("vim_menu_nav = false\n").vim_menu_nav);
        assert!(Config::parse_text("vim-menu-nav = true\n").vim_menu_nav);
        // In BOOL_KEYS (both spellings) so `--check-config` validates it.
        assert!(Config::BOOL_KEYS.contains(&"vim-menu-nav"));
        assert!(Config::BOOL_KEYS.contains(&"vim_menu_nav"));
    }

    #[test]
    fn default_is_tokyonight_night() {
        // v2.28.0 (user-requested): the shipped default is TokyoNight Night.
        // Assert via the bundled theme's fingerprint so it tracks the theme file
        // without transcribing the palette.
        let c = Config::default();
        assert_eq!(c.theme_name, "TokyoNight Night");
        assert_eq!(
            format!("{:?}", c.theme),
            format!("{:?}", Theme::by_name("TokyoNight Night")),
            "default theme must resolve to the bundled TokyoNight Night palette"
        );
        assert_eq!(c.font_family, font::FAMILY);
    }

    #[test]
    fn default_path_falls_through_empty_env_vars() {
        // `XDG_CONFIG_HOME=""`
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
        // Config split-brain fix: the non-XDG fallback is now per-OS.
        // On Unix, XDG empty + HOME set → `$HOME/.config/kettle/config`.
        #[cfg(not(windows))]
        assert_eq!(
            Config::default_path_from(from(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")])),
            Some(
                PathBuf::from("/h")
                    .join(".config")
                    .join("kettle")
                    .join("config"),
            ),
        );
        // On Windows, a stray HOME is IGNORED (it would split-brain the GUI vs a
        // shell launch); APPDATA is the canonical per-user dir. This is the exact
        // regression a git-bash/WSL `HOME` caused.
        #[cfg(windows)]
        {
            // HOME set but no APPDATA → None (HOME must NOT be used on Windows).
            assert_eq!(
                Config::default_path_from(from(&[("XDG_CONFIG_HOME", ""), ("HOME", "/h")])),
                None,
                "Windows must not fall back to HOME/.config (config split-brain)"
            );
            // HOME set AND APPDATA set → APPDATA wins, HOME ignored.
            assert_eq!(
                Config::default_path_from(from(&[
                    ("HOME", r"C:\Users\me"),
                    ("APPDATA", r"C:\Users\me\AppData\Roaming"),
                ])),
                Some(
                    PathBuf::from(r"C:\Users\me\AppData\Roaming")
                        .join("kettle")
                        .join("config"),
                ),
            );
        }
        // XDG empty + APPDATA set → APPDATA-based path ON WINDOWS (Unix has no
        // APPDATA fallback, so it yields None there). PathBuf::join uses the
        // platform separator so the expected value matches on each runner.
        #[cfg(windows)]
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
        // XDG set → wins on EVERY OS (the explicit cross-platform override),
        // even with a Windows-style APPDATA also present.
        assert_eq!(
            Config::default_path_from(from(&[
                ("XDG_CONFIG_HOME", "/x"),
                ("HOME", "/h"),
                ("APPDATA", r"C:\u\AppData\Roaming"),
            ])),
            Some(PathBuf::from("/x").join("kettle").join("config")),
        );
        // All set-but-empty → None (rather than the previous relative
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
        // Pre-fix, `parse_collect` did
        //   cfg.theme_name = e.value.clone();      // typo preserved
        //   cfg.theme = Theme::by_name(&e.value);  // silent fallback
        // so a typo'd theme name had `--check-config` print
        // `theme: TokyoNitght Night` while the runtime used
        // TokyoNight Night's palette. Same docs/runtime mismatch shape
        // as the font-size fix. Now: store the canonical bundled
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
        // back to the default (Catppuccin Mocha, Theme::default());
        // cfg.theme_name ALSO stays at "Catppuccin Mocha" so the diagnostic
        // agrees with the runtime palette. The malformed-value warning still
        // surfaces the typo separately so the user notices.
        let c = Config::parse_text("theme = TokyoNitght Night\n");
        assert_eq!(c.theme_name, "Catppuccin Mocha");
        assert_eq!(c.theme.background, Rgb::new(0x1e, 0x1e, 0x2e));
        // And the diagnostic still flags it.
        let malformed = Config::detect_malformed_values("theme = TokyoNitght Night\n");
        assert!(
            malformed.iter().any(|m| m.contains("TokyoNitght")),
            "typo'd theme should surface as malformed: {malformed:?}"
        );
    }

    #[test]
    fn empty_value_resets_string_keys_to_their_default() {
        // Contract: parse.rs's docstring promised "empty
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

        // Empty-value handling for `command`:
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
        // dropped (matches detect_malformed_values, which
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
        // OpenType feature tags are case-sensitive per spec and
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
    fn modify_other_keys_policy_parsing_validation_and_default() {
        assert_eq!(
            Config::default().modify_other_keys,
            ModifyOtherKeysMode::Enter
        );
        assert_eq!(
            Config::parse_text("modify-other-keys = ENTER").modify_other_keys,
            ModifyOtherKeysMode::Enter
        );
        assert_eq!(
            Config::parse_text("modify_other_keys = off").modify_other_keys,
            ModifyOtherKeysMode::Off
        );
        assert_eq!(
            Config::parse_text("modify-other-keys = bogus").modify_other_keys,
            ModifyOtherKeysMode::Enter
        );
        assert!(Config::detect_malformed_values("modify-other-keys = enter\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("modify-other-keys = sometimes\n").len(),
            1
        );
        let (_, unknown) = Config::parse_collect("modify-other-keys = off\n");
        assert!(unknown.is_empty());
    }

    #[test]
    fn paste_files_policy_parsing_and_default() {
        // On by default: copying a file in Explorer pastes its path.
        let d = Config::default().paste_files;
        assert_eq!(d, PasteFiles::On);
        assert!(d.enabled());

        assert_eq!(
            Config::parse_text("paste-files = off").paste_files,
            PasteFiles::Off
        );
        assert!(
            !Config::parse_text("paste-files = false")
                .paste_files
                .enabled()
        );
        assert_eq!(
            Config::parse_text("paste-files = on").paste_files,
            PasteFiles::On
        );
        // Unknown value falls back to the (on) default.
        assert_eq!(
            Config::parse_text("paste-files = bogus").paste_files,
            PasteFiles::On
        );
    }

    #[test]
    fn paste_images_policy_parsing_and_default() {
        // On by default, so a screenshot pastes without configuration.
        assert_eq!(Config::default().paste_images, PasteImages::On);
        assert!(Config::default().paste_images.enabled());

        for off in [
            "paste-images = off",
            "paste-images = none",
            "paste-images = disabled",
            "paste-images = false",
            "paste-images = 0",
            // Singular alias, since the key names one pasted image at a time.
            "paste-image = off",
        ] {
            assert_eq!(
                Config::parse_text(off).paste_images,
                PasteImages::Off,
                "{off} must disable clipboard-bitmap paste"
            );
        }
        assert_eq!(
            Config::parse_text("paste-images = on").paste_images,
            PasteImages::On
        );
        // Unknown value falls back to the (on) default, matching `paste-files`.
        assert_eq!(
            Config::parse_text("paste-images = bogus").paste_images,
            PasteImages::On
        );
        // Independent of `paste-files` — the two have different privacy
        // profiles (this one writes captured screen content to disk).
        let cfg = Config::parse_text("paste-files = off\npaste-images = on");
        assert!(!cfg.paste_files.enabled() && cfg.paste_images.enabled());
    }

    #[test]
    fn record_policy_parsing_and_default() {
        // Off by default: a fresh install never records unless asked.
        let d = Config::default();
        assert_eq!(d.record, RecordMode::Off);
        assert!(!d.record.enabled());
        assert_eq!(d.record_dir, None);
        assert!(!d.record_raw_input);

        assert!(Config::parse_text("record = on").record.enabled());
        assert!(Config::parse_text("record = true").record.enabled());
        assert!(!Config::parse_text("record = off").record.enabled());
        // Unknown value falls back to the (off) default — never silently record.
        assert_eq!(Config::parse_text("record = bogus").record, RecordMode::Off);

        assert_eq!(
            Config::parse_text("record-dir = /tmp/casts").record_dir,
            Some(PathBuf::from("/tmp/casts"))
        );
        // Empty value clears the directory back to the resolved default.
        assert_eq!(Config::parse_text("record-dir =").record_dir, None);

        // Raw-input is a separate, strictly bool-parsed opt-in.
        assert!(Config::parse_text("record-raw-input = on").record_raw_input);
        assert!(!Config::parse_text("record-raw-input = off").record_raw_input);
        // A typo does NOT enable raw capture (guards the password footgun).
        assert!(!Config::parse_text("record-raw-input = yse").record_raw_input);

        // Malformed enum/bool values are flagged for --check-config.
        assert!(
            Config::detect_malformed_values("record = bogus")
                .iter()
                .any(|m| m.contains("record"))
        );
        assert!(
            Config::detect_malformed_values("record-raw-input = maybe")
                .iter()
                .any(|m| m.contains("record-raw-input"))
        );
    }

    #[test]
    fn update_policy_defaults_to_auto_with_tunable_cadence() {
        // Default-on auto-update (oh-my-zsh style), daily cadence.
        let d = Config::default();
        assert_eq!(d.update_policy, UpdatePolicy::Auto);
        assert_eq!(d.update_check_interval_hours, 24);

        assert_eq!(
            Config::parse_text("update-policy = off").update_policy,
            UpdatePolicy::Off
        );
        assert_eq!(
            Config::parse_text("update-policy = notify").update_policy,
            UpdatePolicy::Notify
        );
        // A typo is conservative: notify-only, never silent auto-install.
        assert_eq!(
            Config::parse_text("update-policy = bogus").update_policy,
            UpdatePolicy::Notify
        );

        assert_eq!(
            Config::parse_text("update-check-interval-hours = 6").update_check_interval_hours,
            6
        );
        // Floored at 1 so a 0 can't turn into a busy-loop against the feed.
        assert_eq!(
            Config::parse_text("update-check-interval-hours = 0").update_check_interval_hours,
            1
        );
        // Non-numeric is ignored (keeps the default) and flagged.
        assert_eq!(
            Config::parse_text("update-check-interval-hours = soon").update_check_interval_hours,
            24
        );
        assert!(
            Config::detect_malformed_values("update-check-interval-hours = soon")
                .iter()
                .any(|m| m.contains("update-check-interval-hours"))
        );
        // `0` is silently floored to 1 at runtime, so --check-config must flag it
        // rather than reporting clean while the effective value differs.
        assert!(
            Config::detect_malformed_values("update-check-interval-hours = 0")
                .iter()
                .any(|m| m.contains("update-check-interval-hours"))
        );
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
        // Drift guard for Terminator's `borderless` +
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
    fn background_animation_parse() {
        // v2.24.0: default is always-on (a wallpaper that only moves while
        // focused felt broken). Freeze-when-hidden is handled at the UI layer.
        assert_eq!(
            Config::default().background_animation,
            BackgroundAnimation::Always
        );
        assert_eq!(
            Config::parse_text("background-animation = always").background_animation,
            BackgroundAnimation::Always
        );
        assert_eq!(
            Config::parse_text("background_animation = on").background_animation,
            BackgroundAnimation::Always
        );
        assert_eq!(
            Config::parse_text("background-animation = off").background_animation,
            BackgroundAnimation::Off
        );
        assert_eq!(
            Config::parse_text("background-animation = static").background_animation,
            BackgroundAnimation::Off
        );
        assert_eq!(
            Config::parse_text("background-animation = when-focused").background_animation,
            BackgroundAnimation::WhenFocused
        );
        // Unknown value → safe default (now Always), not a parse error.
        assert_eq!(
            Config::parse_text("background-animation = bogus").background_animation,
            BackgroundAnimation::Always
        );
        // --check-config accepts the documented spellings, rejects nonsense.
        assert!(Config::detect_malformed_values("background-animation = always").is_empty());
        assert!(Config::detect_malformed_values("background-animation = when-focused").is_empty());
        assert!(!Config::detect_malformed_values("background-animation = nope").is_empty());
    }

    #[test]
    fn starfield_parse_and_defaults() {
        // v2.24.1: the starfield is a FIXED built-in example — its look is baked
        // into the shader, NOT config-driven. Only the background-TYPE toggle is
        // config (the speed/density/glow knobs were removed).
        let d = Config::default();
        assert_eq!(d.background_type, BackgroundType::Solid); // still off by default
        assert_eq!(
            Config::parse_text("background-type = starfield").background_type,
            BackgroundType::Starfield
        );
        assert_eq!(
            Config::parse_text("background_type = starfield").background_type,
            BackgroundType::Starfield
        );
        assert!(Config::detect_malformed_values("background-type = starfield").is_empty());

        // v2.25.0: text-renderer defaults to the cell-locked grid path; legacy
        // is the opt-out rollback escape hatch.
        assert_eq!(d.text_renderer, TextRenderer::Grid);
        assert_eq!(
            Config::parse_text("text-renderer = legacy").text_renderer,
            TextRenderer::Legacy
        );
        assert_eq!(
            Config::parse_text("text_renderer = grid").text_renderer,
            TextRenderer::Grid
        );
        assert!(Config::detect_malformed_values("text-renderer = grid").is_empty());
        // An old config that still carries the removed starfield knobs must
        // parse cleanly (the value falls through) — they surface only as
        // unknown-key warnings, never a hard error.
        let _ = Config::parse_text("starfield-speed = 0.02");
    }

    #[test]
    fn render_and_copy_bools_parse() {
        // Drift guard: allow_bold defaults true (Terminator
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
        // Drift guard closing the remaining config-key
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
        // Drift guard for bell sub-flag aliases + per-pane
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

    /// Drift guard: `split_to_group`, `autoclean_groups`,
    /// `always_split_with_profile`, `cursor_color_default`,
    /// `use_system_font`, `use_theme_colors`, `extra_styling`, and
    /// `http_proxy` are parsed, validated, and stored on `Config` but
    /// (as of this writing) never *read* anywhere outside this file's
    /// own parse/default/test code — `docs/CONFIG.md`'s "Terminator-parity
    /// config keys by disposition" table is the single source of truth for
    /// *why* each one is inert (permanent by-design divergence vs. genuine
    /// forward-compat future-work) and is the required companion read
    /// before "wiring one up" or "just deleting it" — either is wrong
    /// without checking that table first. This guard doesn't (and can't,
    /// from a config-parsing test) verify runtime behavior; it pins that
    /// the honest "why this is a no-op" doc comment stays attached to each
    /// field's declaration so a future edit can't silently drop the
    /// disclosure and leave the field looking like a live, wired setting.
    #[test]
    fn dead_terminator_stub_fields_document_their_own_inertness() {
        let src = include_str!("lib.rs");
        for (field_decl, marker) in [
            ("pub split_to_group: bool", "not yet\n    /// read anywhere"),
            ("pub autoclean_groups: bool", "forward-compat-stub status"),
            (
                "pub always_split_with_profile: bool",
                "not yet read anywhere",
            ),
            (
                "pub cursor_color_default: bool",
                "permanently a no-op by design",
            ),
            ("pub use_system_font: bool", "forward-compat only"),
            (
                "pub use_theme_colors: bool",
                "forward-compat-\n    /// stub",
            ),
            ("pub extra_styling: bool", "not yet\n    /// read anywhere"),
            ("pub http_proxy: String", "Permanently a no-op by design"),
        ] {
            let decl_pos = src.find(field_decl).unwrap_or_else(|| {
                panic!("field declaration `{field_decl}` not found in lib.rs — did it get renamed or removed? Check docs/CONFIG.md's disposition table before changing it")
            });
            // The marker must appear in the doc-comment block directly
            // above the declaration, not merely somewhere earlier in the
            // file — search a bounded window immediately preceding it.
            // This file is full of multi-byte characters (em dashes,
            // arrows, ...), so a raw byte offset can land mid-character;
            // walk back to the nearest char boundary rather than let the
            // slice panic.
            let mut window_start = decl_pos.saturating_sub(700);
            while window_start > 0 && !src.is_char_boundary(window_start) {
                window_start -= 1;
            }
            let window = &src[window_start..decl_pos];
            assert!(
                window.contains(marker),
                "expected the doc comment directly above `{field_decl}` to \
                 contain {marker:?}, marking it as an intentionally inert \
                 Terminator-parity stub (see docs/CONFIG.md); doc comment \
                 window was: {window:?}"
            );
        }
    }

    #[test]
    fn group_focus_handle_window_state_parse() {
        // Drift guard.
        let d = Config::default();
        assert!(!d.split_to_group);
        assert!(d.autoclean_groups);
        assert!(!d.always_split_with_profile);
        assert_eq!(d.focus, FocusMode::Click);
        assert_eq!(d.handle_size, -1);
        assert_eq!(d.window_state, WindowState::Normal);
        assert_eq!(d.window_width, None);
        assert_eq!(d.window_height, None);
        assert_eq!(d.window_position_x, None);
        assert_eq!(d.window_position_y, None);
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
        let geom = Config::parse_text(
            "window-width = 120\n\
             window_height = 40\n\
             window-position-x = -1920\n\
             window_position_y = 80\n",
        );
        assert_eq!(geom.window_width, Some(120));
        assert_eq!(geom.window_height, Some(40));
        assert_eq!(geom.window_position_x, Some(-1920));
        assert_eq!(geom.window_position_y, Some(80));
        assert_eq!(
            Config::parse_text("window-width = 9999").window_width,
            Some(WINDOW_WIDTH_MAX)
        );
        assert_eq!(
            Config::parse_text("window-height = 1").window_height,
            Some(WINDOW_HEIGHT_MIN)
        );
        // group bools both forms.
        assert!(Config::parse_text("split-to-group = true").split_to_group);
        assert!(Config::parse_text("split_to_group = true").split_to_group);
        assert!(!Config::parse_text("autoclean-groups = false").autoclean_groups);
        assert!(!Config::parse_text("autoclean_groups = false").autoclean_groups);
        assert!(Config::parse_text("always-split-with-profile = true").always_split_with_profile);
        assert!(Config::parse_text("always_split_with_profile = true").always_split_with_profile);
    }

    #[test]
    fn gpu_power_preference_parse() {
        // Default is Auto: the platform/wgpu policy picks unless the user pins
        // a GPU or explicitly asks for low/high power preference.
        assert_eq!(
            Config::default().gpu_power_preference,
            GpuPowerPreference::Auto
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = high").gpu_power_preference,
            GpuPowerPreference::High
        );
        assert_eq!(
            Config::parse_text("gpu_power_preference = discrete").gpu_power_preference,
            GpuPowerPreference::High
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = high-performance").gpu_power_preference,
            GpuPowerPreference::High
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = auto").gpu_power_preference,
            GpuPowerPreference::Auto
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = automatic").gpu_power_preference,
            GpuPowerPreference::Auto
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = low").gpu_power_preference,
            GpuPowerPreference::Low
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = low-power").gpu_power_preference,
            GpuPowerPreference::Low
        );
        assert_eq!(
            Config::parse_text("gpu-power-preference = integrated").gpu_power_preference,
            GpuPowerPreference::Low
        );
        // Unknown value falls back to the safe default, not a parse error.
        assert_eq!(
            Config::parse_text("gpu-power-preference = bogus").gpu_power_preference,
            GpuPowerPreference::Auto
        );
        // `--check-config` accepts every documented spelling and UI label.
        for value in [
            "auto",
            "automatic",
            "none",
            "low",
            "low-power",
            "integrated",
            "high",
            "high-performance",
            "discrete",
        ] {
            let text = format!("gpu-power-preference = {value}");
            assert!(
                Config::detect_malformed_values(&text).is_empty(),
                "{value} must validate"
            );
        }
        assert!(
            !Config::detect_malformed_values("gpu-power-preference = nonsense").is_empty(),
            "an invalid value must be flagged by --check-config"
        );
    }

    #[test]
    fn chrome_background_parse() {
        // v2.23.0. Default is Theme (matches the no-wallpaper look).
        assert_eq!(Config::default().chrome_background, ChromeBackground::Theme);
        assert_eq!(
            Config::parse_text("chrome-background = auto").chrome_background,
            ChromeBackground::Auto
        );
        assert_eq!(
            Config::parse_text("chrome_background = black").chrome_background,
            ChromeBackground::Black
        );
        assert_eq!(
            Config::parse_text("chrome-background = white").chrome_background,
            ChromeBackground::White
        );
        // Unknown value falls back to the default, not a parse error.
        assert_eq!(
            Config::parse_text("chrome-background = bogus").chrome_background,
            ChromeBackground::Theme
        );
        // `--check-config` accepts the documented spellings, flags garbage.
        assert!(Config::detect_malformed_values("chrome-background = auto").is_empty());
        assert!(!Config::detect_malformed_values("chrome-background = rainbow").is_empty());
    }

    #[test]
    fn gpu_selection_parse_and_backward_compat() {
        // v2.23.0. A config with NO gpu pin (the historic shape, only
        // gpu-power-preference) leaves the pin fields at their unset defaults,
        // so resolve_adapter falls through to the power-preference policy
        // exactly as before — backward compatible.
        let legacy = Config::parse_text("gpu-power-preference = high");
        assert_eq!(legacy.gpu_vendor_id, 0);
        assert_eq!(legacy.gpu_device_id, 0);
        assert!(legacy.gpu_name.is_empty());
        assert_eq!(legacy.gpu_backend, GpuBackend::Auto);
        assert!(!legacy.gpu_force_software);

        // A pinned GPU: hex ids (the form the picker writes) + backend + name.
        let pinned = Config::parse_text(
            "gpu-vendor-id = 0x10de\n\
             gpu-device-id = 0x2191\n\
             gpu-backend = dx12\n\
             gpu-name = NVIDIA GeForce GTX 1660 Ti\n\
             gpu-force-software = false\n",
        );
        assert_eq!(pinned.gpu_vendor_id, 0x10de);
        assert_eq!(pinned.gpu_device_id, 0x2191);
        assert_eq!(pinned.gpu_backend, GpuBackend::Dx12);
        assert_eq!(pinned.gpu_name, "NVIDIA GeForce GTX 1660 Ti");
        assert!(!pinned.gpu_force_software);

        // Decimal ids parse too; force-software toggles.
        let dec = Config::parse_text("gpu-vendor-id = 32902\ngpu-force-software = true");
        assert_eq!(dec.gpu_vendor_id, 32902); // 0x8086 Intel
        assert!(dec.gpu_force_software);

        // Backend aliases + validation.
        assert_eq!(
            Config::parse_text("gpu-backend = vulkan").gpu_backend,
            GpuBackend::Vulkan
        );
        assert_eq!(
            Config::parse_text("gpu-backend = bogus").gpu_backend,
            GpuBackend::Auto
        );
        assert!(Config::detect_malformed_values("gpu-backend = metal").is_empty());
        assert!(!Config::detect_malformed_values("gpu-backend = quux").is_empty());
        assert!(!Config::detect_malformed_values("gpu-vendor-id = zzz").is_empty());
        assert!(Config::detect_malformed_values("gpu-vendor-id = 0x10de").is_empty());
    }

    #[test]
    fn parse_hex_or_dec_u32_forms() {
        assert_eq!(parse_hex_or_dec_u32("0x10de"), Some(0x10de));
        assert_eq!(parse_hex_or_dec_u32("0X10DE"), Some(0x10de));
        assert_eq!(parse_hex_or_dec_u32("#10de"), Some(0x10de));
        assert_eq!(parse_hex_or_dec_u32("4318"), Some(4318));
        assert_eq!(parse_hex_or_dec_u32("  0x8086 "), Some(0x8086));
        assert_eq!(parse_hex_or_dec_u32("zzz"), None);
        assert_eq!(parse_hex_or_dec_u32(""), None);
    }

    #[test]
    fn key_encoding_broadcast_url_offsets_parse() {
        // Drift guard.
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
        // Drift guard for 8 bool config keys from
        // terminatorlib/config.py:75-97.
        let d = Config::default();
        assert!(d.close_button_on_tab);
        assert!(!d.new_tab_after_current_tab);
        assert!(!d.title_at_bottom);
        assert!(d.scroll_tabbar);
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
    }

    #[test]
    fn shell_exec_and_close_behavior_parse() {
        // Drift guard.
        let d = Config::default();
        assert!(!d.login_shell);
        assert_eq!(d.exit_action, ExitAction::Close);
        assert_eq!(d.ask_before_closing, AskBeforeClosing::MultipleTerminals);
        assert!(Config::parse_text("login-shell = true").login_shell);
        assert!(Config::parse_text("login_shell = true").login_shell);
        // v2.30.0: shell-integration is a bool, default on.
        assert!(Config::default().shell_integration);
        assert!(!Config::parse_text("shell-integration = off").shell_integration);
        assert!(!Config::parse_text("shell-integration = false").shell_integration);
        assert!(Config::parse_text("shell_integration = on").shell_integration);
        assert!(Config::parse_text("shell-integration = true").shell_integration);
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

    /// `agent-server` defaults OFF and parses the
    /// three modes; a typo falls back to OFF (fail-safe — never silently
    /// enable a control surface). The `--check-config` validator pins the value
    /// set so a typo is reported, not silently defaulted.
    #[test]
    fn agent_server_defaults_off_and_parses() {
        assert_eq!(Config::default().agent_server, AgentServer::Off);
        assert!(!Config::default().agent_server.is_enabled());
        assert_eq!(
            Config::parse_text("agent-server = full").agent_server,
            AgentServer::Full
        );
        assert!(
            Config::parse_text("agent-server = full")
                .agent_server
                .allows_mutation()
        );
        assert_eq!(
            Config::parse_text("agent-server = read-only").agent_server,
            AgentServer::ReadOnly
        );
        assert!(
            !Config::parse_text("agent-server = read-only")
                .agent_server
                .allows_mutation(),
            "read-only must not permit mutation"
        );
        assert_eq!(
            Config::parse_text("agent_server = readonly").agent_server,
            AgentServer::ReadOnly
        );
        // Fail-safe: an unknown value never enables the server.
        assert_eq!(
            Config::parse_text("agent-server = yolo").agent_server,
            AgentServer::Off
        );
    }

    /// Terminator parity: `cursor_bg_color` (the block) aliases
    /// `cursor-color` → theme.cursor; `cursor_fg_color` (the glyph under the
    /// cursor) → theme.cursor_text. Both spellings validate as colors.
    #[test]
    fn cursor_fg_bg_color_split() {
        let c = Config::parse_text("cursor-bg-color = #112233\ncursor-fg-color = #445566");
        assert_eq!(c.theme.cursor, crate::color::Rgb::new(0x11, 0x22, 0x33));
        assert_eq!(
            c.theme.cursor_text,
            crate::color::Rgb::new(0x44, 0x55, 0x66)
        );
        // Terminator snake_case spellings work too.
        let c = Config::parse_text("cursor_bg_color = #aabbcc\ncursor_fg_color = #ddeeff");
        assert_eq!(c.theme.cursor, crate::color::Rgb::new(0xaa, 0xbb, 0xcc));
        assert_eq!(
            c.theme.cursor_text,
            crate::color::Rgb::new(0xdd, 0xee, 0xff)
        );
        // Both validate clean; a bad value is diagnosed.
        assert!(Config::detect_malformed_values("cursor-fg-color = #445566\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("cursor-bg-color = nope\n").len(),
            1
        );
    }

    /// Accent resolution + Peacock (including multi-window support). Peacock
    /// (`auto`) is the DEFAULT now — seed 0 lands on the theme's signature
    /// accent (Mocha mauve), other seeds spread across the pool.
    /// `accent-color = theme` (or `off`/`none`) opts back into the static
    /// signature accent; an explicit hex / `--accent` wins over everything.
    #[test]
    fn accent_resolution_and_peacock() {
        let theme = Theme::default(); // Catppuccin Mocha, accent = mauve
        let mauve = crate::color::Rgb::new(0xcb, 0xa6, 0xf7);

        // Default config: Peacock ON, seed 0 → the pool's first entry, which
        // is the theme's signature accent (so a fresh home-dir window still
        // matches the app icon).
        let cfg = Config::default();
        assert!(cfg.accent_auto, "Peacock is the default");
        assert_eq!(cfg.resolved_accent(&theme), mauve);

        // `theme` / `off` / `none` opt out → static signature accent.
        for opt_out in ["theme", "off", "none"] {
            let cfg = Config::parse_text(&format!("accent-color = {opt_out}"));
            assert!(!cfg.accent_auto, "{opt_out} disables Peacock");
            assert!(cfg.accent_color.is_none());
            assert_eq!(cfg.resolved_accent(&theme), mauve);
            assert!(
                Config::detect_malformed_values(&format!("accent-color = {opt_out}\n")).is_empty(),
                "{opt_out} validates clean"
            );
        }

        // Explicit hex wins.
        let cfg = Config::parse_text("accent-color = #112233");
        assert!(!cfg.accent_auto);
        assert_eq!(
            cfg.resolved_accent(&theme),
            crate::color::Rgb::new(0x11, 0x22, 0x33)
        );

        // `auto` parses to the Peacock flag (not a color), validates clean.
        let cfg = Config::parse_text("accent-color = auto");
        assert!(cfg.accent_auto);
        assert!(cfg.accent_color.is_none());
        assert!(Config::detect_malformed_values("accent-color = auto\n").is_empty());

        // The public pool is deduped, non-empty, signature-first.
        let pool = crate::peacock_pool(&theme);
        assert!(!pool.is_empty());
        assert_eq!(pool[0], mauve, "signature accent leads the pool");
        let mut uniq = pool.clone();
        uniq.dedup();
        assert_eq!(uniq.len(), pool.len(), "pool has no duplicate hues");

        // Peacock is deterministic per seed and spreads across seeds.
        let mut cfg = Config::parse_text("accent-color = auto");
        cfg.accent_seed = 0;
        let a0 = cfg.resolved_accent(&theme);
        assert_eq!(cfg.resolved_accent(&theme), a0, "same seed → same accent");
        let mut colors: Vec<(u8, u8, u8)> = (0u64..8)
            .map(|s| {
                cfg.accent_seed = s;
                let c = cfg.resolved_accent(&theme);
                (c.r, c.g, c.b)
            })
            .collect();
        colors.sort_unstable();
        colors.dedup();
        assert!(colors.len() >= 6, "Peacock spreads seeds across hues");

        // An explicit hex still wins over auto.
        cfg.accent_color = Some(crate::color::Rgb::new(0x0a, 0x0b, 0x0c));
        assert_eq!(
            cfg.resolved_accent(&theme),
            crate::color::Rgb::new(0x0a, 0x0b, 0x0c)
        );
    }

    /// A DERIVED theme accent (no explicit `accent` line →
    /// snapshots `palette[4]` at parse time) follows a config-level
    /// `palette = 4=#hex` override; an EXPLICIT theme accent stays put.
    #[test]
    fn derived_accent_follows_palette4_override() {
        // A theme body without an `accent` line derives accent = palette[4].
        let t = Theme::parse("palette = 4=#336699");
        assert_eq!(t.accent, crate::color::Rgb::new(0x33, 0x66, 0x99));

        // Catppuccin Mocha's accent is EXPLICIT (mauve ≠ palette[4]) — a config
        // palette override must NOT hijack it. Explicitly load Mocha: the shipped
        // default is now TokyoNight Night, whose accent DERIVES from palette[4].
        let cfg = Config::parse_text("theme = Catppuccin Mocha\npalette = 4=#102030");
        let mauve = crate::color::Rgb::new(0xcb, 0xa6, 0xf7);
        assert_eq!(
            cfg.theme.palette[4],
            crate::color::Rgb::new(0x10, 0x20, 0x30)
        );
        assert_eq!(cfg.theme.accent, mauve, "explicit accent stays");

        // A derived accent follows the override through the REAL parse path:
        // Dracula has no `accent` line, so its accent derives from palette[4]
        // — and the config-level palette override must carry it along (the
        // chrome accent used to silently stay the OLD blue).
        let cfg = Config::parse_text("theme = Dracula\npalette = 4=#102030");
        assert_eq!(
            cfg.theme.palette[4],
            crate::color::Rgb::new(0x10, 0x20, 0x30)
        );
        assert_eq!(
            cfg.theme.accent,
            crate::color::Rgb::new(0x10, 0x20, 0x30),
            "derived accent follows a palette[4] override"
        );
    }

    #[test]
    fn invert_search_and_env_strings_parse() {
        // Drift guard.
        let d = Config::default();
        assert!(!d.invert_search);
        assert_eq!(d.term, "xterm-256color");
        assert_eq!(d.colorterm, "truecolor");
        assert!(d.env.is_empty());
        assert!(Config::parse_text("invert-search = true").invert_search);
        assert!(Config::parse_text("invert_search = true").invert_search);
        assert_eq!(
            Config::parse_text("term = screen-256color").term,
            "screen-256color"
        );
        assert_eq!(Config::parse_text("colorterm = 24bit").colorterm, "24bit");
        assert_eq!(
            Config::parse_text("env = EDITOR=nvim\nenv = FOO=bar=baz\nenv = EMPTY=\n").env,
            vec![
                ("EDITOR".to_string(), "nvim".to_string()),
                ("FOO".to_string(), "bar=baz".to_string()),
                ("EMPTY".to_string(), String::new())
            ]
        );
        assert!(Config::parse_text("env = BAD-NAME=value").env.is_empty());
        // Empty value preserves the default (avoids breaking shells
        // that rely on a non-empty TERM).
        assert_eq!(Config::parse_text("term =").term, "xterm-256color");
    }

    #[test]
    fn mouse_and_paste_bools_parse() {
        // Drift guard: smart_copy defaults true (Terminator
        // default); others default false.
        let d = Config::default();
        assert!(!d.disable_mousewheel_zoom);
        assert!(!d.disable_mouse_paste);
        assert!(d.clipboard_paste_protection);
        assert!(!d.putty_paste_style);
        assert!(d.smart_copy);
        assert!(Config::parse_text("disable-mousewheel-zoom = true").disable_mousewheel_zoom);
        assert!(Config::parse_text("disable_mousewheel_zoom = true").disable_mousewheel_zoom);
        assert!(Config::parse_text("disable-mouse-paste = true").disable_mouse_paste);
        assert!(Config::parse_text("disable_mouse_paste = true").disable_mouse_paste);
        assert!(
            !Config::parse_text("clipboard-paste-protection = false").clipboard_paste_protection
        );
        assert!(
            !Config::parse_text("clipboard_paste_protection = false").clipboard_paste_protection
        );
        assert!(Config::parse_text("putty-paste-style = true").putty_paste_style);
        assert!(Config::parse_text("putty_paste_style = true").putty_paste_style);
        assert!(!Config::parse_text("smart-copy = false").smart_copy);
        assert!(!Config::parse_text("smart_copy = false").smart_copy);
    }

    #[test]
    fn tab_bar_position_terminator_aliases() {
        // Drift guard. Terminator's `tab_position` accepts
        // top/left/right/bottom/hidden; kettle maps them as:
        //   - top/bottom: native.
        //   - hidden: alias to `tab-bar = off`.
        //   - left/right: promoted from warn-fallback-to-top
        //     to actual `TabBarPos::Left` / `TabBarPos::Right` storage.
        //     The render-layer change to draw vertical strips lands in
        //     phases 2-6 of TERMINATOR-VERTICAL-TABS-DESIGN.md.
        let hidden = Config::parse_text("tab-bar-position = hidden");
        assert_eq!(hidden.tab_bar, TabBarMode::Off);
        let left = Config::parse_text("tab-bar-position = left");
        assert_eq!(left.tab_bar_pos, TabBarPos::Left);
        let right = Config::parse_text("tab-bar-position = right");
        assert_eq!(right.tab_bar_pos, TabBarPos::Right);
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

    /// The bare `padding-x`/`-y` spellings the diagnostic
    /// already accepted must actually apply (and not warn as unknown) — they
    /// were drift: passed `--check-config` yet did nothing + warned "unknown".
    #[test]
    fn bare_padding_aliases_apply_and_are_known() {
        let (cfg, unknown) = Config::parse_collect("padding-x = 12\npadding-y = 5\n");
        assert_eq!(cfg.padding_x, 12.0);
        assert_eq!(cfg.padding_y, 5.0);
        assert!(
            unknown.is_empty(),
            "bare padding aliases must not be reported as unknown: {unknown:?}"
        );
    }

    /// `accent_color` (snake_case) was validated by
    /// `detect_malformed_values` but the apply arm only handled
    /// `accent-color`, so `accent_color = #ff8800` passed `--check-config`,
    /// warned "unknown key", AND did nothing — the same validate-but-don't-
    /// apply drift the `_color` aliases and `padding-x` already fixed.
    #[test]
    fn accent_color_snake_case_applies_and_is_known() {
        let (cfg, unknown) = Config::parse_collect("accent_color = #ff8800\n");
        assert_eq!(
            cfg.accent_color,
            Some(Rgb {
                r: 0xff,
                g: 0x88,
                b: 0x00
            }),
            "snake_case accent_color must pin the explicit color"
        );
        assert!(!cfg.accent_auto, "an explicit hex disables Peacock auto");
        assert!(
            unknown.is_empty(),
            "accent_color (snake_case) must not be reported as unknown: {unknown:?}"
        );
    }

    /// The three background-image placement enums fell back
    /// silently in the renderer on a typo — now `detect_malformed_values` pins
    /// them so a typo fails `--check-config` while every documented value (and
    /// snake_case alias) passes.
    #[test]
    fn background_image_placement_enum_typos_are_flagged() {
        let bad = Config::detect_malformed_values(
            "background-image-mode = tyle\n\
             background-image-align-horiz = lft\n\
             background-image-align-vert = botom\n",
        );
        assert_eq!(bad.len(), 3, "all three typos should be flagged: {bad:?}");
        assert!(bad.iter().any(|b| b.contains("background-image-mode")));
        assert!(
            bad.iter()
                .any(|b| b.contains("background-image-align-horiz"))
        );
        assert!(
            bad.iter()
                .any(|b| b.contains("background-image-align-vert"))
        );
        // Every documented value (and the snake_case key alias) passes cleanly.
        let ok = Config::detect_malformed_values(
            "background-image-mode = tile\n\
             background-image-mode = center\n\
             background-image-mode = scale\n\
             background-image-mode = stretch_and_fill\n\
             background_image_mode = TILE\n\
             background-image-align-horiz = left\n\
             background-image-align-horiz = center\n\
             background-image-align-horiz = right\n\
             background_image_align_horiz = RIGHT\n\
             background-image-align-vert = top\n\
             background-image-align-vert = middle\n\
             background-image-align-vert = bottom\n\
             background_image_align_vert = TOP\n",
        );
        assert!(ok.is_empty(), "all valid placement values: {ok:?}");
    }

    /// Accepting a value is a promise that it does something.
    ///
    /// The test above pins `TILE` and `RIGHT` as passing `--check-config`,
    /// because the validator lowercases before comparing. The renderer does
    /// not: it matches `cfg.background_image_mode.as_str()` against lowercase
    /// literals, so an accepted-but-uppercased value fell straight through to
    /// the default arm. `background_image_mode = Tile` was reported valid and
    /// then tiled nothing.
    ///
    /// Every spelling the validator accepts must therefore be STORED in the
    /// one spelling the renderer matches.
    #[test]
    fn an_accepted_placement_value_is_stored_in_the_spelling_the_renderer_matches() {
        for (key, canonical) in [
            (
                "background-image-mode",
                ["tile", "center", "scale", "stretch_and_fill"].as_slice(),
            ),
            (
                "background-image-align-horiz",
                ["left", "center", "right"].as_slice(),
            ),
            (
                "background-image-align-vert",
                ["top", "middle", "bottom"].as_slice(),
            ),
        ] {
            for value in canonical {
                for spelling in [
                    value.to_string(),
                    value.to_ascii_uppercase(),
                    // Leading capital — what a user actually types.
                    {
                        let mut c = value.chars();
                        match c.next() {
                            Some(f) => f.to_ascii_uppercase().to_string() + c.as_str(),
                            None => String::new(),
                        }
                    },
                ] {
                    let line = format!("{key} = {spelling}\n");
                    assert!(
                        Config::detect_malformed_values(&line).is_empty(),
                        "{line:?} must pass --check-config"
                    );
                    let cfg = Config::parse_text(&line);
                    let stored = match key {
                        "background-image-mode" => &cfg.background_image_mode,
                        "background-image-align-horiz" => &cfg.background_image_align_horiz,
                        _ => &cfg.background_image_align_vert,
                    };
                    assert_eq!(
                        stored, value,
                        "{spelling:?} passes the checker, so it must reach the \
                         renderer as {value:?} rather than falling through its \
                         match to the default"
                    );
                }
            }
        }
    }

    /// Everything a real Terminator config contains must ALSO pass
    /// `--check-config`, not merely apply.
    ///
    /// `a_real_terminator_config_imports` proves the values reach the Config.
    /// It calls `parse_text`, never `detect_malformed_values` — so the colon
    /// palette straight out of Terminator applied correctly at runtime while
    /// the checker called it malformed, sending the user to fix a line that
    /// was already right. Reporting a working line as broken is its own bug.
    #[test]
    fn a_real_terminator_config_also_passes_the_checker() {
        let text = "scroll_on_keystroke = False
scroll_on_output = True
background_color = \"#1a1b26\"
foreground_color = '#c0caf5'
font = DejaVu Sans Mono 13
palette = \"#2e3436:#cc0000:#4e9a06:#c4a000:#3465a4:#75507b:#06989a:#d3d7cf\"
new_tab = <Control><Shift>y
split_horiz = <Control><Shift>j
";
        let bad = Config::detect_malformed_values(text);
        assert!(
            bad.is_empty(),
            "a config that imports correctly must not be reported malformed: {bad:?}"
        );

        // A 16-colour list is equally valid, and a wrong-length or
        // non-colour list must still be caught rather than blanket-accepted.
        let sixteen: Vec<String> = (0..16).map(|i| format!("#0000{i:02x}")).collect();
        assert!(
            Config::detect_malformed_values(&format!(
                "palette = {}
",
                sixteen.join(":")
            ))
            .is_empty(),
            "a 16-colour colon palette is valid"
        );
        assert!(
            !Config::detect_malformed_values(
                "palette = #2e3436:#cc0000:#4e9a06
"
            )
            .is_empty(),
            "a 3-colour list is not a palette and must still be flagged"
        );
        assert!(
            !Config::detect_malformed_values(
                "palette = #2e3436:nope:#4e9a06:#c4a000:#3465a4:#75507b:#06989a:#d3d7cf
"
            )
            .is_empty(),
            "a non-colour entry must still be flagged"
        );
    }

    /// Reverse-coverage guard — every key name the
    /// `detect_malformed_values` validator recognizes must ALSO be recognized
    /// (applied, not warned-as-unknown) by `parse_collect`. This is the test
    /// that would have caught F1 (`accent_color` validated but not applied):
    /// validate-but-don't-apply is a contradictory diagnostic (passes
    /// `--check-config` yet warns "unknown key" + does nothing).
    ///
    /// The detect arms are match-arm literals — not enumerable at runtime — so
    /// this list is hand-maintained. It mirrors at least every `_color` /
    /// snake_case alias and the three background-image placement keys, each fed
    /// a valid value; if a future edit teaches the validator a key the parser
    /// doesn't apply, this reds.
    #[test]
    fn every_validated_key_is_also_applied() {
        // (key, valid-value) pairs mirroring the detect_malformed_values arms.
        let cases: &[(&str, &str)] = &[
            // Color keys + their Terminator/snake_case aliases.
            ("background", "#101010"),
            ("foreground", "#fafafa"),
            ("background-color", "#101010"),
            ("background_color", "#101010"),
            ("foreground-color", "#fafafa"),
            ("foreground_color", "#fafafa"),
            ("cursor-color", "#ff0000"),
            ("cursor-bg-color", "#ff0000"),
            ("cursor_bg_color", "#ff0000"),
            ("cursor-fg-color", "#00ff00"),
            ("cursor_fg_color", "#00ff00"),
            ("selection-background", "#222222"),
            ("selection-foreground", "#eeeeee"),
            ("search-foreground", "#000000"),
            ("search-background", "#ffff00"),
            ("split-divider-color", "#333333"),
            ("focused-split-color", "#7aa2f7"),
            ("split-divider-color-focused", "#7aa2f7"),
            ("title-transmit-bg-color", "#112233"),
            ("title_transmit_bg_color", "#112233"),
            ("title-receive-bg-color", "#112233"),
            ("title_receive_bg_color", "#112233"),
            ("title-inactive-bg-color", "#112233"),
            ("title_inactive_bg_color", "#112233"),
            ("title-transmit-fg-color", "#445566"),
            ("title_transmit_fg_color", "#445566"),
            ("title-receive-fg-color", "#445566"),
            ("title_receive_fg_color", "#445566"),
            ("title-inactive-fg-color", "#445566"),
            ("title_inactive_fg_color", "#445566"),
            // Accent — the F1 regression: snake_case validated but not applied.
            ("accent-color", "#ff8800"),
            ("accent_color", "#ff8800"),
            // Enum keys with snake_case aliases.
            ("background-type", "image"),
            ("background_type", "image"),
            ("text-renderer", "grid"),
            ("text_renderer", "grid"),
            ("modify-other-keys", "enter"),
            ("modify_other_keys", "enter"),
            // The three F2 background-image placement enums (+ snake_case).
            ("background-image-mode", "scale"),
            ("background_image_mode", "scale"),
            ("background-image-align-horiz", "right"),
            ("background_image_align_horiz", "right"),
            ("background-image-align-vert", "top"),
            ("background_image_align_vert", "top"),
            // Other snake_case-aliased keys validated by detect.
            ("theme-schedule", "sunrise/sunset"),
            ("theme_schedule", "sunrise/sunset"),
            ("ask-before-closing", "always"),
            ("ask_before_closing", "always"),
            ("light-theme", "TokyoNight"),
            ("light_theme", "TokyoNight"),
            ("dark-theme", "TokyoNight"),
            ("dark_theme", "TokyoNight"),
            ("theme-mode", "dark"),
            ("theme_mode", "dark"),
            ("cursor-shape", "block"),
            ("cursor_shape", "block"),
        ];
        for (key, val) in cases {
            // The validator must not flag the valid value...
            let malformed = Config::detect_malformed_values(&format!("{key} = {val}\n"));
            assert!(
                malformed.is_empty(),
                "validator flagged a valid `{key} = {val}`: {malformed:?}"
            );
            // ...and the parser must recognize (apply) the key, not warn unknown.
            let (_cfg, unknown) = Config::parse_collect(&format!("{key} = {val}\n"));
            assert!(
                unknown.is_empty(),
                "`{key}` is validated but parse_collect reports it unknown \
                 (validate-but-don't-apply drift): {unknown:?}"
            );
        }
    }

    #[test]
    fn explicit_color_overrides_survive_a_later_theme_line() {
        // Regression: `background = #ff0000` followed by `theme = Dracula`
        // used to lose the red because applying the theme overwrote
        // `cfg.theme.background` in place. Explicit single-color overrides
        // are now order-independent (applied AFTER the theme), so each one
        // wins regardless of where the `theme =` line sits.
        let red = Rgb::parse("#ff0000").unwrap();
        let green = Rgb::parse("#00ff00").unwrap();
        let blue = Rgb::parse("#0000ff").unwrap();
        let yellow = Rgb::parse("#ffff00").unwrap();
        let cyan = Rgb::parse("#00ffff").unwrap();
        let magenta = Rgb::parse("#ff00ff").unwrap();

        // theme AFTER the color line — the historically-broken order.
        let cfg = Config::parse_text(
            "background = #ff0000\n\
             foreground = #00ff00\n\
             cursor-color = #0000ff\n\
             cursor-fg-color = #ffff00\n\
             selection-background = #00ffff\n\
             selection-foreground = #ff00ff\n\
             theme = Dracula\n",
        );
        assert_eq!(
            cfg.theme.background, red,
            "explicit bg must beat later theme"
        );
        assert_eq!(cfg.theme.foreground, green);
        assert_eq!(cfg.theme.cursor, blue);
        assert_eq!(cfg.theme.cursor_text, yellow);
        assert_eq!(cfg.theme.selection_background, cyan);
        assert_eq!(cfg.theme.selection_foreground, magenta);

        // theme BEFORE the color line — must stay correct too (symmetry).
        let cfg2 = Config::parse_text(
            "theme = Dracula\n\
             background = #ff0000\n\
             foreground = #00ff00\n\
             cursor-color = #0000ff\n\
             cursor-fg-color = #ffff00\n\
             selection-background = #00ffff\n\
             selection-foreground = #ff00ff\n",
        );
        assert_eq!(cfg2.theme.background, red);
        assert_eq!(cfg2.theme.foreground, green);
        assert_eq!(cfg2.theme.cursor, blue);
        assert_eq!(cfg2.theme.cursor_text, yellow);
        assert_eq!(cfg2.theme.selection_background, cyan);
        assert_eq!(cfg2.theme.selection_foreground, magenta);
    }

    #[test]
    fn theme_with_no_explicit_color_override_still_applies_its_palette() {
        // The order-independence fix must not break the plain case: a
        // `theme =` with no explicit single-color overrides still loads
        // the theme's own colors (we don't stash a default-None over them).
        let dracula = Theme::by_name("Dracula");
        let cfg = Config::parse_text("theme = Dracula\n");
        assert_eq!(cfg.theme.background, dracula.background);
        assert_eq!(cfg.theme.foreground, dracula.foreground);
        assert_eq!(cfg.theme.cursor, dracula.cursor);
        assert_eq!(cfg.theme.selection_background, dracula.selection_background);
    }

    #[test]
    fn trigger_detect_allows_regex_metachars_in_the_command_half() {
        // The trigger detect arm validates only the LHS
        // regex of `REGEX :: command`, mirroring the apply path. A valid
        // line whose COMMAND contains an unbalanced `[` must NOT be flagged
        // (the `[baz` is an argv token, not part of the pattern).
        let ok = Config::detect_malformed_values("trigger = foo :: grep bar[baz\n");
        assert!(
            ok.is_empty(),
            "command-half metachars must not flag the trigger: {ok:?}"
        );
        // A plain `trigger = REGEX` (no `::`) is still validated whole.
        let ok2 = Config::detect_malformed_values("trigger = error.*panic\n");
        assert!(ok2.is_empty(), "valid bare regex must pass: {ok2:?}");
    }

    #[test]
    fn trigger_detect_still_flags_an_invalid_pattern_half() {
        // The LHS regex is still validated, so an unbalanced `[` BEFORE the
        // `::` separator is flagged even though a command follows.
        let bad = Config::detect_malformed_values("trigger = [unclosed :: notify hi\n");
        assert_eq!(bad.len(), 1, "the invalid LHS regex must flag: {bad:?}");
        assert!(bad[0].contains("trigger"));
        // And a bare invalid regex (no command) is still flagged.
        let bad2 = Config::detect_malformed_values("trigger = [unclosed\n");
        assert_eq!(bad2.len(), 1, "bare invalid regex must flag: {bad2:?}");
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
             scrollback = lots\n\
             env = 1BAD=value\n\
             env = NO_SEPARATOR\n",
        );
        assert_eq!(bad.len(), 8, "all eight should be flagged: {bad:?}");
        assert!(bad.iter().any(|b| b.contains("font-size")));
        assert!(bad.iter().any(|b| b.contains("scrollback")));
        assert!(bad.iter().any(|b| b.contains("1BAD")));
        assert!(bad.iter().any(|b| b.contains("NO_SEPARATOR")));
        // Valid values pass cleanly.
        let ok = Config::detect_malformed_values(
            "font-size = 14\n\
             padding-x = 6.5\n\
             background-opacity = 0.8\n\
             scrollback = infinite\n\
             scrollback = 0\n\
             cursor-blink-interval = 800\n\
             env = EDITOR=nvim\n\
             env = EMPTY=\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
        // Unknown keys aren't *malformed* — that's a separate diagnostic
        // (caught by `parse_collect`'s `unknown` Vec). detect_malformed
        // intentionally returns empty for them so the two lists don't
        // duplicate.
        assert!(Config::detect_malformed_values("totally-unknown = x").is_empty());
    }

    /// The bool-key diagnostic must cover the WHOLE
    /// bool-key set, not just 8 of ~100. Round-trip every `BOOL_KEYS` entry
    /// (each must flag a bad value) so the list stays correctly wired, and
    /// spot-check the keys the audit named plus the newly-validated enum keys.
    #[test]
    fn bool_and_enum_typos_are_all_flagged() {
        // Every listed bool key flags a non-bool value, and accepts a good one.
        for &k in Config::BOOL_KEYS {
            let bad = Config::detect_malformed_values(&format!("{k} = notabool\n"));
            assert!(
                bad.iter().any(|m| m.contains(k)),
                "bool key {k:?} not flagged on a bad value"
            );
            let ok = Config::detect_malformed_values(&format!("{k} = true\n"));
            assert!(
                ok.is_empty(),
                "bool key {k:?} rejected a valid `true`: {ok:?}"
            );
        }
        // The keys the audit named (previously silently swallowed).
        for line in [
            "borderless = treu",
            "login-shell = yse",
            "sticky = nope",
            "always-on-top = 1ish",
            "geometry-hinting = maybe",
        ] {
            let bad = Config::detect_malformed_values(&format!("{line}\n"));
            assert_eq!(bad.len(), 1, "{line:?} should be flagged once: {bad:?}");
        }
        // Enum keys that used to fall through to `_ => true`.
        assert_eq!(Config::detect_malformed_values("focus = sloopy\n").len(), 1);
        assert!(Config::detect_malformed_values("focus = sloppy\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("window-state = maximze\n").len(),
            1
        );
        assert!(Config::detect_malformed_values("window-state = maximize\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("window-width = 9999\n").len(),
            1
        );
        assert_eq!(
            Config::detect_malformed_values("window-height = 4\n").len(),
            1
        );
        assert!(Config::detect_malformed_values("window-position-x = -1920\n").is_empty());
        assert!(Config::detect_malformed_values("window-position-y = 80\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("window-position-x = left\n").len(),
            1
        );
        // case-sensitive accepts named modes AND the Terminator bool form.
        assert!(Config::detect_malformed_values("case-sensitive = smart\n").is_empty());
        assert!(Config::detect_malformed_values("case-sensitive = true\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("case-sensitive = ya\n").len(),
            1
        );
        // More enum keys that used to silently default on a typo —
        // each documented value passes; each typo flags exactly once.
        for (good, bad) in [
            ("exit-action = restart", "exit-action = clse"),
            (
                "backspace-binding = control-h",
                "backspace-binding = ctrl_x",
            ),
            ("delete-binding = escape-sequence", "delete-binding = esc"),
            ("broadcast-default = all", "broadcast-default = evrywhere"),
            ("theme-mode = auto", "theme-mode = atuo"),
            ("background-type = image", "background-type = imag"),
            ("lua-sandbox = trusted", "lua-sandbox = trused"),
            ("status-bar = bottom", "status-bar = botom"),
            // theme-schedule + ask-before-closing diagnostic gaps.
            ("ask-before-closing = always", "ask-before-closing = alwyas"),
            (
                "theme-schedule = 18:00 dark, 06:00 light",
                "theme-schedule = 18:00 drak, 06:00 light",
            ),
        ] {
            assert!(
                Config::detect_malformed_values(&format!("{good}\n")).is_empty(),
                "valid {good:?} should pass"
            );
            assert_eq!(
                Config::detect_malformed_values(&format!("{bad}\n")).len(),
                1,
                "{bad:?} should flag once"
            );
        }
        // Color + theme-role keys.
        assert!(Config::detect_malformed_values("accent-color = #ff8800\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("accent-color = nope\n").len(),
            1
        );
        assert!(Config::detect_malformed_values("title-transmit-bg-color = #c80003\n").is_empty());
        assert_eq!(
            Config::detect_malformed_values("dark-theme = NotARealTheme\n").len(),
            1
        );
        // Clamped/range-checked numerics — an in-range value passes;
        // an out-of-range one (which the runtime silently clamps/discards) is
        // flagged exactly once. Bounds mirror parse_collect.
        for (good, bad) in [
            ("handle-size = 12", "handle-size = 9000"),
            ("tab-bar-width = 200", "tab-bar-width = 5"),
            ("background-darkness = 0.4", "background-darkness = 2.0"),
            ("cell-height = 1.2", "cell-height = 9.0"),
            ("cell-width = 1.0", "cell-width = 0.1"),
            ("inactive-color-offset = 0.5", "inactive-color-offset = 3.0"),
            (
                "inactive-bg-color-offset = 0.5",
                "inactive-bg-color-offset = -1.0",
            ),
            ("theme-schedule-lat = 47.6", "theme-schedule-lat = 200"),
            ("theme-schedule-long = -122.3", "theme-schedule-long = 999"),
        ] {
            assert!(
                Config::detect_malformed_values(&format!("{good}\n")).is_empty(),
                "valid {good:?} should pass"
            );
            assert_eq!(
                Config::detect_malformed_values(&format!("{bad}\n")).len(),
                1,
                "out-of-range {bad:?} should flag once"
            );
        }
    }

    #[test]
    fn load_from_with_diagnostics_surfaces_both_unknown_and_malformed() {
        // Contract: a reload via `Action::ReloadConfig` should
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

    /// Drift guard: an oversized config file (swap-attack
    /// scenario; out of strict SECURITY.md scope but defense-in-depth)
    /// must be refused by the bounded `read_config_bytes` reader rather
    /// than read into RAM. Asserts the 1 MiB cap is enforced and the
    /// function falls through to Config::default() rather than allocating.
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

    /// The primary startup/reload load path (`load_from_with_diagnostics`,
    /// via `load_from`/`load()`) must go through the same hardened
    /// `read_config_bytes` reader as the edit path, not a bare
    /// `std::fs::read`. A non-regular file at the resolved config path
    /// (directory, FIFO, device node, ...) must be refused rather than
    /// blocking or streaming unbounded, and must fall through to
    /// `Config::default()` exactly like a missing file — never panic or
    /// hang the caller (every real caller in `kettle`/`kettle-ui` guards
    /// with `.exists()` first, but the guard and the read are two
    /// separate syscalls, so a TOCTOU swap for a special file must still
    /// be handled safely here).
    #[test]
    fn load_from_with_diagnostics_rejects_non_regular_config_path() {
        let dir = std::env::temp_dir().join(format!(
            "kettle-load-from-diag-non-regular-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // Use the directory itself as the "config path": a directory is
        // the one non-regular-file shape that's trivially constructible
        // on every platform (FIFOs are Unix-only; see the symlink-specific
        // test below for Unix's other special-file guard).
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let (cfg, unknown, malformed) = Config::load_from_with_diagnostics(&dir);
        let default_cfg = Config::default();
        assert_eq!(cfg.font_size, default_cfg.font_size);
        assert!(unknown.is_empty(), "no diagnostics for a refused path");
        assert!(malformed.is_empty(), "no diagnostics for a refused path");
        let _ = std::fs::remove_dir(&dir);
    }

    // NB: startup config loading deliberately DOES follow a symlink to its
    // regular-file target (dotfile managers rely on it, and the write path
    // already does) — see `startup_load_follows_symlinked_config_to_regular_target`.
    // The `O_NOFOLLOW` hardening lives one layer down in `read_config_bytes`
    // / `ConfigDocument::read`, guarded by
    // `config_reads_reject_symlinks_and_external_symlink_replacements`.

    #[test]
    fn detect_malformed_values_skips_empty_values() {
        // Empty values are documented as "reset to
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
        // guard — empty skip mustn't swallow
        // typos).
        let bad = Config::detect_malformed_values("theme = NoSuchTheme\n");
        assert_eq!(bad.len(), 1, "unknown theme still flagged: {bad:?}");
    }

    #[test]
    fn detect_malformed_values_strips_bom_before_scanning() {
        // A BOM-prefixed config
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
        // bell / osc52 / tab-bar / tab-bar-position /
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

        // cursor-style (the `beam` alias for `bar` works with any case too).
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
        // A user copying their Alacritty config writes
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
        // Pre-fix `cursor-style-blink = no` silently meant
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
        // (the previous behavior).
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
        // A user with `scrollback = 100000000` (100 M
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
    fn scrollback_bytes_parses_units_and_can_be_disabled() {
        let c = Config::parse_text("scrollback-bytes = 25MB");
        assert_eq!(c.scrollback_bytes, 25_000_000);

        let c = Config::parse_text("scrollback-byte-limit = 8MiB");
        assert_eq!(c.scrollback_bytes, 8 * 1024 * 1024);

        let c = Config::parse_text("scrollback-memory = 0");
        assert_eq!(c.scrollback_bytes, 0, "0 disables the byte cap");

        let ok = Config::detect_malformed_values(
            "scrollback-bytes = 10MB\n\
             scrollback-byte-limit = 8MiB\n\
             scrollback-memory = 0\n",
        );
        assert!(ok.is_empty(), "documented byte-budget forms pass: {ok:?}");

        let bad = Config::detect_malformed_values(
            "scrollback-bytes = lots\n\
             scrollback-byte-limit = 999999999999999999999999999999999\n",
        );
        assert_eq!(bad.len(), 2, "bad byte budgets should flag: {bad:?}");
    }

    #[test]
    fn detect_malformed_values_flags_clamped_numerics_out_of_range() {
        // Same shape as the font-size out-of-range fix,
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

        // Runtime clamp on background-opacity —
        // even with the warning ignored, wgpu sees a safe alpha.
        let c = Config::parse_text("background-opacity = 2.5");
        assert_eq!(c.background_opacity, 1.0);
        let c = Config::parse_text("background-opacity = -0.5");
        assert_eq!(c.background_opacity, 0.0);
    }

    /// The validator must cover every alias the apply
    /// path accepts, with bounds that mirror the apply-arm clamps exactly.
    /// Before this, a bad value under an *alias* spelling slipped past
    /// `--check-config` (the diagnostic only knew the canonical key), and a
    /// `padding = inf` / valid `cursor-shape = ibeam` mismatched the runtime.
    #[test]
    fn detect_malformed_values_covers_aliases_and_clamps() {
        // Bad values under the previously-uncovered spellings must flag.
        let bad = Config::detect_malformed_values(
            "cursor-shape = wibble\n\
             cursor_shape = nonsense\n\
             scrollback-limit = 99999999999\n\
             background-color = notacolor\n\
             background_color = zzz\n\
             foreground-color = nope\n\
             foreground_color = quux\n\
             tab-silence-threshold-ms = 0\n\
             tab-silence-threshold-ms = 700000\n\
             command-notify-threshold-ms = 99999999999\n\
             padding-x = inf\n\
             padding_x = inf\n\
             window_padding_x = nan\n\
             padding-y = nan\n\
             padding_y = inf\n",
        );
        assert_eq!(bad.len(), 15, "all fifteen should flag: {bad:?}");

        // The matching valid values must NOT flag — including `ibeam`/`i-beam`
        // (apply accepts them; the diagnostic used to false-positive), the
        // disable sentinel `0` for command-notify, and finite padding.
        let ok = Config::detect_malformed_values(
            "cursor-shape = ibeam\n\
             cursor_shape = i-beam\n\
             cursor-shape = block\n\
             scrollback-limit = 50000\n\
             scrollback-limit = infinite\n\
             background-color = #112233\n\
             foreground-color = rgb:aa/bb/cc\n\
             tab-silence-threshold-ms = 1000\n\
             tab-silence-threshold-ms = 600000\n\
             command-notify-threshold-ms = 0\n\
             command-notify-threshold-ms = 86400000\n\
             padding-x = 8\n\
             padding_x = 8\n\
             window_padding_x = 8\n\
             padding-y = 0\n",
        );
        assert!(ok.is_empty(), "all valid forms pass: {ok:?}");
    }

    #[test]
    fn detect_malformed_values_flags_font_size_out_of_renderer_range() {
        // font-size = 500 silently clamps to 72 at the
        // renderer (via `clamp_font_size`); --check-config
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
        // Non-numeric still goes through the parse-fail path.
        let bad = Config::detect_malformed_values("font-size = abc\n");
        assert_eq!(bad.len(), 1);
    }

    #[test]
    fn detect_malformed_values_flags_palette_index_out_of_range() {
        // Documented as 0..=255 but the runtime apply path
        // only writes the 0..=15 ANSI palette. Flag 16+ so the user's
        // typo doesn't silently no-op (the diagnostic is on the same
        // surface as the ssh-host `name=` half-empty-value flag).
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
        // v2.26.0 (audit): VTE/Terminator `word_chars` is the INVERSE concept
        // (word constituents, not delimiters), so it is intentionally NOT an
        // alias — it leaves `word_delimiters` at its default rather than
        // inverting the user's intent.
        assert!(
            Config::parse_text("word_chars = abcXYZ")
                .word_delimiters
                .is_empty()
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
        // Terminator `mouse_autohide` (terminatorlib
        // config.py:249) is accepted as an alias (both
        // underscore + hyphen spellings).
        assert!(!Config::parse_text("mouse_autohide = false").mouse_hide_while_typing);
        assert!(!Config::parse_text("mouse-autohide = false").mouse_hide_while_typing);
    }

    #[test]
    fn unknown_keys_are_reported_not_fatal() {
        let (cfg, unknown) = Config::parse_collect(
            "font-size = 14\nfont-szie = 99\ntheme = TokyoNight Night\nbogus = x\nhomogeneous-tabbar = true\n",
        );
        // Valid keys still applied.
        assert_eq!(cfg.font_size, 14.0);
        // Typo'd / unknown keys collected, sorted + deduped.
        assert_eq!(
            unknown,
            vec![
                "bogus".to_string(),
                "font-szie".to_string(),
                "homogeneous-tabbar".to_string()
            ]
        );
    }

    #[test]
    fn status_bar_parses_off_top_bottom_with_aliases() {
        // Drift guard. Default is Off (no row stolen from
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
        // Drift guard. `trigger = REGEX` accumulates into
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

    /// Drift guard. `menu-item = LABEL = CMD` accumulates
    /// `MenuItem` rows used by the chrome to extend the right-click
    /// context menu (Terminator parity, `custom_commands.py`). The
    /// FIRST `=` in the line splits key/value (consumed by the
    /// config key/value line parser); the FIRST `=` of the value splits
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
        // grammar (underscore-vs-kebab cleanup).
        let alias_cfg = Config::parse_text("menu_item = test = ls\n");
        assert_eq!(alias_cfg.menu_items.len(), 1);
        assert_eq!(alias_cfg.menu_items[0].label, "test");
    }

    /// Drift guard for the check-config malformed-value
    /// surfacing. A `menu-item` line without a second `=` (or with
    /// empty label / command) should show up in the malformed list
    /// so the user sees the issue at `kettle --check-config` time
    /// rather than silently getting no menu row.
    ///
    /// This doc continues after a paragraph break to satisfy
    /// clippy's `doc_list_item_without_indent` lint that fires
    /// when consecutive `///` lines after a single `#[test]` look
    /// like a list continuation.
    #[test]
    fn command_notify_threshold_parses_and_clamps() {
        // body intentionally not changed here; the test
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

    /// `nan`/`inf` parse as valid `f32`, and `clamp(NaN)`
    /// returns NaN — defeating the clamp's "keep the runtime safe" purpose. A
    /// non-finite value must be rejected, leaving the field at its finite
    /// default rather than poisoning rendering with NaN.
    #[test]
    fn non_finite_floats_are_rejected_keeping_default() {
        let def = Config::default();
        let cfg = Config::parse_text("background-opacity = nan\n");
        assert_eq!(cfg.background_opacity, def.background_opacity);
        assert!(cfg.background_opacity.is_finite());
        let cfg = Config::parse_text("cell-height = inf\ncell-width = -inf\n");
        assert_eq!(cfg.cell_height, def.cell_height);
        assert_eq!(cfg.cell_width, def.cell_width);
        let cfg = Config::parse_text("background-darkness = NaN\ntab-bar-width = inf\n");
        assert_eq!(cfg.background_darkness, def.background_darkness);
        assert_eq!(cfg.tab_bar_width, def.tab_bar_width);
    }

    /// Drift guard. `force-no-bell = true` overrides the
    /// `bell` config key — Terminator-parity hard-off for users
    /// who want to silence every bell flavor with one key.
    /// Previously the key parsed but was a documented no-op.
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

    /// Drift guard. `light-theme` / `dark-theme` config
    /// keys store the *canonical* bundled name when the user-typed
    /// value matches one (so the toggle action can do exact-name
    /// matching against `cfg.theme_name`); both kebab + underscore
    /// spellings are accepted; an empty / whitespace-only value is
    /// ignored (so commenting-out via `light-theme = ` doesn't
    /// stick a stray empty string).
    #[test]
    fn search_case_sensitive_parses_terminator_and_named_forms() {
        use SearchCaseSensitivity::*;
        // Default is Smart (ripgrep semantics) — kettle's earlier behavior.
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

    /// Drift guard. `TabBarPos::is_vertical` + the
    /// parser. Phase 1 of [`TERMINATOR-VERTICAL-TABS-DESIGN.md`](
    /// ../../../docs/TERMINATOR-VERTICAL-TABS-DESIGN.md).
    #[test]
    fn tab_bar_pos_left_right_parse_and_classify() {
        // is_vertical classification.
        assert!(!TabBarPos::Top.is_vertical());
        assert!(!TabBarPos::Bottom.is_vertical());
        assert!(TabBarPos::Left.is_vertical());
        assert!(TabBarPos::Right.is_vertical());
        // Parser routes the values to the new variants.
        let cfg = Config::parse_text("tab-bar-position = left\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Left);
        let cfg = Config::parse_text("tab-bar-position = right\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Right);
        // Terminator-spelled alias still works.
        let cfg = Config::parse_text("tab-position = left\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Left);
        let cfg = Config::parse_text("tab_position = right\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Right);
        // Top + Bottom unchanged.
        let cfg = Config::parse_text("tab-bar-position = top\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Top);
        let cfg = Config::parse_text("tab-bar-position = bottom\n");
        assert_eq!(cfg.tab_bar_pos, TabBarPos::Bottom);
    }

    /// Drift guard. Terminator `use_custom_command =
    /// false` clears `cfg.shell` at parse-finalize so the
    /// otherwise-defined `command =` / `custom_command =` /
    /// `shell =` value falls back to $SHELL. Order-independent.
    #[test]
    fn terminator_use_custom_command_gate() {
        // command-then-disable.
        let cfg = Config::parse_text("command = /bin/zsh\nuse_custom_command = false\n");
        assert!(cfg.shell.is_none());
        // disable-then-command (reverse order works too).
        let cfg = Config::parse_text("use_custom_command = false\ncommand = /bin/zsh\n");
        assert!(cfg.shell.is_none());
        // Default (use_custom_command implicit-true) keeps the command.
        let cfg = Config::parse_text("command = /bin/zsh\n");
        assert_eq!(cfg.shell.as_deref(), Some("/bin/zsh"));
        // The Terminator `custom_command` spelling is accepted as
        // an alias for `command =`.
        let cfg = Config::parse_text("custom_command = /bin/zsh\n");
        assert_eq!(cfg.shell.as_deref(), Some("/bin/zsh"));
        // copy_on_selection alias maps to copy_on_select.
        let cfg = Config::parse_text("copy_on_selection = false\n");
        assert!(!cfg.copy_on_select);
        // enabled_plugins is recognized (parses without panic);
        // value is intentionally discarded since kettle's plugin
        // model is Lua-based.
        let _ = Config::parse_text("enabled_plugins = LaunchpadBugURLHandler\n");
    }

    /// Drift guard. `palette = NAME` (no `=` after)
    /// is a Terminator named-palette alias that kettle treats as
    /// `theme = NAME`. Underscore-spelled inputs (Terminator
    /// convention) get a `_` → ` ` fallback to match kettle's
    /// bundled theme names.
    #[test]
    fn palette_named_preset_alias() {
        // Direct match (kettle native spelling).
        let cfg = Config::parse_text("palette = TokyoNight Night\n");
        assert_eq!(cfg.theme_name, "TokyoNight Night");
        // Underscore form → bundled name via space fallback.
        let cfg = Config::parse_text("palette = tokyonight_night\n");
        assert_eq!(cfg.theme_name, "TokyoNight Night");
        // The `palette = N=#hex` per-slot form still works for
        // per-slot overrides (no regression).
        let cfg = Config::parse_text("palette = 4=#001122\n");
        assert_eq!(
            cfg.theme.palette[4],
            Rgb {
                r: 0,
                g: 0x11,
                b: 0x22
            }
        );
        // Unknown name leaves theme alone (default).
        let default = Config::default();
        let cfg = Config::parse_text("palette = some_made_up_palette\n");
        assert_eq!(cfg.theme_name, default.theme_name);
    }

    /// Drift guard. `tab-bar-width` config key parses
    /// + clamps to `[40, 600]`. Phase 7 of vertical-tabs design.
    #[test]
    fn tab_bar_width_parses_and_clamps() {
        // Default unchanged.
        assert!((Config::default().tab_bar_width - 180.0).abs() < f32::EPSILON);
        // In-range value applies.
        let cfg = Config::parse_text("tab-bar-width = 240\n");
        assert!((cfg.tab_bar_width - 240.0).abs() < f32::EPSILON);
        // Underscore form works.
        let cfg = Config::parse_text("tab_bar_width = 120\n");
        assert!((cfg.tab_bar_width - 120.0).abs() < f32::EPSILON);
        // Clamps below min.
        let cfg = Config::parse_text("tab-bar-width = 20\n");
        assert!((cfg.tab_bar_width - 40.0).abs() < f32::EPSILON);
        // Clamps above max.
        let cfg = Config::parse_text("tab-bar-width = 2000\n");
        assert!((cfg.tab_bar_width - 600.0).abs() < f32::EPSILON);
        // Garbage value leaves the default.
        let cfg = Config::parse_text("tab-bar-width = wide\n");
        assert!((cfg.tab_bar_width - 180.0).abs() < f32::EPSILON);
    }

    /// Drift guard. `sunrise_sunset_utc_secs` reproduces
    /// the canonical NOAA fixtures within ~5 minutes (the algorithm
    /// is approximate but good enough for a theme-flip).
    #[test]
    fn sunrise_sunset_utc_secs_known_fixtures() {
        use super::sunrise_sunset_utc_secs;
        // San Francisco, summer solstice (June 21 = day 172).
        // NOAA: sunrise 12:48 UTC, sunset 03:35 UTC next day.
        // (Wraps; UTC offset of SF is -7 hours during PDT.)
        let sf_lat = 37.7749;
        let sf_long = -122.4194;
        let (rise, set) = sunrise_sunset_utc_secs(172, sf_lat, sf_long).unwrap();
        let rise_min = rise / 60;
        let set_min = set / 60;
        // Sunrise ~ 12:48 UTC ⇒ ~768 min into the UTC day.
        assert!(
            (rise_min as i32 - 768).abs() < 10,
            "SF June 21 sunrise: expected ~768 min UTC, got {rise_min}"
        );
        // Sunset ~ 03:35 UTC next day ⇒ 215 min in UTC day terms.
        assert!(
            (set_min as i32 - 215).abs() < 30 || (set_min as i32 - (215 + 1440)).abs() < 30,
            "SF June 21 sunset: expected ~03:35 UTC, got {set_min}"
        );
        // Equator on equinox (March 21 = day 80): roughly
        // 06:00 sunrise + 18:00 sunset local. At longitude 0
        // that's 06:00 + 18:00 UTC.
        let (rise, set) = sunrise_sunset_utc_secs(80, 0.0, 0.0).unwrap();
        let rise_min = rise / 60;
        let set_min = set / 60;
        assert!(
            (rise_min as i32 - 360).abs() < 15,
            "equator equinox sunrise: expected ~06:00 UTC, got {rise_min}"
        );
        assert!(
            (set_min as i32 - 1080).abs() < 15,
            "equator equinox sunset: expected ~18:00 UTC, got {set_min}"
        );
        // Polar night/day: lat 80°N on Jan 1 (day 1) returns
        // None (sun never rises).
        assert!(sunrise_sunset_utc_secs(1, 80.0, 0.0).is_none());
        // Same lat on June 21 returns None (sun never sets).
        assert!(sunrise_sunset_utc_secs(172, 80.0, 0.0).is_none());
    }

    /// Drift guard. `schedule_decision_sunrise`
    /// returns the right dark/light decision for fixed times.
    #[test]
    fn schedule_decision_sunrise_walks_windows() {
        use super::schedule_decision_sunrise;
        // SF June 21: rise ≈ 12:48 UTC (768 min), set ≈ 03:35
        // next-day UTC (215 min). Light window wraps midnight.
        let sf_lat = 37.7749;
        let sf_long = -122.4194;
        // Mid-day UTC (12:00): just before sunrise → dark.
        assert!(
            schedule_decision_sunrise(720 * 60, 172, sf_lat, sf_long),
            "12:00 UTC < sunrise → dark"
        );
        // 14:00 UTC: just after sunrise → light.
        assert!(
            !schedule_decision_sunrise(840 * 60, 172, sf_lat, sf_long),
            "14:00 UTC > sunrise → light"
        );
        // 22:00 UTC (afternoon in SF): light.
        assert!(!schedule_decision_sunrise(22 * 3600, 172, sf_lat, sf_long));
        // Polar regions: lat 80°N day 1 (polar night) → dark.
        assert!(schedule_decision_sunrise(12 * 3600, 1, 80.0, 0.0));
        // lat 80°N day 172 (polar day) → light.
        assert!(!schedule_decision_sunrise(12 * 3600, 172, 80.0, 0.0));
    }

    /// Drift guard. `theme-schedule = sunrise/sunset`
    /// parses + the lat/long config keys patch the SunriseSunset
    /// variant at end-of-parse. If lat OR long is missing, the
    /// schedule downgrades to None (both halves required).
    #[test]
    fn theme_schedule_sunrise_sunset_with_lat_long() {
        // Both halves: schedule populated.
        let cfg = Config::parse_text(
            "theme-schedule = sunrise/sunset\n\
             theme-schedule-lat = 37.7749\n\
             theme-schedule-long = -122.4194\n",
        );
        match cfg.theme_schedule {
            Some(ThemeSchedule::SunriseSunset { lat, long }) => {
                assert!((lat - 37.7749).abs() < 1e-6);
                assert!((long - (-122.4194)).abs() < 1e-6);
            }
            other => panic!("expected SunriseSunset; got {other:?}"),
        }
        // Alias spellings accepted.
        let cfg = Config::parse_text(
            "theme-schedule = sunrise-sunset\n\
             theme-schedule-lat = 0\n\
             theme-schedule-long = 0\n",
        );
        assert!(matches!(
            cfg.theme_schedule,
            Some(ThemeSchedule::SunriseSunset { .. })
        ));
        let cfg = Config::parse_text(
            "theme-schedule = solar\n\
             theme-schedule-lat = 10\n\
             theme-schedule-long = 20\n",
        );
        assert!(matches!(
            cfg.theme_schedule,
            Some(ThemeSchedule::SunriseSunset { .. })
        ));
        // Underscore-spelled lat/long keys.
        let cfg = Config::parse_text(
            "theme_schedule = sunrise/sunset\n\
             theme_schedule_lat = 51.5\n\
             theme_schedule_long = -0.1\n",
        );
        assert!(matches!(
            cfg.theme_schedule,
            Some(ThemeSchedule::SunriseSunset { .. })
        ));
        // longitude alias.
        let cfg = Config::parse_text(
            "theme-schedule = sunrise/sunset\n\
             theme-schedule-lat = 0\n\
             theme-schedule-lon = 0\n",
        );
        assert!(matches!(
            cfg.theme_schedule,
            Some(ThemeSchedule::SunriseSunset { .. })
        ));
        // Missing lat → downgrade to None.
        let cfg = Config::parse_text(
            "theme-schedule = sunrise/sunset\n\
             theme-schedule-long = 0\n",
        );
        assert!(cfg.theme_schedule.is_none());
        // Missing long → downgrade to None.
        let cfg = Config::parse_text(
            "theme-schedule = sunrise/sunset\n\
             theme-schedule-lat = 0\n",
        );
        assert!(cfg.theme_schedule.is_none());
        // Out-of-range lat ignored (parses as None on that key).
        let cfg = Config::parse_text(
            "theme-schedule = sunrise/sunset\n\
             theme-schedule-lat = 91\n\
             theme-schedule-long = 0\n",
        );
        assert!(cfg.theme_schedule.is_none(), "lat > 90 is invalid");
        let cfg = Config::parse_text(
            "theme-schedule = sunrise/sunset\n\
             theme-schedule-lat = 0\n\
             theme-schedule-long = 181\n",
        );
        assert!(cfg.theme_schedule.is_none(), "long > 180 is invalid");
    }

    /// Drift guard. `parse_theme_schedule` accepts
    /// the `HH:MM dark, HH:MM light` config-value shape with
    /// either tag-order. Phase 4 of auto-theme design.
    #[test]
    fn parse_theme_schedule_walks_input_shapes() {
        use super::parse_theme_schedule;
        // Canonical: dark first, then light.
        let s = parse_theme_schedule("18:00 dark, 06:00 light").unwrap();
        assert_eq!(
            s,
            ThemeSchedule::Clock {
                dark_at: (18, 0),
                light_at: (6, 0),
            }
        );
        // Swapped order — same result.
        let s = parse_theme_schedule("06:00 light, 18:00 dark").unwrap();
        assert_eq!(
            s,
            ThemeSchedule::Clock {
                dark_at: (18, 0),
                light_at: (6, 0),
            }
        );
        // Whitespace flexible.
        let s = parse_theme_schedule("  18:00  dark  ,  06:00 light  ").unwrap();
        assert_eq!(
            s,
            ThemeSchedule::Clock {
                dark_at: (18, 0),
                light_at: (6, 0),
            }
        );
        // Failures → None.
        assert!(
            parse_theme_schedule("18:00 dark 06:00 light").is_none(),
            "missing comma"
        );
        assert!(
            parse_theme_schedule("18:00 dark").is_none(),
            "only one entry"
        );
        assert!(
            parse_theme_schedule("18:00 dark, 06:00 dark").is_none(),
            "duplicate tag"
        );
        assert!(
            parse_theme_schedule("24:00 dark, 06:00 light").is_none(),
            "hour > 23"
        );
        assert!(
            parse_theme_schedule("18:60 dark, 06:00 light").is_none(),
            "minute > 59"
        );
        assert!(
            parse_theme_schedule("18 dark, 06:00 light").is_none(),
            "missing :"
        );
        assert!(
            parse_theme_schedule("18:00 weird, 06:00 light").is_none(),
            "bad tag"
        );
        assert!(parse_theme_schedule("").is_none(), "empty");
    }

    /// Drift guard. `schedule_decision_clock` returns
    /// the right dark/light boolean given a `(now, schedule)` pair.
    #[test]
    fn schedule_decision_clock_walks_boundaries() {
        use super::schedule_decision_clock;
        let normal = ThemeSchedule::Clock {
            dark_at: (18, 0),
            light_at: (6, 0),
        };
        // Wraps past midnight: dark in [18:00, 24:00) ∪ [00:00, 06:00).
        assert!(
            schedule_decision_clock((18, 0), normal),
            "18:00 → dark (start)"
        );
        assert!(schedule_decision_clock((23, 59), normal), "23:59 → dark");
        assert!(
            schedule_decision_clock((0, 0), normal),
            "00:00 → dark (across midnight)"
        );
        assert!(
            schedule_decision_clock((5, 59), normal),
            "05:59 → dark (just before light)"
        );
        assert!(
            !schedule_decision_clock((6, 0), normal),
            "06:00 → light (boundary)"
        );
        assert!(
            !schedule_decision_clock((12, 0), normal),
            "12:00 → light (middle of day)"
        );
        assert!(
            !schedule_decision_clock((17, 59), normal),
            "17:59 → light (just before dark)"
        );
        // Same-day window: dark in [dark, light) when dark < light.
        let day = ThemeSchedule::Clock {
            dark_at: (10, 0),
            light_at: (14, 0),
        };
        assert!(
            !schedule_decision_clock((9, 0), day),
            "09:00 → light (before dark window)"
        );
        assert!(
            schedule_decision_clock((10, 0), day),
            "10:00 → dark (window start)"
        );
        assert!(schedule_decision_clock((13, 59), day), "13:59 → dark");
        assert!(
            !schedule_decision_clock((14, 0), day),
            "14:00 → light (window end)"
        );
        assert!(
            !schedule_decision_clock((20, 0), day),
            "20:00 → light (after window)"
        );
        // Degenerate: dark == light → defaults to light.
        let degen = ThemeSchedule::Clock {
            dark_at: (12, 0),
            light_at: (12, 0),
        };
        assert!(!schedule_decision_clock((12, 0), degen));
        assert!(!schedule_decision_clock((0, 0), degen));
    }

    /// Drift guard. `resolve_theme_for_mode` is the
    /// pure helper that picks the next theme given the mode +
    /// configured names + OS preference. Phase 2 of the
    /// auto-theme design.
    #[test]
    fn resolve_theme_for_mode_matrix() {
        use ThemeMode::*;
        // Explicit: always None.
        assert_eq!(
            resolve_theme_for_mode(Explicit, "x", "L", "D", Some(true)),
            None
        );
        assert_eq!(resolve_theme_for_mode(Explicit, "x", "L", "D", None), None);
        // Light: target = L when non-empty + not already current.
        assert_eq!(
            resolve_theme_for_mode(Light, "current", "L", "D", None).as_deref(),
            Some("L")
        );
        assert_eq!(
            resolve_theme_for_mode(Light, "L", "L", "D", None),
            None,
            "already on light theme → no change"
        );
        assert_eq!(
            resolve_theme_for_mode(Light, "x", "", "D", None),
            None,
            "light unset → no change"
        );
        // Dark: target = D when non-empty + not already current.
        assert_eq!(
            resolve_theme_for_mode(Dark, "current", "L", "D", None).as_deref(),
            Some("D")
        );
        assert_eq!(resolve_theme_for_mode(Dark, "D", "L", "D", None), None);
        // Auto with Some(true): dark side.
        assert_eq!(
            resolve_theme_for_mode(Auto, "x", "L", "D", Some(true)).as_deref(),
            Some("D")
        );
        // Auto with Some(false): light side.
        assert_eq!(
            resolve_theme_for_mode(Auto, "x", "L", "D", Some(false)).as_deref(),
            Some("L")
        );
        // Auto with None: no decision.
        assert_eq!(resolve_theme_for_mode(Auto, "x", "L", "D", None), None);
        // Case-insensitive "already current" comparison.
        assert_eq!(
            resolve_theme_for_mode(Light, "tokyonight day", "TokyoNight Day", "D", None),
            None,
            "current matches light (case-insensitive) → no change"
        );
    }

    /// Drift guard. `theme-mode` config parsing (phase
    /// 1 of [`TERMINATOR-AUTO-THEME-DESIGN.md`](
    /// ../../../docs/TERMINATOR-AUTO-THEME-DESIGN.md)). Default
    /// stays `Explicit`, matching prior behavior; the 3 Terminator
    /// modes parse cleanly; aliases for `Auto` accommodate user
    /// muscle memory.
    #[test]
    fn theme_mode_parses_terminator_values() {
        use ThemeMode::*;
        assert_eq!(Config::default().theme_mode, Explicit);
        for (input, want) in [
            ("theme-mode = explicit", Explicit),
            ("theme-mode = light", Light),
            ("theme-mode = dark", Dark),
            ("theme-mode = auto", Auto),
            ("theme-mode = system", Auto),
            ("theme-mode = follow-system", Auto),
            ("theme-mode = follow_system", Auto),
            // Underscore key.
            ("theme_mode = dark", Dark),
            ("theme_mode = AUTO", Auto),
            // Unknown value → default Explicit.
            ("theme-mode = garbage", Explicit),
        ] {
            let cfg = Config::parse_text(&format!("{input}\n"));
            assert_eq!(
                cfg.theme_mode, want,
                "input {input:?} should produce {want:?}"
            );
        }
    }

    /// Drift guard. `AskBeforeClosing::should_prompt` is
    /// the pure decision behind the confirm-dialog primitive
    /// (phase 1 of [`TERMINATOR-CONFIRM-DIALOG-DESIGN.md`](
    /// ../../../docs/TERMINATOR-CONFIRM-DIALOG-DESIGN.md)).
    /// Cover all 3 modes × edge-case scope counts (0, 1, 2, large).
    #[test]
    fn ask_before_closing_should_prompt_matrix() {
        use AskBeforeClosing::*;
        // Never: never prompts, regardless of scope.
        assert!(!Never.should_prompt(0));
        assert!(!Never.should_prompt(1));
        assert!(!Never.should_prompt(2));
        assert!(!Never.should_prompt(100));
        // Always: always prompts.
        assert!(Always.should_prompt(0));
        assert!(Always.should_prompt(1));
        assert!(Always.should_prompt(2));
        assert!(Always.should_prompt(100));
        // MultipleTerminals: prompts iff scope > 1.
        assert!(!MultipleTerminals.should_prompt(0));
        assert!(!MultipleTerminals.should_prompt(1));
        assert!(MultipleTerminals.should_prompt(2));
        assert!(MultipleTerminals.should_prompt(100));
    }

    /// Drift guard. Terminator's `tab_position` is kettle's
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

    /// Drift guard. Terminator's `audible_bell` doesn't
    /// map to anything kettle ships (no audio surface yet), so the
    /// parser accepts the key without setting anything. The drift
    /// guard locks in two outcomes:
    ///   - the key is recognized (no unknown-key warning would
    ///     appear in the `detect_malformed_values` diagnostic surface), and
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
        // The `detect_malformed_values` unknown-key surface should NOT flag this
        // key. (We test by asking detect_malformed_values for the
        // diagnostic list — `audible-bell` should not appear.)
        let bad = Config::detect_malformed_values("audible-bell = true\n");
        assert!(
            !bad.iter().any(|m| m.contains("audible-bell")),
            "audible-bell shouldn't trip --check-config (got: {bad:?})"
        );
    }

    /// Drift guard. Terminator-spelling aliases for kettle's
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

    /// Drift guard. `parse_trigger_with_command` is the
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

    /// Drift guard. Terminator splits the bell into two
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
        // Precedence rule: an explicit canonical
        // `bell = <mode>` wins over Terminator-spelled compat
        // aliases REGARDLESS of file order. Mixing both spellings
        // is the rare hybrid-config case; the canonical key takes
        // precedence so the user gets the explicit kettle mode.
        let cfg = Config::parse_text("bell = visual\nurgent-bell = true\n");
        assert_eq!(cfg.bell, BellMode::Visual);
        let cfg = Config::parse_text("visible-bell = true\nbell = attention\n");
        assert_eq!(cfg.bell, BellMode::Attention);
        // force-no-bell still overrides everything.
        let cfg = Config::parse_text(
            "visible-bell = true\n\
             urgent-bell = true\n\
             force-no-bell = true\n",
        );
        assert_eq!(cfg.bell, BellMode::Off);
    }

    /// Drift guard. The `BellMode::compose` helper is the
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

    /// Drift guard. `profile_name_from_path` is the inverse of
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

    /// Drift guard for the --check-config malformed-value
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
        // Drift guard. A malformed regex pattern like
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
        // bearing — docs/CONFIG.md explicitly tells users to
        // write `(BUILD SUCCESSFUL|FAILED)`).
        let ok = Config::detect_malformed_values("trigger = (BUILD SUCCESSFUL|FAILED)\n");
        assert!(
            ok.iter().all(|b| !b.contains("trigger")),
            "valid alternation flagged as malformed: {ok:?}"
        );
    }

    // Drift guards for `persist_config_toggle`.

    fn tempdir_for(test_name: &str) -> std::path::PathBuf {
        #[cfg(windows)]
        let scratch = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
        #[cfg(not(windows))]
        let scratch = std::env::temp_dir();
        let p = scratch.join(format!(
            "kettle-cfg-{}-{}-{}",
            test_name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).expect("mkdir tmp");
        p
    }

    /// `append_keybind` appends a repeatable `keybind` line, the
    /// written line parses back to the intended binding, and re-binding the
    /// SAME trigger overwrites rather than stacking. Backs the interactive
    /// keybind editor's persistence.
    #[test]
    fn append_keybind_persists_and_parses_back() {
        let dir = tempdir_for("keybind");
        let path = dir.join("config");
        std::fs::write(&path, "font-size = 14\n").unwrap();
        // First bind.
        super::append_keybind(&path, "Ctrl+Alt+R", "split_right").expect("append");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("keybind = Ctrl+Alt+R=split_right"),
            "got: {text:?}"
        );
        assert!(text.contains("font-size = 14"), "must preserve other keys");
        // The whole config parses and the binding takes effect.
        let cfg = super::Config::parse_text(&text);
        let trig = super::keybinds::parse_trigger("Ctrl+Alt+R").unwrap();
        assert_eq!(cfg.keybinds.get(&trig), Some(&super::Action::SplitRight));
        // Re-binding the SAME trigger overwrites (no duplicate keybind lines).
        super::append_keybind(&path, "Ctrl+Alt+R", "new_tab").expect("rebind");
        let text2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text2.matches("Ctrl+Alt+R=").count(),
            1,
            "re-binding the same chord must not stack lines: {text2:?}"
        );
        assert!(text2.contains("keybind = Ctrl+Alt+R=new_tab"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_keybind_refuses_non_utf8_existing_config() {
        let dir = tempdir_for("keybind-non-utf8");
        let path = dir.join("config");
        std::fs::write(&path, [0xff, 0xfe, b'k', b'e', b'y']).unwrap();
        let err = super::append_keybind(&path, "Ctrl+Alt+R", "split_right")
            .expect_err("non-UTF-8 config must not be overwritten");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [0xff, 0xfe, b'k', b'e', b'y']
        );
        assert!(
            !path.with_extension("bak").exists(),
            "must not create an empty backup for unreadable existing bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `append_keybind` de-dups SEMANTICALLY — re-binding the
    /// same chord written in a different case (or a literal `=` chord) overwrites
    /// the old line instead of stacking a stale duplicate. The old first-`=`
    /// string compare missed these.
    #[test]
    fn append_keybind_dedupes_by_semantic_trigger() {
        let dir = tempdir_for("keybind-sem");
        let path = dir.join("config");
        // Existing line uses lower-case; the rebind uses canonical case —
        // different STRING, same CHORD.
        std::fs::write(&path, "keybind = ctrl+alt+r=split_right\n").unwrap();
        super::append_keybind(&path, "Ctrl+Alt+R", "new_tab").expect("rebind");
        let text = std::fs::read_to_string(&path).unwrap();
        let trig = super::keybinds::parse_trigger("Ctrl+Alt+R").unwrap();
        // Exactly one keybind line resolves to this chord (the old case-variant
        // line was de-duped, not stacked).
        let count = text
            .lines()
            .filter(|l| {
                l.trim_start().starts_with("keybind")
                    && l.split_once('=')
                        .and_then(|(_, v)| v.rsplit_once('='))
                        .and_then(|(t, _)| super::keybinds::parse_trigger(t.trim()))
                        .as_ref()
                        == Some(&trig)
            })
            .count();
        assert_eq!(
            count, 1,
            "case-variant chord must de-dup, not stack: {text:?}"
        );
        let cfg = super::Config::parse_text(&text);
        assert_eq!(
            cfg.keybinds.get(&trig),
            super::Action::from_name("new_tab").as_ref()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writing a new key into an empty config appends it.
    #[test]
    fn persist_config_toggle_appends_on_missing_key() {
        let dir = tempdir_for("append");
        let path = dir.join("config");
        let bak =
            super::persist_config_toggle(&path, "cursor-blink", "false").expect("persist on empty");
        let text = std::fs::read_to_string(&path).expect("read back");
        assert!(text.contains("cursor-blink = false"), "got: {text:?}");
        // First write creates the backup of the (empty) original.
        assert!(bak.exists());
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_config_toggle_refuses_non_utf8_existing_config() {
        let dir = tempdir_for("toggle-non-utf8");
        let path = dir.join("config");
        std::fs::write(&path, [0xff, 0xfe, b'f', b'o', b'o']).unwrap();
        let err = super::persist_config_toggle(&path, "cursor-blink", "false")
            .expect_err("non-UTF-8 config must not be overwritten");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            [0xff, 0xfe, b'f', b'o', b'o']
        );
        assert!(
            !path.with_extension("bak").exists(),
            "must not create an empty backup for unreadable existing bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Writing an existing key replaces only that line —
    /// every comment, blank, and other key survives byte-for-byte.
    #[test]
    fn persist_config_toggle_preserves_user_comments_and_blank_lines() {
        let dir = tempdir_for("preserve");
        let path = dir.join("config");
        let original = "# user's pristine config\n\
                        font-size = 14\n\
                        \n\
                        # cursor preferences\n\
                        cursor-blink = true\n\
                        cursor-style = beam\n\
                        \n\
                        # theme\n\
                        theme = TokyoNight Night\n";
        std::fs::write(&path, original).expect("seed");
        super::persist_config_toggle(&path, "cursor-blink", "false").expect("persist");
        let got = std::fs::read_to_string(&path).expect("read back");
        // Targeted replacement: cursor-blink line is changed, others
        // (including the inline `# theme` comment + the blank lines)
        // are byte-for-byte identical.
        let expected = "# user's pristine config\n\
                        font-size = 14\n\
                        \n\
                        # cursor preferences\n\
                        cursor-blink = false\n\
                        cursor-style = beam\n\
                        \n\
                        # theme\n\
                        theme = TokyoNight Night\n";
        assert_eq!(got, expected);
        // First-write backup holds the pre-edit content.
        let bak = path.with_extension("bak");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), original);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A second write doesn't re-overwrite the .bak so the
    /// pre-toggle-session content stays forensically intact.
    #[test]
    fn persist_config_toggle_backup_only_on_first_write() {
        let dir = tempdir_for("bak-once");
        let path = dir.join("config");
        std::fs::write(&path, "cursor-blink = true\n").expect("seed");
        super::persist_config_toggle(&path, "cursor-blink", "false").expect("first");
        let bak = path.with_extension("bak");
        // The backup snapshot is the original.
        let snapshot = std::fs::read_to_string(&bak).expect("read .bak");
        assert_eq!(snapshot, "cursor-blink = true\n");
        // Second write: backup must NOT change to the post-first-
        // write state.
        super::persist_config_toggle(&path, "cursor-blink", "true").expect("second");
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), snapshot);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Contract point 5 — a write that would introduce a
    /// malformed value is rejected and the previous content restored, instead
    /// of leaving a corrupted config. The doc promised this from the start but
    /// it was never implemented.
    #[test]
    fn persist_config_toggle_rolls_back_on_malformed_value() {
        let dir = tempdir_for("rollback");
        let path = dir.join("config");
        std::fs::write(&path, "cursor-blink = true\n").expect("seed");
        // `cursor-style = wibble` is a malformed enum value (detect flags it).
        let err = super::persist_config_toggle(&path, "cursor-style", "wibble")
            .expect_err("malformed value must be rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        // The file is restored to its exact pre-edit content — the bad value
        // never lands.
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "cursor-blink = true\n"
        );
        // A subsequent VALID write still succeeds and persists.
        super::persist_config_toggle(&path, "cursor-style", "bar").expect("valid write");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("cursor-style = bar")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_config_toggle_rejects_a_new_malformed_value_when_count_is_unchanged() {
        let dir = tempdir_for("malformed-count");
        let path = dir.join("config");
        let original = "cursor-style = already-invalid\nfont-size = 14\n";
        std::fs::write(&path, original).expect("seed");

        let error = super::persist_config_toggle(&path, "cursor-style", "still-invalid")
            .expect_err("a pre-existing diagnostic must not mask the replacement");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!path.with_extension("bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Key normalization treats `cursor-blink` /
    /// `cursor_blink` / `Cursor-Blink` as the same line so a user
    /// who hand-edited with underscores doesn't get a duplicate
    /// when the menu toggle uses hyphens.
    #[test]
    fn persist_config_toggle_treats_dash_and_underscore_as_equivalent() {
        let dir = tempdir_for("normalize");
        let path = dir.join("config");
        std::fs::write(&path, "cursor_blink = true\n").expect("seed");
        super::persist_config_toggle(&path, "cursor-blink", "false").expect("persist");
        let got = std::fs::read_to_string(&path).expect("read");
        // Exactly one line; rewritten in the form requested.
        let lines: Vec<&str> = got.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 1, "got multiple lines: {got:?}");
        assert_eq!(lines[0], "cursor-blink = false");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drift guard. A config that already has duplicate lines
    /// for the same key must collapse to exactly ONE line after a toggle —
    /// previously every match was rewritten, so duplicates accumulated
    /// (file bloat). The parser is last-wins, so behavior was always
    /// correct; this pins the on-disk de-duplication.
    #[test]
    fn persist_config_toggle_collapses_duplicate_keys_to_one() {
        let dir = tempdir_for("dedup");
        let path = dir.join("config");
        // Seed with duplicates (dash + underscore forms both match) around
        // an unrelated line that must survive untouched.
        std::fs::write(
            &path,
            "cursor-blink = true\nfont-size = 14\ncursor_blink = false\n",
        )
        .expect("seed");
        super::persist_config_toggle(&path, "cursor-blink", "false").expect("persist");
        let got = std::fs::read_to_string(&path).expect("read");
        let lines: Vec<&str> = got.lines().filter(|l| !l.trim().is_empty()).collect();
        // Exactly one cursor-blink line remains, with the new value.
        let cb: Vec<&&str> = lines
            .iter()
            .filter(|l| {
                let k = l.split('=').next().unwrap_or("").trim();
                k == "cursor-blink" || k == "cursor_blink"
            })
            .collect();
        assert_eq!(cb.len(), 1, "duplicates not collapsed: {got:?}");
        assert_eq!(*cb[0], "cursor-blink = false");
        // The unrelated key is preserved.
        assert!(
            lines.contains(&"font-size = 14"),
            "unrelated key dropped: {got:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Paths containing `..` are refused. The Preferences
    /// menu would never construct such a path, but a hostile or
    /// scripted call must not be able to escape the config dir.
    #[test]
    fn persist_config_toggle_refuses_traversal_paths() {
        let dir = tempdir_for("traversal");
        let bad = dir.join("..").join("hostile");
        let err =
            super::persist_config_toggle(&bad, "x", "y").expect_err("traversal should be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_config_toggle_preserves_utf16_encoding_and_exact_backup() {
        let dir = tempdir_for("toggle-utf16");
        let path = dir.join("config");
        let original = "# PowerShell 5.1\r\ncursor-blink = true\r\n";
        let mut bytes = vec![0xff, 0xfe];
        for unit in original.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        std::fs::write(&path, &bytes).unwrap();

        let backup = super::persist_config_toggle(&path, "cursor-blink", "false").unwrap();
        assert_eq!(std::fs::read(backup).unwrap(), bytes);
        let written = std::fs::read(&path).unwrap();
        assert!(written.starts_with(&[0xff, 0xfe]));
        let decoded = super::ConfigDocument::decode(written).unwrap();
        assert!(decoded.text.contains("cursor-blink = false"));
        assert!(!decoded.text.contains("cursor-blink = true"));
        assert_eq!(decoded.text, "# PowerShell 5.1\r\ncursor-blink = false\r\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn persist_config_toggle_preserves_existing_config_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempdir_for("toggle-mode");
        let path = dir.join("config");
        std::fs::write(&path, "cursor-blink = true\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        super::persist_config_toggle(&path, "cursor-blink", "false").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_editors_preserve_crlf_in_untouched_lines() {
        let dir = tempdir_for("toggle-crlf");
        let path = dir.join("config");
        std::fs::write(
            &path,
            b"# keep me byte-for-byte\r\ncursor-blink = true\r\nfont-size = 14\r\n",
        )
        .unwrap();

        super::persist_config_toggle(&path, "cursor-blink", "false").unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"# keep me byte-for-byte\r\ncursor-blink = false\r\nfont-size = 14\r\n"
        );

        super::append_keybind(&path, "Ctrl+Alt+R", "new_tab").unwrap();
        let written = std::fs::read(&path).unwrap();
        assert_eq!(
            written,
            b"# keep me byte-for-byte\r\ncursor-blink = false\r\nfont-size = 14\r\n\r\nkeybind = Ctrl+Alt+R=new_tab\r\n"
        );
        assert!(!written.windows(2).any(|pair| pair == b"\n\n"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn persist_config_toggle_updates_symlink_target_without_replacing_link() {
        use std::os::unix::fs::symlink;
        let dir = tempdir_for("toggle-symlink");
        let target = dir.join("managed-config");
        let path = dir.join("config");
        std::fs::write(&target, "cursor-blink = true\n").unwrap();
        symlink(&target, &path).unwrap();

        let backup = super::persist_config_toggle(&path, "cursor-blink", "false").unwrap();
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "cursor-blink = false\n"
        );
        assert_eq!(
            backup.canonicalize().unwrap(),
            target.with_extension("bak").canonicalize().unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(backup).unwrap(),
            "cursor-blink = true\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_edit_refuses_to_overwrite_an_external_change() {
        let dir = tempdir_for("toggle-external-change");
        let path = dir.join("config");
        std::fs::write(&path, "cursor-blink = true\n").unwrap();
        let edit = super::ConfigEdit::begin(&path).unwrap();
        std::fs::write(&path, "# changed elsewhere\ncursor-blink = true\n").unwrap();
        let error = edit.commit("cursor-blink = false\n").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# changed elsewhere\ncursor-blink = true\n"
        );
        assert!(!path.with_extension("bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_document_rejects_oversized_and_non_regular_files() {
        let dir = tempdir_for("bounded-config-read");
        let oversized = dir.join("oversized-config");
        std::fs::File::create(&oversized)
            .unwrap()
            .set_len(super::MAX_CONFIG_BYTES + 1)
            .unwrap();
        let error = super::ConfigDocument::read(&oversized)
            .err()
            .expect("an oversized config must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);

        let non_regular = dir.join("config-directory");
        std::fs::create_dir(&non_regular).unwrap();
        let error = super::ConfigDocument::read(&non_regular)
            .err()
            .expect("a config directory must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_edit_refuses_an_oversized_external_replacement() {
        let dir = tempdir_for("toggle-oversized-external-change");
        let path = dir.join("config");
        std::fs::write(&path, "cursor-blink = true\n").unwrap();
        let edit = super::ConfigEdit::begin(&path).unwrap();
        std::fs::File::create(&path)
            .unwrap()
            .set_len(super::MAX_CONFIG_BYTES + 1)
            .unwrap();

        let error = edit.commit("cursor-blink = false\n").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            super::MAX_CONFIG_BYTES + 1
        );
        assert!(!path.with_extension("bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn config_reads_reject_symlinks_and_external_symlink_replacements() {
        use std::os::unix::fs::symlink;

        let dir = tempdir_for("toggle-symlink-swap");
        let target = dir.join("target-config");
        let link = dir.join("linked-config");
        std::fs::write(&target, "cursor-blink = true\n").unwrap();
        symlink(&target, &link).unwrap();
        let error = super::ConfigDocument::read(&link)
            .err()
            .expect("the bounded reader must not follow a symlink");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

        let path = dir.join("config");
        std::fs::write(&path, "font-size = 14\n").unwrap();
        let edit = super::ConfigEdit::begin(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(&target, &path).unwrap();
        let error = edit.commit("font-size = 15\n").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "cursor-blink = true\n"
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!path.with_extension("bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Regression guard: a dotfile-manager setup where the config file is a
    /// symlink into a tracked repo must still load on startup. The low-level
    /// reader is `O_NOFOLLOW`, so `load_from_with_diagnostics` resolves the
    /// link to its regular target first — the read path must agree with the
    /// write path (`ConfigEdit::begin`), which already follows the link.
    #[cfg(unix)]
    #[test]
    fn startup_load_follows_symlinked_config_to_regular_target() {
        use std::os::unix::fs::symlink;

        let dir = tempdir_for("startup-symlink-follow");
        let target = dir.join("real-config");
        let link = dir.join("config");
        std::fs::write(&target, "font-size = 17\n").unwrap();
        symlink(&target, &link).unwrap();

        let (cfg, _unknown, _malformed) = super::Config::load_from_with_diagnostics(&link);
        assert_eq!(
            cfg.font_size, 17.0,
            "a symlinked config must be read through, not silently defaulted"
        );

        // A symlink whose target is a directory (not a regular file) still
        // degrades to defaults rather than erroring the whole load.
        let dir_target = dir.join("a-directory");
        std::fs::create_dir(&dir_target).unwrap();
        let bad_link = dir.join("config-to-dir");
        symlink(&dir_target, &bad_link).unwrap();
        let (defaulted, _, _) = super::Config::load_from_with_diagnostics(&bad_link);
        assert_eq!(defaulted.font_size, Config::default().font_size);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_editors_reject_line_injection() {
        let dir = tempdir_for("toggle-line-injection");
        let path = dir.join("config");
        let error = super::persist_config_toggle(&path, "cursor-blink", "false\ntheme = hostile")
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let error = super::append_keybind(&path, "Ctrl+R\nfont-size = 99", "new_tab").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_keybind_rejects_unknown_action_before_commit() {
        let dir = tempdir_for("keybind-invalid-action");
        let path = dir.join("config");
        std::fs::write(&path, "font-size = 14\n").unwrap();
        let error = super::append_keybind(&path, "Ctrl+Alt+R", "not_a_real_action").unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "font-size = 14\n");
        assert!(!path.with_extension("bak").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod terminator_import_tests {
    use super::*;

    /// A config copied from Terminator must actually import. Every line below
    /// is Terminator's own spelling — underscore keys, GTK accelerators, a
    /// Pango font description, a colon palette, and quoted values as
    /// Terminator's manual itself writes them.
    ///
    /// Each of these was previously dropped: the keys were unrecognised, the
    /// quotes defeated the value parsers, and the `[keybindings]` grammar is
    /// the inverse of kettle's.
    #[test]
    fn a_real_terminator_config_imports() {
        let cfg = Config::parse_text(
            "\
scroll_on_keystroke = False\n\
scroll_on_output = True\n\
background_color = \"#1a1b26\"\n\
foreground_color = '#c0caf5'\n\
font = DejaVu Sans Mono 13\n\
palette = \"#2e3436:#cc0000:#4e9a06:#c4a000:#3465a4:#75507b:#06989a:#d3d7cf\"\n\
new_tab = <Control><Shift>y\n\
split_horiz = <Control><Shift>j\n",
        );

        // Underscore keys reach their hyphenated arms.
        assert!(!cfg.scroll_on_keystroke, "scroll_on_keystroke must apply");
        assert!(cfg.scroll_on_output, "scroll_on_output must apply");

        // Quoted colours survive the quotes.
        assert_eq!(cfg.theme.background, Rgb::parse("#1a1b26").unwrap());
        assert_eq!(cfg.theme.foreground, Rgb::parse("#c0caf5").unwrap());

        // A Pango description carries BOTH family and size.
        assert_eq!(cfg.font_family, "DejaVu Sans Mono");
        assert!(
            (cfg.font_size - 13.0).abs() < f32::EPSILON,
            "font size from the Pango description, got {}",
            cfg.font_size
        );

        // The colon palette lands in order, quotes and all.
        assert_eq!(cfg.theme.palette[0], Rgb::parse("#2e3436").unwrap());
        assert_eq!(cfg.theme.palette[1], Rgb::parse("#cc0000").unwrap());
        assert_eq!(cfg.theme.palette[7], Rgb::parse("#d3d7cf").unwrap());

        // GTK accelerators bind through the bare-action-name grammar.
        //
        // The chords here are deliberately ones kettle does NOT ship a default
        // for. This assertion used to read `<Control><Shift>t` → `NewTab`,
        // which is kettle's own stock binding — so it passed no matter what
        // the importer did, and it went on passing while every line in a
        // `[keybindings]` section was in fact being dropped as an unknown key.
        // Binding a chord that starts out unbound is the only version of this
        // check that can fail when the import breaks.
        let stock = Config::parse_text("");
        for (chord, action) in [
            ("ctrl+shift+y", keybinds::Action::NewTab),
            ("ctrl+shift+j", keybinds::Action::SplitDown),
        ] {
            let trig = keybinds::parse_trigger(chord).expect("trigger parses");
            assert_eq!(
                stock.keybinds.get(&trig),
                None,
                "{chord} must start out unbound or this proves nothing"
            );
            assert_eq!(
                cfg.keybinds.get(&trig).cloned(),
                Some(action),
                "Terminator's `{chord}` line must bind"
            );
        }
    }

    /// A REAL Terminator config is sectioned INI, and each section means
    /// something different.
    ///
    /// The importer flattened it: every line applied regardless of section, so
    /// the LAST profile in the file won and a user's `[[default]]` colours
    /// were silently replaced by whichever other profile happened to be
    /// written last. `[layouts]` internals leaked in as config keys, and
    /// `--check-config` reported every section header as a line missing its
    /// `=` — a wall of errors on a perfectly well-formed file.
    #[test]
    fn a_sectioned_terminator_config_reads_the_default_profile_only() {
        let text = "[global_config]
  focus = system
[keybindings]
  new_tab = <Control><Shift>y
[profiles]
  [[default]]
    background_color = \"#111111\"
    scrollback_lines = 4321
  [[work]]
    background_color = \"#222222\"
[layouts]
  [[default]]
    [[[child1]]]
      type = Terminal
      profile = work
";

        // Section headers are structure, not malformed assignments.
        let bad = Config::detect_malformed_values(text);
        assert!(bad.is_empty(), "a well-formed Terminator file: {bad:?}");

        // `[layouts]` internals are Terminator's own window-tree description
        // and must not surface as unknown kettle keys.
        let (_, unknown) = Config::parse_collect(text);
        assert!(unknown.is_empty(), "layout internals leaked: {unknown:?}");

        let cfg = Config::parse_text(text);
        assert_eq!(
            cfg.theme.background,
            Rgb::parse("#111111").unwrap(),
            "the DEFAULT profile's colour must win — not whichever profile is \
             written last"
        );
        assert_eq!(
            cfg.scrollback, 4321,
            "and its other settings — `scrollback_lines` is Terminator's own \
             key name (config.py:242) and had no arm at all"
        );
        // The global and keybindings sections still apply.
        assert_eq!(
            cfg.keybinds
                .get(&keybinds::parse_trigger("ctrl+shift+y").expect("chord"))
                .cloned(),
            Some(keybinds::Action::NewTab)
        );
    }

    /// A file with no sections at all is kettle's own format and must be
    /// entirely unaffected — the section logic only engages once a header
    /// appears.
    #[test]
    fn a_flat_kettle_config_is_unaffected_by_section_handling() {
        let cfg = Config::parse_text(
            "scrollback = 777
font-size = 15
",
        );
        assert_eq!(cfg.scrollback, 777);
        assert!((cfg.font_size - 15.0).abs() < f32::EPSILON);
    }

    /// A malformed header must not leave the previous section in force.
    ///
    /// A typo'd `[[work]` used to fall through as an ordinary line while the
    /// parser still believed it was inside `[[default]]` — so the work
    /// profile's settings applied as the user's defaults. An unreadable header
    /// means we no longer know where we are, and the safe answer is to apply
    /// nothing until the next header we can read.
    #[test]
    fn a_malformed_header_stops_the_previous_section_from_swallowing_it() {
        let cfg = Config::parse_text(
            "[profiles]\n  [[default]]\n    background_color = \"#111111\"\n\
             \n  [[work]\n    background_color = \"#222222\"\n",
        );
        assert_eq!(
            cfg.theme.background,
            Rgb::parse("#111111").unwrap(),
            "the default profile's colour must stand; the malformed section's \
             lines belong to nothing"
        );
    }

    /// A skipped nesting level must not promote a section.
    ///
    /// `[profiles]` followed by `[[[default]]]` — three deep where two is
    /// meant — used to collapse to the path `profiles/default` and import a
    /// malformed section as the user's default profile.
    #[test]
    fn a_skipped_nesting_level_does_not_become_the_default_profile() {
        let cfg =
            Config::parse_text("[profiles]\n  [[[default]]]\n    background_color = \"#333333\"\n");
        assert_ne!(
            cfg.theme.background,
            Rgb::parse("#333333").unwrap(),
            "a level was skipped, so this is not the default profile"
        );
        // The well-formed depth still works.
        assert_eq!(
            Config::parse_text("[profiles]\n  [[default]]\n    background_color = \"#333333\"\n")
                .theme
                .background,
            Rgb::parse("#333333").unwrap()
        );
    }

    /// `scrollback_infinite` overrides the line count regardless of file order.
    ///
    /// Both simply assigned as they were read, so whichever line came second
    /// won — and Terminator treats the boolean as an override.
    #[test]
    fn infinite_scrollback_does_not_depend_on_line_order() {
        for text in [
            "scrollback_infinite = True\nscrollback_lines = 100\n",
            "scrollback_lines = 100\nscrollback_infinite = True\n",
        ] {
            assert_eq!(
                Config::parse_text(text).scrollback,
                INFINITE_SCROLLBACK,
                "unbounded either way: {text:?}"
            );
        }
        // `False` leaves the given line count alone.
        assert_eq!(
            Config::parse_text("scrollback_infinite = False\nscrollback_lines = 100\n").scrollback,
            100
        );
    }

    /// An empty accelerator is Terminator's "this shortcut is disabled".
    ///
    /// Its shipped defaults contain several, and its preferences UI writes one
    /// when a binding is cleared. Ignoring the line left kettle's own default
    /// chord live, so a config that deliberately freed Ctrl+Shift+T — to hand
    /// it back to tmux, AstroNvim, or an agent CLI — did not free it.
    #[test]
    fn an_empty_terminator_accelerator_unbinds_kettles_default() {
        let stock = keybinds::parse_trigger("ctrl+shift+t").expect("chord");
        assert_eq!(
            Config::parse_text("").keybinds.get(&stock).cloned(),
            Some(keybinds::Action::NewTab)
        );
        let cfg = Config::parse_text("[keybindings]\n  new_tab =\n");
        assert_eq!(
            cfg.keybinds.get(&stock),
            None,
            "an empty accelerator disables the shortcut"
        );
    }

    /// kettle's own grammar binds a literal `<`, and the GTK-accelerator
    /// rewrite must not take it away.
    ///
    /// Rejecting an unclosed `<` broke `keybind = <=copy`, which had bound the
    /// `<` key since before any Terminator support existed.
    #[test]
    fn the_accelerator_rewrite_leaves_a_literal_angle_bracket_alone() {
        let lt = keybinds::parse_trigger("<").expect("`<` is an ordinary key");
        let cfg = Config::parse_text("keybind = <=copy\n");
        assert_eq!(
            cfg.keybinds.get(&lt).cloned(),
            Some(keybinds::Action::Copy),
            "`keybind = <=copy` must still bind the `<` key"
        );
    }

    /// Two distinct Terminator actions must not fight over one kettle action.
    ///
    /// `group_tab` and `group_tab_toggle` are separate operations upstream. Both
    /// resolving to `Action::GroupTab` meant an imported config containing both
    /// had its second line silently unbind the first, because an imported
    /// binding is exclusive per action.
    #[test]
    fn two_terminator_group_names_do_not_unbind_each_other() {
        let cfg = Config::parse_text(
            "[keybindings]\n  group_tab = <Control><Shift>y\n  group_tab_toggle = <Control><Shift>j\n",
        );
        let a = keybinds::parse_trigger("ctrl+shift+y").expect("chord");
        let b = keybinds::parse_trigger("ctrl+shift+j").expect("chord");
        assert_eq!(
            cfg.keybinds.get(&a).cloned(),
            Some(keybinds::Action::GroupTab)
        );
        assert_eq!(
            cfg.keybinds.get(&b).cloned(),
            Some(keybinds::Action::ToggleGroupTab),
            "the toggle is its own action, so both chords survive"
        );
    }

    /// Terminator's `group_all` GROUPS; it does not broadcast.
    ///
    /// `window.py:933` puts every terminal into a group named "All".
    /// Broadcasting to a group is a separate, later choice — so mapping
    /// `group_all` onto a broadcast toggle armed input duplication the user
    /// never asked for: one keypress after importing their config, everything
    /// they typed went to every pane.
    #[test]
    fn terminator_group_actions_group_rather_than_broadcast() {
        use keybinds::Action;
        for (name, expected) in [
            ("group_all", Action::GroupAll),
            ("group_all_toggle", Action::ToggleGroupAll),
            ("ungroup_all", Action::UngroupAll),
            // The broadcast names stay broadcast names.
            ("broadcast_all", Action::ToggleBroadcastWindow),
            ("broadcast_off", Action::ToggleBroadcastOff),
        ] {
            assert_eq!(
                keybinds::Action::from_name(name),
                Some(expected),
                "{name} must map to the action Terminator actually performs"
            );
        }
    }

    /// An imported Terminator binding REPLACES kettle's chord for that action.
    ///
    /// Terminator's grammar is `action = accelerator`: one accelerator per
    /// action. Treating it as additive (kettle's own `keybind =` semantics)
    /// left the stock chord live alongside the imported one — so someone
    /// rebinding `new_tab` precisely BECAUSE Ctrl+Shift+T collides with tmux,
    /// AstroNvim, or an agent CLI found it still captured afterwards. The
    /// rebind looked like it worked and the collision it was meant to resolve
    /// was still there.
    #[test]
    fn an_imported_binding_replaces_kettles_chord_for_that_action() {
        let stock = keybinds::parse_trigger("ctrl+shift+t").expect("chord");
        assert_eq!(
            Config::parse_text("").keybinds.get(&stock).cloned(),
            Some(keybinds::Action::NewTab),
            "Ctrl+Shift+T is kettle's stock NewTab chord"
        );

        let cfg = Config::parse_text(
            "new_tab = <Control><Shift>y
",
        );
        let imported = keybinds::parse_trigger("ctrl+shift+y").expect("chord");
        assert_eq!(
            cfg.keybinds.get(&imported).cloned(),
            Some(keybinds::Action::NewTab),
            "the imported chord binds"
        );
        assert_eq!(
            cfg.keybinds.get(&stock),
            None,
            "and the stock chord it replaces must be GONE — otherwise the \
             collision the user rebound to escape is still there"
        );
        // Only that action is touched.
        assert_eq!(
            cfg.keybinds
                .get(&keybinds::parse_trigger("ctrl+shift+w").expect("chord"))
                .cloned(),
            Config::parse_text("")
                .keybinds
                .get(&keybinds::parse_trigger("ctrl+shift+w").expect("chord"))
                .cloned(),
            "an unrelated action keeps its chord"
        );

        // kettle's OWN grammar stays additive: `keybind =` adds a chord
        // without taking the existing one away.
        let additive = Config::parse_text(
            "keybind = ctrl+shift+y=new_tab
",
        );
        assert_eq!(
            additive.keybinds.get(&imported).cloned(),
            Some(keybinds::Action::NewTab)
        );
        assert_eq!(
            additive.keybinds.get(&stock).cloned(),
            Some(keybinds::Action::NewTab),
            "`keybind =` is additive on purpose — one action may have several \
             chords"
        );
    }

    /// A parameterized action rebinds only its own parameter.
    #[test]
    fn an_imported_binding_for_a_parameterized_action_leaves_its_siblings() {
        let cfg = Config::parse_text(
            "goto_tab:3 = <Control><Shift>F3
",
        );
        let stock_2 = keybinds::parse_trigger("alt+2").expect("chord");
        let imported = keybinds::parse_trigger("ctrl+shift+F3").expect("chord");

        // Assert the import HAPPENED before asserting what it left alone —
        // otherwise ignoring the whole line passes this test.
        assert_eq!(
            cfg.keybinds.get(&imported).cloned(),
            keybinds::Action::from_name("goto_tab:3"),
            "the imported chord must bind tab 3"
        );
        assert_eq!(
            cfg.keybinds.get(&stock_2).cloned(),
            Config::parse_text("").keybinds.get(&stock_2).cloned(),
            "while tab 2's chord is untouched"
        );
    }

    /// A Pango style option is not part of the family name.
    ///
    /// `font = DejaVu Sans Mono Bold 13` asks for the bold FACE of `DejaVu
    /// Sans Mono`. Keeping the whole string requested a family literally named
    /// "DejaVu Sans Mono Bold", which no system has — so the entire font
    /// silently fell back and the user got a different typeface than their
    /// config named. GTK's font chooser writes exactly this shape, so it is
    /// what a copied Terminator config contains.
    #[test]
    fn a_pango_style_option_is_not_part_of_the_family() {
        for (desc, family, size) in [
            ("DejaVu Sans Mono Bold 13", "DejaVu Sans Mono", Some(13.0)),
            ("Monospace 11", "Monospace", Some(11.0)),
            ("Cascadia Code Light Italic 12", "Cascadia Code", Some(12.0)),
            // Hyphenated and spaced spellings of the same option.
            ("Iosevka Ultra-Light 10", "Iosevka", Some(10.0)),
            ("Iosevka Semi Bold 10", "Iosevka", Some(10.0)),
            // No size at all.
            ("Fira Code Bold", "Fira Code", None),
            // Nothing to strip.
            ("JetBrains Mono 14", "JetBrains Mono", Some(14.0)),
        ] {
            let cfg = Config::parse_text(&format!(
                "font = {desc}
"
            ));
            assert_eq!(cfg.font_family, family, "family from {desc:?}");
            if let Some(size) = size {
                assert!(
                    (cfg.font_size - size).abs() < f32::EPSILON,
                    "size from {desc:?}: got {}",
                    cfg.font_size
                );
            }
        }

        // Pango's family LIST is comma-separated as a fallback chain; kettle
        // resolves one family, so the first entry wins and the separator does
        // not ride along into the name.
        assert_eq!(
            Config::parse_text(
                "font = Arial Black, 12
"
            )
            .font_family,
            "Arial Black",
            "a comma-terminated family keeps its style-looking word"
        );
        assert_eq!(
            Config::parse_text(
                "font = Fira Code, DejaVu Sans Mono 12
"
            )
            .font_family,
            "Fira Code",
            "the first family in a fallback list"
        );
        // Stops at the first token that is NOT a style option, so a family
        // whose own name contains one keeps everything up to it.
        assert_eq!(
            Config::parse_text(
                "font = Fira Code Light Retina 12
"
            )
            .font_family,
            "Fira Code Light Retina"
        );
        // And never strips a family down to nothing.
        assert_eq!(
            Config::parse_text(
                "font = Bold 12
"
            )
            .font_family,
            "Bold"
        );
    }

    /// A malformed accelerator must bind NOTHING.
    ///
    /// The normalizer used to drop an empty modifier group, so `<>t` became
    /// the bare key `t` — a typo in a config quietly bound an ordinary letter,
    /// and from then on typing that letter fired the action instead of
    /// reaching the shell. Silently narrowing a chord to a bare key is the
    /// worst possible reading of a malformed line.
    #[test]
    fn a_malformed_accelerator_binds_nothing() {
        for junk in [
            "<>t",          // empty modifier group — used to become plain `t`
            "<Control><>t", // ...including after a real modifier
            "<Control",     // unclosed
            "<Control>",    // modifiers with no key
            "<Control><Shift>",
        ] {
            assert!(
                keybinds::parse_trigger(junk).is_none(),
                "{junk:?} is malformed and must not bind anything"
            );
        }
        // The well-formed neighbours of those still work.
        assert_eq!(
            keybinds::parse_trigger("<Control>t"),
            keybinds::parse_trigger("ctrl+t")
        );
        // GTK's own `Ctl` abbreviation is accepted, since Terminator configs
        // are written by GTK.
        assert_eq!(
            keybinds::parse_trigger("<Ctl><Shift>y"),
            keybinds::parse_trigger("ctrl+shift+y")
        );
    }

    /// The GTK accelerator rewrite must not disturb kettle's own spelling.
    #[test]
    fn kettle_trigger_spelling_is_unchanged_by_the_accelerator_rewrite() {
        assert_eq!(
            keybinds::parse_trigger("ctrl+shift+t"),
            keybinds::parse_trigger("<Control><Shift>t"),
            "both spellings must produce the same trigger"
        );
        assert_eq!(
            keybinds::parse_trigger("alt+1"),
            keybinds::parse_trigger("<Alt>1")
        );
        // Junk still fails rather than silently binding something.
        assert!(keybinds::parse_trigger("<Control>").is_none());
    }

    /// Only a MATCHED pair is stripped, and only the outermost one, so values
    /// that legitimately contain quotes are preserved.
    #[test]
    fn unquoting_is_conservative() {
        let cfg = Config::parse_text("window-title-format = \"a'b\"\n");
        assert_eq!(cfg.window_title_format, "a'b");
        let mismatched = Config::parse_text("window-title-format = \"unterminated\n");
        assert_eq!(mismatched.window_title_format, "\"unterminated");
    }
}

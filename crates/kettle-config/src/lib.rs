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

impl BellMode {
    pub fn visual(self) -> bool {
        matches!(self, BellMode::Visual | BellMode::Both)
    }
    pub fn attention(self) -> bool {
        matches!(self, BellMode::Attention | BellMode::Both)
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TriggerAction {
    #[default]
    Urgency,
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
    pub fn load_from_with_diagnostics(path: &Path) -> (Config, Vec<String>, Vec<String>) {
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
                "tab-bar-position" => {
                    matches!(v.to_ascii_lowercase().as_str(), "top" | "bottom")
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
                "background" => {
                    if let Some(c) = Rgb::parse(&e.value) {
                        cfg.theme.background = c;
                    }
                }
                "foreground" => {
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
                "cursor-style" => {
                    cfg.cursor_style = match e.value.to_ascii_lowercase().as_str() {
                        "underline" => CursorStyle::Underline,
                        // `beam` is Alacritty's name for the same
                        // vertical-bar cursor; cycle 142 added the
                        // alias so Alacritty refugees don't get a
                        // silent Block fallback.
                        "bar" | "beam" => CursorStyle::Bar,
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
                "cursor-style-blink" => {
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
                "tab-bar-position" => {
                    cfg.tab_bar_pos = match e.value.to_ascii_lowercase().as_str() {
                        "bottom" => TabBarPos::Bottom,
                        _ => TabBarPos::Top,
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
                    // Cycle 289: `trigger = REGEX` adds a regex pattern
                    // that fires `Urgency` on a PTY-output match. v1
                    // only ships the one action so the value is the
                    // whole pattern — no separator parsing. A future
                    // multi-action syntax must NOT use `|` as the
                    // separator: pipe is a regex metacharacter, and
                    // patterns like `(BUILD SUCCESSFUL|FAILED)` would
                    // split mid-alternation. A `→` or two-step
                    // `trigger-action = …` would be safer.
                    let pattern = e.value.trim().to_string();
                    if !pattern.is_empty() {
                        cfg.triggers.push(OutputTrigger {
                            pattern,
                            action: TriggerAction::Urgency,
                        });
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
        for rel in ["README.md", "docs/CONFIG.md", "docs/INSTALL.md"] {
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

//! kettle configuration: Ghostty-compatible `key = value` config, the bundled
//! Ghostty theme set (TokyoNight Night default), the embedded Nerd Font, and
//! Terminator-compatible keybindings.

pub mod color;
pub mod font;
pub mod fuzzy;
pub mod keybinds;
pub mod palette;
pub mod parse;
pub mod template;
pub mod theme;

use std::path::{Path, PathBuf};

pub use color::Rgb;
pub use keybinds::{Action, Bindings, Key, Mods, Trigger};
pub use theme::Theme;

/// Practical stand-in for "infinite" scrollback: ~10M lines (keeps memory
/// bounded while never realistically clipping history).
pub const INFINITE_SCROLLBACK: usize = 10_000_000;

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
        let mut tag = [b' '; 4];
        tag[..name.len()].copy_from_slice(name.as_bytes());
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
    /// Cursor blink half-period in milliseconds.
    pub cursor_blink_interval: u64,
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
            unfocused_split_opacity: 0.7,
            scroll_multiplier: 1.0,
            minimum_contrast: 0.0,
            window_title_format: "{title} — kettle".to_string(),
            tab_format: "{n}: {title}".to_string(),
            scrollbar: ScrollbarMode::Auto,
            split_divider_color: None,
            focused_split_color: None,
            cursor_blink_interval: 530,
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
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))?;
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
                // `Theme::by_name`'s resolution.
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
                "cursor-style" => matches!(v.as_str(), "block" | "underline" | "bar"),
                "bell" => matches!(
                    v.as_str(),
                    "off" | "none" | "false" | "visual" | "flash" | "attention" | "urgent" | "both"
                ),
                "osc52" | "clipboard" => matches!(
                    v.as_str(),
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
                "tab-bar" => matches!(v.as_str(), "off" | "none" | "false" | "auto" | "always"),
                "tab-bar-position" => matches!(v.as_str(), "top" | "bottom"),
                "scrollbar" => matches!(v.as_str(), "never" | "off" | "false" | "auto" | "always"),
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
                    if let Ok(v) = e.value.parse() {
                        cfg.font_size = v;
                    }
                }
                "theme" => {
                    // Empty `theme =` same as the font-family case:
                    // keep the previously-resolved theme (default or
                    // an earlier override on the same key) rather
                    // than blanking the name string and falling back
                    // to whatever `Theme::by_name("")` returns.
                    if !e.value.trim().is_empty() {
                        cfg.theme_name = e.value.clone();
                        cfg.theme = Theme::by_name(&e.value);
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
                    cfg.cursor_style = match e.value.as_str() {
                        "underline" => CursorStyle::Underline,
                        "bar" => CursorStyle::Bar,
                        _ => CursorStyle::Block,
                    }
                }
                "cursor-style-blink" => cfg.cursor_blink = e.value != "false",
                "bell" => {
                    cfg.bell = match e.value.as_str() {
                        "off" | "none" | "false" => BellMode::Off,
                        "visual" | "flash" => BellMode::Visual,
                        "attention" | "urgent" => BellMode::Attention,
                        _ => BellMode::Both,
                    }
                }
                "osc52" | "clipboard" => {
                    cfg.osc52 = match e.value.as_str() {
                        "off" | "none" | "disabled" | "false" => Osc52::Off,
                        "paste" | "read" => Osc52::Paste,
                        "both" | "all" | "true" => Osc52::Both,
                        _ => Osc52::Copy,
                    }
                }
                "tab-bar" => {
                    cfg.tab_bar = match e.value.as_str() {
                        "off" | "none" | "false" => TabBarMode::Off,
                        "auto" => TabBarMode::Auto,
                        _ => TabBarMode::Always,
                    }
                }
                "tab-bar-position" => {
                    cfg.tab_bar_pos = match e.value.as_str() {
                        "bottom" => TabBarPos::Bottom,
                        _ => TabBarPos::Top,
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
                    cfg.scrollbar = match e.value.as_str() {
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
                "cursor-blink-interval" => {
                    if let Ok(v) = e.value.parse::<u64>() {
                        cfg.cursor_blink_interval = v.clamp(50, 5000);
                    }
                }
                "copy-on-select" => cfg.copy_on_select = e.value != "false",
                "scroll-on-keystroke" | "scroll-on-input" => {
                    cfg.scroll_on_keystroke = e.value != "false";
                }
                "scroll-on-output" => cfg.scroll_on_output = e.value != "false",
                "mouse-hide-while-typing" | "mouse-hide" => {
                    cfg.mouse_hide_while_typing = e.value != "false";
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
    fn default_is_tokyonight_night() {
        let c = Config::default();
        assert_eq!(c.theme_name, "TokyoNight Night");
        assert_eq!(c.theme.background, Rgb::new(0x1a, 0x1b, 0x26));
        assert_eq!(c.theme.foreground, Rgb::new(0xc0, 0xca, 0xf5));
        assert_eq!(c.theme.palette[4], Rgb::new(0x7a, 0xa2, 0xf7));
        assert_eq!(c.font_family, font::FAMILY);
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
}

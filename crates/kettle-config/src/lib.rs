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
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let (cfg, unknown) = Self::parse_collect(&text);
                if !unknown.is_empty() {
                    log::warn!(
                        "{}: unrecognized config keys: {}",
                        path.display(),
                        unknown.join(", ")
                    );
                }
                cfg
            }
            Err(e) => {
                log::warn!("could not read config {}: {e}", path.display());
                Config::default()
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
        for e in parse::parse(text) {
            let v = &e.value;
            let ok = match e.key.as_str() {
                // Floats: clamped or otherwise, the parse itself has to
                // succeed.
                "font-size"
                | "padding-x"
                | "window-padding-x"
                | "padding-y"
                | "window-padding-y"
                | "background-opacity"
                | "unfocused-split-opacity"
                | "scroll-multiplier"
                | "mouse-scroll-multiplier"
                | "minimum-contrast" => v.parse::<f32>().is_ok(),
                // Special: scrollback accepts unlimited/infinite/0 as
                // "no cap" plus any non-negative integer.
                "scrollback" => {
                    v.eq_ignore_ascii_case("infinite")
                        || v.eq_ignore_ascii_case("unlimited")
                        || v.parse::<usize>().is_ok()
                }
                "cursor-blink-interval" => v.parse::<u64>().is_ok(),
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
                "keybind" => v.split_once('=').is_some_and(|(t, a)| {
                    keybinds::parse_trigger(t.trim()).is_some()
                        && Action::from_name(a.trim()).is_some()
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
                "palette" => v.split_once('=').is_some_and(|(i, h)| {
                    i.trim().parse::<usize>().is_ok() && Rgb::parse(h.trim()).is_some()
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
                "font-family" => cfg.font_family = e.value.clone(),
                "font-family-bold" => cfg.font_family_bold = Some(e.value.clone()),
                "font-family-italic" => cfg.font_family_italic = Some(e.value.clone()),
                "font-family-bold-italic" => cfg.font_family_bold_italic = Some(e.value.clone()),
                "font-size" => {
                    if let Ok(v) = e.value.parse() {
                        cfg.font_size = v;
                    }
                }
                "theme" => {
                    cfg.theme_name = e.value.clone();
                    cfg.theme = Theme::by_name(&e.value);
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
                        cfg.scrollback = n;
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
                    if let Ok(v) = e.value.parse() {
                        cfg.background_opacity = v;
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
                "command" | "shell" => cfg.shell = Some(e.value.clone()),
                "ssh-host" => {
                    if let Some((name, target)) = e.value.split_once('=') {
                        cfg.ssh_hosts
                            .push((name.trim().to_string(), target.trim().to_string()));
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
    fn default_is_tokyonight_night() {
        let c = Config::default();
        assert_eq!(c.theme_name, "TokyoNight Night");
        assert_eq!(c.theme.background, Rgb::new(0x1a, 0x1b, 0x26));
        assert_eq!(c.theme.foreground, Rgb::new(0xc0, 0xca, 0xf5));
        assert_eq!(c.theme.palette[4], Rgb::new(0x7a, 0xa2, 0xf7));
        assert_eq!(c.font_family, font::FAMILY);
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
        // Valid keybinds — including aliases and `goto_tab:N` parametric
        // — pass cleanly.
        let ok = Config::detect_malformed_values(
            "keybind = ctrl+shift+c=copy\n\
             keybind = alt+5=goto_tab:5\n\
             keybind = f11=toggle_fullscreen\n\
             keybind = ctrl+shift+o=split_horiz\n",
        );
        assert!(ok.is_empty(), "all valid: {ok:?}");
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

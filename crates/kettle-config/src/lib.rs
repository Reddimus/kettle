//! kettle configuration: Ghostty-compatible `key = value` config, the bundled
//! Ghostty theme set (TokyoNight Night default), the embedded Nerd Font, and
//! Terminator-compatible keybindings.

pub mod color;
pub mod font;
pub mod fuzzy;
pub mod keybinds;
pub mod palette;
pub mod parse;
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
    pub scrollbar: ScrollbarMode,
    /// Explicit split-divider/border color (else theme palette).
    pub split_divider_color: Option<Rgb>,
    /// Cursor blink half-period in milliseconds.
    pub cursor_blink_interval: u64,
    /// Auto-copy the selection to the clipboard on release.
    pub copy_on_select: bool,
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
            scrollbar: ScrollbarMode::Auto,
            split_divider_color: None,
            cursor_blink_interval: 530,
            copy_on_select: true,
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
                "cursor-blink-interval" => {
                    if let Ok(v) = e.value.parse::<u64>() {
                        cfg.cursor_blink_interval = v.clamp(50, 5000);
                    }
                }
                "copy-on-select" => cfg.copy_on_select = e.value != "false",
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
        let c = Config::parse_text(
            "unfocused-split-opacity = 0.5\nscrollbar = always\n\
             split-divider-color = #ff8800\ncursor-blink-interval = 800\n\
             copy-on-select = false\n",
        );
        assert_eq!(c.unfocused_split_opacity, 0.5);
        assert_eq!(c.scrollbar, ScrollbarMode::Always);
        assert_eq!(c.split_divider_color, Some(Rgb::new(0xff, 0x88, 0x00)));
        assert_eq!(c.cursor_blink_interval, 800);
        assert!(!c.copy_on_select);
        // Clamping.
        assert_eq!(
            Config::parse_text("unfocused-split-opacity = 5").unfocused_split_opacity,
            1.0
        );
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

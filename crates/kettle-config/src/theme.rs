//! Theme model + the bundled Ghostty theme set (the iTerm2-Color-Schemes
//! `ghostty/` collection, 500+ themes, embedded at compile time).

use crate::color::Rgb;
use crate::parse;

include!(concat!(env!("OUT_DIR"), "/themes_generated.rs"));

#[derive(Debug, Clone)]
pub struct Theme {
    /// ANSI palette 0..=15.
    pub palette: [Rgb; 16],
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    pub selection_background: Rgb,
    pub selection_foreground: Rgb,
    /// The theme's UI-chrome accent (focus border, active tab,
    /// status bar). Most themes don't declare one and default it to `palette[4]`
    /// (their ANSI blue, the conventional focus color) — including the shipped
    /// default TokyoNight Night, whose blue `#7aa2f7` is also the app icon's
    /// accent. Catppuccin Mocha is the exception: it sets accent to its
    /// *signature* mauve (#cba6f7), a named Catppuccin color NOT in the 16-slot
    /// ANSI palette, so the chrome can't derive it otherwise. A user
    /// `accent-color = …` still overrides.
    pub accent: Rgb,
}

impl Default for Theme {
    fn default() -> Self {
        // The hard, self-contained fallback theme (Catppuccin Mocha palette),
        // returned only when a configured theme name matches no bundled theme.
        // NOTE: the SHIPPED default (a fresh config) is TokyoNight Night
        // (`Config::default`); this struct stays the safe fallback so it
        // carries no bundle dependency. Matched verbatim to `assets/themes/
        // Catppuccin Mocha`.
        Theme {
            palette: [
                Rgb::new(0x45, 0x47, 0x5a),
                Rgb::new(0xf3, 0x8b, 0xa8),
                Rgb::new(0xa6, 0xe3, 0xa1),
                Rgb::new(0xf9, 0xe2, 0xaf),
                Rgb::new(0x89, 0xb4, 0xfa),
                Rgb::new(0xf5, 0xc2, 0xe7),
                Rgb::new(0x94, 0xe2, 0xd5),
                Rgb::new(0xa6, 0xad, 0xc8),
                Rgb::new(0x58, 0x5b, 0x70),
                Rgb::new(0xf3, 0x77, 0x99),
                Rgb::new(0x89, 0xd8, 0x8b),
                Rgb::new(0xeb, 0xd3, 0x91),
                Rgb::new(0x74, 0xa8, 0xfc),
                Rgb::new(0xf2, 0xae, 0xde),
                Rgb::new(0x6b, 0xd7, 0xca),
                Rgb::new(0xba, 0xc2, 0xde),
            ],
            background: Rgb::new(0x1e, 0x1e, 0x2e),
            foreground: Rgb::new(0xcd, 0xd6, 0xf4),
            cursor: Rgb::new(0xf5, 0xe0, 0xdc),
            cursor_text: Rgb::new(0x1e, 0x1e, 0x2e),
            selection_background: Rgb::new(0x58, 0x5b, 0x70),
            selection_foreground: Rgb::new(0xcd, 0xd6, 0xf4),
            // Catppuccin Mocha signature mauve (its named brand accent).
            accent: Rgb::new(0xcb, 0xa6, 0xf7),
        }
    }
}

impl Theme {
    /// A curated shortlist of the most popular terminal themes
    /// (rough popularity order), surfaced as the Settings → Appearance → Theme
    /// list of options — cycling them with ←/→ live-previews each. Every entry
    /// MUST be a bundled theme name (`popular_names_are_all_bundled` guards it).
    /// The full 500+ bundle stays reachable via the right-click Theme submenu,
    /// the `NextTheme`/`PrevTheme` actions, and the `theme =` config line.
    pub const POPULAR: &'static [&'static str] = &[
        "TokyoNight Night",
        "TokyoNight Storm",
        "TokyoNight Moon",
        "TokyoNight Day",
        "Catppuccin Mocha",
        "Catppuccin Macchiato",
        "Catppuccin Frappe",
        "Catppuccin Latte",
        "Dracula",
        "Gruvbox Dark",
        "Gruvbox Light",
        "Gruvbox Material",
        "Nord",
        "Nord Light",
        "iTerm2 Solarized Dark",
        "iTerm2 Solarized Light",
        "Rose Pine",
        "Rose Pine Moon",
        "Rose Pine Dawn",
        "Everforest Dark Hard",
        "Everforest Light Med",
        "Kanagawa Wave",
        "Kanagawa Lotus",
        "One Half Dark",
        "One Half Light",
        "Ayu Mirage",
        "Ayu Light",
        "Monokai Pro",
        "Night Owl",
    ];

    /// Whether this theme reads as a dark theme, judged by the WCAG
    /// relative luminance of its `background`. The 0.179 threshold is the
    /// contrast crossover point (backgrounds below it have more contrast
    /// against white than black), so the answer matches which button/title
    /// tint a titlebar over this background needs — the exact question the
    /// native-window-theme hint asks. Purely a function of the palette:
    /// per-pane transparency / background images don't reach the titlebar.
    pub fn is_dark(&self) -> bool {
        // sRGB channel -> linear-light (IEC 61966-2-1).
        fn linear(channel: u8) -> f64 {
            let c = f64::from(channel) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        let bg = self.background;
        let luminance = 0.2126 * linear(bg.r) + 0.7152 * linear(bg.g) + 0.0722 * linear(bg.b);
        luminance < 0.179
    }

    /// Parse a theme from Ghostty-syntax text (`palette = N=#hex`, etc.).
    /// Unspecified fields keep the default (Catppuccin Mocha) value.
    pub fn parse(text: &str) -> Theme {
        let mut t = Theme::default();
        // Track whether the theme declared its own accent. Ghostty
        // theme files don't define one, so a theme that doesn't (every bundled
        // theme except our Catppuccin Mocha, which carries an `accent` line)
        // gets `palette[4]` — its ANSI blue — as the conventional focus accent,
        // rather than inheriting the default's mauve.
        let mut explicit_accent = false;
        for e in parse::parse(text) {
            match e.key.as_str() {
                "palette" => {
                    if let Some((idx, hex)) = e.value.split_once('=')
                        && let (Ok(i), Some(rgb)) =
                            (idx.trim().parse::<usize>(), Rgb::parse(hex.trim()))
                        && i < 16
                    {
                        t.palette[i] = rgb;
                    }
                }
                "background" => set(&mut t.background, &e.value),
                "foreground" => set(&mut t.foreground, &e.value),
                "cursor-color" => set(&mut t.cursor, &e.value),
                "cursor-text" => set(&mut t.cursor_text, &e.value),
                "selection-background" => set(&mut t.selection_background, &e.value),
                "selection-foreground" => set(&mut t.selection_foreground, &e.value),
                // kettle extension: the chrome accent (focus border / active tab
                // / status bar). Catppuccin Mocha sets it to its signature mauve.
                "accent" => {
                    if let Some(rgb) = Rgb::parse(e.value.trim()) {
                        t.accent = rgb;
                        explicit_accent = true;
                    }
                }
                _ => {}
            }
        }
        if !explicit_accent {
            t.accent = t.palette[4];
        }
        t
    }

    /// Look up a bundled theme by name (case-insensitive). Returns the default
    /// (Catppuccin Mocha) if not found.
    pub fn by_name(name: &str) -> Theme {
        // `eq_ignore_ascii_case` compares in place — no per-element
        // `to_ascii_lowercase` String alloc (this runs on every theme keypress
        // / session restore over 500+ bundled names).
        let want = name.trim();
        for (n, body) in BUNDLED_THEMES.iter() {
            if n.eq_ignore_ascii_case(want) {
                return Theme::parse(body);
            }
        }
        Theme::default()
    }

    /// Companion to `by_name`: return the *canonical* bundled name
    /// (with the original casing the theme file ships under) for a
    /// case-insensitive user-typed name, or `None` if no bundled theme
    /// matches. `Config::parse_collect` uses it to keep `cfg.theme_name`
    /// in sync with `cfg.theme`, so a typo like `theme = TokyoNitght Night`
    /// doesn't make `--check-config` show a name the runtime isn't using.
    /// Pure.
    pub fn find_name(name: &str) -> Option<&'static str> {
        let want = name.trim();
        BUNDLED_THEMES
            .iter()
            .find_map(|(n, _)| n.eq_ignore_ascii_case(want).then_some(*n))
    }

    pub fn list() -> Vec<&'static str> {
        BUNDLED_THEMES.iter().map(|(n, _)| *n).collect()
    }
}

impl Theme {
    /// The bundled themes of one appearance (dark or light) in browsing
    /// order: each theme is followed by the most similar one left, judged by
    /// its background, foreground and accent in OKLab, starting from the most
    /// extreme background. Stepping through it changes the look a little at a
    /// time instead of jumping between unrelated palettes. Computed once.
    pub fn look_order(dark: bool) -> &'static [&'static str] {
        static ORDER: std::sync::OnceLock<[Vec<&'static str>; 2]> = std::sync::OnceLock::new();
        let [light, dark_order] = ORDER.get_or_init(|| {
            let looks: Vec<Look> = BUNDLED_THEMES
                .iter()
                .map(|(name, body)| Look::of(name, &Theme::parse(body)))
                .collect();
            [nearest_chain(&looks, false), nearest_chain(&looks, true)]
        });
        if dark { dark_order } else { light }
    }

    /// The next (`forward`) or previous bundled theme after `current` in
    /// [`Theme::look_order`], staying with `current`'s appearance and
    /// wrapping within it. An unknown `current` starts the dark order.
    pub fn cycle_by_look(current: &str, forward: bool) -> &'static str {
        let want = current.trim();
        for dark in [true, false] {
            let order = Theme::look_order(dark);
            if let Some(i) = order.iter().position(|n| n.eq_ignore_ascii_case(want)) {
                let n = order.len();
                return order[if forward {
                    (i + 1) % n
                } else {
                    (i + n - 1) % n
                }];
            }
        }
        Theme::look_order(true)
            .first()
            .copied()
            .unwrap_or("Catppuccin Mocha")
    }

    /// Like [`Theme::cycle_by_look`], but only through [`Theme::POPULAR`]:
    /// the next popular theme of `current`'s appearance in look order, from
    /// wherever `current` sits in that order, popular or not.
    pub fn cycle_popular_by_look(current: &str, forward: bool) -> &'static str {
        let want = current.trim();
        let popular = |name: &str| Theme::POPULAR.iter().any(|p| p.eq_ignore_ascii_case(name));
        for dark in [true, false] {
            let order = Theme::look_order(dark);
            if let Some(i) = order.iter().position(|n| n.eq_ignore_ascii_case(want)) {
                let n = order.len();
                return (1..=n)
                    .map(|step| {
                        if forward {
                            (i + step) % n
                        } else {
                            (i + n - step % n) % n
                        }
                    })
                    .map(|j| order[j])
                    .find(|name| popular(name))
                    .unwrap_or(order[i]);
            }
        }
        Theme::POPULAR[0]
    }
}

/// What a theme looks like, as points in OKLab.
struct Look {
    name: &'static str,
    dark: bool,
    background: [f64; 3],
    foreground: [f64; 3],
    accent: [f64; 3],
}

impl Look {
    fn of(name: &'static str, theme: &Theme) -> Self {
        Look {
            name,
            dark: theme.is_dark(),
            background: oklab(theme.background),
            foreground: oklab(theme.foreground),
            accent: oklab(theme.accent),
        }
    }

    /// Perceptual distance; the background weighs most, being most of what
    /// the window shows.
    fn distance(&self, other: &Look) -> f64 {
        let d = |a: [f64; 3], b: [f64; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };
        2.0 * d(self.background, other.background)
            + d(self.foreground, other.foreground)
            + 0.5 * d(self.accent, other.accent)
    }
}

/// Greedy nearest-neighbour walk through the themes of one appearance,
/// starting from the darkest background (dark themes) or the lightest
/// (light themes). Ties break by name, so the order is stable.
fn nearest_chain(looks: &[Look], dark: bool) -> Vec<&'static str> {
    let mut left: Vec<&Look> = looks.iter().filter(|l| l.dark == dark).collect();
    left.sort_by(|a, b| a.name.cmp(b.name));
    let Some(start) = left
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let (a, b) = (a.background[0], b.background[0]);
            if dark {
                a.total_cmp(&b)
            } else {
                b.total_cmp(&a)
            }
        })
        .map(|(i, _)| i)
    else {
        return Vec::new();
    };
    let mut order = Vec::with_capacity(left.len());
    let mut current = left.remove(start);
    order.push(current.name);
    while !left.is_empty() {
        let next = left
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| current.distance(a).total_cmp(&current.distance(b)))
            .map(|(i, _)| i)
            .expect("non-empty");
        current = left.remove(next);
        order.push(current.name);
    }
    order
}

/// sRGB to OKLab (Björn Ottosson's reference conversion).
fn oklab(c: Rgb) -> [f64; 3] {
    fn linear(channel: u8) -> f64 {
        let c = f64::from(channel) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let (r, g, b) = (linear(c.r), linear(c.g), linear(c.b));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

fn set(slot: &mut Rgb, v: &str) {
    if let Some(rgb) = Rgb::parse(v) {
        *slot = rgb;
    }
}

#[cfg(test)]
mod tests {
    use super::Theme;
    use crate::color::Rgb;

    /// The theme's UI accent. Catppuccin Mocha = signature mauve
    /// (matches the icon); a theme that declares no `accent` line gets its ANSI
    /// blue `palette[4]`; an explicit `accent = #hex` line is honored.
    #[test]
    fn theme_accent_is_mocha_mauve_else_palette4() {
        // Default (Mocha) → mauve.
        assert_eq!(Theme::default().accent, Rgb::new(0xcb, 0xa6, 0xf7));
        // The bundled Mocha carries the accent line, so by_name matches default.
        assert_eq!(
            Theme::by_name("Catppuccin Mocha").accent,
            Rgb::new(0xcb, 0xa6, 0xf7)
        );
        // A theme WITHOUT an accent line → its palette[4] (ANSI blue).
        let t = Theme::parse("palette = 4=#001122\nbackground = #000000\n");
        assert_eq!(t.accent, Rgb::new(0x00, 0x11, 0x22));
        // An explicit accent line is honored over palette[4].
        let t = Theme::parse("palette = 4=#001122\naccent = #aabbcc\n");
        assert_eq!(t.accent, Rgb::new(0xaa, 0xbb, 0xcc));
    }

    /// `Theme::default()` is a hand-transcribed copy of the bundled
    /// `Catppuccin Mocha`. Pin that the hard-coded fallback matches the bundled
    /// theme byte-for-byte, so a typo in the literal palette can't silently
    /// diverge the compile-time default from what `theme = Catppuccin Mocha`
    /// resolves to. (Theme has no PartialEq, so compare the Debug fingerprint,
    /// the file's convention.)
    #[test]
    fn default_matches_bundled_catppuccin_mocha() {
        assert!(
            Theme::find_name("Catppuccin Mocha").is_some(),
            "the bundled Catppuccin Mocha theme must exist (the default resolves to it)"
        );
        assert_eq!(
            format!("{:?}", Theme::default()),
            format!("{:?}", Theme::by_name("Catppuccin Mocha")),
            "Theme::default() must equal the bundled Catppuccin Mocha palette"
        );
    }

    /// `is_dark` classifies by WCAG relative luminance of the
    /// background with the 0.179 contrast-crossover threshold. Pin the
    /// classification for the shipped default plus well-known bundled
    /// light/dark pairs, and the pure-black/white/mid-gray boundaries, so a
    /// tweak to the formula can't silently flip native titlebar theming.
    #[test]
    fn is_dark_classifies_bundled_and_boundary_backgrounds() {
        // Shipped default (TokyoNight Night, #1a1b26) and the fallback
        // (Catppuccin Mocha, #1e1e2e) are dark.
        assert!(Theme::by_name("TokyoNight Night").is_dark());
        assert!(Theme::default().is_dark());
        // Well-known light/dark bundled pairs land on opposite sides.
        assert!(Theme::by_name("Gruvbox Dark").is_dark());
        assert!(!Theme::by_name("Gruvbox Light").is_dark());
        assert!(Theme::by_name("iTerm2 Solarized Dark").is_dark());
        assert!(!Theme::by_name("iTerm2 Solarized Light").is_dark());
        // Boundaries: pure black is dark, pure white is light, and #808080
        // (relative luminance ~0.216, above the 0.179 crossover) reads light.
        assert!(
            Theme {
                background: Rgb::new(0x00, 0x00, 0x00),
                ..Theme::default()
            }
            .is_dark()
        );
        assert!(
            !Theme {
                background: Rgb::new(0xff, 0xff, 0xff),
                ..Theme::default()
            }
            .is_dark()
        );
        assert!(
            !Theme {
                background: Rgb::new(0x80, 0x80, 0x80),
                ..Theme::default()
            }
            .is_dark()
        );
    }

    #[test]
    fn by_name_and_find_name_are_case_and_pad_insensitive() {
        let first = Theme::list()[0];
        let typed = format!("  {}  ", first.to_uppercase());
        assert_eq!(
            Theme::find_name(&typed),
            Some(first),
            "find_name resolves a differently-cased/padded name to the canonical one"
        );
        // by_name returns the parsed theme; a matched name must resolve to the
        // same theme as the verbatim-name parse (Theme has no PartialEq — its
        // Debug repr is a faithful structural fingerprint here).
        assert_eq!(
            format!("{:?}", Theme::by_name(&typed)),
            format!("{:?}", Theme::by_name(first)),
        );
        assert_eq!(Theme::find_name("no such theme zzz"), None);
    }

    /// The four Terminator-app built-in palettes (linux / xterm /
    /// rxvt / ambience) are NOT in the upstream iTerm2-Color-Schemes collection
    /// kettle bundles, so they were hand-ported into `assets/themes/` to make
    /// "all Terminator themes" literally complete. Guard that they're bundled
    /// (filename → theme name) and that the file actually parsed.
    #[test]
    fn bundled_themes_include_terminator_defaults() {
        for name in [
            "Terminator Linux",
            "Terminator XTerm",
            "Terminator Rxvt",
            "Terminator Ambience",
        ] {
            assert!(
                Theme::find_name(name).is_some(),
                "hand-ported Terminator default {name:?} is not bundled (file under \
                 assets/themes/ missing, or build.rs didn't rerun)"
            );
        }
        // Ambience must carry Ubuntu's aubergine background — proves the file
        // parsed, not just name-matched then fell back to the default palette.
        let amb = Theme::by_name("Terminator Ambience");
        assert_eq!(
            (amb.background.r, amb.background.g, amb.background.b),
            (0x30, 0x0a, 0x24),
            "Terminator Ambience background should be Ubuntu aubergine #300a24"
        );
    }

    /// Every curated Settings theme must resolve to a real bundled
    /// theme — otherwise the Settings → Theme list would offer a dead option
    /// that silently falls back to the default. Also guards against duplicates.
    /// The two look orders split the bundle by appearance, each theme once.
    #[test]
    fn look_orders_partition_the_bundle_by_appearance() {
        let dark = Theme::look_order(true);
        let light = Theme::look_order(false);
        assert_eq!(dark.len() + light.len(), Theme::list().len());
        let mut seen = std::collections::BTreeSet::new();
        for (order, is_dark) in [(dark, true), (light, false)] {
            for name in order {
                assert!(seen.insert(*name), "{name} listed twice");
                assert_eq!(Theme::by_name(name).is_dark(), is_dark, "{name}");
            }
        }
        assert!(dark.len() > 400 && light.len() > 100);
    }

    /// Stepping by look never jumps between light and dark, and back undoes
    /// forward, from every bundled theme.
    #[test]
    fn cycling_by_look_stays_within_an_appearance() {
        for name in Theme::list() {
            let dark = Theme::by_name(name).is_dark();
            let next = Theme::cycle_by_look(name, true);
            assert_eq!(Theme::by_name(next).is_dark(), dark, "{name} -> {next}");
            assert_eq!(Theme::cycle_by_look(next, false), name);
        }
        assert_eq!(
            Theme::cycle_by_look("not a theme", true),
            Theme::look_order(true)[0]
        );
        // A config name differs in case or spacing; it still cycles from here.
        let first = Theme::look_order(true)[0];
        assert_eq!(
            Theme::cycle_by_look(&format!("  {}  ", first.to_uppercase()), true),
            Theme::cycle_by_look(first, true)
        );
    }

    /// Neighbours in look order resemble each other far more than neighbours
    /// in name order, which is what makes stepping through themes gentle.
    #[test]
    fn look_order_steps_are_small() {
        use super::{BUNDLED_THEMES, Look};
        let looks: Vec<Look> = BUNDLED_THEMES
            .iter()
            .map(|(name, body)| Look::of(name, &Theme::parse(body)))
            .collect();
        let look = |name: &str| looks.iter().find(|l| l.name == name).unwrap();
        let mean_step = |names: &[&str]| {
            let steps: Vec<f64> = names
                .windows(2)
                .map(|w| look(w[0]).distance(look(w[1])))
                .collect();
            steps.iter().sum::<f64>() / steps.len() as f64
        };
        for dark in [true, false] {
            let by_look = Theme::look_order(dark);
            let mut by_name = by_look.to_vec();
            by_name.sort_unstable();
            assert!(
                mean_step(by_look) * 2.0 < mean_step(&by_name),
                "dark={dark}: look {} vs name {}",
                mean_step(by_look),
                mean_step(&by_name)
            );
        }
    }

    /// The popular cycle keeps the appearance too, and reaches popular themes
    /// from a theme outside the list.
    #[test]
    fn popular_cycling_by_look_stays_within_an_appearance() {
        for name in Theme::POPULAR {
            let dark = Theme::by_name(name).is_dark();
            for forward in [true, false] {
                let next = Theme::cycle_popular_by_look(name, forward);
                assert!(Theme::POPULAR.contains(&next), "{next}");
                assert_eq!(Theme::by_name(next).is_dark(), dark, "{name} -> {next}");
            }
        }
        // TokyoNight Moon used to step straight to the light TokyoNight Day.
        assert!(Theme::by_name(Theme::cycle_popular_by_look("TokyoNight Moon", true)).is_dark());
        let outside = Theme::look_order(true)
            .iter()
            .find(|n| !Theme::POPULAR.contains(n))
            .unwrap();
        assert!(Theme::POPULAR.contains(&Theme::cycle_popular_by_look(outside, true)));
    }

    #[test]
    fn popular_names_are_all_bundled() {
        let mut seen = std::collections::HashSet::new();
        for name in Theme::POPULAR {
            assert!(
                Theme::find_name(name).is_some(),
                "curated POPULAR theme {name:?} is not a bundled theme name"
            );
            assert!(seen.insert(*name), "duplicate curated theme {name:?}");
        }
    }

    /// The docs advertise "500+ bundled themes" — range-stable
    /// phrasing chosen so the exact count can't silently drift in the docs each
    /// time the bundle is re-synced. Guard the FLOOR so a catastrophic drop
    /// would fail CI rather than quietly contradict that claim. (Current bundle
    /// is ~532; this only trips if it falls below 500.)
    #[test]
    fn bundled_theme_count_supports_500_plus_claim() {
        let n = Theme::list().len();
        assert!(
            n >= 500,
            "bundled theme count fell to {n}; the docs claim '500+'. Re-sync \
             assets/themes/ from iTerm2-Color-Schemes or update the docs."
        );
    }
}

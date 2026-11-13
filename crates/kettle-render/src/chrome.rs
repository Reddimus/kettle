use crate::{MinimumContrastCache, color, dim_blend};
use kettle_config::Rgb;

// WCAG 2.2 AA: 1.4.3 (normal text) and 1.4.11 (non-text controls).
pub(crate) const TEXT_CONTRAST: f64 = 4.5;
pub(crate) const ICON_CONTRAST: f64 = 3.0;
// WCAG 2.2 AAA 1.4.6, for secondary chrome text people read while scanning:
// inactive tab titles, tab close glyphs and menu shortcut hints.
pub(crate) const SECONDARY_TEXT_CONTRAST: f64 = 7.0;

#[derive(Default)]
pub(crate) struct Colors {
    cache: MinimumContrastCache,
    muted: std::collections::HashMap<(u32, u32), Rgb>,
    secondary: std::collections::HashMap<(u32, u32), Rgb>,
    surfaces: std::collections::HashMap<([u32; 2], u64, bool), Rgb>,
    completion: Option<([u32; 5], crate::CompletionPalette)>,
    inactive: std::collections::HashMap<([u32; 3], u32, bool), Rgb>,
    markers: std::collections::HashMap<([u32; 2], bool), Rgb>,
}

impl Colors {
    pub(crate) fn completion(
        &mut self,
        theme: &kettle_config::Theme,
        accent: Rgb,
    ) -> crate::CompletionPalette {
        let key = [
            theme.foreground,
            theme.background,
            theme.selection_foreground,
            theme.selection_background,
            accent,
        ]
        .map(MinimumContrastCache::color_key);
        if let Some((saved, palette)) = self.completion
            && saved == key
        {
            return palette;
        }
        let palette = crate::completion_palette_with_colors(theme, accent, self);
        self.completion = Some((key, palette));
        palette
    }

    pub(crate) fn text(&mut self, fg: Rgb, bg: Rgb) -> Rgb {
        self.resolve(fg, bg, TEXT_CONTRAST)
    }

    /// Window deactivation fades chrome without losing readable titles.
    pub(crate) fn tab_text(&mut self, fg: Rgb, bg: Rgb, focused: bool) -> Rgb {
        if focused {
            self.text(fg, bg)
        } else {
            self.resolve(color::over(fg, bg, 0.5, false), bg, 3.0)
        }
    }

    pub(crate) fn muted(&mut self, fg: Rgb, bg: Rgb) -> Rgb {
        let key = (
            MinimumContrastCache::color_key(fg),
            MinimumContrastCache::color_key(bg),
        );
        if let Some(color) = self.muted.get(&key) {
            return *color;
        }
        let color = self.text(dim_blend(fg, bg), bg);
        if self.muted.len() >= 4096 {
            self.muted.clear();
        }
        self.muted.insert(key, color);
        color
    }

    /// Quieter than `fg` but clearer than `muted`: `fg` moved toward `bg`
    /// only until it reaches `SECONDARY_TEXT_CONTRAST`, so it keeps the
    /// theme's hue. A theme whose own foreground is weaker stays undimmed,
    /// and nothing goes below `TEXT_CONTRAST`.
    pub(crate) fn secondary(&mut self, fg: Rgb, bg: Rgb) -> Rgb {
        let key = (
            MinimumContrastCache::color_key(fg),
            MinimumContrastCache::color_key(bg),
        );
        if let Some(color) = self.secondary.get(&key) {
            return *color;
        }
        let reaches = |t: f64| {
            color::contrast_ratio(color::over(fg, bg, t, false), bg) >= SECONDARY_TEXT_CONTRAST
        };
        // How far toward the background: none when `fg` is already at or
        // below the target, never further than `muted` goes (55%).
        let mut dim = 0.0_f64;
        if reaches(0.0) {
            let (mut lo, mut hi) = (0.0_f64, 0.55_f64);
            if reaches(hi) {
                lo = hi;
            }
            while hi - lo > 1.0 / 16384.0 {
                let mid = (lo + hi) / 2.0;
                if reaches(mid) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            dim = lo;
        }
        let color = self.text(color::over(fg, bg, dim, false), bg);
        if self.secondary.len() >= 4096 {
            self.secondary.clear();
        }
        self.secondary.insert(key, color);
        color
    }

    pub(crate) fn surface(&mut self, base: Rgb, top: Rgb, alpha: f64, linear: bool) -> Rgb {
        let key = (
            [base, top].map(MinimumContrastCache::color_key),
            alpha.to_bits(),
            linear,
        );
        if let Some(color) = self.surfaces.get(&key) {
            return *color;
        }
        let color = color::over(base, top, alpha, linear);
        if self.surfaces.len() >= 4096 {
            self.surfaces.clear();
        }
        self.surfaces.insert(key, color);
        color
    }

    pub(crate) fn close_background(
        &mut self,
        theme: &kettle_config::Theme,
        bg: Rgb,
        inactive_bg: Rgb,
        state: (bool, bool),
        linear: bool,
    ) -> Rgb {
        let (active, hovered) = state;
        let base = if active { bg } else { inactive_bg };
        let (top, alpha) = if hovered {
            (theme.palette[1], 0.85)
        } else if active {
            (theme.palette[8], 0.55)
        } else {
            (theme.foreground, 0.12)
        };
        self.surface(base, top, alpha, linear)
    }

    /// A background tab's activity marker on the tab's own fill: at least
    /// `ICON_CONTRAST`, reached by moving only the ink's lightness so the
    /// theme's cyan stays cyan and its yellow stays yellow. An unfocused
    /// window fades it halfway toward the tab first, never below the floor.
    pub(crate) fn tab_marker(&mut self, ink: Rgb, bg: Rgb, focused: bool) -> Rgb {
        let key = ([ink, bg].map(MinimumContrastCache::color_key), focused);
        if let Some(color) = self.markers.get(&key) {
            return *color;
        }
        let ink = if focused {
            ink
        } else {
            color::over(ink, bg, 0.5, false)
        };
        let color = color::with_min_contrast_same_hue(ink, bg, ICON_CONTRAST);
        if self.markers.len() >= 4096 {
            self.markers.clear();
        }
        self.markers.insert(key, color);
        color
    }

    pub(crate) fn icon(&mut self, fg: Rgb, bg: Rgb) -> Rgb {
        self.resolve(fg, bg, ICON_CONTRAST)
    }

    pub(crate) fn inactive(
        &mut self,
        fg: Rgb,
        bg: Rgb,
        target: Rgb,
        amount: f32,
        linear: bool,
    ) -> Rgb {
        if amount <= 0.0 {
            return fg;
        }
        let key = (
            [fg, bg, target].map(MinimumContrastCache::color_key),
            amount.to_bits(),
            linear,
        );
        if let Some(color) = self.inactive.get(&key) {
            return *color;
        }
        let background = color::over(bg, target, f64::from(amount), linear);
        let muted = color::over(fg, target, f64::from(amount), linear);
        // Dimming must not take text below the floor, but it must not lift
        // text a program drew fainter than that either: an inactive pane
        // would then show it more clearly than the active one does.
        // The bundled M's least-covered stem phase peaks at 248/255.
        // Resolve against that painted pixel: light and dark backgrounds
        // need different reserves, and output rounding matters too.
        let original_contrast = color::contrast_ratio(fg, bg);
        let floor = TEXT_CONTRAST.min(original_contrast);
        let covered =
            color::with_min_contrast_at_coverage(muted, background, floor, 248.0 / 255.0, linear);
        let resolved = if color::contrast_ratio(covered, background) > original_contrast {
            self.resolve(muted, background, original_contrast)
        } else {
            covered
        };
        if self.inactive.len() >= 4096 {
            self.inactive.clear();
        }
        self.inactive.insert(key, resolved);
        resolved
    }

    fn resolve(&mut self, fg: Rgb, bg: Rgb, minimum: f64) -> Rgb {
        if self.cache.entries.len() >= 4096 {
            self.cache.clear();
        }
        self.cache.resolve(fg, bg, minimum)
    }
}

/// The theme colour a background tab's activity marker starts from: cyan for
/// new output, the dim chrome grey once it stopped, yellow for the bell.
pub(crate) fn activity_ink(theme: &kettle_config::Theme, kind: crate::TabActivity) -> Option<Rgb> {
    use crate::TabActivity;
    match kind {
        TabActivity::Output => Some(theme.palette[6]),
        TabActivity::Silent => Some(theme.palette[8]),
        TabActivity::Bell => Some(theme.palette[3]),
        TabActivity::Normal => None,
    }
}

/// The active tab's bar. It sits on the tab and against the strip around
/// it, so it reaches `ICON_CONTRAST` against both: resolved first against
/// whichever it is weaker on, then checked again against the other. A
/// focused window keeps the accent's hue (broadcast's yellow, a window's own
/// accent); an unfocused one turns it a neutral grey that is still the
/// strongest mark in the strip, unlike the hover fill it once matched.
/// `active_bg` is the terminal's opaque colour, the stated approximation for
/// a translucent background.
pub(crate) fn tab_accent(
    raw: Rgb,
    strip: Rgb,
    active_bg: Rgb,
    foreground: Rgb,
    focused: bool,
) -> Rgb {
    let mut ink = if focused {
        raw
    } else {
        color::over(foreground, strip, 0.5, false)
    };
    for _ in 0..3 {
        let weaker = if color::contrast_ratio(ink, strip) <= color::contrast_ratio(ink, active_bg) {
            strip
        } else {
            active_bg
        };
        if color::contrast_ratio(ink, weaker) >= ICON_CONTRAST {
            break;
        }
        ink = if focused {
            color::with_min_contrast_same_hue(ink, weaker, ICON_CONTRAST)
        } else {
            color::with_min_contrast(ink, weaker, ICON_CONTRAST)
        };
    }
    ink
}

/// The hover fill shared by everything on the strip: tabs, window controls,
/// `+` and the menu button. At 1.3:1 it shows where the pointer is but stays
/// well under the active tab's 3:1 bar, so the active tab remains the
/// strongest mark in the strip. A press deepens it to `tab_detail`'s 1.8:1.
pub(crate) fn strip_hover(strip: Rgb, foreground: Rgb, pressed: bool) -> Rgb {
    if pressed {
        return tab_detail(strip, foreground, true);
    }
    color::with_min_contrast(color::over(strip, foreground, 0.06, false), strip, 1.3)
}

/// Neutral chrome details must remain visible independently of ANSI colours.
pub(crate) fn tab_detail(strip: Rgb, foreground: Rgb, pressed: bool) -> Rgb {
    color::with_min_contrast(
        color::over(strip, foreground, if pressed { 0.18 } else { 0.10 }, false),
        strip,
        if pressed { 1.8 } else { 1.5 },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_config::Theme;

    #[test]
    fn bundled_chrome_roles_meet_contrast_floors() {
        assert_eq!(TEXT_CONTRAST, 4.5);
        assert_eq!(ICON_CONTRAST, 3.0);
        assert_eq!(SECONDARY_TEXT_CONTRAST, 7.0);
        let mut failures = Vec::new();
        let names = Theme::list();
        for name in &names {
            let theme = Theme::by_name(name);
            let mut colors = Colors::default();
            for linear in [false, true] {
                let bg = theme.background;
                let fg = theme.foreground;
                let strip = theme.palette[8];
                let selected = color::over(bg, theme.accent, 0.18, linear);
                let mut roles = vec![
                    (
                        "menu information/empty state/disabled row",
                        colors.muted(fg, bg),
                        bg,
                        TEXT_CONTRAST,
                    ),
                    (
                        "selected menu row shortcut/type/check",
                        colors.secondary(fg, selected),
                        selected,
                        TEXT_CONTRAST,
                    ),
                    (
                        "settings focused-row description",
                        colors.muted(fg, selected),
                        selected,
                        TEXT_CONTRAST,
                    ),
                    (
                        "settings hint/disabled",
                        colors.muted(fg, bg),
                        bg,
                        TEXT_CONTRAST,
                    ),
                    (
                        "inactive tab/status/title",
                        colors.text(fg, strip),
                        strip,
                        TEXT_CONTRAST,
                    ),
                    (
                        "menu selection rail",
                        colors.icon(theme.accent, selected),
                        selected,
                        ICON_CONTRAST,
                    ),
                    (
                        "menu scroll arrow",
                        colors.icon(theme.accent, bg),
                        bg,
                        ICON_CONTRAST,
                    ),
                ];
                for active in [false, true] {
                    for hovered in [false, true] {
                        let close =
                            colors.close_background(&theme, bg, strip, (active, hovered), linear);
                        roles.push((
                            "tab close",
                            colors.icon(if hovered { theme.cursor_text } else { strip }, close),
                            close,
                            ICON_CONTRAST,
                        ));
                    }
                }
                for hovered in [false, true] {
                    let toolbar = if hovered {
                        color::over(strip, theme.accent, 0.14, linear)
                    } else {
                        strip
                    };
                    roles.push((
                        "toolbar glyph",
                        colors.icon(if hovered { theme.accent } else { fg }, toolbar),
                        toolbar,
                        ICON_CONTRAST,
                    ));
                }
                let search = color::over(bg, strip, 0.98, linear);
                for (role, surface) in [
                    ("search label/hint", search),
                    ("search editor", color::over(search, bg, 0.72, linear)),
                    ("search button", color::over(search, bg, 0.28, linear)),
                ] {
                    roles.push((role, colors.text(fg, surface), surface, TEXT_CONTRAST));
                }
                for accent in [theme.accent, theme.palette[3]] {
                    roles.push((
                        "pane header",
                        colors.text(theme.cursor_text, accent),
                        accent,
                        TEXT_CONTRAST,
                    ));
                    let selected = color::over(bg, accent, 0.22, linear);
                    roles.push((
                        "settings selected/disabled",
                        colors.muted(fg, selected),
                        selected,
                        TEXT_CONTRAST,
                    ));
                    for focused in [false, true] {
                        let fill = color::over(
                            search,
                            if focused { accent } else { bg },
                            if focused {
                                f64::from(crate::SEARCH_FOCUSED_FILL_ALPHA)
                            } else {
                                0.28
                            },
                            linear,
                        );
                        for tint in [None, Some((fg, 0.14)), Some((bg, 0.45))] {
                            let fill = tint.map_or(fill, |(tint, alpha)| {
                                color::over(fill, tint, alpha, linear)
                            });
                            roles.push((
                                "search focused/hover/pressed",
                                colors.text(if focused { theme.cursor_text } else { fg }, fill),
                                fill,
                                TEXT_CONTRAST,
                            ));
                        }
                    }
                }
                for amount in [0.2, 0.55, 0.95] {
                    let pane_bg = color::over(bg, bg, amount, linear);
                    // Dimming holds text at the floor, or at the theme's own
                    // contrast where that is lower.
                    roles.push((
                        "inactive terminal",
                        colors.inactive(fg, bg, bg, amount as f32, linear),
                        pane_bg,
                        TEXT_CONTRAST.min(color::contrast_ratio(fg, bg)),
                    ));
                }
                for (role, actual, background, floor) in roles {
                    let ratio = color::contrast_ratio(actual, background);
                    if ratio < floor {
                        failures.push(format!("{name}: {role} ({linear}) {ratio:.3}:1 < {floor}"));
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failing theme/roles across {} themes: {}",
            failures.len(),
            names.len(),
            failures
                .iter()
                .take(15)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    #[test]
    fn tab_controls_and_shortcut_hints_meet_their_contrast_floors() {
        let mut colors = Colors::default();
        for name in Theme::list() {
            let theme = Theme::by_name(name);
            for strip in [
                theme.background,
                theme.palette[8],
                Rgb::new(0, 0, 0),
                Rgb::new(255, 255, 255),
            ] {
                for base in [strip, theme.background] {
                    for hover in [false, true] {
                        let background = if hover {
                            color::over(base, theme.foreground, 0.06, false)
                        } else {
                            base
                        };
                        for glyph in [theme.foreground, theme.accent, theme.palette[8]] {
                            assert!(
                                color::contrast_ratio(colors.text(glyph, background), background)
                                    >= 4.5,
                                "{name}: tab control"
                            );
                        }
                        for (role, ink) in [
                            ("Cmd+N hint", colors.muted(theme.foreground, background)),
                            (
                                "inactive tab title",
                                colors.secondary(theme.foreground, background),
                            ),
                        ] {
                            assert!(
                                color::contrast_ratio(ink, background) >= TEXT_CONTRAST,
                                "{name}: {role}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Inactive dimming keeps readable text at the floor but never makes
    /// text a program drew faintly clearer than it is in the active pane.
    #[test]
    fn inactive_dimming_floors_text_without_lifting_faint_text() {
        let mut colors = Colors::default();
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(0xff, 0xff, 0xff);
        let readable = colors.inactive(white, black, black, 0.6, false);
        assert!(color::contrast_ratio(readable, black) >= TEXT_CONTRAST - 0.01);
        let faint = Rgb::new(0x40, 0x40, 0x40);
        let original = color::contrast_ratio(faint, black);
        assert!(original < TEXT_CONTRAST);
        let dimmed = colors.inactive(faint, black, black, 0.6, false);
        assert!(
            color::contrast_ratio(dimmed, black) <= original + 0.01,
            "inactive text {dimmed:?} is clearer than the active {faint:?}"
        );
    }

    #[test]
    fn chrome_colors_reuse_contrast_results_and_preserve_inactive_opt_out() {
        let theme = Theme::by_name("Builtin Light");
        let mut colors = Colors::default();
        for _ in 0..3 {
            colors.muted(theme.foreground, theme.background);
            colors.icon(theme.foreground, theme.palette[8]);
            colors.inactive(
                theme.foreground,
                theme.background,
                theme.background,
                0.55,
                true,
            );
        }
        // Covered ink uses the inactive cache; muted and icon share this one.
        assert_eq!(colors.cache.misses, 2);
        assert_eq!(colors.inactive.len(), 1);
        let program = Rgb::new(254, 1, 32);
        assert_eq!(
            colors.inactive(program, theme.background, theme.background, 0.0, true),
            program
        );
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    #[test]
    fn bundled_completion_secondary_roles_meet_contrast_floors() {
        let mut failures = Vec::new();
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let palette = crate::completion_palette(&theme, theme.accent);
            for (role, fg, bg, minimum) in [
                (
                    "completion description",
                    palette.description,
                    palette.panel_bg,
                    TEXT_CONTRAST,
                ),
                (
                    "completion selected description",
                    palette.selected_description,
                    palette.selection_bg,
                    TEXT_CONTRAST,
                ),
                (
                    "completion selection rail",
                    palette.rail,
                    palette.selection_bg,
                    ICON_CONTRAST,
                ),
                (
                    "completion scroll thumb",
                    palette.scroll_thumb,
                    palette.scroll_track,
                    ICON_CONTRAST,
                ),
            ] {
                let ratio = color::contrast_ratio(fg, bg);
                if ratio < minimum {
                    failures.push(format!("{name}: {role} {ratio:.3}:1 < {minimum}"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failing theme/roles: {}",
            failures.len(),
            failures
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}

#[cfg(test)]
mod review_regression_tests {
    use super::*;

    #[test]
    fn regression_close_contrast_uses_chip_tint_independent_of_strip() {
        let theme = kettle_config::Theme::by_name("Builtin Dark");
        let strip = Rgb::new(0, 0, 0);
        let mut colors = Colors::default();
        for linear in [false, true] {
            for active in [false, true] {
                for hovered in [false, true] {
                    let base = if active { theme.background } else { strip };
                    let (tint, alpha) = if hovered {
                        (theme.palette[1], 0.85)
                    } else if active {
                        (theme.palette[8], 0.55)
                    } else {
                        (theme.foreground, 0.12)
                    };
                    let painted = color::over(base, tint, alpha, linear);
                    let assumed = colors.close_background(
                        &theme,
                        theme.background,
                        strip,
                        (active, hovered),
                        linear,
                    );
                    let icon = colors.icon(
                        if hovered {
                            theme.cursor_text
                        } else {
                            theme.palette[8]
                        },
                        assumed,
                    );
                    let ratio = color::contrast_ratio(icon, painted);
                    assert!(
                        ratio >= ICON_CONTRAST,
                        "active={active}, hovered={hovered}, linear={linear}: {ratio:.3}:1"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tab_chrome_tests {
    use super::*;

    #[test]
    fn solid_chrome_is_quiet_and_details_and_dimmed_text_keep_contrast() {
        for theme in [
            kettle_config::Theme::default(),
            kettle_config::Theme::by_name("Builtin Light"),
        ] {
            let cfg = kettle_config::Config::default();
            let strip = crate::resolve_chrome_bg(&cfg, &theme, None);
            assert!(color::relative_luminance(strip) < color::relative_luminance(theme.background));
            assert_ne!(strip, theme.palette[8]);
            let mut colors = Colors::default();
            for pressed in [false, true] {
                let detail = tab_detail(strip, theme.foreground, pressed);
                assert!(color::contrast_ratio(detail, strip) >= 1.5);
                for background in [strip, detail, theme.background] {
                    let focused = colors.tab_text(theme.foreground, background, true);
                    let dimmed = colors.tab_text(focused, background, false);
                    assert!(color::contrast_ratio(focused, background) >= 4.5);
                    assert!(color::contrast_ratio(dimmed, background) >= 3.0);
                    assert!(
                        color::contrast_ratio(dimmed, background)
                            < color::contrast_ratio(focused, background)
                    );
                }
            }
            // ANSI bright black can change without tinting any solid chrome role.
            let mut custom = theme.clone();
            custom.palette[8] = Rgb::new(255, 0, 0);
            assert_eq!(crate::resolve_chrome_bg(&cfg, &custom, None), strip);
            assert_eq!(
                tab_detail(strip, custom.foreground, false),
                tab_detail(strip, theme.foreground, false)
            );
        }
    }

    /// The active tab's bar reaches 3:1 against both the tab it sits on and
    /// the strip around it, for every accent source, focused or not. The
    /// unfocused bar was the hover fill's 1.5:1, and broadcast's yellow about
    /// 2.2:1 in Catppuccin Latte.
    #[test]
    fn tab_accent_reaches_3_to_1_on_tab_and_strip() {
        let cfg = kettle_config::Config::default();
        let mut failures = Vec::new();
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let strip = crate::resolve_chrome_bg(&cfg, &theme, None);
            for (source, raw) in [
                ("theme accent", theme.accent),
                ("configured accent", cfg.resolved_accent(&theme)),
                ("broadcast", theme.palette[3]),
            ] {
                for focused in [true, false] {
                    let bar = tab_accent(raw, strip, theme.background, theme.foreground, focused);
                    for (surface, bg) in [("strip", strip), ("active tab", theme.background)] {
                        let ratio = color::contrast_ratio(bar, bg);
                        if ratio < ICON_CONTRAST - 1e-6 {
                            failures.push(format!(
                                "{name}: {source} (focused {focused}) on {surface} {ratio:.2}:1"
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failing bars: {}",
            failures.len(),
            failures
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    /// Every activity marker reaches 3:1 on the tab behind it, hovered or
    /// not and in a focused or unfocused window, and a focused window's
    /// marker keeps the theme colour's hue and saturation. TokyoNight Night's Silent grey
    /// was 2.04:1 on its strip, Catppuccin Latte's bell yellow 2.04:1.
    #[test]
    fn tab_activity_markers_keep_3_to_1_and_their_hue() {
        use crate::TabActivity;
        let cfg = kettle_config::Config::default();
        let mut failures = Vec::new();
        let hue_gap = |a: f64, b: f64| {
            let d = (a - b).rem_euclid(6.0);
            d.min(6.0 - d)
        };
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let strip = crate::resolve_chrome_bg(&cfg, &theme, None);
            let mut colors = Colors::default();
            for (surface, bg) in [
                ("strip", strip),
                ("hovered tab", strip_hover(strip, theme.foreground, false)),
            ] {
                for kind in [TabActivity::Output, TabActivity::Silent, TabActivity::Bell] {
                    let raw = activity_ink(&theme, kind).expect("a marked state has ink");
                    for focused in [true, false] {
                        let ink = colors.tab_marker(raw, bg, focused);
                        let ratio = color::contrast_ratio(ink, bg);
                        if ratio < ICON_CONTRAST - 1e-6 {
                            failures.push(format!(
                                "{name}: {kind:?} on {surface} (focused {focused}) {ratio:.2}:1"
                            ));
                        }
                        let (raw_hue, raw_saturation, _) = color::hsl(raw);
                        let (hue, saturation, lightness) = color::hsl(ink);
                        // Hue is defined only for a visible tint.
                        let tinted = raw_saturation >= 0.25
                            && saturation >= 0.25
                            && (0.1..=0.9).contains(&lightness);
                        // Blending toward white or black keeps the hue but
                        // greys the tint, so saturation is held too.
                        if focused
                            && tinted
                            && (hue_gap(hue, raw_hue) > 0.05
                                || (saturation - raw_saturation).abs() > 0.05)
                        {
                            failures.push(format!(
                                "{name}: {kind:?} on {surface} moved hue {raw_hue:.3} -> {hue:.3}, \
                                 saturation {raw_saturation:.3} -> {saturation:.3}"
                            ));
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failing markers: {}",
            failures.len(),
            failures
                .iter()
                .take(12)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    /// Inactive tab titles, resting close glyphs and menu shortcut hints sit
    /// between primary and disabled text: AAA where the theme's foreground
    /// reaches it, never clearer than that foreground, never below AA.
    #[test]
    fn secondary_chrome_text_is_clearer_than_muted_and_quieter_than_primary() {
        let cfg = kettle_config::Config::default();
        let mut failures = Vec::new();
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let fg = theme.foreground;
            let strip = crate::resolve_chrome_bg(&cfg, &theme, None);
            let mut colors = Colors::default();
            let mut backgrounds = vec![
                ("inactive tab", strip),
                ("hovered inactive tab", tab_detail(strip, fg, false)),
                ("hovered strip control", strip_hover(strip, fg, false)),
                ("menu row", theme.background),
            ];
            // The renderer tints a selected menu row with the window's accent
            // (the config's resolved one unless a window picked its own), in
            // linear light on an sRGB surface and in sRGB on any other.
            for accent in [cfg.resolved_accent(&theme), theme.accent] {
                for linear in [false, true] {
                    backgrounds.push((
                        "selected menu row",
                        colors.surface(theme.background, accent, 0.18, linear),
                    ));
                }
            }
            for (role, bg) in backgrounds {
                let own = color::contrast_ratio(fg, bg);
                let primary = color::contrast_ratio(colors.text(fg, bg), bg);
                let muted = color::contrast_ratio(colors.muted(fg, bg), bg);
                let secondary = colors.secondary(fg, bg);
                let ratio = color::contrast_ratio(secondary, bg);
                let floor = SECONDARY_TEXT_CONTRAST.min(own).max(TEXT_CONTRAST);
                // Where the theme's text is under AA, muted and secondary are
                // both lifted onto the 4.5:1 floor and land within a rounding
                // step of it, either one a little higher.
                let quieter_than_muted = ratio + 0.05 < muted;
                if ratio + 1e-6 < floor || ratio > primary + 1e-6 || quieter_than_muted {
                    failures.push(format!(
                        "{name}: {role} {ratio:.3}:1 (theme {own:.3}:1, muted {muted:.3}:1)"
                    ));
                }
                // An unfocused window dims it, but not below the icon floor.
                let dimmed = color::contrast_ratio(colors.tab_text(secondary, bg, false), bg);
                if dimmed + 1e-6 < ICON_CONTRAST || dimmed > ratio + 1e-6 {
                    failures.push(format!("{name}: unfocused {role} {dimmed:.3}:1"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failing theme/roles: {}",
            failures.len(),
            failures
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
}

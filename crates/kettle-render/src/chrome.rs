use crate::{MinimumContrastCache, color, color::STEM_COVERAGE, dim_blend};
use kettle_config::Rgb;

// WCAG 2.2 AA: 1.4.3 (normal text) and 1.4.11 (non-text controls).
pub(crate) const TEXT_CONTRAST: f64 = 4.5;
pub(crate) const ICON_CONTRAST: f64 = 3.0;
// WCAG 2.2 AAA 1.4.6, for secondary chrome text people read while scanning:
// inactive tab titles, tab close glyphs and menu shortcut hints.
pub(crate) const SECONDARY_TEXT_CONTRAST: f64 = 7.0;

/// The jump button's colours, keyed by the theme colours and blend mode
/// they came from.
type PillColors = (([u32; 3], bool), (Rgb, Rgb));

/// The scrollbar's colours, keyed the same way.
type ScrollbarColorKey = ([u32; 4], bool);

#[derive(Default)]
pub(crate) struct Colors {
    cache: MinimumContrastCache,
    muted: std::collections::HashMap<(u32, u32), Rgb>,
    secondary: std::collections::HashMap<(u32, u32), Rgb>,
    surfaces: std::collections::HashMap<([u32; 2], u64, bool), Rgb>,
    completion: Option<([u32; 5], crate::CompletionPalette)>,
    inactive: std::collections::HashMap<([u32; 3], u32, bool), Rgb>,
    markers: std::collections::HashMap<([u32; 2], bool), Rgb>,
    backed: std::collections::HashMap<([u32; 3], u64, bool), Rgb>,
    pub(crate) pill: Option<PillColors>,
    pub(crate) scrollbar: Option<(ScrollbarColorKey, crate::scrollback::ScrollbarColors)>,
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

    /// The jump-to-bottom button's fill and label
    /// ([`crate::scrollback::scrollback_pill_colors`]), resolved once per
    /// theme rather than every frame.
    pub(crate) fn scrollback_pill(
        &mut self,
        theme: &kettle_config::Theme,
        linear: bool,
    ) -> (Rgb, Rgb) {
        let key = (
            [theme.palette[0], theme.background, theme.foreground]
                .map(MinimumContrastCache::color_key),
            linear,
        );
        if let Some((saved, colors)) = self.pill
            && saved == key
        {
            return colors;
        }
        let colors = crate::scrollback::scrollback_pill_colors(theme, linear);
        self.pill = Some((key, colors));
        colors
    }

    /// The scrollbar's colours
    /// ([`crate::scrollback::scrollbar_colors`]), resolved once per theme
    /// rather than every frame.
    pub(crate) fn scrollbar(
        &mut self,
        theme: &kettle_config::Theme,
        linear: bool,
    ) -> crate::scrollback::ScrollbarColors {
        let key = (
            [
                theme.foreground,
                theme.background,
                theme.palette[1],
                theme.palette[2],
            ]
            .map(MinimumContrastCache::color_key),
            linear,
        );
        if let Some((saved, colors)) = self.scrollbar
            && saved == key
        {
            return colors;
        }
        let colors = crate::scrollback::scrollbar_colors(theme, linear);
        self.scrollbar = Some((key, colors));
        colors
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

    /// A tab's title. An unfocused window fades it halfway toward the tab,
    /// but it still names the tab, so it keeps the text floor.
    pub(crate) fn tab_label(&mut self, fg: Rgb, bg: Rgb, focused: bool) -> Rgb {
        if focused {
            self.text(fg, bg)
        } else {
            self.resolve(color::over(fg, bg, 0.5, false), bg, TEXT_CONTRAST)
        }
    }

    /// `ink`, chosen against a surface's own colour, floored at `minimum`
    /// against everything the surface can show on screen. An ink already
    /// resolved against an opaque surface comes back unchanged, so only
    /// translucent surfaces move it.
    pub(crate) fn on(&mut self, ink: Rgb, backing: color::Backing, minimum: f64) -> Rgb {
        let key = (
            [ink, backing.over_black, backing.over_white].map(MinimumContrastCache::color_key),
            minimum.to_bits(),
            backing.translucent,
        );
        if let Some(color) = self.backed.get(&key) {
            return *color;
        }
        let color = color::with_min_contrast_on_backing(ink, backing, minimum);
        if self.backed.len() >= 4096 {
            self.backed.clear();
        }
        self.backed.insert(key, color);
        color
    }

    /// A tab's title on `surface`, its fill as painted, which `backing`
    /// shows on screen. `prominent` is the active tab, or any vertical tab.
    pub(crate) fn tab_title(
        &mut self,
        fg: Rgb,
        surface: Rgb,
        backing: color::Backing,
        prominent: bool,
        focused: bool,
    ) -> Rgb {
        let ink = if prominent {
            self.text(fg, surface)
        } else {
            self.secondary(fg, surface)
        };
        let ink = self.tab_label(ink, surface, focused);
        self.on(ink, backing, TEXT_CONTRAST)
    }

    /// A tab's Cmd+N shortcut hint, quieter than its title. It is for the
    /// keyboard in a focused window; an unfocused one does not paint it.
    pub(crate) fn tab_hint(&mut self, fg: Rgb, surface: Rgb, backing: color::Backing) -> Rgb {
        let ink = self.muted(fg, surface);
        self.on(ink, backing, TEXT_CONTRAST)
    }

    /// A tab's close glyph, `ink` on `surface`: text-strong in a focused
    /// window, an icon's floor in an unfocused one.
    pub(crate) fn tab_close(
        &mut self,
        ink: Rgb,
        surface: Rgb,
        backing: color::Backing,
        focused: bool,
    ) -> Rgb {
        let ink = self.tab_text(ink, surface, focused);
        self.on(
            ink,
            backing,
            if focused {
                TEXT_CONTRAST
            } else {
                ICON_CONTRAST
            },
        )
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
        // Resolve against the painted stem pixel (`STEM_COVERAGE`): light
        // and dark backgrounds need different reserves, and output rounding
        // matters too.
        let original_contrast = color::contrast_ratio(fg, bg);
        let floor = TEXT_CONTRAST.min(original_contrast);
        let covered =
            color::with_min_contrast_at_coverage(muted, background, floor, STEM_COVERAGE, linear);
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
/// it, so it is resolved once against the weaker of the two and reaches
/// `ICON_CONTRAST` on both whenever any colour can. Over some wallpapers none
/// can, and the bar then takes the best weaker contrast it can find, never
/// worse than its starting colour. A focused window keeps the accent's hue
/// (broadcast's yellow, a window's own accent); an unfocused one turns it a
/// neutral grey that is still the strongest mark in the strip, unlike the
/// hover fill it once matched. `active_bg` is the terminal's opaque colour,
/// the stated approximation for a translucent background.
pub(crate) fn tab_accent(
    raw: Rgb,
    strip: Rgb,
    active_bg: Rgb,
    foreground: Rgb,
    focused: bool,
) -> Rgb {
    let ink = if focused {
        raw
    } else {
        color::over(foreground, strip, 0.5, false)
    };
    color::with_min_contrast_on_both(ink, strip, active_bg, ICON_CONTRAST, focused)
}

/// How much more opaque than the window [`text_surface_alpha`] may make a
/// surface. More than this shows as a seam against the terminal around it.
pub(crate) const MAX_TEXT_SURFACE_RAISE: f64 = 0.1;

/// The opacity a translucent surface needs under chrome text. Behind a
/// translucent window lies anything from black to white, raised by `lift`,
/// and on a mid-tone theme no ink clears `TEXT_CONTRAST` over that whole span.
/// The surface keeps the window's opacity whenever some ink can. Otherwise,
/// when `may_raise`, it takes the least opacity at which one can, but at most
/// [`MAX_TEXT_SURFACE_RAISE`] more than the window's; past that, and over
/// Kettle's own wallpapers (`may_raise` false), the text is best effort.
pub(crate) fn text_surface_alpha(
    surface: Rgb,
    window_alpha: f64,
    lift: [f64; 3],
    may_raise: bool,
) -> f64 {
    let window_alpha = window_alpha.clamp(0.0, 1.0);
    let reachable = |alpha: f64| {
        let backing = color::Backing::translucent(surface, alpha, lift);
        [Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]
            .into_iter()
            .any(|ink| backing.weakest_contrast(ink) >= TEXT_CONTRAST)
    };
    if !may_raise || reachable(window_alpha) {
        return window_alpha;
    }
    let cap = (window_alpha + MAX_TEXT_SURFACE_RAISE).min(1.0);
    if !reachable(cap) {
        return cap;
    }
    let (mut lo, mut hi) = (window_alpha, cap);
    for _ in 0..16 {
        let mid = (lo + hi) / 2.0;
        if reachable(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
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
                    (
                        "picker key hint/placeholder/empty-state message",
                        colors.secondary(fg, bg),
                        bg,
                        TEXT_CONTRAST,
                    ),
                    (
                        "picker scrollbar thumb",
                        colors.icon(crate::dim_blend(fg, bg), bg),
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

    /// Over a wallpaper the strip is the theme's grey, black, white or the
    /// wallpaper's average, and can sit on the far side of the accent from
    /// the active tab. Contrast depends only on luminance, so the best any
    /// colour can do against both is white, black or the luminance between
    /// them with equal contrast to each; whenever that reaches 3:1 the bar
    /// does too, keeping a focused accent's hue, and when it cannot the bar
    /// is never worse than its starting colour. Resolving against one surface
    /// at a time left Adventure Time's broadcast bar at 1.25:1 on the active
    /// tab and made TokyoNight Night's `#5f5fff` worse than it was.
    #[test]
    fn tab_accent_reaches_3_to_1_over_wallpapers() {
        use kettle_config::{BackgroundType, ChromeBackground};
        // Configured `accent-color` values a theme does not choose.
        let arbitrary = [
            Rgb::new(0x5f, 0x5f, 0xff),
            Rgb::new(0xff, 0x00, 0x00),
            Rgb::new(0x00, 0xff, 0x00),
            Rgb::new(0x80, 0x80, 0x80),
            Rgb::new(0xff, 0xd7, 0x00),
            Rgb::new(0x1e, 0x90, 0xff),
            Rgb::new(0x22, 0x55, 0xaa),
            Rgb::new(0xaa, 0x33, 0x55),
        ];
        let weaker = |ink: Rgb, strip: Rgb, tab: Rgb| {
            color::contrast_ratio(ink, strip).min(color::contrast_ratio(ink, tab))
        };
        let best_possible = |strip: Rgb, tab: Rgb| {
            let (a, b) = (
                color::relative_luminance(strip) + 0.05,
                color::relative_luminance(tab) + 0.05,
            );
            let (dark, light) = (a.min(b), a.max(b));
            (1.05 / light).max(dark / 0.05).max((light / dark).sqrt())
        };
        let hue_gap = |a: f64, b: f64| {
            let d = (a - b).rem_euclid(6.0);
            d.min(6.0 - d)
        };
        // Wallpapers reduced to the average `chrome-background = auto` samples.
        let wallpaper = |pixels: &[Rgb]| {
            let rgba: Vec<u8> = pixels.iter().flat_map(|c| [c.r, c.g, c.b, 255]).collect();
            color::average_color(&rgba)
        };
        let levels = [0u8, 51, 102, 153, 204, 255];
        let mut averages = Vec::new();
        for r in levels {
            for g in levels {
                for b in levels {
                    averages.push(wallpaper(&[Rgb::new(r, g, b)]));
                }
            }
        }
        averages.push(wallpaper(&[Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)]));
        averages.push(wallpaper(&[Rgb::new(90, 60, 120), Rgb::new(20, 10, 40)]));
        averages.push(wallpaper(&[
            Rgb::new(250, 240, 200),
            Rgb::new(200, 220, 255),
        ]));

        let mut failures = Vec::new();
        let mut cases = 0usize;
        let mut check = |label: &str, theme: &kettle_config::Theme, strip: Rgb| {
            let tab = theme.background;
            let mut raws = vec![
                ("theme accent".to_string(), theme.accent),
                ("broadcast".to_string(), theme.palette[3]),
            ];
            raws.extend(arbitrary.map(|c| (format!("accent-color {c:?}"), c)));
            raws.extend(
                kettle_config::peacock_pool(theme)
                    .into_iter()
                    .map(|c| (format!("per-window accent {c:?}"), c)),
            );
            let reachable = best_possible(strip, tab) >= ICON_CONTRAST + 0.01;
            for (source, raw) in &raws {
                for focused in [true, false] {
                    cases += 1;
                    let start = if focused {
                        *raw
                    } else {
                        color::over(theme.foreground, strip, 0.5, false)
                    };
                    let bar = tab_accent(*raw, strip, tab, theme.foreground, focused);
                    let got = weaker(bar, strip, tab);
                    let start_ratio = weaker(start, strip, tab);
                    if reachable && got < ICON_CONTRAST - 1e-6 {
                        failures.push(format!("{label}: {source} (focused {focused}) {got:.2}:1"));
                    } else if got < start_ratio - 1e-6 {
                        failures.push(format!(
                            "{label}: {source} (focused {focused}) {got:.2}:1, \
                             worse than its start {start_ratio:.2}:1"
                        ));
                    }
                    let (raw_hue, raw_saturation, _) = color::hsl(*raw);
                    let (hue, saturation, lightness) = color::hsl(bar);
                    // Hue is defined only for a visible tint; below 32
                    // levels of chroma, 8-bit rounding alone moves it.
                    let chroma = bar.r.max(bar.g).max(bar.b) - bar.r.min(bar.g).min(bar.b);
                    let tinted = raw_saturation >= 0.25
                        && saturation >= 0.25
                        && chroma >= 32
                        && (0.1..=0.9).contains(&lightness);
                    if focused && tinted && hue_gap(hue, raw_hue) > 0.05 {
                        failures.push(format!(
                            "{label}: {source} moved hue {raw_hue:.3} -> {hue:.3} ({bar:?})"
                        ));
                    }
                }
            }
        };

        let names = kettle_config::Theme::list();
        let mut cfg = kettle_config::Config {
            background_type: BackgroundType::Image,
            ..kettle_config::Config::default()
        };
        for mode in [
            ChromeBackground::Theme,
            ChromeBackground::Black,
            ChromeBackground::White,
            ChromeBackground::Auto,
        ] {
            cfg.chrome_background = mode;
            for name in &names {
                let theme = kettle_config::Theme::by_name(name);
                // Every average for the five themes the review measured, and
                // a spread for the rest.
                let full = [
                    "TokyoNight Night",
                    "Kettle Day",
                    "Catppuccin Latte",
                    "Adventure Time",
                    "3024 Day",
                ]
                .contains(name);
                let picked: Vec<Rgb> = if full || mode != ChromeBackground::Auto {
                    averages.clone()
                } else {
                    averages.iter().copied().step_by(23).collect()
                };
                let mut strips: Vec<Rgb> = picked
                    .into_iter()
                    .map(|average| crate::resolve_chrome_bg(&cfg, &theme, Some(average)))
                    .collect();
                strips.sort_by_key(|c| (c.r, c.g, c.b));
                strips.dedup();
                for strip in strips {
                    check(&format!("{name} {mode:?} strip {strip:?}"), &theme, strip);
                }
            }
        }
        assert!(cases > 50_000, "the sweep ran {cases} cases");
        assert!(
            failures.is_empty(),
            "{} failing bars of {cases}: {}",
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

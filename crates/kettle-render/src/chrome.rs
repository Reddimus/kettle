use crate::{MinimumContrastCache, color, dim_blend};
use kettle_config::Rgb;

// WCAG 2.2 AA: 1.4.3 (normal text) and 1.4.11 (non-text controls).
pub(crate) const TEXT_CONTRAST: f64 = 4.5;
pub(crate) const ICON_CONTRAST: f64 = 3.0;

#[derive(Default)]
pub(crate) struct Colors {
    cache: MinimumContrastCache,
    muted: std::collections::HashMap<(u32, u32), Rgb>,
    surfaces: std::collections::HashMap<([u32; 2], u64, bool), Rgb>,
    completion: Option<([u32; 5], crate::CompletionPalette)>,
    inactive: std::collections::HashMap<([u32; 3], u32, bool), Rgb>,
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
        fg: Rgb,
        bg: Rgb,
        inactive_bg: Rgb,
        red: Rgb,
        state: (bool, bool),
        linear: bool,
    ) -> Rgb {
        let (active, hovered) = state;
        let base = if active { bg } else { inactive_bg };
        let (top, alpha) = if hovered {
            (red, 0.85)
        } else if active {
            (inactive_bg, 0.55)
        } else {
            (fg, 0.12)
        };
        self.surface(base, top, alpha, linear)
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
        let floor = TEXT_CONTRAST.min(color::contrast_ratio(fg, bg));
        let resolved = self.resolve(muted, background, floor);
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

#[cfg(test)]
fn close_background(
    fg: Rgb,
    bg: Rgb,
    inactive_bg: Rgb,
    red: Rgb,
    active: bool,
    hovered: bool,
    linear: bool,
) -> Rgb {
    Colors::default().close_background(fg, bg, inactive_bg, red, (active, hovered), linear)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_config::Theme;

    #[test]
    fn bundled_chrome_roles_meet_contrast_floors() {
        assert_eq!(TEXT_CONTRAST, 4.5);
        assert_eq!(ICON_CONTRAST, 3.0);
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
                        "menu information/empty state",
                        colors.muted(fg, bg),
                        bg,
                        TEXT_CONTRAST,
                    ),
                    (
                        "menu shortcut/type/check",
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
                        let close = close_background(
                            fg,
                            bg,
                            strip,
                            theme.palette[1],
                            active,
                            hovered,
                            linear,
                        );
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
        assert_eq!(colors.cache.misses, 3);
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

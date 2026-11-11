//! Horizontal tab content and chrome, shared by painting and UI geometry.
use crate::{Rect4, TabBar, batching::rect, labels::display_width, quad::QuadInstance};
use kettle_config::{Rgb, TabBarPos};

/// Fit chrome text into a fixed titlebar band even when terminal text is zoomed.
pub fn tab_text_scale(band_height: f32, line_height: f32) -> f32 {
    (band_height.max(1.0) / line_height.max(1.0)).min(1.0)
}

/// Tab text metrics grown by `scale` for a control glyph in a `(width,
/// height)` button. The em stops at `CONTROL_GLYPH_FILL` of the button's
/// shorter side and the line at its height, so the glyph stays centred and
/// unclipped even in a side bar's half-width buttons; it is never drawn
/// smaller than the tab text.
pub(crate) fn control_glyph_metrics(
    text: glyphon::Metrics,
    scale: f32,
    button: (f32, f32),
) -> glyphon::Metrics {
    let (width, height) = button;
    let font_size = (text.font_size * scale)
        .min(width.min(height) * CONTROL_GLYPH_FILL)
        .max(text.font_size);
    let line_height =
        (text.line_height * font_size / text.font_size.max(1.0)).min(height.max(text.line_height));
    glyphon::Metrics::new(font_size, line_height)
}

/// Largest control-glyph em, as a share of its button's shorter side.
const CONTROL_GLYPH_FILL: f32 = 0.9;

pub struct HorizontalTabContent {
    pub title: Rect4,
    pub close: Rect4,
    pub hint: Option<Rect4>,
}

/// Hidden controls reserve no hint or close space; title and activity take priority.
pub fn horizontal_tab_content_layout(
    segment: Rect4,
    show_controls: bool,
    close_enabled: bool,
    activity: bool,
    label: &str,
    hint: Option<&str>,
    cell_width: f32,
) -> HorizontalTabContent {
    let (x, y, w, h) = segment;
    let close_width = if show_controls && close_enabled && w >= h {
        h
    } else {
        0.0
    };
    let close = (
        x + w - close_width,
        y,
        close_width,
        if close_width > 0.0 { h } else { 0.0 },
    );
    let mut right = (if close_width > 0.0 {
        close.0
    } else {
        x + w - 8.0
    })
    .max(x);
    let left = (x + 8.0 + if activity { 12.0 } else { 0.0 }).min(right);
    let cw = cell_width.max(1.0);
    let hint = hint.filter(|_| show_controls).and_then(|hint| {
        let width = display_width(hint) as f32 * cw;
        let title_width = display_width(label) as f32 * cw;
        if title_width + cw + width <= right - left {
            let lane = (right - width, y, width, h);
            right -= width + cw;
            Some(lane)
        } else {
            None
        }
    });
    HorizontalTabContent {
        title: (left, y, right - left, h),
        close,
        hint,
    }
}

/// Anchor to the actual control while keeping tooltips outside the header.
pub(crate) fn tooltip_anchor(bar: &TabBar) -> Rect4 {
    bar.window_controls
        .iter()
        .find(|b| b.hovered && !b.pressed)
        .map_or(bar.band, |b| (b.rect.0, bar.band.1, b.rect.2, bar.band.3))
}

/// Keep a tooltip inside the window, below top chrome or above bottom chrome.
pub(crate) fn tooltip_rect(
    band: Rect4,
    surface: (f32, f32),
    text: (f32, f32),
    scale: f32,
) -> Rect4 {
    let w = (text.0 + 16.0 * scale).min(surface.0);
    let h = (text.1 + 8.0 * scale).min(surface.1);
    let x = (band.0 + band.2 - w - 8.0 * scale).clamp(0.0, (surface.0 - w).max(0.0));
    let y = if band.1 < surface.1 * 0.5 {
        band.1 + band.3 + 4.0 * scale
    } else {
        band.1 - h - 4.0 * scale
    };
    (x, y.clamp(0.0, (surface.1 - h).max(0.0)), w, h)
}

pub(crate) fn tab_label_origin(lane: Rect4, label: &str, cell: (f32, f32)) -> (f32, f32) {
    crate::geometry::centered_text_origin(
        lane,
        (display_width(label) as f32 * cell.0.max(1.0), cell.1),
    )
}

/// Rounded silhouettes in the existing ordered quad layer. Only the corner
/// bands need slicing; the terminal-facing edge remains square and flush.
pub(crate) fn rounded_fill(
    quads: &mut Vec<QuadInstance>,
    bounds: Rect4,
    radius: f32,
    corners: (bool, bool),
    fill: (Rgb, f32),
    accent: Option<(Rgb, f32, bool)>,
) {
    let (x, y, w, h) = bounds;
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let radius = radius.clamp(0.0, (w * 0.5).min(h * 0.5));
    let rows = radius.ceil().max(1.0) as usize;
    let band = radius / rows as f32;
    let stripe = |q: &mut Vec<QuadInstance>, bx, by, bw, bh| {
        q.push(rect(bx, by, bw, bh, fill.0, fill.1));
        if let Some((color, thickness, bottom)) = accent {
            let a = if bottom {
                (y + h - thickness).max(by)
            } else {
                by
            };
            let b = if bottom {
                by + bh
            } else {
                (y + thickness).min(by + bh)
            };
            if b > a {
                q.push(rect(bx, a, bw, b - a, color, 1.0));
            }
        }
    };
    let top = if corners.0 { radius } else { 0.0 };
    let bottom = if corners.1 { radius } else { 0.0 };
    stripe(quads, x, y + top, w, h - top - bottom);
    for i in 0..rows {
        let dy = i as f32 * band;
        let d = radius - dy - band * 0.5;
        let inset = radius - (radius * radius - d * d).max(0.0).sqrt();
        if corners.0 {
            stripe(quads, x + inset, y + dy, w - inset * 2.0, band);
        }
        if corners.1 {
            stripe(quads, x + inset, y + h - dy - band, w - inset * 2.0, band);
        }
    }
}

pub(crate) struct HorizontalTabQuads {
    pub chrome: Vec<QuadInstance>,
    /// Translucent tab backgrounds, including their opaque accent line.
    /// Use the terminal's replacing base layer unless a wallpaper is present.
    pub surface: Vec<QuadInstance>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn horizontal_tab_quads(
    bar: &TabBar,
    band: Rect4,
    theme: &kettle_config::Theme,
    background: (Rgb, f32),
    chrome: Rgb,
    accent: Rgb,
    pos: TabBarPos,
    scale: f32,
) -> HorizontalTabQuads {
    let mut quads = Vec::new();
    let mut band = vec![rect(band.0, band.1, band.2, band.3, chrome, 1.0)];
    let mut surface = Vec::new();
    let bottom = pos == TabBarPos::Bottom;
    let hover = crate::chrome::tab_detail(chrome, theme.foreground, false);
    let divider = hover;
    for (i, tab) in bar.segments.iter().enumerate() {
        let hovered = bar.hovered_tab_idx == Some(tab.idx);
        if tab.active || hovered {
            let start = quads.len();
            rounded_fill(
                &mut quads,
                tab.rect,
                6.0 * scale,
                (!bottom, bottom),
                if tab.active { background } else { (hover, 1.0) },
                tab.active.then_some((accent, 2.0, bottom)),
            );
            if tab.active && background.1 < 1.0 {
                // Cut with the actual surface slices so the rounded corners'
                // outside stays chrome without painting beneath the tab.
                for quad in quads.drain(start..) {
                    crate::quad::exclude_rect(
                        &mut band,
                        (quad.pos[0], quad.pos[1], quad.size[0], quad.size[1]),
                    );
                    surface.push(quad);
                }
            }
        }
        if !tab.active
            && !hovered
            && let Some(next) = bar.segments.get(i + 1)
            && !next.active
            && bar.hovered_tab_idx != Some(next.idx)
            && (tab.rect.0 + tab.rect.2 - next.rect.0).abs() < 1.0
        {
            let (x, y, w, h) = tab.rect;
            quads.push(rect(
                x + w - 1.0,
                y + 6.0,
                1.0,
                (h - 12.0).max(0.0),
                divider,
                1.0,
            ));
        }
    }
    band.extend(quads);
    HorizontalTabQuads {
        chrome: band,
        surface,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TabActivity;

    #[test]
    fn fixed_titlebar_text_fits_at_large_terminal_zoom() {
        for scale in [1.0, 2.0] {
            for height in [12.0, 28.0, 48.0, 96.0] {
                let line = height * scale;
                let factor = tab_text_scale(28.0 * scale, line);
                assert!(factor > 0.0 && factor <= 1.0);
                assert!(line * factor <= 28.0 * scale);
                assert_eq!(factor, if height <= 28.0 { 1.0 } else { 28.0 / height });
            }
        }
    }

    #[test]
    fn control_glyphs_grow_past_tab_text_but_fit_their_button() {
        for font_size in [10.0_f32, 13.0, 20.0, 40.0] {
            let text = glyphon::Metrics::new(font_size, font_size * 1.25);
            // Square strip buttons, GNOME's header and a 40 px side bar's
            // half-width `▾` and `+`.
            for (width, height) in [
                (20.0_f32, 20.0_f32),
                (28.0, 28.0),
                (46.0, 46.0),
                (20.0, 45.5),
            ] {
                let side = width.min(height);
                for scale in [1.5, 2.0] {
                    let glyph = control_glyph_metrics(text, scale, (width, height));
                    assert!(glyph.font_size >= text.font_size);
                    assert!(glyph.font_size <= (text.font_size * scale).max(text.font_size));
                    assert!(glyph.line_height <= height.max(text.line_height));
                    if text.font_size * scale <= side * 0.9 {
                        assert_eq!(glyph.font_size, text.font_size * scale);
                    } else {
                        assert!(glyph.font_size <= (side * 0.9).max(text.font_size));
                    }
                }
            }
        }
    }

    #[test]
    fn horizontal_single_tab_controls_render_from_shared_geometry() {
        let source = crate::production_source();
        let close = source
            .split_once("if s.close.2 <= 0.0")
            .unwrap()
            .1
            .split_once("let (cx, cy, ccw, cch) = s.close;")
            .unwrap()
            .0;
        assert!(!close.contains("s.active"));
        assert!(!close.contains("hovered_tab_idx"));
    }

    #[test]
    fn horizontal_controls_keep_strip_text_metrics_and_contrast() {
        let source = crate::production_source();
        // The close ×, + and ▾ are small shapes, so they draw larger than tab
        // text (TAB_CLOSE_GLYPH_SCALE, TAB_CONTROL_GLYPH_SCALE).
        for (buffer, metrics) in [
            ("tabbar_buffer", "control_metrics"),
            ("new_tab_arrow_buffer", "control_metrics"),
            ("tab_close_buffer", "close_metrics"),
        ] {
            assert!(source.contains(&format!("self.{buffer}.set_metrics({metrics});")));
            assert!(source.contains(&format!("buffer: &self.{buffer},")));
        }
        for statement in [
            "let plus_fg = self.chrome_colors.tab_text(plus_fg, background, overlay.window_focused);",
            "let arrow_fg = self.chrome_colors.tab_text(arrow_fg, background, overlay.window_focused);",
            "let close_fg = self.chrome_colors.tab_text(close_fg, close_background, overlay.window_focused);",
            "let hint_fg = self.chrome_colors.muted(fg, tab_background);",
        ] {
            assert!(
                source
                    .split_whitespace()
                    .collect::<String>()
                    .contains(&statement.split_whitespace().collect::<String>()),
                "missing contrast resolution: {statement}"
            );
        }
    }

    #[test]
    fn horizontal_single_tab_controls_follow_hover_and_tab_count() {
        for count in [1, 2] {
            for hovered in [false, true] {
                let visible = count > 1 || hovered;
                let content = horizontal_tab_content_layout(
                    (0.0, 0.0, 320.0, 28.0),
                    visible,
                    true,
                    false,
                    "shell",
                    Some("Cmd+1"),
                    8.0,
                );
                assert_eq!(
                    content.hint.is_some(),
                    visible,
                    "count={count}, hover={hovered}"
                );
                assert_eq!(content.close.2 > 0.0, visible);
                if !visible {
                    assert_eq!(content.title, (8.0, 0.0, 304.0, 28.0));
                    assert_eq!(
                        tab_label_origin(content.title, "shell", (8.0, 16.0)),
                        (140.0, 6.0)
                    );
                }
            }
        }
    }

    #[test]
    fn horizontal_content_places_controls_only_when_visible() {
        for width in [24.0, 40.0, 80.0, 160.0, 320.0] {
            for active in [false, true] {
                for hovered in [false, true] {
                    for enabled in [false, true] {
                        let c = horizontal_tab_content_layout(
                            (10.0, 20.0, width, 28.0),
                            active || hovered,
                            enabled,
                            false,
                            "shell",
                            Some("Cmd+1"),
                            8.0,
                        );
                        let close = enabled && (active || hovered) && width >= 28.0;
                        assert_eq!(c.close.2 > 0.0, close);
                        assert!(c.title.0 >= 10.0 && c.title.0 + c.title.2 <= 10.0 + width);
                        if close {
                            assert!(c.title.0 + c.title.2 <= c.close.0);
                        }
                        let available = width - 8.0 - if close { 28.0 } else { 8.0 };
                        assert_eq!(c.hint.is_some(), (active || hovered) && available >= 88.0);
                        if let Some(hint) = c.hint {
                            assert!(c.title.0 + c.title.2 + 8.0 <= hint.0);
                            assert_eq!(
                                hint.0 + hint.2,
                                if close { c.close.0 } else { 10.0 + width - 8.0 }
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn horizontal_content_prioritizes_title_and_activity_over_hint() {
        for width in [80.0, 160.0, 320.0] {
            let full = horizontal_tab_content_layout(
                (0.0, 0.0, width, 28.0),
                true,
                true,
                false,
                "shell",
                None,
                8.0,
            );
            let dot = horizontal_tab_content_layout(
                (0.0, 0.0, width, 28.0),
                true,
                true,
                true,
                "a very long title that needs an ellipsis",
                Some("Cmd+1"),
                8.0,
            );
            assert_eq!(dot.title.0, full.title.0 + 12.0);
            assert!(dot.hint.is_none());
            assert_eq!(dot.close, full.close);
        }
    }

    #[test]
    fn horizontal_narrow_activity_lane_stays_before_close() {
        for width in [0.0, 8.0, 24.0, 28.0, 30.0, 40.0] {
            let content = horizontal_tab_content_layout(
                (0.0, 0.0, width, 28.0),
                true,
                true,
                true,
                "shell",
                Some("Cmd+1"),
                8.0,
            );
            assert!(content.title.2 >= 0.0);
            if content.close.2 > 0.0 {
                assert!(content.title.0 + content.title.2 <= content.close.0);
            }
        }
    }

    #[test]
    fn horizontal_labels_center_and_ellipsize_without_overlapping_controls() {
        for label in [
            "shell",
            "Configuración de la terminal",
            "/home/user/projects/flight-event-line-server-go",
        ] {
            for width in [40.0, 80.0, 160.0, 320.0, 640.0] {
                let content = horizontal_tab_content_layout(
                    (0.0, 0.0, width, 28.0),
                    true,
                    true,
                    false,
                    label,
                    Some("Cmd+1"),
                    8.0,
                );
                let fitted =
                    crate::fit_tab_segment_title(label, None, 0, "{title}", content.title.2, 8.0);
                let (x, y) = tab_label_origin(content.title, &fitted, (8.0, 16.0));
                let ink_width = display_width(&fitted) as f32 * 8.0;
                assert!(x >= content.title.0);
                assert!(x + ink_width <= content.title.0 + content.title.2);
                assert_eq!(y, 6.0);
                assert!((x - content.title.0 - (content.title.2 - ink_width) * 0.5).abs() < 0.01);
                if display_width(label) as f32 * 8.0 <= content.title.2 {
                    assert_eq!(fitted, label);
                }
            }
        }
        let fits = horizontal_tab_content_layout(
            (0.0, 0.0, 124.0, 28.0),
            true,
            true,
            false,
            "shell",
            Some("Cmd+1"),
            8.0,
        );
        assert_eq!(fits.hint, Some((56.0, 0.0, 40.0, 28.0)));
        let narrow = horizontal_tab_content_layout(
            (0.0, 0.0, 123.5, 28.0),
            true,
            true,
            false,
            "shell",
            Some("Cmd+1"),
            8.0,
        );
        assert!(narrow.hint.is_none());
    }

    fn sample(quads: &[QuadInstance], x: f32, y: f32) -> Option<[f32; 4]> {
        quads
            .iter()
            .rev()
            .find(|q| {
                x >= q.pos[0]
                    && x < q.pos[0] + q.size[0]
                    && y >= q.pos[1]
                    && y < q.pos[1] + q.size[1]
            })
            .map(|q| q.color)
    }

    #[test]
    fn horizontal_translucent_active_surface_matches_terminal_alpha() {
        use kettle_config::{BackgroundType, Config, Theme};

        for name in ["TokyoNight Night", "TokyoNight Day"] {
            let theme = Theme::by_name(name);
            assert_eq!(theme.is_dark(), name == "TokyoNight Night");
            for pos in [TabBarPos::Top, TabBarPos::Bottom] {
                for scale in [1.0, 1.5, 2.0] {
                    let mut bar = TabBar::hidden();
                    let bounds = (78.0 * scale, 20.0, 160.0 * scale, 28.0 * scale);
                    bar.segments.push(crate::TabSeg {
                        idx: 0,
                        rect: bounds,
                        title_rect: bounds,
                        hint: None,
                        close: (0.0, 0.0, 0.0, 0.0),
                        title: String::new(),
                        path: None,
                        active: true,
                        activity: TabActivity::Normal,
                    });
                    for (background_type, opacity, darkness) in [
                        (BackgroundType::Solid, 1.0, 0.5),
                        (BackgroundType::Solid, 0.86, 0.5),
                        (BackgroundType::Transparent, 0.86, 0.5),
                        (BackgroundType::Solid, 0.0, 0.5),
                    ] {
                        let cfg = Config {
                            background_type,
                            background_opacity: opacity,
                            background_darkness: darkness,
                            ..Config::default()
                        };
                        let alpha = crate::composed_bg_alpha(&cfg) as f32;
                        let chrome = theme.palette[8];
                        let band = (0.0, 20.0, 400.0 * scale, 28.0 * scale);
                        let quads = horizontal_tab_quads(
                            &bar,
                            band,
                            &theme,
                            (theme.background, alpha),
                            chrome,
                            theme.accent,
                            pos,
                            scale,
                        );
                        let (x, y, w, h) = bounds;
                        let middle = (x + w * 0.5, y + h * 0.5);
                        let expected = rect(0.0, 0.0, 1.0, 1.0, theme.background, alpha).color;
                        if alpha == 1.0 {
                            assert!(quads.surface.is_empty());
                            assert_eq!(quads.chrome[0].pos, [band.0, band.1]);
                            assert_eq!(quads.chrome[0].size, [band.2, band.3]);
                            assert!(quads.chrome.iter().all(|q| q.color[3] == 1.0));
                            assert_eq!(sample(&quads.chrome, middle.0, middle.1), Some(expected));
                        } else {
                            assert!(sample(&quads.chrome, middle.0, middle.1).is_none());
                            assert_eq!(sample(&quads.surface, middle.0, middle.1), Some(expected));
                            // Every surface pixel has a chrome hole; outside the rounded
                            // silhouette the band still covers the titlebar and strip.
                            for py in y as u32..(y + h) as u32 {
                                for px in 0..band.2 as u32 {
                                    let (px, py) = (px as f32 + 0.5, py as f32 + 0.5);
                                    assert_ne!(
                                        sample(&quads.surface, px, py).is_some(),
                                        sample(&quads.chrome, px, py).is_some(),
                                        "{name}, {pos:?}, alpha={alpha}, scale={scale}, {px},{py}"
                                    );
                                }
                            }
                        }
                        let edge = if pos == TabBarPos::Top {
                            y + 0.5
                        } else {
                            y + h - 0.5
                        };
                        let accent_layer = if alpha == 1.0 {
                            &quads.chrome
                        } else {
                            &quads.surface
                        };
                        assert_eq!(
                            sample(accent_layer, middle.0, edge),
                            Some(rect(0.0, 0.0, 1.0, 1.0, theme.accent, 1.0).color)
                        );
                        assert_eq!(
                            sample(&quads.chrome, x + 0.1, edge),
                            Some(rect(0.0, 0.0, 1.0, 1.0, chrome, 1.0).color)
                        );
                        assert_eq!(
                            sample(&quads.chrome, 10.0, middle.1),
                            Some(rect(0.0, 0.0, 1.0, 1.0, chrome, 1.0).color)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn inactive_tab_separator_contrasts_with_resolved_chrome() {
        use kettle_config::{BackgroundType, ChromeBackground, Config, Theme};

        for name in ["Builtin Dark", "Builtin Light"] {
            let theme = Theme::by_name(name);
            let mut bar = TabBar::hidden();
            for i in 0..3 {
                bar.segments.push(crate::TabSeg {
                    idx: i,
                    rect: (i as f32 * 160.0, 0.0, 160.0, 28.0),
                    title_rect: (i as f32 * 160.0 + 8.0, 0.0, 144.0, 28.0),
                    hint: None,
                    close: (0.0, 0.0, 0.0, 0.0),
                    title: "shell".into(),
                    path: None,
                    active: i == 0,
                    activity: TabActivity::Normal,
                });
            }
            for background_type in [BackgroundType::Solid, BackgroundType::Image] {
                for chrome_background in [
                    ChromeBackground::Theme,
                    ChromeBackground::Black,
                    ChromeBackground::White,
                    ChromeBackground::Auto,
                ] {
                    let cfg = Config {
                        background_type,
                        chrome_background,
                        ..Config::default()
                    };
                    let chrome = crate::resolve_chrome_bg(&cfg, &theme, Some(theme.foreground));
                    for pos in [TabBarPos::Top, TabBarPos::Bottom] {
                        let quads = horizontal_tab_quads(
                            &bar,
                            (0.0, 0.0, 480.0, 28.0),
                            &theme,
                            (theme.background, 1.0),
                            chrome,
                            theme.accent,
                            pos,
                            1.0,
                        );
                        let divider = quads
                            .chrome
                            .iter()
                            .find(|q| q.pos[0] == 319.0 && q.size[0] == 1.0)
                            .expect("separator between the two inactive tabs");
                        let rgb = Rgb::new(
                            (divider.color[0] * 255.0).round() as u8,
                            (divider.color[1] * 255.0).round() as u8,
                            (divider.color[2] * 255.0).round() as u8,
                        );
                        assert!(
                            crate::color::contrast_ratio(rgb, chrome) >= 1.5,
                            "{name}, {background_type:?}, {chrome_background:?}, {pos:?}: invisible separator"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn shortcut_hint_fits_natural_advance_with_narrow_terminal_cells() {
        use crate::{Attrs, Family, FontSystem, Metrics, Shaping, TextBuffer};

        let mut fonts = FontSystem::new();
        for face in kettle_config::font::all() {
            crate::load_bundled_font(&mut fonts, face);
        }
        let metrics = Metrics::new(16.0, 20.0);
        let mut buffer = TextBuffer::new(&mut fonts, metrics);
        let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
        buffer.set_text("M", &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut fonts, false);
        let natural_advance = buffer.layout_runs().next().unwrap().line_w;
        for cell_scale in [0.5, 1.0, 2.0] {
            let terminal_cell_width = natural_advance * cell_scale;
            let text_advance = terminal_cell_width / cell_scale;
            let hint = "⌘1".to_string();
            let content = horizontal_tab_content_layout(
                (0.0, 0.0, 320.0, 32.0),
                true,
                true,
                true,
                "shell",
                Some(&hint),
                text_advance,
            );
            let lane = content.hint.expect("a wide tab has room for Cmd+1");
            assert!((lane.2 - 5.0 * natural_advance).abs() < 0.001);
            buffer.set_size(Some(lane.2), Some(lane.3));
            buffer.set_text(&hint, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut fonts, false);
            let runs: Vec<_> = buffer.layout_runs().collect();
            assert_eq!(
                runs.len(),
                1,
                "Cmd+1 must not wrap at cell-width={cell_scale}"
            );
            assert_eq!(
                runs[0].glyphs.len(),
                5,
                "all hint glyphs must remain visible"
            );
            assert!(runs[0].line_w <= lane.2 + 0.001);
            assert!(content.title.0 + content.title.2 < lane.0);
            assert_eq!(lane.0 + lane.2, content.close.0);
        }
    }

    #[test]
    fn horizontal_active_surface_joins_terminal_and_inactive_has_no_fill() {
        for name in ["Builtin Dark", "Builtin Light"] {
            let theme = kettle_config::Theme::by_name(name);
            for pos in [TabBarPos::Top, TabBarPos::Bottom] {
                let mut bar = TabBar::hidden();
                for i in 0..3 {
                    bar.segments.push(crate::TabSeg {
                        idx: i,
                        rect: (i as f32 * 160.0, 0.0, 160.0, 28.0),
                        title_rect: (8.0, 0.0, 100.0, 28.0),
                        hint: None,
                        close: (0.0, 0.0, 0.0, 0.0),
                        title: "shell".into(),
                        path: None,
                        active: i == 0,
                        activity: TabActivity::Normal,
                    });
                }
                let accent = theme.palette[4];
                let q = horizontal_tab_quads(
                    &bar,
                    (0.0, 0.0, 480.0, 28.0),
                    &theme,
                    (theme.background, 1.0),
                    theme.palette[0],
                    accent,
                    pos,
                    1.0,
                );
                let q = &q.chrome[1..];
                assert_eq!(
                    sample(q, 80.0, 14.0),
                    Some(rect(0.0, 0.0, 1.0, 1.0, theme.background, 1.0).color)
                );
                let edge = if pos == TabBarPos::Top { 0.5 } else { 27.5 };
                assert_eq!(
                    sample(q, 80.0, edge),
                    Some(rect(0.0, 0.0, 1.0, 1.0, accent, 1.0).color)
                );
                assert!(sample(q, 0.1, edge).is_none());
                assert!(sample(q, 0.1, 28.0 - edge).is_some());
                assert!(sample(q, 240.0, 14.0).is_none());
                assert!(sample(q, 159.5, 14.0).is_some());
                assert!(sample(q, 319.5, 14.0).is_some());
                bar.hovered_tab_idx = Some(1);
                let hover = horizontal_tab_quads(
                    &bar,
                    (0.0, 0.0, 480.0, 28.0),
                    &theme,
                    (theme.background, 1.0),
                    theme.palette[0],
                    accent,
                    pos,
                    1.0,
                );
                let hover = &hover.chrome[1..];
                assert!(sample(hover, 240.0, 14.0).is_some());
                // Separator at the hovered tab's trailing edge disappears.
                assert!(!hover.iter().any(|q| q.pos[0] == 319.0 && q.size[0] == 1.0));
            }
        }
    }
}

#[cfg(test)]
mod tooltip_tests {
    use super::*;

    #[test]
    fn tooltip_stays_inside_narrow_top_and_bottom_windows() {
        for scale in [1.0, 2.0] {
            for bottom in [false, true] {
                let band = (
                    78.0 * scale,
                    if bottom { 232.0 * scale } else { 0.0 },
                    342.0 * scale,
                    28.0 * scale,
                );
                let (x, y, w, h) = tooltip_rect(
                    band,
                    (420.0 * scale, 260.0 * scale),
                    (220.0 * scale, 20.0 * scale),
                    scale,
                );
                assert!(x >= 0.0 && x + w <= 420.0 * scale);
                assert!(y >= 0.0 && y + h <= 260.0 * scale);
                if bottom {
                    assert!(y + h < band.1);
                } else {
                    assert!(y > band.1 + band.3);
                }
            }
        }
    }
}

/// Logical side of a GNOME header button's minimize/maximize/close glyph.
pub(crate) const WINDOW_CONTROL_GLYPH: f32 = 12.0;

#[allow(clippy::too_many_arguments)]
pub(crate) fn window_control_quads(
    quads: &mut Vec<QuadInstance>,
    bar: &TabBar,
    fg: Rgb,
    bg: Rgb,
    scale: f32,
    focused: bool,
    colors: &mut crate::chrome::Colors,
) {
    use crate::WindowControl as C;
    for button in &bar.window_controls {
        let (x, y, w, h) = button.rect;
        if w <= 0.0 || h <= 0.0 {
            continue;
        }
        let diameter = (28.0 * scale).min(w).min(h);
        let (fill, glyph) =
            window_control_colors(fg, bg, button.hovered, button.pressed, focused, colors);
        if button.hovered {
            rounded_fill(
                quads,
                button.rect,
                6.0 * scale,
                (true, true),
                (fill, 1.0),
                None,
            );
        }
        if diameter < 12.0 * scale {
            continue;
        }
        let cx = x + w * 0.5;
        let cy = y + h * 0.5;
        let size = (WINDOW_CONTROL_GLYPH * scale).min(diameter * 0.5);
        let stroke = (1.5 * scale).max(1.0);
        let left = cx - size * 0.5;
        let top = cy - size * 0.5;
        let mut outline = |left: f32, top: f32, size: f32| {
            for r in [
                (left, top, size, stroke),
                (left, top + size - stroke, size, stroke),
                (left, top, stroke, size),
                (left + size - stroke, top, stroke, size),
            ] {
                quads.push(rect(r.0, r.1, r.2, r.3, glyph, 1.0));
            }
        };
        match button.action {
            C::Minimize => quads.push(rect(left, cy, size, stroke, glyph, 1.0)),
            C::Maximize => outline(left, top, size),
            C::Restore => {
                outline(left + 2.0 * scale, top, size - 2.0 * scale);
                outline(left, top + 2.0 * scale, size - 2.0 * scale);
            }
            C::Close => {
                let steps = size.ceil().max(1.0) as usize;
                for i in 0..=steps {
                    let offset = size * i as f32 / steps as f32;
                    for y in [top + offset, top + size - offset] {
                        quads.push(rect(
                            left + offset - stroke * 0.5,
                            y - stroke * 0.5,
                            stroke,
                            stroke,
                            glyph,
                            1.0,
                        ));
                    }
                }
            }
        }
    }
}

fn window_control_colors(
    fg: Rgb,
    bg: Rgb,
    hovered: bool,
    pressed: bool,
    focused: bool,
    colors: &mut crate::chrome::Colors,
) -> (Rgb, Rgb) {
    let fill = if hovered {
        crate::chrome::tab_detail(bg, fg, pressed)
    } else {
        bg
    };
    (fill, colors.tab_text(fg, fill, focused))
}

#[cfg(test)]
mod window_control_tests {
    use super::*;
    use crate::{WindowControl as C, WindowControlButton};

    #[test]
    fn linux_header_button_tooltips_follow_left_and_right_controls() {
        for scale in [1.0, 1.5, 2.0] {
            let mut bar = TabBar::hidden();
            bar.band = (0.0, 0.0, 800.0 * scale, 46.0 * scale);
            for x in [9.0, 763.0] {
                bar.window_controls = vec![WindowControlButton {
                    action: C::Close,
                    rect: (x * scale, 9.0 * scale, 28.0 * scale, 28.0 * scale),
                    label: String::new(),
                    hovered: true,
                    pressed: false,
                }];
                let anchor = tooltip_anchor(&bar);
                assert_eq!(anchor.0, x * scale);
                let bounds = tooltip_rect(
                    anchor,
                    (800.0 * scale, 600.0 * scale),
                    (160.0 * scale, 20.0 * scale),
                    scale,
                );
                assert_eq!(bounds.1, 50.0 * scale);
                assert!(bounds.0 >= 0.0 && bounds.0 + bounds.2 <= 800.0 * scale);
                if x < 400.0 {
                    assert_eq!(bounds.0, 0.0);
                }
                bar.window_controls[0].pressed = true;
                assert_eq!(tooltip_anchor(&bar), bar.band);
                bar.window_controls[0].hovered = false;
                assert_eq!(tooltip_anchor(&bar), bar.band);
            }
        }
    }

    #[test]
    fn linux_header_buttons_use_current_chrome_contrast_hover_and_dimming() {
        for name in kettle_config::Theme::list() {
            let theme = kettle_config::Theme::by_name(name);
            let mut colors = crate::chrome::Colors::default();
            let bg = crate::resolve_chrome_bg(&Default::default(), &theme, None);
            for focused in [false, true] {
                for hovered in [false, true] {
                    for pressed in [false, true] {
                        let (fill, glyph) = window_control_colors(
                            theme.foreground,
                            bg,
                            hovered,
                            pressed,
                            focused,
                            &mut colors,
                        );
                        assert_eq!(
                            fill,
                            if hovered {
                                crate::chrome::tab_detail(bg, theme.foreground, pressed)
                            } else {
                                bg
                            }
                        );
                        assert_eq!(glyph, colors.tab_text(theme.foreground, fill, focused));
                        let floor = if focused { 4.5 } else { 3.0 };
                        assert!(
                            crate::color::contrast_ratio(glyph, fill) >= floor - 0.01,
                            "{name}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn linux_header_button_shapes_follow_shared_bounds_at_fractional_scales() {
        for scale in [1.0, 1.5, 2.0] {
            for action in [C::Minimize, C::Maximize, C::Restore, C::Close] {
                for hovered in [false, true] {
                    let mut bar = TabBar::hidden();
                    let target = (756.0 * scale, 9.0 * scale, 28.0 * scale, 28.0 * scale);
                    bar.window_controls = vec![WindowControlButton {
                        action,
                        rect: target,
                        label: String::new(),
                        hovered,
                        pressed: hovered,
                    }];
                    let mut quads = Vec::new();
                    window_control_quads(
                        &mut quads,
                        &bar,
                        Rgb::new(255, 255, 255),
                        Rgb::new(0, 0, 0),
                        scale,
                        true,
                        &mut Default::default(),
                    );
                    assert!(!quads.is_empty());
                    if !hovered {
                        // Every quad is glyph ink: it spans at least 12
                        // logical pixels (10 read as tiny in the header).
                        let left = quads.iter().map(|q| q.pos[0]).fold(f32::MAX, f32::min);
                        let right = quads
                            .iter()
                            .map(|q| q.pos[0] + q.size[0])
                            .fold(f32::MIN, f32::max);
                        assert!(
                            right - left >= 12.0 * scale - 0.01,
                            "{action:?} at {scale}x spans {}",
                            right - left
                        );
                    }
                    for q in quads {
                        assert!(
                            q.pos[0] >= target.0
                                && q.pos[0] + q.size[0] <= target.0 + target.2 + 0.01
                        );
                        assert!(
                            q.pos[1] >= target.1
                                && q.pos[1] + q.size[1] <= target.1 + target.3 + 0.01
                        );
                    }
                }
            }
        }
    }
}

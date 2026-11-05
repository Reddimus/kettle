//! Horizontal tab content and chrome, shared by painting and UI geometry.
use crate::{Rect4, TabBar, batching::rect, labels::display_width, quad::QuadInstance};
use kettle_config::{Rgb, TabBarPos};

pub struct HorizontalTabContent {
    pub title: Rect4,
    pub close: Rect4,
    pub hint: Option<Rect4>,
}

pub fn tab_shortcut_hint(index: usize, macos: bool) -> Option<String> {
    (macos && index < 9).then(|| format!("Cmd+{}", index + 1))
}

/// Hint space is optional; close and activity space take priority over it.
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
    let hint = hint.and_then(|hint| {
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
    fill: Rgb,
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
        q.push(rect(bx, by, bw, bh, fill, 1.0));
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

pub(crate) fn horizontal_tab_quads(
    bar: &TabBar,
    theme: &kettle_config::Theme,
    background: Rgb,
    chrome: Rgb,
    accent: Rgb,
    pos: TabBarPos,
    scale: f32,
) -> Vec<QuadInstance> {
    let mut quads = Vec::new();
    let bottom = pos == TabBarPos::Bottom;
    let hover = crate::color::over(chrome, theme.foreground, 0.06, false);
    let divider =
        crate::color::with_min_contrast(theme.palette[8], chrome, crate::chrome::ICON_CONTRAST);
    for (i, tab) in bar.segments.iter().enumerate() {
        let hovered = bar.hovered_tab_idx == Some(tab.idx);
        if tab.active || hovered {
            rounded_fill(
                &mut quads,
                tab.rect,
                6.0 * scale,
                (!bottom, bottom),
                if tab.active { background } else { hover },
                tab.active.then_some((accent, 2.0, bottom)),
            );
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
    quads
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TabActivity;

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
                        assert_eq!(c.hint.is_some(), available >= 88.0);
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
        assert_eq!(tab_shortcut_hint(0, true).as_deref(), Some("Cmd+1"));
        assert_eq!(tab_shortcut_hint(8, true).as_deref(), Some("Cmd+9"));
        assert!(tab_shortcut_hint(9, true).is_none());
        assert!(tab_shortcut_hint(0, false).is_none());
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
                            &theme,
                            theme.background,
                            chrome,
                            theme.accent,
                            pos,
                            1.0,
                        );
                        let divider = quads
                            .iter()
                            .find(|q| q.pos[0] == 319.0 && q.size[0] == 1.0)
                            .expect("separator between the two inactive tabs");
                        let rgb = Rgb::new(
                            (divider.color[0] * 255.0).round() as u8,
                            (divider.color[1] * 255.0).round() as u8,
                            (divider.color[2] * 255.0).round() as u8,
                        );
                        assert!(
                            crate::color::contrast_ratio(rgb, chrome)
                                >= crate::chrome::ICON_CONTRAST,
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
            let hint = tab_shortcut_hint(0, true).unwrap();
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
                    &theme,
                    theme.background,
                    theme.palette[0],
                    accent,
                    pos,
                    1.0,
                );
                assert_eq!(
                    sample(&q, 80.0, 14.0),
                    Some(rect(0.0, 0.0, 1.0, 1.0, theme.background, 1.0).color)
                );
                let edge = if pos == TabBarPos::Top { 0.5 } else { 27.5 };
                assert_eq!(
                    sample(&q, 80.0, edge),
                    Some(rect(0.0, 0.0, 1.0, 1.0, accent, 1.0).color)
                );
                assert!(sample(&q, 0.1, edge).is_none());
                assert!(sample(&q, 0.1, 28.0 - edge).is_some());
                assert!(sample(&q, 240.0, 14.0).is_none());
                assert!(sample(&q, 159.5, 14.0).is_some());
                assert!(sample(&q, 319.5, 14.0).is_some());
                bar.hovered_tab_idx = Some(1);
                let hover = horizontal_tab_quads(
                    &bar,
                    &theme,
                    theme.background,
                    theme.palette[0],
                    accent,
                    pos,
                    1.0,
                );
                assert!(sample(&hover, 240.0, 14.0).is_some());
                // Separator at the hovered tab's trailing edge disappears.
                assert_ne!(sample(&hover, 319.5, 14.0), sample(&q, 319.5, 14.0));
            }
        }
    }
}

use super::{Rect4, Translator, display_width};
use kettle_config::Rgb;

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollbackPill {
    pub rect: Rect4,
    pub label: String,
    pub accessible_label: String,
    pub text_origin: (f32, f32),
}

/// Shared paint, pointer and accessibility geometry. Keep clear of the scrollbar.
pub fn scrollback_pill(
    terminal: Rect4,
    offset: usize,
    scale: f32,
    text_cell: (f32, f32),
    scrollbar_width: f32,
    tr: &Translator,
) -> Option<ScrollbackPill> {
    let (x, y, width, height) = terminal;
    let (cell_width, line_height) = text_cell;
    if offset == 0
        || ![x, y, width, height, scale, cell_width, line_height]
            .into_iter()
            .all(f32::is_finite)
        || scale <= 0.0
        || cell_width <= 0.0
        || line_height <= 0.0
    {
        return None;
    }
    let digits = offset.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    let label = tr.scrollback_return(&grouped, offset as u64);
    let pad = 6.0 * scale;
    let gutter = (scrollbar_hit_width(scrollbar_width, scale) + 2.0 * scale).max(14.0 * scale);
    let w = display_width(&label) as f32 * cell_width + 2.0 * pad;
    let h = line_height + pad;
    // Tiny panes cannot display the button without hiding its label.
    if width < w + gutter + pad || height < h + 2.0 * pad {
        return None;
    }
    let left = x + width - gutter - w;
    let top = y + height - pad - h;
    Some(ScrollbackPill {
        rect: (left, top, w, h),
        label,
        accessible_label: tr.scrollback_a11y(&grouped, offset as u64),
        text_origin: (left + pad, top + pad / 2.0),
    })
}

/// The pill's fill, 3:1 off the terminal, and its label's ink. The label is
/// held to `TEXT_CONTRAST` at the painted stem pixel, not only at full ink, so
/// it does not pass by a rounding step. Where no ink can do that on the 3:1
/// fill, the fill steps back toward the terminal until one can: the label
/// names the button, so it outranks the fill's edge.
pub(crate) fn scrollback_pill_colors(
    theme: &kettle_config::Theme,
    linear: bool,
) -> (kettle_config::Rgb, kettle_config::Rgb) {
    use crate::{chrome, color};
    let ink_on = |fill: kettle_config::Rgb| {
        color::with_min_contrast_at_coverage(
            theme.foreground,
            fill,
            chrome::TEXT_CONTRAST,
            color::STEM_COVERAGE,
            linear,
        )
    };
    let clears = |fill: kettle_config::Rgb| {
        let stem = color::over(fill, ink_on(fill), color::STEM_COVERAGE, linear);
        color::contrast_ratio(stem, fill) >= chrome::TEXT_CONTRAST
    };
    let raised = color::with_min_contrast(theme.palette[0], theme.background, 3.0);
    let toward_terminal = |t: f64| color::over(raised, theme.background, t, false);
    let mut fill = raised;
    if !clears(fill) {
        let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
        for _ in 0..14 {
            let mid = (lo + hi) / 2.0;
            if clears(toward_terminal(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        fill = toward_terminal(hi);
    }
    (fill, ink_on(fill))
}

pub fn scrollbar_draw_width(configured: f32, scale: f32, hovered: bool) -> f32 {
    let width = configured.clamp(2.0, 40.0);
    (if hovered { width.max(10.0) } else { width }) * scale
}

pub fn scrollbar_hit_width(configured: f32, scale: f32) -> f32 {
    (configured.clamp(2.0, 40.0) + 2.0).max(12.0) * scale
}

/// How a pane's scrollbar looks: dim at rest, brighter while the pane is
/// scrolled back or the pointer hovers or drags the bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollbarState {
    Rest,
    Active,
}

/// One pane's scrollbar in physical pixels. The painter, the pointer and the
/// AccessKit ScrollBar node all read it, so the bar that is drawn is the bar
/// that is grabbed and announced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneScrollbar {
    /// The bar's column, the terminal's full height, `edge` in from its
    /// right side: the channel paints here, command ticks lie across it, and
    /// it bounds the ScrollBar node.
    pub track_rect: Rect4,
    /// Whether the channel paints in `track_rect`.
    pub channel: bool,
    /// The thumb, when there is history to scroll.
    pub thumb_rect: Option<Rect4>,
    pub state: ScrollbarState,
    /// The pointer's strip. It runs to the terminal's right edge, so a fling
    /// to the edge of the screen still lands, and is at least 12 logical px
    /// wide whatever the bar's width.
    pub hit_rect: Rect4,
    /// Paint solid colours rather than translucent ones.
    pub opaque: bool,
}

/// The bar's column at its resting width: where command ticks lie.
pub fn scrollbar_rest_column(terminal: Rect4, scale: f32, configured: f32) -> (f32, f32) {
    let (x, _, w, _) = terminal;
    let width = scrollbar_draw_width(configured, scale, false);
    (x + w - width - 2.0 * scale, width)
}

/// The pointer's strip along a terminal's right edge.
pub fn scrollbar_hit_rect(terminal: Rect4, scale: f32, configured: f32) -> Rect4 {
    let (x, y, w, h) = terminal;
    let width = scrollbar_hit_width(configured, scale);
    (x + w - width, y, width, h)
}

/// A pane's scrollbar. `terminal` is the pane's terminal rect, `rows`,
/// `hist` and `off` its visible lines, history and lines scrolled back, and
/// `interacting` whether the pointer hovers or drags its bar.
pub fn pane_scrollbar_geometry(
    terminal: Rect4,
    scale: f32,
    cfg: &kettle_config::Config,
    rows: usize,
    hist: usize,
    off: usize,
    interacting: bool,
) -> PaneScrollbar {
    use kettle_config::ScrollbarMode;
    let (x, y, w, h) = terminal;
    let mode = cfg.scrollbar;
    let shown = match mode {
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => hist > 0,
        ScrollbarMode::Always => true,
    };
    let active = off > 0 || interacting;
    let bar_w = scrollbar_draw_width(cfg.scrollbar_width, scale, interacting);
    let edge = 2.0 * scale;
    let bx = x + w - bar_w - edge;
    let thumb_rect = if shown {
        kettle_core::scrollbar::thumb_with_min(
            rows,
            hist,
            off,
            h,
            kettle_core::scrollbar::MIN_THUMB_LOGICAL * scale,
        )
        .map(|(top, height)| (bx, y + top, bar_w, height))
    } else {
        None
    };
    PaneScrollbar {
        track_rect: (bx, y, bar_w, h),
        channel: shown && (active || mode == ScrollbarMode::Always),
        thumb_rect,
        state: if active {
            ScrollbarState::Active
        } else {
            ScrollbarState::Rest
        },
        hit_rect: scrollbar_hit_rect(terminal, scale, cfg.scrollbar_width),
        opaque: false,
    }
}

/// The scrollbar's colours as they show over the terminal background, and
/// the translucent ink and alpha that show as each one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollbarColors {
    pub(crate) rest: Rgb,
    pub(crate) active: Rgb,
    pub(crate) rest_track: Rgb,
    pub(crate) active_track: Rgb,
    /// `[rest, active, rest_track, active_track]` as translucent layers.
    pub(crate) layers: [(Rgb, f32); 4],
}

impl ScrollbarColors {
    /// The thumb's ink and alpha in `state`.
    pub(crate) fn thumb(&self, state: ScrollbarState, opaque: bool) -> (Rgb, f32) {
        match state {
            ScrollbarState::Rest => self.paint(self.rest, 0, opaque),
            ScrollbarState::Active => self.paint(self.active, 1, opaque),
        }
    }

    /// The channel's ink and alpha in `state`.
    pub(crate) fn track(&self, state: ScrollbarState, opaque: bool) -> (Rgb, f32) {
        match state {
            ScrollbarState::Rest => self.paint(self.rest_track, 2, opaque),
            ScrollbarState::Active => self.paint(self.active_track, 3, opaque),
        }
    }

    fn paint(&self, shown: Rgb, layer: usize, opaque: bool) -> (Rgb, f32) {
        if opaque {
            (shown, 1.0)
        } else {
            self.layers[layer]
        }
    }
}

/// The scrollbar's colours: the theme foreground over its background.
pub(crate) fn scrollbar_colors(theme: &kettle_config::Theme, linear: bool) -> ScrollbarColors {
    let layer = |alpha: f32| (theme.foreground, alpha);
    let layers = [layer(0.42), layer(0.82), layer(0.07), layer(0.14)];
    let shown =
        |(ink, alpha): (Rgb, f32)| crate::color::over(theme.background, ink, alpha.into(), linear);
    ScrollbarColors {
        rest: shown(layers[0]),
        active: shown(layers[1]),
        rest_track: shown(layers[2]),
        active_track: shown(layers[3]),
        layers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pill_follows_pane_and_disappears_at_bottom() {
        let tr = Translator::default();
        assert!(
            scrollback_pill((50.0, 80.0, 800.0, 600.0), 0, 2.0, (16.0, 32.0), 4.0, &tr).is_none()
        );
        let pill = scrollback_pill(
            (50.0, 80.0, 800.0, 600.0),
            1234,
            2.0,
            (16.0, 32.0),
            4.0,
            &tr,
        )
        .unwrap();
        assert_eq!(pill.label, "\u{2193} 1,234 lines");
        assert_eq!(pill.rect.0 + pill.rect.2, 822.0);
        assert_eq!(pill.rect.1 + pill.rect.3, 668.0);
        let es = Translator::new(kettle_i18n::Language::Es);
        assert!(
            scrollback_pill((0.0, 0.0, 800.0, 600.0), 1, 1.0, (8.0, 16.0), 4.0, &es)
                .unwrap()
                .label
                .ends_with("l\u{ed}nea")
        );
        assert!(scrollback_pill((0.0, 0.0, 1.0, 1.0), 10, 1.0, (8.0, 16.0), 4.0, &tr).is_none());
    }

    #[test]
    fn pill_leaves_room_for_a_wide_scrollbar_and_scaled_hit_target() {
        let pill = scrollback_pill(
            (0.0, 0.0, 800.0, 600.0),
            1234,
            2.0,
            (16.0, 32.0),
            40.0,
            &Translator::default(),
        )
        .unwrap();
        assert!(pill.rect.0 + pill.rect.2 <= 800.0 - scrollbar_hit_width(40.0, 2.0));
        assert_eq!(scrollbar_hit_width(4.0, 2.0), 24.0);
        assert!(scrollbar_hit_width(40.0, 2.0) > scrollbar_draw_width(40.0, 2.0, true));
    }

    #[test]
    fn hovered_scrollbar_is_at_least_ten_scaled_pixels() {
        assert_eq!(scrollbar_draw_width(4.0, 2.0, false), 8.0);
        assert_eq!(scrollbar_draw_width(4.0, 2.0, true), 20.0);
        assert_eq!(scrollbar_draw_width(20.0, 2.0, true), 40.0);
    }
}

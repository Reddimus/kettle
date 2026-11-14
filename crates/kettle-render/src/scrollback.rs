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
    /// The bar's column, the terminal's full height, 2 logical px in from
    /// its right side: the channel paints here, command ticks lie across it,
    /// and it bounds the ScrollBar node.
    pub track_rect: Rect4,
    /// Whether the channel paints in `track_rect`.
    pub channel: bool,
    /// The thumb, when there is history to scroll.
    pub thumb_rect: Option<Rect4>,
    /// Which colours the thumb and channel take.
    pub state: ScrollbarState,
    /// The pointer's strip. It runs to the terminal's right edge, so a fling
    /// to the edge of the screen still lands, and is at least 12 logical px
    /// wide whatever the bar's width.
    pub hit_rect: Rect4,
    /// Paint solid colours rather than translucent ones.
    pub opaque: bool,
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
///
/// With history, `auto` draws a faint full-height channel under a dim thumb
/// at rest. Scrolled back, hovered or dragged, the bar widens to at least 10
/// logical px and brightens, its channel as wide as the thumb. `always` keeps
/// one width while there is history, so hovering never makes it jump, and
/// with none draws the empty channel at the configured width, which hovering
/// leaves alone: there is nothing to grab.
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
    let scrollable = hist > 0;
    let shown = match mode {
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => scrollable,
        ScrollbarMode::Always => true,
    };
    let active = scrollable && (off > 0 || interacting);
    let wide = match mode {
        // No bar: command ticks keep the resting column.
        ScrollbarMode::Never => false,
        ScrollbarMode::Auto => active,
        ScrollbarMode::Always => scrollable,
    };
    let bar_w = scrollbar_draw_width(cfg.scrollbar_width, scale, wide);
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
        channel: shown,
        thumb_rect,
        state: if active {
            ScrollbarState::Active
        } else {
            ScrollbarState::Rest
        },
        hit_rect: scrollbar_hit_rect(terminal, scale, cfg.scrollbar_width),
        opaque: scrollbar_paints_opaque(cfg, scale),
    }
}

/// Whether the bar paints solid colours. Only where it cannot cover text,
/// at its widest and with its edge inside the `padding-x` the grid keeps
/// clear on its right, and only in an opaque window, where a solid bar does
/// not stand out as the one thing the desktop cannot show through. Elsewhere
/// it paints translucent layers that show as the same colours over the
/// background, so a glyph under it stays visible.
pub fn scrollbar_paints_opaque(cfg: &kettle_config::Config, scale: f32) -> bool {
    crate::theme::composed_bg_alpha(cfg) >= 1.0
        && scrollbar_draw_width(cfg.scrollbar_width, scale, true) + 2.0 * scale <= cfg.padding_x
}

/// The rest thumb against the background: the WCAG 2.2 1.4.11 floor for a
/// control.
pub(crate) const REST_THUMB_CONTRAST: f64 = crate::chrome::ICON_CONTRAST;
/// The thumb while scrolled back, hovered or dragged: the text floor.
pub(crate) const ACTIVE_THUMB_CONTRAST: f64 = crate::chrome::TEXT_CONTRAST;
/// The channel against the background: there, but quieter than the thumb.
pub(crate) const TRACK_CONTRAST: f64 = 1.5;
/// The channel at rest goes no further than this, so a pane with history
/// keeps a quiet edge.
pub(crate) const REST_TRACK_MAX_CONTRAST: f64 = 1.6;
/// The rest thumb against the channel it runs in.
pub(crate) const REST_THUMB_ON_TRACK: f64 = 2.0;
/// The active thumb against its channel.
pub(crate) const ACTIVE_THUMB_ON_TRACK: f64 = crate::chrome::ICON_CONTRAST;
/// A command tick against the background and the channel it lies across.
pub(crate) const TICK_CONTRAST: f64 = crate::chrome::ICON_CONTRAST;
/// Each colour is resolved this much past its floors, so the 8-bit
/// rounding of a translucent layer cannot take it under one.
const ROUNDING_MARGIN: f64 = 1.02;

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
    /// The success and failure ticks on the rest channel, then on the
    /// active one.
    pub(crate) ticks: [(Rgb, Rgb); 2],
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

    /// A command tick's colour: success or failure, on the channel the bar
    /// paints in `state`.
    pub(crate) fn tick(&self, state: ScrollbarState, success: bool) -> Rgb {
        let (ok, failed) = self.ticks[match state {
            ScrollbarState::Rest => 0,
            ScrollbarState::Active => 1,
        }];
        if success { ok } else { failed }
    }
}

/// The scrollbar's colours. Each starts from the theme foreground over its
/// background, as the bar was drawn before (0.42 for the rest thumb, 0.82
/// active, 0.07 and 0.14 for the channel), and moves only as far as its
/// floors need: the rest thumb 3:1 against the background and 2:1 against
/// the rest channel, the active thumb 4.5:1 and 3:1 against its channel, and
/// the channel 1.5:1, at rest no more than 1.6:1.
pub(crate) fn scrollbar_colors(theme: &kettle_config::Theme, linear: bool) -> ScrollbarColors {
    use crate::color::{self, with_min_contrast};
    let bg = theme.background;
    let mix = |alpha: f64| color::over(bg, theme.foreground, alpha, linear);
    let floor = |ratio: f64| ratio * ROUNDING_MARGIN;
    let rest_track = quiet_channel(mix(0.07), bg, linear);
    let active_track = with_min_contrast(mix(0.14), bg, floor(TRACK_CONTRAST));
    let thumb = |start: Rgb, on_bg: f64, track: Rgb, on_track: f64| {
        let raised = with_min_contrast(start, bg, floor(on_bg));
        let thumb = with_min_contrast(raised, track, floor(on_track));
        if color::contrast_ratio(thumb, bg) >= floor(on_bg) {
            thumb
        } else {
            color::with_min_contrast_on_both(raised, bg, track, floor(on_bg.max(on_track)), false)
        }
    };
    let rest = thumb(
        mix(0.42),
        REST_THUMB_CONTRAST,
        rest_track,
        REST_THUMB_ON_TRACK,
    );
    let active = thumb(
        mix(0.82),
        ACTIVE_THUMB_CONTRAST,
        active_track,
        ACTIVE_THUMB_ON_TRACK,
    );
    let tick = |ink: Rgb, track: Rgb| {
        color::with_min_contrast_on_both(ink, bg, track, floor(TICK_CONTRAST), true)
    };
    let ticks = |track: Rgb| (tick(theme.palette[2], track), tick(theme.palette[1], track));
    let layer = |shown: Rgb| translucent_layer(bg, shown, linear);
    ScrollbarColors {
        rest,
        active,
        rest_track,
        active_track,
        layers: [
            layer(rest),
            layer(active),
            layer(rest_track),
            layer(active_track),
        ],
        ticks: [ticks(rest_track), ticks(active_track)],
    }
}

/// The rest channel: the faint 0.07 mix raised to [`TRACK_CONTRAST`], or, on
/// a theme where that mix is already stronger than
/// [`REST_TRACK_MAX_CONTRAST`], eased back toward the background until it is
/// not.
fn quiet_channel(start: Rgb, bg: Rgb, linear: bool) -> Rgb {
    use crate::color::{self, contrast_ratio};
    let low = TRACK_CONTRAST * ROUNDING_MARGIN;
    if contrast_ratio(start, bg) < low {
        return color::with_min_contrast(start, bg, low);
    }
    if contrast_ratio(start, bg) <= REST_TRACK_MAX_CONTRAST / ROUNDING_MARGIN {
        return start;
    }
    // The least of `start` over the background that still clears the floor.
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..14 {
        let mid = (lo + hi) / 2.0;
        if contrast_ratio(color::over(bg, start, mid, linear), bg) >= low {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    color::over(bg, start, hi, linear)
}

/// The most a translucent layer covers: a glyph under the bar keeps at least
/// a tenth of its ink, so the rightmost column is never hidden.
pub(crate) const MAX_LAYER_ALPHA: f32 = 0.9;

/// The ink and alpha that show as `shown` painted over `bg`.
///
/// The ink lies on the line from `bg` through `shown`, as far out as colour
/// goes, at the lowest alpha that reaches `shown`, so the layer shows as
/// `shown` itself and lets as much of a glyph under it through as it can. The
/// ink rounds away from `bg`, so the layer never shows weaker than `shown`.
/// Where that needs more than [`MAX_LAYER_ALPHA`], white or black reaches the
/// same contrast against `bg` with less, giving up some of the tint; where
/// even that needs more, the layer stops at [`MAX_LAYER_ALPHA`] and falls
/// short of `shown`.
pub(crate) fn translucent_layer(bg: Rgb, shown: Rgb, linear: bool) -> (Rgb, f32) {
    use crate::color::{self, contrast_ratio, encode_srgb, relative_luminance, srgb_to_linear};
    let decode = |value: u8| {
        if linear {
            srgb_to_linear(value)
        } else {
            f64::from(value) / 255.0
        }
    };
    let channels = |c: Rgb| [c.r, c.g, c.b].map(decode);
    let (base, top) = (channels(bg), channels(shown));
    let alpha = base
        .iter()
        .zip(&top)
        .map(|(&b, &t)| {
            let room = if t > b { 1.0 - b } else { b };
            if room > 0.0 {
                (t - b).abs() / room
            } else {
                0.0
            }
        })
        .fold(0.0_f64, f64::max);
    if alpha <= 0.0 {
        return (shown, 0.0);
    }
    let ink = |b: f64, t: f64| {
        let value = (b + (t - b) / alpha).clamp(0.0, 1.0);
        let encoded = if linear {
            encode_srgb(value)
        } else {
            value * 255.0
        };
        // Round away from the background.
        let out = if t >= b {
            encoded.ceil()
        } else {
            encoded.floor()
        };
        out.clamp(0.0, 255.0) as u8
    };
    let tinted = Rgb::new(
        ink(base[0], top[0]),
        ink(base[1], top[1]),
        ink(base[2], top[2]),
    );
    if alpha <= f64::from(MAX_LAYER_ALPHA) {
        return (tinted, alpha as f32);
    }
    let plain = if relative_luminance(shown) > relative_luminance(bg) {
        Rgb::new(255, 255, 255)
    } else {
        Rgb::new(0, 0, 0)
    };
    let target = contrast_ratio(shown, bg);
    let reaches = |a: f64| contrast_ratio(color::over(bg, plain, a, linear), bg) >= target;
    if !reaches(f64::from(MAX_LAYER_ALPHA)) {
        return (plain, MAX_LAYER_ALPHA);
    }
    let (mut lo, mut hi) = (0.0_f64, f64::from(MAX_LAYER_ALPHA));
    for _ in 0..20 {
        let mid = (lo + hi) / 2.0;
        if reaches(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (plain, hi as f32)
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

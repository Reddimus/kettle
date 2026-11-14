//! The GNOME header's window-button glyphs and the discs behind them, drawn
//! as coverage masks rather than shaped from a font, so their size and weight
//! never depend on the installed fonts.
//!
//! The icons follow Adwaita's symbolic set: a 16-unit grid, strokes 2 units
//! wide (1.5 for the window outlines), drawn at one unit per logical pixel.
use crate::{Rect4, batching::rect, quad::QuadInstance};
use kettle_config::Rgb;

/// The design grid of GNOME's symbolic icons, in units per side.
pub(crate) const ICON_GRID: f32 = 16.0;

/// Side of the disc behind a GNOME window button at 1x: libadwaita pads its
/// 16 px icon by 4 px on each side.
pub(crate) const WINDOW_BUTTON_DISC: f32 = 24.0;

/// Supersamples per pixel side when rasterizing a mask.
const SAMPLES: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum HeaderIcon {
    /// `window-minimize-symbolic`: a bar low in the square.
    Minimize,
    /// `window-maximize-symbolic`: a square outline.
    Maximize,
    /// `window-restore-symbolic`: two overlapping square outlines.
    Restore,
    /// `window-close-symbolic`: a diagonal cross.
    Close,
    /// The filled disc behind a window button, the whole mask wide.
    Disc,
}

impl HeaderIcon {
    pub(crate) fn for_window_control(action: crate::WindowControl) -> Self {
        use crate::WindowControl as C;
        match action {
            C::Minimize => Self::Minimize,
            C::Maximize => Self::Maximize,
            C::Restore => Self::Restore,
            C::Close => Self::Close,
        }
    }
}

/// Distance from `(x, y)` to the segment `a`-`b`.
fn segment_distance((x, y): (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((x - a.0) * dx + (y - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (x - (a.0 + t * dx)).hypot(y - (a.1 + t * dy))
}

/// Whether `(x, y)` is inside the rectangle `(left, top, right, bottom)`
/// with corners rounded to `radius`.
fn in_rounded_rect((x, y): (f32, f32), (l, t, r, b): (f32, f32, f32, f32), radius: f32) -> bool {
    if x < l || x > r || y < t || y > b {
        return false;
    }
    let cx = x.clamp(l + radius, r - radius);
    let cy = y.clamp(t + radius, b - radius);
    (x - cx).hypot(y - cy) <= radius
}

/// A square outline `stroke` units wide whose outer edge is `outer`, its
/// outer corners rounded to `radius`.
fn in_outline(p: (f32, f32), outer: (f32, f32, f32, f32), radius: f32, stroke: f32) -> bool {
    let (l, t, r, b) = outer;
    in_rounded_rect(p, outer, radius)
        && !in_rounded_rect(
            p,
            (l + stroke, t + stroke, r - stroke, b - stroke),
            (radius - stroke).max(0.0),
        )
}

/// Whether the point `(x, y)`, in grid units, is ink in `icon`.
fn icon_ink(icon: HeaderIcon, x: f32, y: f32) -> bool {
    let p = (x, y);
    // Round-ended strokes are 2 units wide; the window outlines 1.5.
    let capsule = |a, b| segment_distance(p, a, b) <= 1.0;
    let thin = 1.5;
    match icon {
        HeaderIcon::Minimize => capsule((5.0, 11.0), (11.0, 11.0)),
        HeaderIcon::Maximize => in_outline(p, (4.0, 4.0, 12.0, 12.0), 2.0, thin),
        HeaderIcon::Restore => {
            let front = (4.0, 6.0, 10.0, 12.0);
            in_outline(p, front, 1.5, thin)
                || in_outline(p, (6.0, 4.0, 12.0, 10.0), 1.5, thin)
                    && !in_rounded_rect(p, front, 1.5)
        }
        HeaderIcon::Close => capsule((4.5, 4.5), (11.5, 11.5)) || capsule((11.5, 4.5), (4.5, 11.5)),
        HeaderIcon::Disc => (x - 8.0).hypot(y - 8.0) <= 8.0,
    }
}

/// Coverage of `icon` rasterized `side` pixels square, row by row, 0..=255.
pub(crate) fn header_icon_mask(icon: HeaderIcon, side: u32) -> Vec<u8> {
    let unit = ICON_GRID / side.max(1) as f32;
    let mut mask = Vec::with_capacity((side * side) as usize);
    for py in 0..side {
        for px in 0..side {
            let mut hits = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32) * unit;
                    let y = (py as f32 + (sy as f32 + 0.5) / SAMPLES as f32) * unit;
                    hits += u32::from(icon_ink(icon, x, y));
                }
            }
            mask.push((hits * 255 / (SAMPLES * SAMPLES)) as u8);
        }
    }
    mask
}

/// Masks already rasterized, by icon and side, kept between frames. A
/// header holds at most a few icons at one scale, so the list stays short;
/// it is dropped whole if a scale change ever leaves stale sides in it.
#[derive(Default)]
pub(crate) struct IconMasks {
    masks: Vec<((HeaderIcon, u32), Vec<u8>)>,
}

impl IconMasks {
    pub(crate) fn get(&mut self, icon: HeaderIcon, side: u32) -> &[u8] {
        let index = match self.masks.iter().position(|(key, _)| *key == (icon, side)) {
            Some(index) => index,
            None => {
                if self.masks.len() >= 32 {
                    self.masks.clear();
                }
                self.masks
                    .push(((icon, side), header_icon_mask(icon, side)));
                self.masks.len() - 1
            }
        };
        &self.masks[index].1
    }
}

/// Where a mask `side` device pixels square draws centred in `button`,
/// snapped to whole pixels so its axis-aligned strokes stay crisp.
pub(crate) fn centred_square(button: Rect4, side: f32) -> Rect4 {
    let (x, y, w, h) = button;
    (
        (x + (w - side) * 0.5).round(),
        (y + (h - side) * 0.5).round(),
        side,
        side,
    )
}

/// Side in device pixels of a 16-unit icon at `scale`, never more than
/// `room` allows. `None` when it would be smaller than half its size.
pub(crate) fn icon_side(scale: f32, room: f32) -> Option<f32> {
    let side = (ICON_GRID * scale.max(0.5)).min(room).round();
    (side >= ICON_GRID * 0.5).then_some(side)
}

/// `mask`, `side` pixels square, as one quad per run of equal coverage in
/// `ink`, its top-left corner at `(left, top)`.
pub(crate) fn push_mask(
    quads: &mut Vec<QuadInstance>,
    mask: &[u8],
    side: u32,
    (left, top): (f32, f32),
    ink: Rgb,
) {
    for (row, line) in mask.chunks(side.max(1) as usize).enumerate() {
        let mut start = 0;
        while start < line.len() {
            let coverage = line[start];
            let mut end = start + 1;
            while end < line.len() && line[end] == coverage {
                end += 1;
            }
            if coverage > 0 {
                quads.push(rect(
                    left + start as f32,
                    top + row as f32,
                    (end - start) as f32,
                    1.0,
                    ink,
                    f32::from(coverage) / 255.0,
                ));
            }
            start = end;
        }
    }
}

/// `icon` in `ink`, centred in `button` at `scale` with two pixels to spare
/// on each side. Nothing for a button too small to hold it at half size.
pub(crate) fn push_header_icon(
    quads: &mut Vec<QuadInstance>,
    icon: HeaderIcon,
    button: Rect4,
    ink: Rgb,
    scale: f32,
    masks: &mut IconMasks,
) {
    if button.2 <= 0.0 || button.3 <= 0.0 {
        return;
    }
    let Some(side) = icon_side(scale, button.2.min(button.3) - 4.0) else {
        return;
    };
    let (left, top, ..) = centred_square(button, side);
    let n = side as u32;
    push_mask(quads, masks.get(icon, n), n, (left, top), ink);
}

/// The disc behind a window button: `WINDOW_BUTTON_DISC` points at `scale`,
/// centred in its target, smaller only in a target too small for it.
pub(crate) fn window_button_disc(button: Rect4, scale: f32) -> Rect4 {
    let side = (WINDOW_BUTTON_DISC * scale)
        .min(button.2)
        .min(button.3)
        .max(0.0)
        .round();
    centred_square(button, side)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Total ink of `mask` in pixel units: the area it covers.
    fn ink_area(mask: &[u8]) -> f32 {
        mask.iter().map(|&c| f32::from(c) / 255.0).sum()
    }

    /// The bounding box of pixels at least half covered: `(left, top,
    /// width, height)` in pixels.
    fn ink_box(mask: &[u8], side: u32) -> (u32, u32, u32, u32) {
        let s = side as usize;
        let (mut l, mut t, mut r, mut b) = (u32::MAX, u32::MAX, 0, 0);
        for (i, &c) in mask.iter().enumerate() {
            if c >= 128 {
                let (x, y) = ((i % s) as u32, (i / s) as u32);
                l = l.min(x);
                t = t.min(y);
                r = r.max(x + 1);
                b = b.max(y + 1);
            }
        }
        (l, t, r - l, b - t)
    }

    /// Covered pixels in column `x`, as runs of rows `(start, length)`.
    fn column_runs(mask: &[u8], side: u32, x: u32) -> Vec<(u32, u32)> {
        let mut runs = Vec::new();
        let mut start = None;
        for y in 0..=side {
            let on = y < side && mask[(y * side + x) as usize] >= 128;
            match (on, start) {
                (true, None) => start = Some(y),
                (false, Some(s)) => {
                    runs.push((s, y - s));
                    start = None;
                }
                _ => {}
            }
        }
        runs
    }

    /// The window glyphs keep Adwaita's 1.5 to 2-point strokes: the
    /// minimize bar is 2 points thick, the close cross's arms 2 points
    /// across, the outlines 1.5, and every glyph sits inside the disc with
    /// room to spare.
    #[test]
    fn window_glyph_strokes_are_one_and_a_half_to_two_points() {
        for scale in [1_u32, 2] {
            let side = 16 * scale;
            let s = scale as f32;
            let minimize = header_icon_mask(HeaderIcon::Minimize, side);
            assert_eq!(
                column_runs(&minimize, side, 8 * scale),
                vec![(10 * scale, 2 * scale)]
            );
            // The cross: area over centre-line length, less the overlap.
            let close = header_icon_mask(HeaderIcon::Close, side);
            let length = 2.0 * 7.0 * std::f32::consts::SQRT_2 * s;
            let stroke = ink_area(&close) / length;
            assert!(
                (1.5 * s..=2.2 * s).contains(&stroke),
                "{scale}x close stroke {stroke}"
            );
            for icon in [HeaderIcon::Maximize, HeaderIcon::Restore] {
                let mask = header_icon_mask(icon, side);
                // Along the outline's left side, away from its corners.
                let row = (8 * scale * side) as usize;
                let across: f32 = mask[row..row + (8 * scale) as usize]
                    .iter()
                    .map(|&c| f32::from(c) / 255.0)
                    .sum();
                assert!(
                    (1.5 * s - 0.01..=2.0 * s).contains(&across),
                    "{scale}x {icon:?} stroke {across}"
                );
            }
            for icon in [
                HeaderIcon::Minimize,
                HeaderIcon::Maximize,
                HeaderIcon::Restore,
                HeaderIcon::Close,
            ] {
                let (l, t, w, h) = ink_box(&header_icon_mask(icon, side), side);
                // Inside the 12-point square the disc's 4-point padding
                // leaves around the 16-point icon.
                assert!(l >= 3 * scale && t >= 3 * scale, "{scale}x {icon:?}");
                assert!(
                    l + w <= 13 * scale && t + h <= 13 * scale,
                    "{scale}x {icon:?}"
                );
            }
        }
    }

    /// A window button's disc is 24 points at 1x and 2x, centred on whole
    /// pixels in its target, and shrinks only with a target too small.
    #[test]
    fn window_button_disc_is_24_points_centred_in_its_target() {
        for scale in [1.0_f32, 1.5, 2.0] {
            let button = (700.0 * scale, 6.0 * scale, 34.0 * scale, 34.0 * scale);
            let disc = window_button_disc(button, scale);
            assert_eq!(
                (disc.2, disc.3),
                ((24.0 * scale).round(), (24.0 * scale).round())
            );
            assert_eq!((disc.0.fract(), disc.1.fract()), (0.0, 0.0));
            assert!((disc.0 + disc.2 * 0.5 - (button.0 + button.2 * 0.5)).abs() <= 0.5);
            assert!((disc.1 + disc.3 * 0.5 - (button.1 + button.3 * 0.5)).abs() <= 0.5);
        }
        assert_eq!(window_button_disc((0.0, 0.0, 10.0, 12.0), 1.0).2, 10.0);
        // The disc mask is round: its corners are empty, its middle full.
        let disc = header_icon_mask(HeaderIcon::Disc, 24);
        assert_eq!(disc[0], 0);
        assert_eq!(disc[12 * 24 + 12], 255);
    }

    #[test]
    fn icon_masks_are_kept_per_icon_and_side() {
        let mut masks = IconMasks::default();
        let first = masks.get(HeaderIcon::Minimize, 16).as_ptr();
        assert_eq!(masks.get(HeaderIcon::Minimize, 16).as_ptr(), first);
        assert_eq!(masks.get(HeaderIcon::Minimize, 32).len(), 32 * 32);
        assert_eq!(
            masks.get(HeaderIcon::Close, 16),
            header_icon_mask(HeaderIcon::Close, 16).as_slice()
        );
        assert_eq!(masks.masks.len(), 3);
    }
}

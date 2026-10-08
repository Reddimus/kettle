//! The media viewer: one shelf item, opened by the user over the pane it
//! belongs to. A header names the item and its sender and offers previous,
//! next and close; the image is fitted below it on its canvas, never scaled
//! past its own pixels; a footer says how to leave. The renderer gets only
//! display text and pixels, never a path or a source.

use glyphon::cosmic_text::Wrap;
use glyphon::{
    Attrs, Buffer as TextBuffer, Color as GColor, Family, FontSystem, Metrics, Shaping, TextArea,
    Weight,
};

use crate::Rect4;

/// What shows behind the image's transparent pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaCanvas {
    /// The pane's background.
    Theme,
    /// White, as SVG documents expect.
    White,
    /// A checkerboard, so a raster's transparency shows as such.
    Checker,
}

/// One open shelf item, projected for painting.
#[derive(Clone, Debug)]
pub struct MediaViewerOverlay {
    /// The pane the viewer covers, in physical pixels.
    pub pane_rect: Rect4,
    /// Display-ready (sanitized, bounded) title.
    pub title: String,
    /// The kind, size and sender, in the UI language.
    pub detail: String,
    /// How to leave and browse, in the UI language.
    pub hint: String,
    /// The item's place on its shelf, 1-based, and the shelf's length.
    pub position: (usize, usize),
    /// The pixels, or `None` when they were released.
    pub image: Option<kettle_core::ImageData>,
    /// Shown in the image's place when there are no pixels.
    pub status: String,
    pub canvas: MediaCanvas,
}

/// Exact paint and input geometry of the viewer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaViewerGeometry {
    pub rect: Rect4,
    pub title: Rect4,
    pub counter: Rect4,
    pub previous: Option<Rect4>,
    pub next: Option<Rect4>,
    pub close: Rect4,
    pub detail: Rect4,
    pub hint: Rect4,
    /// Where the image area is: the fitted image, or the whole area when
    /// there is no image.
    pub image_area: Rect4,
    /// The fitted image, when there are pixels.
    pub image: Option<Rect4>,
}

/// What a pointer press at a point means to an open viewer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaViewerHit {
    Close,
    Previous,
    Next,
    /// Anywhere else on the viewer: nothing happens.
    Inside,
    /// Outside it: the viewer closes.
    Outside,
}

fn contains(rect: Rect4, x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

impl MediaViewerGeometry {
    pub fn hit_test(&self, x: f32, y: f32) -> MediaViewerHit {
        if !contains(self.rect, x, y) {
            MediaViewerHit::Outside
        } else if contains(self.close, x, y) {
            MediaViewerHit::Close
        } else if self.previous.is_some_and(|rect| contains(rect, x, y)) {
            MediaViewerHit::Previous
        } else if self.next.is_some_and(|rect| contains(rect, x, y)) {
            MediaViewerHit::Next
        } else {
            MediaViewerHit::Inside
        }
    }
}

/// The counter's text: the item's place and the shelf's length.
pub fn media_viewer_counter(position: (usize, usize)) -> String {
    format!("{}/{}", position.0, position.1)
}

/// Lay the viewer out over its pane. `cell` is the terminal cell, and
/// `text_cell` the overlay text's column width and line height. `None` when
/// the pane is too small to hold a usable viewer.
pub fn media_viewer_geometry(
    viewer: &MediaViewerOverlay,
    cell: (f32, f32),
    text_cell: (f32, f32),
) -> Option<MediaViewerGeometry> {
    let finite = |value: f32| value.is_finite() && value > 0.0;
    let (cw, ch) = cell;
    let (tw, lh) = text_cell;
    if !(finite(cw) && finite(ch) && finite(tw) && finite(lh)) {
        return None;
    }
    let (px, py, pw, ph) = viewer.pane_rect;
    let inset = cw.max(6.0).round();
    let rect = (px + inset, py + inset, pw - 2.0 * inset, ph - 2.0 * inset);
    if rect.2 < 24.0 * tw || rect.3 < 6.0 * lh {
        return None;
    }
    let pad = (tw * 0.75).round().max(4.0);
    let left = rect.0 + pad;
    let right = rect.0 + rect.2 - pad;
    let header_y = rect.1 + pad;
    let button = 3.0 * tw;
    let close = (right - button, header_y, button, lh);
    let (previous, next, counter_right) = if viewer.position.1 > 1 {
        let next = (close.0 - button, header_y, button, lh);
        let previous = (next.0 - button, header_y, button, lh);
        (Some(previous), Some(next), previous.0)
    } else {
        (None, None, close.0)
    };
    let counter_width = (media_viewer_counter(viewer.position).chars().count() as f32 + 1.0) * tw;
    let counter = (counter_right - counter_width, header_y, counter_width, lh);
    let title = (left, header_y, (counter.0 - pad - left).max(0.0), lh);
    let detail = (left, header_y + lh, right - left, lh);
    let hint = (left, rect.1 + rect.3 - pad - lh, right - left, lh);
    let area_top = detail.1 + lh + pad;
    let area_bottom = hint.1 - pad;
    let image_area = (
        left,
        area_top,
        right - left,
        (area_bottom - area_top).max(0.0),
    );
    let image = viewer.image.as_ref().and_then(|image| {
        let (width, height) = (image.width as f32, image.height as f32);
        // The whole pixels inside the area: the image lands on pixel
        // boundaries and never past the area's edges.
        let (left, top) = (image_area.0.ceil(), image_area.1.ceil());
        let room = (
            (image_area.0 + image_area.2).floor() - left,
            (image_area.1 + image_area.3).floor() - top,
        );
        if width <= 0.0 || height <= 0.0 || room.0 < 1.0 || room.1 < 1.0 {
            return None;
        }
        // Fit inside that room, never past the image's own pixels, at one
        // scale. The longer edge is floored to stay inside and the shorter
        // one follows from the image's shape, rounded, so the shape is off by
        // under half a pixel. When rounding would take the shorter edge past
        // the room, it takes the whole room instead and the longer edge
        // follows it. An image too thin to keep its shape and fit still keeps
        // one pixel, so it stays visible.
        let scale = (room.0 / width).min(room.1 / height).min(1.0);
        let fit = |long: f32, short: f32, short_room: f32| {
            let long_edge = (long * scale).floor().max(1.0);
            let short_edge = (long_edge * short / long).round().max(1.0);
            if short_edge <= short_room {
                (long_edge, short_edge)
            } else {
                ((short_room * long / short).round().max(1.0), short_room)
            }
        };
        let (w, h) = if width >= height {
            fit(width, height, room.1)
        } else {
            let (h, w) = fit(height, width, room.0);
            (w, h)
        };
        Some((
            left + ((room.0 - w) / 2.0).floor(),
            top + ((room.1 - h) / 2.0).floor(),
            w,
            h,
        ))
    });
    Some(MediaViewerGeometry {
        rect,
        title,
        counter,
        previous,
        next,
        close,
        detail,
        hint,
        image_area,
        image,
    })
}

/// The viewer's text, shaped once per change: its five lines and the three
/// control glyphs.
pub(crate) struct ViewerText {
    title: TextBuffer,
    detail: TextBuffer,
    counter: TextBuffer,
    hint: TextBuffer,
    status: TextBuffer,
    controls: [TextBuffer; 3],
    /// What each line buffer was last shaped with; `None` until it is, or
    /// once a font change means it must be again.
    shaped: [Option<String>; 5],
}

impl ViewerText {
    pub(crate) fn new(font_system: &mut FontSystem, metrics: Metrics) -> Self {
        let line = |font_system: &mut FontSystem| {
            let mut buffer = TextBuffer::new(font_system, metrics);
            buffer.set_wrap(Wrap::None);
            buffer
        };
        let control = |font_system: &mut FontSystem, glyph: &str| {
            let mut buffer = line(font_system);
            buffer.set_text(
                glyph,
                &Attrs::new().weight(Weight::BOLD),
                Shaping::Advanced,
                None,
            );
            buffer
        };
        Self {
            title: line(font_system),
            detail: line(font_system),
            counter: line(font_system),
            hint: line(font_system),
            status: line(font_system),
            controls: [
                control(font_system, "‹"),
                control(font_system, "›"),
                control(font_system, "×"),
            ],
            shaped: Default::default(),
        }
    }

    /// What each line was last shaped with, for the renderer's text damage
    /// key: buffers keep their addresses when the viewer changes item.
    pub(crate) fn shaped(&self) -> &[Option<String>; 5] {
        &self.shaped
    }

    /// Reshape every line on the next fill, after the font family or the
    /// loaded faces change: the text alone would not say so.
    pub(crate) fn invalidate(&mut self) {
        self.shaped = Default::default();
    }

    /// Shape what `viewer` shows into its rectangles.
    pub(crate) fn shape(
        &mut self,
        font_system: &mut FontSystem,
        metrics: Metrics,
        family: &str,
        viewer: &MediaViewerOverlay,
        geometry: &MediaViewerGeometry,
    ) {
        let counter = media_viewer_counter(viewer.position);
        let status = if viewer.image.is_some() {
            ""
        } else {
            viewer.status.as_str()
        };
        let lines: [(&mut TextBuffer, &str, Rect4, bool); 5] = [
            (&mut self.title, &viewer.title, geometry.title, true),
            (&mut self.detail, &viewer.detail, geometry.detail, false),
            (&mut self.counter, &counter, geometry.counter, false),
            (&mut self.hint, &viewer.hint, geometry.hint, false),
            (&mut self.status, status, geometry.image_area, false),
        ];
        for ((buffer, text, rect, bold), shaped) in lines.into_iter().zip(&mut self.shaped) {
            buffer.set_metrics(metrics);
            buffer.set_size(Some(rect.2), Some(rect.3));
            if shaped.as_deref() != Some(text) {
                let attrs = Attrs::new().family(Family::Name(family));
                let attrs = if bold {
                    attrs.weight(Weight::BOLD)
                } else {
                    attrs
                };
                buffer.set_text(text, &attrs, Shaping::Advanced, None);
                text.clone_into(shaped.get_or_insert_default());
            }
            buffer.shape_until_scroll(font_system, false);
        }
        for buffer in &mut self.controls {
            buffer.set_metrics(metrics);
            buffer.shape_until_scroll(font_system, false);
        }
    }

    /// The text areas to paint, in `label`, `description` and `emphasis`
    /// colors. Control glyphs are centered in their cells by `glyph_width`.
    pub(crate) fn areas<'a>(
        &'a self,
        geometry: &MediaViewerGeometry,
        colors: (GColor, GColor, GColor),
        glyph_width: f32,
        line_height: f32,
    ) -> Vec<TextArea<'a>> {
        let (label, description, emphasis) = colors;
        let area = |buffer: &'a TextBuffer, rect: Rect4, color: GColor| TextArea {
            buffer,
            left: rect.0,
            top: rect.1,
            scale: 1.0,
            bounds: crate::text_bounds_for_rect(rect),
            default_color: color,
            custom_glyphs: &[],
        };
        let mut areas = vec![
            area(&self.title, geometry.title, label),
            area(&self.detail, geometry.detail, description),
            area(&self.counter, geometry.counter, description),
            area(&self.hint, geometry.hint, description),
        ];
        if let Some(status) = self.shaped[4]
            .as_deref()
            .filter(|status| !status.is_empty())
        {
            let (x, y, w, h) = geometry.image_area;
            let text = status.chars().count() as f32 * glyph_width;
            let left = x + ((w - text) / 2.0).max(0.0);
            let top = y + ((h - line_height) / 2.0).max(0.0);
            areas.push(area(
                &self.status,
                (left, top, w - (left - x), line_height),
                description,
            ));
        }
        let buttons = [geometry.previous, geometry.next, Some(geometry.close)];
        for (buffer, rect) in self.controls.iter().zip(buttons) {
            if let Some(rect) = rect {
                let mut glyph = area(buffer, rect, emphasis);
                glyph.left = rect.0 + ((rect.2 - glyph_width) / 2.0).max(0.0);
                glyph.top = rect.1 + ((rect.3 - line_height) / 2.0).max(0.0);
                areas.push(glyph);
            }
        }
        areas
    }
}

/// Most squares a checkerboard is drawn with; larger images get larger
/// squares.
const MAX_CHECKER_SQUARES: usize = 2048;

/// The dark squares of a checkerboard over `rect`, whose light squares are
/// the background beneath. Clipped to `rect` and bounded in number.
pub fn checker_squares(rect: Rect4, square: f32) -> Vec<Rect4> {
    let (x, y, w, h) = rect;
    if !(square.is_finite() && square >= 1.0 && w >= 1.0 && h >= 1.0) {
        return Vec::new();
    }
    let mut size = square;
    while ((w / size).ceil() * (h / size).ceil()) as usize > 2 * MAX_CHECKER_SQUARES {
        size *= 2.0;
    }
    let (columns, rows) = ((w / size).ceil() as usize, (h / size).ceil() as usize);
    let mut squares = Vec::with_capacity(columns * rows / 2 + 1);
    for row in 0..rows {
        for column in (row % 2..columns).step_by(2) {
            let sx = x + column as f32 * size;
            let sy = y + row as f32 * size;
            squares.push((sx, sy, size.min(x + w - sx), size.min(y + h - sy)));
        }
    }
    squares
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: (f32, f32) = (10.0, 20.0);

    fn viewer(position: (usize, usize), image: Option<(u32, u32)>) -> MediaViewerOverlay {
        MediaViewerOverlay {
            pane_rect: (0.0, 0.0, 800.0, 600.0),
            title: "Plot".into(),
            detail: "Raster 640×480".into(),
            hint: "Esc closes".into(),
            position,
            image: image.map(|(w, h)| {
                kettle_core::ImageData::new(w, h, vec![0; (w * h * 4) as usize]).unwrap()
            }),
            status: String::new(),
            canvas: MediaCanvas::Theme,
        }
    }

    #[test]
    fn the_viewer_sits_inside_its_pane_with_its_controls_on_one_row() {
        let geometry =
            media_viewer_geometry(&viewer((2, 5), Some((640, 480))), CELL, CELL).unwrap();
        let (x, y, w, h) = geometry.rect;
        assert!(x >= 0.0 && y >= 0.0 && x + w <= 800.0 && y + h <= 600.0);
        let previous = geometry.previous.unwrap();
        let next = geometry.next.unwrap();
        assert!(previous.0 + previous.2 <= next.0 && next.0 + next.2 <= geometry.close.0);
        assert!(geometry.counter.0 + geometry.counter.2 <= previous.0);
        assert!(geometry.title.0 + geometry.title.2 <= geometry.counter.0);
        for row in [geometry.counter, previous, next, geometry.close] {
            assert_eq!(row.1, geometry.title.1);
        }
        assert!(geometry.detail.1 > geometry.title.1);
        assert!(geometry.hint.1 > geometry.image_area.1 + geometry.image_area.3);
    }

    #[test]
    fn one_item_needs_no_browsing_buttons() {
        let geometry = media_viewer_geometry(&viewer((1, 1), None), CELL, CELL).unwrap();
        assert_eq!((geometry.previous, geometry.next), (None, None));
        assert_eq!(geometry.image, None);
        assert_eq!(
            geometry.hit_test(geometry.close.0 + 1.0, geometry.close.1 + 1.0),
            MediaViewerHit::Close
        );
    }

    #[test]
    fn the_image_is_fitted_centered_and_never_enlarged() {
        let geometry =
            media_viewer_geometry(&viewer((1, 1), Some((4000, 1000))), CELL, CELL).unwrap();
        let (ix, iy, iw, ih) = geometry.image.unwrap();
        let (ax, ay, aw, ah) = geometry.image_area;
        assert!(iw <= aw && ih <= ah);
        assert!((iw / ih - 4.0).abs() < 0.05, "keeps its aspect");
        assert!(
            ((ix - ax) - (ax + aw - ix - iw)).abs() <= 1.0,
            "centered across"
        );
        assert!(
            ((iy - ay) - (ay + ah - iy - ih)).abs() <= 1.0,
            "centered down"
        );
        let small = media_viewer_geometry(&viewer((1, 1), Some((64, 48))), CELL, CELL).unwrap();
        assert_eq!(
            (small.image.unwrap().2, small.image.unwrap().3),
            (64.0, 48.0)
        );
    }

    #[test]
    fn the_fitted_image_keeps_its_shape_within_half_a_pixel() {
        // Whole and fractional overlay metrics: a fractional line height
        // leaves an image area with a fractional height.
        let layouts = [
            ((0.0, 0.0, 800.0, 600.0), CELL, CELL),
            ((0.0, 0.0, 800.0, 600.0), (10.0, 20.25), (10.0, 20.25)),
            ((0.0, 0.0, 800.0, 300.0), (9.0, 19.0), (9.0, 18.75)),
            ((0.0, 0.0, 333.0, 777.0), (7.5, 15.5), (7.25, 15.3)),
        ];
        let sizes = (1..=40).flat_map(|step| {
            [
                (997, step * 37),
                (step * 37, 997),
                (4096, step),
                (step, 4096),
                (1000, 300 + step),
            ]
        });
        for (pane_rect, cell, text_cell) in layouts {
            for (width, height) in sizes.clone() {
                let mut overlay = viewer((1, 1), Some((width, height)));
                overlay.pane_rect = pane_rect;
                let geometry = media_viewer_geometry(&overlay, cell, text_cell).unwrap();
                let (_, _, iw, ih) = geometry.image.unwrap();
                let (_, _, aw, ah) = geometry.image_area;
                let (ix, iy) = (geometry.image.unwrap().0, geometry.image.unwrap().1);
                let (ax, ay) = (geometry.image_area.0, geometry.image_area.1);
                let case = format!("{width}x{height} in {aw}x{ah}: {iw}x{ih} at {ix},{iy}");
                assert!(iw >= 1.0 && ih >= 1.0, "{case}");
                assert!(
                    ix >= ax && iy >= ay && ix + iw <= ax + aw && iy + ih <= ay + ah,
                    "{case}: inside the area"
                );
                for value in [ix, iy, iw, ih] {
                    assert_eq!(value, value.floor(), "{case}: on pixel boundaries");
                }
                let (w, h) = (width as f32, height as f32);
                let (high, wide) = ((iw * h / w - ih).abs(), (ih * w / h - iw).abs());
                if iw * h / w >= 1.0 && ih * w / h >= 1.0 {
                    assert!(high <= 0.5 || wide <= 0.5, "{case}");
                } else {
                    assert!(iw == 1.0 || ih == 1.0, "{case}: one pixel keeps it visible");
                }
            }
        }
        let thin = media_viewer_geometry(&viewer((1, 1), Some((4096, 1))), CELL, CELL).unwrap();
        let (_, _, aw, _) = thin.image_area;
        assert_eq!(
            (thin.image.unwrap().2, thin.image.unwrap().3),
            (aw.floor(), 1.0)
        );
        let mut short_room = viewer((1, 1), Some((1000, 300)));
        short_room.pane_rect = (0.0, 0.0, 800.0, 300.0);
        let geometry = media_viewer_geometry(&short_room, (9.0, 19.0), (9.0, 18.75)).unwrap();
        let (_, iy, iw, ih) = geometry.image.unwrap();
        let (_, ay, _, ah) = geometry.image_area;
        assert!(iy >= ay && iy + ih <= ay + ah, "{iw}x{ih} at {iy}");
        assert!(
            (ih * 1000.0 / 300.0 - iw).abs() <= 0.5 || (iw * 300.0 / 1000.0 - ih).abs() <= 0.5,
            "{iw}x{ih}"
        );
    }

    #[test]
    fn an_invalidated_line_takes_the_new_family_though_its_text_is_the_same() {
        let mut font_system = FontSystem::new();
        let metrics = Metrics::new(14.0, 18.0);
        let mut text = ViewerText::new(&mut font_system, metrics);
        let viewer = viewer((1, 2), Some((64, 48)));
        let geometry = media_viewer_geometry(&viewer, CELL, CELL).unwrap();
        text.shape(&mut font_system, metrics, "Menlo", &viewer, &geometry);
        text.shape(&mut font_system, metrics, "Courier", &viewer, &geometry);
        fn family(text: &ViewerText) -> Family<'_> {
            text.title.lines[0].attrs_list().defaults().family
        }
        assert_eq!(
            family(&text),
            Family::Name("Menlo"),
            "same text, no reshape"
        );
        text.invalidate();
        assert!(text.shaped().iter().all(Option::is_none));
        text.shape(&mut font_system, metrics, "Courier", &viewer, &geometry);
        assert_eq!(family(&text), Family::Name("Courier"));
    }

    #[test]
    fn presses_map_to_controls_inside_and_outside() {
        let geometry = media_viewer_geometry(&viewer((2, 3), Some((64, 48))), CELL, CELL).unwrap();
        let at = |rect: Rect4| geometry.hit_test(rect.0 + 1.0, rect.1 + 1.0);
        assert_eq!(at(geometry.previous.unwrap()), MediaViewerHit::Previous);
        assert_eq!(at(geometry.next.unwrap()), MediaViewerHit::Next);
        assert_eq!(at(geometry.close), MediaViewerHit::Close);
        assert_eq!(at(geometry.image.unwrap()), MediaViewerHit::Inside);
        assert_eq!(geometry.hit_test(1.0, 1.0), MediaViewerHit::Outside);
    }

    #[test]
    fn a_pane_too_small_or_a_degenerate_cell_has_no_viewer() {
        let mut small = viewer((1, 1), None);
        small.pane_rect = (0.0, 0.0, 120.0, 600.0);
        assert!(media_viewer_geometry(&small, CELL, CELL).is_none());
        assert!(media_viewer_geometry(&viewer((1, 1), None), (0.0, 20.0), CELL).is_none());
        assert!(media_viewer_geometry(&viewer((1, 1), None), CELL, (f32::NAN, 20.0)).is_none());
    }

    #[test]
    fn checkerboards_are_clipped_alternating_and_bounded() {
        let squares = checker_squares((10.0, 10.0, 30.0, 20.0), 10.0);
        assert_eq!(
            squares,
            vec![
                (10.0, 10.0, 10.0, 10.0),
                (30.0, 10.0, 10.0, 10.0),
                (20.0, 20.0, 10.0, 10.0)
            ]
        );
        let huge = checker_squares((0.0, 0.0, 4096.0, 4096.0), 1.0);
        assert!(huge.len() <= MAX_CHECKER_SQUARES + 64, "{}", huge.len());
        assert!(checker_squares((0.0, 0.0, 10.0, 10.0), f32::NAN).is_empty());
    }
}

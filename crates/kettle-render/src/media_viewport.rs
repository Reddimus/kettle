//! How a lane's rendered item is zoomed and panned. The zoom is relative to
//! the item's fit in the lane, and the view is kept as the image point at
//! the content's center, a fraction of the image's width and height, so it
//! survives the lane being resized or moved to another display. Every
//! change keeps the image placed by the same rule: an axis longer than the
//! content covers it, and a shorter one is centered in it.

use crate::Rect4;

/// The least a lane zooms out, relative to its fit.
pub const MIN_MEDIA_ZOOM: f64 = 0.125;
/// The most a lane zooms in, relative to its fit.
pub const MAX_MEDIA_ZOOM: f64 = 32.0;
/// One zoom step: a button, a key, a wheel notch.
pub const MEDIA_ZOOM_STEP: f64 = 1.2;

/// A lane's zoom and pan.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MediaViewport {
    /// Relative to the fit, within [`MIN_MEDIA_ZOOM`]..=[`MAX_MEDIA_ZOOM`].
    zoom: f64,
    /// The image point at the content's center, as fractions of its width
    /// and height.
    center: (f64, f64),
}

impl Default for MediaViewport {
    fn default() -> Self {
        Self::FIT
    }
}

/// An image's placement before it is rounded to whole pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placed {
    origin: (f64, f64),
    size: (f64, f64),
}

/// Whether `rect` has a finite position and a finite, positive size.
fn usable(rect: Rect4) -> bool {
    [rect.0, rect.1, rect.2, rect.3]
        .iter()
        .all(|v| v.is_finite())
        && rect.2 > 0.0
        && rect.3 > 0.0
}

/// Where an image `size` long starts on one axis: its point `at` (a
/// fraction of it) at `middle`, kept covering `start..start + extent` when
/// it is longer than that, and centered on `middle` when it is not.
fn axis(start: f64, extent: f64, middle: f64, size: f64, at: f64) -> f64 {
    if size <= extent {
        middle - size / 2.0
    } else {
        (middle - at * size).clamp(start + extent - size, start)
    }
}

impl MediaViewport {
    /// The item fitted to the lane.
    pub const FIT: Self = Self {
        zoom: 1.0,
        center: (0.5, 0.5),
    };

    /// The zoom, relative to the fit.
    pub fn zoom(self) -> f64 {
        self.zoom
    }

    /// Whether the item shows as fitted.
    pub fn is_fit(self) -> bool {
        self == Self::FIT
    }

    /// The image placed: `fit` is its fitted rectangle, centered in the
    /// content `area`. Its size is the fit's times the zoom, about the fit's
    /// center, which the content's center shares to within a pixel.
    fn placed(self, fit: Rect4, area: Rect4) -> Placed {
        let size = (f64::from(fit.2) * self.zoom, f64::from(fit.3) * self.zoom);
        let middle = (
            f64::from(fit.0) + f64::from(fit.2) / 2.0,
            f64::from(fit.1) + f64::from(fit.3) / 2.0,
        );
        Placed {
            origin: (
                axis(
                    f64::from(area.0),
                    f64::from(area.2),
                    middle.0,
                    size.0,
                    self.center.0,
                ),
                axis(
                    f64::from(area.1),
                    f64::from(area.3),
                    middle.1,
                    size.1,
                    self.center.1,
                ),
            ),
            size,
        }
    }

    /// The viewport whose center is the image point at the content's center
    /// once `placed`: what an axis kept covering or centered moved it to.
    fn settled(zoom: f64, placed: Placed, fit: Rect4) -> Self {
        let middle = (
            f64::from(fit.0) + f64::from(fit.2) / 2.0,
            f64::from(fit.1) + f64::from(fit.3) / 2.0,
        );
        Self {
            zoom,
            center: (
                (middle.0 - placed.origin.0) / placed.size.0,
                (middle.1 - placed.origin.1) / placed.size.1,
            ),
        }
    }

    /// Where the image is drawn, in whole pixels, with `fit` its fitted
    /// rectangle in the content `area`: `fit` itself while the item shows
    /// fitted. It may reach past `area`, which clips it. `None` for a
    /// rectangle without a finite, positive size.
    pub fn place(self, fit: Rect4, area: Rect4) -> Option<Rect4> {
        if !(usable(fit) && usable(area)) {
            return None;
        }
        let placed = self.placed(fit, area);
        Some((
            placed.origin.0.round() as f32,
            placed.origin.1.round() as f32,
            placed.size.0.round().max(1.0) as f32,
            placed.size.1.round().max(1.0) as f32,
        ))
    }

    /// Zoomed by `factor`, keeping the image point under `anchor` where it
    /// is, except as far as keeping the content covered moves it. The zoom
    /// stays within its bounds; a factor that is not finite and positive
    /// changes nothing.
    pub fn zoomed_at(self, factor: f64, anchor: (f32, f32), fit: Rect4, area: Rect4) -> Self {
        let anchor = (f64::from(anchor.0), f64::from(anchor.1));
        if !(factor.is_finite() && factor > 0.0)
            || !(anchor.0.is_finite() && anchor.1.is_finite())
            || !(usable(fit) && usable(area))
        {
            return self;
        }
        let before = self.placed(fit, area);
        let at = (
            (anchor.0 - before.origin.0) / before.size.0,
            (anchor.1 - before.origin.1) / before.size.1,
        );
        let zoom = (self.zoom * factor).clamp(MIN_MEDIA_ZOOM, MAX_MEDIA_ZOOM);
        let size = (f64::from(fit.2) * zoom, f64::from(fit.3) * zoom);
        let wanted = Placed {
            origin: (anchor.0 - at.0 * size.0, anchor.1 - at.1 * size.1),
            size,
        };
        let center = Self::settled(zoom, wanted, fit).center;
        Self { zoom, center }.clamped(fit, area)
    }

    /// Zoomed by `factor` about the content's center.
    pub fn zoomed(self, factor: f64, fit: Rect4, area: Rect4) -> Self {
        let middle = (area.0 + area.2 / 2.0, area.1 + area.3 / 2.0);
        self.zoomed_at(factor, middle, fit, area)
    }

    /// The image moved by `by` pixels, as far as keeping the content
    /// covered lets it.
    pub fn panned(self, by: (f64, f64), fit: Rect4, area: Rect4) -> Self {
        if !(by.0.is_finite() && by.1.is_finite()) || !(usable(fit) && usable(area)) {
            return self;
        }
        let mut placed = self.placed(fit, area);
        placed.origin = (placed.origin.0 + by.0, placed.origin.1 + by.1);
        Self::settled(self.zoom, placed, fit).clamped(fit, area)
    }

    /// The same view, its center where the placement rule puts it, so the
    /// next change starts from what shows.
    fn clamped(self, fit: Rect4, area: Rect4) -> Self {
        Self::settled(self.zoom, self.placed(fit, area), fit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 200x100 image fitted in a 400x300 content area at (10, 20).
    const AREA: Rect4 = (10.0, 20.0, 400.0, 300.0);
    const FIT: Rect4 = (10.0, 120.0, 400.0, 100.0);

    #[test]
    fn fitted_is_the_fit_itself() {
        let view = MediaViewport::default();
        assert!(view.is_fit());
        assert_eq!(view.place(FIT, AREA), Some(FIT));
        assert_eq!(view.panned((50.0, -30.0), FIT, AREA), MediaViewport::FIT);
        assert_eq!(MediaViewport::FIT.place((0.0, 0.0, 0.0, 5.0), AREA), None);
        assert_eq!(
            MediaViewport::FIT.place(FIT, (0.0, 0.0, f32::NAN, 1.0)),
            None
        );
    }

    /// Zooming at a point keeps the image point under it there; an axis
    /// longer than the content keeps it covered, and a shorter one stays
    /// centered.
    #[test]
    fn zooming_at_a_point_keeps_that_point_under_it() {
        let anchor = (110.0, 170.0);
        let view = MediaViewport::FIT.zoomed_at(2.0, anchor, FIT, AREA);
        assert_eq!(view.zoom(), 2.0);
        let placed = view.place(FIT, AREA).unwrap();
        // 800x200: wider than the content, so it covers it; the point a
        // quarter of the way along stays at x = 110. Its 200 rows fit the
        // content's 300, so they are centered.
        assert_eq!(placed, (-90.0, 70.0, 800.0, 200.0));
        let point = ((110.0 - placed.0) / placed.2, (170.0 - placed.1) / placed.3);
        assert_eq!(point.0, 0.25);
        // At the content's left edge, the image cannot leave a gap there.
        let edge = MediaViewport::FIT.zoomed_at(2.0, (10.0, 170.0), FIT, AREA);
        assert_eq!(edge.place(FIT, AREA).unwrap().0, 10.0);
        // About the center, the center point stays.
        let centered = MediaViewport::FIT.zoomed(4.0, FIT, AREA);
        assert_eq!(
            centered.place(FIT, AREA).unwrap(),
            (-590.0, -30.0, 1600.0, 400.0)
        );
        // Back to 1x about the same point is the fit again.
        assert!(view.zoomed_at(0.5, anchor, FIT, AREA).is_fit());
    }

    /// The zoom stays within its bounds, and nothing that is not a number
    /// changes it.
    #[test]
    fn zoom_is_bounded_and_finite() {
        let mut view = MediaViewport::FIT;
        for _ in 0..100 {
            view = view.zoomed(MEDIA_ZOOM_STEP, FIT, AREA);
        }
        assert_eq!(view.zoom(), MAX_MEDIA_ZOOM);
        for _ in 0..200 {
            view = view.zoomed(1.0 / MEDIA_ZOOM_STEP, FIT, AREA);
        }
        assert_eq!(view.zoom(), MIN_MEDIA_ZOOM);
        let small = view.place(FIT, AREA).unwrap();
        assert_eq!(small, (185.0, 164.0, 50.0, 13.0), "centered");
        for factor in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
            assert_eq!(view.zoomed(factor, FIT, AREA), view);
        }
        assert_eq!(view.zoomed_at(2.0, (f32::NAN, 0.0), FIT, AREA), view);
        assert_eq!(view.panned((f64::NAN, 0.0), FIT, AREA), view);
    }

    /// Panning moves the image as far as keeping the content covered lets
    /// it, and a pan back from an edge takes effect at once.
    #[test]
    fn panning_stops_at_the_edges() {
        let view = MediaViewport::FIT.zoomed(4.0, FIT, AREA);
        let moved = view.panned((100.0, 10.0), FIT, AREA);
        assert_eq!(
            moved.place(FIT, AREA).unwrap(),
            (-490.0, -20.0, 1600.0, 400.0)
        );
        let far = view.panned((10_000.0, 10_000.0), FIT, AREA);
        assert_eq!(far.place(FIT, AREA).unwrap(), (10.0, 20.0, 1600.0, 400.0));
        let back = far.panned((-5.0, -5.0), FIT, AREA);
        assert_eq!(back.place(FIT, AREA).unwrap(), (5.0, 15.0, 1600.0, 400.0));
    }

    /// The view is the point at the content's center, so a resized lane
    /// keeps showing the same part of the image.
    #[test]
    fn a_resized_lane_keeps_its_center_point() {
        let view = MediaViewport::FIT
            .zoomed(4.0, FIT, AREA)
            .panned((300.0, 0.0), FIT, AREA);
        let placed = view.place(FIT, AREA).unwrap();
        let center = |placed: Rect4, area: Rect4| (area.0 + area.2 / 2.0 - placed.0) / placed.2;
        let before = center(placed, AREA);
        // Twice as wide: the fit doubles, and the same point is centered.
        let (wide_area, wide_fit) = ((10.0, 20.0, 800.0, 600.0), (10.0, 120.0, 800.0, 400.0));
        let after = center(view.place(wide_fit, wide_area).unwrap(), wide_area);
        assert!((after - before).abs() < 1e-3, "{before} {after}");
    }
}

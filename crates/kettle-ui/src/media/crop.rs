//! Sharper pixels for a zoomed preview. A lane shows its item from the
//! pixels it holds; zoomed past them, it asks the worker for the part in
//! view at the size shown. The worker fits the whole image in a target box
//! and returns the crop of that box asked for, saying where its pixels sit,
//! so the lane knows what part of the image they cover whatever the worker
//! rounded. Those pixels belong to the lane: they never replace the item's
//! own, and copying the item copies its own.

use std::time::{Duration, Instant};

use kettle_media::{
    Crop, MAX_RENDERED_EDGE, MAX_SVG_RENDERED_EDGE, MAX_SVG_RENDERED_PIXELS, MediaKind,
    RenderLayout, Target,
};

/// How long a lane's view stays still before it asks.
pub(crate) const CROP_QUIET: Duration = Duration::from_millis(75);
/// How long a view that keeps changing waits at most.
pub(crate) const CROP_LATEST: Duration = Duration::from_millis(150);
/// How much more detail than what a lane holds is worth a render.
const WORTH: f64 = 1.25;
/// Pixels asked for around the part in view, for the worker's rounding.
const GUARD: u32 = 2;
/// Most times a plan shrinks to meet the SVG ceiling.
const MAX_SHRINKS: usize = 64;

/// A part of an image, as fractions of its width and height: left, top,
/// right and bottom. A crop's may reach past the image, over the target
/// box's padding.
pub(crate) type Fraction = (f64, f64, f64, f64);

/// The worker target that shows the part of an item in view, `visible`,
/// sharper. The item was rendered as `layout` into `held` pixels and shows
/// `shown` pixels large, the whole image's size on screen. `None` when
/// nothing sharper can come of a render: the item shows no larger than its
/// pixels give or take a quarter, a raster holds all its own pixels, or the
/// kind has no crop.
///
/// The box takes the image's own shape: its longer side is the size shown,
/// and the other is rounded up, so the worker fits the image to the longer
/// side and fills the box with it, whatever the pixels on screen rounded
/// to. The part in view is then the same part of the box.
pub(crate) fn plan_crop(
    kind: MediaKind,
    layout: &RenderLayout,
    held: (u32, u32),
    shown: (f64, f64),
    visible: Fraction,
    scale: f64,
) -> Option<Target> {
    let (source_width, source_height) = (layout.source_width, layout.source_height);
    let usable = |value: f64| value.is_finite() && value > 0.0;
    if !matches!(
        kind,
        MediaKind::Raster | MediaKind::Svg | MediaKind::Mermaid
    ) || !(usable(source_width) && usable(source_height))
    {
        return None;
    }
    let wide = source_width >= source_height;
    let (along, across, shown_along, held_along) = if wide {
        (source_width, source_height, shown.0, held.0)
    } else {
        (source_height, source_width, shown.1, held.1)
    };
    let held_along = f64::from(held_along);
    if !(shown_along.is_finite() && shown_along >= 1.0) {
        return None;
    }
    // Within the largest target on both sides; a raster is never enlarged
    // past its own pixels.
    let edge = f64::from(MAX_RENDERED_EDGE);
    let mut size = shown_along.min(edge).min(edge * along / across);
    if kind == MediaKind::Raster {
        size = size.min(along);
    }
    for _ in 0..MAX_SHRINKS {
        if !(size.is_finite() && size >= 1.0) {
            return None;
        }
        let long = size.floor() as u32;
        // Worth asking only for more detail than the lane holds.
        if f64::from(long) <= held_along * WORTH {
            return None;
        }
        let short = (f64::from(long) * across / along).ceil().clamp(1.0, edge) as u32;
        let (width, height) = if wide { (long, short) } else { (short, long) };
        let crop = crop_in(width, height, visible)?;
        if kind != MediaKind::Raster && !within_svg_ceiling(crop) {
            // An SVG's crop is held to its ceiling; a smaller box keeps the
            // same part in view at less detail.
            let (cw, ch) = (f64::from(crop.width), f64::from(crop.height));
            let ceiling = f64::from(MAX_SVG_RENDERED_EDGE);
            let shrink = (ceiling / cw)
                .min(ceiling / ch)
                .min((MAX_SVG_RENDERED_PIXELS as f64 / (cw * ch)).sqrt());
            size *= shrink.min(0.99);
            continue;
        }
        let target = Target {
            width,
            height,
            scale,
            crop: Some(crop),
        };
        return target.validate().is_ok().then_some(target);
    }
    None
}

/// The part of a `width` x `height` box in view, `visible`, with a guard
/// around it, in the box's pixels.
fn crop_in(width: u32, height: u32, visible: Fraction) -> Option<Crop> {
    let (left, top, right, bottom) = (
        visible.0.clamp(0.0, 1.0),
        visible.1.clamp(0.0, 1.0),
        visible.2.clamp(0.0, 1.0),
        visible.3.clamp(0.0, 1.0),
    );
    if !(right > left && bottom > top) {
        return None;
    }
    let span = |from: f64, to: f64, length: u32| {
        let length_f = f64::from(length);
        let start = ((from * length_f).floor() as u32).saturating_sub(GUARD);
        let end = ((to * length_f).ceil() as u32)
            .saturating_add(GUARD)
            .min(length);
        (end > start).then_some((start, end - start))
    };
    let (x, crop_width) = span(left, right, width)?;
    let (y, crop_height) = span(top, bottom, height)?;
    Some(Crop {
        x,
        y,
        width: crop_width,
        height: crop_height,
    })
}

fn within_svg_ceiling(crop: Crop) -> bool {
    crop.width <= MAX_SVG_RENDERED_EDGE
        && crop.height <= MAX_SVG_RENDERED_EDGE
        && u64::from(crop.width) * u64::from(crop.height) <= MAX_SVG_RENDERED_PIXELS
}

/// The part of the image a reply's pixels cover, from where it says they
/// sit in the target box. `None` for a layout that places nothing.
pub(crate) fn coverage(layout: &RenderLayout) -> Option<Fraction> {
    let image = layout.image_in_target;
    let result = layout.result_in_target;
    let (width, height) = (f64::from(image.width), f64::from(image.height));
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let along = |at: u32, from: u32, length: f64| (f64::from(at) - f64::from(from)) / length;
    let left = along(result.x, image.x, width);
    let top = along(result.y, image.y, height);
    let right = left + f64::from(result.width) / width;
    let bottom = top + f64::from(result.height) / height;
    (right > left && bottom > top).then_some((left, top, right, bottom))
}

/// Whether `coverage` holds all of `visible`.
pub(crate) fn covers(coverage: Fraction, visible: Fraction) -> bool {
    const SLACK: f64 = 1e-6;
    visible.0 >= coverage.0 - SLACK
        && visible.1 >= coverage.1 - SLACK
        && visible.2 <= coverage.2 + SLACK
        && visible.3 <= coverage.3 + SLACK
}

/// Sharper pixels a lane holds for its item.
#[derive(Clone)]
pub(crate) struct LaneTile {
    pub item: u64,
    pub generation: u64,
    /// The canvas they were rendered on: a diagram's colors follow it.
    pub canvas: kettle_media::Canvas,
    pub image: kettle_core::ImageData,
    /// The part of the image they cover.
    pub coverage: Fraction,
    /// How many of their pixels the whole image's width would take.
    pub image_width: u32,
}

impl LaneTile {
    /// Whether these pixels are for `item` at `generation`, on `canvas`.
    pub(crate) fn serves(&self, item: u64, generation: u64, canvas: kettle_media::Canvas) -> bool {
        self.item == item && self.generation == generation && self.canvas == canvas
    }

    /// Whether they show `visible` with about the detail `target` would.
    pub(crate) fn suffices(&self, visible: Fraction, target: &Target) -> bool {
        covers(self.coverage, visible)
            && f64::from(self.image_width) * WORTH >= f64::from(target.width)
    }
}

/// A lane's sharper pixels, and the render that asks for them.
#[derive(Clone, Default)]
pub(crate) struct LaneCrop {
    /// When to ask, a gesture coalesced: once its view is quiet, or at the
    /// latest.
    due: Option<(Instant, Instant)>,
    /// Whether a control client changed the view last: no file is read for
    /// that view, whatever asks next.
    view_by_control: bool,
    /// Whether a control client's action led to the waiting ask.
    due_by_control: bool,
    /// The ticket of the render asked for last, until it comes back.
    pub asked: Option<u64>,
    pub tile: Option<LaneTile>,
}

impl LaneCrop {
    /// The view changed at `now`, by a control client when `control`: the
    /// latest change decides whose view it is.
    pub(crate) fn view_changed(&mut self, now: Instant, control: bool) {
        self.view_by_control = control;
        self.due_by_control = control;
        self.schedule(now);
    }

    /// What shows of the view may have changed at `now` though the view did
    /// not: the lane moved, or the item's pixels did. A control client's
    /// action (`control`) never makes the view the user's.
    pub(crate) fn again(&mut self, now: Instant, control: bool) {
        self.due_by_control |= control;
        self.schedule(now);
    }

    fn schedule(&mut self, now: Instant) {
        let latest = self.due.map_or(now + CROP_LATEST, |(_, latest)| latest);
        self.due = Some((now + CROP_QUIET, latest));
    }

    /// When to ask next, if anything is waiting.
    pub(crate) fn due(&self) -> Option<Instant> {
        self.due.map(|(quiet, latest)| quiet.min(latest))
    }

    /// Take the waiting ask if it is due at `now`, with whether a control
    /// client changed the view last or led to the ask: then it reads no
    /// file.
    pub(crate) fn take_due(&mut self, now: Instant) -> Option<bool> {
        let due = self.due()?;
        (due <= now).then(|| {
            self.due = None;
            std::mem::take(&mut self.due_by_control) || self.view_by_control
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(source: (f64, f64), image: Crop, result: Crop) -> RenderLayout {
        RenderLayout {
            source_width: source.0,
            source_height: source.1,
            image_in_target: image,
            result_in_target: result,
        }
    }

    fn crop(x: u32, y: u32, width: u32, height: u32) -> Crop {
        Crop {
            x,
            y,
            width,
            height,
        }
    }

    /// A 4000x2000 photo held as 1000x500: shown 4000x2000, its left half in
    /// view, the box is the size shown and the crop its left half with a
    /// guard; shown no larger than its pixels, or a quarter more, nothing is
    /// asked.
    #[test]
    fn a_zoomed_raster_asks_for_the_part_in_view_at_the_size_shown() {
        let canonical = crop(0, 0, 1000, 500);
        let photo = layout((4000.0, 2000.0), canonical, canonical);
        let target = plan_crop(
            MediaKind::Raster,
            &photo,
            (1000, 500),
            (4000.0, 2000.0),
            (0.0, 0.25, 0.5, 0.75),
            2.0,
        )
        .expect("sharper");
        assert_eq!(
            (target.width, target.height, target.scale),
            (4000, 2000, 2.0)
        );
        assert_eq!(target.crop, Some(crop(0, 498, 2002, 1004)));
        for shown in [(1000.0, 500.0), (1250.0, 625.0)] {
            assert_eq!(
                plan_crop(
                    MediaKind::Raster,
                    &photo,
                    (1000, 500),
                    shown,
                    (0.0, 0.0, 1.0, 1.0),
                    1.0
                ),
                None
            );
        }
    }

    /// Where a raster's worker puts the whole image in a `width` x `height`
    /// box: fitted, never enlarged, centered, as kettle-media-render's
    /// `raster::fit` rounds it.
    fn raster_fit(source: (f64, f64), width: u32, height: u32) -> Crop {
        let scale = (f64::from(width) / source.0)
            .min(f64::from(height) / source.1)
            .min(1.0);
        let edge = |length: f64, limit: u32| ((length * scale).round().max(1.0) as u32).min(limit);
        let (fitted_width, fitted_height) = (edge(source.0, width), edge(source.1, height));
        crop(
            (width - fitted_width) / 2,
            (height - fitted_height) / 2,
            fitted_width,
            fitted_height,
        )
    }

    /// The pixels shown round the image's shape: an 8000x10 raster held as
    /// 1024x1 and shown 4096x4 asks for a box of the image's own shape, so
    /// the worker's fit fills it and the reply covers the quarter in view.
    #[test]
    fn the_box_takes_the_images_shape_whatever_the_screen_rounded() {
        let held = crop(0, 0, 1024, 1);
        let strip = layout((8000.0, 10.0), held, held);
        let visible = (0.75, 0.0, 1.0, 1.0);
        let target = plan_crop(
            MediaKind::Raster,
            &strip,
            (1024, 1),
            (4096.0, 4.0),
            visible,
            1.0,
        )
        .unwrap();
        assert_eq!((target.width, target.height), (4096, 6));
        let asked = target.crop.unwrap();
        let reply = layout(
            (8000.0, 10.0),
            raster_fit((8000.0, 10.0), target.width, target.height),
            asked,
        );
        assert!(covers(coverage(&reply).unwrap(), visible), "{reply:?}");
        // A tall one takes its height from the size shown.
        let tall = layout((10.0, 8000.0), crop(0, 0, 1, 1024), crop(0, 0, 1, 1024));
        let target = plan_crop(
            MediaKind::Raster,
            &tall,
            (1, 1024),
            (4.0, 4096.0),
            (0.0, 0.5, 1.0, 1.0),
            1.0,
        )
        .unwrap();
        assert_eq!((target.width, target.height), (6, 4096));
    }

    /// A raster is never asked for past its own pixels: shown at eight
    /// times a 500x250 image, the box is the image's size; one held at its
    /// own size has nothing sharper to give.
    #[test]
    fn a_raster_is_asked_for_no_more_than_its_own_pixels() {
        let held = crop(0, 0, 250, 125);
        let small = layout((500.0, 250.0), held, held);
        let target = plan_crop(
            MediaKind::Raster,
            &small,
            (250, 125),
            (2000.0, 1000.0),
            (0.25, 0.25, 0.75, 0.75),
            1.0,
        )
        .unwrap();
        assert_eq!((target.width, target.height), (500, 250));
        // An 8000x4000 photo shown whole at its size is asked for in the
        // largest box, 4096 on an edge.
        let big = layout(
            (8000.0, 4000.0),
            crop(0, 0, 1024, 512),
            crop(0, 0, 1024, 512),
        );
        let target = plan_crop(
            MediaKind::Raster,
            &big,
            (1024, 512),
            (8000.0, 4000.0),
            (0.0, 0.0, 1.0, 1.0),
            1.0,
        )
        .unwrap();
        assert_eq!((target.width, target.height), (4096, 2048));
        assert_eq!(target.crop, Some(crop(0, 0, 4096, 2048)));
        let native = crop(0, 0, 500, 250);
        let whole = layout((500.0, 250.0), native, native);
        assert_eq!(
            plan_crop(
                MediaKind::Raster,
                &whole,
                (500, 250),
                (4000.0, 2000.0),
                (0.0, 0.0, 0.1, 0.1),
                1.0
            ),
            None
        );
    }

    /// An SVG's crop stays within its ceiling: the box shrinks until the
    /// part in view fits 1024x1024 pixels and a million in all, keeping the
    /// same part in view; the largest box is 4096 on an edge.
    #[test]
    fn an_svg_crop_shrinks_to_its_ceiling_and_the_box_to_its_edge() {
        let held = crop(0, 0, 1024, 512);
        let diagram = layout((200.0, 100.0), held, held);
        let target = plan_crop(
            MediaKind::Svg,
            &diagram,
            (1024, 512),
            (8192.0, 4096.0),
            (0.25, 0.25, 0.75, 0.75),
            1.0,
        )
        .unwrap();
        assert!(target.width <= MAX_RENDERED_EDGE && target.height <= MAX_RENDERED_EDGE);
        let crop = target.crop.unwrap();
        assert!(within_svg_ceiling(crop), "{crop:?}");
        assert!(target.validate().is_ok());
        // The part in view is still the middle half.
        let middle = f64::from(crop.x + crop.width / 2) / f64::from(target.width);
        assert!((middle - 0.5).abs() < 0.01, "{middle}");
        assert!(f64::from(target.width) > 1024.0 * WORTH);
        // A mermaid diagram is held to the same ceiling.
        let mermaid = plan_crop(
            MediaKind::Mermaid,
            &diagram,
            (1024, 512),
            (8192.0, 4096.0),
            (0.25, 0.25, 0.75, 0.75),
            1.0,
        )
        .unwrap();
        assert!(within_svg_ceiling(mermaid.crop.unwrap()));
        // Nothing else has a crop, and a size that is no number asks nothing.
        for (kind, shown) in [
            (MediaKind::Video, (8192.0, 4096.0)),
            (MediaKind::Svg, (f64::NAN, 4096.0)),
            (MediaKind::Svg, (f64::INFINITY, 4096.0)),
        ] {
            assert_eq!(
                plan_crop(
                    kind,
                    &diagram,
                    (1024, 512),
                    shown,
                    (0.0, 0.0, 1.0, 1.0),
                    1.0
                ),
                None
            );
        }
        assert_eq!(
            plan_crop(
                MediaKind::Svg,
                &diagram,
                (1024, 512),
                (8192.0, 4096.0),
                (0.5, 0.5, 0.5, 0.9),
                1.0
            ),
            None,
            "nothing in view"
        );
    }

    /// What a reply covers comes from where it says its pixels sit: the
    /// crop's place in the box against the image's, reaching past the image
    /// over the box's padding; a part in view is covered only within it.
    #[test]
    fn a_reply_covers_what_its_layout_says() {
        let reply = layout((100.0, 50.0), crop(10, 0, 400, 200), crop(110, 50, 100, 50));
        assert_eq!(coverage(&reply), Some((0.25, 0.25, 0.5, 0.5)));
        let padded = layout((100.0, 50.0), crop(10, 0, 400, 200), crop(0, 0, 420, 200));
        let (left, _, right, _) = coverage(&padded).unwrap();
        assert!(left < 0.0 && right > 1.0);
        let empty = layout((1.0, 1.0), crop(0, 0, 0, 1), crop(0, 0, 1, 1));
        assert_eq!(coverage(&empty), None);
        assert!(covers((0.25, 0.25, 0.5, 0.5), (0.3, 0.3, 0.5, 0.45)));
        assert!(!covers((0.25, 0.25, 0.5, 0.5), (0.3, 0.3, 0.51, 0.45)));
        assert!(!covers((0.25, 0.25, 0.5, 0.5), (0.2, 0.3, 0.5, 0.45)));
    }

    /// Sharper pixels serve only the item, generation and canvas they were
    /// rendered for, and suffice while they cover the view with about the
    /// detail asked for.
    #[test]
    fn a_tile_serves_its_own_render_and_suffices_while_it_covers() {
        let tile = LaneTile {
            item: 4,
            generation: 2,
            canvas: kettle_media::Canvas::Theme,
            image: kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap(),
            coverage: (0.25, 0.25, 0.75, 0.75),
            image_width: 2000,
        };
        assert!(tile.serves(4, 2, kettle_media::Canvas::Theme));
        assert!(!tile.serves(4, 3, kettle_media::Canvas::Theme), "replaced");
        assert!(!tile.serves(5, 2, kettle_media::Canvas::Theme), "another");
        assert!(!tile.serves(4, 2, kettle_media::Canvas::White), "recolored");
        let target = |width| Target {
            width,
            height: 1000,
            scale: 1.0,
            crop: Some(crop(0, 0, 1, 1)),
        };
        assert!(tile.suffices((0.3, 0.3, 0.7, 0.7), &target(2500)));
        assert!(
            !tile.suffices((0.3, 0.3, 0.7, 0.7), &target(2501)),
            "zoomed further"
        );
        assert!(
            !tile.suffices((0.2, 0.3, 0.7, 0.7), &target(2000)),
            "panned away"
        );
    }

    /// A gesture is asked about once it is quiet for a moment, or at the
    /// latest a little after it began however long it goes on. Whoever
    /// changed the view last decides whether it reads a file: a control
    /// client's view stays its own through the lane's other changes, and a
    /// control client's action taints the ask it leads to.
    #[test]
    fn a_gesture_is_asked_about_once_quiet_or_at_the_latest() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut crop = LaneCrop::default();
        assert_eq!(crop.due(), None);
        crop.view_changed(start, false);
        assert_eq!(crop.due(), Some(start + CROP_QUIET));
        assert_eq!(crop.take_due(start + CROP_QUIET / 2), None);
        // Still changing: the quiet deadline moves, the latest does not.
        crop.view_changed(at(60), true);
        crop.view_changed(at(120), false);
        assert_eq!(crop.due(), Some(start + CROP_LATEST));
        assert_eq!(
            crop.take_due(start + CROP_LATEST),
            Some(false),
            "the user's"
        );
        assert_eq!(crop.due(), None);
        // A control client's view: a later change of the lane by the user,
        // a copy or a resize, leaves it the control client's.
        crop.view_changed(at(200), true);
        assert_eq!(crop.take_due(at(400)), Some(true));
        crop.again(at(500), false);
        assert_eq!(crop.take_due(at(700)), Some(true), "still its view");
        // The user's view, the lane changed by a control client: that ask
        // reads nothing, and the next one may.
        crop.view_changed(at(800), false);
        crop.again(at(810), true);
        assert_eq!(crop.take_due(at(1000)), Some(true));
        crop.again(at(1100), false);
        assert_eq!(crop.take_due(at(1300)), Some(false));
    }
}

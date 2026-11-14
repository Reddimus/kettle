//! The GNOME header's lone title: GNOME's bold interface face at its title
//! size, fitted to the title's lane with an ellipsis and centred on the
//! header.
//!
//! The face is the bundled Fira Sans Bold, registered under a family name no
//! installed font has, so the title never matches a system font: local fonts
//! have hidden CI render differences before. Characters it lacks fall back as
//! every other chrome string's do.
use crate::{Rect4, Renderer, TabSeg};
use glyphon::{Attrs, Buffer as TextBuffer, Family, FontSystem, Metrics, Shaping, Weight, fontdb};

/// The family the bundled title face is registered under.
pub(crate) const HEADER_TITLE_FAMILY: &str = "Kettle Header Title";

/// Fira Sans Bold 4.301 (assets/fonts/fira-sans). Only targets that draw the
/// GNOME header carry it.
#[cfg(any(test, target_os = "linux"))]
const HEADER_TITLE_FACE: Option<&[u8]> = Some(include_bytes!(
    "../../../assets/fonts/fira-sans/FiraSans-Bold.ttf"
));
#[cfg(not(any(test, target_os = "linux")))]
const HEADER_TITLE_FACE: Option<&[u8]> = None;

/// GNOME's interface text size, 11 pt at 96 DPI, in logical pixels. A
/// header title does not zoom with the terminal.
pub(crate) const HEADER_TITLE_SIZE: f32 = 11.0 * 96.0 / 72.0;

/// The title's line as a multiple of its size: Fira Sans's ascent and
/// descent (935 and 265 per 1000), so the line is the face's logical extent
/// and centring the line centres the text, as GTK centres a label.
const HEADER_TITLE_LINE: f32 = 1.2;

/// Register the bundled title face in `font_system` under
/// [`HEADER_TITLE_FAMILY`]. False on a target that does not carry it.
pub(crate) fn load_header_title_face(font_system: &mut FontSystem) -> bool {
    let Some(face) = HEADER_TITLE_FACE else {
        return false;
    };
    let db = font_system.db_mut();
    let ids = db.load_font_source(fontdb::Source::Binary(std::sync::Arc::new(face)));
    for id in ids {
        let Some(mut info) = db.face(id).cloned() else {
            continue;
        };
        db.remove_face(id);
        info.families = vec![(
            HEADER_TITLE_FAMILY.to_owned(),
            fontdb::Language::English_UnitedStates,
        )];
        db.push_face_info(info);
    }
    true
}

/// The title's text metrics at the display's `scale`.
pub(crate) fn header_title_metrics(scale: f32) -> Metrics {
    let size = HEADER_TITLE_SIZE * scale.max(0.5);
    Metrics::new(size, (size * HEADER_TITLE_LINE).ceil())
}

/// The title's attributes: the bundled face in bold, or `fallback`, the
/// terminal's family, on a target without it.
pub(crate) fn header_title_attrs(loaded: bool, fallback: &str) -> Attrs<'_> {
    Attrs::new()
        .family(Family::Name(if loaded {
            HEADER_TITLE_FAMILY
        } else {
            fallback
        }))
        .weight(Weight::BOLD)
}

/// The advance of `text` on one line in `attrs` at `metrics`, shaped in
/// `scratch`.
pub(crate) fn measure_line(
    font_system: &mut FontSystem,
    scratch: &mut TextBuffer,
    text: &str,
    attrs: &Attrs,
    metrics: Metrics,
) -> f32 {
    scratch.set_metrics(metrics);
    scratch.set_wrap(glyphon::Wrap::None);
    scratch.set_size(None, None);
    scratch.set_text(text, attrs, Shaping::Advanced, None);
    scratch.shape_until_scroll(font_system, false);
    scratch
        .layout_runs()
        .map(|run| run.line_w)
        .fold(0.0, f32::max)
}

/// The lone title's label, fitted to `lane_width` as `measure` measures
/// text: the tab's label in the `format` the tab strip uses, shortened as a
/// tab's is (a directory keeps its leaf, other titles lose their middle) to
/// the longest that fits. `notice` is a recording notice that leads it.
pub(crate) fn fit_header_title(
    seg: &TabSeg,
    format: &str,
    notice: &str,
    lane_width: f32,
    mut measure: impl FnMut(&str) -> f32,
) -> String {
    let label = |cols: usize| {
        // One unit per column, so the tab strip's fitting picks the text.
        let seg = TabSeg {
            idx: seg.idx,
            rect: seg.rect,
            title_rect: (0.0, 0.0, cols as f32, 0.0),
            hint: None,
            close: seg.close,
            title: seg.title.clone(),
            path: seg.path.clone(),
            active: seg.active,
            activity: seg.activity,
        };
        crate::labels::tab_segment_label(&seg, format, 1.0, notice)
    };
    let full = label(usize::MAX / 4);
    if measure(&full) <= lane_width {
        return full;
    }
    // The widest label that fits; the label only grows with its columns.
    let (mut lo, mut hi) = (0, crate::labels::display_width(&full));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if measure(&label(mid)) <= lane_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    label(lo)
}

/// What the shaped title was fitted from: the tab's title and path, the
/// label format and notice, the lane's width, the display's scale, and the
/// face it was shaped in. A frame with the same key reuses the shaping.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HeaderTitleKey {
    title: String,
    path: Option<String>,
    format: String,
    notice: String,
    lane: u32,
    scale: u32,
    family: String,
}

impl Renderer {
    /// Shape the lone title `seg` into `header_title_buffer`, fitted to its
    /// lane in GNOME's title face, and note its width. `fallback` is the
    /// terminal's family, used only where the title face is not carried.
    pub(crate) fn prepare_header_title(
        &mut self,
        seg: &TabSeg,
        format: &str,
        notice: &str,
        fallback: &str,
    ) {
        let loaded = match self.header_title_face {
            Some(loaded) => loaded,
            None => {
                let loaded = load_header_title_face(&mut self.font_system);
                self.header_title_face = Some(loaded);
                loaded
            }
        };
        let attrs = header_title_attrs(loaded, fallback);
        let metrics = header_title_metrics(self.scale);
        let key = HeaderTitleKey {
            title: seg.title.clone(),
            path: seg.path.clone(),
            format: format.to_owned(),
            notice: notice.to_owned(),
            lane: seg.title_rect.2.to_bits(),
            scale: self.scale.to_bits(),
            family: if loaded {
                HEADER_TITLE_FAMILY.to_owned()
            } else {
                fallback.to_owned()
            },
        };
        if self.header_title_key.as_ref() == Some(&key) {
            return;
        }
        let (fonts, scratch) = (&mut self.font_system, &mut self.header_title_scratch);
        let fitted = fit_header_title(seg, format, notice, seg.title_rect.2, |text| {
            measure_line(fonts, scratch, text, &attrs, metrics)
        });
        self.header_title_width = measure_line(
            &mut self.font_system,
            &mut self.header_title_buffer,
            &fitted,
            &attrs,
            metrics,
        );
        self.header_title_key = Some(key);
    }
}

/// Where a title `width` wide draws in `lane`: centred, or from the lane's
/// start when it fills it, with its line of `line_height` centred on the
/// lane's height.
pub(crate) fn header_title_origin(lane: Rect4, width: f32, line_height: f32) -> (f32, f32) {
    let (x, y, w, h) = lane;
    (
        (x + ((w - width) * 0.5).max(0.0)).round(),
        (y + (h - line_height) * 0.5).round(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled_only() -> FontSystem {
        let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
        // The terminal's face too, as the renderer has it, so a title that
        // fell back would find a face and could not pass by having none.
        crate::fonts::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
        assert!(load_header_title_face(&mut fonts));
        fonts
    }

    fn seg(title: &str, path: Option<&str>) -> TabSeg {
        TabSeg {
            idx: 0,
            rect: (0.0, 0.0, 400.0, 46.0),
            title_rect: (8.0, 0.0, 384.0, 46.0),
            hint: None,
            close: (0.0, 0.0, 0.0, 0.0),
            title: title.into(),
            path: path.map(Into::into),
            active: true,
            activity: crate::TabActivity::Normal,
        }
    }

    /// The title shapes in the bundled Fira Sans Bold and in nothing else:
    /// the face is registered under its own family, so no installed font of
    /// the same name can stand in, and the terminal's family is not it.
    #[test]
    fn the_title_shapes_in_the_bundled_bold_sans_face() {
        let mut fonts = bundled_only();
        let attrs = header_title_attrs(true, kettle_config::font::FAMILY);
        let mut buffer = TextBuffer::new(&mut fonts, header_title_metrics(1.0));
        buffer.set_text("fixture ~/project", &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut fonts, false);
        let mut glyphs = 0;
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let face = fonts.db().face(glyph.font_id).expect("a face");
                assert_eq!(face.post_script_name, "FiraSans-Bold");
                assert_eq!(face.weight, fontdb::Weight::BOLD);
                assert_eq!(face.families[0].0, HEADER_TITLE_FAMILY);
                assert!(!face.monospaced);
                glyphs += 1;
            }
        }
        assert!(glyphs > 0);
        // Nothing else answers to the family name.
        let named = fonts
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name == HEADER_TITLE_FAMILY)
            })
            .count();
        assert_eq!(named, 1);
        assert!(
            fonts
                .db()
                .faces()
                .all(|face| face.families.iter().all(|(name, _)| name != "Fira Sans"))
        );
    }

    /// GNOME's title size, 11 pt (14.67 px), at 1x and scaled, on a line
    /// as tall as the face's logical extent.
    #[test]
    fn the_title_is_gnome_size_and_scales() {
        for scale in [1.0_f32, 1.5, 2.0] {
            let metrics = header_title_metrics(scale);
            assert!((metrics.font_size - 14.666_667 * scale).abs() < 1e-3);
            assert_eq!(metrics.line_height, (metrics.font_size * 1.2).ceil());
            // The line fits GNOME's 46-point header with room to spare.
            assert!(metrics.line_height <= 46.0 * scale * 0.5);
        }
    }

    /// A long title loses its end or middle, as a tab's does, until its
    /// measured width fits the lane; a short one is left whole. The fitted
    /// text, measured in the real face, is never wider than the lane.
    #[test]
    fn a_long_title_ellipsizes_to_its_measured_lane() {
        let mut fonts = bundled_only();
        let attrs = header_title_attrs(true, kettle_config::font::FAMILY);
        for scale in [1.0_f32, 2.0] {
            let metrics = header_title_metrics(scale);
            let mut scratch = TextBuffer::new(&mut fonts, metrics);
            let mut width =
                |text: &str| measure_line(&mut fonts, &mut scratch, text, &attrs, metrics);
            for (title, path) in [
                ("fixture", None),
                (
                    "a very long window title that a program set for the whole window",
                    None,
                ),
                ("", Some("~/src/some/deeply/nested/project-directory-name")),
                ("WWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWWW", None),
            ] {
                let seg = seg(title, path);
                for lane in [60.0, 160.0, 384.0] {
                    let lane = lane * scale;
                    let fitted = fit_header_title(&seg, "{title}", "", lane, &mut width);
                    let w = width(&fitted);
                    assert!(w <= lane, "{fitted:?} is {w} in {lane}");
                    let whole = path.unwrap_or(title);
                    if width(whole) <= lane {
                        assert_eq!(fitted, whole);
                    } else {
                        assert_ne!(fitted, whole);
                        // Shortened, not emptied: an ellipsis always fits.
                        assert!(!fitted.is_empty(), "{whole:?} in {lane}");
                    }
                }
            }
        }
    }

    /// The fit tries the tab strip's own shortenings and keeps the widest
    /// one the lane holds: a directory keeps its leaf.
    #[test]
    fn a_fitted_directory_keeps_its_leaf() {
        let seg = seg("", Some("~/src/kettle/crates/kettle-render"));
        // Every character is ten units wide.
        let width = |text: &str| crate::labels::display_width(text) as f32 * 10.0;
        let fitted = fit_header_title(&seg, "{title}", "", 150.0, width);
        assert_eq!(fitted, "kettle-render");
        let fitted = fit_header_title(&seg, "{title}", "", 1000.0, width);
        assert_eq!(fitted, "~/src/kettle/crates/kettle-render");
    }

    /// The title sits centred in its lane on whole pixels, its line centred
    /// on the header's height, and starts at the lane's edge when it fills
    /// the lane.
    #[test]
    fn the_title_centres_on_the_header() {
        for scale in [1.0_f32, 2.0] {
            let lane = (300.0 * scale, 0.0, 400.0 * scale, 46.0 * scale);
            let line = header_title_metrics(scale).line_height;
            let (x, y) = header_title_origin(lane, 100.0 * scale, line);
            assert!((x + 50.0 * scale - (lane.0 + lane.2 * 0.5)).abs() <= 0.5);
            assert!((y + line * 0.5 - lane.3 * 0.5).abs() <= 0.5);
            assert_eq!((x.fract(), y.fract()), (0.0, 0.0));
            assert_eq!(header_title_origin(lane, 500.0 * scale, line).0, lane.0);
        }
    }
}

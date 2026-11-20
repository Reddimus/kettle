//! Portable integration of joining metadata with the real glyph emitter.
use crate::batching::{GlyphSource, emit_positioned_glyphs};
use crate::geometry::GridGlyphCell;
use crate::glyphpipe::GlyphSlot;
use crate::*;
use glyphon::cosmic_text::{Attrs, AttrsList, Family};
use kettle_core::bidi::Joins;

struct Source {
    connectors: usize,
    available: bool,
}
impl GlyphSource for &mut Source {
    fn slot(&mut self, _: glyphon::cosmic_text::CacheKey) -> Option<GlyphSlot> {
        Some(GlyphSlot {
            kind: 1,
            atlas_x: 0.0,
            atlas_y: 0.0,
            w: 4.0,
            h: 5.0,
            left: 0,
            top: 4,
        })
    }
    fn connector(&mut self, _: glyphon::cosmic_text::CacheKey) -> Option<GlyphSlot> {
        self.connectors += 1;
        self.available.then_some(GlyphSlot {
            kind: 1,
            atlas_x: 100.0,
            atlas_y: 0.0,
            w: 5.0,
            h: 2.0,
            left: 0,
            top: 2,
        })
    }
}

fn fixture() -> (FontSystem, TextBuffer) {
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let mut buf = TextBuffer::new(&mut fonts, Metrics::new(16.0, 30.0));
    buf.set_size(Some(1000.0), Some(100.0));
    buf.set_wrap(Wrap::None);
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    buf.set_text("X\u{301}B", &attrs, Shaping::Advanced, None);
    let mut spans = AttrsList::new(&attrs);
    for (bytes, column, joins) in [
        (
            0..3,
            0,
            Joins {
                left: false,
                right: true,
            },
        ),
        (
            3..4,
            1,
            Joins {
                left: true,
                right: false,
            },
        ),
    ] {
        spans.add_span(
            bytes,
            &attrs.clone().metadata(
                GridGlyphCell {
                    column,
                    width: 1,
                    clustered: false,
                    text_presentation: false,
                    single_image: false,
                    joins,
                }
                .metadata(),
            ),
        );
    }
    buf.lines[0].set_attrs_list(spans);
    buf.shape_until_scroll(&mut fonts, false);
    (fonts, buf)
}

#[test]
fn the_emitter_shifts_the_base_and_marks_and_emits_each_connector_once() {
    let (_, buf) = fixture();
    let glyphs = buf.layout_runs().next().unwrap().glyphs;
    assert_eq!(glyphs.len(), 3, "a separate bundled acute mark");
    for positioning in [
        GlyphPositioning::Grid(20.0),
        GlyphPositioning::GraphemeRows(20.0),
    ] {
        let mut source = Source {
            connectors: 0,
            available: true,
        };
        let mut out = Vec::new();
        emit_positioned_glyphs(
            &mut out,
            &buf,
            &[],
            (0.25, 0.0),
            20.0,
            positioning,
            None,
            GColor::rgb(255, 255, 255),
            &mut Vec::new(),
            &mut source,
        );
        assert_eq!(source.connectors, 2);
        assert_eq!(out.len(), 5);
        let b = glyphs.iter().find(|g| g.start == 3).unwrap();
        assert_eq!(out[2].pos[0], (40.25 - b.w).round());
        assert!(
            out[3..]
                .iter()
                .all(|g| g.uv[0] == 102.0 && g.uv_size[0] == 1.0)
        );
        assert_eq!(out[3].pos[0] + out[3].size[0], 20.0);
        assert_eq!(out[4].pos[0], 20.0);
        let mut original = Vec::new();
        let mut source = Source {
            connectors: 0,
            available: false,
        };
        emit_positioned_glyphs(
            &mut original,
            &buf,
            &[],
            (0.25, 0.0),
            20.0,
            positioning,
            None,
            GColor::rgb(255, 255, 255),
            &mut Vec::new(),
            &mut source,
        );
        assert_eq!(original.len(), 3);
        assert_eq!(original[2].pos[0], 20.0);
        assert_eq!(original[0].pos, out[0].pos);
        assert_eq!(original[1].pos, out[1].pos);
    }
}

#[test]
fn joined_cell_emit_has_no_row_advance_prepass_or_linear_cell_search() {
    // A source guard complements the bounded connector-call test: those
    // counters cannot detect a CPU-only quadratic lookup before emission.
    let source = kettle_test_support::code_only(include_str!("batching.rs"));
    let emit = source
        .split("pub(crate) fn emit_positioned_glyphs(")
        .nth(1)
        .unwrap()
        .split("pub(crate) struct CellImage")
        .next()
        .unwrap();
    assert!(!emit.contains("cell_advance"));
    assert!(!emit.contains("advance_of"));
    assert!(!emit.contains("for glyph in run.glyphs"));
}

pub(crate) const ARABIC_FACES: [&str; 3] = ["Geeza Pro", "Noto Sans Arabic", "DejaVu Sans"];

pub(crate) fn isolate_named_face(fonts: &mut FontSystem, family: &str) -> bool {
    let matches = |face: &fontdb::FaceInfo| face.families.iter().any(|(name, _)| name == family);
    if !fonts.db().faces().any(matches)
        && let Some(dir) = std::env::var_os("KETTLE_ARABIC_FONT_DIR")
        && let Some(file) = match family {
            "Noto Sans Arabic" => Some("NotoSansArabic-Regular.ttf"),
            "DejaVu Sans" => Some("DejaVuSans.ttf"),
            _ => None,
        }
    {
        let path = std::path::PathBuf::from(dir).join(file);
        match fonts.db_mut().load_font_file(&path) {
            Ok(()) => eprintln!("ARABIC_FONT_LOAD: {family} from {}", path.display()),
            Err(error) => eprintln!("ARABIC_FONT_SKIP: cannot load {}: {error}", path.display()),
        }
    }
    if !fonts.db().faces().any(matches) {
        eprintln!("ARABIC_FONT_SKIP: named face {family:?} is absent");
        return false;
    }
    let remove: Vec<_> = fonts
        .db()
        .faces()
        .filter(|face| {
            !matches(face)
                && !face
                    .families
                    .iter()
                    .any(|(name, _)| name == kettle_config::font::FAMILY)
        })
        .map(|face| face.id)
        .collect();
    for id in remove {
        fonts.db_mut().remove_face(id);
    }
    true
}

pub(crate) fn arabic_buffer(
    fonts: &mut FontSystem,
    family: &str,
    text: &str,
    size: f32,
) -> TextBuffer {
    use kettle_core::bidi::{BidiCell, CellKind, row_bidi};
    let cells: Vec<_> = text
        .chars()
        .map(|c| BidiCell::new(c, CellKind::Narrow, &[]))
        .collect();
    let map = row_bidi(&cells).unwrap();
    let attrs = Attrs::new().family(Family::Name(family));
    let mut spans = AttrsList::new(&attrs);
    let mut shown = String::new();
    for (col, cell) in cells.iter().enumerate() {
        let start = shown.len();
        shown.push(map.display_char(col).unwrap_or(cell.c));
        shown.push('\u{200c}');
        spans.add_span(
            start..shown.len(),
            &attrs.clone().metadata(
                GridGlyphCell {
                    column: map.visual(col),
                    width: 1,
                    clustered: false,
                    text_presentation: false,
                    single_image: false,
                    joins: map.joins(col),
                }
                .metadata(),
            ),
        );
    }
    let mut buf = TextBuffer::new(fonts, Metrics::new(size, size * 1.5));
    buf.set_size(Some(1000.0), Some(200.0));
    buf.set_wrap(Wrap::None);
    buf.set_text(&shown, &attrs, Shaping::Advanced, None);
    buf.lines[0].set_ltr_override(true);
    buf.lines[0].set_attrs_list(spans);
    buf.shape_until_scroll(fonts, false);
    buf
}

struct RealGlyphs<'a> {
    fonts: &'a mut FontSystem,
    swash: &'a mut SwashCache,
}
impl GlyphSource for &mut RealGlyphs<'_> {
    fn slot(&mut self, key: glyphon::cosmic_text::CacheKey) -> Option<GlyphSlot> {
        let image = self.swash.get_image_uncached(self.fonts, key)?;
        let p = image.placement;
        (p.width > 0 && p.height > 0).then_some(GlyphSlot {
            kind: 1,
            atlas_x: 0.0,
            atlas_y: 0.0,
            w: p.width as f32,
            h: p.height as f32,
            left: p.left,
            top: p.top,
        })
    }
    fn connector(&mut self, mut key: glyphon::cosmic_text::CacheKey) -> Option<GlyphSlot> {
        key.glyph_id = self
            .fonts
            .get_font(key.font_id, key.font_weight)?
            .as_swash()
            .charmap()
            .map('\u{640}');
        if key.glyph_id == 0 {
            return None;
        }
        key.x_bin = glyphon::cosmic_text::SubpixelBin::Zero;
        self.slot(key)
    }
}

#[test]
fn block_cursor_uses_the_panes_shift_and_connectors_in_grid_and_legacy_at_both_scales() {
    for family in ARABIC_FACES {
        let mut fonts = FontSystem::new();
        if !isolate_named_face(&mut fonts, family) {
            continue;
        }
        let mut swash = SwashCache::new();
        for scale in [1.0, 2.0] {
            let size = 13.0 * scale;
            let cw = size * 0.6;
            for text in ["مرحبا", "ـببـ", "ـصصـ"] {
                let buf = arabic_buffer(&mut fonts, family, text, size);
                let run = buf.layout_runs().next().unwrap();
                assert!(run.glyphs.iter().all(|g| g.glyph_id != 0));
                for positioning in [
                    GlyphPositioning::Grid(cw),
                    GlyphPositioning::GraphemeRows(cw),
                ] {
                    for column in 0..text.chars().count() {
                        let mut pane = Vec::new();
                        let mut source = RealGlyphs {
                            fonts: &mut fonts,
                            swash: &mut swash,
                        };
                        emit_positioned_glyphs(
                            &mut pane,
                            &buf,
                            &[],
                            (0.25, 2.0),
                            cw,
                            positioning,
                            Some((0, Some(column))),
                            GColor::rgb(255, 255, 255),
                            &mut Vec::new(),
                            &mut source,
                        );
                        let cursor = crate::batching::moved_cell_images(
                            &buf,
                            &[],
                            (0, column),
                            true,
                            (0.25, 2.0),
                            cw,
                            positioning,
                            &mut fonts,
                            &mut swash,
                            &mut Vec::new(),
                        );
                        assert!(!cursor.is_empty(), "{family} {text} column {column}");
                        let rects: Vec<_> = pane
                            .iter()
                            .map(|g| {
                                [
                                    g.pos[0] as i32,
                                    g.pos[1] as i32,
                                    g.size[0] as i32,
                                    g.size[1] as i32,
                                ]
                            })
                            .collect();
                        assert_eq!(
                            cursor.iter().map(|g| g.rect).collect::<Vec<_>>(),
                            rects,
                            "{family} {text} @{scale} col {column}"
                        );
                        assert!(
                            cursor
                                .iter()
                                .all(|g| g.mask.len() == (g.rect[2] * g.rect[3]) as usize)
                        );
                    }
                }
                if text == "مرحبا" {
                    let base = run
                        .glyphs
                        .iter()
                        .find(|g| {
                            g.w > 0.0
                                && GridGlyphCell::from_metadata(g.metadata)
                                    .is_some_and(|c| c.column == 4)
                        })
                        .unwrap();
                    let images = crate::batching::moved_cell_images(
                        &buf,
                        &[],
                        (0, 4),
                        true,
                        (0.25, 2.0),
                        cw,
                        GlyphPositioning::Grid(cw),
                        &mut fonts,
                        &mut swash,
                        &mut Vec::new(),
                    );
                    let image = swash
                        .get_image_uncached(&mut fonts, base.physical((0.0, 0.0), 1.0).cache_key)
                        .unwrap();
                    assert_eq!(
                        images[0].rect[0],
                        (0.25 + 5.0 * cw - base.w).round() as i32 + image.placement.left
                    );
                    assert!(
                        images.iter().any(|g| g.sample[2] == 1),
                        "initial has its own connector"
                    );
                }
            }
        }
    }
}

#[test]
fn latin_and_hebrew_cells_carry_no_join_sides_or_connector_queries() {
    let (mut fonts, mut buf) = fixture();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    buf.set_text("Latin ASCII", &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(&mut fonts, false);
    let mut source = Source {
        connectors: 0,
        available: true,
    };
    let mut out = Vec::new();
    emit_positioned_glyphs(
        &mut out,
        &buf,
        &[],
        (0.0, 0.0),
        20.0,
        GlyphPositioning::Grid(20.0),
        None,
        GColor::rgb(255, 255, 255),
        &mut Vec::new(),
        &mut source,
    );
    assert_eq!(source.connectors, 0);
    let cells: Vec<_> = "Latin שלום"
        .chars()
        .map(|c| kettle_core::bidi::BidiCell::new(c, kettle_core::bidi::CellKind::Narrow, &[]))
        .collect();
    let map = kettle_core::bidi::row_bidi(&cells).unwrap();
    assert!((0..cells.len()).all(|col| map.joins(col) == Joins::default()));
}

#[test]
fn cursor_only_rasterizes_its_joining_cell_and_immediate_partners() {
    let (mut fonts, mut buf) = fixture();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    let text = "X".repeat(50);
    let mut spans = AttrsList::new(&attrs);
    for column in 0..50 {
        spans.add_span(
            column..column + 1,
            &attrs.clone().metadata(
                GridGlyphCell {
                    column,
                    width: 1,
                    clustered: false,
                    text_presentation: false,
                    single_image: false,
                    joins: Joins {
                        left: true,
                        right: true,
                    },
                }
                .metadata(),
            ),
        );
    }
    buf.set_text(&text, &attrs, Shaping::Advanced, None);
    buf.lines[0].set_attrs_list(spans);
    buf.shape_until_scroll(&mut fonts, false);
    let mut out = Vec::new();
    let mut source = Source {
        connectors: 0,
        available: true,
    };
    emit_positioned_glyphs(
        &mut out,
        &buf,
        &[],
        (0.0, 0.0),
        20.0,
        GlyphPositioning::Grid(20.0),
        Some((0, Some(25))),
        GColor::rgb(255, 255, 255),
        &mut Vec::new(),
        &mut source,
    );
    assert_eq!(
        source.connectors, 3,
        "cursor must not rasterize the whole Arabic row"
    );
    assert_eq!(out.len(), 2, "one base and its right connector");
}

#[test]
fn arabic_cursor_path_is_enabled_for_bidi_legacy() {
    let source = kettle_test_support::code_only(include_str!("terminal.rs"));
    let dispatch = source
        .split("if (cfg.text_renderer == TextRendererMode::Grid || row_map(cvrow).is_some())")
        .nth(1)
        .expect("bidi Legacy must share the mask cursor path");
    let dispatch = dispatch.split("self.span_scratch = spans").next().unwrap();
    assert!(dispatch.contains("moved_cell_images("));
    assert!(dispatch.contains("GlyphPositioning::GraphemeRows(cw)"));
}

type InkPoint = (i32, i32);

fn strong_composited_ink(
    images: &[crate::batching::CellImage],
) -> std::collections::BTreeSet<InkPoint> {
    let mut coverage = std::collections::BTreeMap::<InkPoint, f64>::new();
    for image in images {
        let [left, top, width, height] = image.rect;
        for y in 0..height {
            for x in 0..width {
                let src = f64::from(image.mask[(y * width + x) as usize]) / 255.0;
                let dst = coverage.entry((left + x, top + y)).or_default();
                *dst = src + *dst * (1.0 - src);
            }
        }
    }
    coverage
        .into_iter()
        .filter_map(|(point, alpha)| (alpha > 0.5).then_some(point))
        .collect()
}

fn ink_component(
    ink: &std::collections::BTreeSet<InkPoint>,
    start: InkPoint,
) -> std::collections::BTreeSet<InkPoint> {
    let mut seen = std::collections::BTreeSet::new();
    let mut pending = vec![start];
    while let Some(point) = pending.pop() {
        if !ink.contains(&point) || !seen.insert(point) {
            continue;
        }
        for dx in -1..=1 {
            for dy in -1..=1 {
                if dx != 0 || dy != 0 {
                    pending.push((point.0 + dx, point.1 + dy));
                }
            }
        }
    }
    seen
}

fn base_ink_anchor(image: &crate::batching::CellImage) -> InkPoint {
    // These unmarked fixtures put the main base body above any detached dot.
    // Prefer the first strongest pixel, rather than an antialiased extremity.
    let (index, alpha) = image
        .mask
        .iter()
        .enumerate()
        .max_by_key(|(index, alpha)| (**alpha, std::cmp::Reverse(*index)))
        .unwrap();
    assert!(*alpha > 127, "the base must have a strong ink anchor");
    let [left, top, width, _] = image.rect;
    (left + index as i32 % width, top + index as i32 / width)
}

#[test]
fn flat_named_arabic_masks_form_one_unbroken_stroke_without_a_gpu() {
    for family in ARABIC_FACES {
        let mut fonts = FontSystem::new();
        if !isolate_named_face(&mut fonts, family) {
            continue;
        }
        let mut swash = SwashCache::new();
        for size in [13.0, 14.0] {
            for scale in [1.0, 2.0] {
                let size = size * scale;
                let cw = size * 0.6;
                for text in ["ـببببـ", "ـــــ", "مرحبا"] {
                    let buf = arabic_buffer(&mut fonts, family, text, size);
                    assert!(
                        buf.layout_runs()
                            .next()
                            .unwrap()
                            .glyphs
                            .iter()
                            .filter(|g| g.w > 0.0)
                            .all(|g| {
                                g.glyph_id != 0
                                    && fonts.db().face(g.font_id).is_some_and(|face| {
                                        face.families.iter().any(|(name, _)| name == family)
                                    })
                            }),
                        "{family} must supply every Arabic fixture glyph"
                    );
                    for positioning in [
                        GlyphPositioning::Grid(cw),
                        GlyphPositioning::GraphemeRows(cw),
                    ] {
                        let mut images = Vec::new();
                        let mut anchors = Vec::new();
                        for column in 0..text.chars().count() {
                            let cell = crate::batching::moved_cell_images(
                                &buf,
                                &[],
                                (0, column),
                                true,
                                (0.25, 0.0),
                                cw,
                                positioning,
                                &mut fonts,
                                &mut swash,
                                &mut Vec::new(),
                            );
                            assert!(
                                !cell.is_empty(),
                                "{family} {size}px {text}: column {column} must draw"
                            );
                            anchors.push(base_ink_anchor(&cell[0]));
                            images.extend(cell);
                        }
                        // Keep the half-coverage cutoff. A curved letter can
                        // leave tatweel's row without breaking its raster path.
                        let ink = strong_composited_ink(&images);
                        let groups: &[&[usize]] = if text == "مرحبا" {
                            &[&[0, 1, 2], &[3, 4]]
                        } else if text == "ـــــ" {
                            &[&[0, 1, 2, 3, 4]]
                        } else {
                            &[&[0, 1, 2, 3, 4, 5]]
                        };
                        for group in groups {
                            let connected = ink_component(&ink, anchors[group[0]]);
                            for &column in *group {
                                assert!(
                                    connected.contains(&anchors[column]),
                                    "{family} {size}px {text}: disconnected base at column {column}"
                                );
                            }
                        }
                    }
                }
            }
        }
        eprintln!(
            "ARABIC_MASK_PASS: {family}; 24 configurations (3 strings, 2 sizes, 2 scales, 2 modes)"
        );
    }
}

#[test]
fn joined_glyph_anchoring_keeps_selection_and_underline_on_the_cell_grid() {
    let (_, buf) = fixture();
    let mut source = Source {
        connectors: 0,
        available: true,
    };
    let mut out = Vec::new();
    emit_positioned_glyphs(
        &mut out,
        &buf,
        &[],
        (0.25, 0.0),
        20.0,
        GlyphPositioning::Grid(20.0),
        None,
        GColor::rgb(255, 255, 255),
        &mut Vec::new(),
        &mut source,
    );
    assert!(
        out[2].pos[0] > 20.25,
        "initial glyph is anchored away from the cell edge"
    );
    let cells: Vec<_> = "بب"
        .chars()
        .map(|c| kettle_core::bidi::BidiCell::new(c, kettle_core::bidi::CellKind::Narrow, &[]))
        .collect();
    let map = kettle_core::bidi::row_bidi(&cells).unwrap();
    assert_eq!(
        map.visual_runs(0..=0),
        std::iter::once(1..2).collect::<Vec<_>>()
    );
    let mut quads = Vec::new();
    crate::terminal::push_cell_decorations(
        &mut quads,
        (20.25, 0.0, 20.0, 30.0),
        1,
        alacritty_terminal::term::cell::Flags::UNDERLINE,
        Rgb::new(255, 255, 255),
        Rgb::new(255, 255, 255),
    );
    assert!(!quads.is_empty());
    assert!(
        quads
            .iter()
            .all(|quad| quad.pos[0] >= 20.25 && quad.pos[0] + quad.size[0] <= 40.25)
    );
    assert_eq!(
        quads[0].pos[0], 20.25,
        "underline follows the grid, not the shifted glyph"
    );
}

#[test]
fn a_latin_cursor_does_not_scan_the_rows_join_metadata() {
    let (mut fonts, mut buf) = fixture();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    let mut swash = SwashCache::new();
    for bidi in [false, true] {
        let mut text = "Latin ".repeat(20);
        if bidi {
            text.push_str("\u{5e9}\u{5dc}\u{5d5}\u{5dd} \u{628}\u{628}");
            buf = arabic_buffer(&mut fonts, kettle_config::font::FAMILY, &text, 16.0);
        } else {
            buf.set_text(&text, &attrs, Shaping::Advanced, None);
            buf.shape_until_scroll(&mut fonts, false);
        }
        let cells: Vec<_> = text
            .chars()
            .map(|c| kettle_core::bidi::BidiCell::new(c, kettle_core::bidi::CellKind::Narrow, &[]))
            .collect();
        let map = kettle_core::bidi::row_bidi(&cells);
        assert_eq!(map.is_some(), bidi);
        let joined = map.as_ref().is_some_and(|map| {
            let joins = map.joins(50);
            joins.left || joins.right
        });
        assert!(!joined);
        assert!(buf.layout_runs().next().unwrap().glyphs.len() > 100);
        for positioning in [
            GlyphPositioning::Grid(20.0),
            GlyphPositioning::GraphemeRows(20.0),
        ] {
            let before = GridGlyphCell::metadata_decode_count();
            let images = crate::batching::moved_cell_images(
                &buf,
                &[],
                (0, 50),
                joined,
                (0.0, 0.0),
                20.0,
                positioning,
                &mut fonts,
                &mut swash,
                &mut Vec::new(),
            );
            assert!(images.is_empty());
            assert_eq!(
                GridGlyphCell::metadata_decode_count() - before,
                0,
                "a nonjoining cursor must not inspect row glyph metadata (bidi={bidi})"
            );
        }
    }
}

#[test]
fn a_nonjoining_cursor_still_captures_moved_marks() {
    let (mut fonts, mut buf) = fixture();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    buf.set_text("X\u{301}B", &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(&mut fonts, false);
    let glyphs = buf.layout_runs().next().unwrap().glyphs;
    assert_eq!(glyphs.len(), 3);
    let shifts = vec![Some(
        vec![crate::mark_fallback::MarkShift {
            index: 1,
            glyph_id: glyphs[1].glyph_id,
            x: 0.1,
            y: 0.1,
            top: 1.0,
            bottom: 0.5,
            above: true,
        }]
        .into_boxed_slice(),
    )];
    let images = crate::batching::moved_cell_images(
        &buf,
        &shifts,
        (0, 0),
        false,
        (0.0, 0.0),
        20.0,
        GlyphPositioning::Grid(20.0),
        &mut fonts,
        &mut SwashCache::new(),
        &mut Vec::new(),
    );
    assert_eq!(
        images.len(),
        2,
        "the base and moved mark still use positioned masks"
    );
}

#[test]
fn mask_connectivity_accepts_curves_and_compositing_but_rejects_a_severed_stroke() {
    let (key, _, _) = glyphon::cosmic_text::CacheKey::new(
        fontdb::ID::dummy(),
        1,
        13.0,
        (0.0, 0.0),
        fontdb::Weight::NORMAL,
        glyphon::cosmic_text::CacheKeyFlags::empty(),
    );
    let mask = |rect, data: &[u8]| crate::batching::CellImage {
        key,
        rect,
        mask: data.to_vec(),
        sample: [0, 0, rect[2] as u16, rect[3] as u16],
    };
    // The 124-valued pixel alone is below threshold; the curve above it is
    // strong and diagonally adjacent to the next stroke pixel, as in Noto.
    let curve = strong_composited_ink(&[mask([0, 0, 3, 2], &[255, 230, 0, 0, 124, 255])]);
    assert!(ink_component(&curve, (0, 0)).contains(&(2, 1)));
    let overlap =
        strong_composited_ink(&[mask([0, 0, 2, 1], &[255, 100]), mask([1, 0, 1, 1], &[100])]);
    assert!(
        ink_component(&overlap, (0, 0)).contains(&(1, 0)),
        "source-over coverage exceeds half"
    );
    let gap = strong_composited_ink(&[mask([0, 0, 5, 1], &[255, 0, 255, 0, 255])]);
    assert!(
        !ink_component(&gap, (0, 0)).contains(&(4, 0)),
        "a floating bridge cannot connect the bases"
    );
    let vertical_gap = strong_composited_ink(&[mask([0, 0, 2, 3], &[255, 0, 0, 0, 0, 255])]);
    assert!(!ink_component(&vertical_gap, (0, 0)).contains(&(1, 2)));
}

#[test]
fn supplied_arabic_font_files_load_into_an_empty_test_database() {
    let Some(dir) = std::env::var_os("KETTLE_ARABIC_FONT_DIR") else {
        eprintln!("ARABIC_FONT_SKIP: no test-only font directory supplied");
        return;
    };
    for (family, file) in [
        ("Noto Sans Arabic", "NotoSansArabic-Regular.ttf"),
        ("DejaVu Sans", "DejaVuSans.ttf"),
    ] {
        let path = std::path::PathBuf::from(&dir).join(file);
        if !path.is_file() {
            eprintln!("ARABIC_FONT_SKIP: {} is absent", path.display());
            continue;
        }
        let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
        assert!(
            isolate_named_face(&mut fonts, family),
            "supplied file must provide {family}"
        );
        assert!(
            fonts
                .db()
                .faces()
                .all(|face| face.families.iter().any(|(name, _)| name == family))
        );
    }
}

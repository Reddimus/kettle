use super::geometry::GridGlyphCell;
use super::terminal::{cluster_needs_single_image, replace_unsupported_graphemes};
use super::*;

fn fixture_fonts() -> FontSystem {
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    fonts
}

fn buffer(fonts: &mut FontSystem, text: &str, attrs: &Attrs<'_>) -> TextBuffer {
    let mut buffer = TextBuffer::new(fonts, Metrics::new(16.0, 32.0));
    buffer.set_size(Some(400.0), Some(64.0));
    buffer.set_wrap(Wrap::None);
    buffer.set_text(text, attrs, Shaping::Advanced, None);
    if attrs.metadata != 0 {
        let mut spans = AttrsList::new(&attrs.clone().metadata(0));
        spans.add_span(0..text.len(), attrs);
        buffer.lines[0].set_attrs_list(spans);
    }
    buffer.shape_until_scroll(fonts, false);
    buffer
}

fn ink(buffer: &TextBuffer, fonts: &mut FontSystem, swash: &mut SwashCache) -> (u32, u32) {
    let glyph = &buffer.layout_runs().next().unwrap().glyphs[0];
    assert_ne!(glyph.glyph_id, 0, "fixture must provide a real glyph");
    let image = swash
        .get_image(fonts, glyph.physical((0.0, 0.0), 1.0).cache_key)
        .as_ref()
        .unwrap();
    (image.placement.width, image.placement.height)
}

#[test]
fn emoji_presentation_fills_height_with_repo_font_and_preserves_text_presentation() {
    let mut fonts = fixture_fonts();
    let mut swash = SwashCache::new();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    // The bundled Nerd Font provides lightning (default emoji) and copyright
    // (text by default). No system fallback participates in these raster tests.
    for text in ["\u{26a1}", "\u{a9}\u{fe0f}"] {
        let buf = buffer(&mut fonts, text, &attrs);
        let before = ink(&buf, &mut fonts, &mut swash);
        let baseline = buf.layout_runs().next().unwrap().line_y;
        let (x, y, scale) = super::fonts::emoji_cursor_transform(
            &buf,
            &mut fonts,
            &mut swash,
            (100.0, 50.0, 80.0, 32.0),
        )
        .expect("emoji needs a fitted transform");
        assert!(scale > 1.0);
        assert!((before.1 as f32 * scale - 32.0).abs() < 0.01);
        assert!(x.is_finite() && y.is_finite());
        assert_eq!(buf.layout_runs().next().unwrap().line_y, baseline);
    }
    for text in ["\u{a9}", "\u{a9}\u{fe0e}", "\u{26a1}\u{fe0e}", "A"] {
        let buf = buffer(&mut fonts, text, &attrs);
        assert!(
            super::fonts::emoji_cursor_transform(
                &buf,
                &mut fonts,
                &mut swash,
                (100.0, 50.0, 80.0, 32.0),
            )
            .is_none()
        );
    }
}

#[test]
fn emoji_sizing_respects_cluster_width() {
    use super::fonts::fit_emoji;
    assert_eq!(
        fit_emoji((100.0, 50.0, 24.0, 32.0), (12.0, 16.0)),
        (100.0, 50.0, 24.0, 32.0)
    );
    assert_eq!(
        fit_emoji((100.0, 50.0, 20.0, 32.0), (16.0, 16.0)),
        (100.0, 56.0, 20.0, 20.0)
    );
}

fn assert_ligature_ink_is_preserved(text: &str) {
    use super::batching::{GlyphPositioning, cell_locked_glyph_offset, emit_positioned_glyphs};
    use super::glyphpipe::GlyphSlot;

    let mut fonts = fixture_fonts();
    let mut swash = SwashCache::new();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    let origin = (100.0, 50.0);
    let cell_width = 9.6;
    let mut failures = Vec::new();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        for clustered in [false, true] {
            let mut buf = buffer(&mut fonts, text, &attrs);
            if clustered {
                let mut spans = AttrsList::new(&attrs);
                for column in 0..text.len() {
                    let cell = GridGlyphCell {
                        column,
                        width: 1,
                        clustered: true,
                        text_presentation: false,
                        single_image: false,
                    };
                    spans.add_span(column..column + 1, &attrs.clone().metadata(cell.metadata()));
                }
                buf.lines[0].set_attrs_list(spans);
                buf.shape_until_scroll(&mut fonts, false);
            }
            let positioning = match mode {
                TextRendererMode::Grid => GlyphPositioning::Grid(cell_width),
                TextRendererMode::Legacy if clustered => GlyphPositioning::GraphemeRows(cell_width),
                TextRendererMode::Legacy => GlyphPositioning::Continuous,
            };
            let run = buf.layout_runs().next().unwrap();
            let mut expected = Vec::new();
            let mut empty_images = 0;
            let mut left_overhangs = 0;
            for glyph in run.glyphs {
                assert_ne!(glyph.glyph_id, 0, "fixture must use real bundled glyphs");
                let column = GridGlyphCell::from_metadata(glyph.metadata)
                    .map_or(glyph.start, |cell| cell.column);
                let off_x = if matches!(positioning, GlyphPositioning::Continuous) {
                    origin.0
                } else {
                    cell_locked_glyph_offset(origin.0 + column as f32 * cell_width, glyph, glyph.x)
                };
                let physical = glyph.physical((off_x, origin.1), 1.0);
                let image = swash.get_image(&mut fonts, physical.cache_key).as_ref();
                let Some(image) = image.filter(|i| i.placement.width > 0 && i.placement.height > 0)
                else {
                    empty_images += 1;
                    continue;
                };
                left_overhangs += usize::from(image.placement.left < 0);
                expected.push((
                    [
                        (physical.x + image.placement.left) as f32,
                        run.line_y.round() + (physical.y - image.placement.top) as f32,
                    ],
                    [image.placement.width as f32, image.placement.height as f32],
                ));
            }
            assert!(
                empty_images > 0 && left_overhangs > 0,
                "{text:?}, {mode:?}, 2027={clustered}: fixture needs a calt spacer and left overhang"
            );
            let mut instances = Vec::new();
            emit_positioned_glyphs(
                &mut instances,
                &buf,
                &[],
                origin,
                cell_width,
                positioning,
                GColor::rgb(255, 255, 255),
                &mut Vec::new(),
                |key| {
                    let image = swash.get_image(&mut fonts, key).as_ref()?;
                    (image.placement.width > 0 && image.placement.height > 0).then_some(GlyphSlot {
                        kind: 1,
                        atlas_x: 0.0,
                        atlas_y: 0.0,
                        w: image.placement.width as f32,
                        h: image.placement.height as f32,
                        left: image.placement.left,
                        top: image.placement.top,
                    })
                },
            );
            if instances.len() != expected.len() {
                failures.push(format!(
                    "{mode:?}, 2027={clustered}: emitted {} of {} non-empty images",
                    instances.len(),
                    expected.len(),
                ));
            }
            for (index, (instance, (pos, size))) in instances.iter().zip(&expected).enumerate() {
                if instance.pos != *pos
                    || instance.size != *size
                    || instance.uv != [0.0, 0.0]
                    || instance.uv_size != *size
                {
                    failures.push(format!(
                        "{mode:?}, 2027={clustered}, image {index}: expected pos={pos:?}, size={size:?}; \
                         got pos={:?}, size={:?}, uv={:?}, uv_size={:?}",
                        instance.pos, instance.size, instance.uv, instance.uv_size,
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{text:?}:\n{}", failures.join("\n"));
}

#[test]
fn programming_ligature_double_slash_preserves_shaped_ink() {
    assert_ligature_ink_is_preserved("a//b");
}

#[test]
fn programming_ligature_arrow_preserves_shaped_ink() {
    assert_ligature_ink_is_preserved("->");
}

#[test]
fn programming_ligature_not_equal_preserves_shaped_ink() {
    assert_ligature_ink_is_preserved("!=");
}

#[test]
fn programming_ligature_triple_equal_preserves_shaped_ink() {
    assert_ligature_ink_is_preserved("===");
}

#[test]
fn programming_ligature_url_preserves_shaped_ink() {
    assert_ligature_ink_is_preserved("https://x");
}

#[test]
fn emoji_combining_mark_keeps_one_shared_transform_in_all_positioning_modes() {
    use super::batching::{GlyphPositioning, cell_locked_glyph_offset, emit_positioned_glyphs};
    use super::glyphpipe::GlyphSlot;

    let mut fonts = fixture_fonts();
    let mut swash = SwashCache::new();
    let origin = (100.0, 50.0);
    let cell_width = 20.0;
    let text = "\u{26a1}\u{301} X";
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        for clustered in [false, true] {
            let mut buf = buffer(&mut fonts, text, &attrs);
            if clustered {
                let mut spans = AttrsList::new(&attrs);
                for (bytes, column, width) in [(0..5, 0, 2), (6..7, 2, 1)] {
                    let cell = GridGlyphCell {
                        column,
                        width,
                        clustered: true,
                        text_presentation: false,
                        single_image: false,
                    };
                    spans.add_span(bytes, &attrs.clone().metadata(cell.metadata()));
                }
                buf.lines[0].set_attrs_list(spans);
                buf.shape_until_scroll(&mut fonts, false);
            }
            let positioning = match mode {
                TextRendererMode::Grid => GlyphPositioning::Grid(cell_width),
                TextRendererMode::Legacy if clustered => GlyphPositioning::GraphemeRows(cell_width),
                TextRendererMode::Legacy => GlyphPositioning::Continuous,
            };
            let run = buf.layout_runs().next().unwrap();
            let emoji: Vec<_> = run.glyphs.iter().filter(|g| g.start < 5).collect();
            assert_eq!(emoji.len(), 2, "fixture must have a separate acute glyph");
            assert_eq!(emoji[0].start..emoji[0].end, emoji[1].start..emoji[1].end);
            let raw_pos = |glyph: &glyphon::cosmic_text::LayoutGlyph, column, cluster_x| {
                let off_x = if matches!(positioning, GlyphPositioning::Continuous) {
                    origin.0
                } else {
                    cell_locked_glyph_offset(
                        origin.0 + column as f32 * cell_width,
                        glyph,
                        cluster_x,
                    )
                };
                let physical = glyph.physical((off_x, origin.1), 1.0);
                let image = swash
                    .get_image(&mut fonts, physical.cache_key)
                    .as_ref()
                    .unwrap();
                [
                    (physical.x + image.placement.left) as f32,
                    run.line_y.round() + (physical.y - image.placement.top) as f32,
                ]
            };
            let mut raw_pos = raw_pos;
            let raw_base = raw_pos(emoji[0], 0, emoji[0].x);
            let raw_mark = raw_pos(emoji[1], 0, emoji[0].x);
            let neighbor = run.glyphs.iter().find(|g| g.start == 6).unwrap();
            let raw_neighbor = raw_pos(neighbor, 2, neighbor.x);
            let mut instances = Vec::new();
            emit_positioned_glyphs(
                &mut instances,
                &buf,
                &[],
                origin,
                cell_width,
                positioning,
                GColor::rgb(255, 255, 255),
                &mut Vec::new(),
                |key| {
                    let image = swash.get_image(&mut fonts, key).as_ref()?;
                    (image.placement.width > 0 && image.placement.height > 0).then_some(GlyphSlot {
                        kind: 1,
                        atlas_x: 0.0,
                        atlas_y: 0.0,
                        w: image.placement.width as f32,
                        h: image.placement.height as f32,
                        left: image.placement.left,
                        top: image.placement.top,
                    })
                },
            );
            assert_eq!(instances.len(), 3);
            let [base, mark, neighbor] = instances.as_slice() else {
                unreachable!()
            };
            let scale = base.size[0] / base.uv_size[0];
            for glyph in [base, mark] {
                for axis in 0..2 {
                    assert!(
                        (glyph.size[axis] / glyph.uv_size[axis] - scale).abs() < 0.001,
                        "{mode:?}, 2027={clustered}: all cluster images must share one scale"
                    );
                }
            }
            for axis in 0..2 {
                assert!(
                    ((mark.pos[axis] - base.pos[axis]) - (raw_mark[axis] - raw_base[axis]) * scale)
                        .abs()
                        < 0.001,
                    "{mode:?}, 2027={clustered}: preserve shaped mark displacement"
                );
            }
            let left = base.pos[0].min(mark.pos[0]);
            let top = base.pos[1].min(mark.pos[1]);
            let right = (base.pos[0] + base.size[0]).max(mark.pos[0] + mark.size[0]);
            let bottom = (base.pos[1] + base.size[1]).max(mark.pos[1] + mark.size[1]);
            assert!((left + right - (origin.0 * 2.0 + 40.0)).abs() < 0.001);
            assert!((top + bottom - (origin.1 * 2.0 + 32.0)).abs() < 0.001);
            assert!(right - left <= 40.001 && bottom - top <= 32.001);
            assert!(
                (right - left - 40.0).abs() < 0.001 || (bottom - top - 32.0).abs() < 0.001,
                "combined ink must fill at least one cell dimension"
            );
            assert_eq!(
                neighbor.pos, raw_neighbor,
                "neighbor must keep its baseline and advance"
            );
        }
    }
}

#[test]
fn non_emoji_zwj_clusters_do_not_require_one_image() {
    for flags in [Flags::GRAPHEME, Flags::GRAPHEME | Flags::WIDE_CHAR] {
        assert!(!cluster_needs_single_image(
            flags,
            '\u{915}',
            &['\u{94d}', '\u{200d}', '\u{937}'],
        ));
    }
    assert!(cluster_needs_single_image(
        Flags::GRAPHEME,
        '\u{600}',
        &['\u{1f469}', '\u{200d}', '\u{1f4bb}'],
    ));
}

#[test]
fn indic_zwj_cluster_preserves_multiple_shaped_images() {
    let mut fonts = FontSystem::new();
    if !fonts.db().faces().any(|face| {
        face.families
            .iter()
            .any(|(name, _)| name == "Devanagari Sangam MN")
    }) {
        eprintln!("INDIC_FONT_SKIP: Devanagari Sangam MN is unavailable");
        return;
    }
    let text = "\u{915}\u{94d}\u{200d}\u{937}";
    let cell = GridGlyphCell {
        column: 0,
        width: 1,
        clustered: true,
        text_presentation: false,
        single_image: cluster_needs_single_image(
            Flags::GRAPHEME,
            '\u{915}',
            &['\u{94d}', '\u{200d}', '\u{937}'],
        ),
    };
    let attrs = Attrs::new()
        .family(Family::Name("Devanagari Sangam MN"))
        .metadata(cell.metadata());
    let mut buf = buffer(&mut fonts, text, &attrs);
    let mut swash = SwashCache::new();
    let run = buf.layout_runs().next().unwrap();
    assert!(run.glyphs.iter().all(|glyph| glyph.glyph_id != 0));
    let images = run
        .glyphs
        .iter()
        .filter(|glyph| {
            swash
                .get_image(&mut fonts, glyph.physical((0.0, 0.0), 1.0).cache_key)
                .as_ref()
                .is_some_and(|image| image.placement.width > 0 && image.placement.height > 0)
        })
        .count();
    assert_eq!(images, 2, "fixture must shape half-ka and ssa separately");
    assert!(!replace_unsupported_graphemes(
        &mut buf,
        &mut fonts,
        &mut swash,
        &[0]
    ));
    assert_eq!(buf.lines[0].text(), text);
    eprintln!("INDIC_SHAPING_PASS: retained {images} images in Devanagari Sangam MN");
}

#[test]
fn joined_and_skin_tone_clusters_have_one_image_or_one_replacement() {
    let mut fonts = fixture_fonts();
    let mut swash = SwashCache::new();
    let flags = Flags::GRAPHEME | Flags::WIDE_CHAR;
    for marks in [&['\u{200d}', '\u{1f4bb}'][..], &['\u{1f3fd}'][..]] {
        let single_image = cluster_needs_single_image(flags, '\u{1f469}', marks);
        assert!(single_image);
        assert!(cluster_needs_single_image(
            Flags::GRAPHEME,
            '\u{1f469}',
            marks
        ));
        assert!(!cluster_needs_single_image(
            Flags::WIDE_CHAR,
            '\u{1f469}',
            marks
        ));
        let mut text_marks = marks.to_vec();
        text_marks.push('\u{fe0e}');
        assert!(!cluster_needs_single_image(flags, '\u{1f469}', &text_marks));
        let cell = GridGlyphCell {
            column: 0,
            width: 2,
            clustered: true,
            text_presentation: false,
            single_image,
        };
        let text: String = std::iter::once('\u{1f469}')
            .chain(marks.iter().copied())
            .collect();
        let attrs = Attrs::new()
            .family(Family::Name(kettle_config::font::FAMILY))
            .metadata(cell.metadata());
        let mut buf = buffer(&mut fonts, &text, &attrs);
        // This fixture has no emoji ligatures. Refuse separate component
        // images instead of silently drawing them beyond the cluster's cell.
        replace_unsupported_graphemes(&mut buf, &mut fonts, &mut swash, &[0]);
        buf.shape_until_scroll(&mut fonts, false);
        assert!(buf.lines[0].text() == "\u{fffd}" || buf.lines[0].text() == text);
        let images = buf
            .layout_runs()
            .next()
            .unwrap()
            .glyphs
            .iter()
            .filter(|g| {
                swash
                    .get_image(&mut fonts, g.physical((0.0, 0.0), 1.0).cache_key)
                    .as_ref()
                    .is_some_and(|image| image.placement.width > 0 && image.placement.height > 0)
            })
            .count();
        assert_eq!(images, 1);
        let mut off = buffer(&mut fonts, &text, &attrs.metadata(0));
        assert!(!replace_unsupported_graphemes(
            &mut off,
            &mut fonts,
            &mut swash,
            &[0]
        ));
        assert_eq!(off.lines[0].text(), text);
    }
}

#[test]
fn arabic_shaped_runs_use_contextual_joining_forms() {
    let mut fonts = FontSystem::new();
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let text = "\u{628}\u{628}\u{628}";
    let cfg = Config::default();
    for ligatures in [true, false] {
        let mut cfg = cfg.clone();
        cfg.font_ligatures = ligatures;
        let ff = font_features(&cfg);
        let attrs = run_attrs(&cfg, &ff, Rgb::new(100, 100, 100), false, false);
        let mut joined = buffer(&mut fonts, text, &attrs);
        let mut spans = AttrsList::new(&attrs);
        for (column, (byte, c)) in text.char_indices().enumerate() {
            let cell = GridGlyphCell {
                column,
                width: 1,
                clustered: true,
                text_presentation: false,
                single_image: false,
            };
            spans.add_span(
                byte..byte + c.len_utf8(),
                &attrs.clone().metadata(cell.metadata()),
            );
        }
        joined.lines[0].set_attrs_list(spans);
        joined.shape_until_scroll(&mut fonts, false);
        let isolated = buffer(&mut fonts, "\u{628}", &attrs);
        let run = joined.layout_runs().next().unwrap();
        let single = &isolated.layout_runs().next().unwrap().glyphs[0];
        if single.glyph_id == 0 {
            eprintln!("ARABIC_FONT_SKIP: bundled fonts lack Arabic; no host fallback available");
            return;
        }
        assert_eq!(run.glyphs.len(), 3);
        assert!(
            run.glyphs
                .iter()
                .all(|g| g.font_id == single.font_id && g.glyph_id != 0)
        );
        assert!(
            run.glyphs.iter().all(|g| g.glyph_id != single.glyph_id),
            "joining must differ from the isolated form, ligatures={ligatures}"
        );
        let forms: std::collections::HashSet<_> = run.glyphs.iter().map(|g| g.glyph_id).collect();
        assert_eq!(
            forms.len(),
            3,
            "initial, medial and final forms must differ"
        );
        eprintln!(
            "ARABIC_JOINING_PASS: ligatures={ligatures}, font={:?}",
            fonts.db().face(single.font_id).map(|face| &face.families)
        );
    }
}

#[test]
fn available_emoji_font_shapes_supported_clusters_without_decomposing() {
    let mut fonts = FontSystem::new();
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let mut swash = SwashCache::new();
    for text in ["\u{1f469}\u{200d}\u{1f4bb}", "\u{1f44d}\u{1f3fd}"] {
        let cell = GridGlyphCell {
            column: 0,
            width: 2,
            clustered: true,
            text_presentation: false,
            single_image: true,
        };
        let attrs = Attrs::new()
            .family(Family::Name(kettle_config::font::FAMILY))
            .metadata(cell.metadata());
        let mut buf = buffer(&mut fonts, text, &attrs);
        if buf
            .layout_runs()
            .next()
            .unwrap()
            .glyphs
            .iter()
            .any(|g| g.glyph_id == 0)
        {
            eprintln!(
                "EMOJI_FONT_SKIP: host lacks {text:?}; portable fallback is tested separately"
            );
            continue;
        }
        assert!(
            !replace_unsupported_graphemes(&mut buf, &mut fonts, &mut swash, &[0]),
            "supported cluster {text:?} must retain its glyph"
        );
        let images = buf
            .layout_runs()
            .next()
            .unwrap()
            .glyphs
            .iter()
            .filter(|g| {
                swash
                    .get_image(&mut fonts, g.physical((0.0, 0.0), 1.0).cache_key)
                    .as_ref()
                    .is_some_and(|image| image.placement.width > 0 && image.placement.height > 0)
            })
            .count();
        assert_eq!(images, 1, "{text:?}");
    }
}

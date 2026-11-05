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

#[test]
fn joined_and_skin_tone_clusters_have_one_image_or_one_replacement() {
    let mut fonts = fixture_fonts();
    let mut swash = SwashCache::new();
    let flags = Flags::GRAPHEME | Flags::WIDE_CHAR;
    for marks in [&['\u{200d}', '\u{1f4bb}'][..], &['\u{1f3fd}'][..]] {
        let single_image = cluster_needs_single_image(flags, false, marks);
        assert!(single_image);
        assert!(cluster_needs_single_image(Flags::GRAPHEME, false, marks));
        assert!(!cluster_needs_single_image(Flags::WIDE_CHAR, false, marks));
        assert!(!cluster_needs_single_image(flags, true, marks));
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

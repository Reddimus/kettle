//! Headless frames of combining marks the shaping font does not place.
//!
//! The glyph instances are read in shaped order: a cluster's base, then its
//! marks in canonical order.

use crate::gpu_tests::gpu_test_guard;
use crate::headless_tests::{capture, focused, pane, renderer, snapshot_of};
use crate::*;

const W: u32 = 320;
const H: u32 = 96;

/// A glyph instance's image rectangle: x, y, width, height.
type Image = [f32; 4];

/// The frame and glyph images of `text` alone in a pane, cursor hidden.
fn frame_of(renderer: &mut Renderer, cfg: &Config, text: &str) -> (image::RgbaImage, Vec<Image>) {
    let snap = snapshot_of(16, 3, format!("{text}\x1b[?25l").as_bytes());
    frame_of_snapshot(renderer, cfg, &snap)
}

fn frame_of_snapshot(
    renderer: &mut Renderer,
    cfg: &Config,
    snap: &PaneSnapshot,
) -> (image::RgbaImage, Vec<Image>) {
    let frame = capture(renderer, cfg, &[pane(snap, W, H)], &focused(false));
    let images = renderer
        .glyph_instances
        .iter()
        .map(|glyph| [glyph.pos[0], glyph.pos[1], glyph.size[0], glyph.size[1]])
        .collect();
    (frame, images)
}

/// The fallback placement cached for the first pane's `row`.
fn row_shifts(renderer: &Renderer, row: usize) -> &[mark_fallback::MarkShift] {
    mark_fallback::shifts_of(&renderer.pane_mark_shifts[0], row)
}

/// Whether the first cell's cluster was shaped to `glyphs` real glyphs, none
/// missing. The row's pad blank follows them.
fn shaped_to(renderer: &Renderer, glyphs: usize) -> bool {
    renderer.pane_buffers[0]
        .layout_runs()
        .next()
        .is_some_and(|run| {
            let cluster: Vec<_> = run.glyphs.iter().filter(|glyph| glyph.start == 0).collect();
            cluster.len() == glyphs && cluster.iter().all(|glyph| glyph.glyph_id != 0)
        })
}

fn top(image: &Image) -> f32 {
    image[1]
}

fn bottom(image: &Image) -> f32 {
    image[1] + image[3]
}

fn centre(image: &Image) -> f32 {
    image[0] + image[2] / 2.0
}

/// Whether the frame has ink in rows `rows`, between columns `x0..x1`.
fn ink_in(frame: &image::RgbaImage, rows: std::ops::Range<u32>, x0: u32, x1: u32) -> bool {
    let background = *frame.get_pixel(W - 1, H - 1);
    rows.into_iter()
        .any(|y| (x0..x1).any(|x| *frame.get_pixel(x, y) != background))
}

/// `Z\u{324}\u{308}` from the C08 fixture. JetBrains Mono lacks U+0324, so
/// the cluster falls back to a face that has all three; San Francisco on
/// macOS has GPOS but no mark anchors, and drew the diaeresis inside the
/// capital with both marks off its right edge.
#[test]
fn stacked_marks_clear_a_capital_whose_font_does_not_anchor_them() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    let (frame, glyphs) = frame_of(&mut renderer, &cfg, "Z\u{324}\u{308}");
    if !shaped_to(&renderer, 3) {
        eprintln!(
            "MARK_FONT_SKIP: no face here shapes Z\\u{{324}}\\u{{308}} to three glyphs; skipped"
        );
        return;
    }
    let [base, below, above] = [glyphs[0], glyphs[1], glyphs[2]];
    assert!(
        bottom(&above) <= top(&base),
        "the diaeresis must sit above the capital: {above:?} over {base:?}"
    );
    assert!(
        top(&below) >= bottom(&base),
        "the diaeresis below must hang under the baseline: {below:?} under {base:?}"
    );
    for mark in [above, below] {
        assert!(
            (centre(&mark) - centre(&base)).abs() <= 1.0,
            "a mark must centre on the capital: {mark:?} on {base:?}"
        );
    }
    let (x0, x1) = (base[0] as u32, (base[0] + base[2]) as u32);
    assert!(
        ink_in(&frame, 0..top(&base) as u32, x0, x1),
        "the frame must have ink above the cap top"
    );
    assert!(
        ink_in(&frame, bottom(&base) as u32..H, x0, x1),
        "the frame must have ink below the baseline"
    );
}

/// `a\u{300}\u{316}` from the same fixture: the shaper composes `à` and the
/// grave below stays a mark.
#[test]
fn a_grave_stack_keeps_ink_above_and_below() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    let (_, plain) = frame_of(&mut renderer, &cfg, "a");
    let x_height = top(&plain[0]);
    let (frame, glyphs) = frame_of(&mut renderer, &cfg, "a\u{300}\u{316}");
    if !shaped_to(&renderer, 2) {
        eprintln!(
            "MARK_FONT_SKIP: no face here shapes a\\u{{300}}\\u{{316}} to two glyphs; skipped"
        );
        return;
    }
    let [base, below] = [glyphs[0], glyphs[1]];
    let (x0, x1) = (base[0] as u32, (base[0] + base[2]) as u32);
    assert!(
        ink_in(&frame, 0..x_height as u32 - 1, x0, x1),
        "the grave must rise above the x-height"
    );
    assert!(
        top(&below) >= bottom(&base),
        "the grave below must hang under the baseline: {below:?} under {base:?}"
    );
    assert!((centre(&below) - centre(&base)).abs() <= 1.5);
}

/// The original mixed input includes U+0316, which JetBrains Mono lacks.
/// Fira Sans supplies an anchored grave below; host fallbacks need not do so.
const PLACED_TEXT: &str =
    "e\u{301} Z\u{308} x\u{308}\u{301} q\u{323} s\u{323}\u{307} a\u{300}\u{316}";

fn anchored_fonts() -> FontSystem {
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    load_bundled_font(
        &mut fonts,
        include_bytes!("../../../assets/fonts/fira-sans/FiraSans-Regular.ttf"),
    );
    fonts
}

fn shape_anchored(
    text: &str,
    features: FontFeatures,
) -> (FontSystem, Vec<glyphon::cosmic_text::LayoutGlyph>) {
    let mut fonts = anchored_fonts();
    let mut buffer = TextBuffer::new(&mut fonts, Metrics::new(26.0, 32.5));
    buffer.set_wrap(Wrap::None);
    buffer.set_text(
        text,
        &Attrs::new()
            .family(Family::Name(kettle_config::font::FAMILY))
            .font_features(features),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fonts, false);
    let glyphs = buffer
        .layout_runs()
        .next()
        .expect("one line")
        .glyphs
        .to_vec();
    (fonts, glyphs)
}

/// Portable checks keep the pixel fixture's composition, zero-offset anchors,
/// offset mark-to-base anchors and mark-to-mark attachment from disappearing.
#[test]
fn anchored_fixture_covers_the_intended_shaping_cases() {
    for (text, ids, face) in [
        ("e\u{301}", &[172][..], "JetBrainsMonoNF-Regular"),
        ("Z\u{308}", &[62, 12082][..], "JetBrainsMonoNF-Regular"),
        (
            "x\u{308}\u{301}",
            &[92, 12082, 12085][..],
            "JetBrainsMonoNF-Regular",
        ),
        ("q\u{323}", &[85, 385][..], "JetBrainsMonoNF-Regular"),
        (
            "s\u{323}\u{307}",
            &[87, 12098, 12083][..],
            "JetBrainsMonoNF-Regular",
        ),
        ("a\u{300}\u{316}", &[370, 2679][..], "FiraSans-Regular"),
    ] {
        assert!(PLACED_TEXT.contains(text));
        let (mut fonts, glyphs) = shape_anchored(text, FontFeatures::new());
        assert_eq!(
            glyphs.iter().map(|g| g.glyph_id).collect::<Vec<_>>(),
            ids,
            "{text:?}"
        );
        for glyph in &glyphs {
            assert_eq!(
                fonts.db().face(glyph.font_id).unwrap().post_script_name,
                face
            );
        }
        assert!(glyphs[1..].iter().all(|g| g.w == 0.0));
        assert!(
            mark_fallback::line_shifts(
                &glyphs,
                text,
                &mut fonts,
                &mut SwashCache::new(),
                &mut mark_fallback::MarkCache::default(),
            )
            .is_empty(),
            "{text:?}"
        );
    }
    for (text, tag, index, zero_offset) in [
        ("Z\u{308}", b"mark", 1, true),
        ("q\u{323}", b"mark", 1, true),
        ("x\u{308}\u{301}", b"mark", 1, false),
        ("x\u{308}\u{301}", b"mkmk", 2, false),
        ("a\u{300}\u{316}", b"mark", 1, false),
    ] {
        let (_, placed) = shape_anchored(text, FontFeatures::new());
        let mut features = FontFeatures::new();
        features.disable(FeatureTag::new(tag));
        let (mut fonts, unplaced) = shape_anchored(text, features.clone());
        let offsets = |g: &glyphon::cosmic_text::LayoutGlyph| (g.x_offset, g.y_offset);
        if zero_offset {
            assert_eq!(offsets(&placed[index]), (0.0, 0.0), "{text:?}");
            assert_eq!(offsets(&unplaced[index]), (0.0, 0.0), "{text:?}");
            let mut cache = mark_fallback::MarkCache::default();
            cache.use_features(&features);
            let shifts = mark_fallback::line_shifts(
                &unplaced,
                text,
                &mut fonts,
                &mut SwashCache::new(),
                &mut cache,
            );
            assert_eq!(
                shifts.len(),
                1,
                "disabling the zero-offset anchor must expose {text:?}"
            );
        } else {
            assert_ne!(
                offsets(&placed[index]),
                offsets(&unplaced[index]),
                "{text:?}: {tag:?}"
            );
        }
    }
}

/// Marks the font anchors (mark-to-base with a zero offset, mark-to-base and
/// mark-to-mark with offsets) or the shaper placed draw exactly as they did
/// before fallback placement existed.
#[test]
fn marks_the_font_places_render_exactly_as_before() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    renderer.font_system = anchored_fonts();
    renderer.text_symbol_family = None;
    renderer.emoji_family = None;
    renderer.remeasure_cell();
    let (placed, placed_images) = frame_of(&mut renderer, &cfg, PLACED_TEXT);
    assert!(
        (0..3).all(|row| row_shifts(&renderer, row).is_empty()),
        "no mark here is the fallback's to move: {:?}",
        renderer.pane_mark_shifts[0]
    );
    // The same rows emitted with no fallback placement at all.
    renderer.pane_mark_shifts[0].fill(Some(Box::default()));
    renderer.grid_glyphs_dirty = true;
    let (unplaced, unplaced_images) = frame_of(&mut renderer, &cfg, PLACED_TEXT);
    assert_eq!(
        placed_images, unplaced_images,
        "font-placed glyph rectangles must not move"
    );
    assert!(placed == unplaced, "font-placed marks must not move");
}

/// The bundled JetBrains Mono has no anchor on `#`, `1` or `@`, so this runs
/// the same on every host.
#[test]
fn an_unanchored_bundled_mark_clears_its_base() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    for (text, above) in [("#\u{301}", true), ("1\u{301}", true), ("@\u{323}", false)] {
        let (_, glyphs) = frame_of(&mut renderer, &cfg, text);
        assert!(shaped_to(&renderer, 2), "{text:?}");
        let [base, mark] = [glyphs[0], glyphs[1]];
        if above {
            assert!(
                bottom(&mark) <= top(&base),
                "{text:?}: the accent must sit on the base: {mark:?} over {base:?}"
            );
        } else {
            assert!(
                top(&mark) >= bottom(&base),
                "{text:?}: the dot must hang under the base: {mark:?} under {base:?}"
            );
        }
        assert!(
            (centre(&mark) - centre(&base)).abs() <= 1.0,
            "{text:?}: {mark:?} on {base:?}"
        );
    }
}

/// Placement is worked out when a row is shaped and kept with it: a steady
/// frame or a zoom reuses it, and only a reshaped row recomputes it.
#[test]
fn mark_placement_is_cached_with_the_shaped_row() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    let snap = snapshot_of(16, 3, "#\u{301}\x1b[?25l".as_bytes());
    capture(&mut renderer, &cfg, &[pane(&snap, W, H)], &focused(false));
    let placed = row_shifts(&renderer, 0).to_vec();
    assert_eq!(placed.len(), 1, "the accent is placed once");

    // A marker the renderer would never compute survives frames that do not
    // reshape the row.
    let marker = [mark_fallback::MarkShift {
        index: 9,
        ..placed[0]
    }];
    renderer.pane_mark_shifts[0][0] = Some(Box::new(marker));
    capture(&mut renderer, &cfg, &[pane(&snap, W, H)], &focused(true));
    cfg.font_size += 2.0;
    renderer.set_font_size(cfg.font_size);
    capture(&mut renderer, &cfg, &[pane(&snap, W, H)], &focused(false));
    assert_eq!(
        row_shifts(&renderer, 0),
        marker,
        "a steady frame or a zoom must not redo the placement"
    );

    let changed = snapshot_of(16, 3, "1\u{301}\x1b[?25l".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&changed, W, H)],
        &focused(false),
    );
    assert_eq!(row_shifts(&renderer, 0).len(), 1);
    assert_ne!(row_shifts(&renderer, 0), marker, "a new row is placed");
}

/// A row past the buffer's height, as while a pane shrinks ahead of its
/// terminal, is shaped but not laid out. It is placed once it is laid out,
/// though its content, and so its row key, never changed.
#[test]
fn a_row_laid_out_late_is_placed_then() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    let snap = snapshot_of(16, 3, "\r\n\r\n#\u{301}\x1b[?25l".as_bytes());
    let short = (cfg.padding_y * 2.0 + renderer.cell_h * 1.5) as u32;
    capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, W, short)],
        &focused(false),
    );
    assert!(
        renderer.pane_buffers[0].lines[2].layout_opt().is_none(),
        "the fixture needs the last row unlaid out"
    );
    let (_, images) = frame_of_snapshot(&mut renderer, &cfg, &snap);
    assert_eq!(row_shifts(&renderer, 2).len(), 1, "the late row is placed");
    let [base, mark] = [images[0], images[1]];
    assert!(
        bottom(&mark) <= top(&base),
        "the accent must sit on the base: {mark:?} over {base:?}"
    );
}

/// A stack of unanchored marks stops a quarter line past its row instead of
/// covering the rows above.
#[test]
fn a_tall_unanchored_stack_stays_near_its_row() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    let marks = "\u{308}".repeat(12);
    let text = format!("Z\u{324}{marks}");
    let (_, glyphs) = frame_of(&mut renderer, &cfg, &text);
    if !shaped_to(&renderer, 14) || row_shifts(&renderer, 0).is_empty() {
        eprintln!("MARK_FONT_SKIP: no face here leaves this stack to the fallback; skipped");
        return;
    }
    let (_, row_top) = geometry::pane_grid_origin(
        (0.0, 0.0, W as f32, H as f32),
        (cfg.padding_x, cfg.padding_y),
    );
    let ceiling = row_top - renderer.cell_h * 0.25 - 1.0;
    for glyph in &glyphs {
        assert!(
            top(glyph) >= ceiling,
            "{glyph:?} reaches above {ceiling} into the rows above"
        );
    }
}

/// The focused block cursor's inverted glyph is drawn over the pane's. When
/// the pane moved a mark, the inverted copy must move with it; otherwise the
/// pane's mark shows in the text colour on the block, beside an inverted one
/// left where the font drew it.
#[test]
fn the_block_cursor_inverts_a_moved_mark_where_the_pane_drew_it() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(W, H) else {
        eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    // The default theme's cursor is its foreground, which would hide the
    // pane's copy on the block.
    cfg.theme.cursor = Rgb::new(0xe0, 0x90, 0x30);
    let distance = |pixel: &image::Rgba<u8>, rgb: Rgb| {
        [rgb.r, rgb.g, rgb.b]
            .iter()
            .zip(pixel.0)
            .map(|(&want, got)| u32::from(want.abs_diff(got)))
            .sum::<u32>()
    };
    for text in ["#\u{301}", "Z\u{324}\u{308}"] {
        let snap = snapshot_of(16, 3, format!("{text}\x1b[1;1H").as_bytes());
        let frame = capture(&mut renderer, &cfg, &[pane(&snap, W, H)], &focused(true));
        let glyphs = text.chars().count();
        if !shaped_to(&renderer, glyphs) || row_shifts(&renderer, 0).is_empty() {
            eprintln!("MARK_FONT_SKIP: no face here leaves {text:?} to the fallback; skipped");
            continue;
        }
        let pane: Vec<[i32; 4]> = renderer
            .glyph_instances
            .iter()
            .map(|glyph| {
                [
                    glyph.pos[0] as i32,
                    glyph.pos[1] as i32,
                    glyph.size[0] as i32,
                    glyph.size[1] as i32,
                ]
            })
            .collect();
        assert_eq!(
            renderer.cursor_mark_images, pane,
            "{text:?}: the inverted images must be the pane's"
        );

        let block_colour = color::cursor_block_color(&cfg.theme, &snap.colors);
        let inverted = color::cursor_glyph_color(&cfg.theme, &snap.colors, cfg.theme.background);
        let ink = renderer.glyph_instances[glyphs - 1];
        let text_colour = Rgb::new(
            (ink.color[0] * 255.0).round() as u8,
            (ink.color[1] * 255.0).round() as u8,
            (ink.color[2] * 255.0).round() as u8,
        );
        let (x0, y0) = geometry::pane_grid_origin(
            (0.0, 0.0, W as f32, H as f32),
            (cfg.padding_x, cfg.padding_y),
        );
        let in_block = |x: u32, y: u32| {
            (x0.ceil() as u32..(x0 + renderer.cell_w) as u32).contains(&x)
                && (y0.ceil() as u32..(y0 + renderer.cell_h) as u32).contains(&y)
        };
        // Nothing on the block is nearer the text colour than the block and
        // inverted colours: every pane pixel is drawn over.
        let ghosts = frame
            .enumerate_pixels()
            .filter(|&(x, y, pixel)| {
                let fg = distance(pixel, text_colour);
                in_block(x, y)
                    && fg < distance(pixel, block_colour)
                    && fg < distance(pixel, inverted)
            })
            .count();
        assert_eq!(ghosts, 0, "{text:?}: the pane's glyph shows on the block");
        // Where the moved mark covers the block, it is inverted. (Ink past the
        // block is drawn in the inverted colour too, as for any glyph.)
        let mark = renderer
            .pending_cursor_glyph
            .as_ref()
            .and_then(|cursor| cursor.marks.last())
            .expect("the cursor carries the cell's images");
        let [mx, my, mw, _] = mark.rect;
        for (offset, &coverage) in mark.mask.iter().enumerate() {
            let x = (mx + offset as i32 % mw) as u32;
            let y = (my + offset as i32 / mw) as u32;
            if coverage >= 160 && in_block(x, y) {
                let pixel = frame.get_pixel(x, y);
                assert!(
                    distance(pixel, inverted) < distance(pixel, text_colour),
                    "{text:?}: the moved mark must be inverted where the pane drew it, \
                     at ({x}, {y}): {pixel:?}"
                );
            }
        }
    }
}

/// Switching cursor pipelines must discard the previous cell's moved-mark
/// bounds, or a nearby built-in cursor can acquire an oversized patch.
#[test]
fn moved_mark_and_builtin_cursor_transitions_keep_the_current_patch() {
    let _serial = gpu_test_guard();
    for allow_mapping in [false, true] {
        let cfg = Config {
            background_opacity: 1.0,
            ..crate::gpu_tests::gpu_test_config()
        };
        let Some(mut renderer) =
            Renderer::headless_for_tests_with_mapping(&cfg, W, H, 1.0, false, allow_mapping)
                .expect("headless renderer builds")
        else {
            eprintln!("MARK_GPU_SKIP: no GPU adapter on this host");
            return;
        };
        eprintln!(
            "MARK_BUILTIN_GPU: mapping={allow_mapping}, mapped={}, {:?}",
            renderer.gpu.mapped_uploads(),
            renderer.gpu.adapter_info()
        );
        let mut snap = snapshot_of(32, 3, "#\u{301}\x1b[1;21H\u{2592}\x1b[1;1H".as_bytes());
        frame_of_snapshot(&mut renderer, &cfg, &snap);
        assert!(
            !renderer.cursor_mark_images.is_empty(),
            "bundled mark must move"
        );
        let whole = PatchRect {
            x: 0,
            y: 0,
            width: W,
            height: H,
        };
        for column in [20, 0, 20, 0] {
            snap.cursor.point.column = kettle_core::Column(column);
            frame_of_snapshot(&mut renderer, &cfg, &snap);
            let (patch, _) = renderer
                .plan_cursor_patch(&cfg)
                .expect("only the current cursor contributes patch bounds");
            let cursor = renderer
                .pending_cursor_glyph
                .as_ref()
                .expect("block cursor");
            if column == 20 {
                assert!(cursor.builtin.is_some());
                assert!(renderer.cursor_mark_images.is_empty());
                assert!(renderer.glyph_pipeline.cursor_clip.is_some());
            } else {
                assert!(!cursor.marks.is_empty());
                assert!(!renderer.cursor_mark_images.is_empty());
                assert!(renderer.glyph_pipeline.cursor_clip.is_none());
            }
            for phase in [true, false, true] {
                let full = renderer.scene_window_for_tests(&cfg, whole, phase);
                let cropped = renderer.scene_window_for_tests(&cfg, patch, phase);
                assert_eq!(
                    cropped,
                    image::imageops::crop_imm(&full, patch.x, patch.y, patch.width, patch.height,)
                        .to_image(),
                    "cursor crop: col={column}, phase={phase}"
                );
            }
        }
    }
}

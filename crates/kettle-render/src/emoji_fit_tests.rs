//! Emoji drawn at the size their cell shows them: whole pixels, 1:1, inside
//! their row, and the same images under the block cursor.

use crate::batching::{EmojiCellImages, GlyphPositioning, GlyphSource, emit_positioned_glyphs};
use crate::glyphpipe::GlyphSlot;
use crate::gpu_tests::gpu_test_guard;
use crate::headless_tests::{capture, focused, pane, snapshot_of};
use crate::*;

/// Font sizes of the C08 panel (13 and 16 px on macOS, 14 and 17 px on
/// Linux) at 1x, a fractional scale and 2x.
const SIZES: [f32; 3] = [13.0, 14.0, 17.0];
const SCALES: [f32; 3] = [1.0, 1.5, 2.0];

/// The margin an emoji keeps from its row, restated here so this file also
/// builds against a renderer without the rule (the revert check).
fn inset(line_height: f32) -> f32 {
    (line_height / 16.0).round().max(1.0)
}

/// `[x, y, w, h]` lies in whole pixels inside `area` less `margin` at the
/// top and bottom, and fills the whole pixels there in one dimension.
fn assert_fitted(rect: [f32; 4], area: (f32, f32, f32, f32), margin: f32, what: &str) {
    let [x, y, w, h] = rect;
    assert!(
        rect.iter().all(|v| v.fract() == 0.0),
        "{what}: {rect:?} must be whole pixels"
    );
    let (left, right) = (area.0, area.0 + area.2);
    let (top, bottom) = (area.1 + margin, area.1 + area.3 - margin);
    assert!(
        x >= left - 1e-3 && x + w <= right + 1e-3,
        "{what}: {rect:?} must stay in columns {left}..{right}"
    );
    assert!(
        y >= top - 1e-3 && y + h <= bottom + 1e-3,
        "{what}: {rect:?} must clear its row {area:?} by {margin} px"
    );
    let room_w = (right + 1e-3).floor() - (left - 1e-3).ceil();
    let room_h = (bottom + 1e-3).floor() - (top - 1e-3).ceil();
    assert!(
        w == room_w || h == room_h,
        "{what}: {rect:?} must fill its {room_w}x{room_h} room in one dimension"
    );
}

#[test]
fn a_fitted_rect_is_whole_pixels_inside_its_row_at_every_size_and_scale() {
    // Shaped-size images: Apple (square), Noto (136:128), a wide flag, a
    // tall narrow glyph, and a one-pixel sliver.
    let inks = [
        (13.0, 13.0),
        (17.0, 16.0),
        (22.0, 14.0),
        (9.0, 16.0),
        (1.0, 1.0),
    ];
    for size in SIZES {
        for scale in SCALES {
            let (cw, ch) = (0.6 * size * scale, 1.25 * size * scale);
            for column in [0usize, 7, 16] {
                for columns in [1usize, 2] {
                    let cell = (
                        12.0 + column as f32 * cw,
                        4.0 + 3.0 * ch,
                        columns as f32 * cw,
                        ch,
                    );
                    let margin = fonts::emoji_inset(ch);
                    assert_eq!(margin, inset(ch));
                    assert!(margin >= 1.0);
                    for ink in inks {
                        let [x, y, w, h] = fonts::fit_emoji_pixels(fonts::emoji_box(cell), ink)
                            .expect("an emoji fits its cell");
                        let rect = [x as f32, y as f32, w as f32, h as f32];
                        let what = format!("{size}px@{scale}x col {column}x{columns} ink {ink:?}");
                        assert_fitted(rect, cell, margin, &what);
                        // The aspect ratio holds to the nearest pixel.
                        let expected_h = w as f32 * ink.1 / ink.0;
                        assert!(
                            (h as f32 - expected_h).abs() <= 1.0
                                || (w as f32 - h as f32 * ink.0 / ink.1).abs() <= 1.0,
                            "{what}: {rect:?} keeps the ink's aspect"
                        );
                    }
                }
            }
        }
    }
}

/// A glyph source that draws nothing but records what the emit asked for:
/// shaped-size placements of `natural` size and fitted images.
struct Recorder<'a> {
    natural: [u32; 2],
    fitted: &'a mut Vec<[u32; 2]>,
    cells: &'a mut Vec<EmojiCellImages>,
}

impl GlyphSource for Recorder<'_> {
    fn slot(&mut self, _key: glyphon::cosmic_text::CacheKey) -> Option<GlyphSlot> {
        Some(GlyphSlot {
            kind: 1,
            atlas_x: 0.0,
            atlas_y: 0.0,
            w: self.natural[0] as f32,
            h: self.natural[1] as f32,
            left: 0,
            top: self.natural[1] as i32,
        })
    }

    fn fitted(
        &mut self,
        _key: glyphon::cosmic_text::CacheKey,
        natural: GlyphSlot,
        size: [u32; 2],
    ) -> Option<GlyphSlot> {
        assert_eq!([natural.w as u32, natural.h as u32], self.natural);
        self.fitted.push(size);
        Some(GlyphSlot {
            kind: 0,
            atlas_x: 100.0,
            atlas_y: 200.0,
            w: size[0] as f32,
            h: size[1] as f32,
            left: 0,
            top: 0,
        })
    }

    fn emoji_cell(&mut self, cell: EmojiCellImages) {
        self.cells.push(cell);
    }
}

fn bundled_fonts() -> FontSystem {
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    fonts
}

/// The emit draws an emoji cluster from images rasterized at exactly the
/// size it draws them, in whole pixels inside the row, whatever the source:
/// here a synthetic one, so this holds without any emoji font.
#[test]
fn the_grid_emit_draws_each_emoji_image_one_to_one_inside_its_row() {
    let mut fonts = bundled_fonts();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    for size in SIZES {
        for scale in SCALES {
            let (font, cw, ch) = (size * scale, 0.6 * size * scale, 1.25 * size * scale);
            for (text, images) in [("\u{26a1}X", 1usize), ("\u{26a1}\u{301}X", 2)] {
                let mut buf = TextBuffer::new(&mut fonts, Metrics::new(font, ch));
                buf.set_size(Some(1000.0), Some(ch * 2.0));
                buf.set_wrap(Wrap::None);
                buf.set_text(text, &attrs, Shaping::Advanced, None);
                buf.shape_until_scroll(&mut fonts, false);
                for natural in [[13, 13], [17, 16], [40, 40]] {
                    let (mut fitted, mut cells) = (Vec::new(), Vec::new());
                    let source = Recorder {
                        natural,
                        fitted: &mut fitted,
                        cells: &mut cells,
                    };
                    let origin = (12.25, 4.5);
                    let mut out = Vec::new();
                    emit_positioned_glyphs(
                        &mut out,
                        &buf,
                        &[],
                        origin,
                        cw,
                        GlyphPositioning::Grid(cw),
                        None,
                        GColor::rgb(255, 255, 255),
                        &mut Vec::new(),
                        source,
                    );
                    let what = format!("{text:?} {size}px@{scale}x natural {natural:?}");
                    assert_eq!(cells.len(), 1, "{what}: one emoji cell");
                    let cell = cells[0].clone();
                    assert_eq!((cell.line, cell.column), (0, 0), "{what}");
                    assert_eq!(cell.instances.len(), images, "{what}");
                    assert_eq!(fitted.len(), images, "{what}");
                    let row = (origin.0, origin.1, 2.0 * cw, ch);
                    let margin = inset(ch);
                    let mut union = [f32::INFINITY, f32::INFINITY, 0.0f32, 0.0f32];
                    for (glyph, size) in out[cell.instances.clone()].iter().zip(&fitted) {
                        assert_eq!(glyph.size, [size[0] as f32, size[1] as f32], "{what}");
                        assert_eq!(glyph.uv_size, glyph.size, "{what}: drawn 1:1");
                        assert_eq!((glyph.uv, glyph.kind), ([100.0, 200.0], 0), "{what}");
                        union[0] = union[0].min(glyph.pos[0]);
                        union[1] = union[1].min(glyph.pos[1]);
                        union[2] = union[2].max(glyph.pos[0] + glyph.size[0]);
                        union[3] = union[3].max(glyph.pos[1] + glyph.size[1]);
                    }
                    let rect = [union[0], union[1], union[2] - union[0], union[3] - union[1]];
                    assert_fitted(rect, row, margin, &what);
                }
            }
        }
    }
}

/// Every emoji instance a frame draws in `cells` (column, columns) of row 0.
fn emoji_instances(
    renderer: &Renderer,
    cfg: &Config,
    column: usize,
    columns: usize,
) -> Vec<GlyphInstance> {
    let left = cfg.padding_x + column as f32 * renderer.cell_w;
    let right = left + columns as f32 * renderer.cell_w;
    renderer
        .glyph_instances
        .iter()
        .filter(|glyph| {
            let centre = glyph.pos[0] + glyph.size[0] / 2.0;
            glyph.pos[1] < cfg.padding_y + renderer.cell_h && left <= centre && centre < right
        })
        .copied()
        .collect()
}

/// Draw `text` in a headless pane at `size` px and `scale`, with only the
/// bundled fonts unless `system` asks for the host's.
fn frame(
    size: f32,
    scale: f32,
    system: bool,
    text: &str,
    overlay: &Overlay,
) -> Option<(Renderer, Config, image::RgbaImage)> {
    let mut cfg = crate::gpu_tests::gpu_test_config();
    cfg.font_size = size;
    cfg.text_renderer = TextRendererMode::Grid;
    cfg.minimum_contrast = 1.0;
    // An opaque black window: black and white survive the sRGB round trip
    // exactly, so a block cursor in the background colour leaves no trace of
    // its own, while a translucent window would blend edges differently over
    // the opaque block.
    cfg.background_opacity = 1.0;
    cfg.theme.background = Rgb::new(0, 0, 0);
    cfg.theme.foreground = Rgb::new(255, 255, 255);
    cfg.theme.cursor = cfg.theme.background;
    cfg.theme.cursor_text = cfg.theme.foreground;
    let (w, h) = ((400.0 * scale) as u32, (120.0 * scale) as u32);
    let mut renderer = Renderer::headless_for_tests_with(&cfg, w, h, scale, false)
        .expect("headless renderer builds")?;
    if !system {
        renderer.font_system = bundled_fonts();
        renderer.text_symbol_family = None;
        renderer.emoji_family = None;
        renderer.remeasure_cell();
    }
    let snap = snapshot_of(20, 4, text.as_bytes());
    let image = capture(&mut renderer, &cfg, &[pane(&snap, w, h)], overlay);
    Some((renderer, cfg, image))
}

/// The live renderer draws an emoji 1:1 in whole pixels inside its row: the
/// bundled font's lightning on every host, and a colour 😀 where the host
/// has a colour emoji face.
#[test]
fn a_frame_draws_emoji_one_to_one_inside_the_row() {
    let _serial = gpu_test_guard();
    let mut checked_colour = false;
    for size in SIZES {
        for scale in SCALES {
            for (system, text) in [(false, "\u{26a1}X"), (true, "\u{1f600}X")] {
                let input = format!("\x1b[?2027h{text}\x1b[?25l");
                let Some((renderer, cfg, _)) = frame(size, scale, system, &input, &focused(false))
                else {
                    eprintln!("EMOJI_FIT_GPU_SKIP: no GPU adapter on this host");
                    return;
                };
                if system && renderer.emoji_family.is_none() {
                    eprintln!("EMOJI_FONT_SKIP: no colour emoji face; the bundled lightning ran");
                    continue;
                }
                let glyphs = emoji_instances(&renderer, &cfg, 0, 2);
                let what = format!("{text:?} {size}px@{scale}x");
                assert_eq!(glyphs.len(), 1, "{what}: one image");
                let glyph = glyphs[0];
                checked_colour |= system && glyph.kind == 0;
                assert_eq!(glyph.uv_size, glyph.size, "{what}: drawn 1:1");
                let row = (
                    cfg.padding_x,
                    cfg.padding_y,
                    2.0 * renderer.cell_w,
                    renderer.cell_h,
                );
                assert_fitted(
                    [glyph.pos[0], glyph.pos[1], glyph.size[0], glyph.size[1]],
                    row,
                    inset(renderer.cell_h),
                    &what,
                );
            }
        }
    }
    if checked_colour {
        eprintln!("EMOJI_FIT_COLOUR_PASS");
    }
}

/// The block cursor over an emoji draws the pane's own images, so moving the
/// cursor onto it changes no emoji pixel. The cursor block is the background
/// colour here, so the whole cell must match the frame with the cursor
/// elsewhere.
#[test]
fn the_block_cursor_draws_the_emoji_the_pane_draws() {
    let _serial = gpu_test_guard();
    for scale in SCALES {
        for size in [13.0, 17.0] {
            for (system, emoji) in [
                (false, "\u{26a1}"),
                (true, "\u{1f600}"),
                (true, "\u{2764}\u{fe0f}"),
            ] {
                let away = format!("\x1b[?2027h{emoji}X\x1b[3;1H");
                let over = format!("\x1b[?2027h{emoji}X\x1b[1;1H");
                let Some((renderer, cfg, reference)) =
                    frame(size, scale, system, &away, &focused(true))
                else {
                    eprintln!("EMOJI_CURSOR_GPU_SKIP: no GPU adapter on this host");
                    return;
                };
                if system && renderer.emoji_family.is_none() {
                    eprintln!("EMOJI_FONT_SKIP: no colour emoji face; the bundled lightning ran");
                    continue;
                }
                let (_, _, actual) =
                    frame(size, scale, system, &over, &focused(true)).expect("adapter");
                let x = cfg.padding_x.floor() as u32;
                let y = cfg.padding_y.floor() as u32;
                let w = (2.0 * renderer.cell_w).ceil() as u32 + 1;
                let h = renderer.cell_h.ceil() as u32 + 1;
                let crop = |image: &image::RgbaImage| {
                    image::imageops::crop_imm(image, x, y, w, h).to_image()
                };
                let (reference, actual) = (crop(&reference), crop(&actual));
                let differing = reference
                    .pixels()
                    .zip(actual.pixels())
                    .filter(|(a, b)| a != b)
                    .count();
                assert_eq!(
                    differing, 0,
                    "{emoji:?} {size}px@{scale}x: the cursor changed {differing} emoji pixels"
                );
            }
        }
    }
}

/// Fitted emoji images are keyed by their pixel size: one glyph fitted to one
/// and to two columns in the same frame keeps both images, and a font-size
/// change drops every image and placement of the old size.
#[test]
fn fitted_emoji_images_follow_their_size() {
    let _serial = gpu_test_guard();
    let text = "\x1b[?2027h\u{a9}\u{fe0f}X\r\n\x1b[?2027l\u{a9}\u{fe0f}X\x1b[?25l";
    let Some((mut renderer, cfg, _)) = frame(14.0, 1.0, false, text, &focused(false)) else {
        eprintln!("EMOJI_FIT_GPU_SKIP: no GPU adapter on this host");
        return;
    };
    let sizes = |renderer: &Renderer| {
        let mut sizes: Vec<[u16; 2]> = renderer
            .glyph_instances
            .iter()
            .filter(|glyph| glyph.uv_size == glyph.size)
            .map(|glyph| [glyph.size[0] as u16, glyph.size[1] as u16])
            .collect();
        sizes.sort_unstable();
        sizes.dedup();
        sizes
    };
    let mut held = renderer.glyph_pipeline.emoji_slot_sizes();
    held.sort_unstable();
    assert_eq!(held.len(), 2, "two widths of one glyph: {held:?}");
    assert!(held.iter().all(|size| sizes(&renderer).contains(size)));
    let before = held;
    renderer.set_font_size(17.0);
    let snap = snapshot_of(20, 4, text.as_bytes());
    let (w, h) = (400, 120);
    let _ = capture(&mut renderer, &cfg, &[pane(&snap, w, h)], &focused(false));
    let mut after = renderer.glyph_pipeline.emoji_slot_sizes();
    after.sort_unstable();
    assert_eq!(after.len(), 2, "{after:?}");
    assert!(
        after.iter().all(|size| !before.contains(size)),
        "no image of the old size survives: {before:?} -> {after:?}"
    );
    let physical = renderer.metrics.font_size;
    let placed = renderer.glyph_pipeline.placement_font_sizes();
    assert!(!placed.is_empty());
    assert!(
        placed.iter().all(|&size| size == physical),
        "only placements at {physical} px are held: {placed:?}"
    );
}

/// The host's colour emoji `text` shaped at `size` px, or `None` without a
/// colour emoji face.
fn host_emoji(
    fonts: &mut FontSystem,
    text: &str,
    size: f32,
) -> Option<glyphon::cosmic_text::LayoutGlyph> {
    let family = fonts::resolve_emoji_family(fonts)?;
    let mut buf = TextBuffer::new(fonts, Metrics::new(size, size * 1.25));
    buf.set_wrap(Wrap::None);
    buf.set_text(
        text,
        &Attrs::new().family(Family::Name(family)),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(fonts, false);
    let glyph = buf.layout_runs().next()?.glyphs.first()?.clone();
    (glyph.glyph_id != 0).then_some(glyph)
}

/// Premultiplied area average of `src` (`w` x `h` straight RGBA) to
/// `to` pixels, integrating each target pixel's footprint directly: an
/// independent check on the production resampler.
fn reference_resample(src: &[u8], (w, h): (u32, u32), to: [u32; 2]) -> Vec<[f64; 4]> {
    let (sx, sy) = (w as f64 / to[0] as f64, h as f64 / to[1] as f64);
    let mut out = Vec::new();
    for ty in 0..to[1] {
        for tx in 0..to[0] {
            let (x0, x1) = (tx as f64 * sx, (tx + 1) as f64 * sx);
            let (y0, y1) = (ty as f64 * sy, (ty + 1) as f64 * sy);
            let mut sum = [0f64; 4];
            for y in y0.floor() as u32..(y1.ceil() as u32).min(h) {
                let wy = y1.min(y as f64 + 1.0) - y0.max(y as f64);
                for x in x0.floor() as u32..(x1.ceil() as u32).min(w) {
                    let wx = x1.min(x as f64 + 1.0) - x0.max(x as f64);
                    let p = &src[((y * w + x) * 4) as usize..][..4];
                    let a = p[3] as f64 / 255.0;
                    for c in 0..3 {
                        sum[c] += p[c] as f64 * a * wx * wy;
                    }
                    sum[3] += p[3] as f64 * wx * wy;
                }
            }
            out.push(sum.map(|v| v / (sx * sy)));
        }
    }
    out
}

/// A colour bitmap emoji is resampled from its face's largest strike to the
/// exact size its cell draws it, matching an independent area average of
/// that strike. Skipped on a host without a colour emoji face.
#[test]
fn a_colour_bitmap_emoji_is_an_area_average_of_its_largest_strike() {
    let mut fonts = FontSystem::new();
    let mut swash = SwashCache::new();
    if fonts::resolve_emoji_family(&mut fonts).is_none() {
        eprintln!("EMOJI_FONT_SKIP: no colour emoji face on this host");
        return;
    }
    let mut checked = 0;
    for text in [
        "\u{1f600}",
        "\u{2764}\u{fe0f}",
        "\u{1f469}\u{200d}\u{1f4bb}",
    ] {
        for size in SIZES {
            for scale in SCALES {
                let font = size * scale;
                let Some(glyph) = host_emoji(&mut fonts, text, font) else {
                    eprintln!("EMOJI_FONT_SKIP: host face lacks {text:?}");
                    continue;
                };
                let key = glyph.physical((0.0, 0.0), 1.0).cache_key;
                let natural = swash.get_image_uncached(&mut fonts, key).expect("image");
                if natural.content != glyphon::SwashContent::Color {
                    eprintln!("EMOJI_FONT_SKIP: {text:?} is not a colour bitmap here");
                    continue;
                }
                let (cw, ch) = (0.6 * font, 1.25 * font);
                let ink = (
                    natural.placement.width as f32,
                    natural.placement.height as f32,
                );
                let [_, _, w, h] =
                    fonts::fit_emoji_pixels(fonts::emoji_box((0.0, 0.0, 2.0 * cw, ch)), ink)
                        .expect("fits");
                let size_px = [w as u32, h as u32];
                let slot = GlyphSlot {
                    kind: 0,
                    atlas_x: 0.0,
                    atlas_y: 0.0,
                    w: ink.0,
                    h: ink.1,
                    left: natural.placement.left,
                    top: natural.placement.top,
                };
                let raster =
                    emoji_raster::fitted_emoji_raster(&mut fonts, &mut swash, key, slot, size_px)
                        .expect("a fitted raster");
                let what = format!("{text:?} {size}px@{scale}x");
                assert!(raster.color, "{what}");
                assert_eq!([raster.width, raster.height], size_px, "{what}");
                // The source is the largest strike that holds the glyph,
                // drawn unscaled.
                let font_ref = fonts
                    .get_font(key.font_id, key.font_weight)
                    .expect("the shaped face");
                let largest = font_ref
                    .as_swash()
                    .color_strikes()
                    .filter(|strike| strike.contains(key.glyph_id))
                    .map(|strike| strike.ppem())
                    .filter(|&ppem| ppem <= 256)
                    .max()
                    .expect("a strike");
                let ppem = emoji_raster::source_ppem(&mut fonts, key, ink.1 as u32, size_px[1]);
                assert_eq!(ppem, f32::from(largest), "{what}");
                let (source, _, _) = glyphon::cosmic_text::CacheKey::new(
                    key.font_id,
                    key.glyph_id,
                    ppem,
                    (0.0, 0.0),
                    key.font_weight,
                    key.flags,
                );
                let strike = swash
                    .get_image_uncached(&mut fonts, source)
                    .expect("strike");
                let reference = reference_resample(
                    &strike.data,
                    (strike.placement.width, strike.placement.height),
                    size_px,
                );
                for (index, (pixel, expected)) in raster
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(&reference)
                    .enumerate()
                {
                    let a = pixel[3] as f64 / 255.0;
                    let actual = [
                        pixel[0] as f64 * a,
                        pixel[1] as f64 * a,
                        pixel[2] as f64 * a,
                        pixel[3] as f64,
                    ];
                    for c in 0..4 {
                        assert!(
                            (actual[c] - expected[c]).abs() <= 2.0,
                            "{what}: pixel {index} channel {c}: {actual:?} vs {expected:?}"
                        );
                    }
                }
                checked += 1;
            }
        }
    }
    eprintln!("EMOJI_BITMAP_PASS: {checked} rasters");
}

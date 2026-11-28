//! Timing of a full-screen glyph grid, run by hand:
//!
//! ```sh
//! CARGO_PROFILE_DEV_OPT_LEVEL=3 cargo test -p kettle-render --lib cpu_raster::bench_tests -- --ignored --nocapture
//! ```
//!
//! Each frame is what a terminal repaint draws: the base fill, a few cell
//! backgrounds, one real font glyph per cell, and a block cursor.

use std::time::Instant;

use super::glyphpipe::{GlyphAtlases, draw_glyphs};
use super::quad::{QuadBlend, draw_quads};
use super::{CpuFramebuffer, CpuTexture, CpuTextureFormat};
use crate::glyphpipe::GlyphInstance;
use crate::quad::QuadInstance;

/// Every printable ASCII character, rasterized from the bundled terminal
/// font at the Linux default 14 px by the same `SwashCache` call the GPU
/// atlas uses, so coverage and bitmap sizes are real.
struct Font {
    cell: [u32; 2],
    baseline: f32,
    /// `(atlas x, atlas y, width, height, left, top)` per character.
    glyphs: Vec<(u32, u32, u32, u32, i32, i32)>,
    mask: CpuTexture,
}

fn font() -> Font {
    use crate::{Attrs, Family, FontSystem, Metrics, Shaping, SwashCache, TextBuffer};
    use glyphon::cosmic_text::{Wrap, fontdb};
    let mut fonts = FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new());
    crate::fonts::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let mut swash = SwashCache::new();
    let mut buf = TextBuffer::new(&mut fonts, Metrics::new(14.0, 18.0));
    buf.set_size(Some(4000.0), Some(100.0));
    buf.set_wrap(Wrap::None);
    let text: String = (0x21u8..0x7f).map(char::from).collect();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    buf.set_text(&text, &attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(&mut fonts, false);
    let run = buf.layout_runs().next().expect("one shaped line");
    let advance = run.glyphs[0].w;
    let baseline = run.line_y;
    let keys: Vec<_> = run
        .glyphs
        .iter()
        .map(|g| g.physical((0.0, 0.0), 1.0).cache_key)
        .collect();
    let mut mask = CpuTexture::new(1024, 512, CpuTextureFormat::R8Unorm).unwrap();
    let (mut x, mut y, mut shelf) = (0u32, 0u32, 0u32);
    let mut glyphs = Vec::new();
    for key in keys {
        let image = swash.get_image_uncached(&mut fonts, key).expect("raster");
        let p = image.placement;
        if x + p.width + 1 > 1024 {
            (x, y, shelf) = (0, y + shelf + 1, 0);
        }
        assert!(mask.write(x, y, p.width, p.height, &image.data));
        glyphs.push((x, y, p.width, p.height, p.left, p.top));
        x += p.width + 1;
        shelf = shelf.max(p.height);
    }
    Font {
        cell: [advance.round() as u32, 18],
        baseline,
        glyphs,
        mask,
    }
}

struct Scene {
    base: Vec<QuadInstance>,
    cells: Vec<QuadInstance>,
    glyphs: Vec<GlyphInstance>,
    mask: CpuTexture,
    color: CpuTexture,
}

fn scene(font: &Font, w: u32, h: u32) -> Scene {
    let color = CpuTexture::new(1024, 512, CpuTextureFormat::Rgba8UnormSrgb).unwrap();
    let [cw, ch] = font.cell;
    let (cols, rows) = (w / cw, h / ch);
    let mut cells = Vec::new();
    let mut glyphs = Vec::new();
    for row in 0..rows {
        for col in 0..cols {
            let i = ((row * 31 + col * 7) as usize) % font.glyphs.len();
            let (ax, ay, gw, gh, left, top) = font.glyphs[i];
            let (x, y) = ((col * cw) as f32, (row * ch) as f32);
            if (row * 3 + col) % 11 == 0 {
                cells.push(QuadInstance {
                    pos: [x, y],
                    size: [cw as f32, ch as f32],
                    color: [0.2, 0.25, 0.35, 1.0],
                });
            }
            glyphs.push(GlyphInstance {
                pos: [x + left as f32, (y + font.baseline).round() - top as f32],
                size: [gw as f32, gh as f32],
                uv: [ax as f32, ay as f32],
                uv_size: [gw as f32, gh as f32],
                color: [0.85, 0.87, 0.9, 1.0],
                kind: 1,
                _pad: [0],
            });
        }
    }
    cells.push(QuadInstance {
        pos: [cw as f32 * 3.0, ch as f32 * 2.0],
        size: [cw as f32, ch as f32],
        color: [0.9, 0.9, 0.9, 1.0],
    });
    Scene {
        base: vec![QuadInstance {
            pos: [0.0, 0.0],
            size: [w as f32, h as f32],
            color: [0.1, 0.105, 0.137, 1.0],
        }],
        cells,
        glyphs,
        mask: font.mask.clone(),
        color,
    }
}

fn time_frames(w: u32, h: u32) {
    let font = font();
    let s = scene(&font, w, h);
    let screen = [w as f32, h as f32];
    let mut fb = CpuFramebuffer::new(w, h).unwrap();
    let atlases = GlyphAtlases {
        color: s.color.view(),
        mask: s.mask.view(),
    };
    let draw = |target: &mut super::CpuTarget<'_>, glyphs: bool| {
        draw_quads(target, screen, &s.base, QuadBlend::Replace, None);
        draw_quads(target, screen, &s.cells, QuadBlend::Blend, None);
        if glyphs {
            draw_glyphs(target, screen, &s.glyphs, atlases, None);
        }
    };
    for (glyphs, bands) in [(false, 1), (true, 1), (true, 2), (true, 4), (true, 8)] {
        let frame = |fb: &mut CpuFramebuffer, glyphs: bool| {
            fb.draw_banded(bands, |t| draw(t, glyphs));
        };
        for _ in 0..5 {
            frame(&mut fb, glyphs);
        }
        let n = 60;
        let start = Instant::now();
        let mut best = f64::INFINITY;
        for _ in 0..n {
            let t = Instant::now();
            frame(&mut fb, glyphs);
            best = best.min(t.elapsed().as_secs_f64());
        }
        let mean = start.elapsed().as_secs_f64() / f64::from(n);
        eprintln!(
            "cpu-bench {w}x{h} cell {:?} {}, {bands} band(s): mean {:.3} ms, best {:.3} ms, {} glyphs",
            font.cell,
            if glyphs { "grid" } else { "fills only" },
            mean * 1e3,
            best * 1e3,
            if glyphs { s.glyphs.len() } else { 0 },
        );
    }
}

#[test]
#[ignore = "timing; run by hand with opt-level 3"]
fn bench_full_screen_glyph_grid_1600x1000() {
    time_frames(1600, 1000);
}

#[test]
#[ignore = "timing; run by hand with opt-level 3"]
fn bench_full_screen_glyph_grid_3840x2160() {
    time_frames(3840, 2160);
}

/// The other full-screen costs a frame can carry at 1600x1000.
#[test]
#[ignore = "timing; run by hand with opt-level 3"]
fn bench_other_pipelines_1600x1000() {
    use super::imgpipe::draw_image;
    use super::present::present_straight;
    use super::rounded_surface::draw_rounded_surfaces;
    use super::starfield::{draw_starfield, frame_stars};
    use super::{AddressMode, CpuTextureView};
    use crate::rounded_surface::RoundedSurface;
    use crate::starfield::StarSeed;

    let (w, h) = (1600u32, 1000u32);
    let screen = [w as f32, h as f32];
    let mut fb = CpuFramebuffer::new(w, h).unwrap();
    let time = |name: &str, fb: &mut CpuFramebuffer, f: &dyn Fn(&mut CpuFramebuffer)| {
        f(fb);
        let n = 20;
        let start = Instant::now();
        for _ in 0..n {
            f(fb);
        }
        let ms = start.elapsed().as_secs_f64() * 1e3 / f64::from(n);
        eprintln!("cpu-bench {w}x{h} {name}: mean {ms:.3} ms");
    };
    // A 1280x800 wallpaper stretched over the window.
    let (iw, ih) = (1280u32, 800u32);
    let pixels: Vec<u8> = (0..iw * ih)
        .flat_map(|i| [(i % iw) as u8, (i / iw) as u8, (i * 7) as u8, 255])
        .collect();
    let texture = CpuTextureView::new(iw, ih, CpuTextureFormat::Rgba8UnormSrgb, &pixels).unwrap();
    let inst = crate::imgpipe::Inst {
        pos: [0.0, 0.0],
        size: screen,
        uv_origin: [0.0, 0.0],
        uv_size: [1.0, 1.0],
        uv_clamp: [-1.0; 4],
    };
    time("wallpaper image", &mut fb, &|fb| {
        draw_image(
            &mut fb.target(),
            screen,
            &inst,
            texture,
            AddressMode::ClampToEdge,
            None,
        );
    });
    let seeds = StarSeed::all();
    let (stars, count) = frame_stars(&seeds, screen, 12.5);
    let scratch = std::sync::Mutex::new(Vec::new());
    time("starfield", &mut fb, &|fb| {
        let mut scratch = scratch.lock().unwrap();
        draw_starfield(
            &mut fb.target(),
            screen,
            &stars[..count],
            None,
            &mut scratch,
        );
    });
    // A settings-sized panel with a three-layer shadow and a stroke.
    let panel = |grow: f32, alpha: f32, stroke: f32| RoundedSurface {
        pos: [400.0 - grow, 200.0 - grow],
        size: [800.0 + 2.0 * grow, 600.0 + 2.0 * grow],
        fill: [0.1, 0.1, 0.12, alpha],
        stroke: [0.4, 0.4, 0.45, 1.0],
        params: [12.0 + grow, stroke, 0.0, 0.0],
    };
    let surfaces = [
        panel(12.0, 0.08, 0.0),
        panel(6.0, 0.12, 0.0),
        panel(2.0, 0.2, 0.0),
        panel(0.0, 0.97, 1.0),
    ];
    time("settings panel with shadow", &mut fb, &|fb| {
        draw_rounded_surfaces(&mut fb.target(), screen, &surfaces, None);
    });
    let scene = fb.clone();
    time("present straight alpha", &mut fb, &|fb| {
        present_straight(&scene, fb);
    });
}

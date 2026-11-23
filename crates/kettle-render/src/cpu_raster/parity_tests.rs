//! Differential tests: each CPU pipeline against its wgpu pipeline.
//!
//! Every test draws the same instances, uniforms, textures and scissors
//! twice, once through the real pipeline on a headless device and once on
//! the CPU, over the same non-uniform starting frame, and compares every
//! channel of every pixel. The bound is [`TOLERANCE`] per channel.
//!
//! Each prints `cpu-parity <case>: max .. mean .. differing ..` for the
//! parity table. With `KETTLE_CPU_PARITY_DIR` set, each case also writes its
//! GPU frame, CPU frame and a difference image there as PNGs.
//!
//! A host with no adapter skips, unless `KETTLE_REQUIRE_GPU` is set.

use kettle_core::{GraphicsBudget, ImageData, ImageSourceCrop, ImageSourceRect};

use super::cursor_patch::combine_patch;
use super::glyphpipe::{GlyphAtlases, draw_glyphs, pane_scissor};
use super::imgpipe::draw_image;
use super::outline::draw_outlines;
use super::present::present_straight;
use super::quad::{QuadBlend, draw_quads};
use super::rounded_surface::draw_rounded_surfaces;
use super::starfield::{draw_starfield, frame_stars};
use super::{AddressMode, CpuFramebuffer, CpuTexture, CpuTextureFormat, CpuTextureView, PixelRect};
use crate::builtin_glyphs::{BuiltinKey, Strokes};
use crate::glyphpipe::{GlyphClip, GlyphInstance, GlyphPipeline, RasterGlyph};
use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::imgpipe::{ImageItem, ImagePipeline};
use crate::outline::{OutlineInstance, OutlinePipeline};
use crate::present::PresentationPipeline;
use crate::quad::{QuadInstance, QuadPipeline};
use crate::rounded_surface::{RoundedSurface, RoundedSurfacePipeline};
use crate::starfield::{StarSeed, StarfieldPipeline};

/// The largest difference allowed in any channel.
const TOLERANCE: u8 = 2;
/// The bound for stacks of many translucent layers. One layer differs by at
/// most one code (`srgb_stores_and_blends_match_the_gpu`): the GPU oracle's
/// sRGB encode is itself up to 0.65 of a code off the exact curve, where
/// the CPU rounds exactly. Every layer re-rounds to 8 bits on both sides,
/// so those differences can add up through a deep stack.
const LAYERED_TOLERANCE: u8 = 2 * TOLERANCE;
/// The headless renderer's scene format.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Gpu {
    _instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu(label: &str) -> Option<Gpu> {
    let resolved = pollster::block_on(crate::resolve_headless_adapter(&gpu_test_config(), label));
    let (instance, adapter) = match resolved {
        Ok(found) => found,
        Err(e) => {
            assert!(
                std::env::var_os("KETTLE_REQUIRE_GPU").is_none(),
                "no GPU adapter on this host, and KETTLE_REQUIRE_GPU is set: {e}"
            );
            eprintln!("no GPU adapter on this host; skipped");
            return None;
        }
    };
    eprintln!("cpu-parity adapter: {}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(label),
        required_limits: crate::gpu::live_device_limits(adapter.limits()),
        ..Default::default()
    }))
    .expect("headless device");
    Some(Gpu {
        _instance: instance,
        device,
        queue,
    })
}

impl Gpu {
    fn target(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("cpu-parity-target"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    }

    fn write(&self, texture: &wgpu::Texture, w: u32, h: u32, rgba: &[u8]) {
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }

    /// Start from `init` (RGBA8), record, and read the target back.
    fn render(
        &self,
        w: u32,
        h: u32,
        init: &[u8],
        record: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Vec<u8> {
        let texture = self.target(w, h);
        self.write(&texture, w, h, init);
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        record(&mut encoder, &view);
        self.read(encoder, &texture, w, h)
    }

    fn read(
        &self,
        mut encoder: wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        w: u32,
        h: u32,
    ) -> Vec<u8> {
        let row = (w * 4).div_ceil(256) * 256;
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("cpu-parity-readback"),
            size: u64::from(row) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(h),
                },
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU poll");
        rx.recv().expect("map channel").expect("buffer mapping");
        let data = slice.get_mapped_range().expect("mapped range");
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            let start = y * row as usize;
            out.extend_from_slice(&data[start..start + w as usize * 4]);
        }
        drop(data);
        readback.unmap();
        out
    }
}

/// A render pass that keeps the target's contents.
fn load_pass<'e>(
    encoder: &'e mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
) -> wgpu::RenderPass<'e> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("cpu-parity-pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// A small deterministic generator (SplitMix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in `0..1`.
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    fn byte(&mut self) -> u8 {
        self.next() as u8
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[(self.next() % items.len() as u64) as usize]
    }

    /// A pixel position with a fraction chosen to hit the rasterizer's edge
    /// cases: whole, half (on a pixel centre), quarters, thirds, near-half
    /// and arbitrary.
    fn coord(&mut self, lo: f32, hi: f32) -> f32 {
        let whole = self.range(lo, hi).floor();
        let frac = self.pick(&[
            0.0,
            0.5,
            0.25,
            0.75,
            1.0 / 3.0,
            0.5 + 1.0 / 1024.0,
            0.5 - 1.0 / 1024.0,
            -1.0,
        ]);
        whole + if frac < 0.0 { self.unit() } else { frac }
    }

    /// A pixel position as production geometry has them: whole, or an
    /// arbitrary fraction, never exactly on a pixel centre.
    fn coord_free(&mut self, lo: f32, hi: f32) -> f32 {
        let whole = self.range(lo, hi).floor();
        whole
            + if self.unit() < 0.5 {
                0.0
            } else {
                self.range(0.01, 0.99)
            }
    }

    /// A straight sRGB colour with an alpha from the interesting set.
    fn color(&mut self) -> [f32; 4] {
        let dark = self.unit() < 0.25;
        let c = |r: &mut Self| if dark { r.range(0.0, 0.06) } else { r.unit() };
        let a = self.pick(&[0.0, 0.05, 0.3, 0.5, 0.77, 1.0, 1.0, -1.0]);
        [
            c(self),
            c(self),
            c(self),
            if a < 0.0 { self.unit() } else { a },
        ]
    }
}

/// A starting frame with gradients, noise, dark and light regions, and a
/// translucent quadrant, so blends see every kind of destination.
fn background(w: u32, h: u32, seed: u64) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let base = [
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                ((x + y) * 7) as u8,
            ];
            let noisy = (x / 8 + y / 8) % 3 == 0;
            let rgb = if noisy {
                [rng.byte(), rng.byte(), rng.byte()]
            } else if (x / 16) % 4 == 1 {
                [base[0] / 16, base[1] / 16, base[2] / 16]
            } else {
                base
            };
            let a = if x > w / 2 && y > h / 2 {
                rng.byte()
            } else {
                255
            };
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
    }
    out
}

struct Stats {
    max: u8,
    mean: f64,
    differing: usize,
    over: usize,
    total: usize,
}

fn compare(case: &str, w: u32, h: u32, gpu: &[u8], cpu: &CpuFramebuffer) -> Stats {
    let cpu = cpu.to_rgba8();
    assert_eq!(gpu.len(), cpu.len(), "{case}: frame sizes");
    let mut stats = Stats {
        max: 0,
        mean: 0.0,
        differing: 0,
        over: 0,
        total: gpu.len(),
    };
    let mut sum = 0u64;
    let mut worst = None;
    for (i, (&g, &c)) in gpu.iter().zip(&cpu).enumerate() {
        let d = g.abs_diff(c);
        sum += u64::from(d);
        if d > 0 {
            stats.differing += 1;
        }
        if d > TOLERANCE {
            stats.over += 1;
        }
        if d > stats.max {
            stats.max = d;
            worst = Some(i);
        }
    }
    stats.mean = sum as f64 / stats.total.max(1) as f64;
    let at = worst.map(|i| {
        let px = i / 4;
        let (x, y) = (px as u32 % w, px as u32 / w);
        let p = px * 4;
        format!(
            " worst at ({x}, {y}) gpu {:?} cpu {:?}",
            &gpu[p..p + 4],
            &cpu[p..p + 4]
        )
    });
    eprintln!(
        "cpu-parity {case}: max {} mean {:.5} differing {}/{} channels{}",
        stats.max,
        stats.mean,
        stats.differing,
        stats.total,
        at.unwrap_or_default()
    );
    if let Some(dir) = std::env::var_os("KETTLE_CPU_PARITY_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let save = |name: &str, bytes: Vec<u8>| {
            let image = image::RgbaImage::from_raw(w, h, bytes).expect("frame size");
            image
                .save(dir.join(format!("{case}-{name}.png")))
                .expect("write parity PNG");
        };
        save("gpu", gpu.to_vec());
        save("cpu", cpu.clone());
        let diff = gpu
            .iter()
            .zip(&cpu)
            .enumerate()
            .map(|(i, (&g, &c))| {
                if i % 4 == 3 {
                    255
                } else {
                    g.abs_diff(c).saturating_mul(64)
                }
            })
            .collect();
        save("diff", diff);
    }
    stats
}

fn assert_parity(case: &str, w: u32, h: u32, gpu: &[u8], cpu: &CpuFramebuffer) {
    let stats = compare(case, w, h, gpu, cpu);
    assert!(
        stats.max <= TOLERANCE,
        "{case}: {} channels differ by more than {TOLERANCE} (max {})",
        stats.over,
        stats.max
    );
}

/// Pixels where the CPU frame itself moves past the tolerance when every
/// instance moves by 1/1000 px along either axis: a pixel centre, texel
/// edge or dash end lies exactly on the geometry, and which side wins
/// depends on rounding a GPU is free to do differently.
fn tie_mask(base: &CpuFramebuffer, render: impl Fn([f32; 2]) -> CpuFramebuffer) -> Vec<bool> {
    const D: f32 = 1.0e-3;
    let base = base.to_rgba8();
    let mut mask = vec![false; base.len() / 4];
    for nudge in [[D, 0.0], [-D, 0.0], [0.0, D], [0.0, -D]] {
        let moved = render(nudge).to_rgba8();
        for (i, (a, b)) in base
            .as_chunks::<4>()
            .0
            .iter()
            .zip(moved.as_chunks::<4>().0)
            .enumerate()
        {
            if a.iter().zip(b).any(|(a, b)| a.abs_diff(*b) > TOLERANCE) {
                mask[i] = true;
            }
        }
    }
    mask
}

/// Parity except at ties: every pixel past [`LAYERED_TOLERANCE`] must be
/// one the CPU frame itself flips under a 1/1000 px move ([`tie_mask`]).
/// These scenes stack layers as deep as the `-deep` cases.
fn assert_parity_but_ties(
    case: &str,
    w: u32,
    h: u32,
    gpu: &[u8],
    cpu: &CpuFramebuffer,
    ties: &[bool],
) {
    compare(case, w, h, gpu, cpu);
    let cpu = cpu.to_rgba8();
    let (mut over, mut unexplained) = (0, Vec::new());
    for (i, (g, c)) in gpu
        .as_chunks::<4>()
        .0
        .iter()
        .zip(cpu.as_chunks::<4>().0)
        .enumerate()
    {
        if g.iter().zip(c).any(|(g, c)| g.abs_diff(*c) > TOLERANCE) {
            over += 1;
            let layered = g
                .iter()
                .zip(c)
                .all(|(g, c)| g.abs_diff(*c) <= LAYERED_TOLERANCE);
            if !ties[i] && !layered {
                unexplained.push((i as u32 % w, i as u32 / w));
            }
        }
    }
    eprintln!(
        "cpu-parity {case}: {over} pixels past the tolerance, {} at ties, {} tie-sensitive pixels",
        (0..ties.len())
            .filter(|&i| ties[i] && {
                let (g, c) = (&gpu[i * 4..i * 4 + 4], &cpu[i * 4..i * 4 + 4]);
                g.iter().zip(c).any(|(g, c)| g.abs_diff(*c) > TOLERANCE)
            })
            .count(),
        ties.iter().filter(|&&t| t).count()
    );
    assert!(
        unexplained.is_empty(),
        "{case}: pixels past the layered tolerance away from any tie: {:?}",
        &unexplained[..unexplained.len().min(20)]
    );
}

fn random_quads(rng: &mut Rng, n: usize, w: f32, h: f32) -> Vec<QuadInstance> {
    (0..n)
        .map(|_| {
            let tiny = rng.unit() < 0.3;
            let (sw, sh) = if tiny {
                (rng.range(0.2, 3.0), rng.range(0.2, 3.0))
            } else {
                (rng.range(1.0, w * 0.5), rng.range(1.0, h * 0.5))
            };
            QuadInstance {
                pos: [rng.coord(-8.0, w), rng.coord(-8.0, h)],
                size: [sw, sh],
                color: rng.color(),
            }
        })
        .collect()
}

#[test]
fn quads_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-quads") else {
        return;
    };
    let (w, h) = (160u32, 96u32);
    let screen = [w as f32, h as f32];
    let mut rng = Rng(1);
    let init = background(w, h, 11);
    let base = random_quads(&mut rng, 12, screen[0], screen[1]);
    // About four translucent layers deep on average, as a UI stacks them.
    let mut over = random_quads(&mut rng, 80, screen[0], screen[1]);
    // Edges exactly on, and a hair either side of, pixel centres.
    for (i, frac) in [
        0.5f32,
        0.5 + 1.0 / 1024.0,
        0.5 - 1.0 / 1024.0,
        0.5 + 1.0 / 256.0,
    ]
    .into_iter()
    .enumerate()
    {
        let y = 4.0 + i as f32 * 6.0;
        over.push(QuadInstance {
            pos: [20.0 + frac, y + frac],
            size: [3.0, 3.0],
            color: [1.0, 0.2, 0.1, 1.0],
        });
        over.push(QuadInstance {
            pos: [30.25, y],
            size: [2.0 + frac, 1.0 + frac],
            color: [0.1, 0.9, 0.3, 0.6],
        });
    }
    // Fifty layers deep: each translucent layer re-rounds to 8 bits on both
    // sides, so per-layer encode differences can add up.
    let deep = random_quads(&mut rng, 400, screen[0], screen[1]);
    let scissor = PixelRect::new(17, 9, 101, 53);
    for (case, quads, scissored) in [
        ("quad", &over, false),
        ("quad-scissor", &over, true),
        ("quad-deep", &deep, false),
    ] {
        let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
        let mut blend = QuadPipeline::new(&gpu.device, FORMAT);
        replace.upload(&gpu.device, &gpu.queue, screen, &base);
        blend.upload(&gpu.device, &gpu.queue, screen, quads);
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            let mut pass = load_pass(encoder, view);
            if scissored {
                pass.set_scissor_rect(scissor.x, scissor.y, scissor.w, scissor.h);
            }
            replace.draw(&mut pass);
            blend.draw(&mut pass);
        });
        let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
        let clip = scissored.then_some(scissor);
        draw_quads(&mut fb.target(), screen, &base, QuadBlend::Replace, clip);
        draw_quads(&mut fb.target(), screen, quads, QuadBlend::Blend, clip);
        if case == "quad-deep" {
            let stats = compare(case, w, h, &gpu_frame, &fb);
            assert!(stats.max <= LAYERED_TOLERANCE, "{case}: max {}", stats.max);
        } else {
            assert_parity(case, w, h, &gpu_frame, &fb);
        }
    }
}

#[test]
fn a_window_target_matches_an_offset_viewport() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-window") else {
        return;
    };
    let scene = [300u32, 200u32];
    let window = [113u32, 57, 64, 40];
    let screen = [scene[0] as f32, scene[1] as f32];
    let mut rng = Rng(2);
    let quads: Vec<_> = (0..40)
        .map(|_| QuadInstance {
            pos: [rng.coord(100.0, 180.0), rng.coord(50.0, 100.0)],
            size: [rng.range(0.5, 20.0), rng.range(0.5, 12.0)],
            color: rng.color(),
        })
        .collect();
    let (w, h) = (window[2], window[3]);
    let init = background(w, h, 12);
    let mut blend = QuadPipeline::new(&gpu.device, FORMAT);
    blend.upload(&gpu.device, &gpu.queue, screen, &quads);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        let mut pass = load_pass(encoder, view);
        pass.set_viewport(
            -(window[0] as f32),
            -(window[1] as f32),
            screen[0],
            screen[1],
            0.0,
            1.0,
        );
        blend.draw(&mut pass);
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    draw_quads(
        &mut fb.window_target(window, scene),
        screen,
        &quads,
        QuadBlend::Blend,
        None,
    );
    assert_parity("quad-window", w, h, &gpu_frame, &fb);
}

/// A mask raster: mostly empty or full coverage, as text is, with every
/// partial value in between.
fn mask_raster(rng: &mut Rng, w: u32, h: u32) -> Vec<u8> {
    (0..w * h)
        .map(|_| match rng.next() % 10 {
            0..=3 => 0,
            4..=6 => 255,
            _ => rng.byte(),
        })
        .collect()
}

/// A colour raster, straight or premultiplied as Swash returns PNG and
/// COLR sources.
fn color_raster(rng: &mut Rng, w: u32, h: u32, premultiplied: bool) -> Vec<u8> {
    (0..w * h)
        .flat_map(|_| {
            let a = match rng.next() % 4 {
                0 => 0,
                1 => 255,
                _ => rng.byte(),
            };
            let mut c = [rng.byte(), rng.byte(), rng.byte()];
            if premultiplied {
                c = c.map(|v| (u16::from(v) * u16::from(a) / 255) as u8);
            }
            [c[0], c[1], c[2], a]
        })
        .collect()
}

#[test]
fn glyphs_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-glyphs") else {
        return;
    };
    let (w, h) = (200u32, 120u32);
    let screen = [w as f32, h as f32];
    let mut rng = Rng(3);
    let mut glyphs = GlyphPipeline::new(&gpu.device, FORMAT);
    let mut mask_atlas = CpuTexture::new(1024, 512, CpuTextureFormat::R8Unorm).unwrap();
    let mut color_atlas = CpuTexture::new(1024, 512, CpuTextureFormat::Rgba8UnormSrgb).unwrap();
    // Rasters: text-sized masks, a one-pixel mask, wide strokes, emoji.
    let shapes: [(bool, bool, u32, u32); 8] = [
        (false, false, 7, 13),
        (false, false, 9, 17),
        (false, false, 1, 1),
        (false, false, 12, 3),
        (false, false, 16, 16),
        (true, false, 16, 16),
        (true, true, 20, 18),
        (true, false, 5, 7),
    ];
    let mut slots = Vec::new();
    for (i, &(color, premultiplied, rw, rh)) in shapes.iter().enumerate() {
        let data = if color {
            color_raster(&mut rng, rw, rh, premultiplied)
        } else {
            mask_raster(&mut rng, rw, rh)
        };
        let key = BuiltinKey::new(
            char::from(b'a' + i as u8),
            rw as i32,
            rh as i32,
            Strokes { light: 1, heavy: 2 },
        )
        .unwrap();
        let slot = glyphs
            .ensure_glyph(
                crate::backend::DeviceRef::gpu(&gpu.device, &gpu.queue),
                key,
                || {
                    Some(RasterGlyph {
                        color,
                        width: rw,
                        height: rh,
                        left: 0,
                        top: 0,
                        data: &data,
                    })
                },
            )
            .expect("glyph slot");
        let atlas = if color {
            &mut color_atlas
        } else {
            &mut mask_atlas
        };
        assert!(atlas.write(slot.atlas_x as u32, slot.atlas_y as u32, rw, rh, &data));
        slots.push(slot);
    }
    let instances = |rng: &mut Rng, production: bool| {
        let mut out = Vec::new();
        for i in 0..360 {
            let slot = slots[i % slots.len()];
            let uv_size = [slot.w, slot.h];
            let coord = |rng: &mut Rng, hi: f32| {
                if production {
                    rng.coord_free(-6.0, hi)
                } else {
                    rng.coord(-6.0, hi)
                }
            };
            let whole = |rng: &mut Rng, hi: f32| rng.range(-6.0, hi).floor();
            let (pos, size, uv_size, column) = match i % 6 {
                // On whole pixels at bitmap size: the grid's case.
                0..=2 => (
                    [whole(rng, w as f32), whole(rng, h as f32)],
                    uv_size,
                    uv_size,
                    0.0,
                ),
                // At bitmap size off whole pixels.
                3 => (
                    [coord(rng, w as f32), coord(rng, h as f32)],
                    uv_size,
                    uv_size,
                    0.0,
                ),
                // Scaled: production scales emoji by two on whole pixels and
                // stretches one-texel columns (Arabic joins); the stress set
                // takes any scale anywhere.
                4 if production => (
                    [whole(rng, w as f32), whole(rng, h as f32)],
                    [uv_size[0] * 2.0, uv_size[1] * 2.0],
                    uv_size,
                    0.0,
                ),
                4 => {
                    let k = rng.pick(&[2.0, 1.5, 0.6, 1.3, 0.85]);
                    (
                        [coord(rng, w as f32), coord(rng, h as f32)],
                        [uv_size[0] * k, uv_size[1] * k],
                        uv_size,
                        0.0,
                    )
                }
                _ => (
                    [whole(rng, w as f32), whole(rng, h as f32)],
                    [rng.range(2.0, 12.0).floor(), uv_size[1]],
                    [1.0, uv_size[1]],
                    rng.range(0.0, uv_size[0]).floor(),
                ),
            };
            let mut color = rng.color();
            if slot.kind == 1 && rng.unit() < 0.7 {
                color[3] = 1.0;
            }
            if slot.kind == 0 {
                // A colour glyph's alpha is the brightness it keeps; rgb is
                // the dimming backdrop.
                color[3] = rng.pick(&[1.0, 1.0, 0.4, 0.75]);
            }
            out.push(GlyphInstance {
                pos,
                size,
                uv: [slot.atlas_x + column, slot.atlas_y],
                uv_size,
                color,
                kind: slot.kind,
                _pad: [0],
            });
        }
        out
    };
    let atlases = GlyphAtlases {
        color: color_atlas.view(),
        mask: mask_atlas.view(),
    };
    let init = background(w, h, 13);
    let cpu_frame = |instances: &[GlyphInstance], clips: &[GlyphClip], nudge: [f32; 2]| {
        let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
        for clip in clips {
            let range = clip.start as usize..(clip.start + clip.count) as usize;
            let moved: Vec<_> = instances[range]
                .iter()
                .map(|g| GlyphInstance {
                    pos: [g.pos[0] + nudge[0], g.pos[1] + nudge[1]],
                    ..*g
                })
                .collect();
            let scissor = pane_scissor(clip.rect, [w, h]);
            assert!(scissor.is_some());
            draw_glyphs(&mut fb.target(), screen, &moved, atlases, scissor);
        }
        fb
    };
    for (case, production) in [("glyph", true), ("glyph-ties", false)] {
        let instances = instances(&mut rng, production);
        let half = instances.len() as u32 / 2;
        let clips = [
            GlyphClip {
                rect: [10.5, 5.25, 100.7, 60.0],
                start: 0,
                count: half,
            },
            GlyphClip {
                rect: [0.0, 0.0, w as f32, h as f32],
                start: half,
                count: instances.len() as u32 - half,
            },
        ];
        assert!(glyphs.upload(
            crate::backend::DeviceRef::gpu(&gpu.device, &gpu.queue),
            screen,
            &instances
        ));
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            let mut pass = load_pass(encoder, view);
            glyphs.draw(&mut pass, &clips, [w, h]);
        });
        let fb = cpu_frame(&instances, &clips, [0.0; 2]);
        if production {
            assert_parity(case, w, h, &gpu_frame, &fb);
        } else {
            let ties = tie_mask(&fb, |nudge| cpu_frame(&instances, &clips, nudge));
            assert_parity_but_ties(case, w, h, &gpu_frame, &fb, &ties);
        }
    }
}

fn image(rng: &mut Rng, w: u32, h: u32, translucent: bool) -> ImageData {
    let rgba = (0..w * h)
        .flat_map(|i| {
            let a = if translucent {
                rng.pick(&[0u8, 255, 128, 40, 200, 255])
            } else {
                255
            };
            let checker = ((i % w) / 2 + (i / w) / 2).is_multiple_of(2);
            let c = if checker {
                [rng.byte(), rng.byte(), rng.byte()]
            } else {
                [(i % w * 255 / w) as u8, 30, (i / w * 255 / h) as u8]
            };
            [c[0], c[1], c[2], a]
        })
        .collect();
    ImageData::new(w, h, rgba).unwrap()
}

fn image_case(gpu: &Gpu, case: &str, w: u32, h: u32, items: &[ImageItem]) {
    let screen = [w as f32, h as f32];
    let init = background(w, h, 14);
    let mut pipeline = ImagePipeline::new(&gpu.device, FORMAT).expect("image pipeline");
    pipeline.upload(&gpu.device, &gpu.queue, screen, items);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        let mut pass = load_pass(encoder, view);
        pipeline.draw(&mut pass);
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    for item in items {
        let Some(inst) = item.instance() else {
            continue;
        };
        let img = &item.image;
        let texture = CpuTextureView::new(
            img.width,
            img.height,
            CpuTextureFormat::Rgba8UnormSrgb,
            &img.rgba[..],
        )
        .unwrap();
        let address = if item.repeat {
            AddressMode::Repeat
        } else {
            AddressMode::ClampToEdge
        };
        draw_image(&mut fb.target(), screen, &inst, texture, address, None);
    }
    assert_parity(case, w, h, &gpu_frame, &fb);
}

#[test]
fn images_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-images") else {
        return;
    };
    let (w, h) = (192u32, 128u32);
    let mut rng = Rng(4);
    let small = image(&mut rng, 7, 5, true);
    let large = image(&mut rng, 64, 48, true);
    let opaque = image(&mut rng, 33, 21, false);
    let items = vec![
        // Upscaled about 8.7x from a fractional origin.
        ImageItem::full(3.3, 4.6, 61.4, 37.9, small.clone()),
        // Downscaled 0.37x, translucent.
        ImageItem::full(80.25, 6.5, 23.5, 17.25, large.clone()),
        // Opaque at 1:1 on whole pixels, then at 1:1 off them.
        ImageItem::full(120.0, 50.0, 33.0, 21.0, opaque.clone()),
        ImageItem::full(100.5, 90.25, 33.0, 21.0, opaque.clone()),
        // A cropped placement clipped to a pane.
        ImageItem::placement(
            [10.0, 60.0, 90.0, 60.0],
            large.clone(),
            Some(ImageSourceRect {
                x: 8.0,
                y: 4.0,
                width: 30.0,
                height: 20.0,
            }),
            Some(ImageSourceCrop {
                top: 0.1,
                bottom: 0.9,
            }),
            [20.5, 64.0, 60.0, 50.0],
        ),
        // A fractional tile crop, which filters across tile edges.
        ImageItem::placement(
            [130.0, 4.0, 50.0, 40.0],
            small.clone(),
            Some(ImageSourceRect {
                x: 1.5,
                y: 0.0,
                width: 3.25,
                height: 5.0,
            }),
            None,
            [0.0, 0.0, w as f32, h as f32],
        ),
    ];
    image_case(&gpu, "image", w, h, &items);
    let tiles = vec![ImageItem::tiled(w as f32, h as f32, small)];
    image_case(&gpu, "image-tiled", w, h, &tiles);
}

fn random_surfaces(
    rng: &mut Rng,
    n: usize,
    production: bool,
    w: f32,
    h: f32,
) -> Vec<RoundedSurface> {
    let mut surfaces = Vec::new();
    for i in 0..n {
        let kind = match i % 8 {
            5 => 1.0,
            6 => 2.0,
            7 => 3.0,
            4 => 4.0,
            _ => 0.0,
        };
        let size = if kind > 0.0 && kind < 4.0 {
            let s = rng.range(9.0, 40.0);
            [s, s * rng.range(0.8, 1.25)]
        } else {
            [rng.range(3.0, 120.0), rng.range(3.0, 70.0)]
        };
        let coord = |rng: &mut Rng, hi: f32| {
            if production {
                rng.coord_free(-10.0, hi)
            } else {
                rng.coord(-10.0, hi)
            }
        };
        surfaces.push(RoundedSurface {
            pos: [coord(rng, w - 20.0), coord(rng, h - 20.0)],
            size,
            fill: rng.color(),
            stroke: rng.color(),
            params: [
                rng.pick(&[0.0, 2.0, 5.5, 12.0, 100.0]),
                rng.pick(&[0.0, 0.0, 1.0, 2.5]),
                kind,
                if kind == 4.0 {
                    rng.unit()
                } else {
                    rng.pick(&[0.0, 1.0])
                },
            ],
        });
    }
    surfaces
}

#[test]
fn rounded_surfaces_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-rounded") else {
        return;
    };
    let (w, h) = (240u32, 160u32);
    let screen = [w as f32, h as f32];
    let mut rng = Rng(5);
    // About two layers deep on average, as menus over panels with shadows.
    let production = random_surfaces(&mut rng, 24, true, screen[0], screen[1]);
    // Twice that, still clear of exact ties: per-layer rounding adds up.
    let deep = random_surfaces(&mut rng, 48, true, screen[0], screen[1]);
    let stress = random_surfaces(&mut rng, 24, false, screen[0], screen[1]);
    let init = background(w, h, 15);
    let scissor = PixelRect::new(31, 17, 150, 101);
    for (case, surfaces, scissored) in [
        ("rounded", &production, false),
        ("rounded-scissor", &production, true),
        ("rounded-deep", &deep, false),
        ("rounded-ties", &stress, false),
    ] {
        let mut pipeline =
            RoundedSurfacePipeline::with_capacity(&gpu.device, FORMAT, surfaces.len());
        pipeline.upload(&gpu.device, &gpu.queue, screen, surfaces);
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            let mut pass = load_pass(encoder, view);
            if scissored {
                pass.set_scissor_rect(scissor.x, scissor.y, scissor.w, scissor.h);
            }
            pipeline.draw_range(&mut pass, 0..surfaces.len() as u32);
        });
        let cpu_frame = |nudge: [f32; 2]| {
            let moved: Vec<_> = surfaces
                .iter()
                .map(|s| RoundedSurface {
                    pos: [s.pos[0] + nudge[0], s.pos[1] + nudge[1]],
                    ..*s
                })
                .collect();
            let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
            draw_rounded_surfaces(
                &mut fb.target(),
                screen,
                &moved,
                scissored.then_some(scissor),
            );
            fb
        };
        let fb = cpu_frame([0.0; 2]);
        if case == "rounded-ties" {
            let ties = tie_mask(&fb, cpu_frame);
            assert_parity_but_ties(case, w, h, &gpu_frame, &fb, &ties);
        } else if case == "rounded-deep" {
            let stats = compare(case, w, h, &gpu_frame, &fb);
            assert!(stats.max <= LAYERED_TOLERANCE, "{case}: max {}", stats.max);
        } else {
            assert_parity(case, w, h, &gpu_frame, &fb);
        }
    }
}

#[test]
fn outlines_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-outlines") else {
        return;
    };
    let (w, h) = (240u32, 160u32);
    let screen = [w as f32, h as f32];
    let mut rng = Rng(6);
    let outlines = |rng: &mut Rng, production: bool| {
        let mut out = Vec::new();
        for _ in 0..40 {
            let dashed = rng.unit() < 0.35;
            let coord = |rng: &mut Rng, hi: f32| {
                if production {
                    rng.coord_free(-10.0, hi)
                } else {
                    rng.coord(-10.0, hi)
                }
            };
            out.push(OutlineInstance {
                pos: [coord(rng, w as f32 - 30.0), coord(rng, h as f32 - 30.0)],
                size: [rng.range(8.0, 150.0), rng.range(8.0, 100.0)],
                color: rng.color(),
                border_width: rng.pick(&[0.0, 1.0, 1.0, 2.0, 3.5]),
                corner_radius: rng.pick(&[0.0, 6.0, 14.5]),
                corner_mask: rng.pick(&[0, 0b1111, 0b0110, 0b1000]),
                dash: if dashed { rng.pick(&[4.0, 6.0]) } else { 0.0 },
                gap: if dashed { rng.pick(&[3.0, 2.5]) } else { 0.0 },
            });
        }
        out
    };
    let init = background(w, h, 16);
    for (case, production) in [("outline", true), ("outline-ties", false)] {
        let outlines = outlines(&mut rng, production);
        let mut pipeline = OutlinePipeline::new(&gpu.device, FORMAT);
        pipeline.upload(&gpu.device, &gpu.queue, screen, &outlines);
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            let mut pass = load_pass(encoder, view);
            pipeline.draw(&mut pass);
        });
        let cpu_frame = |nudge: [f32; 2]| {
            let moved: Vec<_> = outlines
                .iter()
                .map(|o| OutlineInstance {
                    pos: [o.pos[0] + nudge[0], o.pos[1] + nudge[1]],
                    ..*o
                })
                .collect();
            let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
            draw_outlines(&mut fb.target(), screen, &moved, None);
            fb
        };
        let fb = cpu_frame([0.0; 2]);
        if production {
            assert_parity(case, w, h, &gpu_frame, &fb);
        } else {
            let ties = tie_mask(&fb, cpu_frame);
            assert_parity_but_ties(case, w, h, &gpu_frame, &fb, &ties);
        }
    }
}

#[test]
fn the_starfield_matches_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-starfield") else {
        return;
    };
    let seeds = StarSeed::all();
    let mut scratch = Vec::new();
    for (w, h, time) in [(320u32, 200u32, 0.0f32), (320, 200, 7.5), (480, 270, 41.3)] {
        let resolution = [w as f32, h as f32];
        let mut pipeline = StarfieldPipeline::new(&gpu.device, FORMAT);
        pipeline.upload(&gpu.queue, resolution, time);
        let init = background(w, h, 17);
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            let mut pass = load_pass(encoder, view);
            pipeline.draw(&mut pass);
        });
        let (stars, count) = frame_stars(&seeds, resolution, time);
        let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
        draw_starfield(
            &mut fb.target(),
            resolution,
            &stars[..count],
            None,
            &mut scratch,
        );
        assert_parity(&format!("starfield-{time}"), w, h, &gpu_frame, &fb);
    }
}

#[test]
fn presentation_matches_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-present") else {
        return;
    };
    let (w, h) = (96u32, 64u32);
    let screen = [w as f32, h as f32];
    let mut rng = Rng(7);
    let mut present = PresentationPipeline::new(&gpu.device, FORMAT, GraphicsBudget::default());
    assert!(present.ensure_target(&gpu.device, w, h));
    let init = background(w, h, 18);
    // Alphas a window's opacity setting produces, then any alpha at all.
    for (case, low) in [("present", 0.25f32), ("present-low-alpha", 0.0)] {
        // A translucent base, then small premultiplied patches.
        let mut quads = vec![QuadInstance {
            pos: [0.0, 0.0],
            size: screen,
            color: [0.3, 0.6, 0.9, 0.37],
        }];
        for _ in 0..600 {
            let a = rng.pick(&[0.0, 1.0, -1.0, -1.0]);
            quads.push(QuadInstance {
                pos: [
                    rng.range(0.0, w as f32).floor(),
                    rng.range(0.0, h as f32).floor(),
                ],
                size: [rng.range(1.0, 6.0).floor(), rng.range(1.0, 6.0).floor()],
                color: [
                    rng.unit(),
                    rng.unit(),
                    rng.unit(),
                    if a < 0.0 { rng.range(low, 1.0) } else { a },
                ],
            });
        }
        let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
        replace.upload(&gpu.device, &gpu.queue, screen, &quads);
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            {
                let scene = present.scene_view().expect("scene target");
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("cpu-parity-scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: scene,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                replace.draw(&mut pass);
            }
            let mut pass = load_pass(encoder, view);
            present.draw(&mut pass);
        });
        let mut scene = CpuFramebuffer::new(w, h).unwrap();
        draw_quads(
            &mut scene.target(),
            screen,
            &quads,
            QuadBlend::Replace,
            None,
        );
        let mut out = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
        assert!(present_straight(&scene, &mut out));
        if low > 0.0 {
            assert_parity(case, w, h, &gpu_frame, &out);
            continue;
        }
        // Dividing by alpha magnifies a one-code difference in the stored
        // premultiplied scene by about 1 / alpha: only low alphas may exceed.
        compare(case, w, h, &gpu_frame, &out);
        let cpu = out.to_rgba8();
        for (g, c) in gpu_frame
            .as_chunks::<4>()
            .0
            .iter()
            .zip(cpu.as_chunks::<4>().0)
        {
            if g.iter().zip(c).any(|(g, c)| g.abs_diff(*c) > TOLERANCE) {
                assert!(c[3] < 64, "{case}: {g:?} against {c:?} at alpha {}", c[3]);
            }
        }
    }
}

#[test]
fn the_cursor_patch_combine_matches_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-cursor-patch") else {
        return;
    };
    let (w, h) = (48u32, 40u32);
    let mut rng = Rng(8);
    let off = background(w, h, 19);
    let mut on = off.clone();
    for (i, byte) in on.iter_mut().enumerate() {
        // Change some pixels in one channel, some in all, some not at all.
        let px = i / 4;
        if px % 3 == 0 && (i % 4 == 0 || px % 5 == 0) {
            *byte = byte.wrapping_add(1 + rng.byte() % 3);
        }
    }
    let device = &gpu.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("cpu-parity-combine"),
        source: wgpu::ShaderSource::Wgsl(crate::cursor_patch::COMBINE_SHADER.into()),
    });
    let entry = |binding| wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    };
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: None,
        entries: &[entry(0), entry(1)],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: None,
        bind_group_layouts: &[Some(&bgl)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("cpu-parity-combine"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: FORMAT,
                blend: None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let on_tex = gpu.target(w, h);
    let off_tex = gpu.target(w, h);
    gpu.write(&on_tex, w, h, &on);
    gpu.write(&off_tex, w, h, &off);
    let on_view = on_tex.create_view(&Default::default());
    let off_view = off_tex.create_view(&Default::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&on_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&off_view),
            },
        ],
    });
    let init = background(w, h, 20);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        let mut pass = load_pass(encoder, view);
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..3, 0..1);
    });
    let on_fb = CpuFramebuffer::from_rgba8(w, h, &on).unwrap();
    let off_fb = CpuFramebuffer::from_rgba8(w, h, &off).unwrap();
    let mut out = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    assert!(combine_patch(&on_fb, &off_fb, &mut out));
    assert_parity("cursor-patch", w, h, &gpu_frame, &out);
}

/// Stores and blends of one colour per pixel, sweeping the transfer curve:
/// how the GPU's sRGB encode and blend round compared with the CPU's.
#[test]
fn srgb_stores_and_blends_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-srgb") else {
        return;
    };
    let (w, h) = (64u32, 64u32);
    let screen = [w as f32, h as f32];
    // Replace: every pixel a different straight colour, alpha 1.
    let stores: Vec<_> = (0..w * h)
        .map(|i| QuadInstance {
            pos: [(i % w) as f32, (i / w) as f32],
            size: [1.0, 1.0],
            color: [
                i as f32 / (w * h - 1) as f32,
                (i * 7 % (w * h)) as f32 / (w * h - 1) as f32,
                ((w * h - 1 - i) as f32 / (w * h - 1) as f32).powi(3),
                1.0,
            ],
        })
        .collect();
    let init = background(w, h, 21);
    let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
    replace.upload(&gpu.device, &gpu.queue, screen, &stores);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        let mut pass = load_pass(encoder, view);
        replace.draw(&mut pass);
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    draw_quads(&mut fb.target(), screen, &stores, QuadBlend::Replace, None);
    assert_parity("srgb-store", w, h, &gpu_frame, &fb);
    // Blend: one translucent colour per row over every destination byte.
    let mut dst = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = ((x * 4 + y % 4) % 256) as u8;
            dst.extend_from_slice(&[v, v / 2, 255 - v, 255]);
        }
    }
    let blends: Vec<_> = (0..h)
        .map(|y| QuadInstance {
            pos: [0.0, y as f32],
            size: [w as f32, 1.0],
            color: [
                (y % 8) as f32 / 7.0,
                1.0 - (y % 5) as f32 / 4.0,
                0.5,
                [0.05, 0.3, 0.5, 0.77, 0.95][(y / 8 % 5) as usize],
            ],
        })
        .collect();
    let mut blend = QuadPipeline::new(&gpu.device, FORMAT);
    blend.upload(&gpu.device, &gpu.queue, screen, &blends);
    let gpu_frame = gpu.render(w, h, &dst, |encoder, view| {
        let mut pass = load_pass(encoder, view);
        blend.draw(&mut pass);
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &dst).unwrap();
    draw_quads(&mut fb.target(), screen, &blends, QuadBlend::Blend, None);
    assert_parity("srgb-blend", w, h, &gpu_frame, &fb);
}

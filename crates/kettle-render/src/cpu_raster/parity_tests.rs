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
use super::{
    AddressMode, CpuFramebuffer, CpuTexture, CpuTextureFormat, CpuTextureView, PixelRect, Viewport,
};
use crate::backend::DeviceRef;
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
    lavapipe: bool,
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
    let info = adapter.get_info();
    eprintln!("cpu-parity adapter: {} ({:?})", info.name, info.backend);
    let lavapipe = info.backend == wgpu::Backend::Vulkan && info.name.starts_with("llvmpipe");
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
        lavapipe,
    })
}

/// Some precision choices are specific to the lavapipe reference driver.
fn lavapipe(label: &str) -> Option<Gpu> {
    let gpu = gpu(label)?;
    if !gpu.lavapipe {
        eprintln!("{label}: skipped; needs lavapipe");
        return None;
    }
    Some(gpu)
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

/// Every listed pixel lies on one known discontinuity in the fixture.
fn assert_exact_tie_choices(
    case: &str,
    gpu: &[u8],
    base: &CpuFramebuffer,
    either_side: [&CpuFramebuffer; 2],
    ties: &[(u32, u32)],
) {
    let (w, h) = (base.width(), base.height());
    compare(case, w, h, gpu, base);
    let alternates = either_side.map(CpuFramebuffer::pixels);
    let mut checked = 0;
    for (i, (g, &c)) in gpu.as_chunks::<4>().0.iter().zip(base.pixels()).enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        let c = super::unpack(c);
        if ties.contains(&(x, y)) {
            checked += 1;
            let choices = alternates.map(|pixels| super::unpack(pixels[i]));
            assert_ne!(
                choices[0], choices[1],
                "{case} ({x}, {y}): no discontinuity"
            );
            assert!(
                choices.contains(g),
                "{case} ({x}, {y}): GPU {g:?} is not one of {choices:?}"
            );
        } else {
            assert!(
                g.iter().zip(c).all(|(g, c)| g.abs_diff(c) <= TOLERANCE),
                "{case} ({x}, {y}): GPU {g:?}, CPU {c:?} outside a tie"
            );
        }
    }
    assert_eq!(checked, ties.len());
}

#[test]
#[should_panic(expected = "is not one of")]
fn a_tie_rejects_a_color_from_neither_side() {
    let [lo, hi] = [[255, 0, 0, 255], [0, 0, 255, 255]]
        .map(|pixel| CpuFramebuffer::from_rgba8(1, 1, &pixel).unwrap());
    assert_exact_tie_choices("tie", &[255, 0, 255, 255], &lo, [&lo, &hi], &[(0, 0)]);
}

#[test]
#[should_panic(expected = "outside a tie")]
fn tie_comparison_rejects_three_codes_outside_the_tie() {
    let fb = CpuFramebuffer::from_rgba8(1, 1, &[10, 20, 30, 255]).unwrap();
    assert_exact_tie_choices("tie", &[10, 23, 30, 255], &fb, [&fb, &fb], &[]);
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

#[test]
fn quad_edges_match_gpu_subpixel_snap() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = lavapipe("cpu-parity-subpixel") else {
        return;
    };
    let (w, h) = (16, 16);
    let screen = [w as f32, h as f32];
    let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
    let quads: Vec<_> = [3.0 / 2048.0, 1.0 / 512.0, 5.0 / 2048.0]
        .into_iter()
        .enumerate()
        .map(|(row, offset)| QuadInstance {
            pos: [3.5 + offset, row as f32 * 4.0 + 3.5 + offset],
            size: [4.0, 2.0],
            color: [1.0; 4],
        })
        .collect();
    let init = vec![0; (w * h * 4) as usize];
    replace.upload(&gpu.device, &gpu.queue, screen, &quads);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        replace.draw(&mut load_pass(encoder, view));
    });
    for (y, alpha) in [(3, 255), (7, 255), (11, 0)] {
        assert_eq!(gpu_frame[((y * w + 3) * 4 + 3) as usize], alpha);
    }
    let mut fb = CpuFramebuffer::new(w, h).unwrap();
    draw_quads(&mut fb.target(), screen, &quads, QuadBlend::Replace, None);
    assert_parity("quad-subpixel", w, h, &gpu_frame, &fb);
}

#[test]
fn an_inset_viewport_clips_scene_coordinates() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = gpu("cpu-parity-clip-volume") else {
        return;
    };
    let (w, h) = (32, 32);
    let screen = [32.0; 2];
    let viewport = Viewport {
        x: 8.0,
        y: 8.0,
        w: 16.0,
        h: 16.0,
    };
    let quads = [
        QuadInstance {
            pos: [-8.0, -8.0],
            size: [20.0; 2],
            color: [1.0; 4],
        },
        QuadInstance {
            pos: [20.0, 20.0],
            size: [20.0; 2],
            color: [1.0; 4],
        },
    ];
    let init = vec![0; (w * h * 4) as usize];
    let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
    replace.upload(&gpu.device, &gpu.queue, screen, &quads);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        let mut pass = load_pass(encoder, view);
        pass.set_viewport(viewport.x, viewport.y, viewport.w, viewport.h, 0.0, 1.0);
        replace.draw(&mut pass);
    });
    let mut fb = CpuFramebuffer::new(w, h).unwrap();
    let mut target = fb.target();
    target.set_viewport(viewport);
    draw_quads(&mut target, screen, &quads, QuadBlend::Replace, None);
    assert_parity("quad-clip-volume", w, h, &gpu_frame, &fb);
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
            .ensure_glyph(DeviceRef::gpu(&gpu.device, &gpu.queue), key, || {
                Some(RasterGlyph {
                    color,
                    width: rw,
                    height: rh,
                    left: 0,
                    top: 0,
                    data: &data,
                })
            })
            .expect("glyph slot");
        let atlas = if color {
            &mut color_atlas
        } else {
            &mut mask_atlas
        };
        assert!(atlas.write(slot.atlas_x as u32, slot.atlas_y as u32, rw, rh, &data));
        slots.push(slot);
    }
    let instances = |rng: &mut Rng| {
        let mut out = Vec::new();
        for i in 0..360 {
            let slot = slots[i % slots.len()];
            let uv_size = [slot.w, slot.h];
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
                    [
                        rng.coord_free(-6.0, w as f32),
                        rng.coord_free(-6.0, h as f32),
                    ],
                    uv_size,
                    uv_size,
                    0.0,
                ),
                // Whole-pixel emoji enlargement.
                4 => (
                    [whole(rng, w as f32), whole(rng, h as f32)],
                    [uv_size[0] * 2.0, uv_size[1] * 2.0],
                    uv_size,
                    0.0,
                ),
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
    let instances = instances(&mut rng);
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
    assert!(glyphs.upload(DeviceRef::gpu(&gpu.device, &gpu.queue), screen, &instances));
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        glyphs.draw(&mut load_pass(encoder, view), &clips, [w, h]);
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    for clip in clips {
        let range = clip.start as usize..(clip.start + clip.count) as usize;
        let scissor = pane_scissor(clip.rect, [w, h]).unwrap();
        draw_glyphs(
            &mut fb.target(),
            screen,
            &instances[range],
            atlases,
            Some(scissor),
        );
    }
    assert_parity("glyph", w, h, &gpu_frame, &fb);

    // Two exact, opaque texels meet at x = 4.5. Moving only the U
    // coordinate selects the other texel without moving the geometry.
    let pixels = [255, 0, 0, 255, 0, 0, 255, 255];
    let key = BuiltinKey::new('z', 2, 1, Strokes { light: 1, heavy: 2 }).unwrap();
    let slot = glyphs
        .ensure_glyph(DeviceRef::gpu(&gpu.device, &gpu.queue), key, || {
            Some(RasterGlyph {
                color: true,
                width: 2,
                height: 1,
                left: 0,
                top: 0,
                data: &pixels,
            })
        })
        .unwrap();
    assert!(color_atlas.write(slot.atlas_x as u32, slot.atlas_y as u32, 2, 1, &pixels));
    let atlases = GlyphAtlases {
        color: color_atlas.view(),
        mask: mask_atlas.view(),
    };
    let (w, h) = (16, 16);
    let screen = [w as f32, h as f32];
    let glyph = GlyphInstance {
        pos: [2.5, 2.0],
        size: [4.0, 4.0],
        uv: [slot.atlas_x, slot.atlas_y],
        uv_size: [slot.w, slot.h],
        color: [1.0; 4],
        kind: slot.kind,
        _pad: [0],
    };
    assert!(glyphs.upload(DeviceRef::gpu(&gpu.device, &gpu.queue), screen, &[glyph]));
    let gpu_frame = gpu.render(w, h, &[0; 16 * 16 * 4], |encoder, view| {
        let clips = [GlyphClip {
            rect: [0.0, 0.0, screen[0], screen[1]],
            start: 0,
            count: 1,
        }];
        glyphs.draw(&mut load_pass(encoder, view), &clips, [w, h]);
    });
    let [at, lo, hi] = [0.0, -0.001, 0.001].map(|offset| {
        let mut fb = CpuFramebuffer::new(w, h).unwrap();
        let glyph = GlyphInstance {
            uv: [glyph.uv[0] + offset, glyph.uv[1]],
            ..glyph
        };
        draw_glyphs(&mut fb.target(), screen, &[glyph], atlases, None);
        fb
    });
    assert_exact_tie_choices(
        "glyph-ties",
        &gpu_frame,
        &at,
        [&lo, &hi],
        &[(4, 2), (4, 3), (4, 4), (4, 5)],
    );
}

#[test]
fn whole_pixel_emoji_fallback_downscales_match_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = lavapipe("cpu-parity-emoji-fallback") else {
        return;
    };
    let (w, h) = (1600, 1000);
    let screen = [w as f32, h as f32];
    let mut glyphs = GlyphPipeline::new(&gpu.device, FORMAT);
    let mut color = CpuTexture::new(1024, 512, CpuTextureFormat::Rgba8UnormSrgb).unwrap();
    let mask = CpuTexture::new(1, 1, CpuTextureFormat::R8Unorm).unwrap();
    let mut instances = Vec::new();
    for (tag, bitmap, fitted) in [(b'a', 32, 16), (b'b', 24, 16), (b'c', 32, 24)] {
        let data: Vec<_> = (0..bitmap * bitmap)
            .flat_map(|i| {
                let (x, y) = (i % bitmap, i / bitmap);
                [
                    if x % 2 == 0 { 255 } else { 0 },
                    0,
                    if y % 2 == 0 { 255 } else { 0 },
                    255,
                ]
            })
            .collect();
        let key = BuiltinKey::new(
            char::from(tag),
            bitmap as i32,
            bitmap as i32,
            Strokes { light: 1, heavy: 2 },
        )
        .unwrap();
        let slot = glyphs
            .ensure_glyph(DeviceRef::gpu(&gpu.device, &gpu.queue), key, || {
                Some(RasterGlyph {
                    color: true,
                    width: bitmap,
                    height: bitmap,
                    left: 0,
                    top: 0,
                    data: &data,
                })
            })
            .unwrap();
        assert!(color.write(
            slot.atlas_x as u32,
            slot.atlas_y as u32,
            bitmap,
            bitmap,
            &data
        ));
        // The second and third origins place sample centres on the
        // shared diagonal where the two UV planes round differently.
        for (x, y) in [(37.0, 11.0), (56.0, 372.0), (507.0, 155.0), (1203.0, 611.0)] {
            let fitted = fitted as f32;
            let x = x + f32::from(tag - b'a') * 40.0;
            let inset = crate::fonts::emoji_inset(fitted);
            let cell = crate::fonts::emoji_box((x, y - inset, fitted, fitted + 2.0 * inset));
            let (x, y, width, height) = crate::fonts::fit_emoji(cell, (slot.w, slot.h));
            assert_eq!((width, height), (fitted, fitted));
            assert!(x.fract() == 0.0 && y.fract() == 0.0);
            instances.push(GlyphInstance {
                pos: [x, y],
                size: [width, height],
                uv: [slot.atlas_x, slot.atlas_y],
                uv_size: [slot.w, slot.h],
                color: [1.0; 4],
                kind: slot.kind,
                _pad: [0],
            });
        }
    }
    let clips = [GlyphClip {
        rect: [0.0, 0.0, screen[0], screen[1]],
        start: 0,
        count: instances.len() as u32,
    }];
    let init = vec![0; (w * h * 4) as usize];
    assert!(glyphs.upload(DeviceRef::gpu(&gpu.device, &gpu.queue), screen, &instances));
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        glyphs.draw(&mut load_pass(encoder, view), &clips, [w, h]);
    });
    let mut fb = CpuFramebuffer::new(w, h).unwrap();
    draw_glyphs(
        &mut fb.target(),
        screen,
        &instances,
        GlyphAtlases {
            color: color.view(),
            mask: mask.view(),
        },
        None,
    );
    assert_parity("glyph-emoji-fallback", w, h, &gpu_frame, &fb);
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
    let init = background(w, h, 15);
    let scissor = PixelRect::new(31, 17, 150, 101);
    for (case, surfaces, scissored) in [
        ("rounded", &production, false),
        ("rounded-scissor", &production, true),
        ("rounded-deep", &deep, false),
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
        let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
        draw_rounded_surfaces(
            &mut fb.target(),
            screen,
            surfaces,
            scissored.then_some(scissor),
        );
        if case == "rounded-deep" {
            let stats = compare(case, w, h, &gpu_frame, &fb);
            assert!(stats.max <= LAYERED_TOLERANCE, "{case}: max {}", stats.max);
        } else {
            assert_parity(case, w, h, &gpu_frame, &fb);
        }
    }

    // Preserve centres exactly on half-pixel edges while isolating each
    // surface so this coverage fixture cannot hide blend-stack drift.
    let (w, h) = (768, 320);
    let screen = [w as f32, h as f32];
    let mut surfaces = random_surfaces(&mut rng, 24, false, 120.0, 70.0);
    for (i, surface) in surfaces.iter_mut().enumerate() {
        surface.pos = [(i % 6) as f32 * 128.0 + 0.5, (i / 6) as f32 * 80.0 + 0.5];
    }
    let mut pipeline = RoundedSurfacePipeline::with_capacity(&gpu.device, FORMAT, surfaces.len());
    pipeline.upload(&gpu.device, &gpu.queue, screen, &surfaces);
    let init = background(w, h, 15);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        pipeline.draw_range(&mut load_pass(encoder, view), 0..surfaces.len() as u32);
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    draw_rounded_surfaces(&mut fb.target(), screen, &surfaces, None);
    assert_parity("rounded-ties", w, h, &gpu_frame, &fb);
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
    let outlines = |rng: &mut Rng| {
        let mut out = Vec::new();
        for _ in 0..40 {
            let dashed = rng.unit() < 0.35;
            out.push(OutlineInstance {
                pos: [
                    rng.coord_free(-10.0, w as f32 - 30.0),
                    rng.coord_free(-10.0, h as f32 - 30.0),
                ],
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
    let outlines = outlines(&mut rng);
    let mut pipeline = OutlinePipeline::new(&gpu.device, FORMAT);
    pipeline.upload(&gpu.device, &gpu.queue, screen, &outlines);
    let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
        pipeline.draw(&mut load_pass(encoder, view));
    });
    let mut fb = CpuFramebuffer::from_rgba8(w, h, &init).unwrap();
    draw_outlines(&mut fb.target(), screen, &outlines, None);
    assert_parity("outline", w, h, &gpu_frame, &fb);

    // A pixel on each horizontal centreline lies on the end of the first
    // dash. Vary only that threshold to select either full ink or discard.
    let (w, h) = (32, 16);
    let screen = [w as f32, h as f32];
    let outline = OutlineInstance {
        pos: [2.5, 2.0],
        size: [16.0, 8.0],
        color: [1.0; 4],
        border_width: 1.0,
        corner_radius: 0.0,
        corner_mask: 0,
        dash: 4.0,
        gap: 3.0,
    };
    pipeline.upload(&gpu.device, &gpu.queue, screen, &[outline]);
    let gpu_frame = gpu.render(w, h, &[0; 32 * 16 * 4], |encoder, view| {
        pipeline.draw(&mut load_pass(encoder, view));
    });
    let [at, lo, hi] = [0.0, -0.001, 0.001].map(|offset| {
        let mut fb = CpuFramebuffer::new(w, h).unwrap();
        let outline = OutlineInstance {
            dash: outline.dash + offset,
            ..outline
        };
        draw_outlines(&mut fb.target(), screen, &[outline], None);
        fb
    });
    assert_exact_tie_choices(
        "outline-ties",
        &gpu_frame,
        &at,
        [&lo, &hi],
        &[(6, 2), (6, 9)],
    );
}

#[test]
fn sdf_fwidth_matches_fine_derivatives() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = lavapipe("cpu-parity-sdf-fwidth") else {
        return;
    };
    let (w, h) = (24, 24);
    let screen = [w as f32, h as f32];
    let pos = [0.375; 2];
    let size = [16.0; 2];
    let surface = RoundedSurface {
        pos,
        size,
        fill: [1.0; 4],
        stroke: [0.0; 4],
        params: [3.0, 0.0, 0.0, 0.0],
    };
    let outline = OutlineInstance {
        pos,
        size,
        color: [1.0; 4],
        border_width: 2.0,
        corner_radius: 3.0,
        corner_mask: 15,
        dash: 0.0,
        gap: 0.0,
    };
    let mut rounded_pipe = RoundedSurfacePipeline::with_capacity(&gpu.device, FORMAT, 1);
    rounded_pipe.upload(&gpu.device, &gpu.queue, screen, &[surface]);
    let mut outline_pipe = OutlinePipeline::new(&gpu.device, FORMAT);
    outline_pipe.upload(&gpu.device, &gpu.queue, screen, &[outline]);
    let init = vec![0; (w * h * 4) as usize];
    for (case, is_outline) in [("rounded-fwidth", false), ("outline-fwidth", true)] {
        let gpu_frame = gpu.render(w, h, &init, |encoder, view| {
            let mut pass = load_pass(encoder, view);
            if is_outline {
                outline_pipe.draw(&mut pass);
            } else {
                rounded_pipe.draw_range(&mut pass, 0..1);
            }
        });
        let mut fine = CpuFramebuffer::new(w, h).unwrap();
        if is_outline {
            draw_outlines(&mut fine.target(), screen, &[outline], None);
        } else {
            draw_rounded_surfaces(&mut fine.target(), screen, &[surface], None);
        }
        assert_parity(case, w, h, &gpu_frame, &fine);

        // The coarse evaluation repeats the top-left lane's row and
        // column differences for all four lanes of each framebuffer quad.
        let inset = if is_outline { 1.0 } else { 0.0 };
        let distance = |x: u32, y: u32| {
            let q = [x, y].map(|v| (v as f32 + 0.5 - 0.375 - 8.0).abs() - 5.0);
            q[0].max(0.0).hypot(q[1].max(0.0)) + q[0].max(q[1]).min(0.0) - (3.0 - inset)
        };
        let mut coarse = CpuFramebuffer::new(w, h).unwrap();
        let table = super::srgb::tables();
        for y in 0..16 {
            for x in 0..16 {
                let (left, top) = (x & !1, y & !1);
                let origin = distance(left, top);
                let fw = (distance(left + 1, top) - origin).abs()
                    + (distance(left, top + 1) - origin).abs();
                let aa = (fw * 0.5).max(0.25);
                let d = distance(x, y);
                let a = if is_outline {
                    1.0 - super::smoothstep((1.0 - aa).max(0.0), 1.0 + aa, d.abs())
                } else {
                    1.0 - super::smoothstep(-aa, aa, d)
                };
                *coarse.target().pixel(x, y) = super::replace(
                    table,
                    super::Premul {
                        r: a,
                        g: a,
                        b: a,
                        a,
                    },
                );
            }
        }
        let coarse_case = format!("{case}-coarse");
        let stats = compare(&coarse_case, w, h, &gpu_frame, &coarse);
        assert!(
            stats.max > TOLERANCE,
            "fixture cannot distinguish coarse fwidth"
        );
        let max_fine_coarse = fine
            .to_rgba8()
            .into_iter()
            .zip(coarse.to_rgba8())
            .map(|(f, c)| f.abs_diff(c))
            .max()
            .unwrap();
        eprintln!("cpu-parity {case}: fine vs coarse CPU max {max_fine_coarse}");
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

fn gpu_presented_scene(gpu: &Gpu, w: u32, h: u32, init: &[u8], replace: &QuadPipeline) -> Vec<u8> {
    let mut present = PresentationPipeline::new(&gpu.device, FORMAT, GraphicsBudget::default());
    assert!(present.ensure_target(&gpu.device, w, h));
    gpu.render(w, h, init, |encoder, view| {
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
                ..Default::default()
            });
            replace.draw(&mut pass);
        }
        present.draw(&mut load_pass(encoder, view));
    })
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
    let init = background(w, h, 18);
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
                if a < 0.0 { rng.range(0.25, 1.0) } else { a },
            ],
        });
    }
    let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
    replace.upload(&gpu.device, &gpu.queue, screen, &quads);
    let gpu_frame = gpu_presented_scene(&gpu, w, h, &init, &replace);
    let mut scene = CpuFramebuffer::new(w, h).unwrap();
    draw_quads(
        &mut scene.target(),
        screen,
        &quads,
        QuadBlend::Replace,
        None,
    );
    let mut out = CpuFramebuffer::new(w, h).unwrap();
    assert!(present_straight(&scene, &mut out));
    assert_parity("present", w, h, &gpu_frame, &out);
}

#[test]
fn presentation_low_opacity_sweep_matches_the_gpu() {
    let _serialized = gpu_test_guard();
    let Some(gpu) = lavapipe("cpu-parity-present-low") else {
        return;
    };
    let (w, h) = (256, 64);
    let screen = [w as f32, h as f32];
    let quads: Vec<_> = (0..h)
        .flat_map(|y| {
            (0..w).map(move |x| QuadInstance {
                pos: [x as f32, y as f32],
                size: [1.0, 1.0],
                color: [
                    x as f32 / 255.0,
                    (255 - x) as f32 / 255.0,
                    (x ^ 0xaa) as f32 / 255.0,
                    y as f32 / 255.0,
                ],
            })
        })
        .collect();
    let init = vec![0; (w * h * 4) as usize];
    let mut replace = QuadPipeline::new_replace(&gpu.device, FORMAT);
    replace.upload(&gpu.device, &gpu.queue, screen, &quads);

    // Render the same replacement pass into a copyable target to obtain the
    // exact scene bytes fed to the production presentation pass.
    let scene_gpu = gpu.render(w, h, &init, |encoder, view| {
        replace.draw(&mut load_pass(encoder, view));
    });
    let gpu_frame = gpu_presented_scene(&gpu, w, h, &init, &replace);
    let scene = CpuFramebuffer::from_rgba8(w, h, &scene_gpu).unwrap();
    let mut same_input = CpuFramebuffer::new(w, h).unwrap();
    assert!(present_straight(&scene, &mut same_input));
    let mut scene_cpu = CpuFramebuffer::new(w, h).unwrap();
    draw_quads(
        &mut scene_cpu.target(),
        screen,
        &quads,
        QuadBlend::Replace,
        None,
    );
    let mut end_to_end = CpuFramebuffer::new(w, h).unwrap();
    assert!(present_straight(&scene_cpu, &mut end_to_end));

    let scene_stats = compare("present-low-alpha-scene", w, h, &scene_gpu, &scene_cpu);
    assert!(scene_stats.max <= 1);
    let sweep_stats = compare("present-low-alpha-sweep", w, h, &gpu_frame, &end_to_end);
    // A one-code difference in a premultiplied scene can grow to 11 RGB
    // codes at these opacities. The divide must preserve alpha exactly.
    assert!(sweep_stats.max <= 11);
    for (i, (gpu, &cpu)) in gpu_frame
        .as_chunks::<4>()
        .0
        .iter()
        .zip(end_to_end.pixels())
        .enumerate()
    {
        assert_eq!(
            gpu[3],
            super::unpack(cpu)[3],
            "alpha at ({}, {})",
            i % w as usize,
            i / w as usize
        );
    }
    let same_input_stats = compare("present-same-input", w, h, &gpu_frame, &same_input);
    assert!(same_input_stats.max <= 1);
    for (gpu, &cpu) in gpu_frame.as_chunks::<4>().0.iter().zip(same_input.pixels()) {
        assert_eq!(gpu[3], super::unpack(cpu)[3]);
    }
    let at = (2 * w + 209) as usize;
    let sampled_red = gpu_frame[at * 4];
    let [cpu_red, _, _, cpu_alpha] = super::unpack(same_input.pixels()[at]);
    assert_eq!((cpu_red, cpu_alpha), (sampled_red, 2));
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

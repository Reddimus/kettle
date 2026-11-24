//! glyphon's own `TextRenderer` on a headless device against the CPU text
//! path, pixel for pixel.
//!
//! Each case prepares the same text areas through both arms of the
//! [`TextRenderer`] stand-in, from one shared `FontSystem`: on the GPU that
//! is glyphon 0.12 unchanged, on the CPU [`prepare_cpu_text`] and
//! [`draw_cpu_text`]. Both draw over the same translucent, non-uniform
//! starting frame, under the same scissor, and every channel of every pixel
//! is compared. The bound is [`TOLERANCE`].
//!
//! Each case prints `cpu-text-parity <case>: max .. mean .. differing ..`.
//! With `KETTLE_CPU_PARITY_DIR` set, it also writes the GPU frame, the CPU
//! frame and an amplified difference there as PNGs.
//!
//! A host with no adapter skips, unless `KETTLE_REQUIRE_GPU` is set. The CJK,
//! Arabic and emoji cases shape with the host's fonts (Noto on Tower); a case
//! whose script found no face fails under `KETTLE_REQUIRE_GPU`, so a parity
//! run cannot pass on fallback boxes alone.

use glyphon::{
    Attrs, Color, ContentType, CustomGlyph, Family, Metrics, RasterizeCustomGlyphRequest,
    RasterizedCustomGlyph, Resolution, Shaping, Style, TextArea, TextBounds, Weight,
};
use kettle_i18n::{Language, Text, Translator};

use super::*;
use crate::backend::{DeviceRef, ScenePass};
use crate::cpu_raster::{CpuFramebuffer, PixelRect};
use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::{SwashCache, TextBuffer};

/// The largest difference allowed in any channel.
const TOLERANCE: u8 = 2;
const W: u32 = 480;
const H: u32 = 320;
/// The scene format, as the renderer's offscreen target.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Gpu {
    _instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

fn gpu() -> Option<Gpu> {
    let resolved = pollster::block_on(crate::resolve_headless_adapter(
        &gpu_test_config(),
        "cpu-text-parity",
    ));
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
    eprintln!("cpu-text-parity adapter: {}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("cpu-text-parity"),
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

/// The starting frame: translucent and non-uniform, valid premultiplied
/// sRGB (each colour channel at most its alpha's encoded value).
fn background() -> Vec<u8> {
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            // Alpha from fully transparent to opaque across the frame.
            let a = ((x * 255) / (W - 1)) as u8;
            let lin_a = f32::from(a) / 255.0;
            let band = (y / 20) % 4;
            let rgb = match band {
                0 => [0.9, 0.9, 0.92],
                1 => [0.02, 0.03, 0.05],
                2 => [0.20, 0.45, 0.80],
                _ => [0.75, 0.20, 0.10],
            }
            .map(|c: f32| {
                let premul = c * lin_a;
                let s = if premul <= 0.003_130_8 {
                    premul * 12.92
                } else {
                    1.055 * premul.powf(1.0 / 2.4) - 0.055
                };
                (s * 255.0).round().clamp(0.0, 255.0) as u8
            });
            out.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
    }
    out
}

/// One text area to draw: its text and how it is set.
struct Spec {
    text: String,
    attrs: Attrs<'static>,
    size: f32,
    left: f32,
    top: f32,
    scale: f32,
    bounds: Option<TextBounds>,
    color: Color,
    custom: Vec<CustomGlyph>,
}

fn spec(text: impl Into<String>, size: f32, left: f32, top: f32) -> Spec {
    Spec {
        text: text.into(),
        attrs: Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
        size,
        left,
        top,
        scale: 1.0,
        bounds: None,
        color: Color::rgb(0xe6, 0xe1, 0xcf),
        custom: Vec::new(),
    }
}

fn font_system() -> FontSystem {
    let mut fonts = FontSystem::new();
    for face in kettle_config::font::all() {
        crate::load_bundled_font(&mut fonts, face);
    }
    crate::load_bundled_font(
        &mut fonts,
        include_bytes!("../../../../assets/fonts/fira-sans/FiraSans-Regular.ttf"),
    );
    crate::load_bundled_font(
        &mut fonts,
        include_bytes!("../../../../assets/fonts/fira-sans/FiraSans-Italic.ttf"),
    );
    fonts
}

fn buffers(fonts: &mut FontSystem, specs: &[Spec]) -> Vec<TextBuffer> {
    specs
        .iter()
        .map(|s| {
            let mut buffer = TextBuffer::new(fonts, Metrics::new(s.size, (s.size * 1.3).ceil()));
            buffer.set_size(Some(W as f32), None);
            buffer.set_text(&s.text, &s.attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            buffer
        })
        .collect()
}

fn areas<'a>(specs: &'a [Spec], buffers: &'a [TextBuffer]) -> Vec<TextArea<'a>> {
    specs
        .iter()
        .zip(buffers)
        .map(|(s, buffer)| TextArea {
            buffer,
            left: s.left,
            top: s.top,
            scale: s.scale,
            bounds: s.bounds.unwrap_or(TextBounds {
                left: 0,
                top: 0,
                right: W as i32,
                bottom: H as i32,
            }),
            default_color: s.color,
            custom_glyphs: &s.custom,
        })
        .collect()
}

/// A custom glyph raster: a diagonal coverage ramp for a mask, a
/// translucent gradient for colour (straight sRGB, as Kettle's callbacks
/// return them).
fn custom_raster(request: RasterizeCustomGlyphRequest) -> Option<RasterizedCustomGlyph> {
    let (w, h) = (usize::from(request.width), usize::from(request.height));
    let color = request.id % 2 == 1;
    let mut data = Vec::with_capacity(w * h * if color { 4 } else { 1 });
    for y in 0..h {
        for x in 0..w {
            let v = (((x + y) * 255) / (w + h).max(1)) as u8;
            if color {
                data.extend_from_slice(&[v, 255 - v, 128, 255 - v / 2]);
            } else {
                data.push(v);
            }
        }
    }
    Some(RasterizedCustomGlyph {
        data,
        content_type: if color {
            ContentType::Color
        } else {
            ContentType::Mask
        },
    })
}

struct Rendered {
    gpu: Vec<u8>,
    cpu: Vec<u8>,
    /// Colour rasters the CPU drew, and glyphs in all.
    color_glyphs: usize,
    glyphs: usize,
}

fn render_both(gpu: &Gpu, specs: &[Spec], scissor: PixelRect) -> Rendered {
    let mut fonts = font_system();
    let text_buffers = buffers(&mut fonts, specs);
    let init = background();
    let resolution = Resolution {
        width: W,
        height: H,
    };

    // glyphon, unchanged.
    let handle = DeviceRef::gpu(&gpu.device, &gpu.queue);
    let cache = Cache::new(handle);
    let mut atlas = TextAtlas::new(handle, &cache, FORMAT);
    let mut viewport = Viewport::new(handle, &cache);
    viewport.update(handle, resolution);
    let mut renderer = TextRenderer::new(&mut atlas, handle);
    let mut swash = SwashCache::new();
    renderer
        .prepare_with_custom(
            handle,
            &mut fonts,
            &mut atlas,
            &viewport,
            areas(specs, &text_buffers),
            &mut swash,
            custom_raster,
        )
        .expect("glyphon prepares");
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("cpu-text-parity-target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    gpu.queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &init,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(W * 4),
            rows_per_image: Some(H),
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("cpu-text-parity"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_scissor_rect(scissor.x, scissor.y, scissor.w, scissor.h);
        renderer
            .render(&atlas, &viewport, &mut ScenePass::Gpu(&mut pass))
            .expect("glyphon renders");
    }
    let gpu_frame = read(gpu, encoder, &texture);

    // The CPU arm.
    let cache = Cache::new(DeviceRef::Cpu);
    let mut atlas = TextAtlas::new(DeviceRef::Cpu, &cache, FORMAT);
    let mut viewport = Viewport::new(DeviceRef::Cpu, &cache);
    viewport.update(DeviceRef::Cpu, resolution);
    let mut renderer = TextRenderer::new(&mut atlas, DeviceRef::Cpu);
    let mut swash = SwashCache::new();
    renderer
        .prepare_with_custom(
            DeviceRef::Cpu,
            &mut fonts,
            &mut atlas,
            &viewport,
            areas(specs, &text_buffers),
            &mut swash,
            custom_raster,
        )
        .expect("the CPU prepares");
    let mut fb = CpuFramebuffer::from_rgba8(W, H, &init).expect("frame");
    let glyphs = renderer.cpu_glyphs().expect("a CPU renderer");
    let TextAtlas::Cpu(cpu_atlas) = &atlas else {
        unreachable!("a CPU atlas");
    };
    draw_cpu_text(
        &mut fb.target(),
        [W as f32, H as f32],
        scissor,
        glyphs,
        cpu_atlas,
    );
    Rendered {
        gpu: gpu_frame,
        cpu: fb.to_rgba8(),
        color_glyphs: glyphs.iter().filter(|g| g.raster.color).count(),
        glyphs: glyphs.len(),
    }
}

fn read(gpu: &Gpu, mut encoder: wgpu::CommandEncoder, texture: &wgpu::Texture) -> Vec<u8> {
    let row = (W * 4).div_ceil(256) * 256;
    let readback = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("cpu-text-parity-readback"),
        size: u64::from(row) * u64::from(H),
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit([encoder.finish()]);
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .expect("GPU poll");
    rx.recv().expect("map channel").expect("buffer mapping");
    let data = slice.get_mapped_range().expect("mapped range");
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H as usize {
        let start = y * row as usize;
        out.extend_from_slice(&data[start..start + W as usize * 4]);
    }
    drop(data);
    readback.unmap();
    out
}

/// Compare, report and assert one case. Returns the largest difference.
fn check(case: &str, rendered: &Rendered) -> u8 {
    let Rendered { gpu, cpu, .. } = rendered;
    assert_eq!(gpu.len(), cpu.len());
    let mut max = 0u8;
    let mut sum = 0u64;
    let mut differing = 0usize;
    let mut worst = (0, 0);
    for (i, (a, b)) in gpu.iter().zip(cpu).enumerate() {
        let d = a.abs_diff(*b);
        sum += u64::from(d);
        if d > 0 {
            differing += 1;
        }
        if d > max {
            max = d;
            let px = i / 4;
            worst = (px as u32 % W, px as u32 / W);
        }
    }
    // Text must have changed the frame, or the case proves nothing.
    let init = background();
    let touched = gpu
        .as_chunks::<4>()
        .0
        .iter()
        .zip(init.as_chunks::<4>().0.iter())
        .filter(|(a, b)| a != b)
        .count();
    eprintln!(
        "cpu-text-parity {case}: max {max} mean {:.4} differing {differing}/{} \
         (worst at {worst:?}); {touched} px drawn, {} glyphs, {} colour",
        sum as f64 / gpu.len() as f64,
        gpu.len(),
        rendered.glyphs,
        rendered.color_glyphs,
    );
    if let Some(dir) = std::env::var_os("KETTLE_CPU_PARITY_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let save = |name: &str, data: Vec<u8>| {
            image::RgbaImage::from_raw(W, H, data)
                .expect("frame")
                .save(dir.join(format!("text-{case}-{name}.png")))
                .expect("PNG written");
        };
        save("gpu", gpu.clone());
        save("cpu", cpu.clone());
        let diff = gpu
            .as_chunks::<4>()
            .0
            .iter()
            .zip(cpu.as_chunks::<4>().0.iter())
            .flat_map(|(a, b)| {
                let d = (0..4).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0);
                [d.saturating_mul(60), 0, 0, 255]
            })
            .collect();
        save("diff", diff);
    }
    assert!(touched > 0, "{case}: nothing was drawn");
    assert!(
        max <= TOLERANCE,
        "{case}: a channel differs by {max} at {worst:?}"
    );
    max
}

fn full() -> PixelRect {
    PixelRect::new(0, 0, W, H)
}

fn run(case: &str, specs: &[Spec], scissor: PixelRect) -> Option<Rendered> {
    let _serial = gpu_test_guard();
    let gpu = gpu()?;
    let rendered = render_both(&gpu, specs, scissor);
    check(case, &rendered);
    Some(rendered)
}

/// Whether some face the host has covers `c`.
fn has_face_for(fonts: &mut FontSystem, text: &str, family: Family<'_>) -> bool {
    let mut buffer = TextBuffer::new(fonts, Metrics::new(16.0, 20.0));
    buffer.set_text(text, &Attrs::new().family(family), Shaping::Advanced, None);
    buffer.shape_until_scroll(fonts, false);
    buffer
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .all(|g| g.glyph_id != 0)
}

fn require_script(text: &str, family: Family<'_>) {
    let found = has_face_for(&mut font_system(), text, family);
    if !found {
        assert!(
            std::env::var_os("KETTLE_REQUIRE_GPU").is_none(),
            "no face on this host covers {text:?}"
        );
        eprintln!("no face on this host covers {text:?}; the case draws fallback glyphs");
    }
}

#[test]
fn latin_text_at_several_sizes_and_fractional_positions_matches_glyphon() {
    let mut specs = Vec::new();
    let mut top = 3.0;
    for (i, size) in [9.0, 11.0, 12.5, 13.0, 14.0, 17.0, 22.0, 31.0]
        .into_iter()
        .enumerate()
    {
        let left = 2.0 + i as f32 * 0.37;
        specs.push(spec(
            "Sphinx of black quartz, judge my vow 0123456789 {}[]",
            size,
            left,
            top + 0.29 * i as f32,
        ));
        top += size * 1.3 + 2.0;
    }
    // Proportional chrome text.
    let mut fira = spec("Tab title — Header · Menu ▸ Footer", 13.0, 240.5, 3.75);
    fira.attrs = Attrs::new().family(Family::Name("Fira Sans"));
    specs.push(fira);
    run("latin", &specs, full());
}

#[test]
fn bold_and_italic_text_matches_glyphon() {
    let styles = [
        (Weight::NORMAL, Style::Normal),
        (Weight::BOLD, Style::Normal),
        (Weight::NORMAL, Style::Italic),
        (Weight::BOLD, Style::Italic),
    ];
    let mut specs = Vec::new();
    for (i, (weight, style)) in styles.into_iter().enumerate() {
        for (j, size) in [12.0, 15.5, 24.0].into_iter().enumerate() {
            let mut s = spec(
                "Bold italic: Ferrous fjord quizzically",
                size,
                4.0 + 0.5 * j as f32,
                4.0 + (i * 3 + j) as f32 * 25.0 + 0.4,
            );
            s.attrs = s.attrs.weight(weight).style(style);
            specs.push(s);
        }
    }
    run("styles", &specs, full());
}

#[test]
fn cjk_text_matches_glyphon() {
    let text = "日本語のテキスト 中文字体 한국어 텍스트";
    require_script(text, Family::SansSerif);
    let specs: Vec<_> = [12.0, 14.0, 18.0, 27.0]
        .into_iter()
        .enumerate()
        .map(|(i, size)| {
            let mut s = spec(text, size, 3.25 + i as f32, 5.0 + i as f32 * 45.5);
            s.attrs = Attrs::new().family(Family::SansSerif);
            s
        })
        .collect();
    run("cjk", &specs, full());
}

#[test]
fn arabic_text_matches_glyphon() {
    let text = "مرحبا بالعالم — الإعدادات والمظهر";
    require_script(text, Family::SansSerif);
    let specs: Vec<_> = [13.0, 16.0, 22.0]
        .into_iter()
        .enumerate()
        .map(|(i, size)| {
            let mut s = spec(text, size, 6.6, 8.0 + i as f32 * 50.3);
            s.attrs = Attrs::new().family(Family::SansSerif);
            s
        })
        .collect();
    run("arabic", &specs, full());
}

#[test]
fn colour_emoji_match_glyphon() {
    let text = "😀🎉👍🏽✅🚀🫖 ok";
    require_script("😀🎉🚀", Family::Name("Noto Color Emoji"));
    let specs: Vec<_> = [13.0, 18.0, 28.0, 40.0]
        .into_iter()
        .enumerate()
        .map(|(i, size)| {
            spec(
                text,
                size,
                3.0 + i as f32 * 0.5,
                4.0 + i as f32 * 60.0 + 0.3,
            )
        })
        .collect();
    let rendered = run("emoji", &specs, full());
    if let Some(rendered) = rendered
        && std::env::var_os("KETTLE_REQUIRE_GPU").is_some()
    {
        assert!(rendered.color_glyphs > 0, "colour rasters were drawn");
    }
}

#[test]
fn text_bounds_and_scissors_clip_as_glyphon_does() {
    let mut specs = Vec::new();
    let line = "Clipped on every side: WWWWW gggg ÁÉÍ";
    // Bounds cutting through glyphs on each side.
    for (i, bounds) in [
        TextBounds {
            left: 12,
            top: 8,
            right: 200,
            bottom: 22,
        },
        TextBounds {
            left: 40,
            top: 50,
            right: 130,
            bottom: 64,
        },
        // Past the viewport, as glyphon clamps it.
        TextBounds {
            left: -50,
            top: 95,
            right: 10_000,
            bottom: 10_000,
        },
        // Empty.
        TextBounds {
            left: 300,
            top: 140,
            right: 300,
            bottom: 160,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut s = spec(line, 16.0, 6.0 - i as f32 * 20.0, 6.0 + i as f32 * 45.0);
        s.bounds = Some(bounds);
        specs.push(s);
    }
    // A run that starts above the bounds and one past the bottom.
    let mut s = spec("one\ntwo\nthree\nfour", 14.0, 300.0, 180.0);
    s.bounds = Some(TextBounds {
        left: 0,
        top: 200,
        right: W as i32,
        bottom: 230,
    });
    specs.push(s);
    // Text running off the frame's right and bottom edges.
    specs.push(spec("Off the edge →→→→→", 20.0, 400.0, 300.0));
    run("clip", &specs, full());
    // The same areas under a scissor, as the cursor exclusion sets one.
    run("clip-scissor", &specs, PixelRect::new(30, 12, 170, 210));
}

#[test]
fn translucent_text_over_a_translucent_frame_matches_glyphon() {
    let mut specs = Vec::new();
    for (i, alpha) in [16u8, 64, 128, 200, 255].into_iter().enumerate() {
        for (j, rgb) in [[255, 255, 255], [0, 0, 0], [40, 170, 250], [230, 60, 20]]
            .into_iter()
            .enumerate()
        {
            let mut s = spec(
                "Translucent ████ text",
                15.0,
                4.0 + j as f32 * 118.0,
                4.0 + i as f32 * 62.0 + j as f32 * 13.0,
            );
            s.color = Color::rgba(rgb[0], rgb[1], rgb[2], alpha);
            specs.push(s);
        }
    }
    run("translucent", &specs, full());
}

#[test]
fn spanish_settings_strings_match_glyphon() {
    let tr = Translator::new(Language::Es);
    let strings = [
        tr.settings_heading("Pestañas"),
        tr.text(Text::SettingsCategoryAppearance).to_string(),
        tr.text(Text::SettingsFieldCopyTrimWhitespace).to_string(),
        tr.text(Text::SettingsDescriptionFieldCopyTrimWhitespace)
            .to_string(),
        tr.text(Text::SettingsFieldMinimumContrast).to_string(),
        tr.text(Text::SettingsDescriptionRestoreScrollbackLines)
            .to_string(),
        tr.text(Text::SettingsValueFollowSystem).to_string(),
        "¿Cerrar la ventana? Ñandú, pingüino, acción".to_string(),
    ];
    let specs: Vec<_> = strings
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let mut s = spec(text, 13.0, 10.0, 6.0 + i as f32 * 38.0);
            if i % 2 == 1 {
                s.attrs = s.attrs.weight(Weight::BOLD);
                s.color = Color::rgb(0x8a, 0xb4, 0xf8);
            }
            s
        })
        .collect();
    run("spanish", &specs, full());
}

#[test]
fn scaled_areas_and_custom_glyphs_match_glyphon() {
    let mut specs = Vec::new();
    for (i, scale) in [1.25f32, 1.5, 2.0].into_iter().enumerate() {
        let mut s = spec(
            "HiDPI chrome ✓",
            12.0,
            5.0 + i as f32 * 0.3,
            4.0 + i as f32 * 70.0,
        );
        s.scale = scale;
        s.custom = vec![
            // Snapped, a mask, in the area's colour.
            CustomGlyph {
                id: 2,
                left: 200.0,
                top: 2.0,
                width: 14.0,
                height: 14.0,
                color: None,
                snap_to_physical_pixel: true,
                metadata: 0,
            },
            // Unsnapped, a colour raster.
            CustomGlyph {
                id: 3,
                left: 220.3,
                top: 3.6,
                width: 12.0,
                height: 9.0,
                color: Some(Color::rgba(255, 0, 0, 90)),
                snap_to_physical_pixel: false,
                metadata: 0,
            },
            // A mask with its own colour.
            CustomGlyph {
                id: 4,
                left: 236.0,
                top: 1.0,
                width: 7.0,
                height: 16.0,
                color: Some(Color::rgba(120, 220, 90, 180)),
                snap_to_physical_pixel: true,
                metadata: 0,
            },
        ];
        specs.push(s);
    }
    run("scaled-custom", &specs, full());
}

/// What drawing a typical Settings panel's text costs on the CPU: the
/// renderer's own Settings overlay, prepared through the real frame path on
/// a CPU renderer, then drawn with [`draw_cpu_text`].
///
/// `cargo test -p kettle-render --lib settings_text_cpu_cost -- --ignored --nocapture`
#[test]
#[ignore = "timing; run by hand"]
fn settings_text_cpu_cost() {
    use crate::{Overlay, Renderer, RendererBackend, SettingsOverlay, SettingsRow, TabBar};
    use std::time::Instant;
    let tr = Translator::new(Language::Es);
    let set = SettingsOverlay {
        categories: (0..11)
            .map(|i| format!("{} {i}", tr.text(Text::SettingsCategoryAppearance)))
            .collect(),
        title: tr.settings_heading("Apariencia"),
        reset_label: "Restablecer".into(),
        rows: (0..14)
            .map(|i| SettingsRow {
                label: format!("{} {i}", tr.text(Text::SettingsFieldMinimumContrast)),
                group: ["Colores y texto", "Ventana", "Cursor"][i / 5].into(),
                value: tr.text(Text::SettingsValueFollowSystem).into(),
                description: tr
                    .text(Text::SettingsDescriptionFieldCopyTrimWhitespace)
                    .into(),
                reset: Some(true),
                ..Default::default()
            })
            .collect(),
        hints: vec![
            ("Arriba/Abajo".into(), "campo".into()),
            ("Izq./Der.".into(), "cambiar".into()),
            ("Supr".into(), "restablecer".into()),
            ("Esc".into(), "cerrar".into()),
        ],
        ..Default::default()
    };
    let overlay = Overlay {
        settings: Some(set),
        ..Default::default()
    };
    let cfg = gpu_test_config();
    for (w, h, scale) in [(1280u32, 820u32, 1.0f32), (2560, 1640, 2.0)] {
        let mut renderer =
            Renderer::headless_for_tests_on(RendererBackend::Cpu, &cfg, w, h, scale, false)
                .expect("builds")
                .expect("a CPU renderer");
        let frame = |renderer: &mut Renderer| {
            let start = Instant::now();
            let _outcome = renderer
                .render_frame(&[], &TabBar::hidden(), &cfg, &overlay)
                .expect("frame");
            start.elapsed().as_secs_f64() * 1e3
        };
        let cold = frame(&mut renderer);
        let warm: Vec<f64> = (0..20).map(|_| frame(&mut renderer)).collect();
        let glyphs = renderer
            .menu_text_renderer
            .cpu_glyphs()
            .expect("CPU")
            .to_vec();
        let TextAtlas::Cpu(atlas) = &renderer.atlas else {
            unreachable!()
        };
        let mut fb = CpuFramebuffer::new(w, h).expect("frame");
        fb.clear([0.1, 0.1, 0.12, 1.0]);
        let runs = 50;
        let start = Instant::now();
        for _ in 0..runs {
            draw_cpu_text(
                &mut fb.target(),
                [w as f32, h as f32],
                PixelRect::new(0, 0, w, h),
                &glyphs,
                atlas,
            );
        }
        let draw = start.elapsed().as_secs_f64() * 1e3 / f64::from(runs);
        let pixels: u64 = glyphs
            .iter()
            .map(|g| u64::from(g.dim[0]) * u64::from(g.dim[1]))
            .sum();
        let (mask, color) = atlas.cached();
        eprintln!(
            "settings-text {w}x{h}@{scale}: {} glyphs, {pixels} glyph px; draw {draw:.3} ms; \
             frame cold {cold:.2} ms, warm median {:.2} ms (prepare + layout, no raster); \
             atlas {mask} mask + {color} colour keys",
            glyphs.len(),
            median(warm),
        );
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

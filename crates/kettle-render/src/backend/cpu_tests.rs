//! The CPU backend through the real frame path, with no GPU.

use std::sync::{Arc, Mutex};

use super::cpu::{CpuDraw, CpuFrame, CpuPipeline, CpuRaster};
use super::layers::QuadLayer;
use super::{DeviceRef, RendererBackend, ScenePass};
use crate::glyphpipe::{GlyphPipeline, RasterGlyph};
use crate::gpu_tests::gpu_test_config;
use crate::headless_tests::{capture, focused, pane, snapshot_of};
use crate::quad::QuadInstance;
use crate::{FrameOutcome, Renderer, StatusBar, TabBar};

fn cpu_renderer(width: u32, height: u32) -> (Renderer, kettle_config::Config) {
    let cfg = gpu_test_config();
    let renderer =
        Renderer::headless_for_tests_on(RendererBackend::Cpu, &cfg, width, height, 1.0, false)
            .expect("a CPU renderer builds")
            .expect("a CPU renderer needs no adapter");
    (renderer, cfg)
}

/// What a test rasterizer saw: each draw's pipeline, size and scissor, and
/// how many presentation passes ran.
#[derive(Default)]
struct Seen {
    draws: Vec<(CpuPipeline, usize, [u32; 4])>,
    presents: usize,
}

/// Records each draw, and marks the presented frame so a test can tell it
/// from the scene.
struct RecordingRaster(Arc<Mutex<Seen>>);

impl CpuRaster for RecordingRaster {
    fn draw(&mut self, _target: &mut CpuFrame, scissor: [u32; 4], draw: &CpuDraw<'_>) {
        let mut seen = self.0.lock().expect("unpoisoned");
        seen.draws.push((draw.pipeline(), draw.len(), scissor));
    }

    fn present(&mut self, scene: &CpuFrame, output: &mut CpuFrame) {
        self.0.lock().expect("unpoisoned").presents += 1;
        output.copy_from(scene);
        output.clear([1, 2, 3, 4]);
    }
}

/// Render one offscreen CPU frame of a pane holding `text`.
fn render(renderer: &mut Renderer, cfg: &kettle_config::Config, text: &[u8]) {
    let snap = snapshot_of(20, 4, text);
    let (width, height) = renderer.surface_size();
    let outcome = renderer
        .render_frame_with_status_and_pre_present(
            &[pane(&snap, width, height)],
            &TabBar::hidden(),
            cfg,
            &focused(true),
            &StatusBar::hidden(),
            || {},
        )
        .expect("a CPU frame renders");
    // With no presenter the frame stays in the backend, as a headless GPU
    // renderer's capture is its frame.
    assert!(matches!(outcome, FrameOutcome::Occluded));
}

#[test]
fn a_cpu_renderer_has_no_gpu_context_and_presents_its_clear_to_a_buffer() {
    let (mut renderer, cfg) = cpu_renderer(320, 120);
    assert_eq!(renderer.backend(), RendererBackend::Cpu);
    assert!(renderer.gpu_context().is_none());
    assert!(renderer.surface.is_none());
    assert_eq!(renderer.surface_size(), (320, 120));
    render(&mut renderer, &cfg, b"hello");
    let frame = renderer.cpu_presented_for_tests().expect("a CPU frame");
    assert_eq!((frame.width(), frame.height()), (320, 120));
    assert!(
        frame.pixels().iter().all(|&byte| byte == 0),
        "until the rasterizer lands, a CPU frame is its transparent clear"
    );
    assert!(renderer.last_scene.is_some(), "the frame is recorded");
}

#[test]
fn the_scene_pass_replays_each_pipeline_on_the_cpu_in_gpu_order() {
    let (mut renderer, cfg) = cpu_renderer(320, 120);
    render(&mut renderer, &cfg, b"hello");
    let draws = &renderer.cpu_backend_for_tests().expect("CPU state").draws;
    let pipelines: Vec<_> = draws.iter().map(|draw| draw.pipeline).collect();
    let at = |pipeline: CpuPipeline| {
        pipelines
            .iter()
            .position(|&p| p == pipeline)
            .unwrap_or_else(|| panic!("{pipeline:?} drew nothing: {pipelines:?}"))
    };
    // The pane's base replaces, its cells and cursor blend over it, and the
    // grid text is drawn above both, as `encode_scene_pass` orders them.
    assert_eq!(pipelines.first(), Some(&CpuPipeline::QuadReplace));
    assert!(at(CpuPipeline::QuadReplace) < at(CpuPipeline::QuadBlend));
    assert!(at(CpuPipeline::QuadBlend) < at(CpuPipeline::Glyph));
    let glyphs = &draws[at(CpuPipeline::Glyph)];
    assert_eq!(
        glyphs.instances, 5,
        "one grid instance per glyph of `hello`"
    );
    let [x, y, w, h] = glyphs.scissor;
    assert!(
        w > 0 && h > 0 && x + w <= 320 && y + h <= 120,
        "the pane's text is clipped to the pane: {:?}",
        glyphs.scissor
    );
    // A steady frame replays the same draws.
    let first = draws.clone();
    render(&mut renderer, &cfg, b"hello");
    assert_eq!(
        renderer.cpu_backend_for_tests().expect("CPU state").draws,
        first
    );
}

#[test]
fn the_rasterizer_seam_receives_every_recorded_draw() {
    let (mut renderer, cfg) = cpu_renderer(320, 120);
    let seen = Arc::new(Mutex::new(Seen::default()));
    assert!(renderer.set_cpu_raster_for_tests(Box::new(RecordingRaster(seen.clone()))));
    render(&mut renderer, &cfg, b"hello");
    let recorded: Vec<_> = renderer
        .cpu_backend_for_tests()
        .expect("CPU state")
        .draws
        .iter()
        .filter(|draw| draw.scissor[2] > 0 && draw.scissor[3] > 0)
        .map(|draw| (draw.pipeline, draw.instances, draw.scissor))
        .collect();
    let seen = seen.lock().expect("unpoisoned");
    assert_eq!(seen.draws, recorded);
    assert_eq!(seen.presents, 0, "an opaque surface needs no conversion");
}

#[test]
fn a_translucent_postmultiplied_cpu_frame_presents_its_conversion() {
    let cfg = kettle_config::Config {
        background_opacity: 0.5,
        ..gpu_test_config()
    };
    let mut renderer =
        Renderer::headless_for_tests_on(RendererBackend::Cpu, &cfg, 64, 32, 1.0, true)
            .expect("a CPU renderer builds")
            .expect("no adapter needed");
    assert_eq!(
        renderer.config.alpha_mode,
        wgpu::CompositeAlphaMode::PostMultiplied
    );
    let seen = Arc::new(Mutex::new(Seen::default()));
    assert!(renderer.set_cpu_raster_for_tests(Box::new(RecordingRaster(seen.clone()))));
    render(&mut renderer, &cfg, b"x");
    assert_eq!(seen.lock().expect("unpoisoned").presents, 1);
    let presented = renderer.cpu_presented_for_tests().expect("a CPU frame");
    assert_eq!(
        &presented.pixels()[..4],
        &[1, 2, 3, 4],
        "the converted frame is presented"
    );
}

#[test]
fn a_cpu_screenshot_saves_the_scene_without_a_gpu() {
    let (mut renderer, cfg) = cpu_renderer(96, 48);
    let snap = snapshot_of(8, 2, b"hi");
    let frame = capture(&mut renderer, &cfg, &[pane(&snap, 96, 48)], &focused(true));
    assert_eq!(frame.dimensions(), (96, 48));
    assert!(
        frame.pixels().all(|pixel| pixel.0 == [0, 0, 0, 0]),
        "the capture is the cleared scene, in straight alpha"
    );
}

#[test]
fn a_cpu_renderer_resizes_like_a_gpu_one() {
    let (mut renderer, cfg) = cpu_renderer(320, 120);
    renderer.resize(0, 100_000);
    assert_eq!(
        renderer.surface_size(),
        (1, super::CPU_MAX_TEXTURE_DIMENSION),
        "floored at one pixel and capped at the backend's texture limit"
    );
    renderer.resize(200, 80);
    render(&mut renderer, &cfg, b"ok");
    let frame = renderer.cpu_presented_for_tests().expect("a CPU frame");
    assert_eq!((frame.width(), frame.height()), (200, 80));
}

#[test]
fn a_cpu_glyph_atlas_keeps_each_raster_at_its_slot() {
    let mut pipeline = GlyphPipeline::new_cpu(
        wgpu::TextureFormat::Rgba8UnormSrgb,
        kettle_core::GraphicsBudget::default(),
    )
    .expect("CPU glyph pipeline");
    let mask = [10, 20, 30, 40, 50, 60];
    let slot = pipeline
        .ensure_glyph(
            DeviceRef::Cpu,
            glyphon::cosmic_text::CacheKey::new(
                glyphon::fontdb::ID::dummy(),
                7,
                13.0,
                (0.0, 0.0),
                glyphon::fontdb::Weight::NORMAL,
                glyphon::cosmic_text::CacheKeyFlags::empty(),
            )
            .0,
            || {
                Some(RasterGlyph {
                    color: false,
                    width: 3,
                    height: 2,
                    left: 0,
                    top: 0,
                    data: &mask,
                })
            },
        )
        .expect("the mask is admitted");
    let atlas = pipeline.mask_texture_for_tests().expect("a CPU atlas copy");
    let (x, y) = (slot.atlas_x as usize, slot.atlas_y as usize);
    let row = |line: usize| {
        let start = (y + line) * atlas.width as usize + x;
        &atlas.pixels[start..start + 3]
    };
    assert_eq!((row(0), row(1)), (&mask[..3], &mask[3..]));
    assert!(pipeline.upload(DeviceRef::Cpu, [64.0, 64.0], &[]));
}

#[test]
fn cpu_quad_layers_draw_what_they_were_given_minus_the_hidden_cursor() {
    let quad = |x: f32| QuadInstance {
        pos: [x, 0.0],
        size: [1.0, 1.0],
        color: [1.0; 4],
    };
    let mut layer = QuadLayer::cpu_blend();
    layer.upload(
        DeviceRef::Cpu,
        [8.0, 8.0],
        &[quad(0.0), quad(1.0), quad(2.0)],
    );
    assert_eq!(layer.count(), 3);
    assert_eq!(layer.uploaded().map(Iterator::count), Some(3));
    let mut frame = CpuFrame::new(8, 8).expect("small frame");
    let mut raster = super::cpu::NoRaster;
    let mut records = Vec::new();
    {
        let mut pass = super::cpu::CpuPass::new(&mut frame, &mut raster, &mut records);
        let mut pass = ScenePass::Cpu(&mut pass);
        layer.draw_hiding(&mut pass, Some(1..2));
        layer.draw_range(&mut pass, 1..2);
    }
    let counts: Vec<_> = records.iter().map(|record| record.instances).collect();
    assert_eq!(
        counts,
        [1, 1, 1],
        "before the cursor, after it, then the cursor"
    );
}

#[test]
fn cpu_text_prepare_lays_out_each_visible_glyph_as_glyphon_does() {
    use super::text::{Cache, TextAtlas, TextRenderer, Viewport};
    use glyphon::{Attrs, Family, Resolution, Shaping, TextArea, TextBounds};
    let mut fonts = crate::FontSystem::new();
    crate::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let mut buffer = crate::TextBuffer::new(&mut fonts, glyphon::Metrics::new(13.0, 16.0));
    buffer.set_text(
        "abc",
        &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fonts, false);
    let cache = Cache::new(DeviceRef::Cpu);
    let mut atlas = TextAtlas::new(DeviceRef::Cpu, &cache, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut viewport = Viewport::new(DeviceRef::Cpu, &cache);
    viewport.update(
        DeviceRef::Cpu,
        Resolution {
            width: 64,
            height: 32,
        },
    );
    let mut text = TextRenderer::new(&mut atlas, DeviceRef::Cpu);
    let mut swash = crate::SwashCache::new();
    for (top, glyphs) in [(0.0, 3), (100.0, 0)] {
        let area = TextArea {
            buffer: &buffer,
            left: 2.0,
            top,
            scale: 1.0,
            bounds: TextBounds {
                left: -5,
                top: 0,
                right: 100,
                bottom: 32,
            },
            default_color: glyphon::Color::rgb(1, 2, 3),
            custom_glyphs: &[],
        };
        text.prepare(
            DeviceRef::Cpu,
            &mut fonts,
            &mut atlas,
            &viewport,
            [area],
            &mut swash,
        )
        .expect("CPU text prepares");
        assert_eq!(
            text.cpu_glyph_count(),
            Some(glyphs),
            "a run below the bounds is skipped, as glyphon skips it"
        );
    }
}

/// The child half of [`no_gpu_driver_is_loaded_by_a_cpu_renderer`]: render
/// frames on the backend `KETTLE_CPU_BACKEND_CHILD` names, then report every
/// mapping of this process.
#[test]
fn renderer_child_process() {
    let Some(backend) = std::env::var_os("KETTLE_CPU_BACKEND_CHILD") else {
        return;
    };
    let backend = if backend == "gpu" {
        RendererBackend::Gpu
    } else {
        RendererBackend::Cpu
    };
    let cfg = gpu_test_config();
    let Some(mut renderer) =
        Renderer::headless_for_tests_on(backend, &cfg, 320, 120, 1.0, false).expect("builds")
    else {
        println!("KETTLE_CPU_BACKEND_NO_ADAPTER");
        return;
    };
    render(&mut renderer, &cfg, b"hello");
    render(&mut renderer, &cfg, b"world");
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    println!("KETTLE_CPU_BACKEND_MAPS_BEGIN\n{maps}KETTLE_CPU_BACKEND_MAPS_END");
}

/// What a GPU driver, the Vulkan loader or Mesa maps into a process.
#[cfg(target_os = "linux")]
const GPU_DRIVER_MAPPINGS: [&str; 10] = [
    "libvulkan",
    "libEGL",
    "libGLX",
    "libGL.so",
    "libgallium",
    "libLLVM",
    "nvidia",
    "_icd",
    "mesa",
    "/dev/dri",
];

/// The mappings of a fresh test process that rendered on `backend`, or
/// `None` when that backend has no adapter here.
#[cfg(target_os = "linux")]
fn child_mappings(backend: &str) -> Option<String> {
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "backend::cpu_tests::renderer_child_process",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("KETTLE_CPU_BACKEND_CHILD", backend)
        .output()
        .expect("the child test runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{backend} child failed: {stdout}");
    if stdout.contains("KETTLE_CPU_BACKEND_NO_ADAPTER") {
        return None;
    }
    let maps = stdout
        .split_once("KETTLE_CPU_BACKEND_MAPS_BEGIN\n")
        .and_then(|(_, rest)| rest.split_once("KETTLE_CPU_BACKEND_MAPS_END"))
        .map(|(maps, _)| maps.to_owned())
        .expect("the child reports its mappings");
    assert!(maps.lines().count() > 10, "the mappings were read");
    Some(maps)
}

/// A CPU renderer creates no wgpu instance, so no GPU driver, Vulkan loader
/// or Mesa library is ever mapped into the process. Each renderer runs in a
/// fresh process, because every other test in this binary may load one.
#[cfg(target_os = "linux")]
#[test]
fn no_gpu_driver_is_loaded_by_a_cpu_renderer() {
    let cpu = child_mappings("cpu").expect("a CPU renderer needs no adapter");
    for driver in GPU_DRIVER_MAPPINGS {
        let mapped: Vec<_> = cpu.lines().filter(|line| line.contains(driver)).collect();
        assert!(mapped.is_empty(), "{driver} is mapped: {mapped:?}");
    }
    // The same check finds the driver a GPU renderer loads, so it cannot
    // pass by looking in the wrong place.
    match child_mappings("gpu") {
        Some(gpu) => assert!(
            GPU_DRIVER_MAPPINGS
                .iter()
                .any(|driver| gpu.contains(driver)),
            "a GPU renderer maps a driver"
        ),
        None => crate::headless_tests::no_gpu_adapter(),
    }
}

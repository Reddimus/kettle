//! Whole frames through both backends: the same scene rendered by the GPU
//! renderer (lavapipe in CI) and by the CPU renderer must agree within
//! `cpu_raster`'s per-pipeline tolerance, and the CPU window path must draw
//! into its presenter's buffer and commit through its pre-commit hook.
//!
//! Chrome text (tab titles, the status bar, menus) is glyphon's and drawn by
//! the CPU text compositor; these scenes keep to the grid, so they measure
//! the eight pipelines.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::selection::SelectionRange;
use kettle_core::{Column, Line, Point};

use super::RendererBackend;
use super::cpu::{CpuPresentOutcome, CpuSurface};
use crate::cpu_raster::pixels_to_rgba8;
use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::headless_tests::{capture, focused, no_gpu_adapter, pane, snapshot_of};
use crate::{FrameOutcome, Renderer, StatusBar, TabBar};

/// The largest channel difference between two frames, and how many
/// channels differ by more than 2.
fn difference(a: &image::RgbaImage, b: &image::RgbaImage) -> (u8, usize) {
    assert_eq!(a.dimensions(), b.dimensions());
    let mut worst = 0;
    let mut over = 0;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for c in 0..4 {
            let d = pa.0[c].abs_diff(pb.0[c]);
            worst = worst.max(d);
            over += usize::from(d > 2);
        }
    }
    (worst, over)
}

/// Render `snap` in one pane on each backend and compare the captures.
fn assert_backends_agree(
    name: &str,
    cfg: &kettle_config::Config,
    snap: &crate::PaneSnapshot,
    size: (u32, u32),
    translucent: bool,
) {
    let _serialized = gpu_test_guard();
    let (w, h) = size;
    let frame = |backend| {
        let mut renderer = Renderer::headless_for_tests_on(backend, cfg, w, h, 1.0, translucent)
            .expect("builds")?;
        Some(capture(
            &mut renderer,
            cfg,
            &[pane(snap, w, h)],
            &focused(true),
        ))
    };
    let cpu = frame(RendererBackend::Cpu).expect("a CPU renderer needs no adapter");
    let Some(gpu) = frame(RendererBackend::Gpu) else {
        no_gpu_adapter();
        return;
    };
    if let Some(dir) = std::env::var_os("KETTLE_CPU_PARITY_DUMP") {
        let dir = std::path::Path::new(&dir);
        cpu.save(dir.join(format!("{name}-cpu.png"))).expect("dump");
        gpu.save(dir.join(format!("{name}-gpu.png"))).expect("dump");
    }
    let (worst, over) = difference(&cpu, &gpu);
    assert!(
        over == 0,
        "{name}: {over} channels differ by more than 2 (worst {worst})"
    );
    // The frames are not trivially equal: the scene drew something.
    let first = cpu.get_pixel(0, 0).0;
    assert!(cpu.pixels().any(|p| p.0 != first), "{name}: an empty frame");
}

const TEXT: &[u8] = b"\x1b[1mkettle\x1b[0m \x1b[31mred\x1b[32m green\x1b[0m \x1b[7minverse\x1b[0m\r\n\
\x1b[4munderline\x1b[0m \x1b[38;2;250;180;40mtruecolor\x1b[0m \xe2\x94\x8c\xe2\x94\x80\xe2\x94\x90 \xe2\x96\x88\xe2\x96\x93\xe2\x96\x92\r\n\
$ seq 1 3\r\n1\r\n2\r\n3\r\n$ ";

#[test]
fn a_text_pane_draws_the_same_on_the_cpu_and_the_gpu() {
    let snap = snapshot_of(48, 8, TEXT);
    assert_backends_agree("text", &gpu_test_config(), &snap, (420, 160), false);
}

#[test]
fn a_selection_draws_the_same_on_the_cpu_and_the_gpu() {
    let mut snap = snapshot_of(48, 8, TEXT);
    snap.selection = Some(SelectionRange::new(
        Point::new(Line(0), Column(3)),
        Point::new(Line(2), Column(6)),
        false,
    ));
    assert_backends_agree("selection", &gpu_test_config(), &snap, (420, 160), false);
}

#[test]
fn a_translucent_window_draws_the_same_on_the_cpu_and_the_gpu() {
    let cfg = kettle_config::Config {
        background_opacity: 0.6,
        ..gpu_test_config()
    };
    let snap = snapshot_of(48, 8, TEXT);
    assert_backends_agree("translucent", &cfg, &snap, (420, 160), true);
}

#[test]
fn a_starfield_draws_the_same_on_the_cpu_and_the_gpu() {
    let cfg = kettle_config::Config {
        background_type: kettle_config::BackgroundType::Starfield,
        background_animation: kettle_config::BackgroundAnimation::Off,
        ..gpu_test_config()
    };
    let snap = snapshot_of(48, 8, b"$ ");
    assert_backends_agree("starfield", &cfg, &snap, (420, 160), false);
}

/// The test config with an opaque background (the default is slightly
/// translucent).
fn opaque() -> kettle_config::Config {
    kettle_config::Config {
        background_opacity: 1.0,
        ..gpu_test_config()
    }
}

/// What the fake window surface saw.
#[derive(Default)]
struct SurfaceLog {
    sizes: Vec<(u32, u32)>,
    translucent: Vec<bool>,
    /// Each committed frame, as RGBA8.
    frames: Vec<Vec<u8>>,
    /// How many times the pre-commit hook ran, and whether every run came
    /// after its frame was drawn.
    pre_commits: usize,
    committed_after_draw: bool,
    occluded: Vec<bool>,
    released: usize,
}

/// A window surface in memory: one buffer, committed at once.
struct FakeSurface {
    log: Arc<Mutex<SurfaceLog>>,
    size: (u32, u32),
    buffer: Vec<u32>,
    translucency: bool,
}

impl CpuSurface for FakeSurface {
    fn resize(&mut self, width: u32, height: u32, _scale: f64) {
        self.size = (width, height);
        self.log.lock().unwrap().sizes.push(self.size);
    }

    fn supports_translucency(&self) -> bool {
        self.translucency
    }

    fn set_translucent(&mut self, translucent: bool) -> bool {
        let on = translucent && self.translucency;
        self.log.lock().unwrap().translucent.push(on);
        on
    }

    fn present_frame(
        &mut self,
        draw: &mut dyn FnMut(&mut [u32], u32, u32) -> bool,
        pre_commit: &mut dyn FnMut(),
    ) -> CpuPresentOutcome {
        let (w, h) = self.size;
        // A recycled buffer's old contents must not show through.
        self.buffer = vec![0xdead_beef; w as usize * h as usize];
        if !draw(&mut self.buffer, w, h) {
            return CpuPresentOutcome::NotDrawn;
        }
        let drawn = !self.buffer.contains(&0xdead_beef);
        pre_commit();
        let mut log = self.log.lock().unwrap();
        log.pre_commits += 1;
        log.committed_after_draw = drawn;
        log.frames.push(pixels_to_rgba8(&self.buffer));
        CpuPresentOutcome::Presented
    }

    fn flush(&mut self, _pre_commit: &mut dyn FnMut()) -> bool {
        false
    }

    fn has_deferred(&self) -> bool {
        false
    }

    fn release_idle_buffers(&mut self) -> usize {
        self.log.lock().unwrap().released += 1;
        1
    }

    fn idle_release_deadline(&self) -> Option<Instant> {
        Some(Instant::now())
    }

    fn set_occluded(&mut self, occluded: bool) {
        self.log.lock().unwrap().occluded.push(occluded);
    }

    fn describe(&self) -> &'static str {
        "fake"
    }
}

fn window(cfg: &kettle_config::Config, translucency: bool) -> (Renderer, Arc<Mutex<SurfaceLog>>) {
    let log = Arc::new(Mutex::new(SurfaceLog::default()));
    let surface = FakeSurface {
        log: log.clone(),
        size: (0, 0),
        buffer: Vec::new(),
        translucency,
    };
    let renderer =
        Renderer::cpu_window_for_tests(cfg, 200, 80, Box::new(surface)).expect("CPU window");
    (renderer, log)
}

fn present(renderer: &mut Renderer, cfg: &kettle_config::Config, pre_present: impl FnOnce()) {
    let snap = snapshot_of(20, 4, b"hello");
    let outcome = renderer
        .render_frame_with_status_and_pre_present(
            &[pane(&snap, 200, 80)],
            &TabBar::hidden(),
            cfg,
            &focused(true),
            &StatusBar::hidden(),
            pre_present,
        )
        .expect("a CPU window frame renders");
    assert!(matches!(outcome, FrameOutcome::Presented));
}

#[test]
fn a_cpu_window_draws_into_its_buffer_and_runs_pre_present_at_the_commit() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, true);
    let notified = Arc::new(Mutex::new(0));
    let counter = notified.clone();
    present(&mut renderer, &cfg, move || *counter.lock().unwrap() += 1);
    assert_eq!(*notified.lock().unwrap(), 1, "pre_present runs once");
    let log = log.lock().unwrap();
    assert_eq!(log.pre_commits, 1);
    assert!(
        log.committed_after_draw,
        "every pixel is drawn before the commit"
    );
    assert_eq!(log.sizes, [(200, 80)]);
    assert_eq!(log.translucent, [false], "an opaque config asks for XRGB");
    // The window's frame is the offscreen renderer's, pixel for pixel.
    let mut offscreen =
        Renderer::headless_for_tests_on(RendererBackend::Cpu, &cfg, 200, 80, 1.0, false)
            .expect("builds")
            .expect("no adapter needed");
    let snap = snapshot_of(20, 4, b"hello");
    let reference = capture(
        &mut offscreen,
        &cfg,
        &[pane(&snap, 200, 80)],
        &focused(true),
    );
    let frame = &log.frames[0];
    let worst = frame
        .iter()
        .zip(reference.as_raw())
        .map(|(a, b)| a.abs_diff(*b))
        .max();
    assert_eq!(worst, Some(0), "the window frame matches the capture");
}

#[test]
fn a_translucent_cpu_window_presents_premultiplied() {
    let cfg = kettle_config::Config {
        background_opacity: 0.5,
        ..gpu_test_config()
    };
    let (mut renderer, log) = window(&cfg, true);
    assert_eq!(
        renderer.config.alpha_mode,
        wgpu::CompositeAlphaMode::PreMultiplied
    );
    present(&mut renderer, &cfg, || {});
    let log = log.lock().unwrap();
    assert_eq!(log.translucent, [true]);
    // The background is drawn at its opacity, premultiplied as the GPU's
    // PreMultiplied surface receives it: no channel above alpha in linear
    // light, and alpha below one.
    let px = log.frames[0].as_chunks::<4>().0[200 * 40 + 100];
    assert!(px[3] > 0 && px[3] < 255, "translucent background: {px:?}");
}

#[test]
fn a_cpu_window_on_an_opaque_only_surface_stays_opaque() {
    let cfg = kettle_config::Config {
        background_opacity: 0.5,
        ..gpu_test_config()
    };
    let (renderer, log) = window(&cfg, false);
    assert_eq!(renderer.config.alpha_mode, wgpu::CompositeAlphaMode::Opaque);
    assert_eq!(log.lock().unwrap().translucent, [false]);
}

#[test]
fn opacity_changes_reach_a_cpu_window() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, true);
    let translucent = kettle_config::Config {
        background_opacity: 0.7,
        ..cfg.clone()
    };
    renderer.set_background_compositing(&translucent);
    renderer.set_background_compositing(&cfg);
    assert_eq!(log.lock().unwrap().translucent, [false, true, false]);
}

#[test]
fn idle_release_occlusion_and_resize_reach_a_cpu_window() {
    let cfg = gpu_test_config();
    let (mut renderer, log) = window(&cfg, false);
    assert!(renderer.idle_release_deadline().is_some());
    assert_eq!(renderer.release_idle_buffers(), 1);
    renderer.set_occluded(true);
    renderer.set_occluded(false);
    renderer.resize(120, 60);
    assert_eq!(renderer.cpu_presenter_name(), Some("fake"));
    let log = log.lock().unwrap();
    assert_eq!(log.released, 1);
    assert_eq!(log.occluded, [true, false]);
    assert_eq!(log.sizes, [(200, 80), (120, 60)]);
}

#[test]
fn a_gpu_or_offscreen_renderer_has_no_cpu_presenter() {
    let cfg = gpu_test_config();
    let mut renderer =
        Renderer::headless_for_tests_on(RendererBackend::Cpu, &cfg, 32, 32, 1.0, false)
            .expect("builds")
            .expect("no adapter needed");
    assert_eq!(renderer.cpu_presenter_name(), None);
    assert_eq!(renderer.idle_release_deadline(), None);
    assert_eq!(renderer.release_idle_buffers(), 0);
    assert!(!renderer.has_held_frame());
    assert!(!renderer.flush_held_frame(|| panic!("nothing to commit")));
}

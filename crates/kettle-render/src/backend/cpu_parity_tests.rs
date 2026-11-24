//! Whole frames through both backends: the same scene rendered by the GPU
//! renderer (lavapipe in CI) and by the CPU renderer must agree within
//! `cpu_raster`'s per-pipeline tolerance, and the CPU window path must draw
//! into its presenter's buffer and commit through its pre-commit hook.
//!
//! Chrome text (tab titles, pane titles, the status bar, menus) is glyphon's
//! and drawn by the CPU text compositor (`text.rs`); the chrome scene checks
//! it, the others keep to the grid and the eight pipelines.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::selection::SelectionRange;
use kettle_core::{Column, Line, Point};

use super::RendererBackend;
use super::cpu::{CpuPresentOutcome, CpuSurface, HeldFrameFlush};
use crate::cpu_raster::pixels_to_rgba8;
use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::headless_tests::{
    capture, capture_with_tabs, focused, no_gpu_adapter, pane, snapshot_of,
};
use crate::{FrameOutcome, Renderer, StatusBar, TabActivity, TabBar, TabSeg};

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

/// Chrome text: a tab title is a glyphon text area, which the CPU raster
/// draws through the text compositor.
#[test]
fn chrome_text_draws_the_same_on_the_cpu_and_the_gpu() {
    let _serialized = gpu_test_guard();
    let (w, h) = (420, 200);
    let cfg = kettle_config::Config {
        show_titlebar: false,
        ..opaque()
    };
    let snap = snapshot_of(48, 6, TEXT);
    let mut tabs = TabBar::hidden();
    tabs.height = 36.0;
    tabs.segments.push(TabSeg {
        idx: 0,
        rect: (0.0, 0.0, 220.0, 36.0),
        title_rect: (8.0, 0.0, 170.0, 36.0),
        hint: None,
        close: (180.0, 0.0, 36.0, 36.0),
        title: "kettle: ~/src".into(),
        path: None,
        active: true,
        activity: TabActivity::Normal,
    });
    let frame = |backend| {
        let mut renderer =
            Renderer::headless_for_tests_on(backend, &cfg, w, h, 1.0, false).expect("builds")?;
        let mut view = pane(&snap, w, h - 36);
        view.rect = (0.0, 36.0, w as f32, (h - 36) as f32);
        view.terminal = view.rect;
        Some(capture_with_tabs(
            &mut renderer,
            &cfg,
            &[view],
            &focused(true),
            &tabs,
        ))
    };
    let cpu = frame(RendererBackend::Cpu).expect("a CPU renderer needs no adapter");
    let Some(gpu) = frame(RendererBackend::Gpu) else {
        no_gpu_adapter();
        return;
    };
    if let Some(dir) = std::env::var_os("KETTLE_CPU_PARITY_DUMP") {
        let dir = std::path::Path::new(&dir);
        cpu.save(dir.join("chrome-cpu.png")).expect("dump");
        gpu.save(dir.join("chrome-gpu.png")).expect("dump");
    }
    // The GPU drew the title: many pixels far from the tab's fill.
    let inked = |img: &image::RgbaImage, y0: u32, y1: u32| {
        let first = img.get_pixel(4, y0 + 2).0;
        (y0..y1)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let p = img.get_pixel(x, y).0;
                (0..3)
                    .map(|c| u32::from(p[c].abs_diff(first[c])))
                    .sum::<u32>()
                    > 60
            })
            .count()
    };
    assert!(inked(&gpu, 0, 36) > 40, "the GPU drew the tab title");
    let (worst, over) = difference(&cpu, &gpu);
    assert!(
        over == 0,
        "chrome: {over} channels differ by more than 2 (worst {worst})"
    );
}

/// The test config with an opaque background (the default is slightly
/// translucent).
fn opaque() -> kettle_config::Config {
    kettle_config::Config {
        background_opacity: 1.0,
        ..gpu_test_config()
    }
}

/// What the next frames on the fake surface do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum FakeMode {
    /// Commit each frame at once.
    #[default]
    Commit,
    /// Draw each frame and hold it, as a frame presented while the
    /// compositor's frame callback is out would be held.
    Defer,
    /// Draw each frame and report no change and no commit.
    Unchanged,
}

/// What the fake window surface saw, and how it behaves.
#[derive(Default)]
struct SurfaceLog {
    sizes: Vec<(u32, u32)>,
    scales: Vec<f64>,
    translucent: Vec<bool>,
    /// Each committed frame, as RGBA8.
    frames: Vec<Vec<u8>>,
    /// How many frames were drawn, committed or not.
    draws: usize,
    /// How many times the pre-commit hook ran, and whether every run came
    /// after its frame was drawn.
    pre_commits: usize,
    committed_after_draw: bool,
    occluded: Vec<bool>,
    released: usize,
    mode: FakeMode,
    /// The last commit's frame callback has not come.
    callback_out: bool,
    /// A drawn frame waits for a commit.
    held: bool,
    /// Buffers are this many pixels larger than the window on each axis,
    /// as an integer-scale Wayland buffer is rounded up.
    pad: u32,
}

/// A window surface in memory: one buffer.
struct FakeSurface {
    log: Arc<Mutex<SurfaceLog>>,
    size: (u32, u32),
    buffer: Vec<u32>,
    translucency: bool,
}

impl FakeSurface {
    fn extent(&self) -> (u32, u32) {
        let pad = self.log.lock().unwrap().pad;
        (self.size.0 + pad, self.size.1 + pad)
    }

    fn commit(&mut self, pre_commit: &mut dyn FnMut()) {
        let drawn = !self.buffer.contains(&0xdead_beef);
        pre_commit();
        let mut log = self.log.lock().unwrap();
        log.pre_commits += 1;
        log.committed_after_draw = drawn;
        log.frames.push(pixels_to_rgba8(&self.buffer));
        log.held = false;
        log.callback_out = true;
    }
}

impl CpuSurface for FakeSurface {
    fn resize(&mut self, width: u32, height: u32, scale: f64) {
        self.size = (width, height);
        let mut log = self.log.lock().unwrap();
        log.sizes.push(self.size);
        log.scales.push(scale);
    }

    fn buffer_size(&self) -> (u32, u32) {
        self.extent()
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
        if self.log.lock().unwrap().callback_out {
            return CpuPresentOutcome::Paced;
        }
        let (w, h) = self.extent();
        // A recycled buffer's old contents must not show through.
        self.buffer = vec![0xdead_beef; w as usize * h as usize];
        if !draw(&mut self.buffer, w, h) {
            self.log.lock().unwrap().held = false;
            return CpuPresentOutcome::NotDrawn;
        }
        let mode = {
            let mut log = self.log.lock().unwrap();
            log.draws += 1;
            log.mode
        };
        match mode {
            FakeMode::Commit => {
                self.commit(pre_commit);
                CpuPresentOutcome::Presented
            }
            FakeMode::Defer => {
                self.log.lock().unwrap().held = true;
                CpuPresentOutcome::Deferred
            }
            FakeMode::Unchanged => CpuPresentOutcome::Unchanged,
        }
    }

    fn flush(&mut self, pre_commit: &mut dyn FnMut()) -> HeldFrameFlush {
        let (held, callback_out) = {
            let log = self.log.lock().unwrap();
            (log.held, log.callback_out)
        };
        if !held {
            return HeldFrameFlush::Nothing;
        }
        if callback_out {
            return HeldFrameFlush::StillHeld;
        }
        self.commit(pre_commit);
        HeldFrameFlush::Presented
    }

    fn has_deferred(&self) -> bool {
        self.log.lock().unwrap().held
    }

    fn pending_deadline(&self) -> Option<Instant> {
        let log = self.log.lock().unwrap();
        log.held
            .then(|| Instant::now() + std::time::Duration::from_secs(1))
    }

    fn frame_pending(&mut self) -> bool {
        self.log.lock().unwrap().callback_out
    }

    fn frame_deadline(&self) -> Option<Instant> {
        let log = self.log.lock().unwrap();
        log.callback_out
            .then(|| Instant::now() + std::time::Duration::from_secs(1))
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
    assert_eq!(log.scales, [1.0, 1.0], "the presenter is given the scale");
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
    assert_eq!(
        renderer.flush_held_frame(|| panic!("nothing to commit")),
        HeldFrameFlush::Nothing
    );
    assert!(!renderer.cpu_frame_ready());
    assert_eq!(renderer.cpu_wake_deadline(), None);
}

/// Render one `w` x `h` frame of `views` and return its outcome.
fn frame_of(
    renderer: &mut Renderer,
    cfg: &kettle_config::Config,
    views: &[crate::PaneView<'_>],
    pre_present: impl FnOnce(),
) -> FrameOutcome {
    renderer
        .render_frame_with_status_and_pre_present(
            views,
            &TabBar::hidden(),
            cfg,
            &focused(true),
            &StatusBar::hidden(),
            pre_present,
        )
        .expect("a CPU window frame renders")
}

#[test]
fn a_held_frame_runs_pre_present_only_when_it_is_committed() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, false);
    log.lock().unwrap().mode = FakeMode::Defer;
    let notified = Arc::new(Mutex::new(0));
    let snap = snapshot_of(20, 4, b"hello");
    let counter = notified.clone();
    let outcome = frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], move || {
        *counter.lock().unwrap() += 1;
    });
    assert_eq!(
        outcome,
        FrameOutcome::Presented,
        "a held frame is drawn: its output is consumed"
    );
    assert_eq!(*notified.lock().unwrap(), 0, "nothing was committed");
    assert!(renderer.has_held_frame());
    assert!(
        renderer.cpu_wake_deadline().is_some(),
        "a held frame needs the loop to wake"
    );
    // The compositor has not taken the last frame: the frame stays held.
    log.lock().unwrap().callback_out = true;
    let counter = notified.clone();
    assert_eq!(
        renderer.flush_held_frame(move || *counter.lock().unwrap() += 1),
        HeldFrameFlush::StillHeld
    );
    assert_eq!(*notified.lock().unwrap(), 0);
    // The callback came: the flush commits it, running the hook once.
    log.lock().unwrap().callback_out = false;
    let counter = notified.clone();
    assert_eq!(
        renderer.flush_held_frame(move || *counter.lock().unwrap() += 1),
        HeldFrameFlush::Presented
    );
    assert_eq!(*notified.lock().unwrap(), 1);
    assert!(!renderer.has_held_frame());
    assert_eq!(
        renderer.flush_held_frame(|| panic!("nothing is held")),
        HeldFrameFlush::Nothing
    );
    let log = log.lock().unwrap();
    assert_eq!((log.draws, log.frames.len(), log.pre_commits), (1, 1, 1));
    assert!(log.committed_after_draw);
}

#[test]
fn painted_cards_move_only_when_a_frame_is_committed() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, false);
    renderer.resize(1200, 400);
    let (cards, snap, nonce) = crate::inline_cards::tests::fixture();
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    let painted = |renderer: &Renderer| {
        renderer
            .painted_cards()
            .iter()
            .map(|card| card.nonce)
            .collect::<Vec<_>>()
    };
    // Unchanged: nothing reached the screen, so nothing is painted.
    log.lock().unwrap().mode = FakeMode::Unchanged;
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[view], || panic!("no commit")),
        FrameOutcome::Presented
    );
    assert!(
        painted(&renderer).is_empty(),
        "an unchanged frame paints nothing"
    );
    // Deferred: drawn, but on screen only from the flush on.
    log.lock().unwrap().mode = FakeMode::Defer;
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[view], || panic!("no commit")),
        FrameOutcome::Presented
    );
    assert!(
        painted(&renderer).is_empty(),
        "a held frame's card is not on screen yet"
    );
    // A frame built while the compositor holds the last one is not drawn,
    // and does not stand in for the held one.
    log.lock().unwrap().callback_out = true;
    let empty = snapshot_of(20, 4, b"no card");
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&empty, 1200, 400)], || {
            panic!("no commit")
        }),
        FrameOutcome::Paced
    );
    log.lock().unwrap().callback_out = false;
    assert_eq!(renderer.flush_held_frame(|| {}), HeldFrameFlush::Presented);
    assert_eq!(
        painted(&renderer),
        [nonce],
        "the held frame's card is on screen once it is committed"
    );
    // Presented: what the frame painted is on screen at once.
    {
        let mut log = log.lock().unwrap();
        log.mode = FakeMode::Commit;
        log.callback_out = false;
    }
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&empty, 1200, 400)], || {}),
        FrameOutcome::Presented
    );
    assert!(painted(&renderer).is_empty());
}

#[test]
fn a_frame_is_not_drawn_until_the_compositor_takes_the_last_one() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, false);
    let snap = snapshot_of(20, 4, b"first");
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], || {}),
        FrameOutcome::Presented
    );
    assert!(log.lock().unwrap().callback_out);
    assert!(
        !renderer.cpu_frame_ready(),
        "nothing is waiting to be drawn"
    );
    assert_eq!(renderer.cpu_wake_deadline(), None);
    let snap = snapshot_of(20, 4, b"second");
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], || {
            panic!("a paced frame is never committed")
        }),
        FrameOutcome::Paced
    );
    assert_eq!(
        log.lock().unwrap().draws,
        1,
        "the paced frame was not drawn"
    );
    assert!(!renderer.cpu_frame_ready(), "the callback has not come");
    assert!(
        renderer.cpu_wake_deadline().is_some(),
        "the loop must wake for the callback"
    );
    log.lock().unwrap().callback_out = false;
    assert!(renderer.cpu_frame_ready(), "the window should redraw now");
    let snap = snapshot_of(20, 4, b"third");
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], || {}),
        FrameOutcome::Presented
    );
    assert!(!renderer.cpu_frame_ready());
    let log = log.lock().unwrap();
    assert_eq!((log.draws, log.frames.len()), (2, 2));
}

#[test]
fn a_screenshot_waits_for_the_frame_that_is_drawn() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, false);
    log.lock().unwrap().callback_out = true;
    let dir = kettle_test_support::private_tempdir("kettle-cpu-paced-");
    let (tx, rx) = std::sync::mpsc::channel();
    renderer
        .set_pending_screenshot(crate::ScreenshotRequest {
            out_path: dir.path().join("shot.png"),
            output_policy: crate::ScreenshotOutputPolicy::UserSelected,
            crop: None,
            completion: Some(tx),
            cancellation: None,
            recovery_wake: None,
        })
        .unwrap_or_else(|_| panic!("no capture pending"));
    let snap = snapshot_of(20, 4, b"hello");
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], || {}),
        FrameOutcome::Paced
    );
    assert!(
        rx.try_recv().is_err(),
        "a paced frame neither takes nor fails it"
    );
    assert!(renderer.pending_screenshot.is_some());
    log.lock().unwrap().callback_out = false;
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], || {}),
        FrameOutcome::Presented
    );
    let written = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the capture completes")
        .expect("the capture succeeds");
    assert_eq!(
        image::GenericImageView::dimensions(&image::open(written).expect("a PNG")),
        (200, 80)
    );
}

#[test]
fn a_buffer_rounded_past_the_window_repeats_its_edge() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, false);
    log.lock().unwrap().pad = 2;
    let snap = snapshot_of(20, 4, b"hello");
    assert_eq!(
        frame_of(&mut renderer, &cfg, &[pane(&snap, 200, 80)], || {}),
        FrameOutcome::Presented
    );
    let log = log.lock().unwrap();
    let frame = log.frames.last().expect("a frame");
    let (bw, bh) = (202usize, 82usize);
    assert_eq!(frame.len(), bw * bh * 4);
    let px = |x: usize, y: usize| &frame[(y * bw + x) * 4..][..4];
    for y in 0..bh {
        let row = y.min(79);
        for x in 200..bw {
            assert_eq!(px(x, y), px(199, row), "column {x}, row {y}");
        }
    }
    for x in 0..200 {
        assert_eq!(px(x, 81), px(x, 79), "row 81, column {x}");
    }
}

#[test]
fn a_scale_change_reaches_the_cpu_presenter() {
    let cfg = opaque();
    let (mut renderer, log) = window(&cfg, false);
    renderer.set_scale(2.0);
    let log = log.lock().unwrap();
    assert_eq!(log.sizes, [(200, 80), (200, 80)]);
    assert_eq!(log.scales, [1.0, 2.0]);
}

/// `kettle --screenshot` draws on the configured backend. Its scenes (tab
/// bar, split panes, grid and glyphon text, a context menu, a scrollbar, a
/// bell flash) agree between the two within 2 per channel.
#[test]
fn screenshot_scenes_draw_the_same_on_the_cpu_and_the_gpu() {
    use crate::DebugScene;
    use kettle_config::RendererPreference;
    let _serialized = gpu_test_guard();
    let dir = kettle_test_support::private_tempdir("kettle-cpu-capture-");
    for (name, scene) in [
        ("default", DebugScene::Default),
        ("menu", DebugScene::ContextMenu),
        ("scrollbar", DebugScene::Scrollbar),
        ("bell", DebugScene::BellFlash),
    ] {
        let shot = |renderer| {
            let cfg = kettle_config::Config {
                renderer,
                gpu_force_software: false,
                ..gpu_test_config()
            };
            let out = dir.path().join(format!("{name}-{renderer:?}.png"));
            crate::capture_png_with(&cfg, 60, 14, &out, scene)
                .map(|_| image::open(&out).expect("a PNG").to_rgba8())
        };
        let cpu = shot(RendererPreference::Cpu).expect("the CPU capture needs no adapter");
        let Ok(gpu) = shot(RendererPreference::Gpu) else {
            no_gpu_adapter();
            return;
        };
        if let Some(dump) = std::env::var_os("KETTLE_CPU_PARITY_DUMP") {
            let dump = std::path::Path::new(&dump);
            cpu.save(dump.join(format!("capture-{name}-cpu.png")))
                .expect("dump");
            gpu.save(dump.join(format!("capture-{name}-gpu.png")))
                .expect("dump");
        }
        // The menu's shadows stack translucent layers over text, where each
        // backend rounds every layer to 8 bits and lavapipe's sRGB encode is
        // up to 0.65 of a code off (A2's deep-stack finding): a few channels
        // may reach 3 or 4. Everything else stays within 2.
        let (worst, over) = difference(&cpu, &gpu);
        let channels = cpu.as_raw().len();
        assert!(
            worst <= 4 && over * 10_000 <= channels,
            "capture {name}: {over} of {channels} channels differ by more than 2 (worst {worst})"
        );
        let first = cpu.get_pixel(0, 0).0;
        assert!(cpu.pixels().any(|p| p.0 != first), "{name}: an empty frame");
    }
}

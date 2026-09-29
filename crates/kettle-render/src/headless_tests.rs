//! Frames rendered through the live frame path with no window.
//!
//! [`Renderer::headless_for_tests`] draws into the offscreen capture target,
//! so these tests see exactly what `kettle ctl screenshot` sees: real panes
//! from a real `Term`, through `build_pane`, the uploads and
//! `encode_scene_pass`.

use crate::gpu_tests::{gpu_test_config, gpu_test_guard};
use crate::*;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::Config as TermConfig;
use alacritty_terminal::vte::ansi::Processor;
use kettle_core::EventProxy;

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// A terminal that has been fed `bytes`, and its snapshot.
pub(crate) fn snapshot_of(cols: usize, rows: usize, bytes: &[u8]) -> PaneSnapshot {
    let (tx, _rx) = crossbeam_channel::unbounded();
    let proxy = EventProxy::new(tx, std::sync::Arc::new(|| {}));
    let mut term =
        alacritty_terminal::Term::new(TermConfig::default(), &Size { cols, rows }, proxy);
    Processor::<alacritty_terminal::vte::ansi::StdSyncHandler>::new().advance(&mut term, bytes);
    let mut snap = PaneSnapshot::default();
    snap.capture(&term);
    snap
}

/// One focused pane filling the whole target.
pub(crate) fn pane<'a>(snap: &'a PaneSnapshot, width: u32, height: u32) -> PaneView<'a> {
    PaneView {
        id: 1,
        rect: (0.0, 0.0, width as f32, height as f32),
        snap,
        focused: true,
        images: &[],
        title: "",
        title_prefix: "",
        title_path: None,
        size_cols: 0,
        size_rows: 0,
        bell: false,
        bell_flash: 0.0,
        group_name: None,
    }
}

/// Render one frame of `panes` with `overlay` and return the pixels the
/// offscreen capture wrote.
pub(crate) fn capture(
    renderer: &mut Renderer,
    cfg: &Config,
    panes: &[PaneView<'_>],
    overlay: &Overlay,
) -> image::RgbaImage {
    // The capture goes through the process-wide persistence pool.
    let _pool = crate::live_screenshot_tests::shared_persistence_pool_guard();
    let dir = kettle_test_support::private_tempdir("kettle-headless-");
    let out = dir.path().join("frame.png");
    let (tx, rx) = std::sync::mpsc::channel();
    renderer
        .set_pending_screenshot(ScreenshotRequest {
            out_path: out.clone(),
            output_policy: ScreenshotOutputPolicy::UserSelected,
            crop: None,
            completion: Some(tx),
            cancellation: None,
            recovery_wake: None,
        })
        .unwrap_or_else(|_| panic!("a capture was already pending"));
    let outcome = renderer
        .render_frame_with_status_and_pre_present(
            panes,
            &TabBar::hidden(),
            cfg,
            overlay,
            &StatusBar::hidden(),
            || {},
        )
        .expect("headless frame renders");
    // With no swapchain, the capture is the frame.
    assert!(matches!(outcome, FrameOutcome::Occluded));
    let written = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the capture completes")
        .expect("the capture succeeds");
    image::open(written)
        .expect("the capture is a PNG")
        .to_rgba8()
}

/// The overlay of a focused window with the cursor in the given blink phase.
pub(crate) fn focused(cursor_visible: bool) -> Overlay {
    Overlay {
        window_focused: true,
        cursor_visible,
        ..Overlay::default()
    }
}

/// Pixels painted exactly the cursor's block colour.
pub(crate) fn cursor_pixels(frame: &image::RgbaImage, cfg: &Config, snap: &PaneSnapshot) -> usize {
    let cursor = crate::color::cursor_block_color(&cfg.theme, &snap.colors);
    frame
        .pixels()
        .filter(|p| (p[0], p[1], p[2]) == (cursor.r, cursor.g, cursor.b))
        .count()
}

/// A headless renderer and its config, or `None` on a host with no GPU.
pub(crate) fn renderer(width: u32, height: u32) -> Option<(Renderer, Config)> {
    let cfg = gpu_test_config();
    let renderer =
        Renderer::headless_for_tests(&cfg, width, height).expect("headless renderer builds")?;
    Some((renderer, cfg))
}

#[test]
fn a_headless_frame_draws_a_real_pane() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, b"hello");
    let overlay = focused(true);
    let frame = capture(&mut renderer, &cfg, &[pane(&snap, 320, 120)], &overlay);
    assert_eq!(frame.dimensions(), (320, 120));
    let bg = cfg.theme.background;
    let ink = frame
        .pixels()
        .filter(|p| (p[0] as i16 - bg.r as i16).abs() + (p[1] as i16 - bg.g as i16).abs() > 60)
        .count();
    assert!(
        ink > 100,
        "expected text and cursor ink, found {ink} pixels"
    );
    // The block cursor after "hello" is a solid cell of the cursor colour.
    assert!(
        cursor_pixels(&frame, &cfg, &snap) > 100,
        "the block cursor is drawn"
    );
    if let Ok(dir) = std::env::var("KETTLE_HEADLESS_DUMP") {
        frame
            .save(std::path::Path::new(&dir).join("headless.png"))
            .expect("dump");
    }
}

/// The quad instance bytes the last frame uploaded.
fn uploaded_quads(renderer: &Renderer) -> Vec<u8> {
    bytemuck::cast_slice(&renderer.quad_scratch).to_vec()
}

/// A blink only changes what is drawn: the same quads are uploaded in both
/// phases and no text is prepared again, so a blinking window does no upload
/// or shaping work.
#[test]
fn a_blink_changes_no_uploaded_quads_and_prepares_no_text() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, b"hello");
    let panes = [pane(&snap, 320, 120)];
    let on = capture(&mut renderer, &cfg, &panes, &focused(true));
    let quads_on = uploaded_quads(&renderer);
    let prepares = renderer.text_prepares;
    let off = capture(&mut renderer, &cfg, &panes, &focused(false));
    assert_eq!(
        uploaded_quads(&renderer),
        quads_on,
        "a blink must not change the uploaded quads"
    );
    assert_eq!(
        renderer.text_prepares, prepares,
        "a blink must not prepare text"
    );
    assert!(
        cursor_pixels(&on, &cfg, &snap) > cursor_pixels(&off, &cfg, &snap) + 100,
        "the on phase shows the block and the off phase hides it"
    );
    let back_on = capture(&mut renderer, &cfg, &panes, &focused(true));
    assert_eq!(back_on, on, "the next on phase is the first one again");
    assert_eq!(renderer.text_prepares, prepares);
}

/// The off phase draws exactly what a cursor hidden with DECTCEM draws.
#[test]
fn the_off_phase_matches_a_hidden_cursor() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    for shape in ["", "\x1b[5 q", "\x1b[3 q"] {
        let shown = snapshot_of(20, 4, format!("{shape}hello").as_bytes());
        let hidden = snapshot_of(20, 4, format!("{shape}hello\x1b[?25l").as_bytes());
        let off = capture(
            &mut renderer,
            &cfg,
            &[pane(&shown, 320, 120)],
            &focused(false),
        );
        let dectcem = capture(
            &mut renderer,
            &cfg,
            &[pane(&hidden, 320, 120)],
            &focused(true),
        );
        assert_eq!(off, dectcem, "cursor shape {shape:?}");
    }
}

/// A wide glyph under the block keeps both cells covered in the on phase, and
/// the off phase restores the glyph in its normal colour.
#[test]
fn a_blink_over_a_wide_glyph_restores_the_glyph() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    // Park the cursor on the wide glyph.
    let snap = snapshot_of(20, 4, "漢字\x1b[1G".as_bytes());
    let hidden = snapshot_of(20, 4, "漢字\x1b[1G\x1b[?25l".as_bytes());
    let panes = [pane(&snap, 320, 120)];
    let on = capture(&mut renderer, &cfg, &panes, &focused(true));
    let off = capture(&mut renderer, &cfg, &panes, &focused(false));
    let dectcem = capture(
        &mut renderer,
        &cfg,
        &[pane(&hidden, 320, 120)],
        &focused(true),
    );
    assert!(
        cursor_pixels(&on, &cfg, &snap)
            > cursor_pixels(
                &capture(
                    &mut renderer,
                    &cfg,
                    &[pane(&snapshot_of(20, 4, b"ab\x1b[1G"), 320, 120)],
                    &focused(true)
                ),
                &cfg,
                &snap
            ),
        "the block over a wide glyph covers two cells"
    );
    assert_eq!(off, dectcem);
}

/// The blink phase reaches the renderer only at draw time: `build_pane` never
/// sees it, so it cannot change what is built or uploaded.
#[test]
fn the_blink_phase_reaches_only_the_draw() {
    let src = crate::production_source();
    let build_pane = src
        .split_once("    fn build_pane(")
        .expect("build_pane")
        .1
        .split_once("\n    fn ")
        .expect("end of build_pane")
        .0;
    assert!(
        !build_pane.contains("cursor_visible"),
        "build_pane must not depend on the blink phase"
    );
    assert_eq!(
        src.matches("overlay.cursor_visible").count(),
        2,
        "only the live and the capture scene passes read the phase"
    );
    let scene = src
        .split_once("    fn encode_scene_pass(")
        .expect("encode_scene_pass")
        .1
        .split_once("\n    fn ")
        .expect("end of encode_scene_pass")
        .0;
    assert!(scene.contains("self.quads.draw_hiding(&mut pass, hidden_cursor);"));
    assert!(
        !scene.contains("self.quads.draw(&mut pass)"),
        "the ordinary quads draw must honour the phase"
    );
    assert!(scene.contains("if cursor_on && self.pending_cursor_glyph.is_some()"));
}

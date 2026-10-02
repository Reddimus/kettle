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

/// A renderer compiles each distinct quad and image pipeline once: its five
/// quad layers draw with one blending and one replacing pipeline, and its three
/// image layers with one, instead of each layer compiling its own.
#[test]
fn a_renderer_compiles_each_distinct_pipeline_once() {
    let _serialized = gpu_test_guard();
    let Some((renderer, _cfg)) = renderer(64, 32) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let replacing = [&renderer.pane_bases, &renderer.live_pane_bases];
    let blending = [
        &renderer.quads,
        &renderer.overlay_quads,
        &renderer.menu_quads,
    ];
    let mut quads: Vec<&wgpu::RenderPipeline> = replacing
        .iter()
        .chain(&blending)
        .map(|layer| layer.pipeline())
        .collect();
    quads.sort();
    quads.dedup();
    assert_eq!(quads.len(), 2, "five quad layers, two blend modes");
    assert_eq!(replacing[0].pipeline(), replacing[1].pipeline());
    assert!(
        blending
            .iter()
            .all(|layer| layer.pipeline() == blending[0].pipeline())
    );
    let image_layers = [
        &renderer.imgs,
        &renderer.media_receipt_img,
        &renderer.bg_imgs,
    ];
    let mut images: Vec<&wgpu::RenderPipeline> =
        image_layers.iter().map(|layer| layer.pipeline()).collect();
    images.sort();
    images.dedup();
    assert_eq!(images.len(), 1, "three image layers, one pipeline");
    crate::gpu_tests::assert_shared_quad_pixels_match_standalone();
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
    let uploads = renderer.render_uploads();
    let off = capture(&mut renderer, &cfg, &panes, &focused(false));
    assert_eq!(
        uploaded_quads(&renderer),
        quads_on,
        "a blink must not change the uploaded quads"
    );
    let after = renderer.render_uploads();
    assert_eq!(
        (after.buffer_writes, after.texture_writes),
        (uploads.buffer_writes, uploads.texture_writes),
        "a blink writes nothing to the GPU: one write keeps Apple's blit pool resident"
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

/// A frame whose content did not change writes nothing to the GPU, and one
/// whose content did writes only what changed.
#[test]
fn a_steady_frame_writes_nothing_and_a_change_writes_only_the_difference() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, b"hello");
    let panes = [pane(&snap, 320, 120)];
    capture(&mut renderer, &cfg, &panes, &focused(true));
    let first = renderer.render_uploads();
    assert!(
        first.buffer_writes > 0,
        "the first frame uploads everything"
    );
    capture(&mut renderer, &cfg, &panes, &focused(true));
    let steady = renderer.render_uploads();
    assert_eq!(
        (
            steady.buffer_writes,
            steady.buffer_bytes,
            steady.texture_writes
        ),
        (
            first.buffer_writes,
            first.buffer_bytes,
            first.texture_writes
        ),
        "an unchanged frame writes nothing"
    );
    assert!(steady.skipped_writes > first.skipped_writes);
    assert_eq!(steady.text_prepares, first.text_prepares);

    let changed = snapshot_of(20, 4, b"hellp");
    let panes = [pane(&changed, 320, 120)];
    capture(&mut renderer, &cfg, &panes, &focused(true));
    let after = renderer.render_uploads();
    assert!(
        after.buffer_writes > steady.buffer_writes,
        "changed text still reaches the GPU"
    );
    assert!(
        after.buffer_bytes - steady.buffer_bytes < first.buffer_bytes,
        "only the difference is written, not the whole first frame again"
    );
}

/// Each pipeline, uploading the same data twice, writes nothing the second
/// time; the glyph pipeline, whose instances the renderer uploads only when
/// the grid changed, writes its instances but not its uniform.
#[test]
fn every_pipeline_skips_an_unchanged_upload() {
    let _serialized = gpu_test_guard();
    let cfg = gpu_test_config();
    let Some((device, queue)) = pollster::block_on(async {
        let (_instance, adapter) = crate::resolve_headless_adapter(&cfg, "upload-test")
            .await
            .ok()?;
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()
    }) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let screen = [64.0, 32.0];
    let writes = |counts: UploadCounts| counts.buffer_writes;

    let mut quads = QuadPipeline::new(&device, format);
    let quad = [QuadInstance {
        pos: [1.0, 1.0],
        size: [4.0, 4.0],
        color: [1.0, 0.0, 0.0, 1.0],
    }];
    quads.upload(&device, &queue, screen, &quad);
    let first = writes(quads.upload_counts());
    quads.upload(&device, &queue, screen, &quad);
    assert_eq!(writes(quads.upload_counts()), first, "quad");

    let mut outlines = OutlinePipeline::new(&device, format);
    let outline = [crate::outline::OutlineInstance {
        pos: [0.0, 0.0],
        size: [16.0, 8.0],
        color: [1.0; 4],
        border_width: 1.0,
        corner_radius: 2.0,
        corner_mask: 0b1100,
        _pad: 0,
    }];
    outlines.upload(&device, &queue, screen, &outline);
    let first = writes(outlines.upload_counts());
    outlines.upload(&device, &queue, screen, &outline);
    assert_eq!(writes(outlines.upload_counts()), first, "outline");

    let mut images = imgpipe::ImagePipeline::new(&device, format).expect("image pipeline");
    images.upload(&device, &queue, screen, &[]);
    let first = writes(images.upload_counts());
    images.upload(&device, &queue, screen, &[]);
    assert_eq!(writes(images.upload_counts()), first, "image");

    let mut stars = starfield::StarfieldPipeline::new(&device, format);
    stars.upload(&queue, screen, 3.0);
    let first = writes(stars.upload_counts());
    stars.upload(&queue, screen, 3.0);
    assert_eq!(writes(stars.upload_counts()), first, "a still starfield");
    stars.upload(&queue, screen, 3.5);
    assert_eq!(
        writes(stars.upload_counts()),
        first + 1,
        "a moving starfield"
    );

    let mut glyphs =
        GlyphPipeline::new_with_budget(&device, format, kettle_core::GraphicsBudget::default())
            .expect("glyph pipeline");
    let glyph = [<GlyphInstance as bytemuck::Zeroable>::zeroed()];
    glyphs.upload(&device, &queue, screen, &glyph);
    let first = writes(glyphs.upload_counts());
    glyphs.upload(&device, &queue, screen, &glyph);
    assert_eq!(
        writes(glyphs.upload_counts()),
        first + 1,
        "the glyph instances are written, the unchanged uniform is not"
    );
}

/// A pipeline reports the instances its last upload left on the GPU, which
/// the cursor patch reads to check the quads under it.
#[test]
fn a_pipeline_reports_what_it_uploaded() {
    let _serialized = gpu_test_guard();
    let cfg = gpu_test_config();
    let Some((device, queue)) = pollster::block_on(async {
        let (_instance, adapter) = crate::resolve_headless_adapter(&cfg, "uploaded-test")
            .await
            .ok()?;
        adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await
            .ok()
    }) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let quad = |x: f32| QuadInstance {
        pos: [x, 1.0],
        size: [4.0, 4.0],
        color: [1.0, 0.0, 0.0, 1.0],
    };
    let positions = |quads: &QuadPipeline| -> Vec<f32> {
        quads
            .uploaded()
            .expect("the retained copy covers the upload")
            .map(|quad| quad.pos[0])
            .collect()
    };
    let mut quads = QuadPipeline::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    quads.upload(&device, &queue, [64.0, 32.0], &[quad(1.0), quad(2.5)]);
    assert_eq!(positions(&quads), [1.0, 2.5]);
    // A prefix writes nothing but draws, and reports, only the prefix.
    quads.upload(&device, &queue, [64.0, 32.0], &[quad(1.0)]);
    assert_eq!(positions(&quads), [1.0]);
    quads.upload(&device, &queue, [64.0, 32.0], &[]);
    assert!(positions(&quads).is_empty());
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

/// A blank cell draws no ink, so a row is shaped only up to its last inked
/// cell. Recolouring the blanks after it (a prompt's padding, keyblock's
/// reverse-video block on blank rows) changes no row key, so no row is
/// reshaped and no text is prepared, while the new colours are still drawn.
#[test]
fn blanks_at_the_end_of_a_row_are_not_shaped() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let overlay = focused(true);
    // The two frames of each pair differ only in the colours of blanks after
    // the last inked cell of a row. The cursor ends on the same cell.
    let pairs: [(&str, &[u8], &[u8]); 2] = [
        // `hi`, then six blanks in reverse video, or plain.
        ("padding", b"hi\x1b[7m      \x1b[0m", b"hi      "),
        // Blank rows and a hidden cursor, then a reverse-video block in the
        // middle of row 1.
        (
            "keyblock",
            b"\x1b[?25l",
            b"\x1b[?25l\x1b[2;8H\x1b[7m    \x1b[0m\x1b[H",
        ),
    ];
    for (what, first, second) in pairs {
        let first_snap = snapshot_of(20, 4, first);
        let first_panes = [pane(&first_snap, 320, 120)];
        let first_frame = capture(&mut renderer, &cfg, &first_panes, &overlay);
        let keys = renderer.pane_line_keys[0].clone();
        let prepares = renderer.text_prepares;
        let second_snap = snapshot_of(20, 4, second);
        let second_panes = [pane(&second_snap, 320, 120)];
        let second_frame = capture(&mut renderer, &cfg, &second_panes, &overlay);
        assert_eq!(
            renderer.pane_line_keys[0], keys,
            "{what}: recolouring blanks after the ink must not reshape a row"
        );
        assert_eq!(
            renderer.text_prepares, prepares,
            "{what}: recolouring blanks after the ink must not prepare text"
        );
        assert_ne!(
            first_frame, second_frame,
            "{what}: the blanks' new colours are drawn, as cell backgrounds"
        );
    }

    // What is shaped: a row up to its ink and one blank, blanks between
    // inked cells in place, and nothing for a blank row.
    let snap = snapshot_of(20, 4, b"hi      \r\na    b");
    capture(&mut renderer, &cfg, &[pane(&snap, 320, 120)], &overlay);
    let rows: Vec<String> = renderer.pane_buffers[0]
        .lines
        .iter()
        .map(|line| line.text().to_string())
        .collect();
    assert_eq!(rows, ["hi ", "a    b ", "", ""]);
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
        3,
        "only the live and the capture scene passes read the phase, and the \
         frame records it for a cursor-layer hand-off"
    );
    assert!(src.contains("cursor_on: overlay.cursor_visible,"));
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

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
        terminal: (0.0, 0.0, width as f32, height as f32),
        titlebar: None,
        snap,
        inline_cards: None,
        tr: kettle_i18n::Translator::default(),
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
    capture_with_tabs(renderer, cfg, panes, overlay, &TabBar::hidden())
}

fn capture_with_tabs(
    renderer: &mut Renderer,
    cfg: &Config,
    panes: &[PaneView<'_>],
    overlay: &Overlay,
    tabs: &TabBar,
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
            tabs,
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

/// Distinct colours inside `rect` (x, y, width, height in frame pixels). A
/// painted label or glyph adds edge colours to its uniform background.
pub(crate) fn distinct_colors(frame: &image::RgbaImage, rect: [f32; 4]) -> usize {
    let x0 = rect[0].max(0.0) as u32;
    let y0 = rect[1].max(0.0) as u32;
    let x1 = ((rect[0] + rect[2]).max(0.0) as u32).min(frame.width());
    let y1 = ((rect[1] + rect[3]).max(0.0) as u32).min(frame.height());
    let mut colors = std::collections::HashSet::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let p = frame.get_pixel(x, y);
            colors.insert((p[0], p[1], p[2]));
        }
    }
    colors.len()
}

/// The rects of this frame's card labels of `kind`.
fn label_rects(renderer: &Renderer, kind: crate::card_scene::CardLabelKind) -> Vec<[f32; 4]> {
    renderer
        .card_scene
        .labels
        .iter()
        .filter(|label| label.kind == kind)
        .map(|label| label.rect)
        .collect()
}

/// A headless renderer and its config, or `None` on a host with no GPU.
pub(crate) fn renderer(width: u32, height: u32) -> Option<(Renderer, Config)> {
    let cfg = gpu_test_config();
    let renderer =
        Renderer::headless_for_tests(&cfg, width, height).expect("headless renderer builds")?;
    Some((renderer, cfg))
}

#[test]
fn returning_to_cached_wallpaper_rejects_an_obsolete_decode() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(32, 32) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.background_type = kettle_config::BackgroundType::Image;
    cfg.background_image = "A".into();
    cfg.background_blur = false;
    cfg.background_animation = kettle_config::BackgroundAnimation::Off;
    let image = kettle_core::ImageData::new(1, 1, vec![255; 4]).unwrap();
    let key = image.allocation_key();
    renderer.bg_image_cache = Some(BgImageAnim {
        path: "A".into(),
        blur: 0,
        frames: vec![image],
        opaque_frames: vec![true],
        gaps: vec![0],
        started: std::time::Instant::now(),
    });
    let (sender, _jobs) = std::sync::mpsc::sync_channel(1);
    let (results, receiver) = std::sync::mpsc::channel();
    renderer.bg_image_worker = Some(BgImageWorker {
        sender,
        receiver,
        busy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
    });
    renderer.bg_image_pending = Some(("B".into(), 0));
    assert!(
        results
            .send(BgImageResult {
                path: "B".into(),
                blur_radius: 0,
                frames: vec![BgFrame {
                    image: BgImage {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    },
                    gap_ms: 0,
                }],
            })
            .is_ok()
    );
    let _ = renderer
        .render_frame(&[], &TabBar::hidden(), &cfg, &focused(false))
        .expect("wallpaper frame renders");
    let cache = renderer.bg_image_cache.as_ref().unwrap();
    assert_eq!(cache.path, "A");
    assert_eq!(cache.frames[0].allocation_key(), key);
    assert!(renderer.bg_image_pending.is_none());
}

#[test]
fn bold_palette_resolution_honors_osc_overrides_and_preserves_truecolor() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.bold_is_bright = true;
    cfg.minimum_contrast = 1.0;
    cfg.theme.palette[1] = Rgb::new(210, 20, 30);
    cfg.theme.palette[9] = Rgb::new(20, 210, 30);
    let low = snapshot_of(20, 4, b"\x1b]4;9;rgb:12/34/56\x07\x1b[1;31mA");
    let bright = snapshot_of(20, 4, b"\x1b]4;9;rgb:12/34/56\x07\x1b[1;91mA");
    let expected = capture(
        &mut renderer,
        &cfg,
        &[pane(&bright, 320, 120)],
        &focused(false),
    );
    let actual = capture(
        &mut renderer,
        &cfg,
        &[pane(&low, 320, 120)],
        &focused(false),
    );
    assert_eq!(
        actual, expected,
        "bold red must resolve the overridden bright slot"
    );

    let rgb = cfg.theme.palette[1];
    let text = format!("\x1b[1;38;2;{};{};{}mA", rgb.r, rgb.g, rgb.b);
    let snap = snapshot_of(20, 4, text.as_bytes());
    cfg.bold_is_bright = false;
    let expected = capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(false),
    );
    cfg.bold_is_bright = true;
    let actual = capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 320, 120)],
        &focused(false),
    );
    assert_eq!(actual, expected, "bold must preserve explicit RGB colors");
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

/// Quad layers share blending and replacing pipelines. Image layers, including
/// the lazily admitted card poster, share one pipeline.
#[test]
fn a_renderer_compiles_each_distinct_pipeline_once() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(1200, 400) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    assert!(
        renderer.card_posters.is_none(),
        "text-only startup allocates no preview layer"
    );
    let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
    let poster = kettle_core::ImageData::new_with_budget(
        2,
        1,
        vec![250, 20, 60, 255, 250, 20, 60, 255],
        &kettle_core::GraphicsBudget::previews(),
    );
    cards.set_poster(nonce, poster.as_ref());
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    let pane_id = view.id;
    capture(&mut renderer, &cfg, &[view], &focused(false));
    let painted: Vec<_> = renderer
        .painted_cards()
        .iter()
        .map(|card| (card.pane, card.nonce))
        .collect();
    assert_eq!(
        painted,
        [(pane_id, nonce)],
        "the frame reports the card it accepted"
    );
    // A press inside the card's drawn rectangle is the card's.
    let card = renderer.painted_cards()[0];
    let [x, y, width, height] = card.rect;
    assert!(width > 0.0 && height > 0.0);
    assert_eq!(
        renderer.card_at(x + width / 2.0, y + height / 2.0),
        Some(card)
    );
    assert_eq!(renderer.card_at(x - 1.0, y), None);
    let replacing = [&renderer.pane_bases, &renderer.live_pane_bases];
    let blending = [
        &renderer.quads,
        &renderer.overlay_quads,
        &renderer.menu_quads,
        &renderer.card_base,
        &renderer.card_decoration,
        &renderer.card_cursors,
    ];
    let mut quads: Vec<&wgpu::RenderPipeline> = replacing
        .iter()
        .chain(&blending)
        .map(|layer| layer.pipeline())
        .collect();
    quads.sort();
    quads.dedup();
    assert_eq!(quads.len(), 2, "all quad layers share two blend modes");
    assert_eq!(replacing[0].pipeline(), replacing[1].pipeline());
    assert!(
        blending
            .iter()
            .all(|layer| layer.pipeline() == blending[0].pipeline())
    );
    let image_layers = [
        &renderer.imgs,
        renderer
            .card_posters
            .as_ref()
            .expect("actual poster creates its layer"),
        &renderer.media_receipt_img,
        &renderer.bg_imgs,
    ];
    let mut images: Vec<&wgpu::RenderPipeline> =
        image_layers.iter().map(|layer| layer.pipeline()).collect();
    images.sort();
    images.dedup();
    assert_eq!(images.len(), 1, "all image layers share one pipeline");
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
    for allow_mapping in [true, false] {
        let cfg = gpu_test_config();
        let Some(mut renderer) =
            Renderer::headless_for_tests_with_mapping(&cfg, 320, 120, 1.0, false, allow_mapping)
                .expect("headless renderer builds")
        else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        if !allow_mapping {
            assert!(
                !renderer.gpu.mapped_uploads(),
                "the forced queue device has no mapped uploads"
            );
        }
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
            (
                after.buffer_writes,
                after.texture_writes,
                after.mapped_writes
            ),
            (
                uploads.buffer_writes,
                uploads.texture_writes,
                uploads.mapped_writes
            ),
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
}

/// A frame whose content did not change writes nothing to the GPU, and one
/// whose content did writes only what changed.
#[test]
fn a_steady_frame_writes_nothing_and_a_change_writes_only_the_difference() {
    let _serialized = gpu_test_guard();
    for allow_mapping in [true, false] {
        let cfg = gpu_test_config();
        let Some(mut renderer) =
            Renderer::headless_for_tests_with_mapping(&cfg, 320, 120, 1.0, false, allow_mapping)
                .expect("headless renderer builds")
        else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        if !allow_mapping {
            assert!(
                !renderer.gpu.mapped_uploads(),
                "the forced queue device has no mapped uploads"
            );
        }
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
                steady.texture_writes,
                steady.mapped_writes
            ),
            (
                first.buffer_writes,
                first.buffer_bytes,
                first.texture_writes,
                first.mapped_writes
            ),
            "an unchanged frame writes nothing"
        );
        assert!(steady.skipped_writes > first.skipped_writes);
        assert_eq!(steady.text_prepares, first.text_prepares);

        let changed = snapshot_of(20, 4, b"hellp");
        let panes = [pane(&changed, 320, 120)];
        capture(&mut renderer, &cfg, &panes, &focused(true));
        let after = renderer.render_uploads();
        // On shared memory the instances go through mapped buffers rather than
        // the queue, so count both.
        let writes = |u: RenderUploads| u.buffer_writes + u.mapped_writes;
        let bytes = |u: RenderUploads| u.buffer_bytes + u.mapped_bytes;
        assert!(
            writes(after) > writes(steady),
            "changed text still reaches the GPU"
        );
        assert!(
            bytes(after) - bytes(steady) < bytes(first),
            "only the difference is written, not the whole first frame again"
        );
    }
}

/// On shared memory, output that changes the grid reaches the GPU without a
/// queue write: its glyph instances and quads go through mapped buffers, and
/// the screen uniforms did not change. On Apple GPUs each queue write is a
/// staging copy and a blit, and one per frame keeps the driver's blit pool
/// (about 128 MiB) resident for as long as a pane prints.
#[test]
fn printing_writes_nothing_through_the_queue_on_shared_memory() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let info = renderer.gpu.adapter.get_info();
    // The production policy decides, so a host it keeps on the queue
    // (Windows, a discrete GPU, GL) skips rather than fails.
    let shared = upload::mapped_upload_features(&renderer.gpu.adapter)
        .contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
    if !shared {
        eprintln!(
            "{} ({:?}, {:?}) keeps queue uploads; skipped",
            info.name, info.backend, info.device_type
        );
        return;
    }
    assert!(
        renderer.gpu.mapped_uploads(),
        "the shared-memory device enabled mapped uploads"
    );
    let first = snapshot_of(20, 4, b"line 1\r\n");
    capture(
        &mut renderer,
        &cfg,
        &[pane(&first, 320, 120)],
        &focused(true),
    );
    let before = renderer.render_uploads();
    // A new line: new glyph instances, and the cursor quad moves down.
    let printed = snapshot_of(20, 4, b"line 1\r\nline 2\r\n");
    capture(
        &mut renderer,
        &cfg,
        &[pane(&printed, 320, 120)],
        &focused(true),
    );
    let after = renderer.render_uploads();
    assert_eq!(
        after.buffer_writes, before.buffer_writes,
        "printing wrote through the queue: {before:?} then {after:?}"
    );
    assert_eq!(after.chrome_prepares, before.chrome_prepares);
    assert!(after.mapped_writes > before.mapped_writes);
}

/// A device with the mapping feature, regardless of adapter type, for ring
/// correctness tests. The live path still excludes discrete adapters.
fn mapped_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let cfg = gpu_test_config();
    pollster::block_on(async {
        let (_instance, adapter) = crate::resolve_headless_adapter(&cfg, "mapped-ring-test")
            .await
            .ok()?;
        let required_features = wgpu::Features::MAPPABLE_PRIMARY_BUFFERS;
        if !adapter.features().contains(required_features) {
            return None;
        }
        adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features,
                ..Default::default()
            })
            .await
            .ok()
    })
}

/// Draw `quads` over black into a `size`-square sRGB target and read the
/// pixels back, tightly packed RGBA. The wait for the readback also lets the
/// ring's map of the buffer this frame replaced complete.
fn draw_quads(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    quads: &QuadPipeline,
    size: u32,
) -> Vec<u8> {
    draw_quads_hiding(device, queue, quads, size, None)
}

fn draw_quads_hiding(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    quads: &QuadPipeline,
    size: u32,
    hidden: Option<std::ops::Range<u32>>,
) -> Vec<u8> {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-ring-target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // Rows are padded to wgpu's 256-byte copy alignment.
    let row = 256_u32;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-ring-readback"),
        size: u64::from(row) * u64::from(size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-ring-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        quads.draw_hiding(&mut pass, hidden);
    }
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(size),
            },
        },
        wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv()
        .expect("the readback's map callback ran")
        .expect("the readback maps");
    let data = slice.get_mapped_range().expect("the readback's bytes");
    let width = size as usize * 4;
    let mut pixels = Vec::with_capacity(width * size as usize);
    for y in 0..size as usize {
        pixels.extend_from_slice(&data[y * row as usize..][..width]);
    }
    drop(data);
    readback.unmap();
    pixels
}

/// On a device with mapped uploads, changing quads reach the GPU through the
/// ring, not the queue, and each frame draws its own data. The middle frame
/// is shorter than the others, so a ring that remembered the first frame's
/// tail as still held would skip the last frame's write and draw a stale
/// buffer.
#[test]
fn a_mapped_ring_draws_each_frame_without_queue_writes() {
    let _serialized = gpu_test_guard();
    let Some((device, queue)) = mapped_device() else {
        eprintln!("no adapter with mapped uploads on this host; skipped");
        return;
    };
    let size = 16_u32;
    let screen = [size as f32, size as f32];
    let quad = |x: f32, color: [f32; 4]| QuadInstance {
        pos: [x, 0.0],
        size: [8.0, 16.0],
        color,
    };
    let black = [0.0, 0.0, 0.0, 1.0];
    let (red, green, blue) = (
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );
    let frames = [
        (vec![quad(0.0, red), quad(8.0, green)], [red, green]),
        (vec![quad(0.0, blue)], [blue, black]),
        (vec![quad(0.0, blue), quad(8.0, green)], [blue, green]),
    ];
    let mut quads = QuadPipeline::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
    for (index, (data, expected)) in frames.into_iter().enumerate() {
        quads.upload(&device, &queue, screen, &data);
        let pixels = draw_quads(&device, &queue, &quads, size);
        for (x, color) in [(4_usize, expected[0]), (12, expected[1])] {
            let at = (8 * size as usize + x) * 4;
            let want: Vec<u8> = color[..3].iter().map(|c| (c * 255.0) as u8).collect();
            assert_eq!(pixels[at..at + 3], want[..], "frame {index}, x {x}");
        }
    }
    let counts = quads.upload_counts();
    assert_eq!(
        counts.buffer_writes, 1,
        "only the screen uniform went through the queue"
    );
    assert_eq!(counts.mapped_writes, 3, "every frame's quads were mapped");
}

/// A refused spare allocation still uploads into the fitting current
/// buffer, both for glyph-like writes and quad-like retained writes.
#[test]
fn a_refused_spare_keeps_the_current_buffer_drawing() {
    let _serialized = gpu_test_guard();
    let Some((device, queue)) = mapped_device() else {
        eprintln!("no adapter with mapped uploads on this host; skipped");
        return;
    };
    for retain in [false, true] {
        let budget = kettle_core::GraphicsBudget::default();
        let mut ring = upload::MappedRing::new(
            "refused-spare-test",
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_SRC,
            Some(budget.clone()),
        );
        let counters = upload::UploadCounters::default();
        let mut held = upload::RetainedBytes::default();
        assert!(ring.write(
            &device,
            &queue,
            retain.then_some(&mut held),
            bytemuck::bytes_of(&1_u32),
            &counters,
        ));
        assert_eq!(ring.current().unwrap().size(), 4096);
        // Consume the remaining scope budget with reservations, without
        // allocating textures or large buffers on the device.
        let limits = budget.limits();
        let mut remaining = limits.retained_bytes - 4096;
        let mut charges = Vec::new();
        while remaining > 0 {
            let bytes = remaining.min(limits.image_bytes);
            charges.push(budget.reserve_gpu(bytes).expect("remaining scope budget"));
            remaining -= bytes;
        }
        assert!(
            budget.reserve_gpu(4096).is_none(),
            "a spare cannot be allocated"
        );
        assert!(
            ring.write(
                &device,
                &queue,
                retain.then_some(&mut held),
                bytemuck::bytes_of(&2_u32),
                &counters,
            ),
            "a fitting update must keep drawing, retain={retain}"
        );
        let counts = counters.snapshot();
        assert_eq!((counts.mapped_writes, counts.buffer_writes), (1, 1));
        assert_eq!(counts.buffer_bytes, 4);

        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("refused-spare-readback"),
            size: 4,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_buffer_to_buffer(ring.current().unwrap(), 0, &readback, 0, 4);
        queue.submit(std::iter::once(encoder.finish()));
        let (tx, rx) = std::sync::mpsc::channel();
        readback.map_async(wgpu::MapMode::Read, .., move |result| {
            tx.send(result).unwrap();
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("readback poll");
        rx.recv().unwrap().expect("readback map");
        let view = readback.get_mapped_range(..).expect("readback bytes");
        assert_eq!(&view[..], bytemuck::bytes_of(&2_u32));
        drop(view);
        readback.unmap();

        // Growth cannot use the smaller current buffer. It must fail
        // without an oversized queue write or a false retained-data update.
        let oversized = vec![0_u8; 8192];
        assert!(!ring.write(
            &device,
            &queue,
            retain.then_some(&mut held),
            &oversized,
            &counters,
        ));
        assert_eq!(counters.snapshot(), counts);
        assert_eq!(ring.current().unwrap().size(), 4096);
        if retain {
            assert!(ring.write(
                &device,
                &queue,
                Some(&mut held),
                bytemuck::bytes_of(&2_u32),
                &counters,
            ));
            assert_eq!(counters.snapshot().buffer_writes, counts.buffer_writes);
        }
        drop(charges);
    }
}

/// The glyph ring takes more instances than its first buffer holds.
#[test]
fn the_glyph_ring_outgrows_its_first_buffer() {
    let _serialized = gpu_test_guard();
    let Some((device, queue)) = mapped_device() else {
        eprintln!("no adapter with mapped uploads on this host; skipped");
        return;
    };
    let mut glyphs = GlyphPipeline::new_with_budget(
        &device,
        wgpu::TextureFormat::Rgba8UnormSrgb,
        kettle_core::GraphicsBudget::default(),
    )
    .expect("glyph pipeline");
    let screen = [64.0, 32.0];
    let glyph = <GlyphInstance as bytemuck::Zeroable>::zeroed();
    glyphs.upload(&device, &queue, screen, &[glyph]);
    let size = std::mem::size_of::<GlyphInstance>() as u64;
    assert!(
        100 * size > 4096,
        "100 instances outgrow the first 4 KiB buffer"
    );
    glyphs.upload(&device, &queue, screen, &[glyph; 100]);
    let counts = glyphs.upload_counts();
    assert_eq!(
        (
            counts.buffer_writes,
            counts.mapped_writes,
            counts.mapped_bytes
        ),
        (1, 2, 101 * size),
        "the uniform once through the queue, both instance sets mapped"
    );
}

#[test]
fn quad_upload_batches_at_the_device_limit_and_recovers_after_shrinking() {
    let _serialized = gpu_test_guard();
    let cfg = gpu_test_config();
    for mapped in [false, true] {
        let Some((device, queue)) = pollster::block_on(async {
            let (_instance, adapter) = crate::resolve_headless_adapter(&cfg, "quad-limit-test")
                .await
                .ok()?;
            let features = if mapped {
                wgpu::Features::MAPPABLE_PRIMARY_BUFFERS
            } else {
                wgpu::Features::empty()
            };
            if !adapter.features().contains(features) {
                return None;
            }
            let device = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    required_features: features,
                    required_limits: wgpu::Limits {
                        max_buffer_size: 128 * 1024,
                        ..live_device_limits(adapter.limits())
                    },
                    ..Default::default()
                })
                .await
                .expect("device with a small buffer limit");
            Some(device)
        }) else {
            eprintln!("no GPU adapter for mapped={mapped}; skipped");
            continue;
        };
        if mapped {
            let mut ring = upload::MappedRing::new("limit-test", wgpu::BufferUsages::VERTEX, None);
            let counters = upload::UploadCounters::default();
            let too_large = vec![0; 128 * 1024 + 4];
            assert!(!ring.write(&device, &queue, None, &too_large, &counters));
            assert!(ring.current().is_none());
            assert!(ring.write(&device, &queue, None, &[0; 32], &counters));
        }
        let red = QuadInstance {
            pos: [0.0, 0.0],
            size: [16.0, 16.0],
            color: [1.0, 0.0, 0.0, 1.0],
        };
        let green = QuadInstance {
            color: [0.0, 1.0, 0.0, 1.0],
            ..red
        };
        let mut data = vec![red; 4097];
        data[4096] = green;
        let mut quads = QuadPipeline::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        quads.upload(&device, &queue, [16.0, 16.0], &data);
        assert_eq!(quads.count, 4097);
        assert_eq!(quads.uploaded().unwrap().count(), 4097);
        assert_eq!(
            &draw_quads(&device, &queue, &quads, 16)[..4],
            &[0, 255, 0, 255]
        );
        assert_eq!(
            &draw_quads_hiding(&device, &queue, &quads, 16, Some(4095..4097))[..4],
            &[255, 0, 0, 255],
        );
        assert_eq!(
            &draw_quads_hiding(&device, &queue, &quads, 16, Some(0..4096))[..4],
            &[0, 255, 0, 255],
        );
        quads.upload(&device, &queue, [16.0, 16.0], &[red]);
        assert_eq!(quads.count, 1);
        assert_eq!(
            &draw_quads(&device, &queue, &quads, 16)[..4],
            &[255, 0, 0, 255]
        );
    }
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
        .split_once("fn build_pane(")
        .expect("build_pane")
        .1
        .split_once("\n    }\n")
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
        .split_once("fn encode_scene_pass(")
        .expect("encode_scene_pass")
        .1
        .split_once("\n    }\n")
        .expect("end of encode_scene_pass")
        .0;
    assert!(scene.contains("self.quads.draw_hiding(&mut pass, hidden_cursor);"));
    assert!(
        !scene.contains("self.quads.draw(&mut pass)"),
        "the ordinary quads draw must honour the phase"
    );
    assert!(scene.contains("if cursor_on && self.pending_cursor_glyph.is_some()"));
}

#[test]
fn combining_marks_do_not_move_the_following_grid_glyph() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    for (plain, marked) in [
        ("ex", "e\u{301}x"),
        ("ex", "e\u{301}\u{308}x"),
        ("\u{4f60}x", "\u{4f60}\u{301}x"),
    ] {
        let snap = snapshot_of(20, 4, plain.as_bytes());
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        let expected = renderer.glyph_instances.last().unwrap().pos;
        let snap = snapshot_of(20, 4, marked.as_bytes());
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        assert_eq!(
            renderer.glyph_instances.last().unwrap().pos,
            expected,
            "the x after {marked:?} must stay on its engine column"
        );
    }
}

/// Pane text in the default grid mode is drawn by the glyph pipeline, not
/// glyphon, so output that changes only pane text prepares no glyphon text. A
/// prepare would rewrite the chrome's unchanged vertices through the queue on
/// every frame of output.
#[test]
fn grid_output_prepares_no_glyphon_text() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    assert_eq!(
        cfg.text_renderer,
        TextRendererMode::Grid,
        "the default mode"
    );
    let first = snapshot_of(20, 4, b"hello");
    let before = capture(
        &mut renderer,
        &cfg,
        &[pane(&first, 320, 120)],
        &focused(true),
    );
    let prepares = renderer.text_prepares;
    let chrome_prepares = renderer.render_uploads().chrome_prepares;
    assert!(
        chrome_prepares >= 2,
        "the first frame prepares main and menu"
    );
    // The cursor stays on the same blank cell, so only pane text changes.
    let printed = snapshot_of(20, 4, b"hellp");
    let after = capture(
        &mut renderer,
        &cfg,
        &[pane(&printed, 320, 120)],
        &focused(true),
    );
    assert_eq!(
        renderer.text_prepares, prepares,
        "grid output must not prepare glyphon text"
    );
    assert_eq!(renderer.render_uploads().chrome_prepares, chrome_prepares);
    assert_ne!(after, before, "the new text is still drawn");
}

#[test]
fn block_cursor_uses_the_pane_baseline_with_scaled_rows() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 160) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let mut snap = snapshot_of(20, 4, b"W\r");
    snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
    for height in [2.0, 3.0] {
        cfg.cell_height = height;
        renderer.set_cell_scale(1.0, height);
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 160)],
            &focused(true),
        );
        assert_eq!(
            renderer.cursor_glyph_buffer.metrics(),
            renderer.pane_buffers[0].metrics()
        );
        let cursor_y = renderer
            .cursor_glyph_buffer
            .layout_runs()
            .next()
            .unwrap()
            .line_y;
        let pane_y = renderer.pane_buffers[0]
            .layout_runs()
            .next()
            .unwrap()
            .line_y;
        assert_eq!(cursor_y, pane_y);
        assert_eq!(
            renderer.last_cursor_glyph_key,
            cursor_glyph_damage_key(
                renderer.pending_cursor_glyph.as_ref(),
                pane_metrics(renderer.metrics.font_size, renderer.cell_h),
                &renderer.font_family,
            ),
        );
    }
}

#[test]
fn block_cursor_preserves_cell_font_attributes_and_invalidates_style() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.font_family_bold_italic = Some("monospace".into());
    let mut previous_key = None;
    for (input, bold, italic) in [
        (b"W\r".as_slice(), false, false),
        (b"\x1b[1;3mW\r".as_slice(), true, true),
        (b"\x1b[3mW\r".as_slice(), false, true),
    ] {
        let mut snap = snapshot_of(20, 4, input);
        snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        let attrs = renderer.cursor_glyph_buffer.lines[0]
            .attrs_list()
            .get_span(0);
        let color = renderer.pending_cursor_glyph.as_ref().unwrap().color;
        let expected = run_attrs(&cfg, &font_features(&cfg), color, bold, italic);
        assert_eq!(attrs.weight, expected.weight);
        assert_eq!(attrs.style, expected.style);
        assert_eq!(attrs.family, expected.family);
        assert_eq!(attrs.font_features, expected.font_features);
        if previous_key.is_some() {
            assert_ne!(renderer.last_cursor_glyph_key, previous_key);
        }
        previous_key = renderer.last_cursor_glyph_key;
    }
}

fn confirmation_overlay() -> Overlay {
    Overlay {
        confirm_dialog: Some(ConfirmDialogOverlay {
            heading: "Close this window?".into(),
            body:
                "Both panes will close. Commands may be interrupted and unsaved work may be lost."
                    .into(),
            buttons: vec![
                ConfirmDialogButton {
                    label: "Cancel".into(),
                    destructive: false,
                },
                ConfirmDialogButton {
                    label: "Close window".into(),
                    destructive: true,
                },
            ],
            focus_idx: 0,
            hover_idx: None,
            pressed_idx: None,
            scroll: 0,
        }),
        ..focused(true)
    }
}

#[test]
fn confirmation_uses_chrome_metrics_and_separate_text_roles() {
    let _serialized = gpu_test_guard();
    let mut cfg = gpu_test_config();
    cfg.cell_width = 0.5;
    cfg.cell_height = 0.5;
    let Some(mut renderer) =
        Renderer::headless_for_tests_with(&cfg, 800, 600, 1.25, false).unwrap()
    else {
        return;
    };
    let snap = snapshot_of(80, 24, b"terminal context");
    let overlay = confirmation_overlay();
    capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let layout = ConfirmDialogLayout::new(
        (800.0, 600.0),
        (
            renderer.overlay_text_cell_width(),
            renderer.overlay_text_line_height(),
        ),
        renderer.scale,
        overlay.confirm_dialog.as_ref().unwrap(),
    );
    assert!(layout.heading_font > layout.body_font);
    assert!(layout.body_font >= 14.0 * renderer.scale);
    assert_eq!(layout.buttons.len(), 2);
    assert!(renderer.confirmation_text.key()[0].contains("Close this window?"));
    assert_eq!(renderer.confirmation_text.key()[2], "Cancel");
    assert_eq!(renderer.confirmation_text.key()[3], "Close window");
}

#[test]
fn confirmation_preserves_settings_below_opaque_panel() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(800, 600) else {
        return;
    };
    let snap = snapshot_of(80, 24, b"terminal context");
    let mut overlay = confirmation_overlay();
    let plain = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let layout = ConfirmDialogLayout::new(
        (800.0, 600.0),
        (
            renderer.overlay_text_cell_width(),
            renderer.overlay_text_line_height(),
        ),
        renderer.scale,
        overlay.confirm_dialog.as_ref().unwrap(),
    );
    overlay.settings = Some(SettingsOverlay {
        categories: vec!["Keys".into()],
        label_cols: 20,
        value_cols: 20,
        active_category: 0,
        background_page: false,
        rows: vec![SettingsRow {
            label: "A conflicting key binding".into(),
            value: "Ctrl+W".into(),
            disabled: false,
        }],
        focused_row: 0,
        title: "Settings: Keys".into(),
        hints: "Settings keyboard hints".into(),
        footer_note: None,
    });
    let raised = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let (x, y, w, h) = layout.rect;
    for py in (y + 8.0 * layout.unit).ceil() as u32..(y + h - 8.0 * layout.unit).floor() as u32 {
        for px in (x + 8.0 * layout.unit).ceil() as u32..(x + w - 8.0 * layout.unit).floor() as u32
        {
            assert_eq!(
                plain.get_pixel(px, py),
                raised.get_pixel(px, py),
                "Settings must not cover the opaque modal interior"
            );
        }
    }
    assert_ne!(
        plain, raised,
        "Settings context must remain visible outside the dialog"
    );
}

#[test]
fn confirmation_retains_uploads_and_clears_text_when_closed() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(800, 600) else {
        return;
    };
    let snap = snapshot_of(80, 24, b"terminal context");
    let overlay = confirmation_overlay();
    let shown = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let uploads = renderer.rounded_surfaces.upload_counts();
    let prepares = renderer.chrome_prepares;
    let repeated = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    assert_eq!(shown, repeated);
    let repeated_uploads = renderer.rounded_surfaces.upload_counts();
    assert_eq!(uploads.buffer_writes, repeated_uploads.buffer_writes);
    assert_eq!(uploads.buffer_bytes, repeated_uploads.buffer_bytes);
    assert!(repeated_uploads.skipped_writes > uploads.skipped_writes);
    assert_eq!(
        prepares, renderer.chrome_prepares,
        "idle dialog should retain shaped glyphs"
    );
    renderer.text_prepare_dirty = true;
    let before_forced = renderer.chrome_prepares;
    capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let active_prepares = renderer.chrome_prepares - before_forced;
    assert!(renderer.confirmation_labels_prepared);
    let closed = capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 800, 600)],
        &focused(true),
    );
    assert_ne!(closed, shown);
    assert!(!renderer.confirmation_labels_prepared);
    let closed_again = capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 800, 600)],
        &focused(true),
    );
    assert_eq!(
        closed, closed_again,
        "closed modal must clear its retained text vertices"
    );
    renderer.text_prepare_dirty = true;
    let before_closed_prepare = renderer.chrome_prepares;
    capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 800, 600)],
        &focused(true),
    );
    let inactive_prepares = renderer.chrome_prepares - before_closed_prepare;
    assert_eq!(
        active_prepares,
        inactive_prepares + 1,
        "a visible modal prepare is counted, and an already-cleared modal performs no empty prepare"
    );
}

#[test]
fn block_cursor_shapes_attached_marks_and_invalidates_their_cache() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    for (input, expected) in [
        ("e\u{301}\r", "e\u{301}"),
        ("e\u{308}\r", "e\u{308}"),
        (" \u{301}\r", " \u{301}"),
        ("\x1b[8me\u{301}\r", " "),
    ] {
        let mut snap = snapshot_of(20, 4, input.as_bytes());
        snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        assert_eq!(
            renderer.cursor_glyph_buffer.lines[0].text(),
            expected,
            "the cursor must shape the complete visible cell"
        );
    }
}

/// The same character under the cursor with and without emoji presentation
/// is two atlas bitmaps. Grid output no longer prepares the chrome, so when
/// the cursor's bitmap changes the chrome must be prepared with it: a 1-glyph
/// cursor prepare that grows or evicts the atlas would otherwise leave the
/// tab bar's and menus' cached vertices pointing at glyphs it replaced.
#[test]
fn a_cursor_glyph_changing_presentation_prepares_the_chrome() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    // U+263A alone is text presentation, one cell; with U+FE0F it is emoji
    // presentation, two cells. Either way the cursor steps back onto it.
    let plain = snapshot_of(20, 4, "\u{263A}\x1b[D".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&plain, 320, 120)],
        &focused(true),
    );
    capture(
        &mut renderer,
        &cfg,
        &[pane(&plain, 320, 120)],
        &focused(true),
    );
    let chrome = renderer.render_uploads().chrome_prepares;
    let qualified = snapshot_of(20, 4, "\u{263A}\u{FE0F}\x1b[2D".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&qualified, 320, 120)],
        &focused(true),
    );
    assert!(
        renderer.render_uploads().chrome_prepares > chrome,
        "a new cursor bitmap must prepare the chrome with it"
    );
}

/// Output that moves the cursor onto another cell with the same character can
/// still rasterize a new bitmap: at a fractional cell width the glyph lands
/// on another subpixel position. A visible cursor glyph therefore never
/// prepares alone, while a cursor on a blank cell (printing, typing at the end
/// of a line) leaves the chrome alone.
#[test]
fn output_moving_the_cursor_over_text_prepares_the_chrome() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let one = snapshot_of(20, 4, b"AA\x1b[D");
    capture(&mut renderer, &cfg, &[pane(&one, 320, 120)], &focused(true));
    capture(&mut renderer, &cfg, &[pane(&one, 320, 120)], &focused(true));
    let chrome = renderer.render_uploads().chrome_prepares;
    // The same character under the cursor, one cell further right.
    let two = snapshot_of(20, 4, b"AAA\x1b[D");
    capture(&mut renderer, &cfg, &[pane(&two, 320, 120)], &focused(true));
    assert!(
        renderer.render_uploads().chrome_prepares > chrome,
        "a moved visible cursor glyph must prepare the chrome with it"
    );
    // A blink keeps the cursor glyph's key, so it prepares nothing.
    let chrome = renderer.render_uploads().chrome_prepares;
    capture(&mut renderer, &cfg, &[pane(&two, 320, 120)], &focused(true));
    assert_eq!(renderer.render_uploads().chrome_prepares, chrome);
    // Whitespace is not proof of an empty bitmap: U+1680 OGHAM SPACE MARK
    // draws a stroke in a font that has it.
    let ogham = snapshot_of(20, 4, "\u{1680}\u{1680}\x1b[D".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&ogham, 320, 120)],
        &focused(true),
    );
    let chrome = renderer.render_uploads().chrome_prepares;
    let ogham = snapshot_of(20, 4, "\u{1680}\u{1680}\u{1680}\x1b[D".as_bytes());
    capture(
        &mut renderer,
        &cfg,
        &[pane(&ogham, 320, 120)],
        &focused(true),
    );
    assert!(
        renderer.render_uploads().chrome_prepares > chrome,
        "a moved non-blank whitespace glyph must prepare the chrome with it"
    );
}

/// Only legacy-mode pane text may force the glyphon prepare; hosts with no
/// GPU still check it here.
#[test]
fn only_legacy_pane_text_forces_a_glyphon_prepare() {
    let src = crate::production_source();
    let need = src
        .split_once("let need_prepare = self.text_prepare_dirty")
        .expect("need_prepare")
        .1
        .split_once(';')
        .expect("end of need_prepare")
        .0;
    assert!(
        !need.contains("any_pane_text_changed"),
        "grid output must not force a glyphon prepare"
    );
    assert!(src.contains("any_pane_text_changed && cfg.text_renderer == TextRendererMode::Legacy"));
}

/// An opaque paste receipt over the cursor cell must hide the whole terminal
/// cursor: its block quad and its inverted glyph.
#[test]
fn opaque_media_receipt_covers_the_terminal_cursor_inverted_glyph() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(640, 400) else {
        eprintln!("MEDIA_CURSOR_GPU_SKIPPED: no GPU adapter");
        return;
    };
    let receipt = MediaPasteReceiptOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (0.0, 0.0, 640.0, 400.0),
        grid_rect: (
            cfg.padding_x,
            cfg.padding_y,
            640.0 - cfg.padding_x * 2.0,
            400.0 - cfg.padding_y * 2.0,
        ),
        right_gutter: 0.0,
        image: Some(kettle_core::ImageData::solid(32, 32, [240, 70, 40, 255]).unwrap()),
        kind: MediaPasteReceiptKind::Image {
            original_width: 32,
            original_height: 32,
        },
        openable: true,
        remote: false,
        expanded: true,
        prefer_top: true,
    };
    let geometry = media_paste_receipt_geometry(
        &receipt,
        None,
        (renderer.cell_w, renderer.cell_h),
        renderer.overlay_text_cell_width(),
        renderer.metrics.line_height,
    )
    .expect("the receipt fits");
    let target = geometry.preview_rect.unwrap_or(geometry.rect);
    let col = ((target.0 + target.2 * 0.5 - cfg.padding_x) / renderer.cell_w).floor() as usize;
    let row = ((target.1 + target.3 * 0.5 - cfg.padding_y) / renderer.cell_h).floor() as usize;
    let x = cfg.padding_x + col as f32 * renderer.cell_w;
    let y = cfg.padding_y + row as f32 * renderer.cell_h;
    assert!(x > geometry.rect.0 && y > geometry.rect.1);
    assert!(x + renderer.cell_w < geometry.rect.0 + geometry.rect.2);
    assert!(y + renderer.cell_h < geometry.rect.1 + geometry.rect.3);
    let snap = snapshot_of(
        80,
        30,
        format!(
            "\x1b[{};{}HW\x1b[{};{}H",
            row + 1,
            col + 1,
            row + 1,
            col + 1
        )
        .as_bytes(),
    );
    let mut overlay = focused(false);
    overlay.media_paste_receipt = Some(receipt);
    let off = capture(&mut renderer, &cfg, &[pane(&snap, 640, 400)], &overlay);
    overlay.cursor_visible = true;
    let on = capture(&mut renderer, &cfg, &[pane(&snap, 640, 400)], &overlay);
    assert_eq!(
        renderer.pending_cursor_glyph.as_ref().map(|glyph| glyph.ch),
        Some('W'),
        "the hidden terminal cursor must contain an actual inverted glyph"
    );
    for py in geometry.rect.1.ceil() as u32..(geometry.rect.1 + geometry.rect.3).floor() as u32 {
        for px in geometry.rect.0.ceil() as u32..(geometry.rect.0 + geometry.rect.2).floor() as u32
        {
            assert_eq!(
                on.get_pixel(px, py),
                off.get_pixel(px, py),
                "terminal cursor leaked through opaque receipt at{px},{py}"
            );
        }
    }
    eprintln!("MEDIA_CURSOR_GPU_ACCEPTANCE: opaque receipt covers cursor quad and inverted glyph");
}

#[test]
fn unknown_inline_cluster_has_exact_owned_fallback_pixels_in_both_renderers() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: unknown fallback, no adapter");
            return;
        };
        cfg.text_renderer = mode;
        cfg.background_opacity = 1.0;
        let marker: String = std::iter::once('\u{10eeee}')
            .chain(std::iter::repeat_n('\u{0305}', 8))
            .collect();
        let mut unknown = snapshot_of(
            20,
            4,
            format!("\x1b[8;4;9;31;41m{marker}\x1b[0m").as_bytes(),
        );
        let mut expected = snapshot_of(20, 4, "\u{2b1a}".as_bytes());
        unknown.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Hidden;
        expected.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Hidden;
        let actual = capture(
            &mut renderer,
            &cfg,
            &[pane(&unknown, 320, 120)],
            &focused(false),
        );
        let background = &renderer.card_scene.base[0];
        let [x, y] = background.pos;
        let [width, height] = background.size;
        let wanted = capture(
            &mut renderer,
            &cfg,
            &[pane(&expected, 320, 120)],
            &focused(false),
        );
        assert!(
            actual == wanted,
            "owned fallback ignores raw marks and SGR in {mode:?}"
        );
        cfg.background_opacity = 0.35;
        let translucent = capture(
            &mut renderer,
            &cfg,
            &[pane(&unknown, 320, 120)],
            &focused(false),
        );
        let ordinary = capture(
            &mut renderer,
            &cfg,
            &[pane(&expected, 320, 120)],
            &focused(false),
        );
        let (left, right) = (x.ceil() as u32, (x + width).floor() as u32);
        let (top, bottom) = (y.ceil() as u32, (y + height).floor() as u32);
        assert!(
            left < right && top < bottom,
            "fallback has a visible cell interior"
        );
        let mut ordinary_is_translucent = false;
        // The owned cell remains opaque above terminal images even when the
        // rest of the pane lets the desktop show through.
        for row in top..bottom {
            for column in left..right {
                let pixel = translucent.get_pixel(column, row);
                ordinary_is_translucent |= ordinary.get_pixel(column, row)[3] < 255;
                assert_eq!(
                    pixel[3], 255,
                    "owned cell alpha at {column},{row} in {mode:?}"
                );
                assert_eq!(
                    pixel,
                    actual.get_pixel(column, row),
                    "owned cell pixels at {column},{row} in {mode:?}"
                );
            }
        }
        assert!(
            ordinary_is_translucent,
            "ordinary cells retain pane transparency"
        );
        let bg = cfg.theme.background;
        assert!(
            actual
                .pixels()
                .any(|pixel| pixel.0[0..3] != [bg.r, bg.g, bg.b]),
            "fallback must have visible ink"
        );
        // Pixel equality alone could compare two .notdef boxes. Verify the
        // same production font database really shapes the requested symbols.
        for symbol in ['\u{25a1}', '\u{2b1a}'] {
            let family = renderer.font_family.clone();
            let mut text = TextBuffer::new(&mut renderer.font_system, Metrics::new(16.0, 20.0));
            text.set_size(Some(80.0), Some(40.0));
            text.set_text(
                &symbol.to_string(),
                &Attrs::new().family(Family::Name(&family)),
                Shaping::Advanced,
                None,
            );
            text.shape_until_scroll(&mut renderer.font_system, false);
            let glyphs: Vec<_> = text
                .layout_runs()
                .flat_map(|run| {
                    run.glyphs
                        .iter()
                        .map(|glyph| (glyph.font_id, glyph.glyph_id))
                })
                .collect();
            assert_eq!(glyphs.len(), 1, "one owned symbol must shape as one glyph");
            let (font_id, glyph_id) = glyphs[0];
            assert_ne!(glyph_id, 0, "owned symbol {symbol:?} must not be .notdef");
            let font = renderer
                .font_system
                .get_font(font_id, Default::default())
                .expect("shaped glyph font remains loaded");
            assert_eq!(
                font.as_swash().charmap().map(symbol),
                glyph_id,
                "font must cover the requested symbol, not a replacement box"
            );
        }
        eprintln!("INLINE_CARD_GPU_ACCEPTANCE: unknown fallback {mode:?}");
    }
}

#[test]
fn fallback_cursor_blinks_above_its_background_without_raw_placeholder_ink() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: fallback cursor, no adapter");
            return;
        };
        cfg.text_renderer = mode;
        let marker: String = std::iter::once('\u{10eeee}')
            .chain(std::iter::repeat_n('\u{0305}', 8))
            .collect();
        let mut snap = snapshot_of(20, 4, format!("{marker}\r").as_bytes());
        snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
        let on = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        let off = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(false),
        );
        assert!(
            cursor_pixels(&on, &cfg, &snap) > cursor_pixels(&off, &cfg, &snap) + 20,
            "owned background must not cover its cursor in {mode:?}"
        );
        assert_eq!(
            renderer.pending_cursor_glyph.as_ref().unwrap().ch,
            '\u{2b1a}'
        );
        assert!(
            !renderer
                .pending_cursor_glyph
                .as_ref()
                .unwrap()
                .emoji_qualified
        );
        assert!(renderer.cursor_quad_range.is_none());
        assert!(renderer.card_cursor_quad_range.is_some());
        let again = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        assert_eq!(
            on.as_raw(),
            again.as_raw(),
            "blink returns to byte-identical pixels"
        );
        eprintln!("INLINE_CARD_GPU_ACCEPTANCE: fallback cursor {mode:?}");
    }
}

#[test]
fn registered_poster_overwrite_removes_tiles_and_badges_in_the_same_frame() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((mut renderer, mut cfg)) = renderer(1200, 400) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: registered overwrite, no adapter");
            return;
        };
        cfg.text_renderer = mode;
        let (mut cards, mut snap, nonce) = crate::inline_cards::tests::fixture();
        let poster = kettle_core::ImageData::new_with_budget(
            2,
            1,
            vec![250, 20, 60, 255, 250, 20, 60, 255],
            &kettle_core::GraphicsBudget::previews(),
        );
        cards.set_poster(nonce, poster.as_ref());
        snap.cursor.point = kettle_core::Point::new(kettle_core::Line(2), kettle_core::Column(8));
        snap.cursor.shape = alacritty_terminal::vte::ansi::CursorShape::Block;
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let first = capture(&mut renderer, &cfg, &[view], &focused(true));
        assert_eq!(renderer.card_scene.posters.len(), 1);
        assert_eq!(renderer.card_scene.labels.len(), 2);
        assert!(
            renderer.pending_cursor_glyph.is_none(),
            "accepted card suppresses its cursor glyph"
        );
        assert!(renderer.cursor_quad_range.is_none() && renderer.card_cursor_quad_range.is_none());
        let center = [
            cfg.padding_x + 11.0 * renderer.cell_w,
            cfg.padding_y + 2.5 * renderer.cell_h,
        ];
        let before = first.get_pixel(center[0] as u32, center[1] as u32);
        assert_eq!(&before.0[0..3], &[250, 20, 60]);
        let badges = [
            label_rects(&renderer, crate::card_scene::CardLabelKind::Brand),
            label_rects(
                &renderer,
                crate::card_scene::CardLabelKind::Harness(crate::CardHarness::ClaudeHook),
            ),
        ]
        .concat();
        assert_eq!(badges.len(), 2);
        for rect in &badges {
            assert!(distinct_colors(&first, *rect) > 1, "badge text is painted");
        }
        snap.cells
            .iter_mut()
            .find(|cell| (cell.line, cell.col) == (2, 8))
            .unwrap()
            .c = 'X';
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let after = capture(&mut renderer, &cfg, &[view], &focused(true));
        assert!(renderer.card_scene.posters.is_empty() && renderer.card_scene.labels.is_empty());
        assert_ne!(
            &after.get_pixel(center[0] as u32, center[1] as u32).0[0..3],
            &[250, 20, 60]
        );
        assert_eq!(renderer.pending_cursor_glyph.as_ref().unwrap().ch, 'X');
        for rect in &badges {
            assert_eq!(
                distinct_colors(&after, *rect),
                1,
                "no badge text survives the overwrite"
            );
        }
        eprintln!("INLINE_CARD_GPU_ACCEPTANCE: registered overwrite {mode:?}");
    }
}

#[test]
fn refused_card_poster_upload_paints_status_and_recovers_on_the_next_frame() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((mut renderer, mut cfg)) = renderer(1200, 400) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: upload refusal, no adapter");
            return;
        };
        cfg.text_renderer = mode;
        let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
        let budget = kettle_core::GraphicsBudget::previews();
        let poster = kettle_core::ImageData::new_with_budget(
            2,
            1,
            vec![250, 20, 60, 255, 250, 20, 60, 255],
            &budget,
        );
        cards.set_poster(nonce, poster.as_ref());
        // Exhaust accounting only; no large VRAM allocation or performance claim.
        let mut low = 0;
        let mut high = budget.limits().process_gpu_bytes;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if let Some(reservation) = budget.reserve_transient_gpu(middle) {
                drop(reservation);
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let exhausted = budget
            .reserve_transient_gpu(low)
            .expect("same serialized account");
        assert!(budget.reserve_transient_gpu(1).is_none());
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let unavailable = capture(&mut renderer, &cfg, &[view], &focused(false));
        assert!(
            renderer
                .card_posters
                .as_ref()
                .is_none_or(|posters| posters.drawn_item_indices().next().is_none())
        );
        assert_eq!(renderer.card_scene.labels.len(), 3);
        assert_eq!(renderer.painted_cards().len(), 1);
        assert_eq!(
            renderer.shown_cards().count(),
            0,
            "a refused poster leaves its card on screen without its image"
        );
        let status = label_rects(&renderer, crate::card_scene::CardLabelKind::Unavailable);
        assert_eq!(status.len(), 1);
        assert!(
            distinct_colors(&unavailable, status[0]) > 1,
            "the Unavailable status text is painted"
        );
        drop(exhausted);
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let recovered = capture(&mut renderer, &cfg, &[view], &focused(false));
        assert_eq!(
            renderer
                .card_posters
                .as_ref()
                .expect("recovered poster layer")
                .drawn_item_indices()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(renderer.card_scene.labels.len(), 2);
        assert_eq!(
            renderer.shown_cards().copied().collect::<Vec<_>>(),
            renderer.painted_cards().to_vec(),
            "the image shows once its poster is uploaded"
        );
        let center = [
            cfg.padding_x + 11.0 * renderer.cell_w,
            cfg.padding_y + 2.5 * renderer.cell_h,
        ];
        assert_eq!(
            &recovered.get_pixel(center[0] as u32, center[1] as u32).0[0..3],
            &[250, 20, 60]
        );
        assert_ne!(unavailable.as_raw(), recovered.as_raw());
        eprintln!("INLINE_CARD_GPU_ACCEPTANCE: upload refusal and recovery {mode:?}");
    }
}

#[test]
fn card_badge_labels_follow_configured_minimum_contrast_after_reload() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((mut renderer, mut cfg)) = renderer(1200, 400) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: label contrast, no adapter");
            return;
        };
        cfg.text_renderer = mode;
        cfg.theme.background = Rgb::new(20, 20, 20);
        cfg.theme.foreground = cfg.theme.background;
        cfg.minimum_contrast = 0.0;
        let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
        cards.set_poster(nonce, None);
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let low = capture(&mut renderer, &cfg, &[view], &focused(false));
        cfg.minimum_contrast = 4.5;
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let readable = capture(&mut renderer, &cfg, &[view], &focused(false));
        let bounds = [
            cfg.padding_x as u32,
            (cfg.padding_y + renderer.cell_h) as u32,
            (cfg.padding_x + 5.0 * renderer.cell_w) as u32,
            (cfg.padding_y + 3.0 * renderer.cell_h) as u32,
        ];
        let ink = |image: &image::RgbaImage| {
            (bounds[1]..bounds[3])
                .flat_map(|y| (bounds[0]..bounds[2]).map(move |x| (x, y)))
                .filter(|&(x, y)| image.get_pixel(x, y).0[0..3] != [20, 20, 20])
                .count()
        };
        assert_eq!(ink(&low), 0, "configured zero contrast remains literal");
        assert!(
            ink(&readable) > 20,
            "same retained badge now has readable ink in {mode:?}"
        );
        eprintln!("INLINE_CARD_GPU_ACCEPTANCE: label contrast reload {mode:?}");
    }
}

#[test]
fn registered_poster_real_scroll_keeps_partial_pixels_and_offscreen_overwrite_removes_them() {
    use alacritty_terminal::grid::Scroll;
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((mut renderer, mut cfg)) = renderer(1200, 400) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: partial scroll, no adapter");
            return;
        };
        cfg.text_renderer = mode;
        for upper_rows_offscreen in [true, false] {
            let (mut cards, mut term, nonce) =
                crate::inline_cards::tests::fixture_term(if upper_rows_offscreen { 0 } else { 6 });
            if upper_rows_offscreen {
                let mut processor: Processor = Processor::new();
                processor.advance(
                    &mut term,
                    b"\r\nordinary 0\r\nordinary 1\r\nordinary 2\r\nordinary 3\r\nordinary 4\r\nordinary 5",
                );
                term.scroll_display(Scroll::Bottom);
            } else {
                term.scroll_display(Scroll::Top);
            }
            let poster = kettle_core::ImageData::new_with_budget(
                2,
                1,
                vec![250, 20, 60, 255, 250, 20, 60, 255],
                &kettle_core::GraphicsBudget::previews(),
            );
            cards.set_poster(nonce, poster.as_ref());
            let mut snap = PaneSnapshot::default();
            snap.capture_with_card_marks(&term, true);
            let mut view = pane(&snap, 1200, 400);
            view.inline_cards = Some(&cards);
            let first = capture(&mut renderer, &cfg, &[view], &focused(false));
            assert_eq!(renderer.card_scene.posters.len(), 1);
            let visible_row = if upper_rows_offscreen { 0.5 } else { 7.5 };
            let center = [
                cfg.padding_x + 11.0 * renderer.cell_w,
                cfg.padding_y + visible_row * renderer.cell_h,
            ];
            assert_eq!(
                &first.get_pixel(center[0] as u32, center[1] as u32).0[0..3],
                &[250, 20, 60],
                "visible partial poster in {mode:?}, upper={upper_rows_offscreen}"
            );
            let mut frame = crate::inline_cards::CardFrame::default();
            cards.recognize_into(&snap, &mut frame);
            let block = &frame.blocks[0];
            let offscreen_line = if upper_rows_offscreen {
                block.line - 1
            } else {
                block.line + i32::from(block.rows)
            };
            let viewport_row = offscreen_line + snap.display_offset as i32;
            assert!(viewport_row < 0 || viewport_row >= snap.screen_lines as i32);
            let column = if upper_rows_offscreen {
                7
            } else {
                block.column
            };
            term.grid_mut()[kettle_core::Point::new(
                kettle_core::Line(offscreen_line),
                kettle_core::Column(column),
            )]
            .c = 'X';
            snap.capture_with_card_marks(&term, true);
            let mut view = pane(&snap, 1200, 400);
            view.inline_cards = Some(&cards);
            let after = capture(&mut renderer, &cfg, &[view], &focused(false));
            assert!(renderer.card_scene.posters.is_empty());
            assert!(renderer.card_scene.labels.is_empty());
            assert_ne!(
                &after.get_pixel(center[0] as u32, center[1] as u32).0[0..3],
                &[250, 20, 60],
                "offscreen context overwrite removes the poster in its next frame"
            );
        }
        eprintln!("INLINE_CARD_GPU_ACCEPTANCE: partial scroll and offscreen overwrite {mode:?}");
    }
}

#[test]
fn exhausted_preview_account_keeps_new_terminal_windows_and_recovers_cards() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((probe, mut cfg)) = renderer(1200, 400) else {
            eprintln!("INLINE_CARD_GPU_SKIPPED: new window with full previews, no adapter");
            return;
        };
        drop(probe);
        cfg.text_renderer = mode;
        let budget = kettle_core::GraphicsBudget::previews();
        // Reserve accounting only, preserving actual GPU capacity for text.
        let mut low = 0;
        let mut high = budget.limits().process_gpu_bytes;
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if let Some(reservation) = budget.reserve_transient_gpu(middle) {
                drop(reservation);
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let exhausted = budget
            .reserve_transient_gpu(low)
            .expect("serialized preview account");
        assert!(budget.reserve_transient_gpu(1).is_none());
        let mut renderer = Renderer::headless_for_tests(&cfg, 1200, 400)
            .expect("preview pressure must not prevent a terminal window")
            .expect("the already-probed adapter remains available");
        let snap = snapshot_of(80, 8, b"a new terminal remains usable");
        let plain = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 1200, 400)],
            &focused(false),
        );
        // The first text row, inside the padding, so the focused pane's
        // border cannot satisfy it.
        let text_row = [
            cfg.padding_x,
            cfg.padding_y,
            29.0 * renderer.cell_w,
            renderer.cell_h,
        ];
        assert!(
            distinct_colors(&plain, text_row) > 1,
            "terminal text is painted"
        );
        let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
        let poster = kettle_core::ImageData::new_with_budget(
            2,
            1,
            vec![250, 20, 60, 255, 250, 20, 60, 255],
            &budget,
        );
        cards.set_poster(nonce, poster.as_ref());
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let unavailable = capture(&mut renderer, &cfg, &[view], &focused(false));
        let status = label_rects(&renderer, crate::card_scene::CardLabelKind::Unavailable);
        assert_eq!(status.len(), 1);
        assert!(
            distinct_colors(&unavailable, status[0]) > 1,
            "the Unavailable status text is painted"
        );
        drop(exhausted);
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let recovered = capture(&mut renderer, &cfg, &[view], &focused(false));
        assert!(
            !renderer
                .card_scene
                .labels
                .iter()
                .any(|label| label.kind == crate::card_scene::CardLabelKind::Unavailable)
        );
        let center = [
            cfg.padding_x + 11.0 * renderer.cell_w,
            cfg.padding_y + 2.5 * renderer.cell_h,
        ];
        assert_eq!(
            &recovered.get_pixel(center[0] as u32, center[1] as u32).0[0..3],
            &[250, 20, 60]
        );
        assert_ne!(unavailable.as_raw(), recovered.as_raw());
        eprintln!(
            "INLINE_CARD_GPU_ACCEPTANCE: new window with full previews and recovery {mode:?}"
        );
    }
}

/// A quick-select chip over an inline card sits above the card's poster and
/// frame, so its label stays readable on any image.
#[test]
fn a_hint_chip_over_a_card_is_drawn_above_it() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(1200, 400) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
    let poster = kettle_core::ImageData::new_with_budget(
        2,
        1,
        vec![20, 200, 60, 255, 20, 200, 60, 255],
        &kettle_core::GraphicsBudget::previews(),
    );
    cards.set_poster(nonce, poster.as_ref());
    let place = cards.placements(&snap)[0];
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    let chip = cfg.search_background.unwrap_or(cfg.theme.palette[3]);
    let near_chip = |frame: &image::RgbaImage| {
        frame
            .pixels()
            .filter(|p| {
                [(p[0], chip.r), (p[1], chip.g), (p[2], chip.b)]
                    .iter()
                    .all(|(a, b)| a.abs_diff(*b) <= 12)
            })
            .count()
    };
    let plain = capture(&mut renderer, &cfg, &[view], &focused(false));
    assert_eq!(
        near_chip(&plain),
        0,
        "nothing chip-coloured before the hint"
    );
    let mut overlay = focused(false);
    overlay.hint_labels = vec![HintLabel {
        row: usize::try_from(place.line + i32::from(place.rows) / 2).unwrap(),
        col: place.column + usize::from(place.columns) / 2,
        label: "a".to_string(),
        dim: false,
    }];
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    let hinted = capture(&mut renderer, &cfg, &[view], &overlay);
    assert!(
        near_chip(&hinted) > 0,
        "the chip shows over the card's poster"
    );
}

/// A quick-select chip is cut to its pane like its label, so one at the
/// last column never paints over the next pane.
#[test]
fn a_hint_chip_stays_inside_its_pane() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(1200, 400) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(80, 20, b"");
    let left = pane(&snap, 600, 400);
    let mut right = pane(&snap, 600, 400);
    right.id = 2;
    right.rect = (600.0, 0.0, 600.0, 400.0);
    right.focused = false;
    // A two-letter label from the left pane's last whole column runs past
    // its edge.
    let col = ((600.0 - cfg.padding_x) / renderer.cell_w).floor() as usize - 1;
    let mut overlay = focused(false);
    overlay.hint_labels = vec![HintLabel {
        row: 1,
        col,
        label: "aa".to_string(),
        dim: false,
    }];
    let frame = capture(&mut renderer, &cfg, &[left, right], &overlay);
    let chip = cfg.search_background.unwrap_or(cfg.theme.palette[3]);
    let chip_pixels = |from: u32, to: u32| {
        (0..frame.height())
            .flat_map(|y| (from..to).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let p = frame.get_pixel(*x, *y);
                [(p[0], chip.r), (p[1], chip.g), (p[2], chip.b)]
                    .iter()
                    .all(|(a, b)| a.abs_diff(*b) <= 12)
            })
            .count()
    };
    assert!(chip_pixels(0, 600) > 0, "the chip shows in its pane");
    assert_eq!(chip_pixels(600, 1200), 0, "and nowhere in the next");
}

/// The card a click would open gets an accent outline over its frame, and
/// only while the UI names it.
#[test]
fn a_hovered_card_is_outlined_in_the_accent() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(1200, 400) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
    let poster = kettle_core::ImageData::new_with_budget(
        2,
        1,
        vec![20, 200, 60, 255, 20, 200, 60, 255],
        &kettle_core::GraphicsBudget::previews(),
    );
    cards.set_poster(nonce, poster.as_ref());
    let accent = renderer.ui_accent(&cfg, &cfg.theme);
    // The outermost whole pixels of the card's visible part, which the
    // outline covers whole.
    let ring = |frame: &image::RgbaImage, rect: [f32; 4]| -> (usize, usize) {
        let [x, y, width, height] = rect;
        let (x0, y0) = (x.ceil() as u32, y.ceil() as u32);
        let (x1, y1) = (
            (x + width).floor() as u32 - 1,
            (y + height).floor() as u32 - 1,
        );
        let mut cells = Vec::new();
        for px in x0..=x1 {
            cells.push((px, y0));
            cells.push((px, y1));
        }
        for py in y0..=y1 {
            cells.push((x0, py));
            cells.push((x1, py));
        }
        let lit = cells
            .iter()
            .filter(|(px, py)| {
                let p = frame.get_pixel(*px, *py);
                (p[0], p[1], p[2]) == (accent.r, accent.g, accent.b)
            })
            .count();
        (lit, cells.len())
    };
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    let plain = capture(&mut renderer, &cfg, &[view], &focused(false));
    let card = renderer.painted_cards()[0];
    let (lit, all) = ring(&plain, card.rect);
    assert!(lit < all, "no outline before the hover");
    let mut overlay = focused(false);
    overlay.card_hover = Some(card);
    let mut view = pane(&snap, 1200, 400);
    view.inline_cards = Some(&cards);
    let hovered = capture(&mut renderer, &cfg, &[view], &overlay);
    let (lit, all) = ring(&hovered, card.rect);
    assert_eq!(lit, all, "the outline rings the card");
    // A hover the UI computed for where the card was, or for another card
    // or pane at the same spot, draws nothing.
    let [x, y, width, height] = card.rect;
    let moved = PaintedCard {
        rect: [x, y + renderer.cell_h, width, height],
        ..card
    };
    let other = PaintedCard {
        nonce: kettle_core::InlineNonce::new([9, 9, 9, 9, 9, 9]).unwrap(),
        ..card
    };
    let elsewhere = PaintedCard {
        pane: card.pane + 1,
        ..card
    };
    for stale in [moved, other, elsewhere] {
        let mut overlay = focused(false);
        overlay.card_hover = Some(stale);
        let mut view = pane(&snap, 1200, 400);
        view.inline_cards = Some(&cards);
        let frame = capture(&mut renderer, &cfg, &[view], &overlay);
        let (lit, all) = ring(&frame, stale.rect);
        assert!(lit < all, "no outline for {stale:?}");
    }
}

#[test]
fn lane_play_control_changes_invalidate_retained_text() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(800, 600) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(80, 30, b"");
    let lane = MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: Some(MediaLanePlay::Pause),
        play_feedback: None,
        sound: Some(MediaLaneSound::Heard),
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 0.0, 800.0, 600.0),
        collapsed: false,
        title: "Video".into(),
        detail: String::new(),
        sender: MediaLaneSender::default(),
        hint: String::new(),
        position: (1, 1),
        page: None,
        document: false,
        image: None,
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: false,
        reload: false,
        notice: None,
        view: MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let mut overlay = focused(false);
    overlay.media_lanes.push(lane);
    capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let prepares = renderer.render_uploads().chrome_prepares;
    overlay.media_lanes[0].play = Some(MediaLanePlay::Play);
    capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    assert!(
        renderer.render_uploads().chrome_prepares > prepares,
        "switching the retained control buffer must prepare lane text"
    );
    let prepares = renderer.render_uploads().chrome_prepares;
    capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    assert_eq!(renderer.render_uploads().chrome_prepares, prepares);
}

/// A lane's open-outside button is drawn where its geometry puts it, a
/// button cell with its glyph like the close button, and is not there when
/// the lane does not offer it.
#[test]
fn a_lane_draws_its_open_outside_button_only_when_offered() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(800, 600) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(80, 30, b"");
    let viewer = |open_outside| MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 0.0, 800.0, 600.0),
        collapsed: false,
        title: "Plot".into(),
        detail: "Image · 4×4".into(),
        sender: MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(4, 4, vec![200; 64]).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: false,

        reload: false,

        notice: None,
        view: crate::MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let geometry = media_lane_geometry(
        &viewer(true),
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
    )
    .expect("the lane fits");
    let open = geometry.open_outside.expect("offered");
    let corner = |frame: &image::RgbaImage, rect: (f32, f32, f32, f32)| {
        let p = frame.get_pixel(rect.0.ceil() as u32 + 1, rect.1.ceil() as u32 + 1);
        (p[0], p[1], p[2])
    };
    let mut overlay = focused(false);
    overlay.media_lanes = vec![viewer(true)];
    let offered = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    assert_eq!(
        corner(&offered, open),
        corner(&offered, geometry.close),
        "a button cell like close"
    );
    assert!(
        distinct_colors(&offered, [open.0, open.1, open.2, open.3]) > 1,
        "its glyph is drawn"
    );
    let mut overlay = focused(false);
    overlay.media_lanes = vec![viewer(false)];
    let plain = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    assert_ne!(
        corner(&plain, open),
        corner(&plain, geometry.close),
        "no button where none is offered"
    );
    eprintln!("MEDIA_LANE_GPU_ACCEPTANCE: open-outside button");
}

/// A gallery's page controls are drawn at the end of the detail line, button
/// cells like close with their glyphs and the counter between; the control
/// with no page that way is drawn in the quieter color.
#[test]
fn a_lane_draws_a_gallerys_page_controls() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(800, 600) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(80, 30, b"");
    let viewer = |page| MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 0.0, 800.0, 600.0),
        collapsed: false,
        title: "Plan".into(),
        detail: "Mermaid diagram · 4×4".into(),
        sender: MediaLaneSender {
            text: "You opened this from the pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page,
        document: false,
        image: Some(kettle_core::ImageData::new(4, 4, vec![200; 64]).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: false,
        reload: false,
        notice: None,
        view: crate::MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let cells = (
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
    );
    let geometry = media_lane_geometry(&viewer(Some((1, 3))), cells.0, cells.1).unwrap();
    let (previous, counter, next) = (
        geometry.page_previous.expect("a gallery of three"),
        geometry.page_counter.unwrap(),
        geometry.page_next.unwrap(),
    );
    let corner = |frame: &image::RgbaImage, rect: (f32, f32, f32, f32)| {
        let p = frame.get_pixel(rect.0.ceil() as u32 + 1, rect.1.ceil() as u32 + 1);
        (p[0], p[1], p[2])
    };
    let area = |rect: (f32, f32, f32, f32)| [rect.0, rect.1, rect.2, rect.3];
    let mut frame = |page| {
        let mut overlay = focused(false);
        overlay.media_lanes = vec![viewer(page)];
        capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay)
    };
    let first = frame(Some((1, 3)));
    for (name, rect) in [("previous", previous), ("next", next)] {
        assert_eq!(
            corner(&first, rect),
            corner(&first, geometry.close),
            "{name}: a button cell like close"
        );
        assert!(distinct_colors(&first, area(rect)) > 1, "{name}: its glyph");
    }
    assert!(distinct_colors(&first, area(counter)) > 1, "the counter");
    // On the first page the page before is quiet and the page after is not;
    // on the second both speak.
    let second = frame(Some((2, 3)));
    let pixels = |frame: &image::RgbaImage, rect: (f32, f32, f32, f32)| {
        let (x0, y0) = (rect.0.ceil() as u32, rect.1.ceil() as u32);
        let (x1, y1) = ((rect.0 + rect.2) as u32, (rect.1 + rect.3) as u32);
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .map(|(x, y)| *frame.get_pixel(x, y))
            .collect::<Vec<_>>()
    };
    assert_ne!(
        pixels(&first, previous),
        pixels(&second, previous),
        "the page before is drawn quieter on the first page"
    );
    assert_eq!(
        pixels(&first, next),
        pixels(&second, next),
        "the page after speaks on both"
    );
    let single = frame(Some((1, 1)));
    assert_ne!(
        corner(&single, previous),
        corner(&single, geometry.close),
        "no page controls for one page"
    );
    eprintln!("MEDIA_LANE_GPU_ACCEPTANCE: gallery page controls");
}

/// A pane's terminal paints nothing outside its rectangle: a snapshot left
/// taller than a terminal that has just shrunk for its preview lane draws no
/// glyph or cell below it, so the lane's share of the pane shows exactly
/// what it would with no text at all.
#[test]
fn a_terminal_paints_nothing_outside_its_rect() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 240) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    // Reverse-video rows paint cell quads; plain rows paint glyphs alone.
    let text: Vec<u8> = (0..14)
        .flat_map(|row| {
            let reverse = if row % 2 == 0 { "\x1b[7m" } else { "" };
            format!("{reverse}MMMMMMMMMMMMMMMMMMM{row}\x1b[0m\r\n").into_bytes()
        })
        .collect();
    let full = snapshot_of(30, 14, &text);
    let empty = snapshot_of(30, 14, b"");
    let carved = |snap| PaneView {
        terminal: (0.0, 0.0, 320.0, 120.0),
        ..pane(snap, 320, 240)
    };
    let drawn = capture(&mut renderer, &cfg, &[carved(&full)], &focused(false));
    let bare = capture(&mut renderer, &cfg, &[carved(&empty)], &focused(false));
    let ink_above =
        (0..120).any(|y| (0..320).any(|x| drawn.get_pixel(x, y) != bare.get_pixel(x, y)));
    assert!(ink_above, "the text inside the terminal is drawn");
    for y in 121..240 {
        for x in 0..320 {
            assert_eq!(
                drawn.get_pixel(x, y),
                bare.get_pixel(x, y),
                "({x}, {y}) is below the terminal"
            );
        }
    }
}

/// In source mode a lane shows its source's rows on the pane's background
/// where the image was, and the image not at all; rendered, the image.
#[test]
fn a_lane_in_source_mode_shows_rows_instead_of_the_image() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(480, 320) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(40, 2, b"");
    let view = || PaneView {
        terminal: (0.0, 0.0, 480.0, 40.0),
        ..pane(&snap, 480, 320)
    };
    let red = [255, 0, 0, 255];
    let lane = |mode| MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 40.0, 480.0, 280.0),
        collapsed: false,
        title: "flow.mmd".into(),
        detail: "Mermaid diagram · 16×16".into(),
        sender: MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(16, 16, red.repeat(16 * 16)).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode,
        source: Some(MediaLaneSource {
            rows: vec!["flowchart LR".into(), "  A --> B".into()],
            first_row: 0,
            total_rows: 2,
        }),
        copy: true,

        reload: false,

        notice: None,
        view: crate::MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let reds = |image: &image::RgbaImage| {
        image
            .pixels()
            .filter(|pixel| pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40)
            .count()
    };
    let geometry = media_lane_geometry(
        &lane(MediaLaneMode::Source),
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
    )
    .unwrap();
    let (x, y, _, _) = geometry.image_area;
    let first_row = [
        x,
        y,
        12.0 * renderer.overlay_text_cell_width(),
        renderer.metrics.line_height,
    ];
    let mut shot = |mode| {
        let overlay = Overlay {
            media_lanes: vec![lane(mode)],
            ..focused(false)
        };
        capture(&mut renderer, &cfg, &[view()], &overlay)
    };
    let rendered = shot(MediaLaneMode::Rendered);
    assert!(reds(&rendered) >= 16 * 16, "the image is drawn");
    let source = shot(MediaLaneMode::Source);
    assert_eq!(reds(&source), 0, "the image is not");
    assert!(
        distinct_colors(&source, first_row) > 2,
        "the first source row is drawn"
    );
}

/// A zoomed lane draws its image larger, covering its content, and clipped
/// to it: none of it reaches the header, the footer or the terminal.
#[test]
fn a_zoomed_lane_covers_its_content_and_stays_inside_it() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(480, 320) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(40, 2, b"");
    let view = || PaneView {
        terminal: (0.0, 0.0, 480.0, 40.0),
        ..pane(&snap, 480, 320)
    };
    let red = [255, 0, 0, 255];
    let lane = |view| MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 40.0, 480.0, 280.0),
        collapsed: false,
        title: "plot.png".into(),
        detail: "Image · 16×16".into(),
        sender: MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(16, 16, red.repeat(16 * 16)).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: true,
        reload: false,
        notice: None,
        view,
        tile: None,
        edge: None,
    };
    let geometry = |view| {
        media_lane_geometry(
            &lane(view),
            (renderer.cell_w, renderer.cell_h),
            (
                renderer.overlay_text_cell_width(),
                renderer.metrics.line_height,
            ),
        )
        .unwrap()
    };
    let fitted = geometry(crate::MediaViewport::FIT);
    let zoom = crate::MediaViewport::FIT.zoomed(32.0, fitted.fit.unwrap(), fitted.content);
    let zoomed = geometry(zoom);
    let (cx, cy, cw, ch) = zoomed.content;
    let overlay = Overlay {
        media_lanes: vec![lane(zoom)],
        ..focused(false)
    };
    let shot = capture(&mut renderer, &cfg, &[view()], &overlay);
    let (mut inside, mut outside) = (0usize, 0usize);
    for (x, y, pixel) in shot.enumerate_pixels() {
        if pixel[0] > 200 && pixel[1] < 40 && pixel[2] < 40 {
            let (x, y) = (x as f32, y as f32);
            if x >= cx && x < cx + cw && y >= cy && y < cy + ch {
                inside += 1;
            } else {
                outside += 1;
            }
        }
    }
    assert_eq!(outside, 0, "nothing past the content");
    let area = (cw * ch) as usize;
    assert!(inside * 100 >= area * 99, "{inside} of {area}: covered");
}

/// Sharper pixels covering a zoomed lane's view are drawn instead of its
/// own, never as well; ones that do not cover it are not drawn.
#[test]
fn a_tile_that_covers_the_view_is_drawn_instead_of_the_image() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(480, 320) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(40, 2, b"");
    let view = || PaneView {
        terminal: (0.0, 0.0, 480.0, 40.0),
        ..pane(&snap, 480, 320)
    };
    let (red, blue) = ([255, 0, 0, 255], [0, 0, 255, 255]);
    let lane = |view, tile| MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 40.0, 480.0, 280.0),
        collapsed: false,
        title: "plot.png".into(),
        detail: "Image · 16×16".into(),
        sender: MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(16, 16, red.repeat(16 * 16)).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: true,
        reload: false,
        notice: None,
        view,
        tile,
        edge: None,
    };
    let fitted = media_lane_geometry(
        &lane(crate::MediaViewport::FIT, None),
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
    )
    .unwrap();
    let zoom = crate::MediaViewport::FIT.zoomed(32.0, fitted.fit.unwrap(), fitted.content);
    let tile = |coverage| {
        Some(crate::MediaLaneTile {
            image: kettle_core::ImageData::new(8, 8, blue.repeat(64)).unwrap(),
            coverage,
        })
    };
    let mut shot = |tile| {
        let overlay = Overlay {
            media_lanes: vec![lane(zoom, tile)],
            ..focused(false)
        };
        capture(&mut renderer, &cfg, &[view()], &overlay)
    };
    let count = |image: &image::RgbaImage, want: [u8; 3]| {
        image
            .pixels()
            .filter(|pixel| (0..3).all(|channel| pixel[channel].abs_diff(want[channel]) < 40))
            .count()
    };
    let covering = shot(tile((0.0, 0.0, 1.0, 1.0)));
    assert_eq!(count(&covering, [255, 0, 0]), 0, "the image is not drawn");
    assert!(count(&covering, [0, 0, 255]) > 1000, "the tile is");
    let short = shot(tile((0.49, 0.49, 0.51, 0.51)));
    assert_eq!(
        count(&short, [0, 0, 255]),
        0,
        "a tile short of the view is not"
    );
    assert!(count(&short, [255, 0, 0]) > 1000);
}

/// A preview lane paints its opaque panel over its share of the pane, with
/// its title, and leaves the terminal beside it as it was.
#[test]
fn a_preview_lane_paints_its_panel_and_leaves_the_terminal() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(480, 320) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(40, 8, b"hello");
    let view = || PaneView {
        terminal: (0.0, 0.0, 480.0, 180.0),
        ..pane(&snap, 480, 320)
    };
    let lane = MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 180.0, 480.0, 140.0),
        collapsed: false,
        title: "Plot of the week".into(),
        detail: "Image · 2×2".into(),
        sender: MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(2, 2, vec![255; 16]).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: false,

        reload: false,

        notice: None,
        view: crate::MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let without = capture(&mut renderer, &cfg, &[view()], &focused(false));
    let overlay = Overlay {
        media_lanes: vec![lane],
        ..focused(false)
    };
    let with = capture(&mut renderer, &cfg, &[view()], &overlay);
    for y in 0..180 {
        for x in 0..480 {
            assert_eq!(
                with.get_pixel(x, y),
                without.get_pixel(x, y),
                "({x}, {y}) is the terminal's"
            );
        }
    }
    let panel = crate::completion_palette(&cfg.theme, cfg.theme.palette[4]).panel_bg;
    let lane_pixels = (182..320)
        .flat_map(|y| (2..480).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = with.get_pixel(x, y);
            (p[0], p[1], p[2]) == (panel.r, panel.g, panel.b)
        })
        .count();
    assert!(
        lane_pixels > 478 * 138 / 2,
        "the panel fills the lane: {lane_pixels}"
    );
    assert!(
        distinct_colors(&with, [8.0, 184.0, 200.0, 24.0]) > 2,
        "the title is drawn"
    );
}

/// A playing video's lane draws its frame in the item's place and its bar
/// under it: the part played in the accent up to where the video is, the
/// rest in the divider's color.
#[test]
fn a_playing_lane_draws_its_frame_and_bar() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(480, 320) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(40, 8, b"hello");
    let view = || PaneView {
        terminal: (0.0, 0.0, 480.0, 60.0),
        ..pane(&snap, 480, 320)
    };
    let lane = MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: Some(crate::MediaLanePlay::Pause),
        play_feedback: None,
        sound: None,
        frame: Some(crate::MediaLaneFrame {
            sheet: kettle_core::ImageData::new(4, 2, [0, 0, 255, 255].repeat(8)).unwrap(),
            source: (0, 0, 4, 2),
            size: (160, 80),
        }),
        scrubber: Some(crate::MediaLaneScrubber {
            seekable: true,
            position_ms: 2000,
            duration_ms: 8000,
            label: "0:02 / 0:08".into(),
        }),
        pane: 1,
        rect: (0.0, 60.0, 480.0, 260.0),
        collapsed: false,
        title: "clip.mp4".into(),
        detail: "Video · 0:08".into(),
        sender: MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(2, 2, vec![255; 16]).unwrap()),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: false,
        reload: false,
        notice: None,
        view: crate::MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let geometry = media_lane_geometry(
        &lane,
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
    )
    .expect("the lane fits");
    let bar = geometry.scrubber.expect("a bar under the picture");
    let picture = geometry.image.expect("the frame in the item's place");
    let overlay = Overlay {
        media_lanes: vec![lane],
        ..focused(false)
    };
    let frame = capture(&mut renderer, &cfg, &[view()], &overlay);
    let palette = crate::completion_palette(&cfg.theme, cfg.theme.palette[4]);
    let at = |x: f32, y: f32| {
        let p = frame.get_pixel(x as u32, y as u32);
        (p[0], p[1], p[2])
    };
    let middle = bar.1 + bar.3 / 2.0;
    let accent = (palette.emphasis.r, palette.emphasis.g, palette.emphasis.b);
    let divider = (palette.divider.r, palette.divider.g, palette.divider.b);
    assert_eq!(at(bar.0 + bar.2 * 0.1, middle), accent, "played");
    assert_eq!(at(bar.0 + bar.2 * 0.6, middle), divider, "to come");
    assert_eq!(at(bar.0 + bar.2 * 0.95, middle), divider, "to come");
    let center = at(picture.0 + picture.2 / 2.0, picture.1 + picture.3 / 2.0);
    assert!(
        center.2 > 200 && center.0 < 40,
        "the frame, blue: {center:?}"
    );

    let mut player = overlay;
    player.media_lanes[0].pin_label = Some("Pin".into());
    player.media_lanes[0].rect = (0.0, 0.0, 480.0, 320.0);
    let geometry = media_lane_geometry_scaled(
        &player.media_lanes[0],
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
        renderer.scale,
    )
    .unwrap();
    let play = geometry.play.unwrap();
    let timeline = geometry.scrubber.unwrap();
    assert!(play.3 >= 44.0 && timeline.3 >= 24.0);
    let painted = capture(&mut renderer, &cfg, &[view()], &player);
    let primary =
        crate::media_transport::paint(&player.media_lanes[0], &geometry, palette, None, false, 1.0)
            .unwrap()
            .shapes[0]
            .fill;
    let fill = [primary[0], primary[1], primary[2]].map(|v| (v * 255.0).round() as i32);
    let mut glyph_pixels = 0;
    for y in (play.1 + play.3 / 2.0 - 10.0) as u32..(play.1 + play.3 / 2.0 + 10.0) as u32 {
        for x in (play.0 + play.2 / 2.0 - 8.0) as u32..(play.0 + play.2 / 2.0 + 8.0) as u32 {
            let pixel = painted.get_pixel(x, y);
            glyph_pixels += usize::from(
                (0..3)
                    .map(|c| (pixel[c] as i32 - fill[c]).abs())
                    .sum::<i32>()
                    > 96,
            );
        }
    }
    assert!(
        glyph_pixels > 20,
        "white Play/Pause icon is visible on the dark player strip"
    );
    for control in [geometry.theater.unwrap(), geometry.fullscreen.unwrap()] {
        let white = (control.1 as u32..(control.1 + control.3) as u32)
            .flat_map(|y| (control.0 as u32..(control.0 + control.2) as u32).map(move |x| (x, y)))
            .filter(|&(x, y)| painted.get_pixel(x, y).0[..3].iter().all(|&v| v > 220))
            .count();
        assert!(white > 30, "display control has a visible vector outline");
        let center = painted.get_pixel(
            (control.0 + control.2 / 2.0) as u32,
            (control.1 + control.3 / 2.0) as u32,
        );
        assert!(
            center.0[..3].iter().all(|&v| v < 40),
            "outlined icons have an open center, not letter glyphs"
        );
    }
    assert!(
        painted
            .get_pixel((play.0 + 3.0) as u32, (play.1 + play.3 / 2.0) as u32)
            .0[..3]
            .iter()
            .all(|&v| v < 40),
        "Play has no accent-filled disc"
    );
    player.media_lanes[0].play = Some(MediaLanePlay::Stop);
    let stop = capture(&mut renderer, &cfg, &[view()], &player);
    assert!(
        stop.get_pixel(
            (play.0 + play.2 / 2.0 + 5.0) as u32,
            (play.1 + play.3 / 2.0 - 5.0) as u32
        )
        .0[..3]
            .iter()
            .all(|&v| v > 220),
        "silent preview advertises Stop with a square"
    );
    player.media_lanes[0].play = Some(MediaLanePlay::Pause);
    player.media_lanes[0].focused_control = Some(play);
    let keyboard_focus = capture(&mut renderer, &cfg, &[view()], &player);
    let edge = keyboard_focus.get_pixel((play.0 + 1.0) as u32, (play.1 + play.3 / 2.0) as u32);
    assert!(
        edge.0[..3].iter().all(|&v| v > 220),
        "keyboard focus is visible above the dark transport"
    );
    player.media_lanes[0].focused_control = None;
    capture(&mut renderer, &cfg, &[view()], &player);
    let prepares = renderer.text_prepares;
    let writes = renderer.rounded_surfaces.upload_counts().buffer_writes;
    player.media_lanes[0].scrubber.as_mut().unwrap().position_ms += 100;
    let progressed = capture(&mut renderer, &cfg, &[view()], &player);
    assert_eq!(
        renderer.text_prepares, prepares,
        "progress-only updates reuse prepared text"
    );
    assert!(renderer.rounded_surfaces.upload_counts().buffer_writes > writes);
    player.media_pointer = Some((timeline.0 + timeline.2 / 2.0, timeline.1 + timeline.3 / 2.0));
    let hovered = capture(&mut renderer, &cfg, &[view()], &player);
    let changed = (timeline.1 as u32..(timeline.1 + timeline.3) as u32)
        .flat_map(|y| (timeline.0 as u32..(timeline.0 + timeline.2) as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| progressed.get_pixel(x, y) != hovered.get_pixel(x, y))
        .count();
    assert!(
        changed > 100,
        "thin timeline grows and reveals its thumb on hover"
    );
    assert_eq!(
        renderer.text_prepares, prepares,
        "timeline hover does not reshape text"
    );
    for scale in [1.0, 1.25, 2.0] {
        let mut scaled = Renderer::headless_for_tests_with(
            &cfg,
            (480.0 * scale) as u32,
            (320.0 * scale) as u32,
            scale,
            false,
        )
        .unwrap()
        .unwrap();
        player.media_pointer = None;
        player.media_lanes[0].rect = (0.0, 0.0, 480.0 * scale, 320.0 * scale);
        player.media_lanes[0].play = Some(MediaLanePlay::Play);
        player.media_lanes[0].play_feedback = Some(MediaLanePlay::Play);
        let geometry = media_lane_geometry_scaled(
            &player.media_lanes[0],
            (scaled.cell_w, scaled.cell_h),
            (scaled.overlay_text_cell_width(), scaled.metrics.line_height),
            scale,
        )
        .unwrap();
        let circle = geometry.play_feedback.unwrap().0;
        let rendered = capture(&mut scaled, &cfg, &[view()], &player);
        let white: Vec<_> = (circle.1.floor() as u32..(circle.1 + circle.3).ceil() as u32)
            .flat_map(|y| {
                (circle.0.floor() as u32..(circle.0 + circle.2).ceil() as u32).map(move |x| (x, y))
            })
            .filter(|&(x, y)| rendered.get_pixel(x, y).0[..3].iter().all(|&v| v > 220))
            .collect();
        assert!(white.len() > 20);
        let min_x = white.iter().map(|p| p.0).min().unwrap() as f32;
        let max_x = white.iter().map(|p| p.0).max().unwrap() as f32 + 1.0;
        let min_y = white.iter().map(|p| p.1).min().unwrap() as f32;
        let max_y = white.iter().map(|p| p.1).max().unwrap() as f32 + 1.0;
        assert!(
            ((min_y + max_y) / 2.0 - (circle.1 + circle.3 / 2.0)).abs() < 0.8,
            "central Play is vertically centered at scale {scale}: {min_y}..{max_y} in {circle:?}"
        );
        let offset = ((min_x + max_x) / 2.0 - (circle.0 + circle.2 / 2.0)) / circle.2;
        assert!(
            (0.02..0.065).contains(&offset),
            "central Play has a small rightward optical offset at scale {scale}: {offset}"
        );
        let height = max_y - min_y;
        let expected_height = circle.3 * 0.42;
        // Solid-white coverage omits antialiased edges, with driver-dependent
        // rounding of a pixel per edge and fractional rectangle bounds.
        assert!(
            (height - expected_height).abs() <= 2.5,
            "central Play has consistent visible proportions at scale {scale}: {height} vs {expected_height}"
        );
    }
}

#[test]
fn disabled_wallpaper_discards_late_worker_frames() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(32, 32) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.background_type = kettle_config::BackgroundType::Solid;
    for cached in [true, false] {
        renderer.bg_image_cache = cached.then(|| BgImageAnim {
            path: "old".into(),
            blur: 0,
            frames: Vec::new(),
            opaque_frames: Vec::new(),
            gaps: Vec::new(),
            started: std::time::Instant::now(),
        });
        renderer.bg_image_pending = Some(("old".into(), 0));
        let (sender, _jobs) = std::sync::mpsc::sync_channel(1);
        let (results, receiver) = std::sync::mpsc::channel();
        renderer.bg_image_worker = Some(BgImageWorker {
            sender,
            receiver,
            busy: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        });
        let _ = renderer
            .render_frame(&[], &TabBar::hidden(), &cfg, &focused(false))
            .expect("disabled frame renders");
        assert!(renderer.bg_image_cache.is_none());
        assert!(
            results
                .send(BgImageResult {
                    path: "old".into(),
                    blur_radius: 0,
                    frames: vec![BgFrame {
                        image: BgImage {
                            width: 1,
                            height: 1,
                            rgba: vec![255; 4],
                        },
                        gap_ms: 0,
                    }],
                })
                .is_ok()
        );
        let _ = renderer
            .render_frame(&[], &TabBar::hidden(), &cfg, &focused(false))
            .expect("late decode redraw renders");
        assert!(renderer.bg_image_pending.is_none());
        assert!(
            renderer
                .bg_image_worker
                .as_ref()
                .unwrap()
                .try_recv()
                .is_none(),
            "disabled backgrounds retained decoded frames"
        );
    }
}

#[test]
fn regression_active_close_glyph_contrasts_with_its_painted_chip() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 160) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.theme = kettle_config::Theme::by_name("Builtin Dark");
    cfg.background_type = kettle_config::BackgroundType::Starfield;
    cfg.chrome_background = kettle_config::ChromeBackground::Black;
    let mut tabs = TabBar::hidden();
    tabs.height = 40.0;
    tabs.segments.push(TabSeg {
        idx: 0,
        rect: (0.0, 0.0, 200.0, 40.0),
        title_rect: (0.0, 0.0, 150.0, 40.0),
        close: (150.0, 0.0, 40.0, 40.0),
        title: "Tab".into(),
        path: None,
        active: true,
        activity: TabActivity::Normal,
    });
    let shot = capture_with_tabs(&mut renderer, &cfg, &[], &focused(false), &tabs);
    let background = shot.get_pixel(156, 6);
    let bg = Rgb::new(background[0], background[1], background[2]);
    let best = (155..185)
        .flat_map(|x| (5..35).map(move |y| (x, y)))
        .map(|(x, y)| {
            let p = shot.get_pixel(x, y);
            color::contrast_ratio(Rgb::new(p[0], p[1], p[2]), bg)
        })
        .fold(1.0_f64, f64::max);
    assert!(best >= 2.9, "painted close glyph: {best:.3}:1");
}

#[test]
fn regression_inactive_color_emoji_dim_grid() {
    inactive_color_emoji_dim(TextRendererMode::Grid);
}

#[test]
fn regression_inactive_color_emoji_dim_legacy() {
    inactive_color_emoji_dim(TextRendererMode::Legacy);
}

fn inactive_color_emoji_dim(mode: TextRendererMode) {
    let _serialized = gpu_test_guard();
    let mut cfg = gpu_test_config();
    cfg.text_renderer = mode;
    // Per-pane titlebars sit over the first row and change color with focus.
    cfg.show_titlebar = false;
    cfg.theme.background = Rgb::new(0, 0, 0);
    cfg.theme.foreground = Rgb::new(255, 255, 255);
    cfg.unfocused_split_opacity = 1.0;
    cfg.inactive_color_offset = 1.0;
    cfg.inactive_bg_color_offset = 1.0;
    let Some(mut renderer) = Renderer::headless_for_tests(&cfg, 400, 160).unwrap() else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let snap = snapshot_of(20, 4, "\u{1f600}  M".as_bytes());
    let other = snapshot_of(20, 4, b"");
    let shot = |renderer: &mut Renderer, cfg: &Config, active| {
        let mut view = pane(&snap, 200, 160);
        view.focused = active;
        let mut second = pane(&other, 200, 160);
        second.id = 2;
        second.rect.0 = 200.0;
        second.terminal.0 = 200.0;
        second.focused = !active;
        capture(renderer, cfg, &[view, second], &focused(false))
    };
    let active = shot(&mut renderer, &cfg, true);
    let color_font_available = {
        let Renderer {
            pane_buffers,
            swash,
            font_system,
            ..
        } = &mut renderer;
        pane_buffers[0].layout_runs().any(|run| {
            run.glyphs.iter().any(|glyph| {
                swash
                    .get_image_uncached(font_system, glyph.physical((0.0, 0.0), 1.0).cache_key)
                    .is_some_and(|image| image.content == glyphon::cosmic_text::SwashContent::Color)
            })
        })
    };
    if !color_font_available {
        eprintln!("no color emoji font on this host; skipped");
        return;
    }
    let undimmed = shot(&mut renderer, &cfg, false);
    let (ox, oy) = pane_grid_origin((0.0, 0.0, 200.0, 160.0), (cfg.padding_x, cfg.padding_y));
    let (cw, ch) = (renderer.cell_w, renderer.cell_h);
    let row = |image: &image::RgbaImage| {
        image::imageops::crop_imm(
            image,
            ox as u32,
            oy as u32,
            (8.0 * cw).ceil() as u32,
            ch.ceil() as u32,
        )
        .to_image()
    };
    assert_eq!(
        row(&active),
        row(&undimmed),
        "zero dimming leaves the text unchanged"
    );
    let emoji_pixels: Vec<_> = (oy as u32..(oy + renderer.cell_h) as u32)
        .flat_map(|y| (ox as u32..(ox + 2.0 * renderer.cell_w) as u32).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let p = active.get_pixel(x, y);
            p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]) > 50
        })
        .collect();
    assert!(emoji_pixels.len() > 10, "fixture must render a color emoji");
    for offsets in [false, true] {
        cfg.unfocused_split_opacity = if offsets { 1.0 } else { 0.1 };
        cfg.inactive_color_offset = if offsets { 0.1 } else { 1.0 };
        let inactive = shot(&mut renderer, &cfg, false);
        let brightness = |image: &image::RgbaImage| -> u64 {
            emoji_pixels
                .iter()
                .map(|&(x, y)| {
                    image.get_pixel(x, y).0[..3]
                        .iter()
                        .map(|&v| u64::from(v))
                        .sum::<u64>()
                })
                .sum()
        };
        assert!(
            brightness(&inactive) < brightness(&active) * 3 / 4,
            "{mode:?} offsets={offsets}: inactive color emoji must dim"
        );
        // Legacy's emoji advance can move the following M off the cell grid.
        let text_x = (ox + 3.0 * cw) as u32;
        let text_end = (ox + 8.0 * cw) as u32;
        let best = (oy as u32..(oy + renderer.cell_h) as u32)
            .flat_map(|y| (text_x..text_end).map(move |x| (x, y)))
            .map(|(x, y)| {
                let p = inactive.get_pixel(x, y);
                color::contrast_ratio(Rgb::new(p[0], p[1], p[2]), cfg.theme.background)
            })
            .fold(1.0_f64, f64::max);
        // Legacy keeps continuous glyph positions, so anti-aliasing can leave
        // no pixel at full coverage; dimming text like a bitmap would fall far
        // below either bound.
        let floor = if mode == TextRendererMode::Grid {
            4.5
        } else {
            4.3
        };
        assert!(best >= floor, "{mode:?} ordinary text: {best:.3}:1");
        assert_eq!(
            inactive,
            shot(&mut renderer, &cfg, false),
            "cached frame retains dimming"
        );
        assert_eq!(
            row(&active),
            row(&shot(&mut renderer, &cfg, true)),
            "focus restores the color emoji"
        );
    }
}

#[test]
fn regression_inactive_decorations_keep_the_glyph_contrast_floor() {
    let _serialized = gpu_test_guard();
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        let Some((probe, mut cfg)) = renderer(400, 160) else {
            eprintln!("no GPU adapter on this host; skipped");
            return;
        };
        drop(probe);
        cfg.text_renderer = mode;
        cfg.theme.background = Rgb::new(0, 0, 0);
        cfg.theme.foreground = Rgb::new(255, 255, 255);
        cfg.unfocused_split_opacity = 0.1;
        let mut renderer = Renderer::headless_for_tests(&cfg, 400, 160)
            .unwrap()
            .unwrap();
        let snap = snapshot_of(20, 4, b"\x1b[4;9mM");
        let other = snapshot_of(20, 4, b"");
        let mut view = pane(&snap, 200, 160);
        view.focused = false;
        let mut second = pane(&other, 200, 160);
        second.id = 2;
        second.rect.0 = 200.0;
        second.terminal.0 = 200.0;
        let shot = capture(&mut renderer, &cfg, &[view, second], &focused(false));
        let x = (cfg.padding_x + renderer.cell_w * 0.5) as u32;
        for (name, dy) in [
            ("underline", renderer.cell_h - 2.0),
            ("strikeout", renderer.cell_h * 0.5),
        ] {
            let y = (cfg.padding_y + dy) as u32;
            let best = (y.saturating_sub(1)..=y + 1)
                .map(|y| {
                    let p = shot.get_pixel(x, y);
                    color::contrast_ratio(Rgb::new(p[0], p[1], p[2]), cfg.theme.background)
                })
                .fold(1.0_f64, f64::max);
            assert!(best >= 4.5, "{mode:?} {name}: {best:.3}:1");
        }
    }
}

#[test]
fn regression_inactive_images_cards_and_preview_lanes_dim() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(1200, 600) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.theme.background = Rgb::new(0, 0, 0);
    cfg.unfocused_split_opacity = 0.4;
    let (mut cards, snap, nonce) = crate::inline_cards::tests::fixture();
    let white = kettle_core::ImageData::new(16, 16, vec![255; 16 * 16 * 4]).unwrap();
    cards.set_poster(nonce, Some(&white));
    let images = [kettle_core::Placement {
        abs_line: snap.history_origin + 6,
        col: 30,
        cell_cols: 4,
        cell_rows: 1,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 4.0,
        display_rows: 1.0,
        img: white.clone(),
        source_rect: None,
        source_crop: None,
        id: None,
        placement_id: 0,
        kitty_params: None,
        z: 0,
    }];
    let lane = MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 1,
        rect: (0.0, 200.0, 1200.0, 300.0),
        collapsed: false,
        title: "Preview".into(),
        detail: String::new(),
        sender: MediaLaneSender::default(),
        hint: String::new(),
        position: (1, 1),
        page: None,
        document: false,
        image: Some(white),
        status: String::new(),
        canvas: MediaCanvas::Theme,
        open_outside: false,
        mode: MediaLaneMode::Rendered,
        source: None,
        copy: false,
        reload: false,
        notice: None,
        view: MediaViewport::FIT,
        tile: None,
        edge: None,
    };
    let geometry = media_lane_geometry(
        &lane,
        (renderer.cell_w, renderer.cell_h),
        (
            renderer.overlay_text_cell_width(),
            renderer.metrics.line_height,
        ),
    )
    .unwrap();
    let (lx, ly, lw, lh) = geometry.image_area;
    let lane_pixel = ((lx + lw * 0.5) as u32, (ly + lh * 0.5) as u32);
    let overlay = Overlay {
        media_lanes: vec![lane],
        ..focused(false)
    };
    let other = snapshot_of(20, 4, b"");
    let shot = |renderer: &mut Renderer, cfg: &Config, active| {
        let mut view = pane(&snap, 1200, 500);
        view.terminal.3 = 200.0;
        view.inline_cards = Some(&cards);
        view.images = &images;
        view.focused = active;
        let mut second = pane(&other, 1200, 100);
        second.id = 2;
        second.rect.1 = 500.0;
        second.terminal.1 = 500.0;
        second.focused = !active;
        capture(renderer, cfg, &[view, second], &overlay)
    };
    let active = shot(&mut renderer, &cfg, true);
    let card = renderer.painted_cards()[0].rect;
    let samples = [
        (
            "inline image",
            (
                (cfg.padding_x + 32.0 * renderer.cell_w) as u32,
                (cfg.padding_y + 6.5 * renderer.cell_h) as u32,
            ),
        ),
        (
            "card poster",
            (
                (card[0] + card[2] * 0.5) as u32,
                (card[1] + card[3] * 0.5) as u32,
            ),
        ),
        ("preview lane", lane_pixel),
    ];
    let mut failures = Vec::new();
    for offsets in [false, true] {
        cfg.unfocused_split_opacity = if offsets { 1.0 } else { 0.4 };
        cfg.inactive_color_offset = if offsets { 0.4 } else { 1.0 };
        let inactive = shot(&mut renderer, &cfg, false);
        for (name, (x, y)) in samples {
            let bright = active.get_pixel(x, y)[0];
            let dim = inactive.get_pixel(x, y)[0];
            assert!(bright > 220, "fixture {name} pixel ({x},{y}) was {bright}");
            if dim >= bright - 20 {
                failures.push(format!("{name} offsets={offsets}: {bright} -> {dim}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("; "));
}

#[test]
fn ime_preedit_single_glyph_survives_solid_block_cursor() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.cursor_blink = false;
    cfg.theme.cursor = cfg.theme.foreground;
    let snap = snapshot_of(20, 4, b"W\r\x1b[2 q");
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        cfg.text_renderer = mode;
        let mut overlay = focused(true);
        overlay.ime_preedit = Some(ImePreedit {
            text: "a".into(),
            row: 0,
            col: 0,
        });
        let glyph = capture(&mut renderer, &cfg, &[pane(&snap, 320, 120)], &overlay);
        overlay.ime_preedit.as_mut().unwrap().text = " ".into();
        let blank = capture(&mut renderer, &cfg, &[pane(&snap, 320, 120)], &overlay);
        let cell = |image: &image::RgbaImage| {
            image::imageops::crop_imm(
                image,
                cfg.padding_x.ceil() as u32,
                cfg.padding_y.ceil() as u32,
                renderer.cell_w.floor() as u32,
                renderer.cell_h.floor() as u32,
            )
            .to_image()
        };
        assert!(
            cell(&glyph) != cell(&blank),
            "composition glyph hidden in {mode:?}"
        );
        assert!(
            cursor_pixels(&blank, &cfg, &snap) > 0,
            "composition caret must remain visible in {mode:?}"
        );
    }
}

#[test]
fn ime_preedit_narrow_cells_cover_the_shaped_text() {
    let _serialized = gpu_test_guard();
    let mut cfg = gpu_test_config();
    cfg.cell_width = 0.5;
    let Some(mut renderer) = Renderer::headless_for_tests(&cfg, 320, 120).unwrap() else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let first = snapshot_of(40, 4, b"WWWWWWWWWWWWWWWW\r");
    let second = snapshot_of(40, 4, b"iiiiiiiiiiiiiiii\r");
    let overlay = Overlay {
        ime_preedit: Some(ImePreedit {
            text: "abcd".into(),
            row: 0,
            col: 0,
        }),
        ..focused(false)
    };
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        cfg.text_renderer = mode;
        let a = capture(&mut renderer, &cfg, &[pane(&first, 320, 120)], &overlay);
        let b = capture(&mut renderer, &cfg, &[pane(&second, 320, 120)], &overlay);
        let shaped_width = renderer.ime_buffer.layout_runs().next().unwrap().line_w;
        assert!(shaped_width > 4.0 * renderer.cell_w);
        let x0 = cfg.padding_x.ceil() as u32;
        let y0 = cfg.padding_y.ceil() as u32;
        let x1 = (cfg.padding_x + shaped_width).floor() as u32;
        let y1 = (cfg.padding_y + renderer.cell_h).floor() as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                assert_eq!(
                    a.get_pixel(x, y),
                    b.get_pixel(x, y),
                    "terminal text leaked through shaped IME extent at {x},{y} in {mode:?}"
                );
            }
        }
    }
}

#[test]
fn ime_preedit_screenshot_hides_existing_glyphs_and_retains_cursor() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    cfg.padding_x = 8.0;
    cfg.padding_y = 8.0;
    let first = snapshot_of(20, 4, b"WWWW\r");
    let second = snapshot_of(20, 4, b"iiii\r");
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        cfg.text_renderer = mode;
        for cursor in [false, true] {
            let mut overlay = focused(cursor);
            overlay.ime_preedit = Some(ImePreedit {
                text: "abcd".into(),
                row: 0,
                col: 0,
            });
            let a = capture(&mut renderer, &cfg, &[pane(&first, 320, 120)], &overlay);
            let b = capture(&mut renderer, &cfg, &[pane(&second, 320, 120)], &overlay);
            let x0 = cfg.padding_x.ceil() as u32;
            let y0 = cfg.padding_y.ceil() as u32;
            let x1 = (cfg.padding_x + 4.0 * renderer.cell_w).floor() as u32;
            let y1 = (cfg.padding_y + renderer.cell_h).floor() as u32;
            for y in y0..y1 {
                for x in x0..x1 {
                    assert_eq!(
                        a.get_pixel(x, y),
                        b.get_pixel(x, y),
                        "underlying text leaked through IME at {x},{y} in {mode:?}"
                    );
                }
            }
            assert!(
                distinct_colors(
                    &b,
                    [x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32]
                ) > 2,
                "screenshot must include composition text"
            );
            if cursor {
                assert!(
                    cursor_pixels(&b, &cfg, &second) > 0,
                    "cursor must remain visible over preedit"
                );
            }
            overlay.ime_preedit = None;
            let closed = capture(&mut renderer, &cfg, &[pane(&second, 320, 120)], &overlay);
            assert_ne!(b, closed, "closing composition must repaint its row");
            assert!(!renderer.ime_prepared);
        }
    }
}

#[test]
fn grapheme_positive_scalars_do_not_displace_the_next_cell_in_either_renderer() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_GPU_SKIP: no adapter");
        return;
    };
    cfg.text_renderer = TextRendererMode::Grid;
    let reference = snapshot_of(20, 4, b"\x1b[?2027h  X\x1b[?25l");
    let expected = capture(
        &mut renderer,
        &cfg,
        &[pane(&reference, 640, 180)],
        &focused(false),
    );
    let snap = snapshot_of(
        20,
        4,
        "\x1b[?2027h\u{1f469}\u{200d}\u{1f4bb}X\x1b[?2027l\x1b[?25l".as_bytes(),
    );
    let x = (cfg.padding_x + renderer.cell_w * 2.0).round() as u32;
    let y = cfg.padding_y.round() as u32;
    let width = renderer.cell_w.ceil() as u32;
    let height = renderer.cell_h.ceil() as u32;
    let crop =
        |image: &image::RgbaImage| image::imageops::crop_imm(image, x, y, width, height).to_image();
    let expected_cell = crop(&expected);
    assert!(
        expected_cell
            .pixels()
            .any(|p| *p != *expected_cell.get_pixel(0, 0)),
        "reference X has ink"
    );
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        cfg.text_renderer = mode;
        let actual = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 640, 180)],
            &focused(false),
        );
        assert!(
            crop(&actual) == expected_cell,
            "{mode:?} must keep X at column 2"
        );
    }
    eprintln!("GRAPHEME_GPU_PASS: {:?}", renderer.gpu.adapter_info());
}

#[test]
fn grapheme_vs15_after_vs16_prefers_a_monochrome_face_without_shrinking() {
    use image::GenericImageView;
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_VS15_GPU_SKIP: no adapter");
        return;
    };
    let Some(_) = renderer.text_symbol_family else {
        eprintln!("GRAPHEME_VS15_GPU_SKIP: no text-symbol face");
        return;
    };
    cfg.theme.foreground = Rgb::new(255, 255, 255);
    cfg.theme.background = Rgb::new(0, 0, 0);
    for sequence in [
        "\u{2764}\u{fe0f}\u{fe0e}",
        "\u{231a}\u{fe0e}",
        "\u{1f600}\u{fe0e}",
    ] {
        let text = format!("\x1b[?2027h{sequence}X\x1b[?25l");
        let snap = snapshot_of(20, 4, text.as_bytes());
        assert_eq!(snap.cursor.point.column.0, 3);
        let frame = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 640, 180)],
            &focused(false),
        );
        let cropped = image::imageops::crop_imm(
            &frame,
            cfg.padding_x.round() as u32,
            cfg.padding_y.round() as u32,
            (renderer.cell_w * 2.0).ceil() as u32,
            renderer.cell_h.ceil() as u32,
        );
        let colored = cropped
            .pixels()
            .filter(|(_, _, p)| {
                p.0[..3].iter().max().unwrap() - p.0[..3].iter().min().unwrap() > 30
            })
            .count();
        let ink = cropped
            .pixels()
            .filter(|(_, _, p)| p.0[..3].iter().any(|c| *c > 80))
            .count();
        eprintln!(
            "GRAPHEME_VS15_GPU: colored={colored} ink={ink} adapter={:?}",
            renderer.gpu.adapter_info()
        );
        assert!(ink > 0, "text glyph must be visible");
        assert_eq!(
            colored, 0,
            "the last VS15 must override an earlier VS16's colored presentation"
        );
    }
}

#[test]
fn grapheme_unsupported_zwj_emoji_uses_one_image_and_keeps_the_next_cell_clear() {
    use super::geometry::GridGlyphCell;
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_ZWJ_GPU_SKIP: no adapter");
        return;
    };
    let reference = snapshot_of(20, 4, b"\x1b[?2027h  X\x1b[?25l");
    let expected = capture(
        &mut renderer,
        &cfg,
        &[pane(&reference, 640, 180)],
        &focused(false),
    );
    // Both are Extended_Pictographic, but woman-ZWJ-cat has no joined glyph
    // in the usual platform emoji fonts. It still owns exactly two cells.
    for marks in [String::new(), "\u{301}".repeat(30)] {
        let text = format!("\x1b[?2027h\u{1f469}{marks}\u{200d}\u{1f408}X\x1b[?25l");
        let snap = snapshot_of(20, 4, text.as_bytes());
        let actual = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 640, 180)],
            &focused(false),
        );
        let keys: Vec<_> = renderer.pane_buffers[0]
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .filter(|g| {
                GridGlyphCell::from_metadata(g.metadata)
                    .is_some_and(|c| c.column == 0 && c.clustered)
            })
            .map(|g| g.physical((0., 0.), 1.).cache_key)
            .collect();
        let visible = keys
            .iter()
            .filter(|key| {
                renderer
                    .swash
                    .get_image(&mut renderer.font_system, **key)
                    .as_ref()
                    .is_some_and(|i| i.placement.width > 0 && i.placement.height > 0)
            })
            .count();
        eprintln!(
            "GRAPHEME_ZWJ_GPU: visible_images={visible} adapter={:?}",
            renderer.gpu.adapter_info()
        );
        assert_eq!(
            visible, 1,
            "unsupported joined emoji must use one replacement, never decomposed images"
        );
        let crop = |i: &image::RgbaImage| {
            image::imageops::crop_imm(
                i,
                (cfg.padding_x + renderer.cell_w * 2.).round() as u32,
                cfg.padding_y.round() as u32,
                renderer.cell_w.ceil() as u32,
                renderer.cell_h.ceil() as u32,
            )
            .to_image()
        };
        assert!(
            crop(&actual) == crop(&expected),
            "cluster must not cover the following X"
        );
    }
}

#[test]
fn grapheme_replacement_stays_consistent_under_a_cursor_on_cached_frames() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_CURSOR_GPU_SKIP: no adapter");
        return;
    };
    let reference = snapshot_of(20, 4, b"\x1b[?2027h  X\x1b[?25l");
    let expected = capture(
        &mut renderer,
        &cfg,
        &[pane(&reference, 640, 180)],
        &focused(false),
    );
    let snap = snapshot_of(
        20,
        4,
        "\x1b[?2027h\u{1f469}\u{200d}\u{1f408}X\x1b[1;1H\x1b[2 q".as_bytes(),
    );
    for repeat in 0..3 {
        let frame = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 640, 180)],
            &focused(true),
        );
        assert!(
            renderer.pending_cursor_glyph.is_some(),
            "block cursor must be exercised"
        );
        let keys: Vec<_> = renderer
            .cursor_glyph_buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .map(|g| g.physical((0., 0.), 1.).cache_key)
            .collect();
        let count = keys
            .iter()
            .filter(|key| {
                renderer
                    .swash
                    .get_image_uncached(&mut renderer.font_system, **key)
                    .as_ref()
                    .is_some_and(|i| i.placement.width > 0 && i.placement.height > 0)
            })
            .count();
        assert_eq!(
            count, 1,
            "cursor on frame {repeat} must draw the same single replacement as the pane"
        );
        let crop = |image: &image::RgbaImage| {
            image::imageops::crop_imm(
                image,
                (cfg.padding_x + renderer.cell_w * 2.).round() as u32,
                cfg.padding_y.round() as u32,
                renderer.cell_w.ceil() as u32,
                renderer.cell_h.ceil() as u32,
            )
            .to_image()
        };
        assert!(
            crop(&frame) == crop(&expected),
            "cursor must not overwrite the following cell"
        );
    }
    eprintln!(
        "GRAPHEME_CURSOR_GPU_PASS: {:?}",
        renderer.gpu.adapter_info()
    );
}

#[test]
fn grapheme_prefixes_keep_the_first_spacing_glyph_visible_in_its_cell() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_PREFIX_GPU_SKIP: no adapter");
        return;
    };
    cfg.theme.foreground = Rgb::new(255, 255, 255);
    cfg.theme.background = Rgb::new(0, 0, 0);
    for (prefix, base) in [
        ("\u{600}", "A"),
        ("\u{605}", "A"),
        ("\u{70f}", "A"),
        ("\u{94d}", "\u{915}"),
    ] {
        let reference = snapshot_of(20, 4, format!("\x1b[?2027h{base}X\x1b[?25l").as_bytes());
        let expected = capture(
            &mut renderer,
            &cfg,
            &[pane(&reference, 640, 180)],
            &focused(false),
        );
        let baseline = renderer.pane_buffers[0]
            .layout_runs()
            .next()
            .unwrap()
            .line_y
            .round() as i32;
        let snap = snapshot_of(
            20,
            4,
            format!("\x1b[?2027h{prefix}{base}X\x1b[?25l").as_bytes(),
        );
        assert_eq!(snap.cursor.point.column.0, 2);
        let actual = capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 640, 180)],
            &focused(false),
        );
        let delta = renderer.pane_buffers[0]
            .layout_runs()
            .next()
            .unwrap()
            .line_y
            .round() as i32
            - baseline;
        let mut ink = 0;
        let mut retained = 0;
        for y in cfg.padding_y.round() as u32..(cfg.padding_y + renderer.cell_h * 2.).ceil() as u32
        {
            for x in cfg.padding_x.round() as u32..(cfg.padding_x + renderer.cell_w).round() as u32
            {
                if expected.get_pixel(x, y).0[..3].iter().all(|c| *c > 220) {
                    ink += 1;
                    let actual_y = y as i32 + delta;
                    if actual_y >= 0
                        && actual_y < actual.height() as i32
                        && actual.get_pixel(x, actual_y as u32).0[..3]
                            .iter()
                            .all(|c| *c > 180)
                    {
                        retained += 1;
                    }
                }
            }
        }
        eprintln!("GRAPHEME_PREFIX_GPU: prefix={prefix:?} base={base:?} retained={retained}/{ink}");
        assert!(
            ink > 0 && retained * 10 >= ink * 9,
            "prefix displaced or hid the spacing glyph"
        );
    }
    eprintln!(
        "GRAPHEME_PREFIX_GPU_PASS: {:?}",
        renderer.gpu.adapter_info()
    );
}

#[test]
fn grapheme_prefix_cursor_anchors_the_spacing_glyph_on_cached_frames() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_PREFIX_CURSOR_GPU_SKIP: no adapter");
        return;
    };
    for (prefix, base) in [
        ("\u{600}", "A"),
        ("\u{605}", "A"),
        ("\u{70f}", "A"),
        ("\u{94d}", "\u{915}"),
    ] {
        let reference = snapshot_of(
            20,
            4,
            format!("\x1b[?2027h{base}X\x1b[1;1H\x1b[2 q").as_bytes(),
        );
        capture(
            &mut renderer,
            &cfg,
            &[pane(&reference, 640, 180)],
            &focused(true),
        );
        let cursor = renderer
            .pending_cursor_glyph
            .as_ref()
            .expect("reference block cursor");
        let glyph = renderer
            .cursor_glyph_buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .find(|g| g.start == 0)
            .unwrap();
        let expected = glyph.physical((cursor.x, 0.), 1.).x;
        let snap = snapshot_of(
            20,
            4,
            format!("\x1b[?2027h{prefix}{base}X\x1b[1;1H\x1b[2 q").as_bytes(),
        );
        for frame in 0..3 {
            capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 640, 180)],
                &focused(true),
            );
            let cursor = renderer
                .pending_cursor_glyph
                .as_ref()
                .expect("prefixed block cursor");
            let glyph = renderer
                .cursor_glyph_buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .find(|g| g.start <= prefix.len() && prefix.len() < g.end)
                .expect("spacing glyph under cursor");
            assert_eq!(
                glyph.physical((cursor.x, 0.), 1.).x,
                expected,
                "{prefix:?} cursor frame {frame}"
            );
        }
    }
    eprintln!(
        "GRAPHEME_PREFIX_CURSOR_GPU_PASS: {:?}",
        renderer.gpu.adapter_info()
    );
}

#[test]
fn grapheme_trailing_blank_colors_reuse_the_shaped_row() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("GRAPHEME_CACHE_GPU_SKIP: no adapter");
        return;
    };
    let first = snapshot_of(20, 4, b"\x1b[?2027hA     \x1b[?25l");
    let before = capture(
        &mut renderer,
        &cfg,
        &[pane(&first, 320, 120)],
        &focused(false),
    );
    let keys = renderer.pane_line_keys[0].clone();
    let second = snapshot_of(20, 4, b"\x1b[?2027hA\x1b[31m     \x1b[?25l");
    let after = capture(
        &mut renderer,
        &cfg,
        &[pane(&second, 320, 120)],
        &focused(false),
    );
    assert_eq!(renderer.pane_line_keys[0], keys);
    assert_eq!(renderer.pane_buffers[0].lines[0].text(), "A ");
    assert_eq!(
        before, after,
        "changing the foreground of cut blanks has no ink"
    );
}

#[test]
fn the_block_cursor_makes_the_same_presentation_choice_as_the_pane() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(320, 120) else {
        eprintln!("CURSOR_PRESENTATION_GPU_SKIP: no adapter");
        return;
    };
    let Some(family) = renderer.text_symbol_family else {
        eprintln!("CURSOR_PRESENTATION_GPU_SKIP: no text-symbol face");
        return;
    };
    for qualified in [false, true] {
        let text = format!(
            "\u{23fa}{}\x1b[1G\x1b[2 q",
            if qualified { "\u{fe0f}" } else { "" }
        );
        let snap = snapshot_of(20, 4, text.as_bytes());
        capture(
            &mut renderer,
            &cfg,
            &[pane(&snap, 320, 120)],
            &focused(true),
        );
        let cursor = renderer
            .pending_cursor_glyph
            .as_ref()
            .expect("block cursor");
        assert_eq!(cursor.emoji_qualified, qualified);
        if !qualified {
            assert_eq!(
                cursor.attrs.as_attrs().family,
                glyphon::Family::Name(family)
            );
        }
        let glyphs: Vec<_> = renderer
            .cursor_glyph_buffer
            .layout_runs()
            .flat_map(|run| run.glyphs.iter())
            .map(|g| g.physical((0., 0.), 1.).cache_key)
            .collect();
        let mut ink = 0;
        for key in glyphs {
            if let Some(image) = renderer
                .swash
                .get_image_uncached(&mut renderer.font_system, key)
                && image.placement.width > 0
                && image.placement.height > 0
            {
                ink += 1;
                if !qualified {
                    assert_ne!(image.content, glyphon::SwashContent::Color);
                }
            }
        }
        assert!(ink > 0, "cursor symbol has visible ink");
        let pane = renderer.pane_buffers[0].lines[0].attrs_list().get_span(0);
        if !qualified {
            assert_eq!(pane.family, glyphon::Family::Name(family));
        }
    }
}

#[test]
fn grapheme_cells_do_not_change_other_legacy_rows() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(640, 180) else {
        eprintln!("GRAPHEME_LEGACY_GPU_SKIP: no adapter");
        return;
    };
    cfg.text_renderer = TextRendererMode::Legacy;
    cfg.font_ligatures = true;
    let first = snapshot_of(40, 5, b"ffi -> === a\x1b[1mb\x1b[0mc\r\n\x1b[?25l");
    let before = capture(
        &mut renderer,
        &cfg,
        &[pane(&first, 640, 180)],
        &focused(false),
    );
    let second = snapshot_of(
        40,
        5,
        "ffi -> === a\x1b[1mb\x1b[0mc\r\n\x1b[?2027h\u{1f469}\u{200d}\u{1f4bb}\x1b[?25l".as_bytes(),
    );
    let after = capture(
        &mut renderer,
        &cfg,
        &[pane(&second, 640, 180)],
        &focused(false),
    );
    let crop = |image: &image::RgbaImage| {
        image::imageops::crop_imm(
            image,
            cfg.padding_x.round() as u32,
            cfg.padding_y.round() as u32,
            (renderer.cell_w * 20.).ceil() as u32,
            renderer.cell_h.ceil() as u32,
        )
        .to_image()
    };
    assert!(
        crop(&before) == crop(&after),
        "an unrelated Legacy row must retain its pixels when another row gains a grapheme"
    );
}

#[test]
fn special_attribute_colors_render_only_when_enabled_and_override_ansi_on_request() {
    use alacritty_terminal::term::cell::Flags;
    use image::GenericImageView;
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("SPECIAL_COLOR_GPU_SKIP: no adapter");
        return;
    };
    cfg.minimum_contrast = 1.0;
    cfg.theme.foreground = Rgb::new(255, 255, 255);
    cfg.theme.background = Rgb::new(0, 0, 0);
    cfg.theme.palette[2] = Rgb::new(0, 255, 0);
    cfg.theme.palette[10] = Rgb::new(0, 255, 0);
    let x = cfg.padding_x.round() as u32;
    let y = cfg.padding_y.round() as u32;
    let width = renderer.cell_w.ceil() as u32;
    let height = renderer.cell_h.ceil() as u32;
    let ink = |frame: &image::RgbaImage, red: bool| {
        image::imageops::crop_imm(frame, x, y, width, height)
            .pixels()
            .filter(|(_, _, p)| {
                let [r, g, b, _] = p.0;
                if red {
                    r > 70 && u16::from(r) > u16::from(g) * 2 && u16::from(r) > u16::from(b) * 2
                } else {
                    g > 70 && u16::from(g) > u16::from(r) * 2 && u16::from(g) > u16::from(b) * 2
                }
            })
            .count()
    };
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        cfg.text_renderer = mode;
        let plain = snapshot_of(20, 4, b"\x1b[1mA\x1b[?25l");
        let expected = capture(
            &mut renderer,
            &cfg,
            &[pane(&plain, 320, 120)],
            &focused(false),
        );
        let stored = snapshot_of(20, 4, b"\x1b]5;0;#ff0000\x07\x1b[1mA\x1b[?25l");
        let inactive = capture(
            &mut renderer,
            &cfg,
            &[pane(&stored, 320, 120)],
            &focused(false),
        );
        assert_eq!(
            expected, inactive,
            "{mode:?}: setting a special color must not enable it"
        );
        for (sgr, override_ansi, want_red) in [
            ("1", false, true),
            ("1;32", false, false),
            ("1;32", true, true),
        ] {
            let text = format!(
                "\x1b]5;0;#ff0000\x07\x1b]6;0;1\x07{}\x1b[{sgr}mA\x1b[?25l",
                if override_ansi {
                    "\x1b]106;5;1\x07"
                } else {
                    ""
                }
            );
            let snap = snapshot_of(20, 4, text.as_bytes());
            assert!(
                snap.cells
                    .iter()
                    .find(|c| c.c == 'A')
                    .unwrap()
                    .flags
                    .contains(Flags::BOLD)
            );
            let frame = capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 320, 120)],
                &focused(false),
            );
            assert!(
                ink(&frame, want_red) > 0,
                "{mode:?}: selected foreground must reach the pixels, SGR={sgr}, expected_red={want_red}, ANSI override={override_ansi}"
            );
        }
        for (index, sgr) in [(0, "1"), (1, "4"), (2, "5"), (3, "7"), (4, "3")] {
            let text =
                format!("\x1b]5;{index};#ff0000\x07\x1b]6;{index};1\x07\x1b[{sgr}mA\x1b[?25l");
            let snap = snapshot_of(20, 4, text.as_bytes());
            let frame = capture(
                &mut renderer,
                &cfg,
                &[pane(&snap, 320, 120)],
                &focused(false),
            );
            assert!(
                ink(&frame, true) > 0,
                "{mode:?}: special color {index} must reach its attribute's glyph"
            );
        }
    }
    eprintln!("SPECIAL_COLOR_GPU_PASS: {:?}", renderer.gpu.adapter_info());
}

#[test]
fn screen_reverse_paints_defaults_preserves_explicit_colors_and_restores_cached_frames() {
    let _serial = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(320, 120) else {
        eprintln!("SCREEN_REVERSE_GPU_SKIP: no adapter");
        return;
    };
    cfg.minimum_contrast = 1.0;
    cfg.theme.foreground = Rgb::new(255, 255, 255);
    cfg.theme.background = Rgb::new(0, 0, 0);
    let x = cfg.padding_x.round() as u32;
    let y = cfg.padding_y.round() as u32;
    let cw = renderer.cell_w;
    let ch = renderer.cell_h;
    let content = b"\x1b[48;2;18;52;86m   \x1b[0mA\x1b[31mR\x1b[0m\r\n\x1b[7m   \x1b[0m\x1b[?25l";
    let sample = |frame: &image::RgbaImage, col: usize, row: usize| {
        *frame.get_pixel(
            x + ((col as f32 + 0.5) * cw) as u32,
            y + ((row as f32 + 0.5) * ch) as u32,
        )
    };
    for mode in [TextRendererMode::Grid, TextRendererMode::Legacy] {
        cfg.text_renderer = mode;
        let normal = snapshot_of(20, 4, content);
        let original = capture(
            &mut renderer,
            &cfg,
            &[pane(&normal, 320, 120)],
            &focused(false),
        );
        let mut reversed = b"\x1b[?5h".to_vec();
        reversed.extend_from_slice(content);
        let reverse = snapshot_of(20, 4, &reversed);
        let flipped = capture(
            &mut renderer,
            &cfg,
            &[pane(&reverse, 320, 120)],
            &focused(false),
        );
        assert_eq!(
            sample(&original, 10, 2).0[..3],
            [0, 0, 0],
            "{mode:?}: initial background"
        );
        assert_eq!(
            sample(&flipped, 10, 2).0[..3],
            [255, 255, 255],
            "{mode:?}: reversed background"
        );
        assert_eq!(
            sample(&original, 1, 0),
            sample(&flipped, 1, 0),
            "{mode:?}: explicit RGB background"
        );
        assert_eq!(
            sample(&flipped, 1, 1).0[..3],
            [0, 0, 0],
            "{mode:?}: SGR inverse composes with screen mode"
        );
        let restored = capture(
            &mut renderer,
            &cfg,
            &[pane(&normal, 320, 120)],
            &focused(false),
        );
        assert_eq!(
            original, restored,
            "{mode:?}: restoring normal mode must restore the frame"
        );
    }
}

#[test]
fn toast_redesign_corners_reveal_terminal_while_center_is_opaque() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, mut cfg)) = renderer(800, 600) else {
        return;
    };
    cfg.theme.background = Rgb::new(35, 75, 120);
    cfg.theme.palette[0] = Rgb::new(16, 18, 25);
    let snap = snapshot_of(80, 24, b"");
    let baseline = capture(
        &mut renderer,
        &cfg,
        &[pane(&snap, 800, 600)],
        &Overlay::default(),
    );
    let geometry = update_toast_geometry(
        (800.0, 600.0),
        0.0,
        1.0,
        (
            renderer.overlay_text_cell_width(),
            renderer.overlay_text_line_height(),
        ),
        7,
        "v6.0.0 is available.",
    )
    .unwrap();
    let overlay = Overlay {
        update_toast: Some(UpdateToastOverlay {
            geometry,
            message: "v6.0.0 is available.".into(),
            details: "Details".into(),
            close: "\u{00d7}".into(),
            opacity: 1.0,
            focused: None,
        }),
        ..Overlay::default()
    };
    let frame = capture(&mut renderer, &cfg, &[pane(&snap, 800, 600)], &overlay);
    let (x, y, w, h) = geometry.rect;
    for (px, py) in [
        (x + 2.0, y + 2.0),
        (x + w - 3.0, y + 2.0),
        (x + 2.0, y + h - 3.0),
        (x + w - 3.0, y + h - 3.0),
    ] {
        let (px, py) = (px.floor() as u32, py.floor() as u32);
        assert_eq!(
            frame.get_pixel(px, py),
            baseline.get_pixel(px, py),
            "transparent rounded corner at {px},{py}"
        );
    }
    let (px, py) = ((x + w * 0.5) as u32, (y + h - 4.0) as u32);
    assert_ne!(frame.get_pixel(px, py), baseline.get_pixel(px, py));
}

#[test]
fn toast_redesign_text_focus_fade_and_dismissal_preserve_retained_layers() {
    let _serialized = gpu_test_guard();
    let Some((mut renderer, cfg)) = renderer(800, 600) else {
        return;
    };
    let snap = snapshot_of(80, 24, b"");
    let panes = [pane(&snap, 800, 600)];
    let baseline = capture(&mut renderer, &cfg, &panes, &Overlay::default());
    let geometry = update_toast_geometry(
        (800.0, 600.0),
        0.0,
        1.0,
        (
            renderer.overlay_text_cell_width(),
            renderer.overlay_text_line_height(),
        ),
        7,
        "v6.0.0 is available.",
    )
    .unwrap();
    let mut overlay = Overlay {
        update_toast: Some(UpdateToastOverlay {
            geometry,
            message: "v6.0.0 is available.".into(),
            details: "Details".into(),
            close: "\u{00d7}".into(),
            opacity: 1.0,
            focused: Some(2),
        }),
        ..Overlay::default()
    };
    let opaque = capture(&mut renderer, &cfg, &panes, &overlay);
    let uploads = renderer.toast_surfaces.upload_counts();
    assert_eq!(opaque, capture(&mut renderer, &cfg, &panes, &overlay));
    let steady = renderer.toast_surfaces.upload_counts();
    assert_eq!(
        (uploads.buffer_writes, uploads.mapped_writes),
        (steady.buffer_writes, steady.mapped_writes)
    );
    // Text must remain above the opaque surfaces, including both button labels.
    overlay.update_toast.as_mut().unwrap().message.clear();
    overlay.update_toast.as_mut().unwrap().details.clear();
    overlay.update_toast.as_mut().unwrap().close.clear();
    let blank = capture(&mut renderer, &cfg, &panes, &overlay);
    for (x, y, w, h) in [geometry.message, geometry.details, geometry.close] {
        let differences = (y.ceil() as u32..(y + h).floor() as u32)
            .flat_map(|py| (x.ceil() as u32..(x + w).floor() as u32).map(move |px| (px, py)))
            .filter(|&(px, py)| opaque.get_pixel(px, py) != blank.get_pixel(px, py))
            .count();
        assert!(differences > 4, "text hidden by surface at {x},{y}");
    }
    overlay.update_toast.as_mut().unwrap().opacity = 0.5;
    let half = capture(&mut renderer, &cfg, &panes, &overlay);
    assert_ne!(blank, half);
    assert_ne!(baseline, half);
    overlay.update_toast.as_mut().unwrap().opacity = 0.0;
    let transparent = capture(&mut renderer, &cfg, &panes, &overlay);
    assert_eq!(
        baseline, transparent,
        "focused stroke must fade with the toast"
    );
    overlay.update_toast = None;
    assert_eq!(baseline, capture(&mut renderer, &cfg, &panes, &overlay));
}

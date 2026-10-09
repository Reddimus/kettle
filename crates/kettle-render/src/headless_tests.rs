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
    assert_eq!(
        renderer.painted_cards(),
        &[(pane_id, nonce)],
        "the frame reports the card it accepted"
    );
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
        quads.draw(&mut pass);
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
            label_rects(&renderer, crate::card_scene::CardLabelKind::Claude),
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

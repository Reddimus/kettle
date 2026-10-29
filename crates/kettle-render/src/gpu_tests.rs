use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Serializes the tests that stand up a real GPU device.
///
/// libtest runs tests in parallel, so without this several of the tests
/// below create wgpu instances, adapters, and devices in the same process
/// at the same moment. On a host whose only adapter is a software or basic
/// display driver, such as the CI Windows runners, that can take the whole
/// test binary down with `STATUS_ACCESS_VIOLATION` (`0xC0000005`). Cargo
/// reports it against `kettle-render` with no test having failed, because
/// the fault is inside the driver rather than in Rust. Hosts with a real
/// GPU driver do not reproduce it.
///
/// One device at a time costs a little wall clock and removes the whole
/// class. These tests have no reason to run concurrently with each other.
static GPU_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the GPU lock, ignoring poisoning.
///
/// A panicking GPU test must report its own failure — not turn every
/// later GPU test into a poisoned-mutex error that buries it.
pub(crate) fn gpu_test_guard() -> std::sync::MutexGuard<'static, ()> {
    GPU_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Use the native Windows ARM software adapter for deterministic unit tests.
///
/// Parallels' ARM64 WDDM adapter currently faults inside the driver while a
/// headless wgpu device is being requested, taking down the whole libtest
/// process with `STATUS_ACCESS_VIOLATION`. WARP renders the same pipelines
/// and readback pixels successfully. Keep this scoped to these unit tests:
/// the live renderer and the standalone smoke outside the affected Parallels
/// guest retain hardware-first adapter selection.
pub(crate) fn gpu_test_config() -> Config {
    Config {
        gpu_force_software: cfg!(all(target_os = "windows", target_arch = "aarch64")),
        ..Config::default()
    }
}

#[test]
fn gpu_pipelines_compile_and_render_offscreen() {
    let _serialized = gpu_test_guard();
    match super::offscreen_selftest_with_config(&gpu_test_config()) {
        Ok(true) => {}
        Ok(false) => eprintln!("no GPU adapter on this host; skipped"),
        Err(e) => panic!("offscreen GPU self-test failed: {e}"),
    }
}

fn await_test_mapping(
    rx: std::sync::mpsc::Receiver<std::result::Result<(), wgpu::BufferAsyncError>>,
) -> Result<()> {
    rx.recv()
        .map_err(|e| anyhow!("GPU test map channel closed: {e}"))?
        .map_err(|e| anyhow!("GPU test buffer mapping failed: {e:?}"))
}

#[test]
fn failed_gpu_mapping_is_an_error() {
    let (tx, rx) = std::sync::mpsc::channel();
    tx.send(Err(wgpu::BufferAsyncError)).unwrap();
    assert!(
        await_test_mapping(rx).is_err(),
        "GPU mapping failure was reported as a skip"
    );
}

#[test]
fn disconnected_gpu_mapping_is_an_error() {
    let (tx, rx) = std::sync::mpsc::channel();
    drop(tx);
    assert!(
        await_test_mapping(rx).is_err(),
        "closed GPU mapping channel was reported as a skip"
    );
}

/// A half-opaque quad must contribute half its colour, not a quarter.
///
/// The quad shader returns PREMULTIPLIED colour (`rgb * a`). If the
/// pipeline blends with `ALPHA_BLENDING`, whose source factor is
/// `SrcAlpha`, the GPU multiplies by alpha a second time and every
/// translucent surface kettle draws comes out at alpha², darkening images,
/// panels, highlights, separators, and the unfocused-pane dim overlay.
///
/// This renders and reads the pixel back, so it is the convention as the
/// hardware actually applies it. `tests/alpha_convention.rs` reads the
/// source for the pipelines this cannot cheaply stand up; a source-token
/// check can always be worked around, a rendered pixel cannot.
#[test]
fn a_half_opaque_quad_blends_at_half_not_a_quarter() {
    let _serialized = gpu_test_guard();
    let Some(pixel) =
        pollster::block_on(render_one_translucent_quad()).expect("GPU fixture renders")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };

    // White at 50% alpha over black: premultiplied source-over leaves 0.5
    // linear, which the sRGB target stores as ~188. Multiplying by alpha
    // twice leaves 0.25 linear, stored as ~137 — far outside any tolerance
    // a driver's rounding needs.
    let expected = 188_u8;
    let doubled = 137_u8;
    for (channel, value) in ["r", "g", "b"].into_iter().zip(pixel) {
        assert!(
            value.abs_diff(expected) <= 3,
            "channel {channel} came back {value}, expected ~{expected}; \
                 ~{doubled} means alpha was applied twice (premultiplied shader \
                 output paired with a straight-alpha blend state)"
        );
    }
}

/// Draw one 50%-opaque white quad over black and read the centre pixel
/// back. `None` when the host has no usable adapter.
async fn render_one_translucent_quad() -> Result<Option<[u8; 3]>> {
    let cfg = gpu_test_config();
    let (_instance, adapter) = match resolve_headless_adapter(&cfg, "alpha_blend_test").await {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("kettle-alpha-blend-test"),
            required_limits: live_device_limits(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("GPU test device request failed: {e:?}"))?;

    // Match the offscreen self-test's format so this measures the same
    // pipeline configuration the renderer builds for a real surface.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let size = 8_u32;
    let mut quads = QuadPipeline::new(&device, format);
    quads.upload(
        &device,
        &queue,
        [size as f32, size as f32],
        &[QuadInstance {
            pos: [0.0, 0.0],
            size: [size as f32, size as f32],
            color: [1.0, 1.0, 1.0, 0.5],
        }],
    );

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-alpha-blend-target"),
        size: wgpu::Extent3d {
            width: size,
            height: size,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    // 8 px * 4 bytes is below the 256-byte copy alignment, so pad the row.
    let bytes_per_row = 256_u32;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-alpha-blend-readback"),
        size: u64::from(bytes_per_row) * u64::from(size),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-alpha-blend-pass"),
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
                bytes_per_row: Some(bytes_per_row),
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
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("GPU test poll failed: {e:?}"))?;
    await_test_mapping(rx)?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("GPU test mapped range failed: {e:?}"))?;
    // Centre row, centre pixel — well inside the quad.
    let offset = (size as usize / 2) * bytes_per_row as usize + (size as usize / 2) * 4;
    let pixel = [data[offset], data[offset + 1], data[offset + 2]];
    drop(data);
    readback.unmap();
    Ok(Some(pixel))
}

/// The starfield's stars must land on the GPU where the CPU put them.
///
/// The model is resolved once per frame on the CPU and delivered through a
/// hand-written uniform array. The compiler checks nothing about that
/// layout. A stride or alignment disagreement between the Rust struct and
/// the WGSL is not an error; the shader just reads a star out of the wrong
/// bytes and draws it somewhere else. So this renders a real frame and
/// looks for light where the CPU said a star would be.
#[test]
fn starfield_stars_land_where_the_cpu_placed_them() {
    let _serialized = gpu_test_guard();
    let Some((luma, expected, side)) =
        pollster::block_on(render_starfield_frame()).expect("GPU fixture renders")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };

    assert!(
        !expected.is_empty(),
        "fixture must pick a time with visible stars"
    );
    let (brightest_index, &peak) = luma
        .iter()
        .enumerate()
        .max_by_key(|&(_, &value)| value)
        .expect("a non-empty frame");
    let (x, y) = (brightest_index as u32 % side, brightest_index as u32 / side);
    assert!(
        peak > 24,
        "the field rendered essentially black ({peak}), so either the \
             shader did not compile the uniform array or nothing was uploaded"
    );

    // Positions are in pixels from the surface centre; the readback is in
    // pixels from the top-left.
    let centre = side as f32 * 0.5;
    let nearest = expected
        .iter()
        .map(|star| {
            let dx = (star[0] + centre) - x as f32;
            let dy = (star[1] + centre) - y as f32;
            (dx * dx + dy * dy).sqrt()
        })
        .fold(f32::INFINITY, f32::min);
    assert!(
        nearest <= 3.0,
        "the brightest pixel sits {nearest:.1} px from the nearest star \
             the CPU uploaded — the uniform block is being read at a \
             different offset than it was written"
    );

    // The star PROFILE, not just its position. The falloff is written in
    // terms of squared distance and squared radii, and an algebra slip
    // there still leaves light near the star, just not a crisp core inside
    // a soft halo.
    //
    // Measured globally rather than by sampling outward from the peak,
    // because a fixed direction runs into whatever neighbouring star lies
    // that way, which reflects where the field put its stars and not the
    // falloff. Core radii are under 1.5 px, so only a handful of pixels
    // per star sit above half the peak.
    let half_peak = peak / 2;
    let bright = luma.iter().filter(|&&v| v > half_peak).count();
    // A collapsed core would light the halo's 3–9 px radius instead —
    // hundreds of pixels per star, an order of magnitude past this.
    assert!(
        bright <= 12 * expected.len(),
        "{bright} pixels are above half the peak across {} stars — the \
             core should be a few pixels each, not a soft orb",
        expected.len()
    );

    // A starfield is sparse points on a black sky, not a wash. If the
    // whole frame lifted, the accumulation is adding something it should
    // not.
    let lit = luma.iter().filter(|&&v| v > 8).count();
    assert!(
        lit * 4 < luma.len(),
        "{lit} of {} pixels are lit — the sky should be mostly black",
        luma.len()
    );
}

/// Render one starfield frame offscreen at a time chosen to have stars on
/// screen, and return its luminance plane, the CPU's star positions, and
/// the square side used. `None` when the host has no usable adapter.
async fn render_starfield_frame() -> Result<Option<(Vec<u8>, Vec<[f32; 2]>, u32)>> {
    let cfg = gpu_test_config();
    let (_instance, adapter) = match resolve_headless_adapter(&cfg, "starfield_layout_test").await {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("kettle-starfield-test"),
            required_limits: live_device_limits(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("GPU test device request failed: {e:?}"))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    // 256 px keeps the readback small while staying wide enough that a
    // star's few-pixel core is unambiguous, and 256*4 already meets the
    // copy alignment exactly.
    let side = 256_u32;
    let bytes_per_row = side * 4;
    let resolution = [side as f32, side as f32];
    let time = 40.0_f32;

    let mut pipeline = starfield::StarfieldPipeline::new(&device, format);
    let expected = pipeline.frame_positions(resolution, time);
    pipeline.upload(&queue, resolution, time);

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-starfield-target"),
        size: wgpu::Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-starfield-readback"),
        size: u64::from(bytes_per_row) * u64::from(side),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-starfield-pass"),
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
        pipeline.draw(&mut pass);
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
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(side),
            },
        },
        wgpu::Extent3d {
            width: side,
            height: side,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("GPU test poll failed: {e:?}"))?;
    await_test_mapping(rx)?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("GPU test mapped range failed: {e:?}"))?;
    // Green dominates perceived luminance and every star colour carries
    // it, so it is a fine single-channel proxy for this whole test.
    let mut luma = vec![0_u8; (side * side) as usize];
    for y in 0..side {
        for x in 0..side {
            luma[(y * side + x) as usize] = data[(y * bytes_per_row + x * 4) as usize + 1];
        }
    }
    drop(data);
    readback.unmap();
    // Keep only the stars that can actually be on this surface, so the
    // nearest-star search is not satisfied by one off screen.
    let half = side as f32 * 0.5;
    let visible = expected
        .into_iter()
        .filter(|s| s[0].abs() <= half && s[1].abs() <= half)
        .collect();
    Ok(Some((luma, visible, side)))
}

/// `PreMultiplied` is the only mode that scales the clear.
///
/// `Opaque` throws alpha away at composite time, so scaling would only
/// darken the surface toward black; `PostMultiplied` divides alpha back
/// out, so it wants the straight value.
#[test]
fn only_a_premultiplied_surface_scales_its_clear_by_alpha() {
    let bg = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    let alpha = 0.5;
    let linear = srgb(bg.r);
    // Precondition: the scale has to be observable at all. A black
    // background or a fully opaque alpha would make every mode agree and
    // this test could not fail.
    assert!(
        linear > 0.0 && alpha < 1.0,
        "fixture must use a non-black colour and a translucent alpha"
    );

    let premultiplied = surface_clear_color(bg, alpha, wgpu::CompositeAlphaMode::PreMultiplied);
    assert!(
        (premultiplied.r - linear * alpha).abs() < 1e-12,
        "PreMultiplied must scale the linear colour by alpha, got {}",
        premultiplied.r
    );
    assert!(
        (premultiplied.a - alpha).abs() < 1e-12,
        "the alpha channel itself is never scaled"
    );

    for mode in [
        wgpu::CompositeAlphaMode::Opaque,
        wgpu::CompositeAlphaMode::PostMultiplied,
        wgpu::CompositeAlphaMode::Inherit,
        wgpu::CompositeAlphaMode::Auto,
    ] {
        let straight = surface_clear_color(bg, alpha, mode);
        assert!(
            (straight.r - linear).abs() < 1e-12,
            "{mode:?} must keep the straight colour, got {}",
            straight.r
        );
    }
}

/// Un-premultiplying has to invert the multiply in the space the GPU
/// applied it in, and the two spaces disagree by far more than rounding.
#[test]
fn unpremultiply_inverts_the_multiply_in_the_matching_space() {
    // A MID-GREY original, not white. Recovering white saturates the
    // divide in either space, so a white fixture reports success for the
    // wrong reason and cannot tell the two apart at all.
    let original = 128_u8;
    let byte_alpha = 128_u8;
    let alpha = byte_alpha as f64 / 255.0;

    // sRGB attachment: the GPU multiplied in LINEAR, so the stored byte is
    // the sRGB encoding of linear(original) * alpha.
    let stored = srgb_encode(srgb(original) * alpha);
    assert!(
        (stored as f64 / alpha) < 255.0,
        "fixture must not saturate the byte-space divide, or the two \
             spaces agree and this test cannot fail"
    );

    let mut srgb_texel = [stored, stored, stored, byte_alpha];
    unpremultiply_rgba8(&mut srgb_texel, true);
    for channel in &srgb_texel[..3] {
        assert!(
            channel.abs_diff(original) <= 2,
            "linear-space un-premultiply must recover ~{original}, got {channel}"
        );
    }

    // The same bytes read in the wrong space land nowhere near it — which
    // is what makes the `srgb_encoded` flag load-bearing rather than
    // decorative.
    let mut wrong_space = [stored, stored, stored, byte_alpha];
    unpremultiply_rgba8(&mut wrong_space, false);
    assert!(
        wrong_space[0].abs_diff(original) > 30,
        "byte-space division on an sRGB texel should NOT recover \
             {original}; got {}",
        wrong_space[0]
    );

    // Plain `Unorm` attachment: the multiply happened on the stored bytes,
    // so plain division is the inverse.
    let mut unorm_texel = [64, 64, 64, byte_alpha];
    unpremultiply_rgba8(&mut unorm_texel, false);
    assert_eq!(unorm_texel[..3], [128, 128, 128]);

    // Degenerate alphas: nothing to recover, and nothing to divide.
    let mut transparent = [200, 200, 200, 0];
    unpremultiply_rgba8(&mut transparent, true);
    assert_eq!(transparent, [0, 0, 0, 0]);
    let mut opaque = [200, 201, 202, 255];
    unpremultiply_rgba8(&mut opaque, true);
    assert_eq!(opaque, [200, 201, 202, 255]);
}

/// The clear is the one write in the frame that does not pass through a
/// blend, so it is the only one that has to premultiply itself — and a
/// straight clear is invisible until something blends over it.
///
/// This renders the same scene twice, once with each clear convention, and
/// asserts they disagree before asserting which one is right. A fixture
/// that only checked the clear would pass either way: a translucent clear
/// with nothing drawn on it reads back identically in both conventions.
///
/// White background at 50% alpha, one 50%-alpha black quad over it:
///
/// - premultiplied clear → dst `(0.5, 0.5)`, blend leaves
///   `rgb = 0 + 0.5·0.5 = 0.25` over `a = 0.75`; un-premultiplied that is
///   `1/3` linear, which the sRGB target stores as ~156.
/// - straight clear → dst `(1.0, 0.5)`, blend leaves `rgb = 0.5`, stored
///   as ~188 with no conversion — the value kettle actually shipped.
#[test]
fn a_translucent_background_composites_against_a_premultiplied_clear() {
    let _serialized = gpu_test_guard();
    let Some((premultiplied, straight, _postmultiplied)) =
        pollster::block_on(render_black_quad_over_translucent_clear())
            .expect("GPU fixture renders")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };

    assert_ne!(
        premultiplied[0], straight[0],
        "the two clear conventions must produce different pixels, or this \
             fixture cannot detect the bug it exists for"
    );

    let expected = 156_u8;
    let shipped = 188_u8;
    for (channel, value) in ["r", "g", "b"].into_iter().zip(premultiplied) {
        assert!(
            value.abs_diff(expected) <= 3,
            "channel {channel} came back {value}, expected ~{expected}; \
                 ~{shipped} means the clear was written straight while every \
                 pipeline drawing over it treats the destination as \
                 premultiplied"
        );
    }
    assert!(
        premultiplied[3].abs_diff(191) <= 2,
        "alpha is stored linearly even on an sRGB target; got {}",
        premultiplied[3]
    );
}

#[test]
fn postmultiplied_presentation_unpremultiplies_the_completed_scene() {
    let _serialized = gpu_test_guard();
    let Some((premultiplied, _wrong_clear, postmultiplied)) =
        pollster::block_on(render_black_quad_over_translucent_clear())
            .expect("GPU fixture renders")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };

    for channel in 0..4 {
        assert!(
            postmultiplied[channel].abs_diff(premultiplied[channel]) <= 2,
            "PostMultiplied output {postmultiplied:?} must be the straight-alpha form of the premultiplied scene {premultiplied:?}"
        );
    }
}

/// Render one 50%-alpha black quad over a 50%-alpha white clear three ways
/// (premultiplied clear, straight clear, and premultiplied clear through
/// the PostMultiplied presentation pass) and return the three centre pixels
/// in that order, all in straight alpha (the first two converted back on
/// the CPU, the third by the presentation pass). `None` when the host has
/// no usable adapter.
async fn render_black_quad_over_translucent_clear() -> Result<Option<([u8; 4], [u8; 4], [u8; 4])>> {
    let cfg = gpu_test_config();
    let (_instance, adapter) =
        match resolve_headless_adapter(&cfg, "premultiplied_clear_test").await {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("kettle-premultiplied-clear-test"),
            required_limits: live_device_limits(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("GPU test device request failed: {e:?}"))?;

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let size = 8_u32;
    let bg = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    let alpha = 0.5;

    let render = |clear: wgpu::Color, postmultiplied: bool| -> Result<[u8; 4]> {
        let mut quads = QuadPipeline::new(&device, format);
        quads.upload(
            &device,
            &queue,
            [size as f32, size as f32],
            &[QuadInstance {
                pos: [0.0, 0.0],
                size: [size as f32, size as f32],
                color: [0.0, 0.0, 0.0, 0.5],
            }],
        );
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-premultiplied-clear-target"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut presentation = postmultiplied.then(|| {
            present::PresentationPipeline::new(
                &device,
                format,
                kettle_core::GraphicsBudget::default(),
            )
        });
        if let Some(presentation) = presentation.as_mut()
            && !presentation.ensure_target(&device, size, size)
        {
            return Err(anyhow!("GPU fixture presentation target allocation failed"));
        }
        let scene_view = presentation
            .as_ref()
            .and_then(present::PresentationPipeline::scene_view)
            .unwrap_or(&view);
        let bytes_per_row = 256_u32;
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-premultiplied-clear-readback"),
            size: u64::from(bytes_per_row) * u64::from(size),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-premultiplied-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: scene_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(clear),
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
        if let Some(presentation) = presentation.as_ref() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-postmultiplied-presentation-test-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
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
            presentation.draw(&mut pass);
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
                    bytes_per_row: Some(bytes_per_row),
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
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow!("GPU test poll failed: {e:?}"))?;
        await_test_mapping(rx)?;
        let data = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("GPU test mapped range failed: {e:?}"))?;
        let offset = (size as usize / 2) * bytes_per_row as usize + (size as usize / 2) * 4;
        let texel = [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ];
        drop(data);
        readback.unmap();
        Ok(texel)
    };

    let mut premultiplied = render(
        surface_clear_color(bg, alpha, wgpu::CompositeAlphaMode::PreMultiplied),
        false,
    )?;
    unpremultiply_rgba8(&mut premultiplied, true);
    let postmultiplied = render(
        surface_clear_color(bg, alpha, wgpu::CompositeAlphaMode::PreMultiplied),
        true,
    )?;
    let mut straight = render(
        wgpu::Color {
            r: srgb(bg.r),
            g: srgb(bg.g),
            b: srgb(bg.b),
            a: alpha,
        },
        false,
    )?;
    unpremultiply_rgba8(&mut straight, true);
    Ok(Some((premultiplied, straight, postmultiplied)))
}

#[test]
fn pane_osc11_bases_replace_without_opacity_or_color_compounding() {
    let _serialized = gpu_test_guard();
    let replace = [
        QuadInstance {
            pos: [0.0, 0.0],
            size: [8.0, 4.0],
            color: [1.0, 0.0, 0.0, 0.5],
        },
        QuadInstance {
            pos: [4.0, 0.0],
            size: [4.0, 4.0],
            color: [0.0, 0.0, 1.0, 0.5],
        },
    ];
    let Some((left, right)) =
        pollster::block_on(render_quad_layers(&replace, &[])).expect("GPU fixture renders")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };

    assert!(left[0] >= 250 && left[1] <= 2 && left[2] <= 2);
    assert!(right[0] <= 2 && right[1] <= 2 && right[2] >= 250);
    assert!(left[3].abs_diff(128) <= 1, "left alpha was {}", left[3]);
    assert!(
        right[3].abs_diff(128) <= 1,
        "right alpha compounded instead of staying 0.5: {}",
        right[3]
    );
}

#[test]
fn wallpaper_darkness_endpoints_are_painted_after_the_wallpaper() {
    let _serialized = gpu_test_guard();
    let wallpaper = [QuadInstance {
        pos: [0.0, 0.0],
        size: [8.0, 4.0],
        color: [1.0, 1.0, 1.0, 1.0],
    }];
    let pane_layers = [
        QuadInstance {
            pos: [0.0, 0.0],
            size: [4.0, 4.0],
            color: [0.0, 0.0, 0.0, 0.0],
        },
        QuadInstance {
            pos: [4.0, 0.0],
            size: [4.0, 4.0],
            color: [0.0, 0.0, 0.0, 1.0],
        },
    ];
    let Some((visible, covered)) = pollster::block_on(render_quad_layers(&wallpaper, &pane_layers))
        .expect("GPU fixture renders")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };

    assert!(visible[..3].iter().all(|&channel| channel >= 250));
    assert!(covered[..3].iter().all(|&channel| channel <= 2));
    assert_eq!(visible[3], 255);
    assert_eq!(covered[3], 255);
}

/// Compare shared and standalone quad pixels for overlapping translucent
/// OSC 11 bases and for wallpaper dimmed at alpha 0.0 and 0.6.
/// The calling headless test holds the GPU test guard.
pub(crate) fn assert_shared_quad_pixels_match_standalone() {
    let bases = [
        QuadInstance {
            pos: [0.0, 0.0],
            size: [8.0, 4.0],
            color: [1.0, 0.0, 0.0, 0.5],
        },
        QuadInstance {
            pos: [4.0, 0.0],
            size: [4.0, 4.0],
            color: [0.0, 0.0, 1.0, 0.5],
        },
    ];
    let wallpaper = [QuadInstance {
        pos: [0.0, 0.0],
        size: [8.0, 4.0],
        color: [1.0, 1.0, 1.0, 1.0],
    }];
    let dimmed = [
        QuadInstance {
            pos: [0.0, 0.0],
            size: [4.0, 4.0],
            color: [0.0, 0.0, 0.0, 0.0],
        },
        QuadInstance {
            pos: [4.0, 0.0],
            size: [4.0, 4.0],
            color: [0.0, 0.0, 0.0, 0.6],
        },
    ];
    for (replace, blend) in [(&bases[..], &[][..]), (&wallpaper[..], &dimmed[..])] {
        let standalone = pollster::block_on(render_quad_layers_with(replace, blend, false))
            .expect("GPU fixture renders")
            .expect("pixel fixture renders after the headless adapter was available");
        let shared = pollster::block_on(render_quad_layers_with(replace, blend, true))
            .expect("GPU fixture renders")
            .expect("the adapter that drew the standalone layers draws the shared ones");
        assert_eq!(shared, standalone);
    }
}

/// Draw `replace` then `blend` through standalone layers.
async fn render_quad_layers(
    replace: &[QuadInstance],
    blend: &[QuadInstance],
) -> Result<Option<([u8; 4], [u8; 4])>> {
    render_quad_layers_with(replace, blend, false).await
}

/// Draw `replace` then `blend` into an 8x4 target and read back one pixel
/// from each half, through `SharedPipelines`' quad pipelines when `shared`
/// and through standalone ones otherwise.
async fn render_quad_layers_with(
    replace: &[QuadInstance],
    blend: &[QuadInstance],
    shared: bool,
) -> Result<Option<([u8; 4], [u8; 4])>> {
    let cfg = gpu_test_config();
    let (_instance, adapter) = match resolve_headless_adapter(&cfg, "pane_base_layers_test").await {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_limits: live_device_limits(adapter.limits()),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("GPU test device request failed: {e:?}"))?;
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let (mut replace_pipeline, mut blend_pipeline) = if shared {
        let pipelines = SharedPipelines::new(&device, format);
        (
            pipelines.quads.replace(&device),
            pipelines.quads.blend(&device),
        )
    } else {
        (
            QuadPipeline::new_replace(&device, format),
            QuadPipeline::new(&device, format),
        )
    };
    replace_pipeline.upload(&device, &queue, [8.0, 4.0], replace);
    blend_pipeline.upload(&device, &queue, [8.0, 4.0], blend);

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-pane-base-test-target"),
        size: wgpu::Extent3d {
            width: 8,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-pane-base-test-readback"),
        size: 256 * 4,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-pane-base-test-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
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
        replace_pipeline.draw(&mut pass);
        blend_pipeline.draw(&mut pass);
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
                bytes_per_row: Some(256),
                rows_per_image: Some(4),
            },
        },
        wgpu::Extent3d {
            width: 8,
            height: 4,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(encoder.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("GPU test poll failed: {e:?}"))?;
    await_test_mapping(rx)?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("GPU test mapped range failed: {e:?}"))?;
    let sample = |x: usize| {
        let offset = 2 * 256 + x * 4;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    };
    let mut left = sample(2);
    let mut right = sample(6);
    drop(data);
    readback.unmap();
    unpremultiply_rgba8(&mut left, true);
    unpremultiply_rgba8(&mut right, true);
    Ok(Some((left, right)))
}

fn assert_windows_default_backend(selected: wgpu::Backend, dx12_available: bool) {
    if dx12_available {
        assert_eq!(
            selected,
            wgpu::Backend::Dx12,
            "available DX12 must be selected"
        );
    }
}

#[test]
#[should_panic(expected = "available DX12 must be selected")]
fn available_dx12_rejects_a_vulkan_default() {
    assert_windows_default_backend(wgpu::Backend::Vulkan, true);
}

#[cfg(target_os = "windows")]
#[test]
fn windows_auto_prefers_dx12_without_preinitializing_vulkan() {
    let _serialized = gpu_test_guard();
    pollster::block_on(async {
        // Exercise the real default resolver first. Do not construct an
        // all-backend discovery instance to decide whether this assertion
        // runs. That would initialize Vulkan before the DX12-only path
        // under test.
        let cfg = gpu_test_config();
        let (_instance, adapter) = resolve_headless_adapter(&cfg, "windows_auto_policy_test")
            .await
            .expect("resolve default Windows adapter");
        let selected = adapter.get_info().backend;
        let dx12 = gpu_instance_for_backends(wgpu::Backends::DX12);
        let dx12_available = request_direct_adapter(&dx12, None, &cfg, cfg.gpu_force_software)
            .await
            .is_ok_and(|adapter| {
                cfg.gpu_force_software || adapter.get_info().device_type != wgpu::DeviceType::Cpu
            });
        assert_windows_default_backend(selected, dx12_available);

        let unavailable = Config {
            gpu_backend: kettle_config::GpuBackend::Metal,
            ..gpu_test_config()
        };
        let (_instance, adapter) =
            resolve_headless_adapter(&unavailable, "windows_backend_fallback_test")
                .await
                .expect("fall back from unavailable Metal on Windows");
        assert_windows_default_backend(adapter.get_info().backend, dx12_available);
    });
}

#[cfg(target_os = "windows")]
#[test]
fn windows_stale_auto_pin_preserves_dx12_platform_preference() {
    let _serialized = gpu_test_guard();
    if cfg!(target_arch = "aarch64") {
        eprintln!("hardware adapter policy is covered by Windows x64 CI; skipped on ARM64");
        return;
    }
    pollster::block_on(async {
        // Keep this regression DX12-only too. A stale pin forces the
        // cross-adapter resolver, but its Auto fallback must still select
        // the physical adapter returned by wgpu/the platform rather than
        // the lowest numeric PCI vendor id.
        let instance = gpu_instance_for_backends(wgpu::Backends::DX12);
        let cfg = Config {
            gpu_name: "__kettle_missing_adapter_regression__".to_string(),
            ..Config::default()
        };
        let Some(expected) = preferred_adapter_key(&instance, None, &cfg).await else {
            eprintln!("DX12 adapter unavailable on this Windows host; skipped");
            return;
        };
        let chosen = resolve_adapter(
            &instance,
            None,
            &cfg,
            AdapterEscalation::Preferred,
            None,
            "windows_stale_pin_policy_test",
        )
        .await
        .expect("stale pin must fall back to the Auto power policy");
        assert_eq!(
            GpuAdapterKey::from_info(&chosen.get_info()),
            expected,
            "stale Auto pin fallback must preserve wgpu's physical adapter"
        );
    });
}

#[cfg(target_os = "windows")]
#[test]
fn windows_explicit_vulkan_needs_no_gpu_pin_when_available() {
    let _serialized = gpu_test_guard();
    if cfg!(target_arch = "aarch64") {
        eprintln!("hardware adapter policy is covered by Windows x64 CI; skipped on ARM64");
        return;
    }
    pollster::block_on(async {
        // This is deliberately isolated from the default-DX12 regression
        // above and enables only Vulkan while checking availability.
        let probe = gpu_instance_for_backends(wgpu::Backends::VULKAN);
        let has_vulkan_hardware = probe
            .enumerate_adapters(wgpu::Backends::VULKAN)
            .await
            .into_iter()
            .any(|adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu);
        if !has_vulkan_hardware {
            eprintln!("Vulkan hardware unavailable on this Windows host; skipped");
            return;
        }

        let cfg = Config {
            gpu_backend: kettle_config::GpuBackend::Vulkan,
            gpu_vendor_id: 0,
            gpu_device_id: 0,
            gpu_name: String::new(),
            ..Config::default()
        };
        let (_instance, adapter) = resolve_headless_adapter(&cfg, "windows_explicit_policy_test")
            .await
            .expect("resolve explicit Vulkan adapter without a GPU pin");
        assert_eq!(adapter.get_info().backend, wgpu::Backend::Vulkan);
    });
}

#[test]
fn screenshot_rejects_padding_larger_than_texture_limit() {
    let _serialized = gpu_test_guard();
    let parsed = Config::parse_text("window-padding-x = 100000\n");
    let mut cfg = Config {
        padding_x: parsed.padding_x,
        ..gpu_test_config()
    };
    assert_eq!(cfg.padding_x, 100_000.0);
    if pollster::block_on(resolve_headless_adapter(&cfg, "screenshot_padding")).is_err() {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    }
    let dir = tempfile::tempdir().expect("screenshot directory");
    let out = dir.path().join("oversized-padding.png");
    for (language, preference) in [
        (Language::En, kettle_config::LanguagePreference::English),
        (Language::Es, kettle_config::LanguagePreference::Spanish),
    ] {
        cfg.language = preference;
        let error = capture_png_with(&cfg, 80, 24, &out, DebugScene::Default)
            .expect_err("oversized padding must return an error");
        assert_eq!(
            error.to_string(),
            Translator::new(language).text(Text::ScreenshotDimensionsTooLarge)
        );
        assert!(!out.exists(), "rejected screenshot must not create a PNG");
    }
}

/// The offscreen Grid fixture must emit cell-locked glyphs through the
/// shared loop used by the live renderer.
#[test]
fn screenshot_grid_emits_cell_locked_glyphs() {
    let _serialized = gpu_test_guard();
    pollster::block_on(async {
        let cfg = gpu_test_config();
        let (_instance, adapter) =
            match resolve_headless_adapter(&cfg, "screenshot_grid_emit").await {
                Ok(value) => value,
                Err(_) => {
                    eprintln!("no GPU adapter on this host; skipped");
                    return;
                }
            };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-screenshot-grid-emit-test"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .expect("request_device");
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        // Default config selects the Grid renderer (the case the screenshot
        // path now honors). Guard that assumption so this stays meaningful.
        assert_eq!(
            cfg.text_renderer,
            TextRendererMode::Grid,
            "default text-renderer must be Grid for this fix to matter"
        );
        let family = cfg.font_family.clone();
        let mut font_system = FontSystem::new();
        for face in kettle_config::font::all() {
            load_bundled_font(&mut font_system, face);
        }
        let mut swash = SwashCache::new();
        let mut glyph_pipe = GlyphPipeline::new(&device, format);

        let metrics = Metrics::new(24.0, 30.0);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let (cw, _ch) = measure_cell(&mut font_system, &mut measure, &family, metrics);

        let mut buf = TextBuffer::new(&mut font_system, metrics);
        buf.set_size(Some(2048.0), Some(512.0));
        buf.set_wrap(Wrap::None);
        buf.set_text(
            "kevim@kettle:~/Repos/kettle$ cargo test",
            &Attrs::new().family(Family::Name(&family)),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut font_system, false);

        let mut instances = Vec::new();
        let mut starts = Vec::new();
        let default_color = GColor::rgb(
            cfg.theme.foreground.r,
            cfg.theme.foreground.g,
            cfg.theme.foreground.b,
        );
        emit_cell_locked_glyphs(
            &mut instances,
            &buf,
            (12.0, 12.0),
            cw,
            default_color,
            &mut glyph_pipe,
            &mut swash,
            &mut font_system,
            &device,
            &queue,
            &mut starts,
        );
        assert!(
            instances.len() > 20,
            "the Grid screenshot path must emit a non-empty cell-locked glyph \
                 set; got {} glyphs",
            instances.len()
        );
        assert!(
            swash.image_cache.is_empty(),
            "grid bitmaps must be released after atlas upload"
        );
    });
}

/// Regression guard for the grid renderer/cursor interaction. The
/// prompt glyphs are uploaded ONCE through the cell-locked glyph pipeline,
/// then two offscreen frames are rendered while only the cursor quad toggles.
/// Every non-cursor pixel must stay byte-identical; a blink may change the
/// cursor cell only. Keep several prompt shapes here because the invariant
/// is renderer-wide, not specific to zsh.
#[test]
fn grid_prompt_pixels_survive_cursor_blink() {
    let _serialized = gpu_test_guard();
    let Some((off, on, cursor_rect, prompt_rects)) =
        grid_prompt_blink_frames().expect("grid prompt blink frames render")
    else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    assert_eq!(
        (off.width(), off.height()),
        (on.width(), on.height()),
        "both blink phases must render the same surface size"
    );

    let mut changed_outside_cursor = 0u64;
    let bg = Config::default().theme.background;
    for (idx, prompt_rect) in prompt_rects.iter().enumerate() {
        let mut prompt_ink = 0u64;
        for y in prompt_rect.1..prompt_rect.1 + prompt_rect.3 {
            for x in prompt_rect.0..prompt_rect.0 + prompt_rect.2 {
                let in_cursor = x >= cursor_rect.0
                    && x < cursor_rect.0 + cursor_rect.2
                    && y >= cursor_rect.1
                    && y < cursor_rect.1 + cursor_rect.3;
                let a = off.get_pixel(x, y);
                let b = on.get_pixel(x, y);
                if !in_cursor && a != b {
                    changed_outside_cursor += 1;
                }
                if !in_cursor
                    && ((a[0] as i16 - bg.r as i16).abs() > 6
                        || (a[1] as i16 - bg.g as i16).abs() > 6
                        || (a[2] as i16 - bg.b as i16).abs() > 6)
                {
                    prompt_ink += 1;
                }
            }
        }
        assert!(
            prompt_ink > 80,
            "expected visible prompt glyph ink outside cursor for fixture {idx}; got {prompt_ink} pixels"
        );
    }
    assert_eq!(
        changed_outside_cursor, 0,
        "cursor blink changed {changed_outside_cursor} non-cursor prompt pixels"
    );
}

type BlinkFrames = (
    image::RgbaImage,
    image::RgbaImage,
    (u32, u32, u32, u32),
    Vec<(u32, u32, u32, u32)>,
);

fn grid_prompt_blink_frames() -> Result<Option<BlinkFrames>> {
    grid_prompt_blink_frames_for(wgpu::TextureFormat::Rgba8UnormSrgb, None)
}

#[test]
fn mask_foreground_roundtrips_on_a_non_srgb_target() {
    let _serialized = gpu_test_guard();
    let Some((off, _, _, _)) = grid_prompt_blink_frames_for(
        wgpu::TextureFormat::Rgba8Unorm,
        Some(Rgb::new(128, 128, 128)),
    )
    .expect("non-sRGB glyph fixture renders") else {
        eprintln!("no GPU adapter on this host; skipped");
        return;
    };
    let peak = off.pixels().map(|pixel| pixel[0]).max().unwrap();
    assert!(
        (125..=131).contains(&peak),
        "mask foreground peak was {peak}, expected 128"
    );
}

fn grid_prompt_blink_frames_for(
    format: wgpu::TextureFormat,
    foreground: Option<Rgb>,
) -> Result<Option<BlinkFrames>> {
    pollster::block_on(async {
        let cfg = gpu_test_config();
        let (_instance, adapter) = match resolve_headless_adapter(&cfg, "grid_prompt_blink").await {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-grid-prompt-blink"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("device: {e:?}"))?;
        let theme = cfg.theme;
        let family = cfg.font_family.clone();
        let mut font_system = FontSystem::new();
        for face in kettle_config::font::all() {
            load_bundled_font(&mut font_system, face);
        }
        let mut swash = SwashCache::new();
        let mut glyph_pipe = GlyphPipeline::new(&device, format);
        let mut quads = QuadPipeline::new(&device, format);

        let metrics = Metrics::new(24.0, 30.0);
        let mut measure = TextBuffer::new(&mut font_system, metrics);
        let (cw, ch) = measure_cell(&mut font_system, &mut measure, &family, metrics);
        let fixtures = if foreground.is_some() {
            ["MMMM", "HHHH", "WWWW", "####", "@@@@"]
        } else {
            [
                "➜  ~",
                "$ ~/project",
                "λ ~/src",
                "❯ git status",
                "PS C:\\Users\\dev>",
            ]
        };
        let max_cols = fixtures
            .iter()
            .map(|s| s.chars().count())
            .max()
            .unwrap_or(1);
        let w = (cw * (max_cols as f32 + 2.0) + 24.0).ceil() as u32;
        let h = (ch * fixtures.len() as f32 + 24.0).ceil() as u32;
        let origin = (12.0_f32, 12.0_f32);
        let cursor_col = 4usize;
        let cursor_rect = (
            (origin.0 + cursor_col as f32 * cw).round() as u32,
            origin.1.round() as u32,
            cw.ceil() as u32,
            ch.ceil() as u32,
        );
        let prompt_rects: Vec<(u32, u32, u32, u32)> = fixtures
            .iter()
            .enumerate()
            .map(|(row, text)| {
                (
                    origin.0.round() as u32,
                    (origin.1 + row as f32 * ch).round() as u32,
                    (cw * (text.chars().count() as f32 + 1.0)).ceil() as u32,
                    ch.ceil() as u32,
                )
            })
            .collect();

        let mut buf = TextBuffer::new(&mut font_system, metrics);
        buf.set_metrics(metrics);
        buf.set_size(Some(w as f32), Some(h as f32));
        buf.set_wrap(Wrap::None);
        buf.set_text(
            &fixtures.join("\n"),
            &Attrs::new().family(Family::Name(&family)),
            Shaping::Advanced,
            None,
        );
        buf.shape_until_scroll(&mut font_system, false);

        let mut instances = Vec::new();
        let mut starts = Vec::new();
        let foreground = foreground.unwrap_or(theme.foreground);
        let default_color = GColor::rgb(foreground.r, foreground.g, foreground.b);
        // Same cell-lock emit the live renderer + screenshot path use.
        emit_cell_locked_glyphs(
            &mut instances,
            &buf,
            origin,
            cw,
            default_color,
            &mut glyph_pipe,
            &mut swash,
            &mut font_system,
            &device,
            &queue,
            &mut starts,
        );
        glyph_pipe.upload(&device, &queue, [w as f32, h as f32], &instances);
        let clips = [GlyphClip {
            rect: [0.0, 0.0, w as f32, h as f32],
            start: 0,
            count: instances.len() as u32,
        }];

        let off = render_grid_prompt_frame(
            &device,
            &queue,
            format,
            &mut quads,
            &glyph_pipe,
            &clips,
            (w, h),
            theme.background,
            None,
        )?;
        let on = render_grid_prompt_frame(
            &device,
            &queue,
            format,
            &mut quads,
            &glyph_pipe,
            &clips,
            (w, h),
            theme.background,
            Some((
                cursor_rect.0 as f32,
                cursor_rect.1 as f32,
                cursor_rect.2 as f32,
                cursor_rect.3 as f32,
                theme.cursor,
            )),
        )?;
        Ok(Some((off, on, cursor_rect, prompt_rects)))
    })
}

#[allow(clippy::too_many_arguments)]
fn render_grid_prompt_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    quads: &mut QuadPipeline,
    glyph_pipe: &GlyphPipeline,
    clips: &[GlyphClip],
    size: (u32, u32),
    bg: Rgb,
    cursor: Option<(f32, f32, f32, f32, Rgb)>,
) -> Result<image::RgbaImage> {
    let (w, h) = size;
    let mut q = Vec::new();
    if let Some((x, y, cw, ch, color)) = cursor {
        q.push(rect(x, y, cw, ch, color, 1.0));
    }
    quads.upload(device, queue, [w as f32, h as f32], &q);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("kettle-grid-prompt-target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    let bpp = 4u32;
    let unpadded = w * bpp;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("kettle-grid-prompt-readback"),
        size: (padded * h) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("kettle-grid-prompt-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: srgb(bg.r),
                        g: srgb(bg.g),
                        b: srgb(bg.b),
                        a: 1.0,
                    }),
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
        glyph_pipe.draw(&mut pass, clips, [w, h]);
    }
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(std::iter::once(enc.finish()));
    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| anyhow!("GPU test poll failed: {e:?}"))?;
    rx.recv()
        .map_err(|_| anyhow!("map channel closed"))?
        .map_err(|e| anyhow!("buffer map failed: {e:?}"))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| anyhow!("grid fixture mapped range failed: {e:?}"))?;
    let mut pixels = Vec::with_capacity((unpadded * h) as usize);
    for row in 0..h {
        let start = (row * padded) as usize;
        pixels.extend_from_slice(&data[start..start + unpadded as usize]);
    }
    drop(data);
    readback.unmap();
    image::RgbaImage::from_raw(w, h, pixels).ok_or_else(|| anyhow!("image buffer size mismatch"))
}

/// Render one solid quad of a known *dark* sRGB color (#1a1b23) covering
/// an sRGB target and read pixel (0,0) back. `Ok(None)` on a GPU-less host.
fn srgb_quad_roundtrip_sample() -> Result<Option<[u8; 3]>> {
    pollster::block_on(async {
        let cfg = gpu_test_config();
        let (_instance, adapter) = match resolve_headless_adapter(&cfg, "srgb_quad_roundtrip").await
        {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("kettle-srgb-test"),
                required_limits: live_device_limits(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|e| anyhow!("device: {e:?}"))?;

        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut quads = QuadPipeline::new(&device, format);
        quads.upload(
            &device,
            &queue,
            [8.0, 8.0],
            &[QuadInstance {
                pos: [0.0, 0.0],
                size: [8.0, 8.0],
                color: [26.0 / 255.0, 27.0 / 255.0, 35.0 / 255.0, 1.0],
            }],
        );
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-srgb-target"),
            size: wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (8u32 * 4).div_ceil(align) * align;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-srgb-readback"),
            size: (padded * 8) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("kettle-srgb-pass"),
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
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(8),
                },
            },
            wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));

        let slice = staging.slice(..);
        let done = Arc::new(AtomicBool::new(false));
        let done_set = done.clone();
        slice.map_async(wgpu::MapMode::Read, move |_| {
            done_set.store(true, Ordering::SeqCst);
        });
        device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| anyhow!("GPU test poll failed: {e:?}"))?;
        if !done.load(Ordering::SeqCst) {
            return Err(anyhow!("srgb readback timed out"));
        }
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| anyhow!("sRGB mapped range failed: {e:?}"))?;
        let px = [mapped[0], mapped[1], mapped[2]];
        drop(mapped);
        staging.unmap();
        Ok(Some(px))
    })
}

/// Drift guard. A dark sRGB quad (#1a1b23) drawn to an sRGB
/// target must read back ≈ #1a1b23, NOT the gamma-lifted ~#5a5f68 that the
/// missing sRGB→linear decode in the quad shader produced (full-screen
/// TUIs like AstroNvim set an explicit bg on every cell, so the lift
/// washed out the whole screen). Allows a few units per channel for the
/// linear↔sRGB round-trip + 8-bit quantization.
#[test]
fn quad_pipeline_does_not_gamma_lift_on_srgb_target() {
    let _serialized = gpu_test_guard();
    match srgb_quad_roundtrip_sample() {
        Ok(None) => eprintln!("no GPU adapter on this host; skipped"),
        Ok(Some([r, g, b])) => {
            assert!(
                r < 40 && g < 40 && b < 48,
                "quad gamma-lifted: got #{r:02x}{g:02x}{b:02x}, expected ≈ #1a1b23 \
                     (regression: the sRGB→linear decode in quad.rs's shader was removed)"
            );
            assert!(
                r > 10 && b > 20,
                "quad crushed too dark: #{r:02x}{g:02x}{b:02x}"
            );
        }
        Err(e) => panic!("srgb round-trip render failed: {e}"),
    }
}

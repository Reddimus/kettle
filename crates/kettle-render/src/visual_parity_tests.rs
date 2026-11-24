//! Replay existing headless visual fixtures on both renderers. Each replay
//! starts with fresh renderers and follows the same state changes, so cached
//! frames and first frames are both checked.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::Path;

use image::{GenericImageView, Rgba, RgbaImage};

use crate::{Config, Renderer, RendererBackend};

macro_rules! parity_scene {
    ($(#[$attr:meta])* fn $name:ident() $body:block) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            fn scene() $body
            $crate::visual_parity_tests::check_scene(stringify!($name), scene);
        }
    };
}
pub(crate) use parity_scene;

enum Pass {
    Record(VecDeque<RgbaImage>),
    Compare {
        name: &'static str,
        expected: VecDeque<RgbaImage>,
        frame: usize,
        failed: usize,
    },
}

thread_local! {
    // Captures stay on the test thread. A scoped pass lets helpers shared by
    // several fixtures participate without changing their inputs or layouts.
    static PASS: RefCell<Option<Pass>> = const { RefCell::new(None) };
}

fn run(pass: Pass, scene: fn()) -> Pass {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            PASS.take();
        }
    }
    let _reset = Reset;
    assert!(
        PASS.replace(Some(pass)).is_none(),
        "nested visual parity fixture"
    );
    scene();
    PASS.take().unwrap()
}

pub(crate) fn check_scene(name: &'static str, scene: fn()) {
    if !cfg!(target_os = "linux") {
        scene();
        return;
    }
    let Pass::Record(expected) = run(Pass::Record(VecDeque::new()), scene) else {
        unreachable!()
    };
    if expected.is_empty() {
        eprintln!("visual parity {name}: fixture skipped before its first capture");
        return;
    }
    let Pass::Compare {
        expected,
        failed,
        frame,
        ..
    } = run(
        Pass::Compare {
            name,
            expected,
            frame: 0,
            failed: 0,
        },
        scene,
    )
    else {
        unreachable!()
    };
    assert!(
        expected.is_empty(),
        "{name}: CPU replay ended before the GPU replay"
    );
    assert_eq!(
        failed, 0,
        "{name}: {failed} of {frame} captures exceeded visual parity tolerance"
    );
}

pub(crate) fn renderer(cfg: &Config, width: u32, height: u32) -> anyhow::Result<Option<Renderer>> {
    renderer_with(cfg, width, height, 1.0, false)
}

pub(crate) fn renderer_with(
    cfg: &Config,
    width: u32,
    height: u32,
    scale: f32,
    translucent: bool,
) -> anyhow::Result<Option<Renderer>> {
    renderer_with_mapping(cfg, width, height, scale, translucent, true)
}

pub(crate) fn renderer_with_mapping(
    cfg: &Config,
    width: u32,
    height: u32,
    scale: f32,
    translucent: bool,
    allow_mapping: bool,
) -> anyhow::Result<Option<Renderer>> {
    if PASS.with_borrow(|pass| matches!(pass, Some(Pass::Compare { .. }))) {
        Renderer::headless_for_tests_on(
            RendererBackend::Cpu,
            cfg,
            width,
            height,
            scale,
            translucent,
        )
    } else {
        let gpu = Renderer::headless_for_tests_with_mapping(
            cfg,
            width,
            height,
            scale,
            translucent,
            allow_mapping,
        )?;
        if gpu.is_none() && PASS.with_borrow(Option::is_some) {
            crate::headless_tests::no_gpu_adapter();
        }
        Ok(gpu)
    }
}

pub(crate) fn observe(backend: RendererBackend, image: &RgbaImage) {
    PASS.with_borrow_mut(|pass| match pass {
        Some(Pass::Record(frames)) => {
            assert_eq!(backend, RendererBackend::Gpu);
            frames.push_back(image.clone());
        }
        Some(Pass::Compare {
            name,
            expected,
            frame,
            failed,
        }) => {
            assert_eq!(backend, RendererBackend::Cpu);
            *frame += 1;
            let gpu = expected
                .pop_front()
                .expect("CPU replay captured an extra frame");
            *failed += usize::from(!compare(name, *frame, &gpu, image));
        }
        None => {}
    });
}

#[derive(Clone, Copy)]
struct Tolerance {
    max: u8,
    over_two: u64,
    low_alpha_only: bool,
}

impl Tolerance {
    const fn new(max: u8, over_two: u64) -> Self {
        Self {
            max,
            over_two,
            low_alpha_only: false,
        }
    }

    fn permits(self, gpu: &Rgba<u8>, cpu: &Rgba<u8>, delta: u8) -> bool {
        delta <= 2
            || delta <= self.max && (!self.low_alpha_only || (gpu[3] == cpu[3] && gpu[3] < 64))
    }
}

const STRICT: Tolerance = Tolerance::new(2, 0);
const ZERO_BACKGROUND_CURSOR: Tolerance = Tolerance {
    max: 4,
    over_two: 4,
    low_alpha_only: true,
};

// Each exception names the affected fixture and limits the affected pixels.
const EXCEPTIONS: &[(&str, usize, Tolerance)] = &[
    // The demo menu's translucent border and shadow stack over sample text.
    ("capture-menu-60", 1, Tolerance::new(4, 16)),
    ("capture-menu", 1, Tolerance::new(4, 16)),
    // The child shadow is composited over the parent's panel, border,
    // selection and ink. Each attachment store rounds back to 8-bit sRGB.
    (
        "submenu_covers_its_parents_selection_labels_and_shadowed_edge",
        2,
        Tolerance::new(3, 16),
    ),
    (
        "submenu_edge_hides_its_parents_selection_and_labels",
        3,
        Tolerance::new(3, 32),
    ),
    // The three zero-opacity surface cases divide edge RGB by glyph alpha
    // in the PNG, amplifying a one-code premultiplied difference.
    (
        "the_patch_over_the_off_frame_is_the_on_frame",
        77,
        ZERO_BACKGROUND_CURSOR,
    ),
    (
        "the_patch_over_the_off_frame_is_the_on_frame",
        78,
        ZERO_BACKGROUND_CURSOR,
    ),
    (
        "the_patch_over_the_off_frame_is_the_on_frame",
        89,
        ZERO_BACKGROUND_CURSOR,
    ),
    (
        "the_patch_over_the_off_frame_is_the_on_frame",
        90,
        ZERO_BACKGROUND_CURSOR,
    ),
    (
        "the_patch_over_the_off_frame_is_the_on_frame",
        97,
        ZERO_BACKGROUND_CURSOR,
    ),
    (
        "the_patch_over_the_off_frame_is_the_on_frame",
        98,
        ZERO_BACKGROUND_CURSOR,
    ),
];

/// Counts grouped by maximum RGBA channel delta (0, 1, 2, or above 2).
pub(crate) fn pixel_counts(a: &[u8], b: &[u8]) -> [u64; 4] {
    assert_eq!(a.len(), b.len());
    let mut counts = [0; 4];
    for (a, b) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        let delta = a.iter().zip(b).map(|(x, y)| x.abs_diff(*y)).max().unwrap();
        counts[usize::from(delta.min(3))] += 1;
    }
    counts
}

pub(crate) fn compare(name: &str, frame: usize, gpu: &RgbaImage, cpu: &RgbaImage) -> bool {
    assert_eq!(gpu.dimensions(), cpu.dimensions(), "{name}/{frame}");
    let tolerance = EXCEPTIONS
        .iter()
        .find(|&&(scene, index, _)| scene == name && index == frame)
        .map_or(STRICT, |&(_, _, tolerance)| tolerance);
    let mut histogram = [0u64; 4];
    let mut sum = 0u64;
    let mut worst = 0;
    let mut valid_pixels = true;
    let mut bounds = (cpu.width(), cpu.height(), 0, 0);
    for ((x, y, cpu), gpu) in cpu.enumerate_pixels().zip(gpu.pixels()) {
        let channels = std::array::from_fn::<_, 4, _>(|c| cpu[c].abs_diff(gpu[c]));
        sum += channels.into_iter().map(u64::from).sum::<u64>();
        let delta = channels.into_iter().max().unwrap();
        histogram[usize::from(delta.min(3))] += 1;
        worst = worst.max(delta);
        if delta > 2 {
            valid_pixels &= tolerance.permits(gpu, cpu, delta);
            bounds = (
                bounds.0.min(x),
                bounds.1.min(y),
                bounds.2.max(x + 1),
                bounds.3.max(y + 1),
            );
        }
    }
    let mean = sum as f64 / cpu.as_raw().len() as f64;
    let over_two = histogram[3];
    let passed = valid_pixels && over_two <= tolerance.over_two;
    eprintln!(
        "VISUAL_PARITY {name}/{frame} {}x{} max={worst} mean={mean:.8} pixels1={} pixels2={} pixels_over2={over_two} {}",
        cpu.width(),
        cpu.height(),
        histogram[1],
        histogram[2],
        if passed { "PASS" } else { "FAIL" }
    );
    if over_two > 0
        && let Some(dir) = std::env::var_os("KETTLE_CPU_PARITY_DUMP")
    {
        dump(Path::new(&dir), name, frame, gpu, cpu, bounds);
    }
    passed
}

fn dump(
    dir: &Path,
    name: &str,
    frame: usize,
    gpu: &RgbaImage,
    cpu: &RgbaImage,
    bounds: (u32, u32, u32, u32),
) {
    let (x0, y0, x1, y1) = bounds;
    let x = x0.saturating_sub(4);
    let y = y0.saturating_sub(4);
    let w = (x1 + 4).min(cpu.width()) - x;
    let h = (y1 + 4).min(cpu.height()) - y;
    let cpu = image::imageops::crop_imm(cpu, x, y, w, h);
    let gpu = image::imageops::crop_imm(gpu, x, y, w, h);
    let diff = RgbaImage::from_fn(w, h, |px, py| {
        let a = cpu.get_pixel(px, py);
        let b = gpu.get_pixel(px, py);
        let alpha = a[3].abs_diff(b[3]);
        Rgba(std::array::from_fn(|c| {
            if c == 3 {
                255
            } else {
                a[c].abs_diff(b[c]).max(alpha).saturating_mul(16)
            }
        }))
    });
    std::fs::create_dir_all(dir).expect("create visual parity artifact directory");
    let stem = format!("{name}-{frame}-x{x}-y{y}");
    for (suffix, image) in [
        ("gpu", gpu.to_image()),
        ("cpu", cpu.to_image()),
        ("diff16", diff),
    ] {
        image
            .save(dir.join(format!("{stem}-{suffix}.png")))
            .expect("write visual parity crop");
    }
}

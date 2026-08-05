//! Procedural GPU starfield background (v2.24.0). A slow forward-flight field
//! of soft-glowing, subtly-colored stars rendered entirely in a WGSL fragment
//! shader — no decoded frames, so it costs ~zero memory (vs the 253 MB an
//! equivalent 1080p GIF decoded to), loops perfectly, and stays crisp at any
//! resolution/aspect ratio. It is the base wallpaper layer: drawn opaque before
//! the chrome + cell-background quads, which composite over it.
//!
//! Look (per the v2.24.0 design): pure-black sky, stars that emerge near the
//! center and drift outward as the camera moves forward, **fading in as they get
//! closer**, with a bright core + soft halo and faint stellar color variation
//! (cool blue-white / white / warm). The motion is deliberately slow; the
//! event loop repaints it at a low fps cap (~10) so idle CPU stays low, while
//! the shader's `time` is continuous so each repaint shows the exact position.
//!
//! The shader mirrors the screen-space radial model proven in
//! `scripts/gen-starfield.py`.
//!
//! # Where the per-star work happens
//!
//! The fragment shader once evaluated the whole model per pixel: the hash, the
//! angle, the radial ease, the colour, and the sRGB decode all ran inside the
//! per-star loop, for every pixel on the surface. Almost none of that depends
//! on the pixel — at 4K it was roughly 456 million star-iterations per frame,
//! about ten transcendentals apiece, to compute 55 stars' worth of values over
//! and over.
//!
//! Everything pixel-independent is hoisted out. Values fixed for the lifetime
//! of the field (angle, phase, colour) are computed once in [`StarSeed::all`];
//! values that change with time or resolution (radial position, radii,
//! brightness) are computed once per frame in [`build_frame_stars`] and
//! uploaded. The shader is left with the only part that genuinely varies per
//! pixel: the distance to each star and the two falloff terms, one `exp`
//! rather than ten transcendentals.
//!
//! The brightness curve lives in [`star_brightness`], which is the function
//! the tests drive — not a copy of it.

use bytemuck::{Pod, Zeroable};

/// Star count. The starfield is a FIXED built-in example (not config-driven,
/// v2.24.1), so this lives here, not in `Config`. It is also the uniform
/// array's compile-time bound, substituted into the WGSL for the broadest
/// naga-backend support (no override-constant required).
const NSTARS: usize = 55;

/// `>1`: slow near the centre, faster near the edge.
const EASE: f32 = 1.7;
/// Baked slow forward-flight, in cycles per second.
const SPEED: f32 = 0.009;
const FADE_IN_END: f32 = 0.30;
const FADE_OUT_START: f32 = 0.92;
/// Below this a star contributes nothing worth a loop iteration. Applied on
/// the CPU now, so a dark star costs zero pixels rather than a `continue` per
/// pixel.
const MIN_VISIBLE_BRIGHTNESS: f32 = 0.002;

/// One star as the fragment shader consumes it: everything already resolved
/// except the distance to the pixel being shaded.
///
/// Packed as two `vec4`s rather than named scalars and a `vec3` because a
/// `vec3` inside a uniform array invites layout surprises across naga
/// backends, and this is the one struct whose byte layout has to agree with
/// hand-written WGSL.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct Star {
    /// `xy` = position in pixels from the surface centre; `z` = core radius
    /// squared; `w` = halo radius squared. Both radii arrive squared because
    /// the shader only ever needs them that way.
    geom: [f32; 4],
    /// `rgb` = linear colour already scaled by the star's brightness, so the
    /// shader multiplies once instead of once per falloff term; `a` unused.
    tint: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    /// Surface size in physical pixels.
    resolution: [f32; 2],
    /// How many entries of `stars` are live. Stars too dim to see are dropped
    /// on the CPU, so the shader's loop bound shrinks with them.
    count: u32,
    _pad: u32,
    stars: [Star; NSTARS],
}

/// The part of a star that never changes: its direction from the centre, its
/// phase along the flight path, and its colour.
///
/// `cos`/`sin` of the angle are stored rather than the angle, and the colour
/// is stored already decoded to linear, because nothing downstream wants
/// either in its original form.
#[derive(Clone, Copy, Debug)]
struct StarSeed {
    direction: [f32; 2],
    phase: f32,
    linear_color: [f32; 3],
}

impl StarSeed {
    /// Evaluate every star's fixed properties once.
    fn all() -> [Self; NSTARS] {
        std::array::from_fn(|index| {
            let i = index as f32;
            let theta = rnd(i + 1.0) * std::f32::consts::TAU;
            let srgb = star_color(i);
            Self {
                direction: [theta.cos(), theta.sin()],
                phase: rnd(i * 2.0 + 0.5),
                linear_color: srgb.map(srgb_to_linear),
            }
        })
    }
}

/// Deterministic index hash, `0..1`.
///
/// This is the WGSL `fract(sin(n * 12.9898) * 43758.5453)` moved to the CPU
/// verbatim. It is evaluated once per star at startup instead of once per star
/// per pixel per frame. The multiply amplifies any ULP difference in `sin`
/// enormously, so the field this produces is not bit-identical to the one the
/// GPU's `sin` produced — it is the same kind of field from the same
/// generator, which is all the model ever asked for.
fn rnd(n: f32) -> f32 {
    // The classic `43758.5453` multiplier, written as the `f32` it actually
    // becomes: the decimal literal carries more digits than the type does, and
    // WGSL rounds the same literal to this same value.
    const SCALE: f32 = 43_758.547;
    let v = (n * 12.9898).sin() * SCALE;
    v - v.floor()
}

/// Faint stellar colour variation, in sRGB.
fn star_color(i: f32) -> [f32; 3] {
    match rnd(i * 3.0 + 1.3) {
        r if r < 0.72 => [0.847, 0.886, 0.965], // cool blue-white
        r if r < 0.90 => [0.918, 0.918, 0.949], // near white
        _ => [0.965, 0.902, 0.808],             // faint warm
    }
}

/// sRGB (`0..1`) to linear, matching `quad.rs`. Star light accumulates in
/// LINEAR space (physically-correct additive glow) and the sRGB surface
/// re-encodes it.
fn srgb_to_linear(c: f32) -> f32 {
    if c > 0.04045 {
        ((c + 0.055) / 1.055).powf(2.4)
    } else {
        c / 12.92
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge0 == edge1 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Per-star brightness as a function of radial progress: `0` is just emerged
/// at the centre, `1` is passing the edge.
///
/// Invisible at both ends, so the population turnover at the loop boundary
/// cannot pop, and brightening sharply with proximity (cubic) so the middle
/// stays dark and stars bloom into view as they approach.
fn star_brightness(prog: f32) -> f32 {
    let fade_in = smoothstep(0.0, FADE_IN_END, prog);
    let fade_out = 1.0 - smoothstep(FADE_OUT_START, 1.0, prog);
    let proximity = prog * prog * prog;
    fade_in * fade_out * proximity
}

/// Resolve every star for one frame, writing the visible ones into `out` and
/// returning how many there are.
///
/// This is the whole of the model that used to run per pixel. It runs once per
/// frame, and the field repaints at a low fps cap, so its cost is not on any
/// hot path.
fn build_frame_stars(
    seeds: &[StarSeed; NSTARS],
    resolution: [f32; 2],
    time_secs: f32,
    out: &mut [Star; NSTARS],
) -> u32 {
    let center = [resolution[0].max(1.0) * 0.5, resolution[1].max(1.0) * 0.5];
    // Reach into the corners.
    let rmax = (center[0] * center[0] + center[1] * center[1]).sqrt() * 1.04;
    let mut count = 0usize;
    for seed in seeds {
        let drift = seed.phase + time_secs * SPEED;
        let prog = drift - drift.floor();
        let brightness = star_brightness(prog);
        if brightness <= MIN_VISIBLE_BRIGHTNESS {
            continue;
        }
        let radius = rmax * prog.powf(EASE);
        // A CRISP bright core (sharp inverse-square, squared for a tight
        // point) plus a SUBTLE bloom — not a big soft orb.
        let core_r = mix(0.7, 1.3, prog);
        let halo_r = mix(3.0, 9.0, prog);
        out[count] = Star {
            geom: [
                seed.direction[0] * radius,
                seed.direction[1] * radius,
                core_r * core_r,
                halo_r * halo_r,
            ],
            tint: [
                seed.linear_color[0] * brightness,
                seed.linear_color[1] * brightness,
                seed.linear_color[2] * brightness,
                0.0,
            ],
        };
        count += 1;
    }
    // Zero the tail so a shrinking frame cannot leave a previous frame's star
    // readable past `count`.
    out[count..].fill(Star::default());
    count as u32
}

const SHADER: &str = r#"
struct Star {
  geom: vec4<f32>,
  tint: vec4<f32>,
};

struct U {
  resolution: vec2<f32>,
  count: u32,
  pad: u32,
  stars: array<Star, %NSTARS%>,
};
@group(0) @binding(0) var<uniform> u: U;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
  // One oversized triangle covering the whole clip space.
  var p = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
  return vec4<f32>(p[vi], 0.0, 1.0);
}

// Everything that does not depend on the pixel is resolved on the CPU and
// arrives in `u.stars` (see `build_frame_stars`). What is left here is the
// only part that genuinely varies per pixel: the distance to each star, and
// the two falloff terms built from it.
@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
  let res = max(u.resolution, vec2<f32>(1.0, 1.0));
  let p = frag.xy - res * 0.5;         // pixels from screen center
  var col = vec3<f32>(0.0);            // pure black sky

  for (var i: u32 = 0u; i < u.count; i = i + 1u) {
    let star = u.stars[i];
    let delta = p - star.geom.xy;
    let d2 = dot(delta, delta);
    let core_r2 = star.geom.z;
    let cc = core_r2 / (d2 + core_r2);
    let halo = exp(-d2 / star.geom.w);
    // `tint` already carries the star's brightness, so one multiply covers
    // both falloff terms.
    col = col + star.tint.rgb * (cc * cc * 1.15 + halo * 0.22);
  }

  return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

pub struct StarfieldPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
    /// The fixed properties of every star, evaluated once.
    seeds: [StarSeed; NSTARS],
}

impl StarfieldPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        // `%NSTARS%` is substituted into the uniform array's bound (a
        // compile-time literal, for the broadest naga-backend support — no
        // WGSL override-constant).
        let src = SHADER.replace("%NSTARS%", &NSTARS.to_string());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-starfield"),
            source: wgpu::ShaderSource::Wgsl(src.into()),
        });
        let uniform_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("starfield-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("starfield-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("starfield-bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("starfield-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("starfield-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // Opaque base layer — no blend needed.
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            bind_group,
            uniform_buf,
            seeds: StarSeed::all(),
        }
    }

    /// Resolve and upload one frame's stars. `time_secs` is the continuous
    /// drift clock; the look (speed / star count / glow) is baked in
    /// (v2.24.1).
    ///
    /// This is where the model is evaluated — once per frame for 55 stars,
    /// rather than once per star per pixel.
    pub fn upload(&self, queue: &wgpu::Queue, resolution: [f32; 2], time_secs: f32) {
        let mut u = Uniforms {
            resolution,
            count: 0,
            _pad: 0,
            stars: [Star::default(); NSTARS],
        };
        u.count = build_frame_stars(&self.seeds, resolution, time_secs, &mut u.stars);
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
    }

    /// The star positions this pipeline would upload for one frame, in pixels
    /// from the surface centre.
    ///
    /// Exists so the GPU test in `lib.rs` can check that the stars land where
    /// the CPU put them, which is the only thing that catches a uniform-layout
    /// disagreement between this file's Rust and its hand-written WGSL.
    #[cfg(test)]
    pub(crate) fn frame_positions(&self, resolution: [f32; 2], time_secs: f32) -> Vec<[f32; 2]> {
        let mut stars = [Star::default(); NSTARS];
        let count = build_frame_stars(&self.seeds, resolution, time_secs, &mut stars) as usize;
        stars[..count]
            .iter()
            .map(|star| [star.geom[0], star.geom[1]])
            .collect()
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These once tested a hand-copied Rust transcription of the brightness
    // curve, so the shader they were protecting could drift away underneath
    // them without a single one going red. The curve is production code now
    // (`star_brightness`) and the shader reads its result, so these drive the
    // thing that actually runs.

    #[test]
    fn brightness_is_zero_at_both_ends_for_a_seamless_loop() {
        // Just emerged (center) and fully passed (edge wrap) → invisible, so the
        // population turnover at the loop boundary can't pop.
        assert!(star_brightness(0.0).abs() < 1e-6, "must be dark at p=0");
        assert!(star_brightness(1.0).abs() < 1e-6, "must be dark at p=1");
    }

    #[test]
    fn brightness_grows_with_proximity_through_the_midfield() {
        // Across the lit plateau (fade-in done, fade-out not begun) a nearer
        // star (larger prog) is brighter — the "fade in as we get closer" look.
        let a = star_brightness(0.40);
        let b = star_brightness(0.70);
        let c = star_brightness(0.88);
        assert!(a > 0.0 && b > a && c > b, "expected a<b<c, got {a} {b} {c}");
    }

    #[test]
    fn brightness_ramps_in_from_the_emergence_point() {
        // Monotonic rise over the fade-in stretch (no flicker as a star appears).
        let p1 = star_brightness(0.05);
        let p2 = star_brightness(0.15);
        let p3 = star_brightness(0.28);
        assert!(
            p1 < p2 && p2 < p3,
            "fade-in must be monotonic: {p1} {p2} {p3}"
        );
    }

    #[test]
    fn brightness_stays_in_unit_range() {
        let mut p = 0.0;
        while p <= 1.0 {
            let b = star_brightness(p);
            assert!(
                (0.0..=1.0).contains(&b),
                "brightness {b} out of range at p={p}"
            );
            p += 0.01;
        }
    }

    #[test]
    fn center_stars_are_invisible_and_bloom_with_proximity() {
        // v2.24.1: a freshly-emerged star at the center is COMPLETELY invisible
        // (no floor), the inner-middle stays near-dark, and brightness blooms
        // sharply (cubic) as the star nears — a strong "warp toward you" feel.
        assert_eq!(star_brightness(0.0), 0.0, "center star must be invisible");
        assert!(
            star_brightness(0.15) < 0.01,
            "inner-middle stars stay near-invisible"
        );
        let mid = star_brightness(0.5);
        let near = star_brightness(0.9);
        assert!(near > mid * 4.0, "near ({near}) should dwarf mid ({mid})");
    }

    #[test]
    fn nstars_is_sane() {
        assert!(
            (1..=4096).contains(&NSTARS),
            "loop bound must be a sane count"
        );
    }

    /// The uniform block's Rust layout has to agree with the hand-written
    /// WGSL, and nothing else checks that: a mismatch is not a compile error,
    /// it is a silently misread star.
    #[test]
    fn the_uniform_block_matches_the_wgsl_layout() {
        // `vec2<f32>` + `u32` + `u32` = 16 bytes, which is also the alignment
        // `array<Star, N>` needs to start on in uniform address space.
        assert_eq!(std::mem::size_of::<Star>(), 32);
        assert_eq!(std::mem::size_of::<Star>() % 16, 0, "uniform array stride");
        assert_eq!(
            std::mem::size_of::<Uniforms>(),
            16 + NSTARS * std::mem::size_of::<Star>()
        );
        // The WGSL declares the array with the substituted bound, so a change
        // to `NSTARS` that missed the substitution would overrun.
        let src = SHADER.replace("%NSTARS%", &NSTARS.to_string());
        assert!(
            src.contains(&format!("array<Star, {NSTARS}>")),
            "the shader's array bound must be the substituted NSTARS"
        );
        assert!(
            !src.contains('%'),
            "every placeholder must be substituted before compilation"
        );
    }

    /// The point of the rewrite: the fragment loop must not carry the model
    /// any more.
    ///
    /// The hash, the angle, the radial ease and the sRGB decode were all
    /// evaluated per pixel. This asserts on the fragment entry point only —
    /// asserting on the whole source would pass merely because the vertex
    /// shader is cheap.
    #[test]
    fn the_fragment_loop_carries_no_per_pixel_transcendentals_but_the_falloff() {
        let src = SHADER.replace("%NSTARS%", &NSTARS.to_string());
        let fragment = src
            .split_once("fn fs(")
            .expect("the shader must have a fragment entry point")
            .1;
        assert!(
            !fragment.is_empty(),
            "fixture must actually slice out the fragment body"
        );
        for banned in ["sin(", "cos(", "pow(", "fract(", "smoothstep("] {
            assert!(
                !fragment.contains(banned),
                "`{banned}` is per-star-per-pixel work that belongs in \
                 `build_frame_stars`; found it in the fragment shader"
            );
        }
        // `exp` is the one that genuinely depends on the pixel, so it stays —
        // and if it ever disappears the halo has been dropped, not optimized.
        assert!(
            fragment.contains("exp(-d2"),
            "the halo falloff is the one per-pixel transcendental and must remain"
        );
    }

    /// Stars too dim to see are dropped before upload, and the tail is cleared
    /// so a shrinking frame cannot leave the previous frame's star readable.
    #[test]
    fn invisible_stars_are_dropped_and_the_tail_is_cleared() {
        let seeds = StarSeed::all();
        let mut stars = [Star::default(); NSTARS];

        // Find a time where at least one star is culled, so the assertions
        // below are not vacuous.
        let mut sampled = Vec::new();
        for step in 0..64 {
            let count = build_frame_stars(&seeds, [1920.0, 1080.0], step as f32 * 4.0, &mut stars);
            sampled.push(count);
        }
        let min = *sampled.iter().min().expect("sampled frames");
        let max = *sampled.iter().max().expect("sampled frames");
        assert!(
            min < NSTARS as u32,
            "no sampled frame culled anything, so the cull is untested \
             (counts ranged {min}..={max})"
        );
        assert!(max > 0, "no sampled frame drew anything");

        // Fill the buffer, then rebuild at a time with fewer visible stars and
        // confirm nothing survives past `count`.
        let busiest = sampled
            .iter()
            .position(|&c| c == max)
            .expect("a busiest frame");
        build_frame_stars(&seeds, [1920.0, 1080.0], busiest as f32 * 4.0, &mut stars);
        let quietest = sampled
            .iter()
            .position(|&c| c == min)
            .expect("a quietest frame");
        let count = build_frame_stars(&seeds, [1920.0, 1080.0], quietest as f32 * 4.0, &mut stars);
        assert_eq!(count, min);
        for (index, star) in stars.iter().enumerate().skip(count as usize) {
            assert_eq!(
                star.tint, [0.0; 4],
                "star {index} past the live count still carries a previous \
                 frame's tint"
            );
        }
    }

    /// Every uploaded star must be positioned and coloured the way the model
    /// says, driven through the production path rather than restated.
    #[test]
    fn uploaded_stars_carry_the_resolved_model() {
        let seeds = StarSeed::all();
        let mut stars = [Star::default(); NSTARS];
        let resolution = [1600.0, 900.0];
        let time = 12.5;
        let count = build_frame_stars(&seeds, resolution, time, &mut stars) as usize;
        assert!(count > 0, "expected some visible stars at this time");

        let rmax = ((resolution[0] * 0.5).powi(2) + (resolution[1] * 0.5).powi(2)).sqrt() * 1.04;
        let mut checked = 0;
        for seed in &seeds {
            let drift = seed.phase + time * SPEED;
            let prog = drift - drift.floor();
            let brightness = star_brightness(prog);
            if brightness <= MIN_VISIBLE_BRIGHTNESS {
                continue;
            }
            let star = stars[checked];
            let radius = rmax * prog.powf(EASE);
            let distance = (star.geom[0] * star.geom[0] + star.geom[1] * star.geom[1]).sqrt();
            assert!(
                (distance - radius).abs() <= radius * 1e-4 + 1e-3,
                "star {checked} sits at {distance} px, model says {radius}"
            );
            // Radii arrive squared, and brightness is folded into the tint.
            assert!((star.geom[2] - mix(0.7, 1.3, prog).powi(2)).abs() < 1e-5);
            assert!((star.geom[3] - mix(3.0, 9.0, prog).powi(2)).abs() < 1e-4);
            assert!((star.tint[0] - seed.linear_color[0] * brightness).abs() < 1e-6);
            checked += 1;
        }
        assert_eq!(
            checked, count,
            "every visible star must be uploaded, in order"
        );
    }

    /// The colour written into `tint` is LINEAR, not the sRGB the palette is
    /// written in — the surface re-encodes, and skipping the decode would wash
    /// every star out.
    #[test]
    fn star_colours_are_decoded_to_linear_once() {
        let seeds = StarSeed::all();
        // The palette's channels are all well above the 0.04045 knee, so the
        // decode has to move every one of them noticeably.
        for seed in &seeds {
            for channel in seed.linear_color {
                assert!(
                    (0.0..=1.0).contains(&channel),
                    "linear channel {channel} out of range"
                );
            }
        }
        let brightest = star_color(0.0)[0];
        assert!(
            brightest > 0.04045,
            "fixture colour must clear the sRGB knee"
        );
        assert!(
            srgb_to_linear(brightest) < brightest - 0.05,
            "decoding to linear must darken an sRGB value well above the knee"
        );
    }
}

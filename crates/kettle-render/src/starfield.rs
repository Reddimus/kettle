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
//! `scripts/gen-starfield.py`; the test module keeps a pure-Rust copy of the
//! brightness curve (`star_brightness`) so the visual contract is unit-tested
//! (WGSL can't be).

use bytemuck::{Pod, Zeroable};

/// Per-pixel star-loop bound in the shader; must equal
/// [`kettle_config::STARFIELD_MAX_STARS`] (the density clamp). The loop is
/// fixed-length for naga-backend portability and `break`s at the live density.
const MAX_STARS: u32 = kettle_config::STARFIELD_MAX_STARS;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    /// Surface size in physical pixels.
    resolution: [f32; 2],
    /// Continuous seconds since playback started (drives the drift).
    time: f32,
    /// Radial-progress cycles per second (`starfield-speed`).
    speed: f32,
    /// Star count as a float (cast to a loop bound in the shader).
    density: f32,
    /// Soft-halo intensity multiplier (`starfield-glow`).
    glow: f32,
    _pad: [f32; 2],
}

const SHADER: &str = r#"
struct U {
  resolution: vec2<f32>,
  time: f32,
  speed: f32,
  density: f32,
  glow: f32,
  pad0: f32,
  pad1: f32,
};
@group(0) @binding(0) var<uniform> u: U;

const TAU: f32 = 6.2831853;
const EASE: f32 = 1.7;          // >1: slow near center, faster near the edge
const FADE_IN_END: f32 = 0.30;
const FADE_OUT_START: f32 = 0.92;
const DIM_FLOOR: f32 = 0.15;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
  // One oversized triangle covering the whole clip space.
  var p = array<vec2<f32>, 3>(
    vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
  return vec4<f32>(p[vi], 0.0, 1.0);
}

// Cheap deterministic hash (index -> 0..1). The args stay small (< 128*4) so
// the sin precision is ample across backends.
fn rnd(n: f32) -> f32 {
  return fract(sin(n * 12.9898) * 43758.5453);
}

// sRGB (0..1) -> linear, matching quad.rs. The surface is sRGB so the hardware
// re-encodes; we accumulate star light in LINEAR space (physically-correct
// additive glow) and output it directly.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
  let lo = c / 12.92;
  let hi = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
  return select(lo, hi, c > vec3<f32>(0.04045));
}

fn star_color(i: f32) -> vec3<f32> {
  let r = rnd(i * 3.0 + 1.3);
  if (r < 0.72) { return vec3<f32>(0.847, 0.886, 0.965); }   // cool blue-white
  if (r < 0.90) { return vec3<f32>(0.918, 0.918, 0.949); }   // near white
  return vec3<f32>(0.965, 0.902, 0.808);                     // faint warm
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
  let res = max(u.resolution, vec2<f32>(1.0, 1.0));
  let center = res * 0.5;
  let p = frag.xy - center;            // pixels from screen center
  let rmax = length(center) * 1.04;    // reach into the corners
  let n = i32(u.density);
  var col = vec3<f32>(0.0);            // pure black sky

  for (var i: i32 = 0; i < %MAX_STARS%; i = i + 1) {
    if (i >= n) { break; }
    let fi = f32(i);
    let th = rnd(fi + 1.0) * TAU;
    let p0 = rnd(fi * 2.0 + 0.5);
    let prog = fract(p0 + u.time * u.speed);

    // Brightness: fade in over the first stretch, keep brightening toward us,
    // brief exit fade so the loop wrap is invisible.
    let fadein = smoothstep(0.0, FADE_IN_END, prog);
    let fadeout = 1.0 - smoothstep(FADE_OUT_START, 1.0, prog);
    let prox = mix(DIM_FLOOR, 1.0, prog);
    let b = fadein * fadeout * prox;
    if (b <= 0.002) { continue; }

    let r = rmax * pow(prog, EASE);
    let sp = vec2<f32>(cos(th), sin(th)) * r;
    let d = distance(p, sp);

    // Real-star look: a CRISP bright core (sharp inverse-square, squared for a
    // tight point) plus a SUBTLE bloom — not a big soft orb.
    let core_r = mix(0.7, 1.3, prog);
    let halo_r = mix(3.0, 9.0, prog) * max(u.glow, 0.0001);
    let cc = (core_r * core_r) / (d * d + core_r * core_r);
    let core = cc * cc;
    let halo = exp(-(d * d) / (halo_r * halo_r));
    let intensity = b * (core * 1.15 + halo * 0.22 * u.glow);

    col = col + srgb_to_linear(star_color(fi)) * intensity;
  }

  return vec4<f32>(clamp(col, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
"#;

pub struct StarfieldPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buf: wgpu::Buffer,
}

impl StarfieldPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        // `%MAX_STARS%` is substituted so the WGSL loop bound matches the shared
        // config cap without a WGSL override-constant (broadest backend support).
        let src = SHADER.replace("%MAX_STARS%", &MAX_STARS.to_string());
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
        }
    }

    /// Refresh the per-frame uniforms. `time_secs` is continuous (the drift
    /// clock); `speed`/`density`/`glow` come from config.
    pub fn upload(
        &self,
        queue: &wgpu::Queue,
        resolution: [f32; 2],
        time_secs: f32,
        speed: f32,
        density: u32,
        glow: f32,
    ) {
        let u = Uniforms {
            resolution,
            time: time_secs,
            speed,
            density: density.min(MAX_STARS) as f32,
            glow,
            _pad: [0.0; 2],
        };
        queue.write_buffer(&self.uniform_buf, 0, bytemuck::bytes_of(&u));
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

    // Copies of the brightness literals embedded in the WGSL `SHADER` string
    // above; keep the two in sync. The tests pin the shape the shader must
    // reproduce (a real WGSL run needs a GPU).
    const FADE_IN_END: f32 = 0.30;
    const FADE_OUT_START: f32 = 0.92;
    const DIM_FLOOR: f32 = 0.15;

    fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
        if edge0 == edge1 {
            return if x < edge0 { 0.0 } else { 1.0 };
        }
        let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Pure-Rust mirror of the shader's per-star brightness curve, as a function
    /// of radial progress `prog` (0 = just emerged at center, 1 = passing the
    /// edge): invisible at both ends (seamless loop) and brightening with
    /// proximity ("fade in as we get closer").
    fn star_brightness(prog: f32) -> f32 {
        let fadein = smoothstep(0.0, FADE_IN_END, prog);
        let fadeout = 1.0 - smoothstep(FADE_OUT_START, 1.0, prog);
        let prox = DIM_FLOOR + (1.0 - DIM_FLOOR) * prog;
        fadein * fadeout * prox
    }

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
    fn max_stars_matches_config_cap() {
        assert_eq!(MAX_STARS, kettle_config::STARFIELD_MAX_STARS);
    }
}

//! Minimal instanced solid-rect pipeline used for cell backgrounds, the
//! cursor, selection and search highlights.

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct QuadInstance {
    /// Top-left in physical pixels.
    pub pos: [f32; 2],
    /// Size in physical pixels.
    pub size: [f32; 2],
    /// Straight-alpha RGBA.
    pub color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Screen {
    size: [f32; 2],
    _pad: [f32; 2],
}

const SHADER: &str = r#"
struct Screen { size: vec2<f32>, pad: vec2<f32> };
@group(0) @binding(0) var<uniform> screen: Screen;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    let px = pos + c * size;
    let ndc = vec2<f32>(
        px.x / screen.size.x * 2.0 - 1.0,
        1.0 - px.y / screen.size.y * 2.0,
    );
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.color = color;
    return out;
}

// sRGB → linear, matching the CPU-side `srgb()` (lib.rs) used for the
// render-pass *clear* color. The render target is an sRGB surface
// (Bgra8UnormSrgb live / Rgba8UnormSrgb offscreen), so the hardware
// sRGB-ENCODES whatever the fragment shader writes. Quad colors arrive
// as plain sRGB components (0..1); without this decode they'd be encoded
// a second time and every solid rect (cell backgrounds, cursor, dims,
// chrome) would render gamma-lifted — e.g. a dark editor bg #1a1b23
// surfaced as a washed-out grey #5a5f68. Decoding here cancels the
// surface's encode so a quad lands on its intended color, consistent
// with the (already-linearized) clear color and the glyph pass.
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(lo, hi, c > vec3<f32>(0.04045));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let lin = srgb_to_linear(in.color.rgb);
    return vec4<f32>(lin * in.color.a, in.color.a);
}
"#;

/// Compute the instance-buffer capacity (rounded up to a power of two) and
/// its byte size needed to hold `len` quad instances. Returns `None` if
/// either the capacity or the resulting byte size would overflow `usize`,
/// so callers can degrade (skip the upload) rather than panic —
/// `usize::next_power_of_two()` panics on overflow, and a plain
/// `capacity * size_of::<QuadInstance>()` multiplication can overflow even
/// when the capacity itself is representable.
fn grow_capacity(len: usize) -> Option<(usize, usize)> {
    let capacity = len.checked_next_power_of_two()?;
    let bytes = capacity.checked_mul(std::mem::size_of::<QuadInstance>())?;
    Some((capacity, bytes))
}

pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    screen_buf: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: usize,
    pub count: u32,
}

impl QuadPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-quad"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-quad-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kettle-quad-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kettle-quad-bg"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kettle-quad-layout"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kettle-quad-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let capacity = 4096;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-quad-instances"),
            size: (capacity * std::mem::size_of::<QuadInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            bind_group,
            screen_buf,
            instances,
            capacity,
            count: 0,
        }
    }

    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        data: &[QuadInstance],
    ) {
        queue.write_buffer(
            &self.screen_buf,
            0,
            bytemuck::bytes_of(&Screen {
                size: screen,
                _pad: [0.0; 2],
            }),
        );
        if data.len() > self.capacity {
            // Checked growth: `next_power_of_two()` panics (and this
            // workspace runs with `panic = "abort"`, so that would be a hard
            // process abort) if the grown capacity overflows `usize`, and a
            // naive `* size_of::<QuadInstance>()` could overflow separately
            // even when the capacity itself fits. Mirror the checked-growth
            // contract used by `ImagePipeline::upload` (imgpipe.rs) and
            // `GlyphPipeline::upload` (glyphpipe.rs): degrade by skipping
            // this frame's quad upload instead of panicking.
            let Some((capacity, bytes)) = grow_capacity(data.len()) else {
                log::warn!(
                    "quad instance buffer growth for {} instances overflows usize; skipping quad upload",
                    data.len()
                );
                self.count = 0;
                return;
            };
            self.capacity = capacity;
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kettle-quad-instances"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !data.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(data));
        }
        self.count = data.len() as u32;
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, 0..self.count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_capacity_rounds_up_to_next_power_of_two() {
        assert_eq!(
            grow_capacity(5),
            Some((8, 8 * std::mem::size_of::<QuadInstance>()))
        );
        assert_eq!(
            grow_capacity(4096 + 1),
            Some((8192, 8192 * std::mem::size_of::<QuadInstance>()))
        );
        // Already a power of two: capacity is unchanged.
        assert_eq!(
            grow_capacity(64),
            Some((64, 64 * std::mem::size_of::<QuadInstance>()))
        );
    }

    #[test]
    fn grow_capacity_degrades_instead_of_panicking_on_next_power_of_two_overflow() {
        // No power of two large enough to hold `usize::MAX` instances is
        // representable in a `usize`, so the checked call must return
        // `None` rather than panicking the way `next_power_of_two()` would.
        assert_eq!(grow_capacity(usize::MAX), None);
    }

    #[test]
    fn grow_capacity_degrades_instead_of_overflowing_on_byte_size() {
        // `huge` is itself a representable power of two, but multiplying it
        // by `size_of::<QuadInstance>()` (32 bytes) overflows `usize` — this
        // exercises the second checked step, independent of the first.
        let huge = 1usize << (usize::BITS - 1);
        assert_eq!(grow_capacity(huge), None);
    }
}

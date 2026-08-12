//! Instanced pane-outline pipeline.
//!
//! Most pane borders remain four ordinary quads. Decorated macOS windows are
//! the exception: AppKit clips the two bottom corners to its rounded window
//! mask, so four square strips visibly terminate at the mask instead of
//! following it. One signed-distance outline gives those outer corners a
//! continuous antialiased arc without changing internal split corners or the
//! rendering on other platforms.

use bytemuck::{Pod, Zeroable};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct OutlineInstance {
    /// Top-left in physical pixels.
    pub pos: [f32; 2],
    /// Size in physical pixels.
    pub size: [f32; 2],
    /// Straight-alpha RGBA.
    pub color: [f32; 4],
    /// Inset width in physical pixels.
    pub border_width: f32,
    /// Radius for corners selected by `corner_mask`, in physical pixels.
    pub corner_radius: f32,
    /// Bits in top-left, top-right, bottom-right, bottom-left order.
    pub corner_mask: u32,
    pub _pad: u32,
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
    @location(1) local: vec2<f32>,
    @location(2) size: vec2<f32>,
    @location(3) border_width: f32,
    @location(4) corner_radius: f32,
    @location(5) @interpolate(flat) corner_mask: u32,
};

@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
    @location(3) border_width: f32,
    @location(4) corner_radius: f32,
    @location(5) corner_mask: u32,
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
    out.local = c * size;
    out.size = size;
    out.border_width = border_width;
    out.corner_radius = corner_radius;
    out.corner_mask = corner_mask;
    return out;
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(lo, hi, c > vec3<f32>(0.04045));
}

fn selected_corner_radius(local: vec2<f32>, size: vec2<f32>, radius: f32, mask: u32) -> f32 {
    let left = local.x < size.x * 0.5;
    let top = local.y < size.y * 0.5;
    var bit = 0u;
    if left && top {
        bit = 1u;
    } else if !left && top {
        bit = 2u;
    } else if !left && !top {
        bit = 4u;
    } else {
        bit = 8u;
    }
    return select(0.0, radius, (mask & bit) != 0u);
}

fn rounded_rect_distance(local: vec2<f32>, size: vec2<f32>, radius: f32) -> f32 {
    let half_size = size * 0.5;
    let q = abs(local - half_size) - (half_size - vec2<f32>(radius));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

fn mitered_rect_distance(local: vec2<f32>, size: vec2<f32>) -> f32 {
    let half_size = size * 0.5;
    let q = abs(local - half_size) - half_size;
    // A Euclidean box SDF rounds the OUTSIDE of a nominally zero-radius
    // corner. That is desirable for a selected native-window corner, but it
    // visibly softens the square top/split joins in the other quadrants when
    // the divider is wider than one pixel. L-infinity distance keeps those
    // joins mitered while agreeing with the rounded SDF along straight edges.
    return max(q.x, q.y);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    if in.border_width <= 0.0 {
        discard;
    }
    let radius = selected_corner_radius(
        in.local,
        in.size,
        min(in.corner_radius, min(in.size.x, in.size.y) * 0.5),
        in.corner_mask,
    );
    // The native window mask cuts through the outermost surface pixels. Keep
    // the outline's centreline one half-stroke inside the pane bounds: its
    // outer antialiasing then remains visible instead of landing underneath
    // AppKit's clip. Square edges use the same inset, so switching a pane to
    // this pipeline cannot make its straight sections look thicker.
    let inset = in.border_width * 0.5;
    let outline_local = in.local - vec2<f32>(inset);
    let outline_size = max(in.size - vec2<f32>(inset * 2.0), vec2<f32>(0.0));
    let outline_radius = max(radius - inset, 0.0);
    var distance = mitered_rect_distance(outline_local, outline_size);
    if outline_radius > 0.0 {
        distance = rounded_rect_distance(outline_local, outline_size, outline_radius);
    }
    // Antialias the two sides of the stroke as one absolute-distance ramp.
    // `fwidth` describes the full pixel footprint, so use half of it on each
    // side of the nominal edge. Multiplying separate inner/outer ramps makes a
    // one-pixel stroke only ~71% opaque at its centre; this form keeps the
    // configured centreline fully opaque while retaining one-pixel AA.
    let half_aa = max(fwidth(distance) * 0.5, 0.25);
    let half_width = in.border_width * 0.5;
    let coverage = 1.0 - smoothstep(
        max(half_width - half_aa, 0.0),
        half_width + half_aa,
        abs(distance),
    );
    if coverage <= 0.0 {
        discard;
    }
    let alpha = in.color.a * coverage;
    let linear = srgb_to_linear(in.color.rgb);
    return vec4<f32>(linear * alpha, alpha);
}
"#;

#[cfg(test)]
mod shader_tests {
    use super::SHADER;

    /// A zero-radius Euclidean box SDF still rounds its outer join. The pane
    /// outline selects radii per quadrant, so unselected top/internal corners
    /// must explicitly use the mitered distance path instead of merely passing
    /// radius zero to `rounded_rect_distance`.
    #[test]
    fn unselected_outline_corners_use_mitered_distance() {
        assert!(SHADER.contains("fn mitered_rect_distance("));
        assert!(SHADER.contains("var distance = mitered_rect_distance("));
        assert!(SHADER.contains("if outline_radius > 0.0"));
    }
}

pub struct OutlinePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    screen_buf: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
}

impl OutlinePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-pane-outline"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-pane-outline-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kettle-pane-outline-bgl"),
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
            label: Some("kettle-pane-outline-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kettle-pane-outline-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kettle-pane-outline-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<OutlineInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x4,
                        3 => Float32,
                        4 => Float32,
                        5 => Uint32
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
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
        let capacity = 16;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-pane-outline-instances"),
            size: (capacity * std::mem::size_of::<OutlineInstance>()) as u64,
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
        data: &[OutlineInstance],
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
            let Some(capacity) = data.len().checked_next_power_of_two() else {
                log::warn!(
                    "pane outline instance buffer growth for {} instances overflows usize; skipping upload",
                    data.len()
                );
                self.count = 0;
                return;
            };
            let Some(bytes) = capacity.checked_mul(std::mem::size_of::<OutlineInstance>()) else {
                log::warn!(
                    "pane outline instance buffer growth for {} instances overflows usize; skipping upload",
                    data.len()
                );
                self.count = 0;
                return;
            };
            self.capacity = capacity;
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kettle-pane-outline-instances"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !data.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(data));
        }
        self.count = u32::try_from(data.len()).unwrap_or_else(|_| {
            log::warn!(
                "pane outline instance count {} exceeds u32; skipping upload",
                data.len()
            );
            0
        });
    }

    pub fn draw<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>) {
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
    use super::SHADER;

    #[test]
    fn zero_width_is_discarded_and_antialiasing_uses_one_distance_ramp() {
        assert!(SHADER.contains("if in.border_width <= 0.0 {\n        discard;"));
        assert!(SHADER.contains("let half_aa = max(fwidth(distance) * 0.5, 0.25);"));
        assert!(SHADER.contains("abs(distance)"));
        assert!(
            !SHADER.contains("let outer =") && !SHADER.contains("let inner ="),
            "multiplying two full-width ramps attenuates a one-pixel stroke at its centre"
        );
    }
}

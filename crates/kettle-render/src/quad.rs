//! Minimal instanced solid-rect pipeline used for cell backgrounds, the
//! cursor, selection and search highlights.

use bytemuck::{Pod, Zeroable};

use crate::upload::{
    MappedRing, RetainedBytes, UploadCounters, UploadCounts, mapped_uploads,
    write_buffer_if_changed,
};

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

/// Premultiplied-alpha blending: the shader returns premultiplied colour, so
/// the blend must not apply alpha a second time (see `build_pipeline`).
const BLEND: Option<wgpu::BlendState> = Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);

/// What every [`QuadPipeline`] a renderer builds for one surface format can
/// share: the uniform's bind group layout and one render pipeline per blend
/// mode, both compiled from one shader module. A renderer's five quad layers
/// need only these two pipelines; each layer keeps its own uniform, bind group
/// and instance buffer. Cloning a wgpu handle adds a reference, not a GPU
/// object.
pub(crate) struct QuadShared {
    bind_group_layout: wgpu::BindGroupLayout,
    blend: wgpu::RenderPipeline,
    replace: wgpu::RenderPipeline,
}

impl QuadShared {
    pub(crate) fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let bind_group_layout = screen_bind_group_layout(device);
        let (shader, layout) = shader_and_layout(device, &bind_group_layout);
        Self {
            blend: build_pipeline(device, &shader, &layout, format, BLEND),
            replace: build_pipeline(device, &shader, &layout, format, None),
            bind_group_layout,
        }
    }

    /// A layer that blends over what lies beneath it, as [`QuadPipeline::new`]
    /// builds one.
    pub(crate) fn blend(&self, device: &wgpu::Device) -> QuadPipeline {
        QuadPipeline::with_pipeline(device, &self.bind_group_layout, self.blend.clone())
    }

    /// A layer that replaces the pixels it covers, alpha included.
    pub(crate) fn replace(&self, device: &wgpu::Device) -> QuadPipeline {
        QuadPipeline::with_pipeline(device, &self.bind_group_layout, self.replace.clone())
    }
}

fn screen_bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
    })
}

fn shader_and_layout(
    device: &wgpu::Device,
    bind_group_layout: &wgpu::BindGroupLayout,
) -> (wgpu::ShaderModule, wgpu::PipelineLayout) {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("kettle-quad"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("kettle-quad-layout"),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    (shader, layout)
}

fn build_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    layout: &wgpu::PipelineLayout,
    format: wgpu::TextureFormat,
    blend: Option<wgpu::BlendState>,
) -> wgpu::RenderPipeline {
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("kettle-quad-pipeline"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some("vs"),
            compilation_options: Default::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<QuadInstance>() as u64,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
            })],
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some("fs"),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // This pipeline's fragment shader returns PREMULTIPLIED
                // color (`rgb * a`), so the blend must not apply alpha a
                // second time. `ALPHA_BLENDING` uses `SrcAlpha` for the
                // source factor and would compute `rgb * a * a`, so a
                // 50%-opaque surface would contribute 25% and every
                // translucent image, panel, highlight, and separator would
                // render too dark. (`glyphpipe` returns STRAIGHT alpha and
                // keeps `ALPHA_BLENDING`.)
                blend,
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
    })
}

pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    screen_buf: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: usize,
    pub count: u32,
    screen_held: RetainedBytes,
    /// What the buffer the draws read holds: `instances`, or the ring's
    /// current buffer when there is a ring.
    instances_held: RetainedBytes,
    /// On shared memory, the instances go through mapped buffers instead of
    /// `instances` (see `upload.rs`).
    ring: Option<MappedRing>,
    counters: UploadCounters,
}

impl QuadPipeline {
    /// A standalone blending layer that compiles its own pipeline, for paths
    /// that draw one frame. A renderer builds its layers through
    /// [`QuadShared`] instead.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::new_with_blend(device, format, BLEND)
    }

    /// A standalone replacing layer; see [`QuadPipeline::new`].
    #[cfg(test)]
    pub fn new_replace(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::new_with_blend(device, format, None)
    }

    fn new_with_blend(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        blend: Option<wgpu::BlendState>,
    ) -> Self {
        let bind_group_layout = screen_bind_group_layout(device);
        let (shader, layout) = shader_and_layout(device, &bind_group_layout);
        let pipeline = build_pipeline(device, &shader, &layout, format, blend);
        Self::with_pipeline(device, &bind_group_layout, pipeline)
    }

    /// A layer that draws with `pipeline`, whose layout holds
    /// `bind_group_layout`, with its own uniform, bind group and instances.
    fn with_pipeline(
        device: &wgpu::Device,
        bind_group_layout: &wgpu::BindGroupLayout,
        pipeline: wgpu::RenderPipeline,
    ) -> Self {
        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-quad-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kettle-quad-bg"),
            layout: bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });
        let ring = mapped_uploads(device)
            .then(|| MappedRing::new("kettle-quad-instances", wgpu::BufferUsages::VERTEX, None));
        // With a ring, the queue path's buffer is never drawn from.
        let capacity = if ring.is_some() { 1 } else { 4096 };
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
            screen_held: RetainedBytes::default(),
            instances_held: RetainedBytes::default(),
            ring,
            counters: UploadCounters::default(),
        }
    }

    /// The render pipeline this layer draws with.
    #[cfg(test)]
    pub(crate) fn pipeline(&self) -> &wgpu::RenderPipeline {
        &self.pipeline
    }

    /// What this pipeline has written to the GPU so far.
    pub(crate) fn upload_counts(&self) -> UploadCounts {
        self.counters.snapshot()
    }

    /// Writes only what changed since the last upload (see `upload.rs`).
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        data: &[QuadInstance],
    ) {
        write_buffer_if_changed(
            queue,
            &self.screen_buf,
            &mut self.screen_held,
            bytemuck::bytes_of(&Screen {
                size: screen,
                _pad: [0.0; 2],
            }),
            &self.counters,
        );
        if let Some(ring) = &mut self.ring {
            let written = ring.write(
                device,
                queue,
                Some(&mut self.instances_held),
                bytemuck::cast_slice(data),
                &self.counters,
            );
            self.count = if written { data.len() as u32 } else { 0 };
            return;
        }
        if data.len() > self.capacity {
            // `grow_capacity` is checked because release builds use
            // `panic = "abort"`, so an overflow panic would abort the process.
            // Like `ImagePipeline::upload` (imgpipe.rs) and
            // `GlyphPipeline::upload` (glyphpipe.rs), skip this frame's quad
            // upload instead.
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
            self.instances_held.invalidate();
        }
        write_buffer_if_changed(
            queue,
            &self.instances,
            &mut self.instances_held,
            bytemuck::cast_slice(data),
            &self.counters,
        );
        self.count = data.len() as u32;
    }

    /// The instances the last upload left in the GPU buffer, read back from
    /// the retained copy. `None` when that copy does not cover them.
    pub(crate) fn uploaded(&self) -> Option<impl Iterator<Item = QuadInstance> + '_> {
        const SIZE: usize = std::mem::size_of::<QuadInstance>();
        let bytes = self
            .instances_held
            .bytes()
            .get(..self.count as usize * SIZE)?;
        let (records, _) = bytes.as_chunks::<SIZE>();
        Some(
            records
                .iter()
                .map(|record| bytemuck::pod_read_unaligned(record)),
        )
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        self.draw_hiding(pass, None);
    }

    /// Draw every instance except `hidden`, in their uploaded order, so
    /// what lies beneath the hidden ones blends exactly as without them.
    pub fn draw_hiding(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        hidden: Option<std::ops::Range<u32>>,
    ) {
        if self.count == 0 {
            return;
        }
        let instances = match &self.ring {
            Some(ring) => match ring.current() {
                Some(buffer) => buffer,
                None => return,
            },
            None => &self.instances,
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, instances.slice(..));
        for range in visible_instance_ranges(self.count, hidden) {
            if !range.is_empty() {
                pass.draw(0..4, range);
            }
        }
    }
}

/// The instance ranges left to draw out of `count` once `hidden` is skipped,
/// in order. A hidden range that is empty or out of bounds hides nothing.
pub(crate) fn visible_instance_ranges(
    count: u32,
    hidden: Option<std::ops::Range<u32>>,
) -> [std::ops::Range<u32>; 2] {
    match hidden {
        Some(h) if h.start < h.end && h.end <= count => [0..h.start, h.end..count],
        _ => [0..count, count..count],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The blink's off phase draws everything but the cursor, in the same
    /// order, as two contiguous ranges around it.
    #[test]
    fn hiding_a_range_keeps_the_rest_in_order() {
        assert_eq!(visible_instance_ranges(10, None), [0..10, 10..10]);
        assert_eq!(visible_instance_ranges(10, Some(3..5)), [0..3, 5..10]);
        assert_eq!(visible_instance_ranges(10, Some(0..2)), [0..0, 2..10]);
        assert_eq!(visible_instance_ranges(10, Some(8..10)), [0..8, 10..10]);
        // A range that is empty or past the end hides nothing.
        assert_eq!(visible_instance_ranges(10, Some(4..4)), [0..10, 10..10]);
        assert_eq!(visible_instance_ranges(10, Some(8..12)), [0..10, 10..10]);
        assert_eq!(visible_instance_ranges(0, Some(0..1)), [0..0, 0..0]);
    }

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

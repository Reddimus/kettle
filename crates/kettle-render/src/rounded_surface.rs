//! Bounded filled and stroked rounded surfaces for ordered UI layers.
use crate::upload::{RetainedBytes, UploadCounters, UploadCounts, write_buffer_if_changed};
use bytemuck::{Pod, Zeroable};
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct RoundedSurface {
    pub pos: [f32; 2],
    pub size: [f32; 2],
    pub fill: [f32; 4],
    pub stroke: [f32; 4],
    pub params: [f32; 4],
}
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Screen {
    size: [f32; 2],
    _pad: [f32; 2],
}
const SHADER: &str = r#"
struct Screen { size:vec2<f32>, pad:vec2<f32> };
@group(0) @binding(0) var<uniform> screen:Screen;
struct Out {
 @builtin(position) clip:vec4<f32>,
 @location(0) local:vec2<f32>,
 @location(1) size:vec2<f32>,
 @location(2) fill:vec4<f32>,
 @location(3) stroke:vec4<f32>,
 @location(4) params:vec4<f32>,
};
@vertex fn vs(@builtin(vertex_index) vi:u32,@location(0) pos:vec2<f32>,@location(1) size:vec2<f32>,@location(2) fill:vec4<f32>,@location(3) stroke:vec4<f32>,@location(4) params:vec4<f32>) -> Out {
 var corners=array<vec2<f32>,4>(vec2<f32>(0.0,0.0),vec2<f32>(1.0,0.0),vec2<f32>(0.0,1.0),vec2<f32>(1.0,1.0));
 let local=corners[vi]*size;
 let px=pos+local;
 var out:Out;
 out.clip=vec4<f32>(px.x/screen.size.x*2.0-1.0,1.0-px.y/screen.size.y*2.0,0.0,1.0);
 out.local=local; out.size=size; out.fill=fill; out.stroke=stroke; out.params=params;
 return out;
}
fn linear(c:vec3<f32>) -> vec3<f32> {
 return select(c/12.92,pow((c+vec3<f32>(0.055))/vec3<f32>(1.055),vec3<f32>(2.4)),c>vec3<f32>(0.04045));
}
@fragment fn fs(in:Out) -> @location(0) vec4<f32> {
 let radius=clamp(in.params.x,0.0,min(in.size.x,in.size.y)*0.5);
 let q=abs(in.local-in.size*0.5)-(in.size*0.5-vec2<f32>(radius));
 var distance=length(max(q,vec2<f32>(0.0)))+min(max(q.x,q.y),0.0)-radius;
 let p=(in.local-in.size*0.5)/(in.size*0.5);
 if in.params.z == 1.0 {
  // Play is vertically centered with a small rightward optical offset.
  distance=max(-p.x-0.5416667,p.x+abs(p.y)*1.7777778-0.7916667)*min(in.size.x,in.size.y)*0.5;
 } else if in.params.z == 2.0 {
  // Four fullscreen corners, each an L with a transparent center.
  let a=abs(p);
  distance=max(max(a.x,a.y)-0.85,max(0.4-min(a.x,a.y),0.65-max(a.x,a.y)))*min(in.size.x,in.size.y)*0.5;
 } else if in.params.z == 3.0 {
  // Speaker body plus sound wave, or diagonal mute stroke.
  let body=max(max(abs(p.x+0.45)-0.25,abs(p.y)-0.3),-p.x-0.8);
  let cone=max(abs(p.x+0.15)-0.3,abs(p.y)-(p.x+0.8));
  var sound=max(abs(length(p-vec2<f32>(-0.25,0.0))-0.95)-0.07,0.35-p.x);
  if in.params.w == 1.0 {
   sound=max(abs(p.x+p.y)-0.09,max(abs(p.x),abs(p.y))-0.85);
  }
  distance=min(min(body,cone),sound)*min(in.size.x,in.size.y)*0.5;
 }
 let aa=max(fwidth(distance)*0.5,0.25);
 let outer=1.0-smoothstep(-aa,aa,distance);
 let inner=1.0-smoothstep(-aa,aa,distance+max(in.params.y,0.0));
 let opacity=select(1.0,in.params.w,in.params.z==4.0);
 let fa=in.fill.a*inner*opacity;
 let sa=in.stroke.a*max(outer-inner,0.0);
 return vec4<f32>(linear(in.fill.rgb)*fa+linear(in.stroke.rgb)*sa,fa+sa);
}
"#;
pub struct RoundedSurfacePipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    screen_buf: wgpu::Buffer,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
    screen_held: RetainedBytes,
    instances_held: RetainedBytes,
    counters: UploadCounters,
}

impl RoundedSurfacePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-rounded-surface"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-rounded-surface-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kettle-rounded-surface-bgl"),
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
            label: Some("kettle-rounded-surface-bg"),
            layout: &bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kettle-rounded-surface-layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kettle-rounded-surface-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RoundedSurface>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4],
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
        let capacity = 32;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-rounded-surface-instances"),
            size: (capacity * std::mem::size_of::<RoundedSurface>()) as u64,
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
            counters: UploadCounters::default(),
        }
    }

    /// What this pipeline has written to the GPU so far.
    pub(crate) fn upload_counts(&self) -> UploadCounts {
        self.counters.snapshot()
    }

    /// Writes only what changed since the last upload (see `upload.rs`).
    pub fn upload(
        &mut self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        data: &[RoundedSurface],
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
        if data.len() > self.capacity {
            log::warn!("rounded surface budget exceeded; skipping frame");
            self.count = 0;
            return;
        }
        write_buffer_if_changed(
            queue,
            &self.instances,
            &mut self.instances_held,
            bytemuck::cast_slice(data),
            &self.counters,
        );
        self.count = u32::try_from(data.len()).unwrap_or_else(|_| {
            log::warn!(
                "rounded surface instance count {} exceeds u32; skipping upload",
                data.len()
            );
            0
        });
    }

    pub fn draw_range<'a>(&'a self, pass: &mut wgpu::RenderPass<'a>, range: std::ops::Range<u32>) {
        let start = range.start.min(self.count);
        let end = range.end.min(self.count);
        if start >= end {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        pass.draw(0..4, start..end);
    }
}

//! Textured-quad pipeline for compositing decoded images (Sixel / kitty /
//! iTerm2) onto the grid. Textures are cached by `ImageData` identity so a
//! static image uploads once.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use kettle_core::ImageData;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Inst {
    pos: [f32; 2],
    size: [f32; 2],
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
@group(1) @binding(0) var tex: texture_2d<f32>;
@group(1) @binding(1) var smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32,
      @location(0) pos: vec2<f32>,
      @location(1) size: vec2<f32>) -> VsOut {
    var c = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0));
    let corner = c[vi];
    let px = pos + corner * size;
    let ndc = vec2<f32>(px.x / screen.size.x * 2.0 - 1.0,
                         1.0 - px.y / screen.size.y * 2.0);
    var o: VsOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = corner;
    return o;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(tex, smp, in.uv);
    return vec4<f32>(c.rgb * c.a, c.a);
}
"#;

/// A cached GPU texture for one decoded image, plus a clone of the `Arc`
/// whose pointer is the cache key.
///
/// Cycle 807 (audit, ABA fix): the cache key is `Arc::as_ptr(&img.rgba)` —
/// the heap address of the pixel buffer. Holding an `Arc` clone here **pins
/// that address** for exactly as long as the entry lives in the cache, so a
/// dropped-then-reallocated image can't land on a still-cached key and bind
/// the wrong texture. Without this, image A could cache at address `P`, A
/// gets dropped freeing `P`, and a *different* image B's buffer reallocates
/// at `P` before [`ImagePipeline::gc`] evicts A — making `ensure_texture(B)`
/// hit A's stale entry and draw A's pixels for B. The clone is just an
/// `Arc` refcount bump (the buffer is already shared with the VT layer), and
/// `gc` releases it the first frame the image isn't drawn.
struct CachedTexture {
    /// Keeps the keyed pixel-buffer address alive while cached (see above).
    _rgba: std::sync::Arc<Vec<u8>>,
    bind_group: wgpu::BindGroup,
}

pub struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    tex_bgl: wgpu::BindGroupLayout,
    screen_buf: wgpu::Buffer,
    screen_bg: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    cap: usize,
    cache: HashMap<usize, CachedTexture>,
    draws: Vec<(usize, u32)>, // (cache key, instance index)
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-img"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let screen_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img-screen-bgl"),
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
        let tex_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("img-tex-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("img-layout"),
            bind_group_layouts: &[Some(&screen_bgl), Some(&tex_bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("img-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Inst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
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
        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let screen_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("img-screen-bg"),
            layout: &screen_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: screen_buf.as_entire_binding(),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("img-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let cap = 64;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img-instances"),
            size: (cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            tex_bgl,
            screen_buf,
            screen_bg,
            sampler,
            instances,
            cap,
            cache: HashMap::new(),
            draws: Vec::new(),
        }
    }

    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        img: &ImageData,
    ) -> Option<usize> {
        // Cycle 813 (audit) defense-in-depth: never hand wgpu a texture larger
        // than the device supports — that's a validation error wgpu's default
        // (no error-scope) handler turns into a panic, and panic=abort makes it
        // a whole-process abort. `ImageData::new` already caps dims at
        // MAX_IMAGE_DIM (8192) for every decode path, but a struct-literal
        // construction (e.g. the background-image decode in lib.rs) bypasses
        // `new`, so this is the last guard before `create_texture`. Skipping the
        // draw is strictly better than aborting the renderer.
        let max = device.limits().max_texture_dimension_2d;
        if img.width > max || img.height > max {
            log::warn!(
                "skipping {}x{} image: exceeds GPU max_texture_dimension_2d {max}",
                img.width,
                img.height
            );
            return None;
        }
        let key = std::sync::Arc::as_ptr(&img.rgba) as usize;
        if self.cache.contains_key(&key) {
            return Some(key);
        }
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kettle-image"),
            size: wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: Some(img.height),
            },
            wgpu::Extent3d {
                width: img.width,
                height: img.height,
                depth_or_array_layers: 1,
            },
        );
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("img-tex-bg"),
            layout: &self.tex_bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });
        // Store an `Arc` clone alongside the bind group so the keyed buffer
        // address stays pinned while cached (cycle 807 ABA guard — see
        // `CachedTexture`).
        self.cache.insert(
            key,
            CachedTexture {
                _rgba: img.rgba.clone(),
                bind_group: bg,
            },
        );
        Some(key)
    }

    /// `items`: `(x, y, w, h, image)` in physical pixels.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        items: &[(f32, f32, f32, f32, ImageData)],
    ) {
        queue.write_buffer(
            &self.screen_buf,
            0,
            bytemuck::bytes_of(&Screen {
                size: screen,
                _pad: [0.0; 2],
            }),
        );
        self.draws.clear();
        if items.is_empty() {
            return;
        }
        if items.len() > self.cap {
            self.cap = items.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("img-instances"),
                size: (self.cap * std::mem::size_of::<Inst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let mut insts = Vec::with_capacity(items.len());
        for (i, (x, y, w, h, img)) in items.iter().enumerate() {
            // Push the instance for every item so buffer slot `i` stays aligned
            // with the enumerate index stored in `draws`; only record a draw for
            // images that produced a texture (cycle 813: an oversized image is
            // skipped rather than aborting the renderer).
            insts.push(Inst {
                pos: [*x, *y],
                size: [*w, *h],
            });
            if let Some(key) = self.ensure_texture(device, queue, img) {
                self.draws.push((key, i as u32));
            }
        }
        queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&insts));
    }

    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        if self.draws.is_empty() {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.screen_bg, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        for (key, idx) in &self.draws {
            if let Some(cached) = self.cache.get(key) {
                pass.set_bind_group(1, &cached.bind_group, &[]);
                pass.draw(0..4, *idx..*idx + 1);
            }
        }
    }

    /// Forget textures no longer referenced this frame.
    pub fn gc(&mut self, live: &std::collections::HashSet<usize>) {
        self.cache.retain(|k, _| live.contains(k));
    }
}

#[cfg(test)]
mod aba_guard_tests {
    /// Cycle 807 drift guard (audit, ABA fix). The image cache keys textures
    /// by the rgba `Arc`'s raw pointer; it MUST hold an `Arc` clone
    /// (`CachedTexture._rgba`) to pin that address while the entry is cached,
    /// or a dropped-then-reallocated image can collide on a stale key and draw
    /// the wrong texture. The field is `_`-prefixed (never read), so a future
    /// "remove the unused field" cleanup would silently reintroduce the
    /// hazard — exercising the cache needs a real GPU device, so pin the
    /// invariant at the source level (same approach as the pane-buffer
    /// lifecycle guards in `lib.rs`).
    #[test]
    fn cache_pins_arc_to_prevent_address_reuse() {
        let src = include_str!("imgpipe.rs");
        assert!(
            src.contains("_rgba: std::sync::Arc<Vec<u8>>"),
            "CachedTexture must keep an Arc clone to pin the cache-key address"
        );
        assert!(
            src.contains("_rgba: img.rgba.clone()"),
            "ensure_texture must store the Arc clone so the keyed address stays pinned"
        );
    }

    /// The property the pin relies on, as pure `Arc` semantics (no GPU): a
    /// clone shares the pointer used as the cache key and keeps the buffer —
    /// and therefore that exact address — alive after the VT layer drops its
    /// own reference, so nothing else can allocate at the still-cached key.
    #[test]
    fn arc_clone_shares_pointer_and_keeps_address_alive() {
        use std::sync::Arc;
        let rgba: Arc<Vec<u8>> = Arc::new(vec![1, 2, 3, 4]);
        let key = Arc::as_ptr(&rgba) as usize;
        let pinned = rgba.clone(); // stands in for CachedTexture._rgba
        assert_eq!(Arc::as_ptr(&pinned) as usize, key);
        assert_eq!(Arc::strong_count(&rgba), 2);
        // VT layer drops its reference; the pin keeps the buffer (and its
        // address) alive, so `key` cannot be reused while cached.
        drop(rgba);
        assert_eq!(Arc::strong_count(&pinned), 1);
        assert_eq!(Arc::as_ptr(&pinned) as usize, key);
    }
}

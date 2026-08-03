//! Textured-quad pipeline for compositing decoded images (Sixel / kitty /
//! iTerm2) onto the grid. Textures are cached by `ImageData` identity so a
//! static image uploads once.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use kettle_core::{
    GraphicsBudget, GraphicsReservation, ImageData, ImageSourceCrop, ImageSourceRect,
};

pub(crate) struct ImageItem {
    rect: [f32; 4],
    image: ImageData,
    source_rect: Option<ImageSourceRect>,
    source_crop: Option<ImageSourceCrop>,
    /// Optional destination clip in physical surface pixels. Inline terminal
    /// images use the owning pane's grid viewport; wallpapers stay unclipped.
    clip_rect: Option<[f32; 4]>,
}

impl ImageItem {
    pub(crate) fn full(x: f32, y: f32, width: f32, height: f32, image: ImageData) -> Self {
        Self {
            rect: [x, y, width, height],
            image,
            source_rect: None,
            source_crop: None,
            clip_rect: None,
        }
    }

    pub(crate) fn placement(
        rect: [f32; 4],
        image: ImageData,
        source_rect: Option<ImageSourceRect>,
        source_crop: Option<ImageSourceCrop>,
        clip_rect: [f32; 4],
    ) -> Self {
        Self {
            rect,
            image,
            source_rect,
            source_crop,
            clip_rect: Some(clip_rect),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Inst {
    pos: [f32; 2],
    size: [f32; 2],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
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
      @location(1) size: vec2<f32>,
      @location(2) uv_origin: vec2<f32>,
      @location(3) uv_size: vec2<f32>) -> VsOut {
    var c = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 1.0));
    let corner = c[vi];
    let px = pos + corner * size;
    let ndc = vec2<f32>(px.x / screen.size.x * 2.0 - 1.0,
                         1.0 - px.y / screen.size.y * 2.0);
    var o: VsOut;
    o.clip = vec4<f32>(ndc, 0.0, 1.0);
    o.uv = uv_origin + corner * uv_size;
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
/// ABA fix: the cache key is `Arc::as_ptr(&img.rgba)` —
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
    /// Keeps both the keyed pixels and their CPU reservation alive.
    _image: ImageData,
    /// Accounts the retained GPU allocation until cache eviction.
    _gpu: GraphicsReservation,
    bind_group: wgpu::BindGroup,
    last_used: u64,
}

fn rgba_texture_bytes(width: u32, height: u32) -> Option<usize> {
    let row = u64::from(width).checked_mul(4)?;
    let aligned_row = row.checked_add(255)? & !255;
    aligned_row.checked_mul(u64::from(height))?.try_into().ok()
}

fn rgba_pixel_bytes(width: u32, height: u32) -> Option<usize> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?
        .try_into()
        .ok()
}

fn source_uv(
    image: &ImageData,
    source_rect: Option<ImageSourceRect>,
    source_crop: Option<ImageSourceCrop>,
) -> Option<([f32; 2], [f32; 2])> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let (uv_origin, uv_size) = if let Some(source) = source_rect {
        let x1 = source.x.checked_add(source.width)?;
        let y1 = source.y.checked_add(source.height)?;
        if source.width == 0 || source.height == 0 || x1 > image.width || y1 > image.height {
            return None;
        }

        // Sample sub-rect edges at pixel centers. A cropped texture would clamp
        // there; doing the same in the shared parent texture prevents linear
        // filtering from bleeding adjacent placeholder tiles into one another.
        let image_w = image.width as f32;
        let image_h = image.height as f32;
        let u0 = (source.x as f32 + 0.5) / image_w;
        let v0 = (source.y as f32 + 0.5) / image_h;
        let u1 = (x1 as f32 - 0.5) / image_w;
        let v1 = (y1 as f32 - 0.5) / image_h;
        ([u0, v0], [u1 - u0, v1 - v0])
    } else {
        ([0.0, 0.0], [1.0, 1.0])
    };
    let Some(crop) = source_crop else {
        return Some((uv_origin, uv_size));
    };
    if !crop.top.is_finite()
        || !crop.bottom.is_finite()
        || crop.top < 0.0
        || crop.bottom > 1.0
        || crop.top >= crop.bottom
    {
        return None;
    }
    Some((
        [uv_origin[0], uv_origin[1] + uv_size[1] * crop.top],
        [uv_size[0], uv_size[1] * (crop.bottom - crop.top)],
    ))
}

/// Build one image instance, clipping its destination and UVs together.
///
/// Clipping on the CPU keeps each pane's images in the existing globally
/// batched draw list without relying on mutable render-pass scissor state.
/// Adjusting the UVs by the same normalized fractions is essential: clamping
/// only the destination would squash the entire source into the visible slice.
fn clipped_instance(
    rect: [f32; 4],
    uv_origin: [f32; 2],
    uv_size: [f32; 2],
    clip_rect: Option<[f32; 4]>,
) -> Option<Inst> {
    if !rect
        .into_iter()
        .chain(uv_origin)
        .chain(uv_size)
        .all(f32::is_finite)
        || rect[2] <= 0.0
        || rect[3] <= 0.0
    {
        return None;
    }

    let Some(clip) = clip_rect else {
        return Some(Inst {
            pos: [rect[0], rect[1]],
            size: [rect[2], rect[3]],
            uv_origin,
            uv_size,
        });
    };
    if !clip.into_iter().all(f32::is_finite) || clip[2] <= 0.0 || clip[3] <= 0.0 {
        return None;
    }

    let rect_end = [rect[0] + rect[2], rect[1] + rect[3]];
    let clip_end = [clip[0] + clip[2], clip[1] + clip[3]];
    if !rect_end.into_iter().chain(clip_end).all(f32::is_finite) {
        return None;
    }
    let x0 = rect[0].max(clip[0]);
    let y0 = rect[1].max(clip[1]);
    let x1 = rect_end[0].min(clip_end[0]);
    let y1 = rect_end[1].min(clip_end[1]);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }

    let u0 = (x0 - rect[0]) / rect[2];
    let v0 = (y0 - rect[1]) / rect[3];
    let u1 = (x1 - rect[0]) / rect[2];
    let v1 = (y1 - rect[1]) / rect[3];
    let instance = Inst {
        pos: [x0, y0],
        size: [x1 - x0, y1 - y0],
        uv_origin: [
            uv_origin[0] + uv_size[0] * u0,
            uv_origin[1] + uv_size[1] * v0,
        ],
        uv_size: [uv_size[0] * (u1 - u0), uv_size[1] * (v1 - v0)],
    };
    instance
        .pos
        .into_iter()
        .chain(instance.size)
        .chain(instance.uv_origin)
        .chain(instance.uv_size)
        .all(f32::is_finite)
        .then_some(instance)
}

fn capped_instance_count(requested: usize, max_instances: usize) -> usize {
    requested.min(max_instances)
}

/// Decide whether an "N image placements dropped" warning should fire this
/// frame, given how many placements are being dropped and what was last
/// warned about. Returns `(should_warn, next_last_warned)`.
///
/// Only warns on a *transition* to a new drop count (including 0 -> N and any
/// change in N), never every frame of a steady-state overflow — a REPL or TUI
/// pinned above the placement budget would otherwise spam one `log::warn!`
/// per frame forever. Dropping back to 0 clears the memory, so the next
/// overflow (even at the same count) is reported again as a fresh event.
fn dropped_warn_transition(dropped: usize, last_warned: Option<usize>) -> (bool, Option<usize>) {
    if dropped == 0 {
        (false, None)
    } else if last_warned == Some(dropped) {
        (false, last_warned)
    } else {
        (true, Some(dropped))
    }
}

fn record_draw(draws: &mut Vec<(usize, u32, u32)>, key: usize, index: u32) {
    if let Some((last_key, start, count)) = draws.last_mut()
        && *last_key == key
        && start.saturating_add(*count) == index
    {
        *count += 1;
    } else {
        draws.push((key, index, 1));
    }
}

pub struct ImagePipeline {
    pipeline: wgpu::RenderPipeline,
    tex_bgl: wgpu::BindGroupLayout,
    screen_buf: wgpu::Buffer,
    screen_bg: wgpu::BindGroup,
    sampler: wgpu::Sampler,
    _screen_gpu: GraphicsReservation,
    instances: wgpu::Buffer,
    instance_gpu: GraphicsReservation,
    cap: usize,
    cache: HashMap<usize, CachedTexture>,
    draws: Vec<(usize, u32, u32)>, // (cache key, first instance, count)
    budget: GraphicsBudget,
    max_instances: usize,
    epoch: u64,
    /// Drop count from the last frame a "skipping N image placements"
    /// warning fired, so `upload` logs once per exceedance transition
    /// instead of every frame of a steady-state overflow. `None` once the
    /// backlog clears (or on startup).
    last_dropped_warn: Option<usize>,
}

impl ImagePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Option<Self> {
        Self::new_with_budget(device, format, GraphicsBudget::default())
    }

    pub fn new_with_budget(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        budget: GraphicsBudget,
    ) -> Option<Self> {
        let max_instances = budget.limits().placements;
        Self::new_with_budget_and_instance_limit(device, format, budget, max_instances)
    }

    pub(crate) fn new_with_budget_and_instance_limit(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        budget: GraphicsBudget,
        max_instances: usize,
    ) -> Option<Self> {
        if max_instances == 0 {
            return None;
        }
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
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Inst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2,
                        1 => Float32x2,
                        2 => Float32x2,
                        3 => Float32x2
                    ],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    // This pipeline's fragment shader returns PREMULTIPLIED
                    // color (`rgb * a`), so the blend must not apply alpha a
                    // second time. `ALPHA_BLENDING` uses `SrcAlpha` for the
                    // source factor, which computed `rgb * a * a` — a
                    // 50%-opaque surface contributed 25%, darkening every
                    // translucent image, panel, highlight, and separator.
                    // (`glyphpipe` deliberately returns STRAIGHT alpha and
                    // correctly keeps `ALPHA_BLENDING`.)
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
        let screen_bytes = std::mem::size_of::<Screen>();
        let screen_gpu = budget.reserve_gpu(screen_bytes)?;
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
        let cap = 64.min(max_instances);
        let instance_bytes = cap.checked_mul(std::mem::size_of::<Inst>())?;
        let instance_gpu = budget.reserve_gpu(instance_bytes)?;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img-instances"),
            size: (cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(Self {
            pipeline,
            tex_bgl,
            screen_buf,
            screen_bg,
            sampler,
            _screen_gpu: screen_gpu,
            instances,
            instance_gpu,
            cap,
            cache: HashMap::new(),
            draws: Vec::new(),
            budget,
            max_instances,
            epoch: 0,
            last_dropped_warn: None,
        })
    }

    fn ensure_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        img: &ImageData,
    ) -> Option<usize> {
        // Defense-in-depth: never hand wgpu a texture larger
        // than the device supports — that's a validation error wgpu's default
        // (no error-scope) handler turns into a panic, and panic=abort makes it
        // a whole-process abort. `ImageData::new` already caps dims at
        // MAX_IMAGE_DIM (8192) for every decode path, but a struct-literal
        // construction (e.g. the background-image decode in lib.rs) bypasses
        // `new`, so this is the last guard before `create_texture`. Skipping the
        // draw is strictly better than aborting the renderer.
        let max = device.limits().max_texture_dimension_2d;
        if img.width == 0 || img.height == 0 || img.width > max || img.height > max {
            log::warn!(
                "skipping {}x{} image: exceeds GPU max_texture_dimension_2d {max}",
                img.width,
                img.height
            );
            return None;
        }
        let Some(expected_bytes) = rgba_pixel_bytes(img.width, img.height) else {
            log::warn!("skipping image with overflowing texture byte size");
            return None;
        };
        if expected_bytes != img.byte_len() || expected_bytes > self.budget.limits().image_bytes {
            log::warn!(
                "skipping {}x{} image: {} bytes exceeds/mismatches the texture budget",
                img.width,
                img.height,
                img.byte_len()
            );
            return None;
        }
        let texture_bytes = rgba_texture_bytes(img.width, img.height)?;
        let key = img.allocation_key();
        if let Some(cached) = self.cache.get_mut(&key) {
            cached.last_used = self.epoch;
            return Some(key);
        }
        // Reserve before creating or uploading. The cache's RAII token keeps
        // both per-window and process GPU counters charged until eviction.
        let Some(gpu_reservation) = self.budget.reserve_gpu(texture_bytes) else {
            log::warn!("skipping image texture: GPU graphics budget exhausted");
            return None;
        };
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
                bytes_per_row: img.width.checked_mul(4),
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
        // address stays pinned while cached (ABA guard — see
        // `CachedTexture`).
        self.cache.insert(
            key,
            CachedTexture {
                _image: img.clone(),
                _gpu: gpu_reservation,
                bind_group: bg,
                last_used: self.epoch,
            },
        );
        Some(key)
    }

    /// Image rectangles are in physical pixels; source rectangles are in the
    /// referenced image's pixel coordinates.
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        items: &[ImageItem],
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
        self.epoch = self.epoch.saturating_add(1);
        if items.is_empty() {
            // No placements this frame, so nothing is being dropped; clear the
            // transition memory so a later overflow (even at the same count)
            // is reported as a fresh event rather than staying suppressed.
            self.last_dropped_warn = None;
            return;
        }
        let item_count = capped_instance_count(items.len(), self.max_instances);
        let dropped = items.len() - item_count;
        let (should_warn, next_warn_state) =
            dropped_warn_transition(dropped, self.last_dropped_warn);
        self.last_dropped_warn = next_warn_state;
        if should_warn {
            log::warn!(
                "skipping {dropped} image placement(s): per-frame budget of {} exceeded ({} requested this frame)",
                self.max_instances,
                items.len()
            );
        }
        if item_count > self.cap {
            let Some(next_cap) = item_count.checked_next_power_of_two() else {
                log::warn!("image instance count overflow; skipping frame images");
                return;
            };
            let next_cap = next_cap.min(self.max_instances);
            let Some(bytes) = next_cap.checked_mul(std::mem::size_of::<Inst>()) else {
                log::warn!("image instance buffer size overflow; skipping frame images");
                return;
            };
            let Some(instance_gpu) = self.budget.reserve_gpu(bytes) else {
                log::warn!("image instance buffer growth exceeds GPU graphics budget");
                return;
            };
            let instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("img-instances"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instances = instances;
            self.instance_gpu = instance_gpu;
            self.cap = next_cap;
        }
        let mut insts = Vec::with_capacity(item_count);
        for (i, item) in items.iter().take(item_count).enumerate() {
            // Push the instance for every item so buffer slot `i` stays aligned
            // with the enumerate index stored in `draws`. Invalid or wholly
            // clipped items receive a zero-sized slot and no draw.
            let uv = source_uv(&item.image, item.source_rect, item.source_crop);
            let Some((uv_origin, uv_size)) = uv else {
                insts.push(Inst::zeroed());
                log::warn!("skipping image placement with an invalid source rectangle");
                continue;
            };
            let Some(instance) = clipped_instance(item.rect, uv_origin, uv_size, item.clip_rect)
            else {
                insts.push(Inst::zeroed());
                continue;
            };
            insts.push(instance);
            if let Some(key) = self.ensure_texture(device, queue, &item.image) {
                record_draw(&mut self.draws, key, i as u32);
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
        for (key, first, count) in &self.draws {
            if let Some(cached) = self.cache.get(key) {
                pass.set_bind_group(1, &cached.bind_group, &[]);
                pass.draw(0..4, *first..first.saturating_add(*count));
            }
        }
    }

    /// Forget textures no longer referenced this frame.
    pub fn gc(&mut self, live: &std::collections::HashSet<usize>) {
        let mut dead: Vec<(u64, usize)> = self
            .cache
            .iter()
            .filter_map(|(&key, cached)| (!live.contains(&key)).then_some((cached.last_used, key)))
            .collect();
        dead.sort_unstable();
        for (_, key) in dead {
            self.cache.remove(&key);
        }
    }
}

#[cfg(test)]
mod aba_guard_tests {
    use kettle_core::{ImageData, ImageSourceCrop, ImageSourceRect};

    /// Drift guard (ABA fix). The image cache keys textures
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
            src.contains("_image: ImageData"),
            "CachedTexture must keep an ImageData clone to pin pixels + CPU reservation"
        );
        assert!(
            src.contains("_image: img.clone()"),
            "ensure_texture must store the image clone so the keyed address stays pinned"
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

    #[test]
    fn texture_byte_math_accepts_limit_and_identifies_one_past() {
        let limit = kettle_core::GraphicsLimits::default().image_bytes;
        assert_eq!(super::rgba_texture_bytes(8192, 2048), Some(limit));
        assert_eq!(
            super::rgba_texture_bytes(8192, 2049),
            Some(limit + 8192 * 4)
        );
        assert_eq!(super::rgba_texture_bytes(u32::MAX, u32::MAX), None);
        assert_eq!(super::rgba_pixel_bytes(1, 8192), Some(8192 * 4));
        assert_eq!(super::rgba_texture_bytes(1, 8192), Some(8192 * 256));
    }

    #[test]
    fn source_rect_uses_pixel_centers_and_rejects_out_of_bounds() {
        let image = ImageData::new(4, 2, vec![0; 4 * 2 * 4]).expect("test image");
        assert_eq!(
            super::source_uv(&image, None, None),
            Some(([0.0; 2], [1.0; 2]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                Some(ImageSourceRect {
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                }),
                None,
            ),
            Some(([0.625, 0.25], [0.25, 0.5]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                None,
                Some(ImageSourceCrop {
                    top: 0.25,
                    bottom: 0.75,
                }),
            ),
            Some(([0.0, 0.25], [1.0, 0.5]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                Some(ImageSourceRect {
                    x: 2,
                    y: 0,
                    width: 2,
                    height: 2,
                }),
                Some(ImageSourceCrop {
                    top: 0.25,
                    bottom: 0.75,
                }),
            ),
            Some(([0.625, 0.375], [0.25, 0.25]))
        );
        assert_eq!(
            super::source_uv(
                &image,
                None,
                Some(ImageSourceCrop {
                    top: 0.75,
                    bottom: 0.25,
                }),
            ),
            None
        );
        assert_eq!(
            super::source_uv(
                &image,
                Some(ImageSourceRect {
                    x: 4,
                    y: 0,
                    width: 1,
                    height: 1,
                }),
                None,
            ),
            None
        );
    }

    #[test]
    fn pane_clip_crops_destination_and_uvs_without_squashing() {
        let inst = super::clipped_instance(
            [-4.0, -8.0, 16.0, 16.0],
            [0.0, 0.0],
            [1.0, 1.0],
            Some([0.0, 0.0, 8.0, 4.0]),
        )
        .expect("partially visible image");
        assert_eq!(inst.pos, [0.0, 0.0]);
        assert_eq!(inst.size, [8.0, 4.0]);
        assert_eq!(inst.uv_origin, [0.25, 0.5]);
        assert_eq!(inst.uv_size, [0.5, 0.25]);
    }

    #[test]
    fn pane_clip_rejects_fully_outside_or_degenerate_destinations() {
        assert!(
            super::clipped_instance(
                [-20.0, 0.0, 4.0, 4.0],
                [0.0; 2],
                [1.0; 2],
                Some([0.0, 0.0, 10.0, 10.0])
            )
            .is_none()
        );
        assert!(
            super::clipped_instance(
                [0.0, 0.0, 0.0, 4.0],
                [0.0; 2],
                [1.0; 2],
                Some([0.0, 0.0, 10.0, 10.0])
            )
            .is_none()
        );
        assert!(
            super::clipped_instance(
                [f32::MAX, 0.0, f32::MAX, 4.0],
                [0.0; 2],
                [1.0; 2],
                Some([0.0, 0.0, 10.0, 10.0])
            )
            .is_none()
        );
    }

    #[test]
    fn wallpaper_without_clip_preserves_destination_and_uvs() {
        let inst = super::clipped_instance([-2.0, 3.0, 8.0, 9.0], [0.1, 0.2], [0.6, 0.7], None)
            .expect("valid wallpaper");
        assert_eq!(inst.pos, [-2.0, 3.0]);
        assert_eq!(inst.size, [8.0, 9.0]);
        assert_eq!(inst.uv_origin, [0.1, 0.2]);
        assert_eq!(inst.uv_size, [0.6, 0.7]);
    }

    #[test]
    fn wallpaper_and_inline_instance_limits_are_independent() {
        let inline = kettle_core::GraphicsLimits::default().placements;
        assert_eq!(super::capped_instance_count(4096, inline), inline);
        assert_eq!(super::capped_instance_count(4096, 4096), 4096);
    }

    #[test]
    fn dropped_warn_fires_once_per_exceedance_transition() {
        // First overflow: no prior warning recorded -> warn, remember 5.
        assert_eq!(super::dropped_warn_transition(5, None), (true, Some(5)));
        // Same drop count next frame (steady-state overflow) -> stay silent.
        assert_eq!(super::dropped_warn_transition(5, Some(5)), (false, Some(5)));
        // Drop count changes (worse overflow) -> warn again, remember 9.
        assert_eq!(super::dropped_warn_transition(9, Some(5)), (true, Some(9)));
        // Backlog clears -> reset memory, no warning for zero drops.
        assert_eq!(super::dropped_warn_transition(0, Some(9)), (false, None));
        // A fresh overflow at the *same* count as before the clear is
        // reported again rather than staying suppressed.
        assert_eq!(super::dropped_warn_transition(9, None), (true, Some(9)));
    }

    #[test]
    fn consecutive_instances_of_one_texture_are_batched() {
        let mut draws = Vec::new();
        super::record_draw(&mut draws, 10, 0);
        super::record_draw(&mut draws, 10, 1);
        super::record_draw(&mut draws, 20, 2);
        super::record_draw(&mut draws, 10, 3);
        assert_eq!(draws, vec![(10, 0, 2), (20, 2, 1), (10, 3, 1)]);
    }
}

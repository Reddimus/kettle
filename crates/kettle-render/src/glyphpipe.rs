//! Cell-locked instanced glyph pipeline (v2.25.0).
//!
//! Terminal text must sit on a fixed cell grid: every grapheme cluster's glyph
//! has to render at exactly `pane_origin + col * cell_w`. glyphon/cosmic-text lay
//! out a row as ONE continuous advance-positioned run, so any glyph whose advance
//! ≠ the measured monospace cell width — fallback punctuation (`— · " " …`), Nerd
//! icons, color emoji, CJK, ligature clusters, a bold/italic face with a
//! different width — shifts every following glyph off the `col * cell_w` grid
//! that selection highlights, the block cursor, link underlines and mouse
//! hit-testing all assume. Glyphs drift, the grid does not → "every now and then
//! the text is misaligned" and "selecting text is off by one letter".
//!
//! This module renders pane text the way Alacritty / kitty / WezTerm / Ghostty
//! do: the row is still shaped by cosmic-text (unchanged — the per-line shaping
//! cache stays warm), but instead of handing the whole `Buffer` to glyphon we
//! walk the laid-out glyphs and emit ONE instanced textured quad per glyph,
//! pinned to its grid cell. The bitmaps come from cosmic-text's own `SwashCache`
//! (byte-identical to what glyphon would rasterize) and the WGSL fragment
//! replicates glyphon's shader exactly — mask glyphs as `sRGB→linear(fg) ·
//! coverage`, color glyphs as a straight sample of an sRGB atlas — so antialias,
//! gamma and theme colors are pixel-identical to the old path. Only the X
//! position changes, and only for glyphs that used to drift.
//!
//! Because a primary-face monospace glyph already has advance == `cell_w`, its
//! cell-locked position is IDENTICAL to the old continuous layout — so this is a
//! no-op for ordinary ASCII and a fix purely where drift occurred.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use glyphon::cosmic_text::{CacheKey, SwashContent, SwashImage};

/// One pinned glyph quad. `kind` selects the atlas: 0 = color, 1 = mask.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub struct GlyphInstance {
    /// Top-left in physical pixels.
    pub pos: [f32; 2],
    /// Size in physical pixels (= the atlas slot's pixel size).
    pub size: [f32; 2],
    /// Atlas slot top-left in atlas PIXELS. The shader normalizes by the live
    /// `textureDimensions`, so growing the atlas never invalidates an instance.
    pub uv: [f32; 2],
    /// Straight-alpha sRGB rgba. Used for mask glyphs (the glyph fg); ignored
    /// for color glyphs (the atlas already carries their color).
    pub color: [f32; 4],
    /// 0 = sample the color atlas, 1 = sample the mask atlas.
    pub kind: u32,
    pub _pad: [u32; 3],
}

/// A rasterized glyph the caller hands to the atlas. Borrowing the bitmap (and
/// keeping `FontSystem` / `SwashCache` out of this module entirely) lets the
/// caller hold the disjoint field borrows it needs while emitting.
pub struct RasterGlyph<'a> {
    /// true => 4-channel straight RGBA color glyph; false => 1-channel coverage.
    pub color: bool,
    pub width: u32,
    pub height: u32,
    /// `placement.left` — horizontal bearing from the pen origin to the bitmap.
    pub left: i32,
    /// `placement.top` — vertical bearing from the baseline to the bitmap top.
    pub top: i32,
    pub data: &'a [u8],
}

impl<'a> RasterGlyph<'a> {
    /// Adapt a cosmic-text `SwashImage` (the exact bitmap glyphon uses).
    pub fn from_swash(img: &'a SwashImage) -> Option<Self> {
        let color = match img.content {
            SwashContent::Color => true,
            SwashContent::Mask => false,
            // Not implemented upstream either (glyphon falls back to Mask and
            // gets an empty image); skip rather than draw garbage.
            SwashContent::SubpixelMask => return None,
        };
        Some(Self {
            color,
            width: img.placement.width,
            height: img.placement.height,
            left: img.placement.left,
            top: img.placement.top,
            data: &img.data,
        })
    }
}

/// A per-pane scissor region + the contiguous instance range that belongs to it.
/// Pane glyphs are emitted pane-by-pane, so each pane owns one contiguous range;
/// drawing each range under its own scissor rect clips text to its pane — the
/// per-`TextArea` bounds clip glyphon used to do — so a glyph (italic overhang, a
/// fallback glyph wider than its cell) can't bleed into a sibling pane or chrome.
#[derive(Clone, Copy)]
pub struct GlyphClip {
    /// Pane rect in physical pixels: `[x, y, w, h]`.
    pub rect: [f32; 4],
    pub start: u32,
    pub count: u32,
}

/// Where a glyph lives in the atlas + the bearings needed to place its quad.
#[derive(Clone, Copy)]
pub struct GlyphSlot {
    pub kind: u32,
    pub atlas_x: f32,
    pub atlas_y: f32,
    pub w: f32,
    pub h: f32,
    pub left: i32,
    pub top: i32,
}

/// A single-format atlas texture with a trivial shelf packer. Append-only +
/// grow-by-doubling-height with a content-preserving copy, so a glyph's pixel
/// coords never move once placed (instances stay valid across a grow).
struct Atlas {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    bpp: u32,
    width: u32,
    height: u32,
    // Shelf cursor.
    cursor_x: u32,
    shelf_y: u32,
    shelf_h: u32,
}

/// 1px gutter so neighboring glyphs can never sample into each other.
const GUTTER: u32 = 1;

impl Atlas {
    fn new(device: &wgpu::Device, label: &str, format: wgpu::TextureFormat, bpp: u32) -> Self {
        let (width, height) = (1024u32, 512u32);
        let tex = Self::make_tex(device, label, format, width, height);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            tex,
            view,
            format,
            bpp,
            width,
            height,
            cursor_x: 0,
            shelf_y: 0,
            shelf_h: 0,
        }
    }

    fn make_tex(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Reserve a `w×h` rectangle (with gutter). `None` ⇒ the current texture is
    /// out of vertical room and the caller must `grow`.
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (gw, gh) = (w + GUTTER, h + GUTTER);
        if self.cursor_x + gw > self.width {
            // Next shelf.
            self.cursor_x = 0;
            self.shelf_y += self.shelf_h;
            self.shelf_h = 0;
        }
        if self.shelf_y + gh > self.height {
            return None;
        }
        let (x, y) = (self.cursor_x, self.shelf_y);
        self.cursor_x += gw;
        self.shelf_h = self.shelf_h.max(gh);
        Some((x, y))
    }

    /// Double the height (clamped to `max_dim`), preserving existing pixels.
    /// Returns false when already at the device limit.
    fn grow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        label: &str,
        max_dim: u32,
    ) -> bool {
        let new_h = (self.height * 2).min(max_dim);
        if new_h <= self.height {
            return false;
        }
        let new_tex = Self::make_tex(device, label, self.format, self.width, new_h);
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("kettle-glyph-atlas-grow"),
        });
        enc.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &new_tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(enc.finish()));
        self.view = new_tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.tex = new_tex;
        self.height = new_h;
        true
    }

    fn write(&self, queue: &wgpu::Queue, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.tex,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * self.bpp),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
    }
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
@group(0) @binding(1) var color_atlas: texture_2d<f32>;
@group(0) @binding(2) var mask_atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_smp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
};

@vertex
fn vs(
    @builtin(vertex_index) vi: u32,
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_px: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    let p = pos + c * size;
    let ndc = vec2<f32>(
        p.x / screen.size.x * 2.0 - 1.0,
        1.0 - p.y / screen.size.y * 2.0,
    );
    // Normalize the atlas PIXEL coords against the LIVE texture dims so a grown
    // atlas (taller texture, same pixel coords) keeps every cached instance valid.
    var dim = vec2<u32>(1u, 1u);
    if kind == 0u {
        dim = textureDimensions(color_atlas);
    } else {
        dim = textureDimensions(mask_atlas);
    }
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = (uv_px + c * size) / vec2<f32>(dim);
    out.color = color;
    out.kind = kind;
    return out;
}

// sRGB → linear, matching `quad.rs` / glyphon's `srgb_to_linear`. The render
// target is an sRGB surface that re-encodes the fragment output, so mask glyph
// colors must be decoded here to land on their intended theme color (identical
// to glyphon's Accurate ColorMode, which decodes in its vertex stage).
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(lo, hi, c > vec3<f32>(0.04045));
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    if in.kind == 0u {
        // Color glyph: straight sample of the sRGB color atlas (the hardware
        // decodes rgb to linear); returned as-is for straight-alpha blending,
        // exactly as glyphon's color branch does.
        return textureSampleLevel(color_atlas, atlas_smp, in.uv, 0.0);
    }
    // Mask glyph: theme-colored coverage. `vec4(linear_fg, fg.a * coverage)`,
    // straight alpha — identical to glyphon's mask branch.
    let cov = textureSampleLevel(mask_atlas, atlas_smp, in.uv, 0.0).r;
    let lin = srgb_to_linear(in.color.rgb);
    return vec4<f32>(lin, in.color.a * cov);
}
"#;

/// The instanced glyph renderer: two atlases (color + mask), one pipeline, a
/// per-`CacheKey` slot cache, and a growable instance buffer.
pub struct GlyphPipeline {
    pipeline: wgpu::RenderPipeline,
    atlas_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    screen_buf: wgpu::Buffer,
    color: Atlas,
    mask: Atlas,
    bind_group: wgpu::BindGroup,
    bg_dirty: bool,
    slots: HashMap<CacheKey, Option<GlyphSlot>>,
    max_dim: u32,
    instances: wgpu::Buffer,
    capacity: usize,
    count: u32,
    /// Surface size [w, h] in physical px from the last `upload`, used to clamp
    /// per-pane scissor rects in `draw`.
    screen: [f32; 2],
}

impl GlyphPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("kettle-glyph"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        // glyphon's color atlas is sRGB only when the target is sRGB (Accurate
        // ColorMode); mirror that so a straight sample decodes correctly.
        let color_format = if format.is_srgb() {
            wgpu::TextureFormat::Rgba8UnormSrgb
        } else {
            wgpu::TextureFormat::Rgba8Unorm
        };
        let color = Atlas::new(device, "kettle-glyph-color-atlas", color_format, 4);
        let mask = Atlas::new(
            device,
            "kettle-glyph-mask-atlas",
            wgpu::TextureFormat::R8Unorm,
            1,
        );

        let screen_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-glyph-screen"),
            size: std::mem::size_of::<Screen>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Nearest sampling: every glyph is snapped to an integer physical pixel
        // and drawn at its exact bitmap size, so 1 screen texel maps to 1 atlas
        // texel — crisp text, no blur, and the 1px gutter is never sampled.
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("kettle-glyph-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let atlas_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("kettle-glyph-bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let bind_group = Self::make_bg(
            device,
            &atlas_bgl,
            &screen_buf,
            &color.view,
            &mask.view,
            &sampler,
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("kettle-glyph-layout"),
            bind_group_layouts: &[Some(&atlas_bgl)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("kettle-glyph-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<GlyphInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x4, 4 => Uint32
                    ],
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
        let capacity = 8192;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-glyph-instances"),
            size: (capacity * std::mem::size_of::<GlyphInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            atlas_bgl,
            sampler,
            screen_buf,
            color,
            mask,
            bind_group,
            bg_dirty: false,
            slots: HashMap::new(),
            max_dim: device.limits().max_texture_dimension_2d,
            instances,
            capacity,
            count: 0,
            screen: [0.0; 2],
        }
    }

    fn make_bg(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        screen_buf: &wgpu::Buffer,
        color_view: &wgpu::TextureView,
        mask_view: &wgpu::TextureView,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("kettle-glyph-bg"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: screen_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(color_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    /// Resolve a glyph's atlas slot, rasterizing + uploading on a cache miss.
    /// `rasterize` is only called on a miss (so the caller's `SwashCache` borrow
    /// is paid once per unique glyph). Returns `None` for empty/whitespace
    /// glyphs (nothing to draw) — cached so the miss isn't retried every frame.
    pub fn ensure_glyph<'r>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        key: CacheKey,
        rasterize: impl FnOnce() -> Option<RasterGlyph<'r>>,
    ) -> Option<GlyphSlot> {
        if let Some(slot) = self.slots.get(&key) {
            return *slot;
        }
        let slot = self.rasterize_into_atlas(device, queue, rasterize);
        self.slots.insert(key, slot);
        slot
    }

    fn rasterize_into_atlas<'r>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rasterize: impl FnOnce() -> Option<RasterGlyph<'r>>,
    ) -> Option<GlyphSlot> {
        let g = rasterize()?;
        // Empty glyph (space, zero-width): nothing to draw, but cache the miss.
        if g.width == 0 || g.height == 0 || g.data.is_empty() {
            return None;
        }
        let (atlas, kind, label) = if g.color {
            (&mut self.color, 0u32, "kettle-glyph-color-atlas")
        } else {
            (&mut self.mask, 1u32, "kettle-glyph-mask-atlas")
        };
        // The atlas only grows in HEIGHT; a glyph wider than the (fixed) atlas
        // width can never be packed, and writing it would copy past the texture's
        // right edge (a wgpu validation error → with panic=abort, a process
        // abort). Skip it instead. Unreachable in practice (needs a single glyph
        // > ~1024 physical px, i.e. an absurd font size), but a GPU-input guard.
        if g.width + GUTTER > atlas.width {
            log::warn!(
                "kettle glyph {}px wider than the {}px atlas — skipping",
                g.width,
                atlas.width
            );
            return None;
        }
        let (x, y) = loop {
            if let Some(p) = atlas.alloc(g.width, g.height) {
                break p;
            }
            if !atlas.grow(device, queue, label, self.max_dim) {
                // Atlas is at the device's texture limit (tens of thousands of
                // glyphs — far past any real session). Skip this one glyph
                // rather than abort the frame.
                log::warn!(
                    "kettle glyph atlas full ({}px) — skipping a glyph",
                    atlas.height
                );
                return None;
            }
            self.bg_dirty = true;
        };
        atlas.write(queue, x, y, g.width, g.height, g.data);
        Some(GlyphSlot {
            kind,
            atlas_x: x as f32,
            atlas_y: y as f32,
            w: g.width as f32,
            h: g.height as f32,
            left: g.left,
            top: g.top,
        })
    }

    /// Upload this frame's instances + the screen size, rebuilding the atlas
    /// bind group first if a glyph upload grew a texture.
    pub fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        screen: [f32; 2],
        data: &[GlyphInstance],
    ) {
        if self.bg_dirty {
            self.bind_group = Self::make_bg(
                device,
                &self.atlas_bgl,
                &self.screen_buf,
                &self.color.view,
                &self.mask.view,
                &self.sampler,
            );
            self.bg_dirty = false;
        }
        self.screen = screen;
        queue.write_buffer(
            &self.screen_buf,
            0,
            bytemuck::bytes_of(&Screen {
                size: screen,
                _pad: [0.0; 2],
            }),
        );
        if data.len() > self.capacity {
            self.capacity = data.len().next_power_of_two();
            self.instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kettle-glyph-instances"),
                size: (self.capacity * std::mem::size_of::<GlyphInstance>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !data.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(data));
        }
        self.count = data.len() as u32;
    }

    /// Draw the uploaded instances, each pane's contiguous range clipped to its
    /// pane rect via a scissor (so glyphs can't bleed across panes / into chrome).
    /// An empty `clips` draws everything unclipped (used when there's a single
    /// full-surface region).
    pub fn draw(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        clips: &[GlyphClip],
        target_size: [u32; 2],
    ) {
        if self.count == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.instances.slice(..));
        if clips.is_empty() {
            pass.draw(0..4, 0..self.count);
            return;
        }
        let (sw, sh) = (target_size[0].max(1) as f32, target_size[1].max(1) as f32);
        for c in clips {
            if c.count == 0 {
                continue;
            }
            // Clamp the pane rect to the surface — `set_scissor_rect` requires
            // x+w ≤ width and y+h ≤ height, in u32 physical pixels.
            let x0 = c.rect[0].clamp(0.0, sw);
            let y0 = c.rect[1].clamp(0.0, sh);
            let x1 = (c.rect[0] + c.rect[2]).clamp(0.0, sw);
            let y1 = (c.rect[1] + c.rect[3]).clamp(0.0, sh);
            let (swd, shd) = ((x1 - x0) as u32, (y1 - y0) as u32);
            if swd == 0 || shd == 0 {
                continue;
            }
            pass.set_scissor_rect(x0 as u32, y0 as u32, swd, shd);
            pass.draw(0..4, c.start..c.start + c.count);
        }
        // Restore the full-surface scissor so the following passes (chrome text,
        // menus, the cursor glyph) aren't clipped by the last pane's rect.
        pass.set_scissor_rect(0, 0, sw as u32, sh as u32);
    }

    /// Drop every cached glyph slot. Called when the font family / size / scale
    /// changes (the cache keys are now stale). The atlas textures keep their
    /// allocation; the packer resets so freed space is reused.
    pub fn clear(&mut self) {
        self.slots.clear();
        // Drop the live draw count too: a `draw` after a bare `clear` (no
        // intervening `upload`) would otherwise render stale instances whose UVs
        // point at atlas pixels the packer is about to overwrite from (0,0).
        self.count = 0;
        self.color.cursor_x = 0;
        self.color.shelf_y = 0;
        self.color.shelf_h = 0;
        self.mask.cursor_x = 0;
        self.mask.shelf_y = 0;
        self.mask.shelf_h = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instance layout the vertex attributes index by offset. If a field is
    /// reordered/resized without updating `vertex_attr_array!`, the GPU reads
    /// garbage — pin the size + the field offsets the shader depends on.
    #[test]
    fn glyph_instance_layout_is_stable() {
        assert_eq!(std::mem::size_of::<GlyphInstance>(), 56);
        // Field order matches `0 => Float32x2 (pos @0), 1 => size @8,
        // 2 => uv @16, 3 => color @24, 4 => kind @40`.
        let i = GlyphInstance {
            pos: [1.0, 2.0],
            size: [3.0, 4.0],
            uv: [5.0, 6.0],
            color: [0.1, 0.2, 0.3, 1.0],
            kind: 1,
            _pad: [0; 3],
        };
        let bytes = bytemuck::bytes_of(&i);
        assert_eq!(&bytes[0..8], bytemuck::bytes_of(&i.pos));
        assert_eq!(&bytes[8..16], bytemuck::bytes_of(&i.size));
        assert_eq!(&bytes[16..24], bytemuck::bytes_of(&i.uv));
        assert_eq!(&bytes[24..40], bytemuck::bytes_of(&i.color));
        assert_eq!(&bytes[40..44], bytemuck::bytes_of(&i.kind));
    }
}

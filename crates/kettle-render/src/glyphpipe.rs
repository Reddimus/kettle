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
use kettle_core::{GraphicsBudget, GraphicsReservation};

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

/// A cached slot entry plus the frame `epoch` it was last touched at, so the
/// cache can find and evict the coldest entries once it's full instead of
/// refusing every new glyph from then on (see `GlyphPipeline::ensure_glyph`).
struct CachedSlot {
    slot: Option<GlyphSlot>,
    last_used: u64,
}

enum CacheOutcome {
    Slot(GlyphSlot),
    Empty,
    AtlasFull,
}

/// Pick the `n` keys with the smallest `last_used` value out of `ages` — the
/// least-recently-touched entries. Generic over the key type so the eviction
/// *policy* is unit-testable with plain keys, without needing a real
/// `CacheKey` (which can only be constructed from a loaded font face).
///
/// This runs on the render thread, inside the frame that overflowed the cache,
/// over `MAX_GLYPH_SLOTS` (131,072) entries, with `n` = 16,384 — the single
/// caller evicts down to 7/8 of capacity. Fully sorting them to keep a prefix did `O(len log len)`
/// comparisons for an answer that needs `O(len)`: partition around the nth
/// element and take what falls below it. `select_nth_unstable_by_key` is
/// average linear and leaves the prefix unordered, which is all a victim list
/// needs — nothing downstream depends on the order they are dropped in.
fn lru_victims<K: Copy>(ages: impl Iterator<Item = (K, u64)>, n: usize) -> Vec<K> {
    if n == 0 {
        return Vec::new();
    }
    let mut by_age: Vec<(u64, K)> = ages.map(|(k, age)| (age, k)).collect();
    if n >= by_age.len() {
        return by_age.into_iter().map(|(_, k)| k).collect();
    }
    // Everything at or before index `n - 1` is <= everything after it.
    by_age.select_nth_unstable_by_key(n - 1, |&(age, _)| age);
    by_age.truncate(n);
    by_age.into_iter().map(|(_, k)| k).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FreeRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

struct AtlasAllocator {
    width: u32,
    height: u32,
    cursor_x: u32,
    shelf_y: u32,
    shelf_h: u32,
    free: Vec<FreeRect>,
}

impl AtlasAllocator {
    fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            cursor_x: 0,
            shelf_y: 0,
            shelf_h: 0,
            free: Vec::new(),
        }
    }

    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (gw, gh) = (w.checked_add(GUTTER)?, h.checked_add(GUTTER)?);
        if let Some((index, _)) = self
            .free
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.width >= gw && rect.height >= gh)
            .min_by_key(|(_, rect)| u64::from(rect.width) * u64::from(rect.height))
        {
            let rect = self.free.swap_remove(index);
            if rect.width > gw {
                self.free.push(FreeRect {
                    x: rect.x + gw,
                    y: rect.y,
                    width: rect.width - gw,
                    height: gh,
                });
            }
            if rect.height > gh {
                self.free.push(FreeRect {
                    x: rect.x,
                    y: rect.y + gh,
                    width: rect.width,
                    height: rect.height - gh,
                });
            }
            return Some((rect.x, rect.y));
        }
        if self.cursor_x.checked_add(gw)? > self.width {
            self.cursor_x = 0;
            self.shelf_y = self.shelf_y.checked_add(self.shelf_h)?;
            self.shelf_h = 0;
        }
        if self.shelf_y.checked_add(gh)? > self.height {
            return None;
        }
        let (x, y) = (self.cursor_x, self.shelf_y);
        self.cursor_x = self.cursor_x.checked_add(gw)?;
        self.shelf_h = self.shelf_h.max(gh);
        Some((x, y))
    }

    #[cfg(test)]
    fn free(&mut self, x: u32, y: u32, w: u32, h: u32) {
        self.free_unmerged(x, y, w, h);
        self.coalesce_free();
    }

    fn free_unmerged(&mut self, x: u32, y: u32, w: u32, h: u32) {
        let Some(width) = w.checked_add(GUTTER) else {
            return;
        };
        let Some(height) = h.checked_add(GUTTER) else {
            return;
        };
        self.free.push(FreeRect {
            x,
            y,
            width,
            height,
        });
    }

    fn coalesce_free(&mut self) {
        loop {
            let before = self.free.len();
            self.free
                .sort_unstable_by_key(|rect| (rect.y, rect.height, rect.x, rect.width));
            let mut horizontal: Vec<FreeRect> = Vec::with_capacity(self.free.len());
            for rect in self.free.drain(..) {
                if let Some(last) = horizontal.last_mut()
                    && let Some(merged) = merge_free_rects(*last, rect)
                {
                    *last = merged;
                    continue;
                }
                horizontal.push(rect);
            }
            horizontal.sort_unstable_by_key(|rect| (rect.x, rect.width, rect.y, rect.height));
            let mut vertical: Vec<FreeRect> = Vec::with_capacity(horizontal.len());
            for rect in horizontal {
                if let Some(last) = vertical.last_mut()
                    && let Some(merged) = merge_free_rects(*last, rect)
                {
                    *last = merged;
                    continue;
                }
                vertical.push(rect);
            }
            self.free = vertical;
            if self.free.len() == before {
                break;
            }
        }
    }

    fn grow_height(&mut self, height: u32) {
        self.height = height;
    }
}

fn merge_free_rects(left: FreeRect, right: FreeRect) -> Option<FreeRect> {
    if left.y == right.y && left.height == right.height {
        if left.x.checked_add(left.width) == Some(right.x) {
            return Some(FreeRect {
                x: left.x,
                y: left.y,
                width: left.width.checked_add(right.width)?,
                height: left.height,
            });
        }
        if right.x.checked_add(right.width) == Some(left.x) {
            return Some(FreeRect {
                x: right.x,
                y: left.y,
                width: right.width.checked_add(left.width)?,
                height: left.height,
            });
        }
    }
    if left.x == right.x && left.width == right.width {
        if left.y.checked_add(left.height) == Some(right.y) {
            return Some(FreeRect {
                x: left.x,
                y: left.y,
                width: left.width,
                height: left.height.checked_add(right.height)?,
            });
        }
        if right.y.checked_add(right.height) == Some(left.y) {
            return Some(FreeRect {
                x: left.x,
                y: right.y,
                width: left.width,
                height: right.height.checked_add(left.height)?,
            });
        }
    }
    None
}

/// A single-format atlas texture with a shelf packer plus reclaimed rectangles.
/// Texture growth preserves pixel coordinates, and evicted slots return their
/// rectangles to the allocator so capacity exhaustion can recover in place.
struct Atlas {
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    format: wgpu::TextureFormat,
    bpp: u32,
    width: u32,
    height: u32,
    allocator: AtlasAllocator,
    budget: GraphicsBudget,
    _gpu: GraphicsReservation,
}

/// 1px gutter so neighboring glyphs can never sample into each other.
const GUTTER: u32 = 1;

impl Atlas {
    fn new(
        device: &wgpu::Device,
        label: &str,
        format: wgpu::TextureFormat,
        bpp: u32,
        budget: GraphicsBudget,
    ) -> Option<Self> {
        let (width, height) = (1024u32, 512u32);
        if width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            return None;
        }
        let bytes = texture_bytes(width, height, bpp)?;
        let gpu = budget.reserve_gpu(bytes)?;
        let tex = Self::make_tex(device, label, format, width, height);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        Some(Self {
            tex,
            view,
            format,
            bpp,
            width,
            height,
            allocator: AtlasAllocator::new(width, height),
            budget,
            _gpu: gpu,
        })
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
        self.allocator.alloc(w, h)
    }

    fn free_unmerged(&mut self, slot: GlyphSlot) {
        self.allocator.free_unmerged(
            slot.atlas_x as u32,
            slot.atlas_y as u32,
            slot.w as u32,
            slot.h as u32,
        );
    }

    fn coalesce_free(&mut self) {
        self.allocator.coalesce_free();
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
        let new_h = self.height.checked_mul(2).unwrap_or(max_dim).min(max_dim);
        if new_h <= self.height {
            return false;
        }
        let Some(bytes) = texture_bytes(self.width, new_h, self.bpp) else {
            return false;
        };
        let Some(gpu) = self.budget.reserve_gpu(bytes) else {
            return false;
        };
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
        self.allocator.grow_height(new_h);
        self._gpu = gpu;
        true
    }

    fn write(&self, queue: &wgpu::Queue, x: u32, y: u32, w: u32, h: u32, data: &[u8]) {
        let Some(expected) = texture_bytes(w, h, self.bpp) else {
            return;
        };
        if data.len() != expected {
            log::warn!("skipping malformed glyph bitmap: byte length mismatch");
            return;
        }
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

fn texture_bytes(width: u32, height: u32, bytes_per_pixel: u32) -> Option<usize> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(u64::from(bytes_per_pixel))?
        .try_into()
        .ok()
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
    slots: HashMap<CacheKey, CachedSlot>,
    /// Frame counter bumped once per `upload`. Stamped onto a slot on every
    /// `ensure_glyph` touch (hit or miss) so `evict_lru` can tell which slots
    /// are cold once the cache needs to make room.
    epoch: u64,
    max_dim: u32,
    instances: wgpu::Buffer,
    capacity: usize,
    instance_gpu: GraphicsReservation,
    budget: GraphicsBudget,
    count: u32,
    /// Surface size [w, h] in physical px from the last `upload`, used to clamp
    /// per-pane scissor rects in `draw`.
    screen: [f32; 2],
}

impl GlyphPipeline {
    #[cfg(test)]
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        Self::new_with_budget(device, format, GraphicsBudget::default())
            .expect("fixed glyph pipeline allocations fit the default GPU budget")
    }

    pub fn new_with_budget(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        budget: GraphicsBudget,
    ) -> Option<Self> {
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
        let color = Atlas::new(
            device,
            "kettle-glyph-color-atlas",
            color_format,
            4,
            budget.clone(),
        )?;
        let mask = Atlas::new(
            device,
            "kettle-glyph-mask-atlas",
            wgpu::TextureFormat::R8Unorm,
            1,
            budget.clone(),
        )?;

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
        let capacity: usize = 8192;
        let instance_bytes = capacity.checked_mul(std::mem::size_of::<GlyphInstance>())?;
        let instance_gpu = budget.reserve_gpu(instance_bytes)?;
        let instances = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kettle-glyph-instances"),
            size: (capacity * std::mem::size_of::<GlyphInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(Self {
            pipeline,
            atlas_bgl,
            sampler,
            screen_buf,
            color,
            mask,
            bind_group,
            bg_dirty: false,
            slots: HashMap::new(),
            epoch: 0,
            max_dim: device.limits().max_texture_dimension_2d,
            instances,
            capacity,
            instance_gpu,
            budget,
            count: 0,
            screen: [0.0; 2],
        })
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
        if let Some(cached) = self.slots.get_mut(&key) {
            cached.last_used = self.epoch;
            return cached.slot;
        }
        // Bound an unbounded stream of cached glyph and whitespace entries at
        // stream of misses would still grow this map forever. Bound it at a
        // deterministic ceiling, but instead of refusing every new glyph from
        // then on, evict the coldest (least-recently-touched) slots first to
        // make room. Every instance-buffer rebuild re-emits all visible panes,
        // so a glyph still being drawn gets `last_used` refreshed before cold
        // slots are reclaimed — a long session that floods through many distinct glyphs
        // (unicode/emoji streaming, repeated zoom-driven subpixel bins)
        // self-heals instead of permanently losing glyph rendering once the
        // cap is first hit.
        const MAX_GLYPH_SLOTS: usize = 131_072;
        if self.slots.len() >= MAX_GLYPH_SLOTS {
            // Evict down to 7/8 capacity rather than one slot at a time, so
            // the age scan amortizes over the next ~16K misses instead of
            // running on every single insert once the cache is steady-state at
            // the cap. (The scan is linear, not `O(n log n)` — see
            // `lru_victims`. This comment said otherwise for as long as the
            // sort it described was there.)
            let target = MAX_GLYPH_SLOTS - MAX_GLYPH_SLOTS / 8;
            self.evict_lru(self.slots.len().saturating_sub(target));
        }
        match self.rasterize_into_atlas(device, queue, rasterize) {
            CacheOutcome::Slot(slot) => {
                self.slots.insert(
                    key,
                    CachedSlot {
                        slot: Some(slot),
                        last_used: self.epoch,
                    },
                );
                Some(slot)
            }
            CacheOutcome::Empty => {
                self.slots.insert(
                    key,
                    CachedSlot {
                        slot: None,
                        last_used: self.epoch,
                    },
                );
                None
            }
            CacheOutcome::AtlasFull => None,
        }
    }

    /// Evict the `n` coldest slots (smallest `last_used` epoch), returning
    /// their atlas rectangles to the appropriate free list.
    fn evict_lru(&mut self, n: usize) {
        let victims: Vec<CacheKey> = lru_victims(
            self.slots
                .iter()
                .filter(|(_, cached)| cached.last_used < self.epoch)
                .map(|(&k, cached)| (k, cached.last_used)),
            n,
        );
        for key in victims {
            if let Some(cached) = self.slots.remove(&key) {
                self.free_cached_slot(cached);
            }
        }
        self.coalesce_freed_slots();
    }

    fn evict_cold_kind(&mut self, kind: u32, n: usize) -> usize {
        let victims = lru_victims(
            self.slots.iter().filter_map(|(&key, cached)| {
                cached
                    .slot
                    .filter(|slot| slot.kind == kind && cached.last_used < self.epoch)
                    .map(|_| (key, cached.last_used))
            }),
            n,
        );
        let count = victims.len();
        for key in victims {
            if let Some(cached) = self.slots.remove(&key) {
                self.free_cached_slot(cached);
            }
        }
        self.coalesce_freed_slots();
        count
    }

    fn free_cached_slot(&mut self, cached: CachedSlot) {
        if let Some(slot) = cached.slot {
            if slot.kind == 0 {
                self.color.free_unmerged(slot);
            } else {
                self.mask.free_unmerged(slot);
            }
        }
    }

    fn coalesce_freed_slots(&mut self) {
        self.color.coalesce_free();
        self.mask.coalesce_free();
    }

    fn rasterize_into_atlas<'r>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        rasterize: impl FnOnce() -> Option<RasterGlyph<'r>>,
    ) -> CacheOutcome {
        let Some(g) = rasterize() else {
            return CacheOutcome::Empty;
        };
        // Empty glyph (space, zero-width): nothing to draw, but cache the miss.
        if g.width == 0 || g.height == 0 || g.data.is_empty() {
            return CacheOutcome::Empty;
        }
        let (kind, label, bpp, atlas_width) = if g.color {
            (
                0u32,
                "kettle-glyph-color-atlas",
                self.color.bpp,
                self.color.width,
            )
        } else {
            (
                1u32,
                "kettle-glyph-mask-atlas",
                self.mask.bpp,
                self.mask.width,
            )
        };
        if texture_bytes(g.width, g.height, bpp) != Some(g.data.len()) {
            log::warn!("skipping malformed glyph bitmap: byte length mismatch");
            return CacheOutcome::Empty;
        }
        // The atlas only grows in HEIGHT; a glyph wider than the (fixed) atlas
        // width can never be packed, and writing it would copy past the texture's
        // right edge (a wgpu validation error → with panic=abort, a process
        // abort). Skip it instead. Unreachable in practice (needs a single glyph
        // > ~1024 physical px, i.e. an absurd font size), but a GPU-input guard.
        if g.width.checked_add(GUTTER).is_none_or(|w| w > atlas_width) {
            log::warn!(
                "kettle glyph {}px wider than the {}px atlas — skipping",
                g.width,
                atlas_width
            );
            return CacheOutcome::Empty;
        }
        let (x, y) = loop {
            let allocation = if kind == 0 {
                self.color.alloc(g.width, g.height)
            } else {
                self.mask.alloc(g.width, g.height)
            };
            if let Some(p) = allocation {
                break p;
            }
            let grew = if kind == 0 {
                self.color.grow(device, queue, label, self.max_dim)
            } else {
                self.mask.grow(device, queue, label, self.max_dim)
            };
            if grew {
                self.bg_dirty = true;
                continue;
            }
            let batch = (self.slots.len() / 8).max(1);
            if self.evict_cold_kind(kind, batch) == 0 {
                log::warn!(
                    "kettle glyph atlas full ({}px) — skipping a glyph",
                    if kind == 0 {
                        self.color.height
                    } else {
                        self.mask.height
                    }
                );
                return CacheOutcome::AtlasFull;
            }
        };
        if kind == 0 {
            self.color.write(queue, x, y, g.width, g.height, g.data);
        } else {
            self.mask.write(queue, x, y, g.width, g.height, g.data);
        }
        CacheOutcome::Slot(GlyphSlot {
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
        // Bump the frame counter once per frame, matching every `ensure_glyph`
        // touch this frame having already stamped `last_used` with the *prior*
        // value — the next frame's misses (if any) evict against this new
        // value, so a slot untouched since is unambiguously older than one
        // touched this frame.
        self.epoch = self.epoch.saturating_add(1);
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
            let Some(capacity) = data.len().checked_next_power_of_two() else {
                self.count = 0;
                return;
            };
            let Some(bytes) = capacity.checked_mul(std::mem::size_of::<GlyphInstance>()) else {
                self.count = 0;
                return;
            };
            let Some(gpu) = self.budget.reserve_gpu(bytes) else {
                log::warn!("glyph instance buffer growth exceeds GPU graphics budget");
                self.count = 0;
                return;
            };
            let instances = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kettle-glyph-instances"),
                size: bytes as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instances = instances;
            self.capacity = capacity;
            self.instance_gpu = gpu;
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
        self.color.allocator = AtlasAllocator::new(self.color.width, self.color.height);
        self.mask.allocator = AtlasAllocator::new(self.mask.width, self.mask.height);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production source of this file, excluding test-only items.
    fn production_source() -> String {
        let production = kettle_test_support::production_source(include_str!("glyphpipe.rs"));
        assert!(
            !production.contains("fn production_source()"),
            "the production slice retained its own helper"
        );
        assert!(
            !production.contains("#[test]"),
            "the production slice retained a test function"
        );
        assert!(
            !production.contains("#[cfg(test)]"),
            "the production slice retained a test-only item"
        );
        production
    }

    #[test]
    fn texture_byte_math_is_checked_at_the_per_texture_boundary() {
        let limit = kettle_core::GraphicsLimits::default().image_bytes;
        assert_eq!(texture_bytes(8192, 2048, 4), Some(limit));
        assert_eq!(texture_bytes(8192, 2049, 4), Some(limit + 8192 * 4));
        assert_eq!(texture_bytes(u32::MAX, u32::MAX, 4), None);
    }

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

    /// `lru_victims` picks the smallest-epoch entries — the ones `evict_lru`
    /// should reclaim first when the glyph slot cache is full. Uses plain
    /// `u32` keys (a real `CacheKey` can only be constructed from a loaded
    /// font face, which needs a real font — see the drift guard below for the
    /// wiring into `GlyphPipeline` itself).
    #[test]
    fn lru_victims_picks_the_coldest_keys() {
        let ages = vec![(10u32, 3u64), (11, 1), (12, 4), (13, 1), (14, 5)];
        let mut victims = lru_victims(ages.into_iter(), 2);
        victims.sort_unstable();
        // Keys 11 and 13 share the minimum epoch (1); everything else is
        // strictly newer, so they're the only valid pick for n = 2.
        assert_eq!(victims, vec![11, 13]);
    }

    #[test]
    fn lru_victims_zero_is_a_no_op() {
        let ages = vec![(1u32, 0u64), (2, 1)];
        assert!(lru_victims(ages.into_iter(), 0).is_empty());
    }

    #[test]
    fn lru_victims_caps_at_the_available_entry_count() {
        let ages = vec![(1u32, 5u64), (2, 2)];
        // Asking for more victims than exist must not panic or duplicate.
        let mut victims = lru_victims(ages.into_iter(), 10);
        victims.sort_unstable();
        assert_eq!(victims, vec![1, 2]);
        // And exactly as many as exist — the boundary between partitioning and
        // taking everything.
        let ages = vec![(1u32, 5u64), (2, 2)];
        let mut victims = lru_victims(ages.into_iter(), 2);
        victims.sort_unstable();
        assert_eq!(victims, vec![1, 2]);
    }

    /// The victims must be the `n` coldest for every `n`, at a size where the
    /// difference between partitioning and sorting is real.
    ///
    /// Eviction runs on the render thread over as many as `MAX_GLYPH_SLOTS`
    /// entries, so it partitions rather than sorts. A partition leaves the
    /// prefix unordered, which is fine — but only if it is still the right
    /// SET. This checks that against the definition directly.
    #[test]
    fn lru_victims_are_the_coldest_set_whatever_the_order_within_it() {
        // A deterministic scatter of ages with many ties, since real epochs
        // repeat heavily — every glyph touched in one frame shares an epoch.
        let entries: Vec<(u32, u64)> = (0..4096u32)
            .map(|k| (k, u64::from(k.wrapping_mul(2_654_435_761) >> 20)))
            .collect();

        for n in [1usize, 2, 17, 512, 2048, 4095, 4096] {
            let mut victims = lru_victims(entries.iter().copied(), n);
            assert_eq!(victims.len(), n, "n = {n}: wrong number of victims");
            victims.sort_unstable();
            victims.dedup();
            assert_eq!(victims.len(), n, "n = {n}: victims must be distinct keys");

            // Every victim must be at least as cold as every survivor.
            let picked: std::collections::HashSet<u32> = victims.into_iter().collect();
            let coldest_survivor = entries
                .iter()
                .filter(|(k, _)| !picked.contains(k))
                .map(|&(_, age)| age)
                .min();
            let warmest_victim = entries
                .iter()
                .filter(|(k, _)| picked.contains(k))
                .map(|&(_, age)| age)
                .max();
            if let (Some(survivor), Some(victim)) = (coldest_survivor, warmest_victim) {
                assert!(
                    victim <= survivor,
                    "n = {n}: evicted an entry of age {victim} while keeping one of \
                     age {survivor}"
                );
            }
        }
    }

    #[test]
    fn tiny_atlas_reuses_evicted_pixels_after_exhaustion() {
        let mut atlas = AtlasAllocator::new(8, 4);
        let first = atlas.alloc(3, 3).expect("first glyph");
        let second = atlas.alloc(3, 3).expect("second glyph");
        assert_eq!(first, (0, 0));
        assert_eq!(second, (4, 0));
        assert_eq!(atlas.alloc(3, 3), None, "tiny atlas must be full");

        atlas.free(first.0, first.1, 3, 3);
        let newcomer = atlas.alloc(3, 3).expect("cold slot must be reusable");
        assert_eq!(newcomer, first);

        atlas.free(second.0, second.1, 3, 3);
        let revisited = atlas
            .alloc(3, 3)
            .expect("an evicted glyph must render again without clearing the atlas");
        assert_eq!(revisited, second);
    }

    #[test]
    fn atlas_coalesces_every_full_edge_sibling_without_merging_partial_edges() {
        // Horizontal siblings: two 4x4 allocated rectangles (3x3 glyph plus
        // gutter) must become one 8x4 rectangle for a wider replacement.
        let mut horizontal = AtlasAllocator::new(8, 4);
        let left = horizontal.alloc(3, 3).expect("left slot");
        let right = horizontal.alloc(3, 3).expect("right slot");
        assert_eq!(horizontal.alloc(7, 3), None);
        horizontal.free(left.0, left.1, 3, 3);
        horizontal.free(right.0, right.1, 3, 3);
        assert_eq!(
            horizontal.alloc(7, 3),
            Some((0, 0)),
            "horizontal cold siblings must satisfy one wider glyph"
        );

        // Vertical siblings are the same geometry rotated: max-height atlases
        // must recover a taller slot too.
        let mut vertical = AtlasAllocator::new(4, 8);
        let top = vertical.alloc(3, 3).expect("top slot");
        let bottom = vertical.alloc(3, 3).expect("bottom slot");
        vertical.free(top.0, top.1, 3, 3);
        vertical.free(bottom.0, bottom.1, 3, 3);
        assert_eq!(
            vertical.alloc(3, 7),
            Some((0, 0)),
            "vertical cold siblings must satisfy one taller glyph"
        );

        // Eviction order cannot matter. Free the outside rectangles first so
        // the middle rectangle has to merge transitively with both.
        let mut transitive = AtlasAllocator::new(12, 4);
        let first = transitive.alloc(3, 3).expect("first slot");
        let middle = transitive.alloc(3, 3).expect("middle slot");
        let last = transitive.alloc(3, 3).expect("last slot");
        transitive.free(first.0, first.1, 3, 3);
        transitive.free(last.0, last.1, 3, 3);
        transitive.free(middle.0, middle.1, 3, 3);
        assert_eq!(
            transitive.alloc(11, 3),
            Some((0, 0)),
            "coalescing must reach a fixed point across three siblings"
        );

        // Touching only part of an edge is not a rectangle union. Merging
        // these would hand the allocator pixels it never owned.
        let mut partial = AtlasAllocator::new(8, 4);
        let tall = partial.alloc(3, 3).expect("tall slot");
        let short = partial.alloc(3, 1).expect("short slot");
        partial.free(tall.0, tall.1, 3, 3);
        partial.free(short.0, short.1, 3, 1);
        assert_eq!(
            partial.alloc(7, 3),
            None,
            "partial-edge neighbors must remain distinct"
        );
    }

    #[test]
    fn atlas_batch_free_coalesces_once_after_collecting_slots() {
        let mut atlas = AtlasAllocator::new(12, 4);
        let slots = [
            atlas.alloc(3, 3).unwrap(),
            atlas.alloc(3, 3).unwrap(),
            atlas.alloc(3, 3).unwrap(),
        ];
        for (x, y) in slots {
            atlas.free_unmerged(x, y, 3, 3);
        }
        assert_eq!(atlas.alloc(11, 3), None);
        atlas.coalesce_free();
        assert_eq!(atlas.alloc(11, 3), Some((0, 0)));
    }

    #[test]
    fn atlas_capacity_failures_are_not_cached_as_whitespace() {
        let src = production_source();
        assert!(
            src.contains("CacheOutcome::AtlasFull => None"),
            "capacity failure must return without inserting a permanent None slot"
        );
    }

    /// Drift guard (eviction-not-refusal fix). `ensure_glyph` must evict cold
    /// slots to make room once `MAX_GLYPH_SLOTS` is hit, not silently return
    /// `None` for every new glyph from then on — the latter turns a long
    /// session's rare glyph combinations (unicode/emoji floods, zoom-driven
    /// subpixel bins) into permanent blank space with no recovery short of an
    /// explicit font-setting change. Exercising the real cache end-to-end
    /// needs a live GPU device to rasterize into (`rasterize_into_atlas`), so
    /// pin the wiring at the source level instead, mirroring the `imgpipe.rs`
    /// ABA guard.
    #[test]
    fn ensure_glyph_evicts_instead_of_refusing_at_the_cap() {
        let src = production_source();
        assert!(
            src.contains("self.evict_lru("),
            "ensure_glyph must evict cold slots when MAX_GLYPH_SLOTS is reached"
        );
        assert!(
            src.contains("self.evict_cold_kind(kind, batch)"),
            "atlas exhaustion must reclaim cold pixels, not only map entries"
        );
        assert!(
            src.contains("cached.last_used = self.epoch"),
            "a cache hit must refresh last_used so a glyph still on screen is never evicted"
        );
        assert!(
            src.contains("self.epoch = self.epoch.saturating_add(1)"),
            "upload must advance the frame epoch so `last_used` ages actually separate over time"
        );
    }
}

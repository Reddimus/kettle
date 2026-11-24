//! Software equivalents of Kettle's wgpu pipelines.
//!
//! Each submodule draws the same instance structs its GPU pipeline uploads,
//! with the same screen-size uniform, bound textures and scissor, into a
//! [`CpuTarget`]: a view of a [`CpuFramebuffer`] that holds what the GPU's
//! `*Srgb` colour attachment holds. Nothing here touches wgpu, so a CPU
//! backend can run with no GPU driver in the process.
//!
//! What the module reproduces, per the WGSL and pipeline state:
//!
//! - **Rasterization.** Instances expand to the same four-corner quads.
//!   Vertex positions go through the shader's pixel-to-NDC mapping and the
//!   viewport transform in `f32`, snap to 1/256 px as the rasterizer does,
//!   and cover a pixel when its centre lies inside under the top-left rule.
//! - **Attributes** are interpolated at pixel centres, linearly in the
//!   unsnapped corner positions.
//! - **Colour.** Blending is in linear light. Stored RGB is sRGB-encoded and
//!   alpha is linear, so every blend decodes the destination and encodes the
//!   result (see `srgb`). Premultiplied and straight-alpha blend states are
//!   kept per pipeline.
//! - **Derivatives.** The CPU evaluates fine 2x2-quad differences at helper
//!   pixel centres. Plain WGSL `fwidth` can use either fine or coarse
//!   differences. The rounded and outline parity fixtures pin lavapipe's
//!   fine evaluation.
//!
//! Only `*Srgb` scene targets are modelled. The plain-UNORM fallback that a
//! GPU surface can take when no sRGB format is offered has no CPU
//! counterpart: the CPU framebuffer is always sRGB-encoded.
//!
//! Pixels are packed `u32` `0xAARRGGBB`: on a little-endian host that is the
//! B, G, R, A byte order of Wayland's `wl_shm` ARGB8888 and XRGB8888.

// The cursor patch is a macOS Core Animation hand-off a CPU renderer never
// makes; its CPU combine pass is kept for its parity test.
#[cfg(test)]
pub(crate) mod cursor_patch;
pub(crate) mod glyphpipe;
pub(crate) mod imgpipe;
pub(crate) mod outline;
pub(crate) mod present;
pub(crate) mod quad;
pub(crate) mod rounded_surface;
pub(crate) mod srgb;
pub(crate) mod starfield;

#[cfg(test)]
mod bench_tests;
#[cfg(test)]
mod parity_tests;

#[cfg(test)]
use srgb::tables;
use srgb::unorm8;

/// The largest framebuffer or texture side, the same cap wgpu's default
/// limits put on a 2D texture.
pub(crate) const MAX_SIDE: u32 = 16_384;
/// The largest texture this module will hold or borrow, in bytes.
pub(crate) const MAX_TEXTURE_BYTES: usize = 512 * 1024 * 1024;

/// Pack sRGB-encoded RGB and linear alpha bytes.
#[inline(always)]
pub(crate) const fn pack(r: u8, g: u8, b: u8, a: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// The `[r, g, b, a]` bytes of a packed pixel.
#[inline(always)]
pub(crate) const fn unpack(p: u32) -> [u8; 4] {
    [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8]
}

/// A rectangle in whole pixels, `set_scissor_rect`'s `x, y, w, h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelRect {
    pub(crate) x: u32,
    pub(crate) y: u32,
    pub(crate) w: u32,
    pub(crate) h: u32,
}

impl PixelRect {
    pub(crate) const fn new(x: u32, y: u32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// The overlap of two rectangles, `None` when it is empty.
    pub(crate) fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self
            .x
            .saturating_add(self.w)
            .min(other.x.saturating_add(other.w));
        let y1 = self
            .y
            .saturating_add(self.h)
            .min(other.y.saturating_add(other.h));
        (x1 > x0 && y1 > y0).then(|| Self::new(x0, y0, x1 - x0, y1 - y0))
    }
}

/// A render pass viewport, in target pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Viewport {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) w: f32,
    pub(crate) h: f32,
}

/// What a GPU colour attachment of the scene format holds: sRGB-encoded RGB
/// and linear alpha, premultiplied in linear light, one packed `u32` per
/// pixel (see [`pack`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CpuFramebuffer {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

impl CpuFramebuffer {
    /// A transparent-black framebuffer, `None` when a side is zero or past
    /// [`MAX_SIDE`].
    pub(crate) fn new(width: u32, height: u32) -> Option<Self> {
        let len = Self::len_for(width, height)?;
        Some(Self {
            width,
            height,
            pixels: vec![0; len],
        })
    }

    #[cfg(test)]
    /// A framebuffer holding `rgba`, RGBA8 bytes as a GPU readback returns
    /// them for an `Rgba8UnormSrgb` target.
    pub(crate) fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> Option<Self> {
        let len = Self::len_for(width, height)?;
        if rgba.len() != len.checked_mul(4)? {
            return None;
        }
        let (texels, _) = rgba.as_chunks::<4>();
        Some(Self {
            width,
            height,
            pixels: texels
                .iter()
                .map(|&[r, g, b, a]| pack(r, g, b, a))
                .collect(),
        })
    }

    fn len_for(width: u32, height: u32) -> Option<usize> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return None;
        }
        usize::try_from(u64::from(width) * u64::from(height)).ok()
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// Rows top to bottom, `width` pixels each.
    pub(crate) fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub(crate) fn pixels_mut(&mut self) -> &mut [u32] {
        &mut self.pixels
    }

    /// Change the size, keeping the allocation where it fits. The contents
    /// become transparent black. `false` leaves it unchanged.
    pub(crate) fn resize(&mut self, width: u32, height: u32) -> bool {
        let Some(len) = Self::len_for(width, height) else {
            return false;
        };
        self.pixels.clear();
        self.pixels.resize(len, 0);
        self.width = width;
        self.height = height;
        true
    }

    #[cfg(test)]
    /// `LoadOp::Clear` with a `wgpu::Color`, whose components are linear and
    /// which the attachment encodes on store.
    pub(crate) fn clear(&mut self, color: [f64; 4]) {
        self.pixels.fill(clear_pixel(color));
    }

    #[cfg(test)]
    /// RGBA8 bytes, the order a GPU readback of an `Rgba8UnormSrgb` target
    /// returns.
    pub(crate) fn to_rgba8(&self) -> Vec<u8> {
        pixels_to_rgba8(&self.pixels)
    }

    /// The whole framebuffer as a target, with the viewport that covers it.
    pub(crate) fn target(&mut self) -> CpuTarget<'_> {
        let (width, height) = (self.width, self.height);
        CpuTarget {
            pixels: &mut self.pixels,
            width,
            rows: 0..height,
            viewport: Viewport {
                x: 0.0,
                y: 0.0,
                w: width as f32,
                h: height as f32,
            },
        }
    }

    /// A target that maps the `window` (`[x, y, w, h]`) of a `scene`-sized
    /// frame onto this framebuffer 1:1, as `GlyphPipeline::draw_in_window`
    /// and the cursor patch's offset viewport do.
    pub(crate) fn window_target(&mut self, window: [u32; 4], scene: [u32; 2]) -> CpuTarget<'_> {
        let mut target = self.target();
        target.viewport = Viewport {
            x: -(window[0] as f32),
            y: -(window[1] as f32),
            w: scene[0] as f32,
            h: scene[1] as f32,
        };
        target
    }
}

/// RGBA8 bytes of packed pixels, the order a GPU readback of an
/// `Rgba8UnormSrgb` target returns.
pub(crate) fn pixels_to_rgba8(pixels: &[u32]) -> Vec<u8> {
    pixels.iter().flat_map(|&p| unpack(p)).collect()
}

#[cfg(test)]
/// The packed pixel a `LoadOp::Clear(color)` stores.
pub(crate) fn clear_pixel(color: [f64; 4]) -> u32 {
    let t = tables();
    pack(
        t.encode(color[0] as f32),
        t.encode(color[1] as f32),
        t.encode(color[2] as f32),
        unorm8(color[3] as f32),
    )
}

/// The pixels a draw may write: a band of a framebuffer's rows, with the
/// viewport the draw's vertices go through. Coordinates are the whole
/// framebuffer's, so a band drawn on its own thread rasterizes exactly as it
/// would as part of the whole.
pub(crate) struct CpuTarget<'a> {
    /// The rows in `rows`, `width` pixels each.
    pixels: &'a mut [u32],
    width: u32,
    rows: std::ops::Range<u32>,
    viewport: Viewport,
}

impl<'a> CpuTarget<'a> {
    /// A whole `width` x `height` target over `pixels`, rows packed, such as
    /// a presenter's shared-memory buffer, so a frame is drawn where it is
    /// shown. `None` when a side is zero or past [`MAX_SIDE`], or `pixels`
    /// is not exactly `width * height` long.
    pub(crate) fn from_pixels(pixels: &'a mut [u32], width: u32, height: u32) -> Option<Self> {
        if CpuFramebuffer::len_for(width, height)? != pixels.len() {
            return None;
        }
        Some(CpuTarget {
            pixels,
            width,
            rows: 0..height,
            viewport: Viewport {
                x: 0.0,
                y: 0.0,
                w: width as f32,
                h: height as f32,
            },
        })
    }

    /// The same rows and viewport, borrowed again.
    pub(crate) fn reborrow(&mut self) -> CpuTarget<'_> {
        CpuTarget {
            pixels: &mut *self.pixels,
            width: self.width,
            rows: self.rows.clone(),
            viewport: self.viewport,
        }
    }

    /// The band's pixels, rows packed.
    pub(crate) fn pixels(&self) -> &[u32] {
        self.pixels
    }

    /// The framebuffer's width, which is also this band's.
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    /// The framebuffer rows this target holds.
    pub(crate) fn rows(&self) -> std::ops::Range<u32> {
        self.rows.clone()
    }

    pub(crate) fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub(crate) fn set_viewport(&mut self, viewport: Viewport) {
        self.viewport = viewport;
    }

    /// Split into the rows above `y` and the rows from `y` on, for drawing
    /// bands in parallel. `y` is clamped to the band.
    pub(crate) fn split_at_row(self, y: u32) -> (CpuTarget<'a>, CpuTarget<'a>) {
        let y = y.clamp(self.rows.start, self.rows.end);
        let at = (y - self.rows.start) as usize * self.width as usize;
        let (top, bottom) = self.pixels.split_at_mut(at);
        (
            CpuTarget {
                pixels: top,
                width: self.width,
                rows: self.rows.start..y,
                viewport: self.viewport,
            },
            CpuTarget {
                pixels: bottom,
                width: self.width,
                rows: y..self.rows.end,
                viewport: self.viewport,
            },
        )
    }

    /// The pixels a draw under `scissor` may touch: the band, clipped.
    pub(crate) fn clip(&self, scissor: Option<PixelRect>) -> Option<PixelRect> {
        let band = PixelRect::new(
            0,
            self.rows.start,
            self.width,
            self.rows.end - self.rows.start,
        );
        match scissor {
            Some(scissor) => band.intersect(scissor),
            None => (band.w > 0 && band.h > 0).then_some(band),
        }
    }

    /// Pixels `x0..x1` of framebuffer row `y`, which must lie in the band.
    #[inline(always)]
    pub(crate) fn span(&mut self, y: u32, x0: u32, x1: u32) -> &mut [u32] {
        let row = (y - self.rows.start) as usize * self.width as usize;
        &mut self.pixels[row + x0 as usize..row + x1 as usize]
    }

    /// Pixel `x` of framebuffer row `y`, which must lie in the band.
    #[inline(always)]
    pub(crate) fn pixel(&mut self, x: u32, y: u32) -> &mut u32 {
        let row = (y - self.rows.start) as usize * self.width as usize;
        &mut self.pixels[row + x as usize]
    }
}

#[cfg(test)]
impl CpuFramebuffer {
    /// [`CpuTarget::draw_banded`] over the whole framebuffer.
    pub(crate) fn draw_banded(&mut self, bands: u32, draw: impl Fn(&mut CpuTarget<'_>) + Sync) {
        self.target().draw_banded(bands, draw);
    }
}

impl CpuTarget<'_> {
    /// Run `draw` on at most `bands` horizontal bands of this target at once,
    /// bounded by available parallelism and the number of row pairs, one
    /// scoped thread each. Every band sees every instance and keeps only its
    /// own rows; coordinates and derivative quads stay the whole
    /// framebuffer's, so the result is the single-threaded one. Bands start
    /// on even framebuffer rows when this target does, and none is empty.
    pub(crate) fn draw_banded(&mut self, bands: u32, draw: impl Fn(&mut CpuTarget<'_>) + Sync) {
        let start = self.rows.start;
        let pairs = (self.rows.end - start).div_ceil(2);
        let mut bands = bands.clamp(1, pairs.max(1));
        if bands > 1 {
            let available = std::thread::available_parallelism().map_or(1, |n| n.get());
            bands = available.min(bands as usize) as u32;
        }
        if bands == 1 {
            draw(self);
            return;
        }
        let mut rest = self.reborrow();
        std::thread::scope(|scope| {
            let draw = &draw;
            for i in 1..bands {
                let next = start + (i * pairs / bands) * 2;
                let (mut band, tail) = rest.split_at_row(next);
                rest = tail;
                scope.spawn(move || draw(&mut band));
            }
            draw(&mut rest);
        });
    }
}

/// A texture format a pipeline binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CpuTextureFormat {
    /// One coverage byte per texel (the glyph mask atlas).
    R8Unorm,
    /// RGBA bytes, RGB sRGB-encoded (images, the colour atlas).
    Rgba8UnormSrgb,
    /// RGBA bytes sampled as stored.
    Rgba8Unorm,
}

impl CpuTextureFormat {
    pub(crate) const fn bytes_per_texel(self) -> u32 {
        match self {
            Self::R8Unorm => 1,
            Self::Rgba8UnormSrgb | Self::Rgba8Unorm => 4,
        }
    }
}

/// How a sampler addresses texels outside the texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AddressMode {
    ClampToEdge,
    Repeat,
}

/// Borrowed texels, tightly packed rows. Images sample their decoded pixel
/// buffer in place through one of these.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CpuTextureView<'a> {
    width: u32,
    height: u32,
    format: CpuTextureFormat,
    data: &'a [u8],
}

impl<'a> CpuTextureView<'a> {
    /// `None` when a side is zero or past [`MAX_SIDE`], or `data` is not
    /// exactly `width * height` texels.
    pub(crate) fn new(
        width: u32,
        height: u32,
        format: CpuTextureFormat,
        data: &'a [u8],
    ) -> Option<Self> {
        (texture_bytes(width, height, format)? == data.len()).then_some(Self {
            width,
            height,
            format,
            data,
        })
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn format(&self) -> CpuTextureFormat {
        self.format
    }

    pub(crate) fn data(&self) -> &'a [u8] {
        self.data
    }
}

fn texture_bytes(width: u32, height: u32, format: CpuTextureFormat) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let bytes = u64::from(width) * u64::from(height) * u64::from(format.bytes_per_texel());
    usize::try_from(bytes)
        .ok()
        .filter(|&b| b <= MAX_TEXTURE_BYTES)
}

#[cfg(test)]
/// An owned texture with sub-rectangle uploads and height growth, for the
/// glyph atlases.
#[derive(Clone, Debug)]
pub(crate) struct CpuTexture {
    width: u32,
    height: u32,
    format: CpuTextureFormat,
    data: Vec<u8>,
}

#[cfg(test)]
impl CpuTexture {
    /// A zeroed texture; `None` past [`MAX_SIDE`] or [`MAX_TEXTURE_BYTES`].
    pub(crate) fn new(width: u32, height: u32, format: CpuTextureFormat) -> Option<Self> {
        let bytes = texture_bytes(width, height, format)?;
        Some(Self {
            width,
            height,
            format,
            data: vec![0; bytes],
        })
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    pub(crate) fn format(&self) -> CpuTextureFormat {
        self.format
    }

    pub(crate) fn view(&self) -> CpuTextureView<'_> {
        CpuTextureView {
            width: self.width,
            height: self.height,
            format: self.format,
            data: &self.data,
        }
    }

    /// `Queue::write_texture` of a `w`x`h` block at `(x, y)` from tightly
    /// packed rows. `false`, writing nothing, when the block leaves the
    /// texture or `data` is not exactly its size.
    pub(crate) fn write(&mut self, x: u32, y: u32, w: u32, h: u32, data: &[u8]) -> bool {
        let bpp = self.format.bytes_per_texel() as usize;
        let fits = x.checked_add(w).is_some_and(|x1| x1 <= self.width)
            && y.checked_add(h).is_some_and(|y1| y1 <= self.height);
        let row = w as usize * bpp;
        if !fits || data.len() != row * h as usize {
            return false;
        }
        if row == 0 {
            return true;
        }
        let stride = self.width as usize * bpp;
        for (i, src) in data.chunks_exact(row).enumerate() {
            let start = (y as usize + i) * stride + x as usize * bpp;
            self.data[start..start + row].copy_from_slice(src);
        }
        true
    }

    /// Grow to `height` rows, keeping every texel where it was, as the glyph
    /// atlas grows. `false` when it would shrink or pass the caps.
    pub(crate) fn grow_height(&mut self, height: u32) -> bool {
        if height < self.height {
            return false;
        }
        let Some(bytes) = texture_bytes(self.width, height, self.format) else {
            return false;
        };
        self.data.resize(bytes, 0);
        self.height = height;
        true
    }
}

/// A quad instance after vertex expansion: where its corners land in the
/// target and which pixels it covers.
#[derive(Clone, Debug)]
pub(crate) struct Raster {
    /// Target-space x of the corner at `c.x == 0` and of the one at 1.
    pub(crate) x0: f32,
    pub(crate) x1: f32,
    /// Target-space y of the corner at `c.y == 0` and of the one at 1.
    pub(crate) y0: f32,
    pub(crate) y1: f32,
    /// Covered columns and rows, already clipped.
    pub(crate) cols: std::ops::Range<u32>,
    pub(crate) rows: std::ops::Range<u32>,
}

impl Raster {
    /// A vertex attribute that is `a0` at the `c.x == 0` corner and `a1` at
    /// the other, at the centre of column `x`. Written as a start plus a
    /// slope, as plane-equation interpolation evaluates it, so whole-pixel
    /// geometry interpolates exactly.
    #[inline(always)]
    pub(crate) fn lerp_x(&self, x: u32, a0: f32, a1: f32) -> f32 {
        a0 + (x as f32 + 0.5 - self.x0) * ((a1 - a0) / (self.x1 - self.x0))
    }

    /// [`Raster::lerp_x`] down the rows.
    #[inline(always)]
    pub(crate) fn lerp_y(&self, y: u32, a0: f32, a1: f32) -> f32 {
        a0 + (y as f32 + 0.5 - self.y0) * ((a1 - a0) / (self.y1 - self.y0))
    }
}

/// Subpixel precision of vertex positions: 8 bits.
const SUBPIXEL: f32 = 256.0;

/// Where a vertex shader's `pos + c * size` lands in the target, axis by
/// axis, through `px / screen * 2 - 1`, the y flip and the viewport
/// transform, including llvmpipe's fused viewport multiply-add.
#[inline]
fn vertex_x(p: f32, screen: f32, v: &Viewport) -> f32 {
    let ndc = p / screen * 2.0 - 1.0;
    viewport_position(ndc, v.x, v.w)
}

#[inline]
fn vertex_y(p: f32, screen: f32, v: &Viewport) -> f32 {
    let ndc = 1.0 - p / screen * 2.0;
    viewport_position(-ndc, v.y, v.h)
}

#[inline]
fn viewport_position(ndc: f32, offset: f32, size: f32) -> f32 {
    let scale = size * 0.5;
    if offset == 0.0 && ndc.abs() <= 2.0 {
        // Here ndc came from subtracting 1 in f32, so 1 + ndc and its
        // product with scale fit exactly in f64. The final f32 cast is
        // the same rounding as FMA without a libcall on baseline x86.
        ((1.0 + f64::from(ndc)) * f64::from(scale)) as f32
    } else {
        ndc.mul_add(scale, offset + scale)
    }
}

/// The position in sample-centre coordinates on llvmpipe's 8-bit grid.
#[inline]
fn snap_to_subpixel(edge: f32) -> f32 {
    ((edge - 0.5) * SUBPIXEL).round_ties_even() / SUBPIXEL
}

/// The first framebuffer pixel on or after the snapped edge.
#[inline]
fn first_centre_at_or_after(edge: f32) -> u32 {
    let centre = (edge - 0.5).clamp(0.0, MAX_SIDE as f32);
    let pixel = centre as u32;
    // An integral centre is even in 8-bit fixed point. The snap moves
    // any edge up to and including the midpoint back onto that centre.
    pixel + u32::from(centre - pixel as f32 > 0.5 / SUBPIXEL)
}

fn covered(
    a: f32,
    b: f32,
    clip_lo: f32,
    clip_hi: f32,
    limit: std::ops::Range<u32>,
) -> Option<std::ops::Range<u32>> {
    // Intersect coverage with the transformed clip volume. Keep the
    // unclipped endpoints on Raster to interpolate fragment attributes.
    let lo = a.min(b).max(clip_lo);
    let hi = a.max(b).min(clip_hi);
    if lo >= hi {
        return None;
    }
    // Top-left rule: a centre on the left (top) edge is in, on the right
    // (bottom) edge is out.
    let start = first_centre_at_or_after(lo).max(limit.start);
    let end = first_centre_at_or_after(hi).min(limit.end);
    (start < end).then_some(start..end)
}

/// Expand one instance at `pos` and `size` (pixels, as uploaded) on a
/// `screen`-sized uniform, clipped to `clip`. `None` when it covers no pixel
/// or a position is not finite.
pub(crate) fn rasterize(
    viewport: &Viewport,
    screen: [f32; 2],
    pos: [f32; 2],
    size: [f32; 2],
    clip: PixelRect,
) -> Option<Raster> {
    // Rows first: a band drawing every instance rejects most of them here.
    let y0 = vertex_y(pos[1], screen[1], viewport);
    let y1 = vertex_y(pos[1] + size[1], screen[1], viewport);
    if !(y0.is_finite() && y1.is_finite()) {
        return None;
    }
    let rows = covered(
        y0,
        y1,
        viewport.y,
        viewport.y + viewport.h,
        clip.y..clip.y + clip.h,
    )?;
    let x0 = vertex_x(pos[0], screen[0], viewport);
    let x1 = vertex_x(pos[0] + size[0], screen[0], viewport);
    if !(x0.is_finite() && x1.is_finite()) {
        return None;
    }
    let cols = covered(
        x0,
        x1,
        viewport.x,
        viewport.x + viewport.w,
        clip.x..clip.x + clip.w,
    )?;
    Some(Raster {
        x0,
        x1,
        y0,
        y1,
        cols,
        rows,
    })
}

/// WGSL `smoothstep`.
#[inline(always)]
pub(crate) fn smoothstep(low: f32, high: f32, x: f32) -> f32 {
    let t = ((x - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Fine derivatives at pixel `(x, y)`: differences across its row and
/// column in the 2x2 quad. WGSL's plain `fwidth` leaves the fine/coarse
/// choice to the backend. Quads sit on even framebuffer coordinates. A
/// neighbour outside the primitive is a helper invocation of `f`.
#[inline(always)]
pub(crate) fn fine_fwidth(x: u32, y: u32, here: f32, f: impl Fn(u32, u32) -> f32) -> f32 {
    let ddx = if x & 1 == 0 {
        f(x + 1, y) - here
    } else {
        here - f(x - 1, y)
    };
    let ddy = if y & 1 == 0 {
        f(x, y + 1) - here
    } else {
        here - f(x, y - 1)
    };
    ddx.abs() + ddy.abs()
}

/// A pixel's colour in linear light, premultiplied, as a fragment returns it
/// to a premultiplied blend.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Premul {
    pub(crate) r: f32,
    pub(crate) g: f32,
    pub(crate) b: f32,
    pub(crate) a: f32,
}

/// `PREMULTIPLIED_ALPHA_BLENDING` of `src` over `dst`, in `f32`.
#[inline(always)]
pub(crate) fn blend_premul(t: &srgb::SrgbTables, dst: u32, src: Premul) -> u32 {
    let [dr, dg, db, da] = unpack(dst);
    let k = 1.0 - src.a;
    pack(
        t.encode(src.r + t.decf[usize::from(dr)] * k),
        t.encode(src.g + t.decf[usize::from(dg)] * k),
        t.encode(src.b + t.decf[usize::from(db)] * k),
        unorm8(src.a + t.unorm[usize::from(da)] * k),
    )
}

/// A blend state's `None`: the fragment replaces the pixel.
#[inline(always)]
pub(crate) fn replace(t: &srgb::SrgbTables, src: Premul) -> u32 {
    pack(
        t.encode(src.r),
        t.encode(src.g),
        t.encode(src.b),
        unorm8(src.a),
    )
}

/// [`ConstBlend`] per channel, indexed by the destination byte.
pub(crate) struct ChannelTables([[u8; 256]; 4]);

impl ChannelTables {
    /// Blend over a span of pixels.
    #[inline]
    pub(crate) fn blend_span(&self, span: &mut [u32]) {
        for p in span {
            let [r, g, b, a] = unpack(*p);
            *p = pack(
                self.0[0][usize::from(r)],
                self.0[1][usize::from(g)],
                self.0[2][usize::from(b)],
                self.0[3][usize::from(a)],
            );
        }
    }
}

/// Spans larger than this many pixels blend through [`ChannelTables`],
/// which cost about a thousand blends to build.
pub(crate) const TABLE_SPAN_PIXELS: u64 = 4096;

/// One constant premultiplied colour, prepared for blending many pixels in
/// 16-bit integer math.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConstBlend {
    /// The colour stored where the source is opaque, or `None` when it is
    /// translucent.
    opaque: Option<u32>,
    /// Premultiplied linear source, 16-bit.
    src: [u32; 3],
    /// `1 - a` in 1/65536 steps.
    keep: u32,
    /// `a * 255` in 1/65536 steps.
    alpha: u32,
}

impl ConstBlend {
    /// `None` when the colour leaves `0..=1` or the alpha does, which the
    /// integer path cannot represent; such draws take the `f32` path.
    pub(crate) fn new(t: &srgb::SrgbTables, src: Premul) -> Option<Self> {
        let in_range = |v: f32| (0.0..=1.0).contains(&v);
        if ![src.r, src.g, src.b, src.a].into_iter().all(in_range) {
            return None;
        }
        let opaque = (src.a == 1.0).then(|| replace(t, src));
        let q = |v: f32| (v * 65535.0 + 0.5) as u32;
        Some(Self {
            opaque,
            src: [q(src.r), q(src.g), q(src.b)],
            keep: ((1.0 - src.a) * 65536.0 + 0.5) as u32,
            alpha: (src.a * 255.0 * 65536.0 + 0.5) as u32,
        })
    }

    /// The pixel it stores where it is opaque.
    pub(crate) fn opaque(&self) -> Option<u32> {
        self.opaque
    }

    /// Whether drawing it changes nothing.
    pub(crate) fn is_noop(&self) -> bool {
        self.keep == 65536 && self.src == [0; 3] && self.alpha == 0
    }

    #[inline(always)]
    fn blend(&self, t: &srgb::SrgbTables, dst: u32) -> u32 {
        let [dr, dg, db, da] = unpack(dst);
        let ch = |s: u32, d: u8| {
            let lin = s + ((u32::from(t.dec16[usize::from(d)]) * self.keep + 32768) >> 16);
            t.encode16(lin.min(65535) as u16)
        };
        pack(
            ch(self.src[0], dr),
            ch(self.src[1], dg),
            ch(self.src[2], db),
            ((self.alpha + u32::from(da) * self.keep + 32768) >> 16).min(255) as u8,
        )
    }

    /// The blend as one table per channel: with a constant source, each
    /// output channel depends only on the same channel of the destination.
    pub(crate) fn tables(&self, t: &srgb::SrgbTables) -> ChannelTables {
        let mut out = ChannelTables([[0; 256]; 4]);
        for d in 0..=255u8 {
            let [r, g, b, a] = unpack(self.blend(t, pack(d, d, d, d)));
            let i = usize::from(d);
            (out.0[0][i], out.0[1][i], out.0[2][i], out.0[3][i]) = (r, g, b, a);
        }
        out
    }

    /// Blend over a span of pixels.
    #[inline]
    pub(crate) fn blend_span(&self, t: &srgb::SrgbTables, span: &mut [u32]) {
        if let Some(px) = self.opaque {
            span.fill(px);
            return;
        }
        // Translucent layers mostly cover flat colour: reuse the last result.
        let (mut last_in, mut last_out) = (!span.first().copied().unwrap_or(0), 0);
        for p in span {
            if *p != last_in {
                last_in = *p;
                last_out = self.blend(t, last_in);
            }
            *p = last_out;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_cover(pos: f32, size: f32) -> Option<std::ops::Range<u32>> {
        let v = Viewport {
            x: 0.0,
            y: 0.0,
            w: 64.0,
            h: 64.0,
        };
        rasterize(
            &v,
            [64.0, 64.0],
            [pos, 0.0],
            [size, 1.0],
            PixelRect::new(0, 0, 64, 64),
        )
        .map(|r| r.cols)
    }

    #[test]
    fn pixel_centres_decide_coverage_with_the_top_left_rule() {
        assert_eq!(rect_cover(2.0, 3.0), Some(2..5));
        // A centre on the left edge is covered; on the right it is not.
        assert_eq!(rect_cover(2.5, 1.0), Some(2..3));
        assert_eq!(rect_cover(2.6, 0.8), None);
        assert_eq!(rect_cover(2.4, 0.2), Some(2..3));
        // Negative sizes cover the same pixels as their positive mirror.
        assert_eq!(rect_cover(5.0, -3.0), Some(2..5));
        // Within 1/512 px of a centre, the snapped edge decides.
        assert_eq!(rect_cover(2.5 + 1.0 / 1024.0, 1.0), Some(2..3));
        assert_eq!(rect_cover(2.5 + 1.0 / 512.0, 1.0), Some(2..3));
        assert_eq!(rect_cover(2.5 + 3.0 / 1024.0, 1.0), Some(3..4));
        assert_eq!(rect_cover(-10.0, 20.0), Some(0..10));
        assert_eq!(rect_cover(f32::NAN, 1.0), None);
    }

    #[test]
    fn inset_clip_volume_does_not_move_interpolation_endpoints() {
        let view = Viewport {
            x: 8.0,
            y: 8.0,
            w: 16.0,
            h: 16.0,
        };
        let r = rasterize(
            &view,
            [32.0; 2],
            [-8.0; 2],
            [20.0; 2],
            PixelRect::new(0, 0, 32, 32),
        )
        .unwrap();
        assert_eq!(r.cols, 8..14);
        assert_eq!(r.rows, 8..14);
        assert_eq!(r.lerp_x(8, 0.0, 20.0), 9.0);
        assert_eq!(r.lerp_y(8, 0.0, 20.0), 9.0);
    }

    #[test]
    fn a_window_target_offsets_the_scene() {
        let mut fb = CpuFramebuffer::new(8, 8).unwrap();
        let mut target = fb.target();
        target.set_viewport(Viewport {
            x: -10.0,
            y: -20.0,
            w: 100.0,
            h: 100.0,
        });
        assert_eq!(target.width(), 8);
        let same = target.viewport();
        let target = fb.window_target([10, 20, 8, 8], [100, 100]);
        assert_eq!(target.viewport(), same);
        let r = rasterize(
            &target.viewport(),
            [100.0, 100.0],
            [12.0, 21.0],
            [3.0, 2.0],
            target.clip(None).unwrap(),
        )
        .unwrap();
        assert_eq!((r.cols, r.rows), (2..5, 1..3));
    }

    #[test]
    fn bands_split_rows_and_clip_scissors() {
        let mut fb = CpuFramebuffer::new(4, 6).unwrap();
        let (top, mut bottom) = fb.target().split_at_row(2);
        assert_eq!((top.rows(), bottom.rows()), (0..2, 2..6));
        assert_eq!(
            bottom.clip(Some(PixelRect::new(1, 0, 2, 3))),
            Some(PixelRect::new(1, 2, 2, 1))
        );
        *bottom.pixel(3, 5) = 7;
        assert_eq!(fb.pixels()[23], 7);
    }

    #[test]
    fn banded_drawing_matches_one_band() {
        let quads: Vec<_> = (0..40)
            .map(|i| crate::quad::QuadInstance {
                pos: [i as f32 * 1.7, i as f32 * 0.9],
                size: [9.5, 7.25],
                color: [0.1 * (i % 10) as f32, 0.5, 0.9, 0.6],
            })
            .collect();
        let draw = |t: &mut CpuTarget<'_>| {
            quad::draw_quads(t, [70.0, 45.0], &quads, quad::QuadBlend::Blend, None);
        };
        let mut one = CpuFramebuffer::new(70, 45).unwrap();
        one.draw_banded(1, draw);
        for bands in [2, 3, 7, 100] {
            let mut many = CpuFramebuffer::new(70, 45).unwrap();
            many.draw_banded(bands, draw);
            assert_eq!(many, one, "{bands} bands");
        }
    }

    #[test]
    fn bands_cover_all_rows_without_empty_callbacks_or_excess_threads() {
        let available = std::thread::available_parallelism().map_or(1, |n| n.get()) as u32;
        for (height, requested) in [(10, 4), (9, 0), (3, u32::MAX), (MAX_SIDE, available + 1)] {
            let mut fb = CpuFramebuffer::new(1, height).unwrap();
            let seen = std::sync::Mutex::new(Vec::new());
            fb.draw_banded(requested, |band| {
                seen.lock()
                    .unwrap()
                    .push((band.rows(), std::thread::current().id()));
            });
            let mut seen = seen.into_inner().unwrap();
            seen.sort_unstable_by_key(|(rows, _)| rows.start);
            let expected = requested.max(1).min(height.div_ceil(2)).min(available);
            assert_eq!(seen.len(), expected as usize);
            let mut workers = std::collections::HashSet::new();
            let mut next_row = 0;
            for (rows, thread) in seen {
                assert!(!rows.is_empty(), "{rows:?}");
                assert_eq!(rows.start, next_row);
                assert_eq!(rows.start % 2, 0);
                next_row = rows.end;
                workers.insert(thread);
            }
            assert_eq!(next_row, height);
            assert!(workers.len() <= expected as usize);
        }
    }

    #[test]
    fn textures_take_sub_rectangles_and_grow_in_place() {
        let mut tex = CpuTexture::new(4, 2, CpuTextureFormat::R8Unorm).unwrap();
        assert!(tex.write(1, 0, 2, 2, &[1, 2, 3, 4]));
        assert!(!tex.write(3, 0, 2, 1, &[1, 2]), "leaves the texture");
        assert!(!tex.write(0, 0, 2, 1, &[1]), "short data");
        assert!(tex.grow_height(4));
        assert_eq!(
            (tex.width(), tex.height(), tex.format()),
            (4, 4, CpuTextureFormat::R8Unorm)
        );
        assert_eq!(tex.view().data()[..8], [0, 1, 2, 0, 0, 3, 4, 0]);
        assert_eq!(tex.view().data().len(), 16);
        assert!(!tex.grow_height(3));
        assert!(CpuTexture::new(MAX_SIDE + 1, 1, CpuTextureFormat::R8Unorm).is_none());
        assert!(CpuTextureView::new(2, 2, CpuTextureFormat::Rgba8UnormSrgb, &[0; 15]).is_none());
    }

    #[test]
    fn framebuffers_round_trip_rgba_and_clear_in_linear_light() {
        let mut fb = CpuFramebuffer::from_rgba8(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        assert_eq!(fb.to_rgba8(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(fb.pixels()[0], 0x0401_0203);
        fb.clear([0.5, 0.0, 1.0, 0.5]);
        assert_eq!(fb.to_rgba8()[..4], [188, 0, 255, 128]);
        assert!(CpuFramebuffer::new(0, 1).is_none());
        assert!(fb.resize(3, 3));
        assert_eq!(fb.pixels().len(), 9);
    }

    #[test]
    fn integer_blending_matches_the_float_blend() {
        let t = tables();
        let srcs = [
            Premul {
                r: 0.2,
                g: 0.0,
                b: 0.45,
                a: 0.5,
            },
            Premul {
                r: 0.001,
                g: 0.01,
                b: 0.1,
                a: 0.13,
            },
            Premul {
                r: 0.9,
                g: 0.5,
                b: 0.0,
                a: 1.0,
            },
        ];
        for src in srcs {
            let blend = ConstBlend::new(t, src).unwrap();
            for d in [0u32, 0xff12_3456, 0x80ff_ffff, 0xff00_0001] {
                let a = unpack(blend.blend(t, d));
                let b = unpack(blend_premul(t, d, src));
                for c in 0..4 {
                    assert!(a[c].abs_diff(b[c]) <= 1, "{src:?} over {d:#x}: {a:?} {b:?}");
                }
                let mut span = [d];
                blend.tables(t).blend_span(&mut span);
                assert_eq!(unpack(span[0]), a, "tables agree with the blend");
            }
        }
    }
}

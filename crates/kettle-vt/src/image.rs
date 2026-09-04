//! Decoded image payload shared between the VT layer and the renderer.

use std::sync::Arc;

use crate::graphics_limits::{GraphicsBudget, GraphicsReservation};

/// Decompression-bomb defense: max per-axis pixel count
/// accepted by `ImageData::from_encoded`. Matches `sixel::MAX_DIM`
/// (same realistic-terminal envelope).
pub const MAX_IMAGE_DIM: u32 = 8192;

/// Decompression-bomb defense: max total bytes the `image`
/// crate may allocate while decoding. The independent axis cap still rejects
/// pathological shapes; the byte cap limits any one retained image to 64 MiB.
///
/// `pub(crate)` so the kitty decoder can bound its zlib (`o=z`)
/// inflate to this same envelope.
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// A decoded image plus placement metadata (kitty image/placement ids +
/// z-index; Sixel/iTerm2 use `id = None`, `placement_id = 0`, `z = 0`).
#[derive(Clone, Debug)]
pub struct Placed {
    pub img: ImageData,
    pub id: Option<u32>,
    /// Kitty placement id (`p=`). Zero means an anonymous placement.
    pub placement_id: u32,
    pub z: i32,
    /// Kitty source/destination geometry. `None` preserves the legacy
    /// Sixel/iTerm2 placement and cursor policy.
    pub params: Option<PlacementParams>,
}

/// Raw Kitty placement geometry, resolved against live cell/pixel dimensions
/// by kettle-core. Keeping the protocol values allows monitor/DPI changes to
/// recompute natural-size and one-axis-auto placements without losing intent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PlacementParams {
    /// Source rectangle in image pixels (`x,y,w,h`); zero width/height means
    /// "to the image edge".
    pub source_x: u32,
    pub source_y: u32,
    pub source_width: u32,
    pub source_height: u32,
    /// Destination rectangle in cells (`c,r`); zero means auto.
    pub columns: u32,
    pub rows: u32,
    /// Pixel offset within the first destination cell (`X,Y`).
    pub cell_x_offset: u32,
    pub cell_y_offset: u32,
    /// Kitty `C=1`: do not move the application cursor after placement.
    pub suppress_cursor_movement: bool,
}

impl Placed {
    pub fn plain(img: ImageData) -> Placed {
        Placed {
            img,
            id: None,
            placement_id: 0,
            z: 0,
            params: None,
        }
    }
}

/// An RGBA8 image ready to upload as a GPU texture.
#[derive(Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA8, top-left origin.
    pub rgba: Arc<Vec<u8>>,
    /// Pins the CPU reservation for exactly as long as this allocation lives.
    /// Clones share both the pixel buffer and this token, so bytes count once.
    _cpu: Arc<GraphicsReservation>,
}

impl ImageData {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<ImageData> {
        Self::new_with_budget(width, height, rgba, &GraphicsBudget::default())
    }

    pub fn new_with_budget(
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        budget: &GraphicsBudget,
    ) -> Option<ImageData> {
        // Checked arithmetic. The previous unchecked
        // `width as usize * height as usize * 4` would panic on debug
        // and silently wrap on release for adversarial header values
        // (e.g. a kitty `f=32,s=4294967295,v=4294967295` payload). The
        // overflow is reachable on 64-bit: `u32::MAX² × 4` ≈ 7.4×10¹⁹
        // bytes, well above `u64::MAX`. With `checked_mul` the
        // oversize case becomes a clean `None` return, identical UX
        // to the existing `rgba.len()` mismatch path. The 8192-px
        // `from_encoded` cap funnels the encoded path safely;
        // this guard covers the *raw* `ImageData::new` surface for any
        // future caller.
        if width == 0 || height == 0 {
            return None;
        }
        // Cap per-axis dimensions at the same `MAX_IMAGE_DIM`
        // the `from_encoded` decoder already enforces — but here, at the single
        // chokepoint every constructor funnels through (`new`/`crop`/`solid`/
        // `from_encoded`). The kitty `f=32`/`f=24` raw-pixel branches parse
        // width/height straight from the untrusted `s=`/`v=` control words; a
        // payload like `f=32,s=10000,v=1` (40 KB of pixels — trivially under the
        // payload caps) used to yield a 10000×1 `ImageData` that reached
        // `wgpu::Device::create_texture` with width > the 8192 `max_texture_
        // dimension_2d` limit, a validation error wgpu's default handler turns
        // into a panic = (panic=abort) a whole-process abort killing every tab.
        // Rejecting oversized dims here closes that remote DoS for every caller.
        if width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return None;
        }
        let expected = rgba_bytes(width, height)?;
        if expected > budget.limits().image_bytes {
            return None;
        }
        if rgba.len() != expected {
            return None;
        }
        let reservation = budget.reserve_image_cpu(expected)?;
        Self::from_reserved(width, height, rgba, reservation)
    }

    pub(crate) fn from_reserved(
        width: u32,
        height: u32,
        rgba: Vec<u8>,
        reservation: GraphicsReservation,
    ) -> Option<ImageData> {
        if width == 0 || height == 0 || width > MAX_IMAGE_DIM || height > MAX_IMAGE_DIM {
            return None;
        }
        let expected = rgba_bytes(width, height)?;
        if expected > reservation.budget().limits().image_bytes
            || rgba.len() != expected
            || reservation.bytes() != expected
        {
            return None;
        }
        Some(ImageData {
            width,
            height,
            rgba: Arc::new(rgba),
            _cpu: Arc::new(reservation),
        })
    }

    pub fn byte_len(&self) -> usize {
        self.rgba.len()
    }

    pub fn allocation_key(&self) -> usize {
        Arc::as_ptr(&self.rgba) as usize
    }

    /// Decode an encoded terminal-embedded image (PNG / JPEG / GIF — the
    /// only formats kettle-vt enables on the `image` crate per Cargo.toml's
    /// narrowed feature list). Bounded against decompression bombs:
    /// rejects images wider/taller than 8192 px or whose decoded RGBA
    /// buffer would exceed 64 MiB.
    pub fn from_encoded(bytes: &[u8]) -> Option<ImageData> {
        Self::from_encoded_with_budget(bytes, &GraphicsBudget::default())
    }

    pub(crate) fn from_encoded_with_budget(
        bytes: &[u8],
        budget: &GraphicsBudget,
    ) -> Option<ImageData> {
        // Reserve both the retained RGBA output and the decoder's bounded
        // working allocation before invoking the image crate.
        let decode_cap = budget.limits().image_bytes.min(MAX_IMAGE_BYTES as usize);
        let mut output = budget.reserve_image_cpu(decode_cap)?;
        let _scratch = budget.reserve_transient_cpu(decode_cap)?;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIM);
        limits.max_image_height = Some(MAX_IMAGE_DIM);
        limits.max_alloc = Some(decode_cap as u64);
        reader.limits(limits);
        let img = reader.decode().ok()?.to_rgba8();
        let expected = rgba_bytes(img.width(), img.height())?;
        if expected > decode_cap || !output.shrink_to(expected) {
            return None;
        }
        ImageData::from_reserved(img.width(), img.height(), img.into_raw(), output)
    }

    /// A copied sub-rectangle. The rect is clamped to the image bounds; an
    /// empty intersection yields `None`. Used to slice the tile of a kitty
    /// virtual image that a single Unicode-placeholder cell displays.
    pub fn crop(&self, x: u32, y: u32, w: u32, h: u32) -> Option<ImageData> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let w = w.min(self.width - x);
        let h = h.min(self.height - y);
        if w == 0 || h == 0 {
            return None;
        }
        let bytes = rgba_bytes(w, h)?;
        let reservation = self._cpu.budget().reserve_image_cpu(bytes)?;
        let mut out = Vec::new();
        out.try_reserve_exact(bytes).ok()?;
        for row in 0..h {
            let src = ((u64::from(y + row) * u64::from(self.width) + u64::from(x)) * 4) as usize;
            let row_bytes = usize::try_from(u64::from(w) * 4).ok()?;
            out.extend_from_slice(&self.rgba[src..src + row_bytes]);
        }
        ImageData::from_reserved(w, h, out, reservation)
    }

    /// A `w × h` canvas filled with one RGBA color (kitty animation `Y=`
    /// background). Returns `None` for zero dimensions.
    pub fn solid(w: u32, h: u32, color: [u8; 4]) -> Option<ImageData> {
        Self::solid_with_budget(w, h, color, &GraphicsBudget::default())
    }

    pub(crate) fn solid_with_budget(
        w: u32,
        h: u32,
        color: [u8; 4],
        budget: &GraphicsBudget,
    ) -> Option<ImageData> {
        if w == 0 || h == 0 || w > MAX_IMAGE_DIM || h > MAX_IMAGE_DIM {
            return None;
        }
        let bytes = rgba_bytes(w, h)?;
        let reservation = budget.reserve_image_cpu(bytes)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(bytes).ok()?;
        while rgba.len() < bytes {
            rgba.extend_from_slice(&color);
        }
        ImageData::from_reserved(w, h, rgba, reservation)
    }

    /// Compose `src` onto this image at `(x, y)`, clipped to bounds. With
    /// `replace`, pixels are overwritten; otherwise `src` is alpha-blended
    /// over the destination (straight-alpha "source-over"). This is the
    /// kitty animation frame-composition primitive (`graphics-protocol.rst`
    /// frame canvas + `a=c`).
    pub fn compose(&mut self, src: &ImageData, x: u32, y: u32, replace: bool) -> bool {
        if x >= self.width || y >= self.height {
            return true;
        }
        let cw = src.width.min(self.width - x);
        let ch = src.height.min(self.height - y);
        if Arc::strong_count(&self.rgba) > 1 {
            // `Arc::make_mut` would allocate an unaccounted full-image copy.
            // Reserve first and clone fallibly, then install matching pixels +
            // lease as one allocation identity.
            let Some(reservation) = self._cpu.budget().reserve_image_cpu(self.rgba.len()) else {
                return false;
            };
            let mut copy = Vec::new();
            if copy.try_reserve_exact(self.rgba.len()).is_err() {
                return false;
            }
            copy.extend_from_slice(&self.rgba);
            self.rgba = Arc::new(copy);
            self._cpu = Arc::new(reservation);
        }
        let Some(dst) = Arc::get_mut(&mut self.rgba) else {
            return false;
        };
        for row in 0..ch {
            for col in 0..cw {
                // Compute byte offsets in u64 so the multiply can't
                // wrap u32 for very large frames. `cw`/`ch` already clamp the
                // result in-bounds (x+col < width, y+row < height), so the
                // cast back to usize is always valid.
                let s = ((row as u64 * src.width as u64 + col as u64) * 4) as usize;
                let d = (((y as u64 + row as u64) * self.width as u64 + x as u64 + col as u64) * 4)
                    as usize;
                if replace {
                    dst[d..d + 4].copy_from_slice(&src.rgba[s..s + 4]);
                    continue;
                }
                let sa = src.rgba[s + 3] as u32;
                if sa == 0 {
                    continue;
                }
                if sa == 255 {
                    dst[d..d + 4].copy_from_slice(&src.rgba[s..s + 4]);
                    continue;
                }
                // Straight-alpha (non-premultiplied) source-over:
                //   out_a = sa + da*(1-sa)
                //   out_c = (sc*sa + dc*da*(1-sa)) / out_a
                // The destination's own alpha weights its contribution, and
                // the result is divided back out of premultiplied space —
                // without that division, colour over a transparent
                // destination comes out darkened toward black instead of
                // keeping its hue at reduced alpha, which is exactly the
                // case a kitty animation frame canvas starts from.
                let da = dst[d + 3] as u32;
                // 255 × out_a, i.e. the premultiplied weights' sum. `sa > 0`
                // here (the fully transparent source returned above), so this
                // is never zero and the divide is always defined.
                let out_a = sa * 255 + da * (255 - sa);
                let blend = |sc: u8, dc: u8| -> u8 {
                    let num = sc as u32 * sa * 255 + dc as u32 * da * (255 - sa);
                    ((num + out_a / 2) / out_a) as u8
                };
                for k in 0..3 {
                    dst[d + k] = blend(src.rgba[s + k], dst[d + k]);
                }
                // The colour above divides by the exact `out_a` while the
                // stored alpha is that value rounded to 8 bits, so the pixel
                // can read up to ~0.2% bright against its own alpha tag. The
                // alternative — dividing by the rounded alpha — trades that
                // for a colour error of the same order, and both are inside
                // one 8-bit step; the exact divisor is the one that keeps the
                // opaque-destination case bit-identical to the pre-existing
                // formula, so no stored output shifts.
                dst[d + 3] = ((out_a + 127) / 255) as u8;
            }
        }
        true
    }
}

pub(crate) fn rgba_bytes(width: u32, height: u32) -> Option<usize> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    usize::try_from(bytes).ok()
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({}x{})", self.width, self.height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_slices_and_clamps() {
        // 3×2 RGBA image, each pixel's R = x, G = y.
        let mut px = Vec::new();
        for y in 0..2u8 {
            for x in 0..3u8 {
                px.extend_from_slice(&[x, y, 0, 255]);
            }
        }
        let img = ImageData::new(3, 2, px).unwrap();
        let c = img.crop(1, 0, 2, 2).unwrap();
        assert_eq!((c.width, c.height), (2, 2));
        // Top-left of the crop is source pixel (1,0).
        assert_eq!(&c.rgba[0..4], &[1, 0, 0, 255]);
        // Width is clamped to the image edge.
        let edge = img.crop(2, 1, 9, 9).unwrap();
        assert_eq!((edge.width, edge.height), (1, 1));
        assert_eq!(&edge.rgba[0..4], &[2, 1, 0, 255]);
        // Fully out of bounds → None.
        assert!(img.crop(3, 0, 1, 1).is_none());
    }

    #[test]
    fn solid_fills_one_color() {
        let s = ImageData::solid(2, 3, [10, 20, 30, 255]).unwrap();
        assert_eq!((s.width, s.height), (2, 3));
        assert!(
            s.rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [10, 20, 30, 255])
        );
        assert!(ImageData::solid(0, 4, [0; 4]).is_none());
    }

    #[test]
    fn solid_delegates_to_the_budgeted_constructor() {
        let body = include_str!("image.rs")
            .split("pub fn solid(")
            .nth(1)
            .and_then(|rest| rest.split("pub(crate) fn solid_with_budget(").next())
            .expect("solid body");
        assert!(body.contains("Self::solid_with_budget"));
    }

    /// `ImageData::new` must not panic or wrap on adversarial
    /// `width × height × 4` arithmetic. Tests `u32::MAX × u32::MAX × 4`
    /// (which overflows `u64::MAX` ≈ 1.8 × 10¹⁹ on 64-bit) returns
    /// cleanly — no panic in debug, no silent acceptance in release.
    /// Without `checked_mul` this would panic on debug builds and
    /// silently compare against a wrapped value on release.
    #[test]
    fn new_rejects_overflowing_dimensions_without_panic() {
        // u32::MAX × u32::MAX × 4 = 7.4 × 10¹⁹ — overflows u64.
        // Empty rgba can never equal the (unrepresentable) expected
        // size, so the only correct answer is None.
        assert!(ImageData::new(u32::MAX, u32::MAX, vec![]).is_none());
        // The intermediate product u32::MAX × u32::MAX = 1.8 × 10¹⁹
        // already saturates u64; the `* 4` step is what tips it over.
        // Cover the boundary just below as well.
        assert!(ImageData::new(u32::MAX, 1, vec![]).is_none());
        // Sanity: a sane construction still works.
        assert!(ImageData::new(2, 2, vec![0; 16]).is_some());
    }

    /// Drift guard: `ImageData::new` must reject per-axis
    /// dimensions above `MAX_IMAGE_DIM` so the kitty `f=32`/`f=24` raw-pixel
    /// decoders (which parse width/height straight from the untrusted `s=`/`v=`
    /// control words) can never produce an image that overflows the GPU's
    /// 8192-px `max_texture_dimension_2d` and aborts the renderer. The byte
    /// buffer is sized to the claimed dims so we test the dim cap specifically,
    /// not the size-mismatch path.
    #[test]
    fn new_rejects_dims_above_max_image_dim() {
        // One row at the cap is fine.
        assert!(ImageData::new(MAX_IMAGE_DIM, 1, vec![0; MAX_IMAGE_DIM as usize * 4]).is_some());
        // One past the cap (the `f=32,s=8193,v=1` shape) is rejected, even
        // though the byte buffer matches the claimed size exactly.
        let w = MAX_IMAGE_DIM + 1;
        assert!(ImageData::new(w, 1, vec![0; w as usize * 4]).is_none());
        assert!(ImageData::new(1, w, vec![0; w as usize * 4]).is_none());
        let budget = GraphicsBudget::default();
        let bytes = rgba_bytes(w, 1).unwrap();
        let reservation = budget.reserve_image_cpu(bytes).unwrap();
        assert!(ImageData::from_reserved(w, 1, vec![0; bytes], reservation).is_none());
        // `solid` guards its pre-fill allocation the same way.
        assert!(ImageData::solid(MAX_IMAGE_DIM + 1, 1, [0; 4]).is_none());
    }

    #[test]
    fn image_byte_budget_accepts_limit_and_rejects_one_past_without_large_allocations() {
        assert_eq!(rgba_bytes(8192, 2048), Some(MAX_IMAGE_BYTES as usize));
        assert_eq!(
            rgba_bytes(8192, 2049),
            Some(MAX_IMAGE_BYTES as usize + 8192 * 4)
        );

        let limits = crate::GraphicsLimits {
            image_bytes: 16,
            retained_bytes: 32,
            ..crate::GraphicsLimits::default()
        };
        let budget = GraphicsBudget::isolated(limits).unwrap();
        assert!(ImageData::new_with_budget(2, 2, vec![0; 16], &budget).is_some());
        assert!(ImageData::new_with_budget(5, 1, vec![0; 20], &budget).is_none());
    }

    #[test]
    fn compose_refuses_unbudgeted_copy_on_write_without_mutating() {
        let limits = crate::GraphicsLimits {
            image_bytes: 16,
            retained_bytes: 16,
            ..crate::GraphicsLimits::default()
        };
        let budget = GraphicsBudget::isolated(limits).unwrap();
        let mut dst = ImageData::new_with_budget(2, 2, vec![0; 16], &budget).unwrap();
        let pinned = dst.clone();
        let src = ImageData::new(1, 1, vec![255, 0, 0, 255]).unwrap();
        assert!(!dst.compose(&src, 0, 0, true));
        assert_eq!(dst.rgba.as_slice(), &[0; 16]);
        assert_eq!(pinned.rgba.as_slice(), &[0; 16]);
    }

    /// Drift guard for the decompression-bomb defense in
    /// `from_encoded`. Encodes a small PNG (positive case) and a PNG
    /// whose width exceeds `MAX_IMAGE_DIM` (negative case) via the
    /// `image` crate's encoder, then re-decodes through `from_encoded`
    /// and asserts the oversized one is rejected by the `Limits` we
    /// install. If a future refactor of `from_encoded` drops the
    /// `ImageReader::limits` wire-up, this test fails immediately
    /// rather than the regression slipping into a release.
    #[test]
    fn from_encoded_rejects_oversized_images() {
        use image::ImageEncoder;
        let encode_solid = |w: u32, h: u32| -> Vec<u8> {
            let pixels = vec![0u8; (w as usize) * (h as usize) * 4];
            let mut buf = Vec::new();
            let enc = image::codecs::png::PngEncoder::new(&mut buf);
            enc.write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
                .expect("encode test PNG");
            buf
        };

        // Positive: 4×4 PNG round-trips through from_encoded.
        let ok = encode_solid(4, 4);
        let decoded = ImageData::from_encoded(&ok).expect("small PNG should decode");
        assert_eq!((decoded.width, decoded.height), (4, 4));

        // Negative: width exceeds MAX_IMAGE_DIM (8192). The image-crate
        // encoder accepts arbitrary widths; our decoder must reject.
        // Use 8193 × 1 — minimal pixel-count over the dim cap, so the
        // test cost stays low (8193 × 4 = ~32 KB encoded buffer).
        let oversized = encode_solid(MAX_IMAGE_DIM + 1, 1);
        assert!(
            ImageData::from_encoded(&oversized).is_none(),
            "from_encoded must reject width {} (cap {MAX_IMAGE_DIM})",
            MAX_IMAGE_DIM + 1
        );
    }

    /// Straight-alpha source-over weights the destination by its OWN alpha
    /// and divides the result back out of premultiplied space:
    /// `out_a = sa + da*(1-sa)`, `out_c = (sc*sa + dc*da*(1-sa)) / out_a`.
    /// Dropping either term darkens colour toward black in proportion to
    /// the transparency it is drawn over — and a kitty animation frame
    /// canvas starts out fully transparent, so that is the common case, not
    /// the corner case. The opaque-destination test above cannot see this:
    /// with `da = 255` both terms collapse to the naive form.
    #[test]
    fn compose_over_transparent_destination_preserves_color() {
        // 50% red over transparent black stays red at 50% alpha.
        let mut canvas = ImageData::solid(1, 1, [0, 0, 0, 0]).unwrap();
        let half_red = ImageData::new(1, 1, vec![255, 0, 0, 128]).unwrap();
        assert!(canvas.compose(&half_red, 0, 0, false));
        assert_eq!(&canvas.rgba[0..4], &[255, 0, 0, 128]);

        // Over a half-transparent blue destination both hues survive,
        // weighted by the destination's alpha: out_a = 0.5 + 0.5*0.5 = 0.75,
        // out_r = 0.5/0.75, out_b = (0.5*0.5)/0.75.
        let mut canvas = ImageData::solid(1, 1, [0, 0, 255, 128]).unwrap();
        assert!(canvas.compose(&half_red, 0, 0, false));
        assert_eq!(&canvas.rgba[0..4], &[170, 0, 85, 192]);
    }

    /// The blend divides by `out_a = sa*255 + da*(255-sa)`, which is zero
    /// only when `sa` is. That case is short-circuited before the divide —
    /// a fully transparent source contributes nothing — and this pins the
    /// invariant so a future edit to the early-out cannot quietly turn the
    /// composite into a division by zero.
    #[test]
    fn compose_skips_a_fully_transparent_source_pixel() {
        for da in [0u8, 128, 255] {
            let mut canvas = ImageData::solid(1, 1, [10, 20, 30, da]).unwrap();
            let clear = ImageData::new(1, 1, vec![255, 0, 0, 0]).unwrap();
            assert!(canvas.compose(&clear, 0, 0, false));
            assert_eq!(
                &canvas.rgba[0..4],
                &[10, 20, 30, da],
                "a zero-alpha source pixel must leave the destination alone"
            );
        }
    }

    #[test]
    fn compose_replace_blend_and_clip() {
        // 3×2 opaque black canvas.
        let mut canvas = ImageData::solid(3, 2, [0, 0, 0, 255]).unwrap();

        // Replace: opaque red 1×1 at (1,0) overwrites just that pixel.
        let red = ImageData::new(1, 1, vec![255, 0, 0, 255]).unwrap();
        canvas.compose(&red, 1, 0, true);
        assert_eq!(&canvas.rgba[4..8], &[255, 0, 0, 255]);
        assert_eq!(&canvas.rgba[0..4], &[0, 0, 0, 255], "neighbor untouched");

        // Blend: 50%-alpha white over black → mid grey (alpha-rounded).
        let half = ImageData::new(1, 1, vec![255, 255, 255, 128]).unwrap();
        canvas.compose(&half, 0, 0, false);
        let p = &canvas.rgba[0..4];
        assert!(
            (127..=129).contains(&p[0]) && p[0] == p[1] && p[1] == p[2],
            "blended grey, got {p:?}"
        );
        assert_eq!(p[3], 255, "over an opaque canvas stays opaque");

        // Fully transparent src is a no-op.
        let clear = ImageData::new(1, 1, vec![9, 9, 9, 0]).unwrap();
        let before = canvas.rgba.to_vec();
        canvas.compose(&clear, 2, 1, false);
        assert_eq!(canvas.rgba.to_vec(), before);

        // Oversized src is clipped to the canvas; off-canvas origin no-ops.
        let big = ImageData::solid(9, 9, [1, 2, 3, 255]).unwrap();
        canvas.compose(&big, 2, 1, true);
        // Pixel (2,1) in a 3-wide image = index 5 → byte offset 20.
        assert_eq!(&canvas.rgba[20..24], &[1, 2, 3, 255]);
        let snapshot = canvas.rgba.to_vec();
        canvas.compose(&big, 99, 0, true);
        assert_eq!(
            canvas.rgba.to_vec(),
            snapshot,
            "off-canvas origin is a no-op"
        );
    }
}

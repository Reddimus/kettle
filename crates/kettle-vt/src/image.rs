//! Decoded image payload shared between the VT layer and the renderer.

use std::sync::Arc;

/// Cycle 576 decompression-bomb defense: max per-axis pixel count
/// accepted by `ImageData::from_encoded`. Matches `sixel::MAX_DIM`
/// (cycle predates the audit doc; same realistic-terminal envelope).
const MAX_IMAGE_DIM: u32 = 8192;

/// Cycle 576 decompression-bomb defense: max total bytes the `image`
/// crate may allocate while decoding. 8192² × 4 RGBA bytes = 256 MiB,
/// the natural upper bound paired with `MAX_IMAGE_DIM`.
const MAX_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

/// A decoded image plus placement metadata (kitty image id + z-index;
/// Sixel/iTerm2 use `id = None`, `z = 0`).
#[derive(Clone, Debug)]
pub struct Placed {
    pub img: ImageData,
    pub id: Option<u32>,
    pub z: i32,
}

impl Placed {
    pub fn plain(img: ImageData) -> Placed {
        Placed {
            img,
            id: None,
            z: 0,
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
}

impl ImageData {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<ImageData> {
        // Cycle 577: checked arithmetic. The previous unchecked
        // `width as usize * height as usize * 4` would panic on debug
        // and silently wrap on release for adversarial header values
        // (e.g. a kitty `f=32,s=4294967295,v=4294967295` payload). The
        // overflow is reachable on 64-bit: `u32::MAX² × 4` ≈ 7.4×10¹⁹
        // bytes, well above `u64::MAX`. With `checked_mul` the
        // oversize case becomes a clean `None` return, identical UX
        // to the existing `rgba.len()` mismatch path. Cycle 576's
        // 8192-px `from_encoded` cap funnels the encoded path safely;
        // this guard covers the *raw* `ImageData::new` surface for any
        // future caller.
        if width == 0 || height == 0 {
            return None;
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|wh| wh.checked_mul(4))?;
        if rgba.len() != expected {
            return None;
        }
        Some(ImageData {
            width,
            height,
            rgba: Arc::new(rgba),
        })
    }

    /// Decode an encoded terminal-embedded image (PNG / JPEG / GIF — the
    /// only formats kettle-vt enables on the `image` crate per Cargo.toml
    /// cycle-277 narrow features). Bounded against decompression bombs:
    /// rejects images wider/taller than 8192 px or whose decoded RGBA
    /// buffer would exceed 256 MiB. Cycle 576.
    pub fn from_encoded(bytes: &[u8]) -> Option<ImageData> {
        let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIM);
        limits.max_image_height = Some(MAX_IMAGE_DIM);
        limits.max_alloc = Some(MAX_IMAGE_BYTES);
        reader.limits(limits);
        let img = reader.decode().ok()?.to_rgba8();
        ImageData::new(img.width(), img.height(), img.into_raw())
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
        let mut out = Vec::with_capacity(w as usize * h as usize * 4);
        for row in 0..h {
            let src = (((y + row) * self.width + x) * 4) as usize;
            out.extend_from_slice(&self.rgba[src..src + w as usize * 4]);
        }
        ImageData::new(w, h, out)
    }

    /// A `w × h` canvas filled with one RGBA color (kitty animation `Y=`
    /// background). Returns `None` for zero dimensions.
    pub fn solid(w: u32, h: u32, color: [u8; 4]) -> Option<ImageData> {
        if w == 0 || h == 0 {
            return None;
        }
        let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
        for _ in 0..(w as usize * h as usize) {
            rgba.extend_from_slice(&color);
        }
        ImageData::new(w, h, rgba)
    }

    /// Compose `src` onto this image at `(x, y)`, clipped to bounds. With
    /// `replace`, pixels are overwritten; otherwise `src` is alpha-blended
    /// over the destination (straight-alpha "source-over"). This is the
    /// kitty animation frame-composition primitive (`graphics-protocol.rst`
    /// frame canvas + `a=c`).
    pub fn compose(&mut self, src: &ImageData, x: u32, y: u32, replace: bool) {
        if x >= self.width || y >= self.height {
            return;
        }
        let cw = src.width.min(self.width - x);
        let ch = src.height.min(self.height - y);
        let dst = std::sync::Arc::make_mut(&mut self.rgba);
        for row in 0..ch {
            for col in 0..cw {
                let s = ((row * src.width + col) * 4) as usize;
                let d = (((y + row) * self.width + x + col) * 4) as usize;
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
                // out = src*a + dst*(1-a), rounded; a = sa/255.
                let blend = |sc: u8, dc: u8| -> u8 {
                    ((sc as u32 * sa + dc as u32 * (255 - sa) + 127) / 255) as u8
                };
                for k in 0..3 {
                    dst[d + k] = blend(src.rgba[s + k], dst[d + k]);
                }
                dst[d + 3] = (sa + (dst[d + 3] as u32) * (255 - sa) / 255).min(255) as u8;
            }
        }
    }
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
        assert!(s.rgba.chunks_exact(4).all(|p| p == [10, 20, 30, 255]));
        assert!(ImageData::solid(0, 4, [0; 4]).is_none());
    }

    /// Cycle 577: `ImageData::new` must not panic or wrap on adversarial
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

    /// Cycle 576 drift guard for the decompression-bomb defense in
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

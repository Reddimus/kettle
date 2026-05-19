//! Decoded image payload shared between the VT layer and the renderer.

use std::sync::Arc;

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
        if width == 0 || height == 0 || rgba.len() != (width as usize * height as usize * 4) {
            return None;
        }
        Some(ImageData {
            width,
            height,
            rgba: Arc::new(rgba),
        })
    }

    /// Decode an encoded image (PNG/JPEG/GIF/WebP/BMP) via the `image` crate.
    pub fn from_encoded(bytes: &[u8]) -> Option<ImageData> {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
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
}

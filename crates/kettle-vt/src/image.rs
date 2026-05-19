//! Decoded image payload shared between the VT layer and the renderer.

use std::sync::Arc;

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
}

impl std::fmt::Debug for ImageData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ImageData({}x{})", self.width, self.height)
    }
}

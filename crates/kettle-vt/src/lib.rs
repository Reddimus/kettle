//! kettle-vt: image-protocol support layered in front of the VT engine.
//!
//! Sixel, the kitty graphics protocol and iTerm2 inline images are extracted
//! from the PTY stream by [`Extractor`], decoded to RGBA [`ImageData`], and
//! handed to the renderer for GPU compositing.

pub mod extract;
pub mod image;
pub mod iterm;
pub mod kitty;
pub mod sixel;

pub use extract::{Chunk, Extractor};
pub use image::ImageData;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_plain_text_through() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"hello world");
        assert_eq!(chunks.len(), 1);
        match &chunks[0] {
            Chunk::Pass(b) => assert_eq!(b, b"hello world"),
            _ => panic!("expected pass"),
        }
    }

    #[test]
    fn extracts_iterm_image() {
        // 1x1 red PNG.
        let png = base64_png();
        let seq = format!("\x1b]1337;File=inline=1:{png}\x07");
        let mut e = Extractor::new();
        let chunks = e.feed(seq.as_bytes());
        assert!(
            chunks.iter().any(|c| matches!(c, Chunk::Image(_))),
            "expected an image chunk, got {chunks:?}"
        );
    }

    #[test]
    fn non_image_osc_passes_through() {
        let mut e = Extractor::new();
        let chunks = e.feed(b"\x1b]0;my title\x07");
        assert!(chunks.iter().all(|c| matches!(c, Chunk::Pass(_))));
    }

    fn base64_png() -> String {
        use base64::Engine;
        let img = image::ImageData::new(1, 1, vec![255, 0, 0, 255]).unwrap();
        let mut buf = std::io::Cursor::new(Vec::new());
        let rgba = ::image::RgbaImage::from_raw(1, 1, img.rgba.as_ref().clone()).unwrap();
        ::image::DynamicImage::ImageRgba8(rgba)
            .write_to(&mut buf, ::image::ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
    }
}

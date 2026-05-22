//! Cycle 381 (Terminator parity, background-image Bucket-D sub-cycle 2):
//! background-image decode helper. Reads a user-supplied file path,
//! decodes via the `image` crate, returns RGBA bytes + dimensions
//! ready for wgpu texture upload.
//!
//! Supported formats (per the cycle-381 Cargo.toml features):
//!   - PNG (default, also used for kitty/iTerm2 inline images)
//!   - JPEG (most common for photo wallpapers)
//!   - WebP (modern web format)
//!   - BMP, GIF (legacy support)
//!
//! AVIF / HEIF / TIFF are NOT in the feature set — they pull
//! significant transitive deps (rav1e is multi-MB) for marginal
//! user value on a terminal-bg use case.
//!
//! Errors:
//!   - File not found / not readable → log::warn, return None.
//!   - Format unsupported → log::warn, return None.
//!   - Decode failed → log::warn, return None.
//!
//! Design doc: docs/TERMINATOR-BG-IMAGE-DESIGN.md sub-cycle 2.
//! Subsequent sub-cycles add the wgpu texture upload (3) + render
//! pass (4) + UV-mode variants (5+6) + blur shader (9).

use std::path::Path;

/// Decoded RGBA image ready for texture upload. Width/height in
/// pixels; data is tightly-packed RGBA8 (4 bytes per pixel).
#[derive(Debug, Clone)]
pub struct BgImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode a background image from disk. Returns None on any I/O,
/// format, or decode error (with a `log::warn` so users discover
/// the misconfiguration in their kettle logs).
///
/// Empty paths are handled silently (cfg.background_image defaults
/// to empty string when bg-image isn't configured).
pub fn decode_bg_image(path: &str) -> Option<BgImage> {
    if path.trim().is_empty() {
        return None;
    }
    let expanded = if let Some(stripped) = path.strip_prefix("~/") {
        std::env::var("HOME")
            .ok()
            .map(|h| format!("{h}/{stripped}"))
    } else {
        Some(path.to_string())
    };
    let p = match expanded {
        Some(s) => s,
        None => {
            log::warn!("background-image: can't expand ~ (HOME unset): {path}");
            return None;
        }
    };
    let p = Path::new(&p);
    if !p.exists() {
        log::warn!("background-image: file not found: {}", p.display());
        return None;
    }
    match image::open(p) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            Some(BgImage {
                width: w,
                height: h,
                rgba: rgba.into_raw(),
            })
        }
        Err(e) => {
            log::warn!("background-image decode {}: {e}", p.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_none() {
        // Cycle 381 drift guard. The cfg default for
        // background_image is an empty string; decode_bg_image
        // must NOT log::warn or panic on that case (it would
        // spam logs for every non-bg-image kettle run).
        assert!(decode_bg_image("").is_none());
        assert!(decode_bg_image("   ").is_none());
    }

    #[test]
    fn nonexistent_file_returns_none_with_warn() {
        // Should log a warning but not panic.
        assert!(decode_bg_image("/nonexistent/path/does-not-exist.png").is_none());
    }

    #[test]
    fn real_png_roundtrip() {
        // Cycle 392 (Terminator parity, bg-image Bucket-D sub-cycle 12):
        // acceptance test. Generate a known 8x4 RGBA PNG in-memory,
        // write to a temp file, decode via decode_bg_image, assert
        // the round-trip yields the expected dimensions + a
        // non-empty rgba buffer. Doesn't pixel-compare (PNG encoders
        // can vary on the precise byte layout); just confirms the
        // full path works.
        let dir = std::env::temp_dir();
        let path = dir.join("kettle-bg-image-cycle392-smoke.png");
        // Encode a small RGBA PNG using the image crate directly
        // (writer feature is on for kettle-render's image dep).
        let w = 8;
        let h = 4;
        let mut buf = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                buf.push((x * 32) as u8); // R
                buf.push((y * 64) as u8); // G
                buf.push(255); // B
                buf.push(255); // A
            }
        }
        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf).expect("rgba buffer");
        img.save(&path).expect("write png");
        let decoded = decode_bg_image(path.to_str().unwrap()).expect("decode");
        assert_eq!(decoded.width, 8);
        assert_eq!(decoded.height, 4);
        assert_eq!(decoded.rgba.len(), 8 * 4 * 4);
        // Spot-check: first pixel should be (0, 0, 255, 255) per
        // the encoding formula above.
        assert_eq!(&decoded.rgba[..4], &[0, 0, 255, 255]);
        let _ = std::fs::remove_file(&path);
    }
}

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

/// Cycle 584 decompression-bomb defense for the user-configured
/// `background-image` path. The bg-image source is a config-file
/// path (not attacker-controlled at the PTY layer), so the threat
/// model is weaker than the kitty graphics path that motivated
/// cycle 576 — but a malicious download masquerading as a 4K
/// wallpaper could still OOM kettle on launch via the same
/// PNG/JPEG/GIF/WebP/BMP decompression-bomb shape. Reuse the
/// same per-axis + total-alloc envelope as `kettle_vt::image`.
const MAX_BG_IMAGE_DIM: u32 = 8192;
const MAX_BG_IMAGE_BYTES: u64 = 256 * 1024 * 1024;

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
/// Cycle 396 (Terminator parity, bg-image Bucket-D sub-cycle 9):
/// CPU-side separable box blur (3-pass approximates a Gaussian
/// — same technique Photoshop / GIMP / CSS use for fast
/// "Gaussian blur" with much less compute). Applied to the
/// decoded RGBA buffer at load time so subsequent renders just
/// upload the blurred texture; no per-frame shader needed.
///
/// `radius` clamped to a sane range (1..=16). A 1080p image at
/// radius 8 blurs in ~30-50ms on a modern CPU — acceptable for a
/// one-time startup cost (background_image config rarely
/// changes mid-session).
///
/// A wgpu-side Gaussian shader (the docs/TERMINATOR-BG-IMAGE-
/// DESIGN.md sub-cycle 9 design) gives the same visual at
/// negligible per-frame cost; CPU-side is the bounded
/// foundation that ships the user-visible effect today.
fn box_blur(img: &mut BgImage, radius: u32) {
    if radius == 0 || img.width == 0 || img.height == 0 {
        return;
    }
    let r = radius.min(16);
    // Cycle 850 (audit): one scratch buffer reused across all six sub-passes
    // (the old code allocated a fresh full-image Vec in each — up to 6 × 256 MB
    // at MAX_BG_IMAGE_DIM). Each pass writes into `scratch` then swaps, so the
    // result always lands back in `img.rgba`.
    let mut scratch = vec![0u8; img.rgba.len()];
    for _ in 0..3 {
        box_blur_axis(img, r, true, &mut scratch);
        box_blur_axis(img, r, false, &mut scratch);
    }
}

/// One separable box-blur pass along the horizontal (`horizontal == true`) or
/// vertical axis, reading `img.rgba` and writing the blurred result back into
/// it via `scratch`.
///
/// Cycle 850 (audit): a sliding-window running sum makes the pass O(W·H)
/// regardless of radius (the old code summed `2r+1` samples *per pixel*,
/// O(W·H·R)). The divisor stays a constant `2r+1` — like the old brute force,
/// which counted every clamped sample — so the output is byte-identical. The
/// telescoping `sum += entering − leaving` holds even under edge clamping
/// because the clamp is applied per index consistently on both windows.
fn box_blur_axis(img: &mut BgImage, r: u32, horizontal: bool, scratch: &mut Vec<u8>) {
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let r = r as usize;
    let win = (2 * r + 1) as u32;
    // `len` = number of pixels along the blur axis; `lines` = the other axis.
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    // Index of pixel `pos` on line `line`, mapped back to a flat rgba offset.
    let offset = |line: usize, pos: usize| -> usize {
        let (x, y) = if horizontal { (pos, line) } else { (line, pos) };
        (y * w + x) * 4
    };
    let last = len as isize - 1;
    let src = &img.rgba;
    let dst = scratch.as_mut_slice();
    for line in 0..lines {
        // Initial window centered at pos 0 (the left/top side clamps to 0).
        let mut sum = [0u32; 4];
        for d in -(r as isize)..=(r as isize) {
            let i = offset(line, d.clamp(0, last) as usize);
            sum[0] += src[i] as u32;
            sum[1] += src[i + 1] as u32;
            sum[2] += src[i + 2] as u32;
            sum[3] += src[i + 3] as u32;
        }
        for pos in 0..len {
            let o = offset(line, pos);
            dst[o] = (sum[0] / win) as u8;
            dst[o + 1] = (sum[1] / win) as u8;
            dst[o + 2] = (sum[2] / win) as u8;
            dst[o + 3] = (sum[3] / win) as u8;
            if pos + 1 < len {
                // Slide to pos+1: add entering (pos+1+r), drop leaving (pos−r),
                // both clamped — keeps the constant `win` divisor.
                let entering = offset(
                    line,
                    (pos as isize + 1 + r as isize).clamp(0, last) as usize,
                );
                let leaving = offset(line, (pos as isize - r as isize).clamp(0, last) as usize);
                sum[0] = sum[0] + src[entering] as u32 - src[leaving] as u32;
                sum[1] = sum[1] + src[entering + 1] as u32 - src[leaving + 1] as u32;
                sum[2] = sum[2] + src[entering + 2] as u32 - src[leaving + 2] as u32;
                sum[3] = sum[3] + src[entering + 3] as u32 - src[leaving + 3] as u32;
            }
        }
    }
    std::mem::swap(&mut img.rgba, scratch);
}

/// Home directory for `~/` expansion. Cycle 916 (file-by-file audit): Windows is
/// the primary platform and sets `USERPROFILE`, not `HOME`, so a HOME-only probe
/// silently failed every `background-image = ~/wallpaper.png` there. Mirrors
/// kettle-core's `home_dir_fallback` (HOME -> USERPROFILE -> APPDATA).
fn home_dir() -> Option<String> {
    for key in ["HOME", "USERPROFILE", "APPDATA"] {
        if let Ok(v) = std::env::var(key)
            && !v.is_empty()
        {
            return Some(v);
        }
    }
    None
}

pub fn decode_bg_image(path: &str) -> Option<BgImage> {
    if path.trim().is_empty() {
        return None;
    }
    let expanded = if let Some(stripped) = path.strip_prefix("~/") {
        home_dir().map(|h| format!("{}/{stripped}", h.trim_end_matches(['/', '\\'])))
    } else {
        Some(path.to_string())
    };
    let p = match expanded {
        Some(s) => s,
        None => {
            log::warn!("background-image: can't expand ~ (no HOME/USERPROFILE): {path}");
            return None;
        }
    };
    let p = Path::new(&p);
    if !p.exists() {
        log::warn!("background-image: file not found: {}", p.display());
        return None;
    }
    // Cycle 584: bound the decoder against PNG/JPEG/GIF/WebP/BMP
    // decompression bombs. `image::open` is a convenience wrapper
    // that decodes the whole DynamicImage; an attacker-supplied
    // file with header dimensions of 2^31 × 2^31 would OOM during
    // decode without these limits. Same envelope as cycle 576's
    // PTY-layer fix in kettle-vt; surfaces a `log::warn` on
    // exceedance and returns None (the bg-image just doesn't load).
    let reader = match image::ImageReader::open(p) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("background-image open {}: {e}", p.display());
            return None;
        }
    };
    let reader = match reader.with_guessed_format() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("background-image format-detect {}: {e}", p.display());
            return None;
        }
    };
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_BG_IMAGE_DIM);
    limits.max_image_height = Some(MAX_BG_IMAGE_DIM);
    limits.max_alloc = Some(MAX_BG_IMAGE_BYTES);
    reader.limits(limits);
    match reader.decode() {
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

/// Cycle 396 public entry point: decode + optionally apply
/// background-blur. Callers that want the configured blur effect
/// (cfg.background_blur = true) use this; callers that need a
/// pristine image use `decode_bg_image` directly.
pub fn decode_bg_image_with_blur(path: &str, blur_radius: u32) -> Option<BgImage> {
    let mut img = decode_bg_image(path)?;
    if blur_radius > 0 {
        box_blur(&mut img, blur_radius);
    }
    Some(img)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pre-cycle-850 O(W·H·R) brute force, kept as a correctness oracle.
    fn box_blur_reference(img: &mut BgImage, radius: u32) {
        if radius == 0 || img.width == 0 || img.height == 0 {
            return;
        }
        let r = (radius.min(16)) as i32;
        let pass = |img: &mut BgImage, horizontal: bool| {
            let w = img.width as i32;
            let h = img.height as i32;
            let mut out = vec![0u8; img.rgba.len()];
            for y in 0..h {
                for x in 0..w {
                    let mut sum = [0u32; 4];
                    let mut count = 0u32;
                    for d in -r..=r {
                        let (xx, yy) = if horizontal {
                            ((x + d).clamp(0, w - 1), y)
                        } else {
                            (x, (y + d).clamp(0, h - 1))
                        };
                        let i = (yy as usize * w as usize + xx as usize) * 4;
                        sum[0] += img.rgba[i] as u32;
                        sum[1] += img.rgba[i + 1] as u32;
                        sum[2] += img.rgba[i + 2] as u32;
                        sum[3] += img.rgba[i + 3] as u32;
                        count += 1;
                    }
                    let o = (y as usize * w as usize + x as usize) * 4;
                    out[o] = (sum[0] / count) as u8;
                    out[o + 1] = (sum[1] / count) as u8;
                    out[o + 2] = (sum[2] / count) as u8;
                    out[o + 3] = (sum[3] / count) as u8;
                }
            }
            img.rgba = out;
        };
        for _ in 0..3 {
            pass(img, true);
            pass(img, false);
        }
    }

    /// Cycle 850 drift guard: the O(W·H) sliding-window blur must be
    /// byte-identical to the O(W·H·R) brute force it replaced, across odd/even
    /// dimensions, single-row/column degenerates, and a radius larger than the
    /// image (the clamping edge case).
    #[test]
    fn box_blur_matches_reference_brute_force() {
        for (w, h) in [(7usize, 5usize), (2, 2), (1, 9), (9, 1), (16, 16)] {
            let mut rgba = Vec::with_capacity(w * h * 4);
            for y in 0..h {
                for x in 0..w {
                    for c in 0..4 {
                        rgba.push(((x * 31 + y * 17 + c * 7 + 13) % 256) as u8);
                    }
                }
            }
            for r in [1u32, 2, 3, 16] {
                let mut a = BgImage {
                    width: w as u32,
                    height: h as u32,
                    rgba: rgba.clone(),
                };
                let mut b = a.clone();
                box_blur(&mut a, r);
                box_blur_reference(&mut b, r);
                assert_eq!(a.rgba, b.rgba, "blur mismatch at {w}x{h} r={r}");
            }
        }
    }

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
        // Cycle 592: PID + nanos in the filename so parallel `cargo
        // test` runs and CI-runner concurrency don't race on a shared
        // /tmp path. Matches the pattern used in session::tests.
        let path = std::env::temp_dir().join(format!(
            "kettle-bg-image-cycle392-smoke-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
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

    /// Cycle 584 drift guard: bg-image decoder must reject a PNG
    /// whose dimensions exceed `MAX_BG_IMAGE_DIM`, even though the
    /// `image` crate is happy to keep decoding. Encodes a tiny
    /// 8193 × 1 PNG (one px past the per-axis cap, ~32 KB on disk)
    /// and asserts decode_bg_image returns None. Catches the
    /// regression where a future refactor drops `reader.limits(...)`.
    #[test]
    fn rejects_oversized_dimensions() {
        let w = (MAX_BG_IMAGE_DIM + 1) as usize;
        let h = 1usize;
        // Black RGBA: w * 1 * 4 = ~32 KB at width 8193.
        let buf = vec![0u8; w * h * 4];
        let img = image::RgbaImage::from_raw(w as u32, h as u32, buf).expect("rgba buffer");
        // Cycle 592: PID + nanos so parallel test runs don't race.
        let path = std::env::temp_dir().join(format!(
            "kettle-bg-image-cycle584-oversize-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        img.save(&path).expect("write oversize png");
        assert!(
            decode_bg_image(path.to_str().unwrap()).is_none(),
            "decode_bg_image must reject width {w} (cap {MAX_BG_IMAGE_DIM})"
        );
        let _ = std::fs::remove_file(&path);
    }
}

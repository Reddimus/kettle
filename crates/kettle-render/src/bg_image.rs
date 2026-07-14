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

/// Cycle 584 decompression-bomb defense for the user-configured
/// `background-image` path. The bg-image source is a config-file
/// path (not attacker-controlled at the PTY layer), so the threat
/// model is weaker than the kitty graphics path that motivated
/// cycle 576 — but a malicious download masquerading as a 4K
/// wallpaper could still OOM kettle on launch via the same
/// PNG/JPEG/GIF/WebP/BMP decompression-bomb shape. Reuse the
/// same per-axis + total-alloc envelope as `kettle_vt::image`.
const MAX_BG_IMAGE_DIM: u32 = 8192;
const MAX_BG_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Animated background bounds (v2.21.x). A multi-frame background (animated
/// GIF / APNG / animated WebP) decodes EVERY frame's RGBA up front, so the
/// envelope is the SUM across frames, not one frame: a 1080p × 200-frame GIF
/// would be ~1.6 GB. Cap the total decoded bytes AND the frame count; on
/// exceedance the decoder truncates to the frames that fit (≥ 1) with a
/// `log::warn`, degrading gracefully to a shorter loop / first-frame-static
/// rather than OOMing on launch. A 0 ms inter-frame gap (common in
/// "play as fast as possible" GIFs) is clamped up so the loop has a real
/// period and the render tick (capped at ~30 fps) governs the actual wake rate.
const MAX_BG_ANIM_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BG_FRAMES: usize = 128;
const MIN_BG_FRAME_GAP_MS: u32 = 20;

/// Decoded RGBA image ready for texture upload. Width/height in
/// pixels; data is tightly-packed RGBA8 (4 bytes per pixel).
#[derive(Debug, Clone)]
pub struct BgImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One frame of an animated background: the decoded image plus its dwell time
/// (ms) before the next frame. A still image decodes to a single `BgFrame`
/// with `gap_ms = 0`, so callers handle still + animated uniformly.
#[derive(Debug, Clone)]
pub struct BgFrame {
    pub image: BgImage,
    pub gap_ms: u32,
}

/// The frame index to display right now for an animated background, given each
/// frame's dwell `gaps` (ms) and the wall-clock `elapsed_ms` since playback
/// started. Pure + loops forever. Mirrors the kitty animation clock
/// (`kettle_vt::kitty::current_frame`) but with the background's simpler
/// always-looping semantics (no kitty run-state). Drift-tested below.
pub fn bg_current_frame(gaps: &[u32], elapsed_ms: u128) -> usize {
    if gaps.len() <= 1 {
        return 0;
    }
    let total: u128 = gaps.iter().map(|&g| g as u128).sum();
    if total == 0 {
        return 0;
    }
    let mut t = elapsed_ms % total;
    for (i, &g) in gaps.iter().enumerate() {
        let g = g as u128;
        if t < g {
            return i;
        }
        t -= g;
    }
    gaps.len() - 1
}

/// Milliseconds until the displayed frame index next changes, given each
/// frame's dwell `gaps` (ms) and the wall-clock `elapsed_ms` since playback
/// started. The companion to [`bg_current_frame`]: the render loop sleeps this
/// long, then wakes to show the next frame, so an N-fps animated background
/// repaints N×/s instead of at a fixed 30 fps (the v2.23.1 animated-idle fix).
/// `None` for a still image (≤ 1 frame) or all-zero gaps. Floored at 16 ms so a
/// degenerate fast GIF can't drive the loop past ~60 fps. Pure; unit-tested.
pub fn bg_next_frame_ms(gaps: &[u32], elapsed_ms: u128) -> Option<u64> {
    if gaps.len() <= 1 {
        return None;
    }
    let total: u128 = gaps.iter().map(|&g| g as u128).sum();
    if total == 0 {
        return None;
    }
    let mut t = elapsed_ms % total;
    for &g in gaps {
        let g = g as u128;
        if t < g {
            return Some(((g - t) as u64).max(16));
        }
        t -= g;
    }
    Some(16)
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
fn box_blur(img: &mut BgImage, radius: u32) -> bool {
    if radius == 0 || img.width == 0 || img.height == 0 {
        return true;
    }
    let r = radius.min(16);
    // Cycle 850 (audit): one scratch buffer reused across all six sub-passes
    // (the old code allocated a fresh full-image Vec in each — up to 6 × 256 MB
    // at MAX_BG_IMAGE_DIM). Each pass writes into `scratch` then swaps, so the
    // result always lands back in `img.rgba`.
    let mut scratch = Vec::new();
    if scratch.try_reserve_exact(img.rgba.len()).is_err() {
        return false;
    }
    scratch.resize(img.rgba.len(), 0);
    for _ in 0..3 {
        box_blur_axis(img, r, true, &mut scratch);
        box_blur_axis(img, r, false, &mut scratch);
    }
    true
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

/// Resolve a configured `background-image` path: trims, expands a leading `~/`
/// via [`home_dir`], and confirms the file exists. Shared by the single-frame
/// and animated decoders so `~/` handling + the not-found warning live in one
/// place. Returns `None` (silently for an empty path; with a `log::warn`
/// otherwise) on any failure.
fn resolve_bg_path(path: &str) -> Option<std::path::PathBuf> {
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
    let pb = std::path::PathBuf::from(&p);
    if !pb.exists() {
        log::warn!("background-image: file not found: {}", pb.display());
        return None;
    }
    Some(pb)
}

pub fn decode_bg_image(path: &str) -> Option<BgImage> {
    let pb = resolve_bg_path(path)?;
    let p = pb.as_path();
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
    if blur_radius > 0 && !box_blur(&mut img, blur_radius) {
        log::warn!("background-image: insufficient memory budget for blur scratch");
        return None;
    }
    Some(img)
}

/// One frame, wrapped so still + animated callers are uniform.
fn single_frame(path: &str) -> Option<Vec<BgFrame>> {
    decode_bg_image(path).map(|image| vec![BgFrame { image, gap_ms: 0 }])
}

/// Decode a background image into ONE-OR-MORE frames (v2.21.x animated
/// background). Animated GIF / APNG / animated WebP yield every frame (bounded
/// by `MAX_BG_ANIM_BYTES` + `MAX_BG_FRAMES`, truncating gracefully on
/// exceedance); a still image, or a non-animated GIF/PNG/WebP, yields exactly
/// one frame (`gap_ms = 0`). Returns `None` on any I/O/format/decode error
/// (with a `log::warn`), matching [`decode_bg_image`]. Frames decode once here;
/// the render loop only swaps the already-decoded RGBA per the playback clock.
pub fn decode_bg_image_frames(path: &str) -> Option<Vec<BgFrame>> {
    use image::{AnimationDecoder, ImageDecoder, ImageFormat};
    let pb = resolve_bg_path(path)?;
    let reader = match image::ImageReader::open(&pb).and_then(|r| r.with_guessed_format()) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("background-image open {}: {e}", pb.display());
            return None;
        }
    };
    let format = reader.format();
    // Only GIF / WebP / PNG (APNG) can be multi-frame; everything else is still.
    if !matches!(
        format,
        Some(ImageFormat::Gif | ImageFormat::WebP | ImageFormat::Png)
    ) {
        return single_frame(path);
    }
    let inner = reader.into_inner();
    // Build the format-specific animation frame iterator, or fall back to the
    // single-frame path for a non-animated GIF/PNG/WebP (or any decoder error).
    let (mut frames, frame_upper_bound) = match format {
        Some(ImageFormat::Gif) => match image::codecs::gif::GifDecoder::new(inner) {
            Ok(d) if decoder_dimensions_fit(d.dimensions()) => {
                let bytes = decoder_frame_bytes(d.dimensions())?;
                (d.into_frames(), bytes)
            }
            Ok(d) => {
                let (w, h) = d.dimensions();
                log::warn!("background-image: animation canvas {w}x{h} exceeds resource limits");
                return None;
            }
            Err(e) => {
                log::warn!("background-image gif {}: {e}", pb.display());
                return None;
            }
        },
        Some(ImageFormat::WebP) => match image::codecs::webp::WebPDecoder::new(inner) {
            Ok(d) if d.has_animation() && decoder_dimensions_fit(d.dimensions()) => {
                let bytes = decoder_frame_bytes(d.dimensions())?;
                (d.into_frames(), bytes)
            }
            Ok(d) if d.has_animation() => {
                let (w, h) = d.dimensions();
                log::warn!("background-image: animation canvas {w}x{h} exceeds resource limits");
                return None;
            }
            Ok(_) => return single_frame(path),
            Err(e) => {
                log::warn!("background-image webp {}: {e}", pb.display());
                return None;
            }
        },
        Some(ImageFormat::Png) => match image::codecs::png::PngDecoder::new(inner) {
            Ok(d) if decoder_dimensions_fit(d.dimensions()) => {
                let bytes = decoder_frame_bytes(d.dimensions())?;
                match d.is_apng() {
                    Ok(true) => match d.apng() {
                        Ok(a) => (a.into_frames(), bytes),
                        Err(e) => {
                            log::warn!("background-image apng {}: {e}", pb.display());
                            return None;
                        }
                    },
                    // Plain (non-animated) PNG → still.
                    _ => return single_frame(path),
                }
            }
            Ok(d) => {
                let (w, h) = d.dimensions();
                log::warn!("background-image: animation canvas {w}x{h} exceeds resource limits");
                return None;
            }
            Err(_) => return single_frame(path),
        },
        _ => return single_frame(path),
    };
    // Collect bounded: total decoded RGBA ≤ MAX_BG_ANIM_BYTES, ≤ MAX_BG_FRAMES,
    // each frame ≤ MAX_BG_IMAGE_DIM per axis. Truncate (≥ 1 frame) on exceedance.
    let mut out: Vec<BgFrame> = Vec::new();
    let mut total: u64 = 0;
    loop {
        if out.len() >= MAX_BG_FRAMES {
            log::warn!("background-image: animation truncated at {MAX_BG_FRAMES} frames");
            break;
        }
        if total
            .checked_add(frame_upper_bound)
            .is_none_or(|next| next > MAX_BG_ANIM_BYTES)
        {
            log::warn!(
                "background-image: animation exceeds {} MB budget, truncating at {} frames",
                MAX_BG_ANIM_BYTES / (1024 * 1024),
                out.len()
            );
            break;
        }
        // Check the canvas-sized upper bound before requesting the next frame:
        // animation decoders allocate during `next()`, so a `for` loop would
        // decode one over-budget frame before the post-decode size check.
        let Some(fr) = frames.next() else {
            break;
        };
        let fr = match fr {
            Ok(f) => f,
            Err(e) => {
                log::warn!("background-image frame decode {}: {e}", pb.display());
                break;
            }
        };
        let (numer, denom) = fr.delay().numer_denom_ms();
        let gap = numer
            .checked_div(denom)
            .unwrap_or(numer)
            .max(MIN_BG_FRAME_GAP_MS);
        let buf = fr.into_buffer();
        let (w, h) = buf.dimensions();
        if w > MAX_BG_IMAGE_DIM || h > MAX_BG_IMAGE_DIM {
            log::warn!("background-image: frame {w}x{h} exceeds {MAX_BG_IMAGE_DIM}px cap");
            break;
        }
        let Some(bytes) = u64::from(w)
            .checked_mul(u64::from(h))
            .and_then(|n| n.checked_mul(4))
        else {
            break;
        };
        let Some(next_total) = total.checked_add(bytes) else {
            break;
        };
        if bytes > MAX_BG_IMAGE_BYTES || next_total > MAX_BG_ANIM_BYTES {
            log::warn!(
                "background-image: animation exceeds {} MB budget, truncating at {} frames",
                MAX_BG_ANIM_BYTES / (1024 * 1024),
                out.len()
            );
            break;
        }
        total = next_total;
        out.push(BgFrame {
            image: BgImage {
                width: w,
                height: h,
                rgba: buf.into_raw(),
            },
            gap_ms: gap,
        });
    }
    if out.is_empty() {
        // Animated path yielded nothing usable — last-ditch single decode.
        return single_frame(path);
    }
    Some(out)
}

/// [`decode_bg_image_frames`] + the configured `background-blur` applied to
/// every frame at load time (so renders just upload the blurred frame; no
/// per-frame shader). Blur is bounded work per frame; the frame-count cap above
/// keeps the total bounded.
pub fn decode_bg_image_frames_with_blur(path: &str, blur_radius: u32) -> Option<Vec<BgFrame>> {
    let mut frames = decode_bg_image_frames(path)?;
    if blur_radius > 0 {
        for f in &mut frames {
            if !box_blur(&mut f.image, blur_radius) {
                log::warn!("background-image: insufficient memory budget for blur scratch");
                return None;
            }
        }
    }
    Some(frames)
}

fn decoder_dimensions_fit((width, height): (u32, u32)) -> bool {
    decoder_frame_bytes((width, height)).is_some()
}

fn decoder_frame_bytes((width, height): (u32, u32)) -> Option<u64> {
    if width == 0 || height == 0 || width > MAX_BG_IMAGE_DIM || height > MAX_BG_IMAGE_DIM {
        return None;
    }
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    (bytes <= MAX_BG_IMAGE_BYTES).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_frame_envelope_accepts_limit_and_rejects_one_past() {
        assert!(decoder_dimensions_fit((8192, 2048)));
        assert!(!decoder_dimensions_fit((8192, 2049)));
        assert!(!decoder_dimensions_fit((MAX_BG_IMAGE_DIM + 1, 1)));
        assert_eq!(MAX_BG_IMAGE_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_BG_ANIM_BYTES, 128 * 1024 * 1024);
        assert_eq!(MAX_BG_FRAMES, 128);
    }

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

    /// v2.21.x: the animated-background clock loops through frames per their
    /// dwell gaps and never indexes out of bounds, including the degenerate
    /// single-frame / zero-gap cases.
    #[test]
    fn bg_current_frame_loops_by_gap() {
        // Single frame (or none) → always frame 0.
        assert_eq!(bg_current_frame(&[], 0), 0);
        assert_eq!(bg_current_frame(&[100], 999_999), 0);
        // Three frames, 100ms each → total 300ms period.
        let gaps = [100u32, 100, 100];
        assert_eq!(bg_current_frame(&gaps, 0), 0);
        assert_eq!(bg_current_frame(&gaps, 50), 0);
        assert_eq!(bg_current_frame(&gaps, 100), 1);
        assert_eq!(bg_current_frame(&gaps, 250), 2);
        assert_eq!(bg_current_frame(&gaps, 300), 0); // wrapped
        assert_eq!(bg_current_frame(&gaps, 1_000_000), 1); // still in range, looped
        // Uneven gaps.
        let uneven = [40u32, 200, 60];
        assert_eq!(bg_current_frame(&uneven, 0), 0);
        assert_eq!(bg_current_frame(&uneven, 40), 1);
        assert_eq!(bg_current_frame(&uneven, 239), 1);
        assert_eq!(bg_current_frame(&uneven, 240), 2);
        // All-zero gaps must not divide-by-zero / panic.
        assert_eq!(bg_current_frame(&[0, 0, 0], 12345), 0);
    }

    /// v2.23.1 animated-idle fix: the wake interval is the time to the NEXT frame
    /// boundary, so the loop ticks at the GIF's fps, not a fixed 30 fps.
    #[test]
    fn bg_next_frame_ms_wakes_at_frame_boundaries() {
        // Still image / single frame / empty → no animation tick.
        assert_eq!(bg_next_frame_ms(&[], 0), None);
        assert_eq!(bg_next_frame_ms(&[125], 999), None);
        assert_eq!(bg_next_frame_ms(&[0, 0, 0], 5), None);
        // Uniform 125 ms (8 fps): at t=0 the current frame has 125 ms left.
        let g = [125u32, 125, 125];
        assert_eq!(bg_next_frame_ms(&g, 0), Some(125));
        assert_eq!(bg_next_frame_ms(&g, 100), Some(25)); // 25 ms left in frame 0
        assert_eq!(bg_next_frame_ms(&g, 125), Some(125)); // just entered frame 1
        assert_eq!(bg_next_frame_ms(&g, 375), Some(125)); // wrapped to frame 0
        // Uneven gaps resolve per-frame.
        let u = [40u32, 200, 60];
        assert_eq!(bg_next_frame_ms(&u, 0), Some(40));
        assert_eq!(bg_next_frame_ms(&u, 40), Some(200));
        assert_eq!(bg_next_frame_ms(&u, 239), Some(16)); // 1 ms left in frame 1 → floored to 16
        // Floor: never below 16 ms (a 5 ms gap would otherwise drive >60 fps).
        assert_eq!(bg_next_frame_ms(&[5, 5], 0), Some(16));
    }

    /// v2.21.x: a still PNG decodes to exactly one frame via the animated entry
    /// point, so still + animated callers are uniform.
    #[test]
    fn still_png_decodes_to_one_frame() {
        let path = std::env::temp_dir().join(format!(
            "kettle-bg-frames-still-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let img = image::RgbaImage::from_raw(4, 2, vec![200u8; 4 * 2 * 4]).expect("rgba");
        img.save(&path).expect("write png");
        let frames = decode_bg_image_frames(path.to_str().unwrap()).expect("decode frames");
        assert_eq!(frames.len(), 1, "a still PNG must yield exactly one frame");
        assert_eq!(frames[0].image.width, 4);
        assert_eq!(frames[0].gap_ms, 0);
        let _ = std::fs::remove_file(&path);
    }

    /// v2.21.x: an animated GIF decodes to multiple frames with per-frame gaps,
    /// proving the animated-background path (and that the clock would cycle it).
    #[test]
    fn animated_gif_decodes_to_multiple_frames() {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame, RgbaImage};
        let path = std::env::temp_dir().join(format!(
            "kettle-bg-frames-anim-{}-{}.gif",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        {
            let file = std::fs::File::create(&path).expect("create gif");
            let mut enc = GifEncoder::new(file);
            enc.set_repeat(Repeat::Infinite).ok();
            for c in [40u8, 120, 200] {
                let buf = RgbaImage::from_raw(4, 4, vec![c; 4 * 4 * 4]).expect("rgba");
                let frame = Frame::from_parts(buf, 0, 0, Delay::from_numer_denom_ms(80, 1));
                enc.encode_frame(frame).expect("encode frame");
            }
        }
        let frames = decode_bg_image_frames(path.to_str().unwrap()).expect("decode anim");
        assert!(
            frames.len() >= 2,
            "an animated GIF must yield >1 frame; got {}",
            frames.len()
        );
        assert!(
            frames.iter().all(|f| f.gap_ms >= MIN_BG_FRAME_GAP_MS),
            "every frame gap must be clamped to the floor"
        );
        let _ = std::fs::remove_file(&path);
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

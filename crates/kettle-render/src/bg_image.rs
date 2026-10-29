//! Bounded still and GIF/APNG/WebP background decoding for GPU upload.
//! Still formats follow enabled `image` features; see docs/BACKGROUNDS.md.

/// Bound dimensions and decoded RGBA storage before materializing a wallpaper.
const MAX_BG_IMAGE_DIM: u32 = 8192;
const MAX_BG_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Total decoded animation storage and frame count. Keep the prefix that fits;
/// clamp frame gaps so malformed delays cannot drive continuous redraws.
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
/// repaints N×/s instead of at a fixed 30 fps.
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

/// Blur premultiplied linear RGBA with three separable box passes.
/// Radius is capped at 16; allocation failure leaves the image unchanged.
fn box_blur(img: &mut BgImage, radius: u32) -> bool {
    if radius == 0 || img.width == 0 || img.height == 0 {
        return true;
    }
    let Some(bytes) = decoder_frame_bytes((img.width, img.height)) else {
        return false;
    };
    if usize::try_from(bytes).ok() != Some(img.rgba.len()) {
        return false;
    }
    let r = radius.min(16);
    // Two f32 buffers use at most eight times the bounded RGBA byte count.
    // Reserve both before modifying the image.
    let mut linear = Vec::new();
    let mut scratch = Vec::new();
    if linear.try_reserve_exact(img.rgba.len()).is_err()
        || scratch.try_reserve_exact(img.rgba.len()).is_err()
    {
        return false;
    }
    linear.resize(img.rgba.len(), 0.0);
    scratch.resize(img.rgba.len(), 0.0);
    premultiply_linear_rgba8(&img.rgba, &mut linear);
    for _ in 0..3 {
        box_blur_axis(&mut linear, (img.width, img.height), r, true, &mut scratch);
        box_blur_axis(&mut linear, (img.width, img.height), r, false, &mut scratch);
    }
    unpremultiply_linear_rgba8(&linear, &mut img.rgba);
    true
}

fn premultiply_linear_rgba8(rgba: &[u8], out: &mut [f32]) {
    for (pixel, dst) in rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(out.as_chunks_mut::<4>().0)
    {
        let alpha = pixel[3] as f64 / 255.0;
        for (channel, &encoded) in dst[..3].iter_mut().zip(&pixel[..3]) {
            let srgb = encoded as f64 / 255.0;
            let linear = if srgb <= 0.04045 {
                srgb / 12.92
            } else {
                ((srgb + 0.055) / 1.055).powf(2.4)
            };
            *channel = (linear * alpha) as f32;
        }
        dst[3] = alpha as f32;
    }
}

fn unpremultiply_linear_rgba8(linear: &[f32], rgba: &mut [u8]) {
    for (src, pixel) in linear
        .as_chunks::<4>()
        .0
        .iter()
        .zip(rgba.as_chunks_mut::<4>().0)
    {
        let alpha = f64::from(src[3]).clamp(0.0, 1.0);
        pixel[3] = (alpha * 255.0).round() as u8;
        if alpha == 0.0 {
            pixel[..3].fill(0);
            continue;
        }
        for (channel, &premultiplied) in pixel[..3].iter_mut().zip(&src[..3]) {
            let linear = (f64::from(premultiplied) / alpha).clamp(0.0, 1.0);
            let srgb = if linear <= 0.003_130_8 {
                linear * 12.92
            } else {
                1.055 * linear.powf(1.0 / 2.4) - 0.055
            };
            *channel = (srgb * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// One separable box-blur pass along the horizontal (`horizontal == true`) or
/// vertical axis, reading premultiplied linear RGBA and swapping in `scratch`.
///
/// A sliding-window running sum makes the pass O(W·H) regardless of radius.
/// The divisor is a constant `2r+1` because every clamped edge sample counts,
/// so the output is byte-identical to the brute-force reference in the tests.
/// The telescoping `sum += entering − leaving` holds even under edge clamping
/// because the clamp is applied per index consistently on both windows.
fn box_blur_axis(
    rgba: &mut Vec<f32>,
    (width, height): (u32, u32),
    r: u32,
    horizontal: bool,
    scratch: &mut Vec<f32>,
) {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let r = r as usize;
    let win = (2 * r + 1) as f64;
    // `len` = number of pixels along the blur axis; `lines` = the other axis.
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    // Index of pixel `pos` on line `line`, mapped back to a flat rgba offset.
    let offset = |line: usize, pos: usize| -> usize {
        let (x, y) = if horizontal { (pos, line) } else { (line, pos) };
        (y * w + x) * 4
    };
    let last = len as isize - 1;
    let src = &*rgba;
    let dst = scratch.as_mut_slice();
    for line in 0..lines {
        // Initial window centered at pos 0 (the left/top side clamps to 0).
        let mut sum = [0.0f64; 4];
        for d in -(r as isize)..=(r as isize) {
            let i = offset(line, d.clamp(0, last) as usize);
            sum[0] += f64::from(src[i]);
            sum[1] += f64::from(src[i + 1]);
            sum[2] += f64::from(src[i + 2]);
            sum[3] += f64::from(src[i + 3]);
        }
        for pos in 0..len {
            let o = offset(line, pos);
            dst[o] = (sum[0] / win) as f32;
            dst[o + 1] = (sum[1] / win) as f32;
            dst[o + 2] = (sum[2] / win) as f32;
            dst[o + 3] = (sum[3] / win) as f32;
            if pos + 1 < len {
                // Slide to pos+1: add entering (pos+1+r), drop leaving (pos−r),
                // both clamped — keeps the constant `win` divisor.
                let entering = offset(
                    line,
                    (pos as isize + 1 + r as isize).clamp(0, last) as usize,
                );
                let leaving = offset(line, (pos as isize - r as isize).clamp(0, last) as usize);
                sum[0] = sum[0] + f64::from(src[entering]) - f64::from(src[leaving]);
                sum[1] = sum[1] + f64::from(src[entering + 1]) - f64::from(src[leaving + 1]);
                sum[2] = sum[2] + f64::from(src[entering + 2]) - f64::from(src[leaving + 2]);
                sum[3] = sum[3] + f64::from(src[entering + 3]) - f64::from(src[leaving + 3]);
            }
        }
    }
    std::mem::swap(rgba, scratch);
}

/// Home directory for `~/` expansion. Windows sets `USERPROFILE`, not `HOME`,
/// so a HOME-only probe would fail every `background-image = ~/wallpaper.png`
/// there. Mirrors kettle-core's `home_dir_fallback` (HOME -> USERPROFILE -> APPDATA).
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

/// Expand leading `~/` through [`home_dir`] and require an existing path.
/// Empty input returns `None` silently; other failures log a warning.
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
    // Apply pixel and allocation limits before decoding user-provided data.
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
    let mut reader = if reader.format() == Some(image::ImageFormat::WebP) {
        let mut inner = reader.into_inner();
        if let Err(e) = validate_webp_headers(&mut inner) {
            log::warn!("background-image webp {}: {e}", p.display());
            return None;
        }
        image::ImageReader::with_format(inner, image::ImageFormat::WebP)
    } else {
        reader
    };
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_BG_IMAGE_DIM);
    limits.max_image_height = Some(MAX_BG_IMAGE_DIM);
    limits.max_alloc = Some(MAX_BG_IMAGE_BYTES);
    reader.limits(limits.clone());
    match reader.into_decoder().and_then(|mut decoder| {
        use image::ImageDecoder;
        if decoder_frame_bytes(decoder.dimensions()).is_none() {
            return Err(image::ImageError::Limits(
                image::error::LimitError::from_kind(
                    image::error::LimitErrorKind::InsufficientMemory,
                ),
            ));
        }
        // Preserve ImageReader::decode's accounting for the decoded buffer.
        limits.reserve(decoder.total_bytes())?;
        decoder.set_limits(limits)?;
        image::DynamicImage::from_decoder(decoder)
    }) {
        Ok(img) => {
            let rgba = img.into_rgba8();
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

/// Public entry point: decode + optionally apply
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

/// Decode a background image into ONE-OR-MORE frames. Animated GIF / APNG /
/// animated WebP yield every frame (bounded by `MAX_BG_ANIM_BYTES` +
/// `MAX_BG_FRAMES`, truncating gracefully on exceedance); a still image, or a
/// non-animated GIF/PNG/WebP, yields exactly
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
    // Build the format-specific animation frame iterator. A still PNG/WebP (or
    // a PNG the decoder rejects) takes the single-frame path; a still GIF
    // decodes as a one-frame animation.
    let (mut frames, frame_upper_bound) = match format {
        Some(ImageFormat::Gif) => match image::codecs::gif::GifDecoder::new(inner) {
            Ok(mut d) if decoder_dimensions_fit(d.dimensions()) => {
                let (w, h) = d.dimensions();
                let bytes = decoder_frame_bytes((w, h))?;
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(w);
                limits.max_image_height = Some(h);
                // Previous canvas, decoded subframe, and composited output.
                limits.max_alloc = Some(bytes.checked_mul(3)?);
                if let Err(e) = d.set_limits(limits) {
                    log::warn!("background-image gif {}: {e}", pb.display());
                    return None;
                }
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
        Some(ImageFormat::WebP) => match background_webp_decoder(inner) {
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
        Some(ImageFormat::Png) => match background_png_decoder(inner) {
            Ok(mut d) if decoder_dimensions_fit(d.dimensions()) => {
                let bytes = decoder_frame_bytes(d.dimensions())?;
                // Two canvases, a potentially 16-bit raw frame, and RGBA conversion.
                let mut scratch = image::Limits::default();
                scratch.max_alloc = Some(bytes.checked_mul(5)?);
                d.set_limits(scratch).ok()?;
                match d.is_apng() {
                    Ok(true) => match d.apng() {
                        Ok(a) => (a.into_frames(), bytes),
                        Err(e) => {
                            log::warn!("background-image apng {}: {e}", pb.display());
                            return None;
                        }
                    },
                    // Plain (non-animated) PNG → still.
                    _ => {
                        drop(d);
                        return single_frame(path);
                    }
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
        // Do not retry a GIF through a decoder with different frame limits.
        if format == Some(ImageFormat::Gif) {
            return None;
        }
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

fn validate_webp_headers<R: std::io::Read + std::io::Seek>(reader: &mut R) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind, SeekFrom};
    let invalid = || {
        Error::new(
            ErrorKind::InvalidData,
            "invalid or oversized WebP frame header",
        )
    };
    let end = reader.seek(SeekFrom::End(0))?;
    reader.rewind()?;
    let mut header = [0; 12];
    reader.read_exact(&mut header)?;
    if &header[..4] != b"RIFF"
        || &header[8..] != b"WEBP"
        || u64::from(u32::from_le_bytes(header[4..8].try_into().unwrap())) + 8 != end
    {
        return Err(invalid());
    }
    let mut parser = WebpHeaders {
        reader,
        chunks_left: 4096,
    };
    let (tag, start, size, _) = parser.chunk(12, end).ok_or_else(invalid)?;
    let canvas = parser.dimensions(tag, start, size).ok_or_else(invalid)?;
    if !decoder_dimensions_fit(canvas) {
        return Err(invalid());
    }
    parser.walk(12, end, canvas, None).ok_or_else(invalid)?;
    parser.reader.rewind()
}

struct WebpHeaders<'a, R> {
    reader: &'a mut R,
    chunks_left: usize,
}

impl<R: std::io::Read + std::io::Seek> WebpHeaders<'_, R> {
    fn read<const N: usize>(&mut self, at: u64) -> Option<[u8; N]> {
        self.reader.seek(std::io::SeekFrom::Start(at)).ok()?;
        let mut bytes = [0; N];
        self.reader.read_exact(&mut bytes).ok()?;
        Some(bytes)
    }

    fn chunk(&mut self, at: u64, end: u64) -> Option<([u8; 4], u64, u64, u64)> {
        self.chunks_left = self.chunks_left.checked_sub(1)?;
        let start = at.checked_add(8).filter(|&start| start <= end)?;
        let header = self.read::<8>(at)?;
        let size = u64::from(u32::from_le_bytes(header[4..].try_into().ok()?));
        let next = start.checked_add(size)?.checked_add(size % 2)?;
        (next <= end).then_some((header[..4].try_into().ok()?, start, size, next))
    }

    fn dimensions(&mut self, tag: [u8; 4], start: u64, size: u64) -> Option<(u32, u32)> {
        match &tag {
            b"VP8 " if size >= 10 => {
                let h = self.read::<10>(start)?;
                if h[0] & 1 != 0 || h[3..6] != [0x9d, 1, 0x2a] {
                    return None;
                }
                Some((
                    u32::from(u16::from_le_bytes([h[6], h[7]]) & 0x3fff),
                    u32::from(u16::from_le_bytes([h[8], h[9]]) & 0x3fff),
                ))
            }
            b"VP8L" if size >= 5 => {
                let h = self.read::<5>(start)?;
                let bits = u32::from_le_bytes(h[1..].try_into().ok()?);
                if h[0] != 0x2f || bits >> 29 != 0 {
                    return None;
                }
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8X" if size == 10 => {
                let h = self.read::<10>(start)?;
                Some((webp_u24(&h[4..7]) + 1, webp_u24(&h[7..10]) + 1))
            }
            _ => None,
        }
    }

    fn walk(
        &mut self,
        mut at: u64,
        end: u64,
        canvas: (u32, u32),
        frame: Option<(u32, u32)>,
    ) -> Option<()> {
        let mut alpha = false;
        let mut image = false;
        while at < end {
            let (tag, start, size, next) = self.chunk(at, end)?;
            // ALPH has no stored dimensions; it inherits the enclosing frame's.
            // The decoder treats its next chunk as VP8 even if the tag differs.
            if frame.is_some() && (image || (alpha && &tag != b"VP8 ")) {
                return None;
            }
            match &tag {
                b"VP8X" if at == 12 && frame.is_none() => {}
                b"VP8 " | b"VP8L" => {
                    let dims = self.dimensions(tag, start, size)?;
                    if !decoder_dimensions_fit(dims) || dims != frame.unwrap_or(canvas) {
                        return None;
                    }
                    image = true;
                }
                b"ALPH" => {
                    let dims = frame.unwrap_or(canvas);
                    let h = (size >= 1).then(|| self.read::<1>(start)).flatten()?[0];
                    if !decoder_dimensions_fit(dims)
                        || h & 0xc0 != 0
                        || h & 3 > 1
                        || (h >> 4) & 3 > 1
                    {
                        return None;
                    }
                    if h & 3 == 0 && size - 1 < u64::from(dims.0) * u64::from(dims.1) {
                        return None;
                    }
                    alpha = true;
                }
                b"ANMF" if frame.is_none() && size >= 16 => {
                    let h = self.read::<16>(start)?;
                    let dims = (webp_u24(&h[6..9]) + 1, webp_u24(&h[9..12]) + 1);
                    if !decoder_dimensions_fit(dims)
                        || webp_u24(&h[..3]) * 2 + dims.0 > canvas.0
                        || webp_u24(&h[3..6]) * 2 + dims.1 > canvas.1
                    {
                        return None;
                    }
                    self.walk(start + 16, start + size, canvas, Some(dims))?;
                }
                b"ANMF" | b"VP8X" => return None,
                _ if frame.is_some() => return None,
                _ => {}
            }
            at = next;
        }
        (frame.is_none() || image).then_some(())
    }
}

fn webp_u24(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}

fn background_webp_decoder<R: std::io::BufRead + std::io::Seek>(
    mut reader: R,
) -> image::ImageResult<image::codecs::webp::WebPDecoder<R>> {
    validate_webp_headers(&mut reader)?;
    image::codecs::webp::WebPDecoder::new(reader)
}

fn background_png_decoder<R: std::io::BufRead + std::io::Seek>(
    reader: R,
) -> image::ImageResult<image::codecs::png::PngDecoder<R>> {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_BG_IMAGE_DIM);
    limits.max_image_height = Some(MAX_BG_IMAGE_DIM);
    limits.max_alloc = Some(MAX_BG_IMAGE_BYTES);
    image::codecs::png::PngDecoder::with_limits(reader, limits)
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

    fn webp_chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut chunk = tag.to_vec();
        chunk.extend_from_slice(&(data.len() as u32).to_le_bytes());
        chunk.extend_from_slice(data);
        if !data.len().is_multiple_of(2) {
            chunk.push(0);
        }
        chunk
    }

    fn animated_webp_header(frame: &[u8]) -> Vec<u8> {
        let mut body = b"WEBP".to_vec();
        body.extend(webp_chunk(b"VP8X", &[2, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
        body.extend(webp_chunk(b"ANIM", &[0; 6]));
        let mut anmf = vec![0; 16];
        anmf.extend_from_slice(frame);
        body.extend(webp_chunk(b"ANMF", &anmf));
        let mut file = b"RIFF".to_vec();
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend(body);
        file
    }

    #[test]
    fn webp_rejects_oversized_nested_header_before_decode() {
        let vp8 = webp_chunk(
            b"VP8 ",
            &[0x10, 0, 0, 0x9d, 1, 0x2a, 0xff, 0x3f, 0xff, 0x3f],
        );
        let file = animated_webp_header(&vp8);
        assert!(
            background_webp_decoder(std::io::Cursor::new(file)).is_err(),
            "tiny WebP canvas must reject oversized VP8 header before decoding any frame"
        );
    }

    #[test]
    fn webp_preflight_checks_all_frames_sizes_and_chunk_budget() {
        let vp8 = webp_chunk(b"VP8 ", &[0x10, 0, 0, 0x9d, 1, 0x2a, 1, 0, 1, 0]);
        let valid = animated_webp_header(&vp8);
        assert!(validate_webp_headers(&mut std::io::Cursor::new(&valid)).is_ok());
        for bits in [1_u32, 16382 | (16382 << 14)] {
            let mut header = vec![0x2f];
            header.extend_from_slice(&bits.to_le_bytes());
            let file = animated_webp_header(&webp_chunk(b"VP8L", &header));
            assert!(validate_webp_headers(&mut std::io::Cursor::new(file)).is_err());
        }
        let mut alpha_vp8 = webp_chunk(b"ALPH", &[0, 255]);
        alpha_vp8.extend(webp_chunk(
            b"VP8 ",
            &[0x10, 0, 0, 0x9d, 1, 0x2a, 2, 0, 1, 0],
        ));
        assert!(
            background_webp_decoder(std::io::Cursor::new(animated_webp_header(&alpha_vp8)))
                .is_err()
        );
        let mut later_oversized = valid.clone();
        let mut anmf = vec![0; 16];
        anmf.extend(webp_chunk(
            b"VP8 ",
            &[0x10, 0, 0, 0x9d, 1, 0x2a, 0xff, 0x3f, 0xff, 0x3f],
        ));
        later_oversized.extend(webp_chunk(b"ANMF", &anmf));
        let size = (later_oversized.len() - 8) as u32;
        later_oversized[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(background_webp_decoder(std::io::Cursor::new(later_oversized)).is_err());
        let mut alpha_wrong_tag = webp_chunk(b"ALPH", &[0, 255]);
        alpha_wrong_tag.extend(webp_chunk(
            b"JUNK",
            &[0x10, 0, 0, 0x9d, 1, 0x2a, 0xff, 0x3f, 0xff, 0x3f],
        ));
        assert!(
            background_webp_decoder(std::io::Cursor::new(animated_webp_header(&alpha_wrong_tag)))
                .is_err()
        );
        let mut oversized_anmf = valid.clone();
        oversized_anmf[58] = 1;
        assert!(background_webp_decoder(std::io::Cursor::new(oversized_anmf)).is_err());
        let mut truncated = valid.clone();
        truncated.pop();
        assert!(validate_webp_headers(&mut std::io::Cursor::new(truncated)).is_err());
        let mut too_many = valid;
        for _ in 0..4096 {
            too_many.extend(webp_chunk(b"JUNK", &[]));
        }
        let size = (too_many.len() - 8) as u32;
        too_many[4..8].copy_from_slice(&size.to_le_bytes());
        assert!(validate_webp_headers(&mut std::io::Cursor::new(too_many)).is_err());
    }

    #[test]
    fn webp_preflight_preserves_still_decode_and_rejects_bad_animation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("background.webp");
        image::RgbaImage::from_pixel(3, 2, image::Rgba([6, 7, 8, 255]))
            .save(&path)
            .unwrap();
        assert_eq!(
            decode_bg_image(path.to_str().unwrap()).unwrap().rgba,
            [6, 7, 8, 255].repeat(6)
        );
        assert_eq!(
            decode_bg_image_frames(path.to_str().unwrap())
                .unwrap()
                .len(),
            1
        );
        let vp8 = webp_chunk(
            b"VP8 ",
            &[0x10, 0, 0, 0x9d, 1, 0x2a, 0xff, 0x3f, 0xff, 0x3f],
        );
        std::fs::write(&path, animated_webp_header(&vp8)).unwrap();
        assert!(decode_bg_image(path.to_str().unwrap()).is_none());
        assert!(decode_bg_image_frames(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn png_metadata_respects_wallpaper_budget() {
        use image::ImageDecoder;
        let data = include_bytes!("../tests/fixtures/oversized-profile.png");
        match background_png_decoder(std::io::Cursor::new(data)) {
            Ok(mut decoder) => {
                assert_eq!(decoder.dimensions(), (1, 1));
                assert!(
                    decoder.icc_profile().unwrap().is_none(),
                    "oversized ICC profile was allocated"
                );
            }
            Err(image::ImageError::Limits(_)) => {}
            Err(error) => panic!("unexpected PNG fixture error: {error}"),
        }
    }

    #[test]
    fn decoded_frame_envelope_accepts_limit_and_rejects_one_past() {
        assert!(decoder_dimensions_fit((8192, 2048)));
        assert!(!decoder_dimensions_fit((8192, 2049)));
        assert!(!decoder_dimensions_fit((MAX_BG_IMAGE_DIM + 1, 1)));
        assert_eq!(MAX_BG_IMAGE_BYTES, 64 * 1024 * 1024);
        assert_eq!(MAX_BG_ANIM_BYTES, 128 * 1024 * 1024);
        assert_eq!(MAX_BG_FRAMES, 128);
    }

    /// O(W·H·R) brute-force blur, kept as a correctness oracle for the
    /// sliding-window version.
    fn box_blur_reference(img: &mut BgImage, radius: u32) {
        if radius == 0 || img.width == 0 || img.height == 0 {
            return;
        }
        let r = (radius.min(16)) as i32;
        let mut linear = vec![0.0f32; img.rgba.len()];
        premultiply_linear_rgba8(&img.rgba, &mut linear);
        let pass = |linear: &mut Vec<f32>, horizontal: bool| {
            let w = img.width as i32;
            let h = img.height as i32;
            let mut out = vec![0.0f32; linear.len()];
            for y in 0..h {
                for x in 0..w {
                    let mut sum = [0.0f64; 4];
                    let mut count = 0u32;
                    for d in -r..=r {
                        let (xx, yy) = if horizontal {
                            ((x + d).clamp(0, w - 1), y)
                        } else {
                            (x, (y + d).clamp(0, h - 1))
                        };
                        let i = (yy as usize * w as usize + xx as usize) * 4;
                        sum[0] += f64::from(linear[i]);
                        sum[1] += f64::from(linear[i + 1]);
                        sum[2] += f64::from(linear[i + 2]);
                        sum[3] += f64::from(linear[i + 3]);
                        count += 1;
                    }
                    let o = (y as usize * w as usize + x as usize) * 4;
                    out[o] = (sum[0] / f64::from(count)) as f32;
                    out[o + 1] = (sum[1] / f64::from(count)) as f32;
                    out[o + 2] = (sum[2] / f64::from(count)) as f32;
                    out[o + 3] = (sum[3] / f64::from(count)) as f32;
                }
            }
            *linear = out;
        };
        for _ in 0..3 {
            pass(&mut linear, true);
            pass(&mut linear, false);
        }
        unpremultiply_linear_rgba8(&linear, &mut img.rgba);
    }

    #[test]
    fn blur_preserves_constant_dark_colors() {
        for channel in [6u8, 1, 13, 64] {
            for alpha in [255u8, 128, 1] {
                for radius in [1, 8, 16] {
                    let pixel = [channel, channel, channel, alpha];
                    let rgba = pixel.repeat(5 * 3);
                    let mut image = BgImage {
                        width: 5,
                        height: 3,
                        rgba: rgba.clone(),
                    };
                    assert!(box_blur(&mut image, radius));
                    assert_eq!(
                        image.rgba, rgba,
                        "channel={channel} alpha={alpha} r={radius}"
                    );
                }
            }
        }
    }

    /// Drift guard: the O(W·H) sliding-window blur must be
    /// byte-identical to the O(W·H·R) brute-force reference, across odd/even
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
    fn blur_preserves_white_chroma_across_a_transparent_edge() {
        let mut image = BgImage {
            width: 9,
            height: 1,
            rgba: (0..9)
                .flat_map(|x| {
                    if x == 4 {
                        [255, 255, 255, 255]
                    } else {
                        [0, 0, 0, 0]
                    }
                })
                .collect(),
        };

        assert!(box_blur(&mut image, 1));
        let fringe = image
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .find(|pixel| pixel[3] > 0 && pixel[3] < 255)
            .expect("blur must create a partially transparent fringe");
        assert!(
            fringe[..3].iter().all(|&channel| channel >= 245),
            "premultiplied linear filtering must preserve white chroma, got {fringe:?}"
        );
    }

    /// The animated-background clock loops through frames per their
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

    /// The wake interval is the time to the NEXT frame
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

    /// A still PNG decodes to exactly one frame via the animated entry
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

    /// An animated GIF decodes to multiple frames with per-frame gaps,
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
    fn gif_rejects_subframe_larger_than_canvas() {
        use image::codecs::gif::GifEncoder;
        use image::{Frame, Rgba, RgbaImage};

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("subframe.gif");
        let mut encoded = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut encoded);
            let pixels = RgbaImage::from_pixel(4, 4, Rgba([6, 6, 6, 255]));
            encoder
                .encode_frame(Frame::new(pixels))
                .expect("encode gif");
        }
        std::fs::write(&path, &encoded).expect("write gif");
        assert!(decode_bg_image_frames(path.to_str().unwrap()).is_some());

        // Shrink only the logical screen; retain the valid 4x4 frame.
        encoded[6..10].copy_from_slice(&[1, 0, 1, 0]);
        std::fs::write(&path, &encoded).expect("write small canvas gif");
        assert!(decode_bg_image_frames(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn empty_path_is_none() {
        // Drift guard. The cfg default for
        // background_image is an empty string; decode_bg_image
        // must NOT log::warn or panic on that case (it would
        // spam logs for every non-bg-image kettle run).
        assert!(decode_bg_image("").is_none());
        assert!(decode_bg_image("   ").is_none());
    }

    #[test]
    fn rejects_rgb_bmp_whose_rgba_exceeds_byte_cap() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("oversized-rgba.bmp");
        let pixels = image::RgbImage::new(8192, 2049);
        pixels.save(&path).expect("write rgb bmp");
        drop(pixels);

        assert!(decode_bg_image(path.to_str().unwrap()).is_none());
        assert!(decode_bg_image_frames(path.to_str().unwrap()).is_none());
    }

    #[test]
    fn nonexistent_file_returns_none_with_warn() {
        // Should log a warning but not panic.
        assert!(decode_bg_image("/nonexistent/path/does-not-exist.png").is_none());
    }

    #[test]
    fn real_png_roundtrip() {
        // Acceptance test for the full decode path. Writes a known 8x4 RGBA
        // PNG to a temp file, decodes it via decode_bg_image, and checks the
        // dimensions, buffer length, and first pixel.
        // PID + nanos in the filename so parallel `cargo
        // test` runs and CI-runner concurrency don't race on a shared
        // /tmp path. Matches the pattern used in session::tests.
        let path = std::env::temp_dir().join(format!(
            "kettle-bg-image-roundtrip-smoke-{}-{}.png",
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

    /// Drift guard: bg-image decoder must reject a PNG
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
        // PID + nanos so parallel test runs don't race.
        let path = std::env::temp_dir().join(format!(
            "kettle-bg-image-oversize-{}-{}.png",
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

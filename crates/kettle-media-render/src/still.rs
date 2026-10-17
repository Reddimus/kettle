//! HEIF and AVIF stills: known by their file-type box's brands
//! ([`kettle_media::video::still_image`]), drawn by this platform's own
//! decoder from the bytes held, in a confined worker only, then fitted to the
//! job's box as a raster is. The renderer itself parses none of the file.

use std::time::{Duration, Instant};

use image::RgbaImage;
use kettle_media::video::{StillImage, StillInput};
use kettle_media::{FailureCode, Job, JobKind, MAX_RENDERED_EDGE, Rendered, content_digest};

use crate::{Means, raster, source};

/// How long a decoder may take over one still: inside an Auto job's 3 s,
/// which the worker's watchdog keeps since a still is not reported as a
/// raster (2 s) before it is drawn, with room left to fit and reply.
const DEADLINE: Duration = Duration::from_millis(2500);

/// Draw the still `held` holds, `still` by its brands, fitted to `job`'s box.
/// Unconfined it is `SandboxUnavailable`, and with no decoder at all
/// `BackendUnavailable`; a decoder that reads no stills answers
/// `CodecUnavailable`.
pub(crate) fn render(
    job: &Job,
    held: &source::Held<'_>,
    still: StillImage,
    means: Means<'_>,
) -> Result<Rendered, FailureCode> {
    if !means.confined {
        return Err(FailureCode::SandboxUnavailable);
    }
    let decoder = means.decoder.ok_or(FailureCode::BackendUnavailable)?;
    let snapshot = held.snapshot(JobKind::Raster.input_cap())?;
    let max_edge = job
        .target
        .width
        .max(job.target.height)
        .clamp(1, MAX_RENDERED_EDGE);
    let input = StillInput {
        bytes: &snapshot.bytes,
        file: match held {
            source::Held::File { file, identity, .. } => Some((file, *identity)),
            source::Held::Bytes(_) => None,
        },
    };
    // The decoder may read the file through a descriptor of its own: what it
    // drew counts only if the file is still the one held, and a failure of a
    // file changed meanwhile is that change.
    let decoded = match decoder.still_image(input, still, max_edge, Instant::now() + DEADLINE) {
        Ok(decoded) => decoded,
        Err(failure) => {
            held.unchanged()?;
            return Err(failure);
        }
    };
    held.unchanged()?;
    // What came back is checked as anything a decoder hands over is.
    let fits = decoded.width > 0
        && decoded.height > 0
        && decoded.width.max(decoded.height) <= max_edge
        && u64::from(decoded.width) * u64::from(decoded.height) * 4 == decoded.rgba.len() as u64;
    if !fits {
        return Err(FailureCode::RenderResource);
    }
    let (source_width, source_height) = (decoded.source_width, decoded.source_height);
    let image = RgbaImage::from_raw(decoded.width, decoded.height, decoded.rgba)
        .ok_or(FailureCode::RenderResource)?;
    let (width, height, rgba, mut layout) = raster::fit(image, job.target)?;
    // A zoomed lane plans its sharper pixels from the still's own size, not
    // the size it was drawn at for this box.
    if source_width > 0 && source_height > 0 {
        layout.source_width = f64::from(source_width);
        layout.source_height = f64::from(source_height);
    }
    let digest =
        content_digest(&snapshot.bytes, snapshot.identity).map_err(|_| FailureCode::BadParams)?;
    let rendered = Rendered {
        width,
        height,
        rgba,
        digest,
        layout,
        exact_source: None,
        source_text: Vec::new(),
        fence_sources: Vec::new(),
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: Vec::new(),
        warnings: Vec::new(),
        video: None,
        page: None,
    };
    rendered
        .validate()
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(rendered)
}

//! PDF pages. The whole document is read under its own cap, then one page is
//! drawn by hayro on white, as paper is, fitted to the job's box within the
//! vector ceiling exactly as an SVG is (a crop included, for a zoomed lane),
//! and returned with which page it is of how many.
//!
//! hayro parses the document in this process and is younger than the rest
//! of what the worker runs, so a PDF renders only in a confined worker:
//! unconfined it is `SandboxUnavailable`, as a video is. Its render cannot be
//! cancelled; the worker's deadline, CPU limit and memory watch bound it. A
//! document opens when its user password is empty; another encrypted one is
//! `UnsupportedMedia`. A font the document does not embed is drawn with the
//! standard fonts compiled in, never one read from the system, and a font
//! hayro cannot use is reported as missing glyphs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hayro::hayro_interpret::util::TransformExt as _;
use hayro::hayro_interpret::{InterpreterSettings, InterpreterWarning};
use hayro::hayro_syntax::{LoadPdfError, Pdf};
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{Pixmap, RasterizerSettings, RenderContext, Resources, TargetInit};
use kettle_media::{FailureCode, Job, MAX_PDF_BYTES, PageOf, Rendered, Warning, content_digest};

use crate::source;

/// Bytes from the start of a file within which a reader finds `%PDF-`.
const HEADER_WINDOW: usize = 1024;

/// Whether `prefix`, a file's first bytes, is a PDF's: `%PDF-` within its
/// first kilobyte, where readers look for the header.
pub(crate) fn is_pdf(prefix: &[u8]) -> bool {
    prefix[..prefix.len().min(HEADER_WINDOW)]
        .windows(5)
        .any(|window| window == b"%PDF-")
}

/// Page `index` of the job's PDF, read under the PDF cap.
pub(crate) fn render(job: &Job, index: u32, confined: bool) -> Result<Rendered, FailureCode> {
    if !confined {
        return Err(FailureCode::SandboxUnavailable);
    }
    render_loaded(job, source::load(&job.source, MAX_PDF_BYTES)?, index)
}

/// Page `index` of the PDF `snapshot` holds, its bytes handed to the parser
/// without a copy once their digest is taken.
pub(crate) fn render_loaded(
    job: &Job,
    snapshot: source::Snapshot<'_>,
    index: u32,
) -> Result<Rendered, FailureCode> {
    snapshot.within(MAX_PDF_BYTES)?;
    let digest =
        content_digest(&snapshot.bytes, snapshot.identity).map_err(|_| FailureCode::BadParams)?;
    let bytes = snapshot.bytes.into_owned();
    let unusable_font = Arc::new(AtomicBool::new(false));
    let noted = Arc::clone(&unusable_font);
    let settings = InterpreterSettings {
        warning_sink: Arc::new(move |warning| {
            if matches!(warning, InterpreterWarning::UnsupportedFont) {
                noted.store(true, Ordering::Relaxed);
            }
        }),
        ..InterpreterSettings::default()
    };
    let (placement, rgba, count) = crate::guarded(FailureCode::RenderParse, || {
        let pdf = Pdf::new(bytes).map_err(|error| match error {
            LoadPdfError::Decryption(_) => FailureCode::UnsupportedMedia,
            LoadPdfError::Invalid => FailureCode::RenderParse,
        })?;
        let pages = pdf.pages();
        let count = u32::try_from(pages.len()).map_err(|_| FailureCode::RenderResource)?;
        if count == 0 {
            return Err(FailureCode::RenderParse);
        }
        let page = usize::try_from(index)
            .ok()
            .and_then(|index| pages.get(index))
            .ok_or(FailureCode::IndexOutOfRange)?;
        let (width, height) = page.render_dimensions();
        let size = usvg::Size::from_wh(width, height).ok_or(FailureCode::RenderParse)?;
        // The canvas is the vector ceiling's at most (a crop's too), so it
        // fits the renderer's 16-bit sizes; the page's own size never decides.
        let placement = crate::svg::place(size, job.target)?;
        let canvas = (
            u16::try_from(placement.width).map_err(|_| FailureCode::RenderResource)?,
            u16::try_from(placement.height).map_err(|_| FailureCode::RenderResource)?,
        );
        let mut context = RenderContext::new(canvas.0, canvas.1);
        let t = placement.transform;
        let onto_canvas = Affine::new([t.sx, t.ky, t.kx, t.sy, t.tx, t.ty].map(f64::from));
        hayro::render_into(
            page,
            &hayro::RenderCache::new(),
            &settings,
            &hayro::RenderSettings::default(),
            &mut context,
            onto_canvas * page.initial_transform(true).to_kurbo(),
        );
        context.flush();
        let mut pixmap = Pixmap::new(canvas.0, canvas.1);
        context.render_with(
            &mut pixmap,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..RasterizerSettings::default()
            },
        );
        // Drawn over opaque white, every pixel is opaque, so its
        // premultiplied bytes are straight RGBA already.
        Ok((placement, pixmap.data_as_u8_slice().to_vec(), count))
    })?;
    let warnings = if unusable_font.load(Ordering::Relaxed) {
        vec![Warning::MissingGlyphs]
    } else {
        Vec::new()
    };
    let rendered = Rendered {
        width: placement.width,
        height: placement.height,
        rgba,
        digest,
        layout: placement.layout,
        exact_source: None,
        source_text: Vec::new(),
        fence_sources: Vec::new(),
        fence_count: 0,
        fence_index: None,
        uncovered_scripts: Vec::new(),
        warnings,
        video: None,
        page: Some(PageOf { index, count }),
    };
    rendered
        .validate()
        .map_err(|_| FailureCode::RenderResource)?;
    Ok(rendered)
}

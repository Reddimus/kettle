//! The entry points for fuzzing what the worker renders from untrusted
//! bytes: a raster, an SVG, a Mermaid diagram, a Markdown gallery, a PDF
//! page, or bytes the worker classifies itself. Each case is a job the parent could send,
//! rendered into a small box. It must not panic outside a guarded library
//! call (whose panic the renderer answers with a fixed failure, as the
//! worker does), and what it renders must pass the checks the parent
//! applies to a reply, encode as the worker would write it, and be the kind
//! asked for. The fuzz target (feature `fuzzing`) and the in-process sweep
//! below call the same function.

use std::cell::Cell;

use kettle_media::wire::{self, Direction, Frame};
use kettle_media::{Canvas, Job, JobKind, MediaKind, Source, Target, Theme};

/// What a case's first byte picks.
const KINDS: [JobKind; 6] = [
    JobKind::Auto,
    JobKind::Raster,
    JobKind::Svg,
    JobKind::Mermaid,
    JobKind::MarkdownDiagrams { index: 0 },
    JobKind::PdfPage { index: 0 },
];
/// The PDF pages a `pdf` case's first byte picks among.
const PDF_PAGES: u32 = 4;
/// The box each case renders into: small, so a case costs little.
const EDGE: u32 = 64;

thread_local! {
    static GUARDS: Cell<u32> = const { Cell::new(0) };
}

/// Marks this thread as inside a guarded library call until dropped.
pub(crate) struct Guard;

impl Guard {
    pub(crate) fn enter() -> Self {
        GUARDS.with(|guards| guards.set(guards.get() + 1));
        Self
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        GUARDS.with(|guards| guards.set(guards.get() - 1));
    }
}

/// Whether this thread is inside a guarded library call: a fuzz target's
/// panic hook lets such a panic unwind to the guard rather than count it.
pub fn in_guard() -> bool {
    GUARDS.with(|guards| guards.get() > 0)
}

/// Render `data` past its first byte as the kind that byte picks.
pub fn render(data: &[u8]) {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    check(KINDS[usize::from(pick) % KINDS.len()], bytes);
}

/// Render `data` past its first byte as a PDF, at the page that byte picks:
/// the PDF parser and interpreter get a target of their own.
pub fn pdf(data: &[u8]) {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    check(
        JobKind::PdfPage {
            index: u32::from(pick) % PDF_PAGES,
        },
        bytes,
    );
}

/// Render `bytes` as `kind` and hold the result to the parent's checks.
fn check(kind: JobKind, bytes: &[u8]) {
    let job = Job {
        kind,
        source: Source::Bytes(bytes.to_vec()),
        theme: Theme {
            background: [16, 16, 16, 255],
            foreground: [235, 235, 235, 255],
            palette: [[128, 128, 128, 255]; 16],
            accent: [64, 128, 255, 255],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        target: Target {
            width: EDGE,
            height: EDGE,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: Vec::new(),
    };
    // Only a job the parent could send: one past its kind's caps never
    // leaves the parent.
    let Ok(sent) = wire::encode(&Frame::Job(job), Direction::ParentToWorker) else {
        return;
    };
    let Ok(Frame::Job(job)) = wire::decode(&sent, Direction::ParentToWorker) else {
        panic!("a job the parent encodes does not decode");
    };
    let Ok((made, rendered)) = crate::render_with_kind(&job, |_| {}) else {
        return;
    };
    let asked = match kind {
        JobKind::Raster => Some(MediaKind::Raster),
        JobKind::Svg => Some(MediaKind::Svg),
        JobKind::Mermaid => Some(MediaKind::Mermaid),
        JobKind::MarkdownDiagrams { .. } => Some(MediaKind::Markdown),
        JobKind::PdfPage { .. } => Some(MediaKind::Pdf),
        _ => None,
    };
    if let Some(asked) = asked {
        assert_eq!(made, asked, "asked for {kind:?}");
    }
    if let Err(error) = rendered.validate_as(made) {
        panic!("a {made:?} render the parent refuses: {error:?}");
    }
    assert!(
        rendered.width <= EDGE && rendered.height <= EDGE,
        "a {made:?} render of {}x{} for a {EDGE} box",
        rendered.width,
        rendered.height
    );
    let reply = match kind {
        JobKind::Auto => Frame::DetectedRendered {
            kind: made,
            rendered,
        },
        _ => Frame::Rendered(rendered),
    };
    assert!(
        wire::encode(&reply, Direction::WorkerToParent).is_ok(),
        "a {made:?} render the worker cannot write"
    );
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{ImageFormat, Rgb, RgbImage};
    use kettle_test_support::mutate::{cases, save_seeds};

    use super::*;

    /// A `width` by `height` picture in `format`.
    fn picture(format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_fn(width, height, |x, y| {
            Rgb([(x * 2) as u8, (y * 3) as u8, 90])
        });
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    /// `bytes` behind each of the bytes that pick `kinds`.
    fn seeds_for(kinds: &[usize], bytes: &[u8], seeds: &mut Vec<Vec<u8>>) {
        for &kind in kinds {
            let mut seed = vec![kind as u8];
            seed.extend_from_slice(bytes);
            seeds.push(seed);
        }
    }

    /// Every kind's seeds, mutated: nothing panics outside a guarded call,
    /// and every render passes the parent's checks, fits its box and is
    /// the kind asked for.
    #[test]
    fn mutated_jobs_never_break_the_renderer() {
        const AUTO: usize = 0;
        const RASTER: usize = 1;
        const SVG: usize = 2;
        const MERMAID: usize = 3;
        const MARKDOWN: usize = 4;
        const PDF: usize = 5;
        let mut seeds = Vec::new();
        for format in [
            ImageFormat::Png,
            ImageFormat::Gif,
            ImageFormat::Jpeg,
            ImageFormat::WebP,
            ImageFormat::Bmp,
            ImageFormat::Tiff,
        ] {
            seeds_for(&[AUTO, RASTER], &picture(format, 12, 8), &mut seeds);
        }
        // Wider and taller than the box, so each is scaled to fit.
        for (width, height) in [(96, 80), (50, 100)] {
            let large = picture(ImageFormat::Png, width, height);
            seeds_for(&[AUTO, RASTER], &large, &mut seeds);
        }
        let shapes = br##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="30" viewBox="0 0 40 30"><rect x="2" y="2" width="20" height="10" fill="#c33"/><circle cx="30" cy="20" r="6" stroke="blue"/><text x="2" y="28" font-size="8">Kettle</text></svg>"##;
        let paths = br##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 50 50"><defs><linearGradient id="g"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#08f"/></linearGradient><path id="p" d="M5 5 L45 5 Q25 45 5 5 Z"/></defs><use xlink:href="#p" fill="url(#g)" transform="rotate(10 25 25)"/><g opacity="0.5"><polyline points="0,50 25,30 50,50" stroke="red" fill="none"/></g></svg>"##;
        for svg in [&shapes[..], &paths[..]] {
            seeds_for(&[AUTO, SVG], svg, &mut seeds);
        }
        for mermaid in [
            &include_bytes!("../tests/fixtures/mermaid/pie.mmd")[..],
            include_bytes!("../tests/fixtures/mermaid/sequence.mmd"),
            include_bytes!("../tests/fixtures/mermaid/class.mmd"),
            include_bytes!("../tests/fixtures/mermaid/er.mmd"),
            include_bytes!("../tests/fixtures/mermaid/gantt.mmd"),
            include_bytes!("../tests/fixtures/mermaid/mindmap.mmd"),
            b"flowchart LR\n  A[Start] --> B{Ok?}\n  B -->|yes| C[Done]\n  B -->|no| A\n",
            b"stateDiagram-v2\n  [*] --> Idle\n  Idle --> Busy: go\n  Busy --> [*]\n",
        ] {
            seeds_for(&[AUTO, MERMAID], mermaid, &mut seeds);
        }
        for markdown in [
            &b"# Notes\n\n```mermaid\nflowchart TD\n  A --> B\n```\n"[..],
            b"Two:\n\n```mermaid\npie\n  \"a\" : 1\n  \"b\" : 2\n```\n\ntext\n\n~~~mermaid\nsequenceDiagram\n  A->>B: hi\n~~~\n",
            // A bare `%%` line once hung the sequence lexer (#584).
            b"~~~mermaid\nsequenceDiagram\n  A->>B: hi\n%%\n  B->>A: ok\n~~~\n",
        ] {
            seeds_for(&[AUTO, MARKDOWN], markdown, &mut seeds);
        }
        // Text that starts with an image format's short signature, and DOT,
        // which starts like a Mermaid flowchart: both are text to classify.
        for text in [
            &b"BMI results\n\n```mermaid\nflowchart TD\n  A --> B\n```\n"[..],
            b"P1 tasks\n\n```mermaid\npie\n  \"a\" : 1\n```\n",
            b"qoif\nflowchart TD\n  A --> B\n",
            b"DDS graph TD\n  A --> B\n",
            b"strict digraph \"deps\" {\n  a -> b [label=<<b>x</b>>];\n}\n",
            b"graph G { a -- b }",
        ] {
            seeds_for(&[AUTO], text, &mut seeds);
        }
        for document in pdf_seeds() {
            seeds_for(&[AUTO, PDF], &document, &mut seeds);
        }
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("render", &seeds);
        for case in cases(&seeds, 800, 0x5eed_0004) {
            render(&case);
        }
    }

    /// A PDF of `pages`, each a 200 by 100 point page drawn by its content
    /// stream, with its cross-reference table at the offsets written.
    fn document(pages: &[&str]) -> Vec<u8> {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                (0..pages.len())
                    .map(|page| format!("{} 0 R", 3 + 2 * page))
                    .collect::<Vec<_>>()
                    .join(" "),
                pages.len()
            ),
        ];
        for (page, content) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents {} 0 R >>",
                4 + 2 * page
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ));
        }
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{object}\nendobj\n", number + 1).into_bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).into_bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").into_bytes());
        }
        out.extend(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .into_bytes(),
        );
        out
    }

    /// PDFs for the sweeps: paths, text in a standard font, a gradient-free
    /// clip and transform, several pages, and upstream's nested object
    /// streams, which the vendored parser refuses.
    fn pdf_seeds() -> Vec<Vec<u8>> {
        vec![
            document(&["1 0 0 rg 0 0 100 100 re f"]),
            document(&["BT /F1 24 Tf 10 40 Td (Kettle) Tj ET"]),
            document(&["q 0.5 0 0 0.5 50 25 cm 0 0 1 rg 0 0 200 100 re W n 0 0 100 50 re f Q"]),
            document(&["0 1 0 rg 0 0 50 50 re f", "1 1 0 rg 10 10 m 190 90 l S", ""]),
            include_bytes!("../tests/fixtures/pdf/nested_object_streams.pdf").to_vec(),
        ]
    }

    /// The PDF target's seeds, mutated: nothing panics outside a guarded
    /// call, and every page rendered passes the parent's checks.
    #[test]
    fn mutated_pdfs_never_break_the_renderer() {
        let mut seeds = Vec::new();
        for document in pdf_seeds() {
            for page in 0..PDF_PAGES as u8 {
                let mut seed = vec![page];
                seed.extend_from_slice(&document);
                seeds.push(seed);
            }
        }
        let seeds: Vec<&[u8]> = seeds.iter().map(Vec::as_slice).collect();
        save_seeds("pdf", &seeds);
        for case in cases(&seeds, 400, 0x5eed_0005) {
            pdf(&case);
        }
    }

    /// A guarded call marks its thread while it runs, a panic in it
    /// included, and leaves it unmarked after.
    #[test]
    fn guarded_calls_mark_their_thread() {
        use kettle_media::FailureCode;
        assert_eq!(
            crate::guarded(FailureCode::RenderParse, || Ok(in_guard())),
            Ok(true)
        );
        assert!(!in_guard());
        let marked = Cell::new(false);
        let failed = crate::guarded::<()>(FailureCode::RenderParse, || {
            marked.set(in_guard());
            panic!("a decoder's bug")
        });
        assert_eq!(
            (marked.get(), failed),
            (true, Err(FailureCode::RenderParse))
        );
        assert!(!in_guard());
    }

    /// A guard marks its thread only while it is held, nested or not.
    #[test]
    fn a_guard_marks_its_thread_while_held() {
        assert!(!in_guard());
        let outer = Guard::enter();
        let inner = Guard::enter();
        assert!(in_guard());
        drop(inner);
        assert!(in_guard());
        drop(outer);
        assert!(!in_guard());
        assert!(!std::thread::spawn(in_guard).join().unwrap());
    }
}

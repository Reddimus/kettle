//! What a shelf item was rendered from, kept so the lane can render it
//! again (another canvas, a zoomed crop), show and copy its source, and
//! read its file again.
//!
//! Bytes a request carried are kept, charged to the process preview
//! account, and shared between the queue, the render lane and the shelf.
//! A file is kept as its path and the authorization it was read under. A
//! file's text comes back from the worker as it was read, from the
//! snapshot the digest covers, and is kept, charged, for a textual kind.
//! When an item's pixels are evicted its charged bytes go too.
//!
//! A gallery (a Markdown document's Mermaid fences, or diagrams copied
//! together) keeps every page, each charged on its own, and its item shows
//! one: that page is the item's source, rendered from the bytes kept, so
//! paging never reads the file again and one gallery is one version of it.

use std::sync::Arc;

use kettle_core::{GraphicsBudget, GraphicsReservation};
use kettle_media::{
    Authorization, Canvas, Digest, FailureCode, FallbackFont, Job, JobKind, MAX_FENCES, NativePath,
    Source, Target, Theme,
};

/// Bytes a preview keeps, charged to the process preview account for as
/// long as they are held.
pub(crate) struct ChargedBytes {
    bytes: Box<[u8]>,
    _charge: Option<GraphicsReservation>,
}

impl ChargedBytes {
    /// Keep `bytes`, charged; `None` when the account has no room.
    pub(crate) fn new(bytes: Vec<u8>) -> Option<Self> {
        let bytes = bytes.into_boxed_slice();
        let charge = match bytes.len() {
            0 => None,
            len => Some(GraphicsBudget::previews().reserve_retained_cpu(len)?),
        };
        Some(Self {
            bytes,
            _charge: charge,
        })
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes
    }
}

impl std::fmt::Debug for ChargedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ChargedBytes({} bytes)", self.bytes.len())
    }
}

/// The pages of a gallery, in order, each kept and charged on its own, so
/// the page an item shows shares its bytes with the job that renders it and
/// a copy of its source.
#[derive(Debug)]
pub(crate) struct Gallery {
    pages: Vec<Arc<ChargedBytes>>,
}

impl Gallery {
    /// A gallery of `pages`, each kept and charged: `None` for none, for more
    /// than [`MAX_FENCES`], or when the account has no room for them all.
    pub(crate) fn new(pages: Vec<String>) -> Option<Self> {
        if pages.is_empty() || pages.len() > MAX_FENCES {
            return None;
        }
        let pages = pages
            .into_iter()
            .map(|page| ChargedBytes::new(page.into_bytes()).map(Arc::new))
            .collect::<Option<Vec<_>>>()?;
        Some(Self { pages })
    }

    /// How many pages it has.
    pub(crate) fn len(&self) -> usize {
        self.pages.len()
    }

    /// Page `index`, from 0.
    pub(crate) fn page(&self, index: usize) -> Option<&Arc<ChargedBytes>> {
        self.pages.get(index)
    }
}

/// Where a render job reads its source.
#[derive(Clone, Debug)]
pub(crate) enum SourceInput {
    /// Bytes the request carried.
    Bytes(Arc<ChargedBytes>),
    /// A file, and what lets the worker read it.
    Path {
        path: NativePath,
        authorization: Authorization,
    },
    /// Bytes given back to the preview account: nothing to render again.
    Released,
}

/// A render job that can be asked for again: what it reads and how it
/// renders.
#[derive(Clone, Debug)]
pub(crate) struct JobSpec {
    pub kind: JobKind,
    pub input: SourceInput,
    pub theme: Theme,
    pub canvas: Canvas,
    pub target: Target,
    pub fallback_fonts: Vec<FallbackFont>,
}

/// A job ready for the worker, its copy of the bytes charged while it lives.
pub(crate) struct LaneJob {
    pub job: Job,
    _charge: Option<GraphicsReservation>,
}

impl JobSpec {
    /// The spec of `job`, its bytes kept and charged; `None` when the
    /// account has no room for them.
    pub(crate) fn from_job(job: Job) -> Option<Self> {
        let input = match job.source {
            Source::Bytes(bytes) => SourceInput::Bytes(Arc::new(ChargedBytes::new(bytes)?)),
            Source::Path {
                path,
                authorization,
            } => SourceInput::Path {
                path,
                authorization,
            },
        };
        Some(Self {
            kind: job.kind,
            input,
            theme: job.theme,
            canvas: job.canvas,
            target: job.target,
            fallback_fonts: job.fallback_fonts,
        })
    }

    /// The job to send the worker. Bytes are copied, and the copy charged
    /// for as long as it lives, so the render lane builds it, not the UI
    /// thread. Released bytes, or no room for the copy, refuse.
    pub(crate) fn job(&self) -> Result<LaneJob, FailureCode> {
        let (source, charge) = match &self.input {
            SourceInput::Bytes(bytes) => {
                let len = bytes.as_slice().len();
                let charge = match len {
                    0 => None,
                    len => Some(
                        GraphicsBudget::previews()
                            .reserve_transient_cpu(len)
                            .ok_or(FailureCode::OverBudget)?,
                    ),
                };
                (Source::Bytes(bytes.as_slice().to_vec()), charge)
            }
            SourceInput::Path {
                path,
                authorization,
            } => (
                Source::Path {
                    path: path.clone(),
                    authorization: authorization.clone(),
                },
                None,
            ),
            SourceInput::Released => return Err(FailureCode::WorkerUnavailable),
        };
        Ok(LaneJob {
            job: Job {
                kind: self.kind,
                source,
                theme: self.theme.clone(),
                canvas: self.canvas,
                target: self.target,
                fallback_fonts: self.fallback_fonts.clone(),
            },
            _charge: charge,
        })
    }
}

/// What an item was rendered from, and how.
#[derive(Clone, Debug)]
pub(crate) struct ItemSource {
    /// The job that rendered it.
    pub spec: JobSpec,
    /// The source the pixels came from.
    pub digest: Digest,
    /// How many display rows the item's text has, when it is text (SVG or
    /// Mermaid) held as UTF-8: counted once, as what an item holds never
    /// changes, so a lane drawn again does not count it again.
    rows: Option<usize>,
    /// A file's text as the worker read it; bytes a request carried are
    /// their own text.
    file_text: Option<Arc<ChargedBytes>>,
    /// Whether the file changed since it was read: nothing more is read for
    /// the item unasked, until a reload replaces it.
    pub changed: bool,
    /// For a page of a gallery: every page, and the one shown. Given back
    /// with the item's other charged bytes.
    gallery: Option<(Arc<Gallery>, u8)>,
    /// For a page of a gallery read from a file: that file, and the page
    /// shown, which a reload reads again. Kept when the pages go.
    origin: Option<(NativePath, u8)>,
}

/// What holds an item's text, if any: the bytes a request carried, or else
/// a file's text.
fn held_text<'a>(
    input: &'a SourceInput,
    file_text: &'a Option<Arc<ChargedBytes>>,
) -> Option<&'a Arc<ChargedBytes>> {
    match (input, file_text) {
        (SourceInput::Bytes(bytes), _) => Some(bytes),
        (_, Some(text)) => Some(text),
        _ => None,
    }
}

/// The display rows `text` has: its lines, a final line break ending the
/// last rather than starting another.
fn display_row_count(text: &str) -> usize {
    text.strip_suffix('\n').unwrap_or(text).split('\n').count()
}

impl ItemSource {
    /// The source of an item rendered by `spec`. `exact_source` is the text
    /// the worker read, kept, charged, for a textual file; dropped when the
    /// account has no room, as copying it is then all that is lost.
    pub(crate) fn new(
        spec: JobSpec,
        digest: Digest,
        textual: bool,
        exact_source: Option<String>,
    ) -> Self {
        let file_text = match (&spec.input, exact_source) {
            (SourceInput::Path { .. }, Some(text)) if textual => {
                ChargedBytes::new(text.into_bytes()).map(Arc::new)
            }
            _ => None,
        };
        let rows = held_text(&spec.input, &file_text)
            .filter(|_| textual)
            .and_then(|text| std::str::from_utf8(text.as_slice()).ok())
            .map(display_row_count);
        Self {
            spec,
            digest,
            rows,
            file_text,
            changed: false,
            gallery: None,
            origin: None,
        }
    }

    /// Page `index` of `gallery`, the pages a render by `spec` returned or
    /// that were copied together, as an item's source: the page renders as
    /// Mermaid from the bytes kept, on `spec`'s theme, canvas and target.
    /// `None` past the last page.
    pub(crate) fn page(spec: &JobSpec, gallery: Arc<Gallery>, index: u8) -> Option<Self> {
        let origin = match &spec.input {
            SourceInput::Path { path, .. } => Some((path.clone(), index)),
            _ => None,
        };
        let mut source = Self {
            spec: spec.clone(),
            digest: Digest {
                sha256: [0; 32],
                path_identity: None,
            },
            rows: None,
            file_text: None,
            changed: false,
            gallery: Some((gallery, index)),
            origin,
        };
        source.select(index).then_some(source)
    }

    /// Show page `index` of the item's gallery: its source, digest and rows
    /// become that page's. False, changing nothing, past the last page or
    /// once the pages are gone.
    pub(crate) fn select(&mut self, index: u8) -> bool {
        let Some(spec) = self.page_spec(index) else {
            return false;
        };
        let SourceInput::Bytes(bytes) = &spec.input else {
            return false;
        };
        let Ok(digest) = kettle_media::content_digest(bytes.as_slice(), None) else {
            return false;
        };
        self.rows = std::str::from_utf8(bytes.as_slice())
            .ok()
            .map(display_row_count);
        self.digest = digest;
        self.spec = spec;
        if let Some((_, shown)) = &mut self.gallery {
            *shown = index;
        }
        if let Some((_, page)) = &mut self.origin {
            *page = index;
        }
        true
    }

    /// The job that renders page `index` of the item's gallery as it
    /// renders now: on its canvas, theme and target. `None` past the last
    /// page, without a gallery, or once the pages are gone.
    pub(crate) fn page_spec(&self, index: u8) -> Option<JobSpec> {
        let (gallery, _) = self.gallery.as_ref()?;
        let bytes = gallery.page(usize::from(index))?;
        Some(JobSpec {
            kind: JobKind::Mermaid,
            input: SourceInput::Bytes(Arc::clone(bytes)),
            ..self.spec.clone()
        })
    }

    /// The digest a render of page `index` of the item's gallery has: its
    /// bytes', which no file can change. `None` past the last page, without
    /// a gallery, or once the pages are gone.
    pub(crate) fn page_digest(&self, index: u8) -> Option<Digest> {
        let (gallery, _) = self.gallery.as_ref()?;
        kettle_media::content_digest(gallery.page(usize::from(index))?.as_slice(), None).ok()
    }

    /// Whether rendering the item again reads its file.
    pub(crate) fn reads_file(&self) -> bool {
        matches!(self.spec.input, SourceInput::Path { .. })
    }

    /// The page shown, from 0, and how many there are, for a page of a
    /// gallery whose pages are still held.
    pub(crate) fn gallery_page(&self) -> Option<(u8, usize)> {
        self.gallery
            .as_ref()
            .map(|(gallery, shown)| (*shown, gallery.len()))
    }

    /// The file a reload reads again: the one the item was read from, or
    /// the one its gallery was; with the gallery page shown, for a gallery.
    pub(crate) fn reload_file(&self) -> Option<(&NativePath, Option<u8>)> {
        match (&self.spec.input, &self.origin) {
            (SourceInput::Path { path, .. }, _) => Some((path, None)),
            (_, Some((path, page))) => Some((path, Some(*page))),
            _ => None,
        }
    }

    /// The item's source text exactly as rendered, when it is text and is
    /// still held.
    pub(crate) fn text(&self) -> Option<&str> {
        self.text_backing()
            .and_then(|backing| std::str::from_utf8(backing.as_slice()).ok())
    }

    /// What holds the item's source text, shared, its UTF-8 checked when the
    /// item took it: a copy of it to the clipboard keeps it, charged, rather
    /// than a second copy of the text.
    pub(crate) fn text_backing(&self) -> Option<&Arc<ChargedBytes>> {
        self.rows.and(held_text(&self.spec.input, &self.file_text))
    }

    /// A small SVG an inline request carried, for tests.
    #[cfg(test)]
    pub(crate) fn sample(svg: &[u8]) -> Self {
        let spec = JobSpec::from_job(Job {
            kind: JobKind::Svg,
            source: Source::Bytes(svg.to_vec()),
            theme: kettle_media::Theme {
                background: [0; 4],
                foreground: [255; 4],
                palette: [[0; 4]; 16],
                accent: [0; 4],
                is_dark: true,
            },
            canvas: Canvas::Theme,
            target: Target {
                width: 1,
                height: 1,
                scale: 1.0,
                crop: None,
            },
            fallback_fonts: Vec::new(),
        })
        .expect("room for a sample");
        let digest = kettle_media::content_digest(svg, None).expect("a small digest");
        Self::new(spec, digest, true, None)
    }

    /// A file `path` the worker read, whose render carried `digest`, for
    /// tests of opening a video, which happens on Unix.
    #[cfg(all(test, unix))]
    pub(crate) fn sample_file(path: &std::path::Path, digest: Digest) -> Self {
        let mut source = Self::sample(b"<svg/>");
        source.spec.kind = JobKind::Auto;
        source.spec.input = SourceInput::Path {
            path: NativePath::from_path(path).expect("a short path"),
            authorization: Authorization::ExternalAttested(kettle_media::ExternalAttested {
                dev: digest.path_identity.map_or(0, |identity| identity.dev),
                ino: digest.path_identity.map_or(0, |identity| identity.ino),
            }),
        };
        source.digest = digest;
        source.rows = None;
        source
    }

    /// How many display rows the source text has: its lines, a final line
    /// break ending the last rather than starting another. None without
    /// text.
    pub(crate) fn text_rows(&self) -> usize {
        self.text_backing().and(self.rows).unwrap_or(0)
    }

    /// Give the charged bytes back to the preview account: what a request
    /// carried, and a file's text. A file stays a path, which costs nothing.
    pub(crate) fn release(&mut self) {
        if matches!(self.spec.input, SourceInput::Bytes(_)) {
            self.spec.input = SourceInput::Released;
        }
        self.file_text = None;
        self.gallery = None;
    }
}

/// Tab stops a source row expands to.
const TAB_STOP: usize = 8;

/// Most bytes a row may hold per column shown, so characters that take no
/// column (combining marks) cannot make a row in view arbitrarily long.
const MAX_ROW_BYTES_PER_COLUMN: usize = 16;

/// The rows of `text` a lane shows: `rows` lines from `first_row`, each from
/// display column `first_column` and at most `columns` wide. Tabs expand to
/// stops of eight; control, format and bidirectional characters show as
/// U+FFFD, so source text can neither reorder nor hide what is shown; a
/// line's CR is dropped. Work is bounded by what is shown.
pub(crate) fn display_rows(
    text: &str,
    first_row: usize,
    rows: usize,
    first_column: usize,
    columns: usize,
) -> Vec<String> {
    use unicode_width::UnicodeWidthChar as _;
    text.strip_suffix('\n')
        .unwrap_or(text)
        .split('\n')
        .skip(first_row)
        .take(rows)
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let mut row = String::new();
            let (mut column, mut shown) = (0usize, 0usize);
            let max_bytes = columns
                .saturating_add(1)
                .saturating_mul(MAX_ROW_BYTES_PER_COLUMN);
            for c in line.chars() {
                if shown >= columns || row.len() + c.len_utf8() > max_bytes {
                    break;
                }
                let (glyph, width) = match c {
                    '\t' => (' ', TAB_STOP - column % TAB_STOP),
                    c if hidden(c) => ('\u{FFFD}', 1),
                    c => (c, c.width().unwrap_or(0)),
                };
                // A tab's columns are spaces, each its own column.
                let (glyph, repeat, each) = if c == '\t' {
                    (glyph, width, 1)
                } else {
                    (glyph, 1, width)
                };
                for _ in 0..repeat {
                    if column >= first_column && shown < columns {
                        row.push(glyph);
                        shown += each;
                    }
                    column += each;
                }
            }
            row
        })
        .collect()
}

/// Whether `c` would act rather than show: a control character, or one
/// Unicode ignores by default (format, bidirectional, separator, filler and
/// tag characters), which could reorder text or hide text in plain sight.
/// Variation selectors, which only choose a glyph's form, still show.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'..='\u{1160}'
                | '\u{17B4}'..='\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E00FF}'
                | '\u{E01F0}'..='\u{E0FFF}'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme {
            background: [0; 4],
            foreground: [255; 4],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        }
    }

    fn spec(input: Source) -> JobSpec {
        JobSpec::from_job(Job {
            kind: JobKind::Svg,
            source: input,
            theme: theme(),
            canvas: Canvas::White,
            target: Target {
                width: 64,
                height: 32,
                scale: 2.0,
                crop: None,
            },
            fallback_fonts: Vec::new(),
        })
        .expect("room for a few bytes")
    }

    fn digest(bytes: &[u8]) -> Digest {
        kettle_media::content_digest(bytes, None).unwrap()
    }

    /// A spec asks for the same job again: the bytes it carried, or the
    /// file under the authorization it was read with, and every setting.
    #[test]
    fn a_spec_rebuilds_the_job_it_came_from() {
        let svg = b"<svg/>".to_vec();
        let bytes = spec(Source::Bytes(svg.clone()));
        let job = bytes.job().unwrap().job;
        assert_eq!(job.source, Source::Bytes(svg));
        assert_eq!(
            (job.kind, job.canvas, job.target.width, job.target.scale),
            (JobKind::Svg, Canvas::White, 64, 2.0)
        );
        let pulled = Source::user_pull(
            NativePath::new(b"/tmp/plot.svg".to_vec()).unwrap(),
            kettle_media::GuiActionWitness::from_explicit_gui_action(),
        );
        assert_eq!(spec(pulled.clone()).job().unwrap().job.source, pulled);
    }

    /// An item's text is its source exactly as rendered: the bytes a
    /// request carried, or a file's text as the worker read it; never a
    /// raster's, and gone once released, when a carried source cannot be
    /// rendered again but a file can.
    #[test]
    fn an_items_text_is_its_exact_source_until_released() {
        let carried = "<svg>\r\n</svg>";
        let mut inline = ItemSource::new(
            spec(Source::Bytes(carried.as_bytes().to_vec())),
            digest(carried.as_bytes()),
            true,
            Some("ignored: the bytes are the text".into()),
        );
        assert_eq!(inline.text(), Some(carried));
        // A copy shares the bytes the item holds, charged once.
        let SourceInput::Bytes(held) = &inline.spec.input else {
            panic!("carried bytes");
        };
        assert!(Arc::ptr_eq(inline.text_backing().unwrap(), held));
        inline.release();
        assert_eq!(inline.text(), None);
        assert!(matches!(inline.spec.input, SourceInput::Released));
        assert_eq!(
            inline.spec.job().err(),
            Some(FailureCode::WorkerUnavailable)
        );

        let path = NativePath::new(b"/tmp/flow.mmd".to_vec()).unwrap();
        let file_spec = spec(Source::Path {
            path,
            authorization: Authorization::ExternalAttested(kettle_media::ExternalAttested {
                dev: 1,
                ino: 2,
            }),
        });
        let mut file = ItemSource::new(
            file_spec.clone(),
            digest(b"flowchart LR"),
            true,
            Some("flowchart LR\n".into()),
        );
        assert_eq!(file.text(), Some("flowchart LR\n"));
        file.release();
        assert_eq!(file.text(), None);
        assert!(file.spec.job().is_ok(), "a file can be read again");

        let raster = ItemSource::new(file_spec, digest(b"png"), false, None);
        assert_eq!(raster.text(), None);
        let carried_raster = ItemSource::new(
            spec(Source::Bytes(b"BM text".to_vec())),
            digest(b"BM"),
            false,
            None,
        );
        assert_eq!(carried_raster.text(), None, "a raster's bytes are no text");
    }

    /// A gallery holds one to [`MAX_FENCES`] pages.
    #[test]
    fn a_gallery_holds_its_page_limit() {
        let pages = |count: usize| {
            (0..count)
                .map(|n| format!("pie\n  \"{n}\" : 1\n"))
                .collect()
        };
        assert!(Gallery::new(Vec::new()).is_none());
        assert_eq!(Gallery::new(pages(MAX_FENCES)).unwrap().len(), MAX_FENCES);
        assert!(Gallery::new(pages(MAX_FENCES + 1)).is_none());
    }

    /// A page of a gallery is its item's source: it renders as Mermaid from
    /// the bytes kept, which a copy of its source shares, on the job's own
    /// canvas and target; another page takes its place whole, and a page
    /// past the last changes nothing. A gallery read from a file reloads
    /// that file at the page shown, still once its pages are given back.
    #[test]
    fn a_gallery_page_is_its_items_source_until_released() {
        let gallery = Arc::new(
            Gallery::new(vec![
                "flowchart LR\n  A --> B\n".into(),
                "pie\n  \"a\" : 1\n".into(),
            ])
            .unwrap(),
        );
        let path = NativePath::new(b"/tmp/plan.md".to_vec()).unwrap();
        let mut markdown = spec(Source::Path {
            path: path.clone(),
            authorization: Authorization::ExternalAttested(kettle_media::ExternalAttested {
                dev: 1,
                ino: 2,
            }),
        });
        markdown.kind = JobKind::Auto;
        let mut page = ItemSource::page(&markdown, Arc::clone(&gallery), 0).unwrap();
        assert_eq!(page.text(), Some("flowchart LR\n  A --> B\n"));
        assert_eq!(page.text_rows(), 2);
        assert_eq!(page.digest, digest(b"flowchart LR\n  A --> B\n"));
        assert_eq!(page.gallery_page(), Some((0, 2)));
        assert_eq!(page.reload_file(), Some((&path, Some(0))));
        assert!(Arc::ptr_eq(
            page.text_backing().unwrap(),
            gallery.page(0).unwrap()
        ));
        let job = page.spec.job().unwrap().job;
        assert_eq!(job.kind, JobKind::Mermaid);
        assert_eq!(
            job.source,
            Source::Bytes(b"flowchart LR\n  A --> B\n".to_vec())
        );
        assert_eq!((job.canvas, job.target.width), (Canvas::White, 64));

        assert!(page.select(1));
        assert_eq!(page.text(), Some("pie\n  \"a\" : 1\n"));
        assert_eq!(page.digest, digest(b"pie\n  \"a\" : 1\n"));
        assert_eq!(page.gallery_page(), Some((1, 2)));
        assert_eq!(page.reload_file(), Some((&path, Some(1))));
        assert!(!page.select(2));
        assert_eq!(page.gallery_page(), Some((1, 2)));
        assert!(ItemSource::page(&markdown, Arc::clone(&gallery), 2).is_none());
        assert_eq!(
            page.page_spec(0).unwrap().job().unwrap().job.source,
            Source::Bytes(b"flowchart LR\n  A --> B\n".to_vec())
        );

        page.release();
        assert_eq!((page.text(), page.gallery_page()), (None, None));
        assert!(!page.select(0));
        assert_eq!(page.reload_file(), Some((&path, Some(1))));

        let copied = ItemSource::page(
            &spec(Source::Bytes(b"flowchart LR\n  A --> B\n".to_vec())),
            gallery,
            0,
        )
        .unwrap();
        assert_eq!(copied.reload_file(), None, "copied pages have no file");
    }

    /// Nothing to keep needs no room in the account.
    #[test]
    fn empty_bytes_need_no_charge() {
        let empty = ChargedBytes::new(Vec::new()).expect("nothing to charge");
        assert_eq!(empty.as_slice(), b"");
        assert_eq!(format!("{empty:?}"), "ChargedBytes(0 bytes)");
    }

    /// A lane shows the rows in view: tabs at stops of eight, CRs dropped,
    /// anything that would act rather than show as U+FFFD, wide characters
    /// two columns, from the first row and column asked for and no wider.
    #[test]
    fn display_rows_show_what_is_in_view_and_nothing_that_acts() {
        let text = "a\tb\r\n\u{202E}evil\u{0007}\n日本x\nlast\n";
        assert_eq!(
            display_rows(text, 0, 10, 0, 80),
            ["a       b", "\u{FFFD}evil\u{FFFD}", "日本x", "last"]
        );
        assert_eq!(display_rows(text, 2, 1, 0, 80), ["日本x"]);
        assert_eq!(display_rows(text, 3, 5, 0, 80), ["last"], "the end ends it");
        assert_eq!(
            display_rows(text, 0, 1, 4, 80),
            ["    b"],
            "from column four"
        );
        assert_eq!(
            display_rows(text, 2, 1, 2, 80),
            ["本x"],
            "past a wide character"
        );
        assert_eq!(display_rows(text, 0, 1, 0, 3), ["a  "], "three columns");
        assert!(display_rows(text, 9, 3, 0, 80).is_empty());
        assert_eq!(display_rows("", 0, 3, 0, 80), [""]);
        // Tag characters can spell text no one sees; they show as U+FFFD.
        // Variation selectors only choose a form, and show.
        assert_eq!(
            display_rows("a\u{E0001}\u{E0068}b\u{034F}c\u{2764}\u{FE0F}", 0, 1, 0, 80),
            ["a\u{FFFD}\u{FFFD}b\u{FFFD}c\u{2764}\u{FE0F}"]
        );
        // Marks that take no column cannot make a row in view long.
        let marks = format!("x{}", "\u{0301}".repeat(100_000));
        let row = &display_rows(&marks, 0, 1, 0, 80)[0];
        assert!(
            row.len() <= 81 * MAX_ROW_BYTES_PER_COLUMN,
            "{} bytes",
            row.len()
        );
    }

    /// A source's rows are its lines, a final line break ending the last.
    #[test]
    fn text_rows_count_lines_not_a_final_break() {
        let rows = |text: &str| ItemSource::sample(text.as_bytes()).text_rows();
        assert_eq!(rows("<svg/>"), 1);
        assert_eq!(rows("<svg>\n</svg>\n"), 2);
        assert_eq!(rows("<svg>\n\n</svg>"), 3);
        let mut released = ItemSource::sample(b"<svg>\n</svg>");
        released.release();
        assert_eq!(released.text_rows(), 0, "no text, no rows");
        let not_text = ItemSource::sample(b"<svg>\xff</svg>");
        assert_eq!((not_text.text(), not_text.text_rows()), (None, 0));
    }
}

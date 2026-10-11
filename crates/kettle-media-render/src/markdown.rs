//! Markdown jobs: the Mermaid diagrams in one held snapshot of a CommonMark
//! document, as a gallery. Each fenced code block whose info string's first
//! word is `mermaid`, in any case, is a page, in document order, wherever
//! CommonMark puts it: at the top level, in a list item or in a block quote,
//! with its container's indentation removed. An unclosed fence runs to the
//! end of its container, as CommonMark reads it. Indented code, other
//! languages and fences holding only white space are not pages.
//!
//! One reply carries every page and the one asked for rendered, all from
//! the same snapshot, with that snapshot's digest and the document as read:
//! later pages render from those bytes, so a gallery is one version of its
//! file. A document over [`MAX_FENCES`] pages, or with a page over
//! [`MAX_FENCE_BYTES`], is refused whole rather than shown in part.

use std::borrow::Cow;

use kettle_media::{
    FailureCode, Job, MAX_FENCE_BYTES, MAX_FENCES, MAX_MARKDOWN_BYTES, Rendered, content_digest,
};
use merman::OperationControl;
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

use crate::{mermaid, source};

/// Whether a fenced code block's info string names Mermaid.
fn names_mermaid(info: &str) -> bool {
    info.split_whitespace()
        .next()
        .is_some_and(|language| language.eq_ignore_ascii_case("mermaid"))
}

/// The pages of `document`, the text of `snapshot`, in order. Refused over
/// [`MAX_FENCES`] pages or with a page over [`MAX_FENCE_BYTES`], as a file
/// over its size limit or inline bytes over theirs. `control`'s deadline
/// holds before each parser event, the first included.
pub(crate) fn pages(
    document: &str,
    snapshot: &source::Snapshot<'_>,
    control: &OperationControl,
) -> Result<Vec<String>, FailureCode> {
    let too_large = || match snapshot.identity {
        Some(_) => FailureCode::FileTooLarge,
        None => FailureCode::TooLarge,
    };
    let checkpoint = || control.checkpoint().map_err(|_| FailureCode::RenderTimeout);
    snapshot.within(MAX_MARKDOWN_BYTES)?;
    let mut parser = crate::guarded(FailureCode::RenderParse, || Ok(Parser::new(document)))?;
    let mut pages: Vec<String> = Vec::new();
    // The fence being read, and how many bytes it has held: white space
    // past the page limit is not kept, since a fence of white space alone
    // is no page, but it still counts toward the limit of one that is.
    let mut page: Option<(String, usize)> = None;
    loop {
        checkpoint()?;
        let Some(event) = crate::guarded(FailureCode::RenderParse, || Ok(parser.next()))? else {
            break;
        };
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) if names_mermaid(&info) => {
                page = Some((String::new(), 0));
            }
            Event::Text(text) => {
                if let Some((page, held)) = page.as_mut() {
                    *held = held.saturating_add(text.len());
                    if *held > MAX_FENCE_BYTES {
                        if !(page.trim().is_empty() && text.trim().is_empty()) {
                            return Err(too_large());
                        }
                        page.clear();
                        continue;
                    }
                    page.try_reserve(text.len())
                        .map_err(|_| FailureCode::RenderResource)?;
                    page.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((page, _)) = page.take().filter(|(page, _)| !page.trim().is_empty()) {
                    if pages.len() == MAX_FENCES {
                        return Err(too_large());
                    }
                    pages.push(page);
                }
            }
            _ => {}
        }
    }
    Ok(pages)
}

/// Render an explicit Markdown job: page `index` of the document its source
/// holds. The deadline starts before the document is read.
pub(crate) fn render(job: &Job, index: u8) -> Result<Rendered, FailureCode> {
    let control = mermaid::control();
    let snapshot = source::load(&job.source, MAX_MARKDOWN_BYTES)?;
    let document =
        std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::UnsupportedMedia)?;
    let pages = pages(document, &snapshot, &control)?;
    render_page(job, &snapshot, document, pages, index, control)
}

/// Render page `index` of `pages`, read from `document`, the text of
/// `snapshot`: its pixels, beside every page, the document as read and the
/// snapshot's digest. No page means no gallery.
pub(crate) fn render_page(
    job: &Job,
    snapshot: &source::Snapshot<'_>,
    document: &str,
    pages: Vec<String>,
    index: u8,
    control: OperationControl,
) -> Result<Rendered, FailureCode> {
    if pages.is_empty() {
        return Err(FailureCode::UnsupportedMedia);
    }
    let count = u8::try_from(pages.len()).map_err(|_| FailureCode::TooLarge)?;
    let page = pages
        .get(usize::from(index))
        .ok_or(FailureCode::IndexOutOfRange)?;
    // The page renders as the Mermaid it is; what identifies the result is
    // the document it came from.
    let page_snapshot = source::Snapshot {
        bytes: Cow::Borrowed(page.as_bytes()),
        identity: None,
    };
    let mut rendered = mermaid::render_loaded(job, &page_snapshot, control)?;
    rendered.digest =
        content_digest(&snapshot.bytes, snapshot.identity).map_err(|_| FailureCode::BadParams)?;
    rendered.exact_source = Some(document.to_owned());
    rendered.fence_count = count;
    rendered.fence_index = Some(index);
    rendered.fence_sources = pages;
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_media::{Canvas, JobKind, Source, Target, Theme};
    use std::time::Duration;

    fn inline(text: &str) -> source::Snapshot<'_> {
        source::Snapshot {
            bytes: Cow::Borrowed(text.as_bytes()),
            identity: None,
        }
    }

    fn pages_of(document: &str) -> Result<Vec<String>, FailureCode> {
        pages(document, &inline(document), &mermaid::control())
    }

    fn job(document: &str, index: u8) -> Job {
        Job {
            kind: JobKind::MarkdownDiagrams { index },
            source: Source::Bytes(document.as_bytes().to_vec()),
            target: Target {
                width: 320,
                height: 200,
                scale: 1.0,
                crop: None,
            },
            theme: Theme {
                background: [20, 24, 30, 255],
                foreground: [240, 242, 244, 255],
                palette: [[160, 100, 180, 255]; 16],
                accent: [125, 207, 255, 255],
                is_dark: true,
            },
            canvas: Canvas::Theme,
            fallback_fonts: Vec::new(),
        }
    }

    fn fence(body: &str) -> String {
        format!("```mermaid\n{body}\n```\n")
    }

    /// Mermaid fences are pages in document order, backtick or tilde, the
    /// language in any case and with attributes after it, inside list items
    /// and block quotes with their indentation removed. Other languages,
    /// indented code, fences in no language and blank fences are not pages.
    #[test]
    fn mermaid_fences_are_pages_in_document_order() {
        let document = concat!(
            "# Notes\n\n",
            "```mermaid\nflowchart LR\n  A --> B\n```\n\n",
            "```rust\nfn main() {}\n```\n\n",
            "    flowchart TD\n    indented --> code\n\n",
            "~~~ Mermaid title=\"second\"\nsequenceDiagram\n  A->>B: hi\n~~~\n\n",
            "```\nflowchart LR\n  no --> language\n```\n\n",
            "- a list item\n\n  ```MERMAID\n  pie\n    \"a\" : 1\n  ```\n\n",
            "> ```mermaid\n> graph TD\n>   quoted --> fence\n> ```\n\n",
            "```mermaid\n   \n```\n",
            "```mermaid-ish\nflowchart LR\n```\n",
        );
        assert_eq!(
            pages_of(document).unwrap(),
            [
                "flowchart LR\n  A --> B\n",
                "sequenceDiagram\n  A->>B: hi\n",
                "pie\n  \"a\" : 1\n",
                "graph TD\n  quoted --> fence\n",
            ]
        );
    }

    /// An unclosed fence runs to the end of the document, as CommonMark has
    /// it; a longer closing fence closes it, a shorter one is its text.
    #[test]
    fn fences_open_and_close_as_commonmark_reads_them() {
        assert_eq!(
            pages_of("````mermaid\nflowchart LR\n```\nA --> B\n`````\nafter\n").unwrap(),
            ["flowchart LR\n```\nA --> B\n"]
        );
        assert_eq!(
            pages_of("text\n\n```mermaid\nflowchart LR\n  A --> B\n").unwrap(),
            ["flowchart LR\n  A --> B\n"]
        );
        assert_eq!(
            pages_of("plain prose, no fences").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(pages_of("").unwrap(), Vec::<String>::new());
    }

    /// Thirty-two pages are a gallery and a thirty-third refuses the
    /// document; blank fences do not count. A page at the fence limit is
    /// kept, one byte over refuses the document, wherever the page is. A
    /// file is refused as a file over its limit.
    #[test]
    fn a_gallery_keeps_its_page_and_byte_limits() {
        let many = |count: usize| {
            (0..count)
                .map(|n| fence(&format!("flowchart LR\n  A{n} --> B")))
                .collect::<String>()
        };
        let full = many(MAX_FENCES) + &fence(" ");
        assert_eq!(pages_of(&full).unwrap().len(), MAX_FENCES);
        assert_eq!(pages_of(&many(MAX_FENCES + 1)), Err(FailureCode::TooLarge));
        let file = many(MAX_FENCES + 1);
        let snapshot = source::Snapshot {
            bytes: Cow::Borrowed(file.as_bytes()),
            identity: Some(kettle_media::PathIdentity {
                dev: 1,
                ino: 2,
                size: file.len() as u64,
                mtime_seconds: 3,
                mtime_nanos: 4,
            }),
        };
        assert_eq!(
            pages(&file, &snapshot, &mermaid::control()),
            Err(FailureCode::FileTooLarge)
        );

        // "```mermaid\n" and "\n```\n" around a body of `len` bytes whose
        // page, with its final line break, is `len + 1` bytes.
        let body = |len: usize| format!("flowchart LR\n%%{}", "x".repeat(len - 15));
        let at_limit = fence(&body(MAX_FENCE_BYTES - 1));
        assert_eq!(pages_of(&at_limit).unwrap()[0].len(), MAX_FENCE_BYTES);
        let over = fence(&body(MAX_FENCE_BYTES));
        assert_eq!(pages_of(&over), Err(FailureCode::TooLarge));
        assert_eq!(
            pages_of(&(many(2) + &over + &many(1))),
            Err(FailureCode::TooLarge)
        );
        // White space alone is no page, however much of it; past the limit,
        // the text after it still counts toward it.
        let blank = fence(&" ".repeat(MAX_FENCE_BYTES * 2));
        assert_eq!(pages_of(&(many(1) + &blank)).unwrap().len(), 1);
        let late = fence(&format!("{}\nflowchart LR", " ".repeat(MAX_FENCE_BYTES)));
        assert_eq!(pages_of(&late), Err(FailureCode::TooLarge));
        // In a block quote each row is read on its own: the white space
        // still counts once the text comes.
        let quoted = format!(
            "> ```mermaid\n> {}\n> flowchart LR\n> ```\n",
            " ".repeat(MAX_FENCE_BYTES)
        );
        assert_eq!(pages_of(&quoted), Err(FailureCode::TooLarge));
    }

    /// The document limit is its own: a document at it parses, one byte
    /// over is refused before it is parsed.
    #[test]
    fn a_document_keeps_its_byte_limit() {
        let mut document = fence("flowchart LR\n  A --> B");
        document.push_str(&"x".repeat(MAX_MARKDOWN_BYTES - document.len()));
        assert_eq!(pages_of(&document).unwrap().len(), 1);
        document.push('x');
        assert_eq!(pages_of(&document), Err(FailureCode::TooLarge));
    }

    /// Text built to make a parser work hard stays within the document
    /// limit and comes back well inside a diagram's deadline.
    #[test]
    fn hostile_markdown_at_the_limit_stays_fast() {
        for document in [
            ">".repeat(MAX_MARKDOWN_BYTES),
            "- ".repeat(MAX_MARKDOWN_BYTES / 2),
            "`".repeat(MAX_MARKDOWN_BYTES),
            "[".repeat(MAX_MARKDOWN_BYTES),
            "*a".repeat(MAX_MARKDOWN_BYTES / 2),
            "```mermaid\n".repeat(MAX_MARKDOWN_BYTES / 11),
        ] {
            let started = std::time::Instant::now();
            let _ = pages_of(&document);
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "{:?}",
                &document[..12]
            );
        }
    }

    /// The deadline holds through extraction.
    #[test]
    fn extraction_keeps_the_deadline() {
        let document = fence("flowchart LR\n  A --> B");
        assert_eq!(
            pages(
                &document,
                &inline(&document),
                &OperationControl::new().with_deadline(Duration::ZERO)
            ),
            Err(FailureCode::RenderTimeout)
        );
    }

    /// The page asked for is rendered beside every page, with the document
    /// as read and the document's digest; another page renders other
    /// pixels from the same snapshot.
    #[test]
    fn a_page_renders_with_every_page_and_the_documents_identity() {
        let document = format!(
            "# Two diagrams\n\n{}\ntext\n\n{}",
            fence("flowchart LR\n  A --> B"),
            fence("flowchart TD\n  C[Wide WWWW] --> D[Another]\n  D --> E")
        );
        let first = crate::render(&job(&document, 0)).unwrap();
        first
            .validate_as(kettle_media::MediaKind::Markdown)
            .unwrap();
        assert_eq!(
            first.fence_sources,
            [
                "flowchart LR\n  A --> B\n",
                "flowchart TD\n  C[Wide WWWW] --> D[Another]\n  D --> E\n"
            ]
        );
        assert_eq!((first.fence_count, first.fence_index), (2, Some(0)));
        assert_eq!(first.exact_source.as_deref(), Some(document.as_str()));
        assert_eq!(
            first.digest,
            content_digest(document.as_bytes(), None).unwrap()
        );
        assert_eq!(first.source_text, ["flowchart LR", "  A --> B"]);
        let second = crate::render(&job(&document, 1)).unwrap();
        assert_eq!((second.fence_count, second.fence_index), (2, Some(1)));
        assert_eq!(second.digest, first.digest);
        assert_eq!(second.fence_sources, first.fence_sources);
        assert_ne!(
            (second.width, second.height, second.rgba),
            (first.width, first.height, first.rgba)
        );
    }

    /// No page is no gallery, a page past the last is out of range, a page
    /// that is not a diagram fails as Mermaid does, and text that is not
    /// UTF-8 is not Markdown.
    #[test]
    fn a_gallery_refuses_with_fixed_failures() {
        let document = fence("flowchart LR\n  A --> B");
        assert_eq!(
            crate::render(&job("no diagrams here", 0)),
            Err(FailureCode::UnsupportedMedia)
        );
        assert_eq!(
            crate::render(&job(&document, 1)),
            Err(FailureCode::IndexOutOfRange)
        );
        assert_eq!(
            crate::render(&job(&fence("flowchart TD\n A[unterminated"), 0)),
            Err(FailureCode::RenderParse)
        );
        let mut binary = job(&document, 0);
        binary.source = Source::Bytes(vec![0xff, 0xfe, b'\n']);
        assert_eq!(crate::render(&binary), Err(FailureCode::UnsupportedMedia));
        let mut large = job(&document, 0);
        large.source = Source::Bytes(vec![b'x'; MAX_MARKDOWN_BYTES + 1]);
        assert_eq!(crate::render(&large), Err(FailureCode::TooLarge));
    }

    /// The page rendered and the pages returned come from the one snapshot
    /// read, whatever the file holds by then.
    #[test]
    fn pages_and_pixels_come_from_one_snapshot() {
        use std::os::unix::fs::MetadataExt as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("notes.md");
        let before = fence("flowchart LR\n  A --> B");
        std::fs::write(&path, &before).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        let mut job = job("", 0);
        job.source = Source::Path {
            path: kettle_media::NativePath::from_path(&path).unwrap(),
            authorization: kettle_media::Authorization::ExternalAttested(
                kettle_media::ExternalAttested {
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                },
            ),
        };
        let snapshot = source::load(&job.source, MAX_MARKDOWN_BYTES).unwrap();
        std::fs::write(
            &path,
            fence("pie\n  \"a\" : 1") + &fence("pie\n  \"b\" : 2"),
        )
        .unwrap();
        let document = std::str::from_utf8(&snapshot.bytes).unwrap();
        let control = mermaid::control();
        let pages = pages(document, &snapshot, &control).unwrap();
        let rendered = render_page(&job, &snapshot, document, pages, 0, control).unwrap();
        assert_eq!(rendered.fence_sources, ["flowchart LR\n  A --> B\n"]);
        assert_eq!(rendered.exact_source.as_deref(), Some(before.as_str()));
        assert_eq!(rendered.digest.path_identity, snapshot.identity);
        assert_eq!(
            rendered.digest,
            content_digest(before.as_bytes(), snapshot.identity).unwrap()
        );
    }
}

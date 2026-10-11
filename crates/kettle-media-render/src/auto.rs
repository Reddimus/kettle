//! Auto jobs: one held snapshot of a file, classified by its bytes rather than
//! its name, then rendered as what it is. The snapshot is read once, within
//! the largest input cap of the kinds it can be, and the actual kind's own cap
//! applies before anything decodes it. Raster is recognized by its magic
//! bytes; SVG is UTF-8 markup whose root element is `svg`; other UTF-8 text
//! goes to the Mermaid renderer, which recognizes a diagram with its own
//! preprocessing as it parses and refuses anything else. Anything else is
//! `UnsupportedMedia`.

use kettle_media::{FailureCode, Job, JobKind, MediaKind, Rendered};

use crate::{mermaid, raster, source, svg};

pub(crate) fn render(
    job: &Job,
    on_kind: &mut impl FnMut(MediaKind),
) -> Result<(MediaKind, Rendered), FailureCode> {
    let snapshot = source::load(&job.source, job.kind.input_cap())?;
    render_loaded(job, &snapshot, on_kind)
}

fn render_loaded(
    job: &Job,
    snapshot: &source::Snapshot<'_>,
    on_kind: &mut impl FnMut(MediaKind),
) -> Result<(MediaKind, Rendered), FailureCode> {
    if image::guess_format(&snapshot.bytes).is_ok() {
        snapshot.within(JobKind::Raster.input_cap())?;
        on_kind(MediaKind::Raster);
        return raster::render_loaded(job, snapshot).map(|rendered| (MediaKind::Raster, rendered));
    }
    let text = std::str::from_utf8(&snapshot.bytes).map_err(|_| FailureCode::UnsupportedMedia)?;
    if svg::is_markup(text) {
        return svg::render_auto(job, snapshot, || on_kind(MediaKind::Svg))
            .map(|rendered| (MediaKind::Svg, rendered));
    }
    // Mermaid is known to be Mermaid only once it parses as a diagram, so
    // the kind is heard after rendering; the renderer keeps its own
    // deadline, which starts here, before its fonts load.
    snapshot.within(JobKind::Mermaid.input_cap())?;
    let rendered = mermaid::render_loaded(job, snapshot, mermaid::control())?;
    on_kind(MediaKind::Mermaid);
    Ok((MediaKind::Mermaid, rendered))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgba, RgbaImage};
    use kettle_media::{
        Authorization, Canvas, ExternalAttested, NativePath, Source, Target, Theme, content_digest,
    };
    use std::os::unix::fs::MetadataExt as _;
    use std::path::Path;

    fn job(path: &Path) -> Job {
        let metadata = std::fs::metadata(path).unwrap();
        Job {
            kind: JobKind::Auto,
            source: Source::Path {
                path: NativePath::from_path(path).unwrap(),
                authorization: Authorization::ExternalAttested(ExternalAttested {
                    dev: metadata.dev(),
                    ino: metadata.ino(),
                }),
            },
            target: Target {
                width: 2,
                height: 1,
                scale: 1.0,
                crop: None,
            },
            theme: Theme {
                background: [0; 4],
                foreground: [255; 4],
                palette: [[0; 4]; 16],
                accent: [0; 4],
                is_dark: true,
            },
            canvas: Canvas::White,
            fallback_fonts: vec![],
        }
    }

    fn png(path: &Path) {
        RgbaImage::from_pixel(2, 1, Rgba([10, 200, 30, 255]))
            .save_with_format(path, ImageFormat::Png)
            .unwrap();
    }

    const SVG: &str = concat!(
        "\u{feff}<?xml version=\"1.0\"?>\n<!-- generated diagram -->\n",
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"2\" height=\"1\">",
        "<rect width=\"2\" height=\"1\" fill=\"rgb(10,200,30)\"/></svg>"
    );

    /// Every kind this reports, in order, beside the result.
    fn classify(job: &Job) -> (Vec<MediaKind>, Result<(MediaKind, Rendered), FailureCode>) {
        let mut kinds = Vec::new();
        let result = render(job, &mut |kind| kinds.push(kind));
        (kinds, result)
    }

    #[test]
    fn raster_content_is_raster_whatever_the_file_is_called() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["diagram.bin", "diagram.svg", "diagram"] {
            let path = directory.path().join(name);
            png(&path);
            let (kinds, result) = classify(&job(&path));
            let (kind, output) = result.unwrap();
            assert_eq!((kind, kinds), (MediaKind::Raster, vec![MediaKind::Raster]));
            assert_eq!((output.width, output.height), (2, 1));
            assert_eq!(output.rgba, [10, 200, 30, 255].repeat(2));
            assert!(output.digest.path_identity.is_some());
        }
    }

    #[test]
    fn declared_svg_is_svg_whatever_the_file_is_called() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["diagram.bin", "diagram.png"] {
            let path = directory.path().join(name);
            std::fs::write(&path, SVG).unwrap();
            let (kinds, result) = classify(&job(&path));
            let (kind, output) = result.unwrap();
            assert_eq!((kind, kinds), (MediaKind::Svg, vec![MediaKind::Svg]));
            assert_eq!((output.width, output.height), (2, 1));
            assert_eq!(output.rgba, [10, 200, 30, 255].repeat(2));
            assert!(!output.source_text.is_empty());
        }
    }

    #[test]
    fn classification_and_rendering_use_one_loaded_file_version() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagram.bin");
        png(&path);
        let job = job(&path);
        let snapshot = source::load(&job.source, job.kind.input_cap()).unwrap();
        std::fs::write(&path, SVG).unwrap();
        let (kind, output) = render_loaded(&job, &snapshot, &mut |_| {}).unwrap();
        assert_eq!(kind, MediaKind::Raster);
        assert_eq!(output.rgba, [10, 200, 30, 255].repeat(2));
        assert_eq!(output.digest.path_identity, snapshot.identity);
        assert_eq!(
            output.digest,
            content_digest(&snapshot.bytes, snapshot.identity).unwrap()
        );
    }

    #[test]
    fn other_content_is_unsupported_and_reports_no_kind() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagram.svg");
        for bytes in [
            b"ordinary prose".as_slice(),
            b"<html><body>ordinary page</body></html>".as_slice(),
            b"\xff\xfe not text and not an image".as_slice(),
            b"".as_slice(),
        ] {
            std::fs::write(&path, bytes).unwrap();
            let (kinds, result) = classify(&job(&path));
            assert_eq!(result, Err(FailureCode::UnsupportedMedia), "{bytes:?}");
            assert!(kinds.is_empty());
        }
    }

    /// Text that is a Mermaid diagram is Mermaid whatever the file is
    /// called, heard once it has rendered; one that only starts like a
    /// diagram is a parse failure, never another kind.
    #[test]
    fn mermaid_text_is_mermaid_whatever_the_file_is_called() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["diagram.bin", "diagram.svg", "notes.txt"] {
            let path = directory.path().join(name);
            std::fs::write(&path, b"flowchart TD\nA --> B").unwrap();
            let (kinds, result) = classify(&job(&path));
            let (kind, rendered) = result.unwrap();
            assert_eq!(
                (kind, kinds),
                (MediaKind::Mermaid, vec![MediaKind::Mermaid])
            );
            assert_eq!(rendered.source_text, ["flowchart TD", "A --> B"]);
            assert!(
                rendered
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] != 0)
            );
        }
        let path = directory.path().join("broken.mmd");
        std::fs::write(&path, b"flowchart TD\n A[unterminated").unwrap();
        let (kinds, result) = classify(&job(&path));
        assert_eq!(result.map(|(kind, _)| kind), Err(FailureCode::RenderParse));
        assert!(kinds.is_empty());
    }

    /// Markup whose root is not `svg` is unsupported however large it is
    /// and however hard it would be to parse; SVG behind a document type
    /// declaration is SVG, refused as an explicit SVG job is.
    #[test]
    fn the_root_decides_before_the_svg_cap_and_parser() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("page.svg");
        let mut large = String::from("<html>");
        large.push_str(&" ".repeat(JobKind::Svg.input_cap()));
        large.push_str("</html>");
        let attributes: String = (0..257).map(|n| format!(" a{n}=''")).collect();
        for text in [
            large,
            format!("<html{attributes}/>"),
            "<?xml version='1.0'?><!-- <svg/> --><html/>".into(),
        ] {
            std::fs::write(&path, &text).unwrap();
            let (kinds, result) = classify(&job(&path));
            assert_eq!(result, Err(FailureCode::UnsupportedMedia));
            assert!(kinds.is_empty());
        }
        let declared = format!(
            "<!DOCTYPE svg PUBLIC '-//W3C//DTD SVG 1.1//EN' 'svg11.dtd'>{}",
            SVG.trim_start_matches('\u{feff}')
                .trim_start_matches("<?xml version=\"1.0\"?>\n")
        );
        std::fs::write(&path, &declared).unwrap();
        let (kinds, result) = classify(&job(&path));
        assert_eq!(
            (kinds, result),
            (vec![MediaKind::Svg], Err(FailureCode::RenderParse))
        );
        let mut explicit = job(&path);
        explicit.kind = JobKind::Svg;
        assert_eq!(crate::render(&explicit), Err(FailureCode::RenderParse));
    }

    /// The snapshot is read under the raster cap, the largest; SVG content
    /// over its own, smaller cap is refused before it is parsed, as an
    /// explicit SVG job's would be.
    #[test]
    fn svg_content_keeps_its_own_input_cap() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagram.svg");
        let mut svg = String::from(SVG);
        svg.push_str(&" ".repeat(JobKind::Svg.input_cap() + 1 - svg.len()));
        assert!(svg.len() < JobKind::Auto.input_cap());
        std::fs::write(&path, &svg).unwrap();
        let mut file_job = job(&path);
        let (kinds, result) = classify(&file_job);
        assert_eq!(result, Err(FailureCode::FileTooLarge));
        assert!(kinds.is_empty());
        file_job.kind = JobKind::Svg;
        assert_eq!(crate::render(&file_job), Err(FailureCode::FileTooLarge));

        let mut inline = job(&path);
        inline.source = Source::Bytes(svg.clone().into_bytes());
        let (kinds, result) = classify(&inline);
        assert_eq!(result, Err(FailureCode::TooLarge));
        assert!(kinds.is_empty());
        svg.pop();
        inline.source = Source::Bytes(svg.into_bytes());
        assert_eq!(classify(&inline).1.unwrap().0, MediaKind::Svg);
    }

    /// A raster is read under its own cap, not the smaller SVG one.
    #[test]
    fn a_raster_over_the_svg_cap_still_renders() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("photo.bin");
        RgbaImage::from_pixel(1024, 768, Rgba([10, 200, 30, 255]))
            .save_with_format(&path, ImageFormat::Bmp)
            .unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > JobKind::Svg.input_cap() as u64);
        let (kinds, result) = classify(&job(&path));
        let (kind, output) = result.unwrap();
        assert_eq!((kind, kinds), (MediaKind::Raster, vec![MediaKind::Raster]));
        assert_eq!(output.rgba, [10, 200, 30, 255]);
    }

    #[test]
    fn auto_reads_no_more_than_the_largest_kind_allows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("diagram.bin");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(JobKind::Auto.input_cap() as u64 + 1).unwrap();
        let (kinds, result) = classify(&job(&path));
        assert_eq!(result, Err(FailureCode::FileTooLarge));
        assert!(kinds.is_empty());
    }

    /// Bytes an Auto job carries inline are classified the same way.
    #[test]
    fn inline_bytes_are_classified_by_content() {
        let mut job = job(Path::new("/"));
        job.source = Source::Bytes(SVG.as_bytes().to_vec());
        let (kinds, result) = classify(&job);
        assert_eq!(result.unwrap().0, MediaKind::Svg);
        assert_eq!(kinds, [MediaKind::Svg]);
    }
}

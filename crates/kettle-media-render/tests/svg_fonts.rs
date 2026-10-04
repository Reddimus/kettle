//! Explicit font files, selected collection faces and actual glyph warnings.
#![cfg(any(target_os = "macos", target_os = "linux"))]

#[path = "support/fonts.rs"]
mod fonts;

use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use kettle_media::{
    Canvas, FailureCode, FallbackFont, Job, JobKind, NativePath, Rendered, Source, Target, Theme,
    Warning,
};
use kettle_media_render::render;

fn job(text: &str, family: &str) -> Job {
    Job {
        kind: JobKind::Svg,
        source: Source::Bytes(
            format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"64\"><text x=\"2\" y=\"42\" font-size=\"32\" font-family=\"{family}\">{text}</text></svg>"
            )
            .into_bytes(),
        ),
        theme: Theme {
            background: [0; 4],
            foreground: [255; 4],
            palette: [[0; 4]; 16],
            accent: [0; 4],
            is_dark: true,
        },
        canvas: Canvas::Theme,
        target: Target {
            width: 160,
            height: 64,
            scale: 1.0,
            crop: None,
        },
        fallback_fonts: Vec::new(),
    }
}

fn fallback(path: &Path, face_index: u32) -> FallbackFont {
    FallbackFont {
        path: NativePath::new(path.as_os_str().as_bytes().to_vec()).unwrap(),
        face_index,
    }
}

fn with_font(path: &Path, face_index: u32, text: &str) -> Result<Rendered, FailureCode> {
    let mut job = job(text, fonts::FAMILY);
    job.fallback_fonts.push(fallback(path, face_index));
    render(&job)
}

#[test]
fn generated_fonts_have_valid_outline_faces() {
    let (bytes, _, _) = fonts::collection();
    let mut database = usvg::fontdb::Database::new();
    // Only trusted generated fixture bytes go through the all-face loader.
    database.load_font_data(bytes);
    let ids: Vec<_> = database.faces().map(|face| (face.id, face.index)).collect();
    assert_eq!(ids.len(), 2);
    assert_eq!((ids[0].1, ids[1].1), (0, 1));
    database.remove_face(ids[0].0);
    let options = usvg::Options {
        fontdb: std::sync::Arc::new(database),
        font_family: fonts::FAMILY.into(),
        ..usvg::Options::default()
    };
    let tree = usvg::Tree::from_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"64\"><text y=\"42\" font-size=\"32\">A\u{4e2d}</text></svg>",
        &options,
    )
    .unwrap();
    let mut groups = vec![tree.root()];
    let mut glyphs = 0;
    while let Some(group) = groups.pop() {
        for node in group.children() {
            match node {
                usvg::Node::Group(group) => groups.push(group),
                usvg::Node::Text(text) => {
                    for span in text.layouted() {
                        for glyph in &span.positioned_glyphs {
                            assert_ne!(glyph.id.0, 0, "fixture must cover A and Han");
                            glyphs += 1;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    assert_eq!(glyphs, 2);
}

#[test]
fn svg_fallback_face_index_is_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let collection_path = directory.path().join("faces.ttc");
    let first_path = directory.path().join("first.ttf");
    let second_path = directory.path().join("second.ttf");
    let (collection, first, second) = fonts::collection();
    std::fs::write(&collection_path, collection).unwrap();
    std::fs::write(&first_path, first).unwrap();
    std::fs::write(&second_path, second).unwrap();
    let selected = with_font(&collection_path, 1, "AAAA").unwrap();
    let standalone = with_font(&second_path, 0, "AAAA").unwrap();
    let other = with_font(&collection_path, 0, "AAAA").unwrap();
    assert!(
        selected.rgba == standalone.rgba,
        "selected face differs from its standalone font"
    );
    assert!(
        selected.rgba != other.rgba,
        "the second face has double A advance"
    );
    assert!(other.rgba == with_font(&first_path, 0, "AAAA").unwrap().rgba);
    assert!(selected.warnings.contains(&Warning::FontFallback));
    assert_eq!(
        with_font(&collection_path, 2, "AAAA").unwrap_err(),
        FailureCode::RenderParse
    );
    assert_eq!(
        with_font(&second_path, 1, "AAAA").unwrap_err(),
        FailureCode::RenderParse
    );
}

#[test]
fn svg_only_explicit_font_bytes_are_loaded() {
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("fallback.ttf");
    std::fs::write(&font, fonts::single(Some('\u{4e2d}'), false)).unwrap();
    // Merely existing on disk, or naming its family, cannot load this face.
    let absent = render(&job("\u{4e2d}", fonts::FAMILY)).unwrap();
    assert!(absent.warnings.contains(&Warning::MissingGlyphs));
    assert_eq!(absent.uncovered_scripts, ["Han"]);
    let supplied = with_font(&font, 0, "\u{4e2d}").unwrap();
    assert!(supplied.warnings.contains(&Warning::FontFallback));
    assert!(!supplied.warnings.contains(&Warning::MissingGlyphs));
    assert!(supplied.uncovered_scripts.is_empty());
    assert!(
        absent.rgba != supplied.rgba,
        "explicit font must change the glyph"
    );
    // The per-job database cannot retain the preceding job's extra face.
    let later = render(&job("\u{4e2d}", fonts::FAMILY)).unwrap();
    assert!(
        later.rgba == absent.rgba,
        "fallback face leaked across jobs"
    );
    assert_eq!(later.warnings, absent.warnings);
}

#[test]
fn fallback_is_selected_for_glyphs_not_covered_by_the_primary_face() {
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("fallback.ttf");
    std::fs::write(&font, fonts::single(Some('\u{4e2d}'), false)).unwrap();
    let mut job = job("A\u{4e2d}", "monospace");
    job.fallback_fonts.push(fallback(&font, 0));
    let rendered = render(&job).unwrap();
    assert!(rendered.warnings.contains(&Warning::FontFallback));
    assert!(!rendered.warnings.contains(&Warning::MissingGlyphs));
    assert!(rendered.uncovered_scripts.is_empty());
    job.source = self::job("A\u{4e2d}\u{5d0}\u{627}", "monospace").source;
    let missing = render(&job).unwrap();
    assert!(missing.warnings.contains(&Warning::MissingGlyphs));
    assert!(missing.warnings.contains(&Warning::FontFallback));
    assert_eq!(missing.uncovered_scripts, ["Arabic", "Hebrew"]);
}

#[test]
fn an_uncovered_character_does_not_hide_later_available_glyphs() {
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("fallback.ttf");
    std::fs::write(&font, fonts::single(Some('\u{4e2d}'), false)).unwrap();
    let mut job = job("\u{2ffff}\u{4e2d}", "monospace");
    job.fallback_fonts.push(fallback(&font, 0));
    let rendered = render(&job).unwrap();
    assert!(rendered.warnings.contains(&Warning::FontFallback));
    assert!(rendered.warnings.contains(&Warning::MissingGlyphs));
    assert_eq!(rendered.uncovered_scripts, ["Unknown"]);
}

#[test]
fn supplied_fonts_have_regular_file_and_byte_limits() {
    let directory = tempfile::tempdir().unwrap();
    assert_eq!(
        with_font(directory.path(), 0, "AAAA").unwrap_err(),
        FailureCode::FileNotRegular
    );
    assert_eq!(
        with_font(&directory.path().join("missing.ttf"), 0, "AAAA").unwrap_err(),
        FailureCode::FileNotFound
    );
    let font = directory.path().join("large.ttf");
    std::fs::File::create(&font)
        .unwrap()
        .set_len(32 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        with_font(&font, 0, "AAAA").unwrap_err(),
        FailureCode::FileTooLarge
    );
    let mut job = job("AAAA", "monospace");
    job.fallback_fonts = vec![fallback(&font, 0); 9];
    assert_eq!(render(&job).unwrap_err(), FailureCode::BadParams);
}

#[test]
fn invalid_fonts_and_embedded_glyph_formats_are_refused() {
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("invalid.ttf");
    for bytes in [
        b"not a font".to_vec(),
        b"ttcf\0\x01\0\0\xff\xff\xff\xff".to_vec(),
    ] {
        std::fs::write(&font, bytes).unwrap();
        assert_eq!(
            with_font(&font, 0, "AAAA").unwrap_err(),
            FailureCode::RenderParse
        );
    }
    for tag in [
        *b"SVG ", *b"COLR", *b"CPAL", *b"CBDT", *b"CBLC", *b"sbix", *b"EBDT", *b"EBLC", *b"EBSC",
        *b"bdat", *b"bloc",
    ] {
        let mut tables = fonts::tables();
        tables.insert(tag, vec![0]);
        std::fs::write(&font, fonts::sfnt(tables)).unwrap();
        assert_eq!(
            with_font(&font, 0, "AAAA").unwrap_err(),
            FailureCode::RenderParse,
            "{tag:?}"
        );
    }
}

#[test]
fn aggregate_font_bytes_and_exact_per_file_boundary_are_enforced() {
    use kettle_media::{MAX_FALLBACK_FONT_BYTES, MAX_FALLBACK_FONT_TOTAL_BYTES};
    assert_eq!(MAX_FALLBACK_FONT_BYTES, 32 * 1024 * 1024);
    assert_eq!(MAX_FALLBACK_FONT_TOTAL_BYTES, 128 * 1024 * 1024);
    use std::io::Write as _;
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("padded.ttf");
    let mut file = std::fs::File::create(&font).unwrap();
    file.write_all(&fonts::single(None, false)).unwrap();
    file.set_len(MAX_FALLBACK_FONT_BYTES as u64).unwrap();
    drop(file);
    let mut job = job("A", fonts::FAMILY);
    job.fallback_fonts = vec![fallback(&font, 0); 4];
    assert!(render(&job).is_ok(), "exactly 128 MiB must be admitted");
    job.fallback_fonts.push(fallback(&font, 0));
    assert_eq!(render(&job).unwrap_err(), FailureCode::FileTooLarge);
}

#[test]
fn font_metadata_has_bounded_tables_names_cmaps_and_variation_axes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("metadata.ttf");
    let mut cases = Vec::new();
    let mut tables = fonts::tables();
    tables.get_mut(b"name").unwrap().resize(64 * 1024 + 1, 0);
    cases.push(tables);
    let mut tables = fonts::tables();
    let mut names = vec![0, 0];
    names.extend(513_u16.to_be_bytes());
    names.extend((6 + 12 * 513_u16).to_be_bytes());
    for _ in 0..513 {
        names.extend([0, 3, 0, 1, 4, 9, 0, 1, 0, 2, 0, 0]);
    }
    names.extend([0, b'A']);
    tables.insert(*b"name", names);
    cases.push(tables);
    let mut tables = fonts::tables();
    // A valid format-0 name table with one overlong UTF-16 family name.
    let mut names = vec![0, 0, 0, 1, 0, 18, 0, 3, 0, 1, 4, 9, 0, 1];
    names.extend(8194_u16.to_be_bytes());
    names.extend([0, 0]);
    for _ in 0..4097 {
        names.extend([0, b'A']);
    }
    tables.insert(*b"name", names);
    cases.push(tables);
    let mut tables = fonts::tables();
    let mut cmap = vec![0, 0, 0, 65];
    for _ in 0..65 {
        cmap.extend([0, 3, 0, 10]);
        cmap.extend((4 + 65 * 8_u32).to_be_bytes());
    }
    cmap.extend([0, 12, 0, 0]);
    cmap.extend(28_u32.to_be_bytes());
    cmap.extend(0_u32.to_be_bytes());
    cmap.extend(1_u32.to_be_bytes());
    cmap.extend(u32::from('A').to_be_bytes());
    cmap.extend(u32::from('A').to_be_bytes());
    use skrifa::MetadataProvider as _;
    let bundled = skrifa::FontRef::new(include_bytes!(
        "../../../assets/fonts/JetBrainsMonoNerdFont-Regular.ttf"
    ))
    .unwrap();
    cmap.extend(bundled.charmap().map('A').unwrap().to_u32().to_be_bytes());
    tables.insert(*b"cmap", cmap);
    cases.push(tables);
    let mut tables = fonts::tables();
    // fvar 1.0, axis-array offset 16, 65 twenty-byte axis records.
    let mut fvar = vec![0, 1, 0, 0, 0, 16, 0, 2, 0, 65, 0, 20, 0, 0, 1, 8];
    fvar.resize(16 + 65 * 20, 0);
    tables.insert(*b"fvar", fvar);
    cases.push(tables);
    let mut tables = fonts::tables();
    for i in 0..128 {
        tables.insert(
            format!("Z{i:03}").as_bytes().try_into().unwrap(),
            Vec::new(),
        );
    }
    cases.push(tables);
    for (i, tables) in cases.into_iter().enumerate() {
        std::fs::write(&path, fonts::sfnt(tables)).unwrap();
        assert_eq!(
            with_font(&path, 0, "A").unwrap_err(),
            FailureCode::RenderResource,
            "case {i}"
        );
    }
}

#[test]
fn unused_fonts_and_invisible_text_do_not_warn() {
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("fallback.ttf");
    std::fs::write(&font, fonts::single(Some('\u{4e2d}'), false)).unwrap();
    let mut job = job("A", "monospace");
    job.fallback_fonts.push(fallback(&font, 0));
    let rendered = render(&job).unwrap();
    assert!(rendered.warnings.is_empty());
    assert!(rendered.uncovered_scripts.is_empty());
    job.source = Source::Bytes("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"64\"><text visibility=\"hidden\">\u{4e2d}\u{5d0}</text><text fill=\"none\" stroke=\"none\">\u{627}</text></svg>".as_bytes().to_vec());
    assert!(render(&job).unwrap().warnings.is_empty());
}

#[test]
fn text_in_a_pattern_reports_actual_font_use() {
    let directory = tempfile::tempdir().unwrap();
    let font = directory.path().join("fallback.ttf");
    std::fs::write(&font, fonts::single(Some('\u{4e2d}'), false)).unwrap();
    let mut job = job("", "monospace");
    job.fallback_fonts.push(fallback(&font, 0));
    job.source = Source::Bytes("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"160\" height=\"64\"><defs><pattern id=\"p\" width=\"1\" height=\"1\"><text y=\"32\" font-size=\"32\">\u{4e2d}\u{5d0}</text></pattern></defs><rect width=\"160\" height=\"64\" fill=\"url(#p)\"/></svg>".as_bytes().to_vec());
    let rendered = render(&job).unwrap();
    assert!(rendered.warnings.contains(&Warning::FontFallback));
    assert!(rendered.warnings.contains(&Warning::MissingGlyphs));
    assert_eq!(rendered.uncovered_scripts, ["Hebrew"]);
}

#[test]
fn supplied_font_fifo_is_refused_without_waiting_for_a_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("font.fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    let entry = fallback(&path, 0);
    let (sent, answer) = std::sync::mpsc::channel();
    let loader = std::thread::spawn(move || {
        let mut job = job("A", "monospace");
        job.fallback_fonts.push(entry);
        let _ = sent.send(render(&job).map(|_| ()));
    });
    match answer.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(result) => {
            loader.join().unwrap();
            assert_eq!(result.unwrap_err(), FailureCode::FileNotRegular);
        }
        Err(_) => {
            drop(std::fs::OpenOptions::new().write(true).open(&path));
            loader.join().unwrap();
            panic!("a font FIFO with no writer blocked");
        }
    }
}

#[test]
fn script_report_overflow_is_refused_instead_of_silently_truncated() {
    use skrifa::MetadataProvider as _;
    use unicode_script::UnicodeScript as _;
    let font = skrifa::FontRef::new(include_bytes!(
        "../../../assets/fonts/JetBrainsMonoNerdFont-Regular.ttf"
    ))
    .unwrap();
    let charmap = font.charmap();
    let mut scripts = std::collections::BTreeMap::new();
    for code in 0x800..=0x10ffff {
        let Some(c) = char::from_u32(code) else {
            continue;
        };
        if c.is_control() || c.is_whitespace() || charmap.map(c).is_some() {
            continue;
        }
        let script = c.script().full_name();
        if matches!(script, "Common" | "Inherited" | "Unknown") {
            continue;
        }
        scripts.entry(script).or_insert(c);
        if scripts.len() > kettle_media::MAX_UNCOVERED_SCRIPTS {
            break;
        }
    }
    assert_eq!(scripts.len(), kettle_media::MAX_UNCOVERED_SCRIPTS + 1);
    let text: String = scripts.into_values().collect();
    assert_eq!(
        render(&job(&text, "monospace")).unwrap_err(),
        FailureCode::RenderResource
    );
}

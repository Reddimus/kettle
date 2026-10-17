use super::{AttrsList, BufferLine, LineEnding, load_bundled_font};
use glyphon::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping};

/// Lines where a bidi paragraph separator divides left-to-right text from
/// right-to-left text, in both orders. cosmic-text splits lines only on
/// CR and LF; the others (U+2029, NEL, FS) leave several paragraphs in one
/// line.
fn mixed_lines() -> Vec<String> {
    let mut lines = Vec::new();
    for separator in ['\u{2029}', '\u{0085}', '\u{001C}'] {
        for (first, second) in [("abc", "שלום"), ("שלום", "abc"), ("abc", "مرحبا")] {
            lines.push(format!("{first}{separator}{second}"));
        }
    }
    lines
}

fn font_system() -> FontSystem {
    let mut font_system = FontSystem::new();
    for face in kettle_config::font::all() {
        load_bundled_font(&mut font_system, face);
    }
    font_system
}

/// Chrome text (tab titles, the status bar, overlays) and grid rows both
/// shape such a line, and it lays out instead of stopping the process.
#[test]
fn a_line_with_paragraphs_of_both_directions_still_shapes() {
    let mut font_system = font_system();
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    for line in mixed_lines() {
        for shaping in [Shaping::Advanced, Shaping::Basic] {
            let mut buffer = Buffer::new(&mut font_system, Metrics::new(16.0, 20.0));
            buffer.set_size(Some(2000.0), Some(200.0));
            buffer.set_text(&line, &attrs, shaping, None);
            buffer.shape_until_scroll(&mut font_system, false);
            let glyphs: usize = buffer.layout_runs().map(|run| run.glyphs.len()).sum();
            assert!(glyphs > 0, "{line:?} laid out no glyphs");

            let mut row = BufferLine::new(
                line.as_str(),
                LineEnding::Lf,
                AttrsList::new(&attrs),
                shaping,
            );
            let shaped = row.shape(&mut font_system, 8);
            assert!(!shaped.spans.is_empty(), "{line:?} shaped no spans");
        }
    }
}

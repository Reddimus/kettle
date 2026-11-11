//! Font fallback for clusters that no installed face covers.
//!
//! cosmic-text's last fallback stage shapes a run with every face in the
//! database. A screen of characters no face covers used to repeat that walk
//! for every word, every time a row was shaped, which held the event loop for
//! seconds. The vendored copy now skips a face only when its character map
//! proves it cannot fill a missing cluster, and remembers clusters no face's
//! character map holds. These tests use a database of bundled faces, or
//! character maps built here, so the counts do not depend on the host.

use super::{AttrsList, BufferLine, LineEnding, fontdb};
use glyphon::cosmic_text::fallback_coverage::{face_maps_any, required_chars};
use glyphon::{Attrs, Family, FontSystem, Shaping};

static FIRA_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/fira-sans/FiraSans-Regular.ttf");

/// The bundled regular face, then `fira` copies of Fira Sans. No family on
/// any platform's fallback lists is present, so every fallback face is
/// reached through the walk of the whole database.
fn font_system(fira: usize) -> FontSystem {
    let mut db = fontdb::Database::new();
    db.load_font_data(kettle_config::font::REGULAR.to_vec());
    for _ in 0..fira {
        db.load_font_data(FIRA_REGULAR.to_vec());
    }
    FontSystem::new_with_locale_and_db("en-US".into(), db)
}

/// Each glyph of `text` shaped as a pane row is: its face's PostScript name
/// and glyph id.
fn shape(font_system: &mut FontSystem, text: &str) -> Vec<(String, u16)> {
    let attrs = Attrs::new().family(Family::Name(kettle_config::font::FAMILY));
    let mut line = BufferLine::new(
        text,
        LineEnding::Lf,
        AttrsList::new(&attrs),
        Shaping::Advanced,
    );
    line.set_ltr_override(true);
    let glyphs: Vec<_> = line
        .shape(font_system, 8)
        .spans
        .iter()
        .flat_map(|span| &span.words)
        .flat_map(|word| &word.glyphs)
        .map(|glyph| (glyph.font_id, glyph.glyph_id))
        .collect();
    glyphs
        .into_iter()
        .map(|(id, glyph)| {
            let name = font_system
                .db()
                .face(id)
                .map(|face| face.post_script_name.clone())
                .unwrap_or_default();
            (name, glyph)
        })
        .collect()
}

/// `count` CJK Extension H ideographs from U+31350, which neither bundled
/// face covers. Line breaking puts each in a word of its own.
fn uncovered(count: u32) -> String {
    (0x31350..0x31350 + count)
        .map(|cp| char::from_u32(cp).expect("assigned scalar"))
        .collect()
}

fn assert_drawn_by(glyphs: &[(String, u16)], face: &str, text: &str) {
    assert!(!glyphs.is_empty(), "{text:?} shaped no glyphs");
    for (name, glyph) in glyphs {
        assert!(
            name == face && *glyph != 0,
            "{text:?} should be drawn by {face}, got {glyphs:?}"
        );
    }
}

#[test]
fn a_run_no_face_maps_is_not_walked_again() {
    let mut fonts = font_system(8);
    let text = uncovered(70);
    shape(&mut fonts, &text);
    let (checks, shapes) = (fonts.coverage_check_count(), fonts.fallback_shape_count());
    assert!(
        checks >= 70 * 9,
        "the first shaping reads every face's character map for each word, read {checks}"
    );
    let glyphs = shape(&mut fonts, &text);
    assert_eq!(
        (
            fonts.coverage_check_count() - checks,
            fonts.fallback_shape_count() - shapes
        ),
        (0, 0),
        "a run whose clusters no face maps must not read or shape any fallback face again"
    );
    assert!(
        glyphs.iter().all(|(_, glyph)| *glyph == 0),
        "the run still draws the primary face's .notdef: {glyphs:?}"
    );
}

#[test]
fn a_cluster_some_face_maps_is_walked_again_though_none_drew_it() {
    let mut fonts = font_system(1);
    // Fira maps U+0132 but not U+1AB0, so it is shaped and fails. Whether a
    // face fails a cluster depends on the run (a ligature can join it to a
    // neighbour), so only a cluster no face maps may be remembered.
    shape(&mut fonts, "\u{132}\u{1AB0}");
    let shapes = fonts.fallback_shape_count();
    let glyphs = shape(&mut fonts, "\u{132}\u{1AB0}");
    assert!(
        fonts.fallback_shape_count() > shapes,
        "Fira maps the cluster's base, so it must be tried again"
    );
    assert!(
        glyphs.iter().any(|(_, glyph)| *glyph == 0),
        "the fixture needs a cluster no face draws: {glyphs:?}"
    );
}

#[test]
fn a_screen_no_face_covers_loads_and_shapes_no_fallback_face() {
    let mut fonts = font_system(8);
    for row in 0..10 {
        let start = 0x31350 + row * 70;
        let text: String = (start..start + 70)
            .map(|cp| char::from_u32(cp).expect("assigned scalar"))
            .collect();
        shape(&mut fonts, &text);
    }
    // 700 distinct characters, one word each, and nine faces. Every face
    // used to be loaded and shaped with every word: 6300 shapes.
    assert_eq!(
        fonts.fallback_shape_count(),
        0,
        "no fallback face maps any of these characters, so none is shaped"
    );
    assert_eq!(
        fonts.loaded_font_count(),
        1,
        "only the primary face is loaded; the others are read for their character maps"
    );
}

#[test]
fn a_cluster_no_face_covers_does_not_hide_its_base() {
    let mut fonts = font_system(1);
    // Fira has U+0132 but no U+1AB0, so the cluster is missing everywhere.
    let cluster = shape(&mut fonts, "\u{132}\u{1AB0}");
    assert!(
        cluster.iter().any(|(_, glyph)| *glyph == 0),
        "the fixture needs a cluster no face covers: {cluster:?}"
    );
    let base = shape(&mut fonts, "\u{132}");
    assert_drawn_by(&base, "FiraSans-Regular", "\u{132}");
}

#[test]
fn adding_a_face_forgets_the_clusters_it_may_cover() {
    let mut fonts = font_system(0);
    let before = shape(&mut fonts, "\u{132}");
    assert!(
        before.iter().all(|(_, glyph)| *glyph == 0),
        "the bundled face has no U+0132: {before:?}"
    );
    // The renderer adds its bundled bold and italic faces the same way.
    fonts.db_mut().load_font_data(FIRA_REGULAR.to_vec());
    let after = shape(&mut fonts, "\u{132}");
    assert_drawn_by(&after, "FiraSans-Regular", "\u{132}");
}

#[test]
fn characters_a_face_covers_through_decomposition_still_fall_back_to_it() {
    let mut fonts = font_system(1);
    // Fira maps neither as written, but maps their canonical decompositions:
    // B with U+0331, and U+0313. The shaper decomposes them when the face
    // lacks the precomposed character.
    for text in ["\u{1E06}", "\u{343}"] {
        let glyphs = shape(&mut fonts, text);
        assert_drawn_by(&glyphs, "FiraSans-Regular", text);
    }
}

#[test]
fn lao_and_thai_clusters_never_let_a_face_be_skipped() {
    // harfrust's Thai shaper also shapes Lao, and splits SARA AM (U+0E33 and
    // U+0EB3) into NIKHAHIT and SARA AA, which no normalization yields.
    for cluster in ["\u{0EB3}", "\u{0E81}\u{0EB3}", "\u{0E33}", "\u{0E81}"] {
        assert_eq!(required_chars(cluster, false), None, "{cluster:?}");
    }
}

#[test]
fn right_to_left_runs_never_let_a_face_be_skipped() {
    // A right-to-left run draws `(` with the face's `)` when it has one.
    assert_eq!(required_chars("(", true), None);
    assert_eq!(required_chars("(", false), Some(vec!['(']));
}

#[test]
fn the_required_characters_include_both_normalization_forms() {
    let decomposes = required_chars("\u{1E06}", false).expect("no exemption");
    assert!(decomposes.contains(&'B') && decomposes.contains(&'\u{331}'));
    let composes = required_chars("B\u{331}", false).expect("no exemption");
    assert!(composes.contains(&'\u{1E06}'));
    let jamo = required_chars("\u{1100}\u{1161}", false).expect("no exemption");
    assert!(jamo.contains(&'\u{AC00}'));
    // Default-ignorables and fallback spaces are drawn without a mapping.
    for cluster in ["\u{200D}", "\u{FE0F}", "\u{2003}", "\u{2011}", "\t"] {
        assert_eq!(required_chars(cluster, false), None, "{cluster:?}");
    }
}

/// A face holding only a `cmap` table with these (platform, encoding,
/// subtable) records, in this order.
fn cmap_face(records: &[(u16, u16, Vec<u8>)]) -> Vec<u8> {
    let mut cmap = Vec::new();
    cmap.extend(0u16.to_be_bytes());
    cmap.extend((records.len() as u16).to_be_bytes());
    let mut offset = 4 + 8 * records.len();
    for (platform, encoding, subtable) in records {
        cmap.extend(platform.to_be_bytes());
        cmap.extend(encoding.to_be_bytes());
        cmap.extend((offset as u32).to_be_bytes());
        offset += subtable.len();
    }
    for (_, _, subtable) in records {
        cmap.extend(subtable);
    }
    let mut face = Vec::new();
    face.extend(0x0001_0000u32.to_be_bytes());
    face.extend(1u16.to_be_bytes()); // numTables
    face.extend(16u16.to_be_bytes()); // searchRange
    face.extend(0u16.to_be_bytes()); // entrySelector
    face.extend(0u16.to_be_bytes()); // rangeShift
    face.extend(*b"cmap");
    face.extend(0u32.to_be_bytes()); // checksum
    face.extend(28u32.to_be_bytes()); // offset
    face.extend((cmap.len() as u32).to_be_bytes());
    face.extend(cmap);
    face
}

/// A format 4 subtable mapping `c` to `glyph`.
fn format4(c: u16, glyph: u16) -> Vec<u8> {
    let words = [
        4,
        32,
        0, // format, length, language
        4,
        4,
        1,
        0, // segCountX2, searchRange, entrySelector, rangeShift
        c,
        0xFFFF, // endCode
        0,      // reservedPad
        c,
        0xFFFF, // startCode
        glyph.wrapping_sub(c),
        1, // idDelta
        0,
        0, // idRangeOffset
    ];
    words.iter().flat_map(|word| word.to_be_bytes()).collect()
}

/// A format 12 subtable mapping `c` to `glyph`.
fn format12(c: u32, glyph: u32) -> Vec<u8> {
    let mut table = Vec::new();
    table.extend(12u16.to_be_bytes());
    table.extend(0u16.to_be_bytes());
    for word in [28, 0, 1, c, c, glyph] {
        table.extend(word.to_be_bytes());
    }
    table
}

/// A format 14 subtable, which harfrust never maps base characters with.
fn format14() -> Vec<u8> {
    let mut table = Vec::new();
    table.extend(14u16.to_be_bytes());
    table.extend(10u32.to_be_bytes());
    table.extend(0u32.to_be_bytes());
    table
}

#[test]
fn the_character_map_is_read_from_the_subtable_harfrust_shapes_with() {
    // A 32-bit Unicode subtable is preferred to a BMP one, wherever it is.
    let face = cmap_face(&[(3, 1, format4(0x41, 3)), (3, 10, format12(0x42, 4))]);
    assert!(!face_maps_any(&face, 0, &['A']));
    assert!(face_maps_any(&face, 0, &['B']));
    // Within a platform and encoding, a record in a format harfrust does not
    // map with is passed over for the next one.
    let face = cmap_face(&[(3, 1, format14()), (3, 1, format4(0x41, 3))]);
    assert!(face_maps_any(&face, 0, &['A']));
    assert!(!face_maps_any(&face, 0, &['Z']));
    // A symbol subtable is preferred, and maps U+0041 through U+F041.
    let face = cmap_face(&[(3, 1, format4(0x42, 3)), (3, 0, format4(0xF041, 5))]);
    assert!(face_maps_any(&face, 0, &['A']));
    assert!(!face_maps_any(&face, 0, &['B']));
    // A mapping to `.notdef` is no mapping.
    let face = cmap_face(&[(3, 1, format4(0x41, 0))]);
    assert!(!face_maps_any(&face, 0, &['A']));
}

#[test]
fn a_character_map_that_cannot_be_read_never_lets_a_face_be_skipped() {
    // Only a format harfrust does not map with: it would find no subtable.
    assert!(face_maps_any(&cmap_face(&[(3, 1, format14())]), 0, &['A']));
    // Mac Roman, which harfrust remaps before looking up.
    let mut roman = vec![0, 0, 1, 6, 0, 0];
    roman.extend([0u8; 256]);
    assert!(face_maps_any(&cmap_face(&[(1, 0, roman)]), 0, &['A']));
    // No character map, and no font at all.
    let mut empty = cmap_face(&[]);
    empty[12..16].copy_from_slice(b"cmaq");
    assert!(face_maps_any(&empty, 0, &['A']));
    assert!(face_maps_any(b"not a font", 0, &['A']));
}

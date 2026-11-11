//! Font fallback for clusters that no installed face covers.
//!
//! cosmic-text's last fallback stage shapes a run with every face in the
//! database. A screen of characters no face covers used to repeat that walk
//! for every word, every time a row was shaped, which held the event loop for
//! seconds. The vendored copy now skips faces whose character map cannot fill
//! a missing cluster and remembers clusters no face covers. These tests use a
//! database of bundled faces only, so the counts do not depend on the host.

use super::{AttrsList, BufferLine, LineEnding, fontdb};
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
fn a_run_no_face_covers_is_not_walked_again() {
    let mut fonts = font_system(8);
    let text = uncovered(70);
    shape(&mut fonts, &text);
    let after_first = fonts.fallback_shape_count();
    let glyphs = shape(&mut fonts, &text);
    assert_eq!(
        fonts.fallback_shape_count() - after_first,
        0,
        "shaping the same uncovered run again must not try any fallback face"
    );
    assert!(
        glyphs.iter().all(|(_, glyph)| *glyph == 0),
        "the run still draws the primary face's .notdef: {glyphs:?}"
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

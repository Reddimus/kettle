use super::{
    Attrs, AttrsList, BufferLine, Config, Family, FontSystem, GColor, LineEnding, Metrics,
    ShapedRow, Shaping, Style, TextBuffer, Weight, Wrap, font_features, load_bundled_font,
    production_source, run_attrs,
};
use kettle_config::Rgb;

/// A run as `build_pane`'s cell walk builds it: text, colour, bold, italic.
type Run = (String, Rgb, bool, bool);

fn run(text: &str, fg: u8, bold: bool, italic: bool) -> Run {
    (text.to_string(), Rgb::new(fg, 0, 0), bold, italic)
}

/// Regular runs, one per part, each in its own colour.
fn runs(parts: &[&str]) -> Vec<Run> {
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| run(part, i as u8, false, false))
        .collect()
}

/// The text `build_pane` shapes for a row of `runs`.
fn shaped_text(runs: &[Run]) -> String {
    let shaped = ShapedRow::of(runs);
    let mut text: String = runs[..shaped.runs]
        .iter()
        .enumerate()
        .map(|(i, (part, ..))| shaped.text(i, part))
        .collect();
    let cfg = Config::default();
    let mut attrs = AttrsList::new(&Attrs::new());
    shaped.push_pad(&mut text, &mut attrs, &cfg, &font_features(&cfg));
    text
}

#[test]
fn a_row_is_shaped_up_to_its_last_inked_cell() {
    // keyblock's rows: blanks in three colours and nothing inked.
    assert_eq!(shaped_text(&runs(&["    ", "  ", "    "])), "");
    assert_eq!(
        ShapedRow::of(&runs(&["    ", "  "])),
        ShapedRow {
            runs: 0,
            last_len: 0,
            pad_faces: 0,
        }
    );
    assert_eq!(shaped_text(&[]), "");
    // A prompt: its padding goes and one blank stays, whether the padding
    // shares the prompt's run or has its own colour.
    assert_eq!(shaped_text(&runs(&["$ ls      "])), "$ ls ");
    assert_eq!(shaped_text(&runs(&["$ ls", "      "])), "$ ls ");
    assert_eq!(shaped_text(&runs(&["a  ", "  "])), "a ");
    // Blanks between inked cells keep every later cell on its column.
    assert_eq!(shaped_text(&runs(&["a", "   ", "b"])), "a   b");
    assert_eq!(shaped_text(&runs(&["a   b", "  "])), "a   b ");
    // A row inked to its last column is shaped whole, with no pad.
    assert_eq!(shaped_text(&runs(&["abc"])), "abc");
    assert_eq!(shaped_text(&runs(&["ab", "c"])), "abc");
    // Only U+0020 is cut: a space carrying a combining mark, a no-break
    // space and an ideographic space stay.
    assert_eq!(shaped_text(&runs(&["x \u{301}", "  "])), "x \u{301} ");
    assert_eq!(shaped_text(&runs(&["x\u{a0}"])), "x\u{a0}");
    assert_eq!(shaped_text(&runs(&["x\u{3000}", " "])), "x\u{3000} ");
}

/// The pad holds one blank per face among the cut blanks, each in that
/// face and the fixed pad colour. Blanks that stay add no pad.
#[test]
fn the_pad_has_one_blank_in_each_face_of_the_cut_blanks() {
    let regular = ShapedRow::face_bit(false, false);
    let bold = ShapedRow::face_bit(true, false);
    let italic = ShapedRow::face_bit(false, true);
    let bold_italic = ShapedRow::face_bit(true, true);
    assert_eq!([regular, bold, italic, bold_italic], [1, 2, 4, 8]);
    let pad = |row: &[Run]| ShapedRow::of(row).pad_faces;
    // Vim's status line: bold text, then bold blanks.
    assert_eq!(
        pad(&[run("NORMAL  ", 1, true, false), run("    ", 1, true, false)]),
        bold
    );
    assert_eq!(
        pad(&[run("$ ls", 1, false, false), run("    ", 2, true, false)]),
        bold
    );
    assert_eq!(
        pad(&[
            run("a ", 1, false, true),
            run("  ", 2, true, true),
            run("  ", 3, true, false),
            run("  ", 4, false, false),
        ]),
        regular | bold | italic | bold_italic
    );
    // Blanks between inked cells, and the blanks of a blank row, stay or
    // go without a pad.
    assert_eq!(
        pad(&[
            run("a", 1, false, false),
            run("  ", 2, true, true),
            run("b", 3, false, false),
        ]),
        0
    );
    assert_eq!(pad(&[run("    ", 1, true, false)]), 0);

    let cfg = Config {
        font_family_bold: Some("Bold Family".to_string()),
        font_family_italic: Some("Italic Family".to_string()),
        ..Config::default()
    };
    let row = [
        run("a", 1, false, false),
        run("  ", 2, true, false),
        run("  ", 3, false, true),
        run("  ", 4, true, false),
    ];
    let shaped = ShapedRow::of(&row);
    let mut text = shaped.text(0, &row[0].0).to_string();
    let mut attrs = AttrsList::new(&Attrs::new());
    shaped.push_pad(&mut text, &mut attrs, &cfg, &font_features(&cfg));
    assert_eq!(text, "a  ", "one blank per face, not per cut blank");
    let face = |at: usize| {
        let a = attrs.get_span(at);
        (a.family, a.weight, a.style, a.color_opt)
    };
    let pad_fg = Some(GColor::rgb(0, 0, 0));
    assert_eq!(
        face(1),
        (
            Family::Name("Bold Family"),
            Weight::BOLD,
            Style::Normal,
            pad_fg
        )
    );
    assert_eq!(
        face(2),
        (
            Family::Name("Italic Family"),
            Weight::NORMAL,
            Style::Italic,
            pad_fg
        )
    );
}

/// The row key covers exactly what is shaped. Blanks after the last inked
/// cell change colour without changing it; anything shaped changes it.
#[test]
fn a_row_key_covers_exactly_what_is_shaped() {
    let key = |row: &[Run]| ShapedRow::of(row).key(row);
    let prompt = key(&[run("$ ls", 1, false, false), run("      ", 2, false, false)]);
    assert_eq!(
        key(&[run("$ ls", 1, false, false), run("      ", 9, false, false)]),
        prompt,
        "recoloured blanks after the ink"
    );
    assert_eq!(
        key(&[run("$ ls  ", 1, false, false), run("    ", 9, false, false)]),
        prompt,
        "the same blanks split across runs another way"
    );
    // keyblock: a reverse-video block on a blank row.
    assert_eq!(
        key(&[
            run("       ", 1, false, false),
            run("    ", 7, false, false),
            run("         ", 1, false, false),
        ]),
        key(&[run(&" ".repeat(20), 1, false, false)]),
        "a blank row in any colours"
    );
    assert_ne!(
        key(&[run("$ ls", 3, false, false), run("      ", 2, false, false)]),
        prompt,
        "recoloured ink"
    );
    assert_ne!(
        key(&[run("$ lt", 1, false, false), run("      ", 2, false, false)]),
        prompt,
        "other ink"
    );
    assert_ne!(key(&[run("$ ls", 1, false, false)]), prompt, "no pad");
    assert_ne!(
        key(&[run("$ ls", 1, false, false), run("      ", 2, true, false)]),
        prompt,
        "a pad in another face"
    );
    assert_ne!(
        key(&[
            run("$ ls", 1, false, false),
            run("   ", 2, false, false),
            run("   ", 2, true, false),
        ]),
        prompt,
        "one more pad face"
    );
}

/// Shapes `row` as `build_pane` does, cut as `shaped` says, and returns
/// the line's baseline and the x of every glyph.
fn layout(fs: &mut FontSystem, cfg: &Config, row: &[Run], shaped: ShapedRow) -> (f32, Vec<f32>) {
    let ff = font_features(cfg);
    let default_attrs = Attrs::new()
        .family(Family::Name(&cfg.font_family))
        .font_features(ff.clone());
    let mut text = String::new();
    let mut attrs = AttrsList::new(&default_attrs);
    for (i, (part, fg, bold, italic)) in row[..shaped.runs].iter().enumerate() {
        let s = text.len();
        text.push_str(shaped.text(i, part));
        attrs.add_span(s..text.len(), &run_attrs(cfg, &ff, *fg, *bold, *italic));
    }
    shaped.push_pad(&mut text, &mut attrs, cfg, &ff);
    let mut buf = TextBuffer::new(fs, Metrics::new(26.0, 32.0));
    buf.set_size(Some(2000.0), Some(64.0));
    buf.set_wrap(Wrap::None);
    buf.lines = vec![BufferLine::new(
        text,
        LineEnding::Lf,
        attrs,
        Shaping::Advanced,
    )];
    buf.shape_until_scroll(fs, false);
    let run = buf.layout_runs().next().expect("one laid-out line");
    (run.line_y, run.glyphs.iter().map(|glyph| glyph.x).collect())
}

/// A cut row keeps the baseline and glyph positions it had when shaped
/// whole. The baseline comes from the largest ascent and descent among a
/// row's glyphs, so it moves when the cut takes away the only glyphs of a
/// face with other metrics: here a bold family, Courier New, beside the
/// bundled JetBrains Mono.
#[test]
fn a_cut_row_keeps_the_baseline_of_the_whole_row() {
    const BOLD: &str = "Courier New";
    let mut fs = FontSystem::new();
    for face in kettle_config::font::all() {
        load_bundled_font(&mut fs, face);
    }
    if !fs
        .db()
        .faces()
        .any(|face| face.families.iter().any(|(name, _)| name == BOLD))
    {
        eprintln!("{BOLD} is not installed on this host; skipped");
        return;
    }
    let cfg = Config {
        font_family_bold: Some(BOLD.to_string()),
        ..Config::default()
    };
    let blanks = " ".repeat(20);
    let rows = [
        // Vim's status line: bold text and bold blanks.
        vec![run("NORMAL", 1, true, false), run(&blanks, 1, true, false)],
        // Bold text, then plain blanks.
        vec![run("NORMAL", 1, true, false), run(&blanks, 2, false, false)],
        // Plain text, then bold blanks.
        vec![run("$ ls", 1, false, false), run(&blanks, 2, true, false)],
        // Plain text, then blanks in both faces.
        vec![
            run("$ ls", 1, false, false),
            run(&blanks, 2, true, false),
            run(&blanks, 3, false, false),
        ],
    ];
    for row in rows {
        let whole = ShapedRow {
            runs: row.len(),
            last_len: row[row.len() - 1].0.len(),
            pad_faces: 0,
        };
        let (whole_y, whole_x) = layout(&mut fs, &cfg, &row, whole);
        let (cut_y, cut_x) = layout(&mut fs, &cfg, &row, ShapedRow::of(&row));
        let ink = row[0].0.chars().count();
        assert_eq!(cut_y, whole_y, "{row:?}: the baseline moved");
        assert_eq!(cut_x[..ink], whole_x[..ink], "{row:?}: a glyph moved");
    }
}

/// `build_pane` keys and shapes each row through one `ShapedRow`. This
/// guard runs where the headless test has no GPU and skips.
#[test]
fn build_pane_keys_and_shapes_the_same_extent() {
    let src = production_source();
    let build_pane = src
        .split_once("    fn build_pane_cells(")
        .expect("build_pane_cells")
        .1
        .split_once("\n    fn ")
        .expect("end of build_pane")
        .0;
    let compact: String = build_pane.split_whitespace().collect();
    for needle in [
        "letshaped=ShapedRow::of(runs);",
        "letkey=shaped.key(runs);",
        "inruns[..shaped.runs].iter().enumerate(){lettext=shaped.text(i,text);",
        "shaped.push_pad(&mutrow_text,&mutattrs_list,cfg,&ff);",
    ] {
        assert!(compact.contains(needle), "build_pane lost {needle:?}");
    }
    assert!(
        !build_pane.contains("in runs {"),
        "a loop over a row's whole runs would key or shape the blanks after its ink"
    );
    assert_eq!(
        build_pane.matches("row_text.push").count(),
        1,
        "the row text has one source besides the pad"
    );
}

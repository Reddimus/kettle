use super::load_bundled_font;
use glyphon::{Attrs, Buffer as TextBuffer, Family, FontSystem, Metrics, Shaping};

/// Shape `text` once under `shaping` against a fresh `FontSystem` that
/// carries the bundled faces plus whatever `FontSystem::new()` loaded
/// from the host via `fontdb`'s system-font scan — the same stack the
/// chrome buffers shape with. Returns each resolved glyph id in source
/// order; `0` is TrueType's reserved `.notdef` — the tofu box.
fn shape_glyph_ids(text: &str, shaping: Shaping) -> Vec<u16> {
    let mut fs = FontSystem::new();
    for face in kettle_config::font::all() {
        load_bundled_font(&mut fs, face);
    }
    let metrics = Metrics::new(16.0, 20.0);
    let mut buf = TextBuffer::new(&mut fs, metrics);
    buf.set_size(Some(2000.0), Some(200.0));
    buf.set_text(
        text,
        &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
        shaping,
        None,
    );
    buf.shape_until_scroll(&mut fs, false);
    buf.layout_runs()
        .flat_map(|run| run.glyphs.iter().map(|g| g.glyph_id))
        .collect()
}

use crate::{
    SwashCache, TEXT_SYMBOL_PROBE, resolve_text_symbol_family, single_column_text_presentation,
};

/// The width table already knows which codepoints are text by default.
///
/// This is the whole selection rule, so it is worth pinning against real
/// codepoints rather than trusting the derivation. The `Emoji_Presentation`
/// column is the fact each row encodes.
#[test]
fn text_presentation_is_derived_from_the_width_table() {
    // Emoji=Yes, Emoji_Presentation=No: one column alone, two with U+FE0F.
    // `⏺` is the bullet Claude Code prints at the head of every message.
    for ch in [
        '\u{23FA}', // ⏺ record button
        '\u{23F8}', // ⏸ pause
        '\u{23F9}', // ⏹ stop
        '\u{25B6}', // ▶ play
        '\u{2611}', // ☑ ballot box with check
        '\u{26A0}', // ⚠ warning
        '\u{2049}', // ⁉ exclamation question
    ] {
        assert!(
            single_column_text_presentation(ch),
            "U+{:04X} defaults to text presentation",
            ch as u32
        );
    }
    // Emoji_Presentation=Yes: already two columns, so a colour face is
    // the right answer and this must not touch them.
    for ch in [
        '\u{23F0}',  // ⏰ alarm clock
        '\u{2B50}',  // ⭐ star
        '\u{26AB}',  // ⚫ medium black circle
        '\u{2705}',  // ✅ check mark button
        '\u{1F600}', // 😀
    ] {
        assert!(
            !single_column_text_presentation(ch),
            "U+{:04X} is emoji-presentation and must keep its colour face",
            ch as u32
        );
    }
    // Ordinary text is not emoji-capable at all. ASCII short-circuits
    // before the width table is consulted at all.
    for ch in ['a', '0', ' ', '\u{00E9}', '\u{4E00}', '\u{2500}'] {
        assert!(
            !single_column_text_presentation(ch),
            "U+{:04X} is not emoji-capable",
            ch as u32
        );
    }
    // Out of scope by design: default-text emoji that are East Asian Wide
    // stay at two columns with or without U+FE0F, so width cannot separate
    // them from `Emoji_Presentation=Yes`. A colour glyph in a two-column
    // cell fits the space it was given, which is why they are left alone.
    for ch in ['\u{3030}', '\u{1F202}'] {
        assert!(
            !single_column_text_presentation(ch),
            "U+{:04X} is wide, so it is outside this rule",
            ch as u32
        );
    }
}

/// An explicit U+FE0F is the user asking for the colour face.
///
/// Kettle appends a cell's zero-width marks straight after its base char in
/// the run it hands the shaper, so a qualified `⏺️` arrives as two chars in
/// one run. Overriding that would be the same defect in the other
/// direction: Unicode names `23FA FE0F` an emoji-style sequence.
#[test]
fn a_variation_selector_keeps_the_colour_face() {
    let src = crate::production_source();
    let peek = ["chars.peek() == Some(&'\\u{FE0F}')"].concat();
    assert!(
        src.contains(&peek),
        "the row builder must look at the mark that follows the base char"
    );
    let gate = ["if !qualified && single_column_text_", "presentation(ch)"].concat();
    assert!(
        src.contains(&gate),
        "a qualified sequence must skip the override, not merely be noticed"
    );
}

/// The selection rule has to reach the shaper, not just exist.
///
/// A behavioural test cannot get here: it needs a GPU surface, a shaped
/// pane buffer and a rasterizer, and the pieces this pins are already
/// covered above on their own. What is left is the wiring, so pin it at the
/// source. Every needle is built at runtime and matched against
/// `production_source()`, so this cannot match itself or any test.
#[test]
fn the_row_builder_asks_for_a_text_face_on_text_presentation_cells() {
    let src = crate::production_source();
    let predicate = ["single_column_text_", "presentation(ch)"].concat();
    let request = ["Family::Name(symbol_", "family)"].concat();
    let resolve = ["resolve_text_symbol_", "family(&mut font_system)"].concat();

    assert!(
        src.contains(&resolve),
        "the face must be resolved once at renderer init; resolving it per \
             row would walk fontdb on every frame"
    );
    assert!(
        src.contains(&predicate),
        "the row builder must consult the presentation rule, or the span \
             below is never added and the bullet keeps its colour face"
    );
    assert!(
        src.contains(&request),
        "the span must request the resolved text face by name"
    );
    // The narrower span has to come after the run's own or the run
    // overwrites it and the whole thing is a no-op.
    let run_span = ["attrs_list.add_span(s..row_", "text.len(), &a);"].concat();
    let cell_span = ["attrs_list.add_span(at..at + len, &text_", "attrs);"].concat();
    let (Some(run_at), Some(cell_at)) = (src.find(&run_span), src.find(&cell_span)) else {
        panic!("both spans must exist in the row builder");
    };
    assert!(
        run_at < cell_at,
        "the per-cell span must be added after the run's own, or the run \
             attrs win and every cell keeps the default family"
    );
}

/// The resolver has to answer against the font system the renderer builds.
///
/// `Font::unicode_codepoints` is empty unless cosmic-text's
/// `monospace_fallback` feature is on, and it is not. A resolver that probes
/// it rejects every candidate, and the other tests report that no-op as a
/// skip rather than a failure.
#[test]
fn the_resolver_answers_against_the_renderer_font_system() {
    let mut fs = FontSystem::new();
    crate::load_bundled_font(&mut fs, kettle_config::font::REGULAR);
    let faces = fs.db().faces().count();
    assert!(
        faces > 1,
        "the system font db is empty, so nothing below proves anything"
    );
    let resolved = resolve_text_symbol_family(&mut fs);
    eprintln!("text-presentation face: {resolved:?} out of {faces} faces");
    // A host may legitimately have no candidate. What it may not do is
    // answer with a family that does not carry the probe, which is the
    // failure mode that silently disabled the fix.
    if let Some(family) = resolved {
        let ids: Vec<_> = fs
            .db()
            .faces()
            .filter(|face| {
                face.families
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case(family))
            })
            .map(|face| face.id)
            .collect();
        assert!(
            ids.iter().any(
                |id| fs.get_font(*id, Default::default()).is_some_and(|font| font
                    .as_swash()
                    .charmap()
                    .map(TEXT_SYMBOL_PROBE)
                    != 0)
            ),
            "{family:?} was chosen without a glyph for the probe"
        );
    }
}

/// The face requested for those cells must actually be monochrome.
///
/// Picking a family that the system lacks, or one that turns out to be the
/// colour-emoji face under another name, would leave the bullet exactly as
/// it was. Rasterizing is the only way to tell: font selection is decided
/// by cmap coverage, which is a property of the host, not of this crate.
#[test]
fn the_text_presentation_face_rasterizes_a_monochrome_bullet() {
    let mut fs = FontSystem::new();
    for face in kettle_config::font::all() {
        load_bundled_font(&mut fs, face);
    }
    let Some(symbol_family) = resolve_text_symbol_family(&mut fs) else {
        // A font-poor image has nothing to steer towards, and the code
        // leaves such a system exactly as it was.
        eprintln!("no monochrome symbol face installed ... skipped");
        return;
    };
    let metrics = Metrics::new(16.0, 20.0);
    let mut buf = TextBuffer::new(&mut fs, metrics);
    buf.set_size(Some(200.0), Some(40.0));
    buf.set_text(
        "\u{23FA}",
        &Attrs::new().family(Family::Name(symbol_family)),
        Shaping::Advanced,
        None,
    );
    buf.shape_until_scroll(&mut fs, false);
    let key = buf
        .layout_runs()
        .flat_map(|run| run.glyphs.iter())
        .map(|g| g.physical((0.0, 0.0), 1.0).cache_key)
        .next()
        .expect("the bullet shaped to a glyph");
    let mut swash = SwashCache::new();
    let image = swash
        .get_image(&mut fs, key)
        .as_ref()
        .expect("the bullet rasterized");
    assert!(
        !matches!(image.content, glyphon::cosmic_text::SwashContent::Color),
        "U+23FA resolved to a colour face through {symbol_family:?}, so \
             the bullet would still render as a colour square"
    );
}

/// A pane-titlebar-shaped label: leading status glyph (Claude Code's
/// OSC 0/2 titles lead with one), title text, size text, and the
/// U+1F514 bell kettle appends itself when `icon-bell` fires. Neither
/// U+2733 nor U+1F514 exists in the bundled Nerd Font.
const LABEL: &str = "  \u{2733} kettle  120x40  \u{1F514}";

/// Cross-platform invariant: Advanced's fallback cascade is a strict
/// superset of Basic's single generic-family retry, so for the same
/// font db Advanced may only GAIN glyph coverage over Basic, never
/// lose it. Holds on any host regardless of which fonts are
/// installed, so it cannot flake on a font-poor CI image.
#[test]
fn advanced_shaping_never_loses_a_glyph_basic_resolves() {
    let basic = shape_glyph_ids(LABEL, Shaping::Basic);
    let advanced = shape_glyph_ids(LABEL, Shaping::Advanced);
    // Cluster counts can differ between shapers; compare per-char by
    // walking both in order only when lengths match, else just assert
    // Advanced produced no MORE notdefs than Basic.
    if basic.len() == advanced.len() {
        for (i, (b, a)) in basic.iter().zip(&advanced).enumerate() {
            assert!(
                *b == 0 || *a != 0,
                "glyph {i} resolved under Basic (id {b}) but not Advanced"
            );
        }
    }
    let basic_notdefs = basic.iter().filter(|&&id| id == 0).count();
    let advanced_notdefs = advanced.iter().filter(|&&id| id == 0).count();
    assert!(
        advanced_notdefs <= basic_notdefs,
        "Advanced produced more tofu than Basic: {advanced_notdefs} > {basic_notdefs}"
    );
}

/// Windows regression for the split-pane titlebar tofu bug. Segoe UI
/// Emoji and Segoe UI Symbol are stock Windows fonts and cosmic-text's
/// Windows fallback list names both, so Advanced must resolve every glyph
/// in the label. The second assert pins upstream cosmic-text's no-fallback
/// Basic behavior. If it starts failing, that contract changed; Kettle does
/// not use Basic, so drop that assert.
#[cfg(windows)]
#[test]
fn advanced_shaping_resolves_titlebar_emoji_on_windows() {
    let advanced = shape_glyph_ids(LABEL, Shaping::Advanced);
    assert!(
        advanced.iter().all(|&id| id != 0),
        "Advanced shaping still produced a .notdef on Windows: {advanced:?}"
    );
    let basic = shape_glyph_ids(LABEL, Shaping::Basic);
    assert!(
        basic.contains(&0),
        "expected Basic shaping to tofu the bell/asterisk (no-fallback contract)"
    );
}

/// Source guard (same shape as pane_buffer_lifecycle_tests): Basic
/// shaping skips the fallback cascade, which is how the titlebar
/// tofu-boxed emoji while the tab bar rendered them. It must never
/// reappear in production code. `production_source` excludes this
/// module's two comparison calls, so production must contain zero uses.
/// The needle is assembled at runtime so this test's own source cannot
/// match it.
#[test]
fn no_call_site_uses_basic_shaping() {
    let src = super::production_source();
    let needle = format!("Shaping::{}", "Basic");
    let count = src.matches(&needle).count();
    assert_eq!(
        count, 0,
        "expected no production uses of {needle} but found {count} — it \
             skips cosmic-text's font-fallback cascade (no CJK/emoji/symbol \
             fallback): the split-titlebar tofu-box bug"
    );
}

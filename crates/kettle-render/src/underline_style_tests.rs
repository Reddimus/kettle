use super::{Flags, push_underline_quads};
use kettle_config::Rgb;

const COLOR: Rgb = Rgb::new(200, 60, 60);
const X: f32 = 100.0;
const Y: f32 = 0.0;
const CW: f32 = 12.0;
const CH: f32 = 20.0;

fn quads_for(flags: Flags, col: usize) -> Vec<super::QuadInstance> {
    let mut quads = Vec::new();
    push_underline_quads(&mut quads, X, Y, CW, CH, col, flags, COLOR);
    quads
}

#[test]
fn plain_underline_is_a_single_full_width_line() {
    let quads = quads_for(Flags::UNDERLINE, 0);
    assert_eq!(quads.len(), 1, "plain underline is exactly one quad");
    assert_eq!(quads[0].pos, [X, Y + CH - 2.0]);
    assert_eq!(quads[0].size, [CW, 1.0]);
}

#[test]
fn double_underline_adds_a_second_stacked_line() {
    let quads = quads_for(Flags::DOUBLE_UNDERLINE, 0);
    assert_eq!(
        quads.len(),
        2,
        "double underline draws the primary line plus one extra"
    );
    assert_eq!(quads[1].pos, [X, Y + CH - 4.0]);
}

#[test]
fn undercurl_is_a_multi_segment_zigzag_not_a_straight_line() {
    let quads = quads_for(Flags::UNDERCURL, 0);
    assert!(
        quads.len() > 1,
        "undercurl must be built from more than one segment"
    );
    // A zigzag has at least two distinct y positions; a straight line has
    // only one.
    let ys: std::collections::BTreeSet<i32> =
        quads.iter().map(|q| (q.pos[1] * 1000.0) as i32).collect();
    assert!(
        ys.len() > 1,
        "undercurl segments must vary in y (a zigzag), not sit on one line"
    );
    // Segments must tile the full cell width with no gap (unlike dotted/
    // dashed, a curl is a continuous — if stepped — line).
    let total_w: f32 = quads.iter().map(|q| q.size[0]).sum();
    assert!(
        (total_w - CW).abs() < 0.01,
        "undercurl segments must cover the full cell width: got {total_w}, want {CW}"
    );
}

#[test]
fn undercurl_phase_spans_a_two_cell_period_instead_of_resetting_every_cell() {
    // The wave's period is 2 cells. An even column rises (0, 1, 2, 1) and
    // the next odd column falls (0, -1, -2, -1), continuing the SAME wave.
    // Redrawing one tent shape per cell would show a seam at every edge.
    let ys_at = |col: usize| -> Vec<f32> {
        quads_for(Flags::UNDERCURL, col)
            .iter()
            .map(|q| q.pos[1])
            .collect()
    };
    let col0 = ys_at(0);
    let col1 = ys_at(1);
    let col2 = ys_at(2);
    assert_ne!(
        col0, col1,
        "adjacent columns must continue the wave, not repeat the same shape"
    );
    assert_eq!(
        col0, col2,
        "the wave period is 2 cells, so column N and N+2 must match"
    );
    // Both halves of the period still start exactly on the baseline (a
    // proper wave crosses zero at its period boundaries) — verifies the
    // OFFSETS table is a real symmetric wave, not an arbitrary jump.
    assert_eq!(col0[0], base_y_for_test());
    assert_eq!(col1[0], base_y_for_test());
}

fn base_y_for_test() -> f32 {
    Y + CH - 2.0
}

#[test]
fn dotted_and_dashed_both_leave_gaps_and_differ_from_each_other() {
    let dotted = quads_for(Flags::DOTTED_UNDERLINE, 0);
    let dashed = quads_for(Flags::DASHED_UNDERLINE, 0);
    assert!(dotted.len() > 1, "dotted must be more than one mark");
    assert!(dashed.len() > 1, "dashed must be more than one mark");
    let dotted_w: f32 = dotted.iter().map(|q| q.size[0]).sum();
    let dashed_w: f32 = dashed.iter().map(|q| q.size[0]).sum();
    assert!(
        dotted_w < CW,
        "dotted marks must leave visible gaps (not cover the full width)"
    );
    assert!(
        dashed_w < CW,
        "dashed marks must leave visible gaps (not cover the full width)"
    );
    // Dashed marks are individually longer than dotted marks — the two
    // styles must be visually distinguishable from each other, not just
    // from a plain/undercurl line.
    let dotted_mark_w = dotted[0].size[0];
    let dashed_mark_w = dashed[0].size[0];
    assert!(
        dashed_mark_w > dotted_mark_w,
        "dashed marks ({dashed_mark_w}) must be longer than dotted marks ({dotted_mark_w})"
    );
}

/// `QuadInstance` only derives `Pod`/`Zeroable` (needed for the GPU
/// upload), not `PartialEq`/`Debug` — extract the two fields this test
/// cares about into a comparable/printable shape instead.
fn shape_of(quads: &[super::QuadInstance]) -> Vec<([f32; 2], [f32; 2])> {
    quads.iter().map(|q| (q.pos, q.size)).collect()
}

#[test]
fn dotted_dashed_and_undercurl_are_all_pairwise_distinct_shapes() {
    // None of the "distinctive" styles may produce the plain underline's
    // single full-width quad.
    let plain = shape_of(&quads_for(Flags::UNDERLINE, 0));
    for (name, flags) in [
        ("undercurl", Flags::UNDERCURL),
        ("dotted", Flags::DOTTED_UNDERLINE),
        ("dashed", Flags::DASHED_UNDERLINE),
    ] {
        let styled = shape_of(&quads_for(flags, 0));
        assert_ne!(
            styled, plain,
            "{name} must render differently from a plain underline"
        );
    }
}

#[test]
fn concealed_cells_emit_no_foreground_decorations() {
    for underline in [
        Flags::UNDERLINE,
        Flags::DOUBLE_UNDERLINE,
        Flags::UNDERCURL,
        Flags::DOTTED_UNDERLINE,
        Flags::DASHED_UNDERLINE,
    ] {
        for line_color in [COLOR, Rgb::new(10, 240, 30)] {
            let mut visible = Vec::new();
            super::push_cell_decorations(
                &mut visible,
                (X, Y, CW, CH),
                0,
                underline | Flags::STRIKEOUT,
                COLOR,
                line_color,
            );
            assert!(!visible.is_empty());
            let mut hidden = Vec::new();
            super::push_cell_decorations(
                &mut hidden,
                (X, Y, CW, CH),
                0,
                underline | Flags::STRIKEOUT | Flags::HIDDEN,
                COLOR,
                line_color,
            );
            assert!(
                hidden.is_empty(),
                "concealed cell emitted {} foreground quads",
                hidden.len()
            );
        }
    }
}

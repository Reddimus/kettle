use super::selection_row_span;

/// The selection highlight quad's width = `(c1 + 1 - c0) * cell_w`, the same
/// arithmetic the `build_pane` draw uses. Pin it here so a span change is
/// caught at the source of truth.
fn span_width(span: (usize, usize)) -> usize {
    span.1 + 1 - span.0
}

/// A BLOCK (Alt+drag) selection is a column rectangle. Every row, including
/// interior rows, spans only `min_col..=max_col`, so the highlight matches
/// the rectangular text the copy yields. A linear selection drawn for the
/// same endpoints would span the FULL row on an interior row.
#[test]
fn block_selection_highlights_column_rectangle_on_every_row() {
    let cols = 200;
    // Block from (row 4, col 10) to (row 8, col 25): a 16-wide column band.
    let (start, end) = ((4, 10), (8, 25));
    let block_w = 25 + 1 - 10; // 16 columns
    for r in 4..=8 {
        let span = selection_row_span(r, start, end, cols, true);
        assert_eq!(
            span,
            (10, 25),
            "block row {r} must span the column rectangle, not the full line"
        );
        assert_eq!(span_width(span), block_w);
    }
    // The interior row (r = 6) is the load-bearing case: a linear selection
    // would span the whole row here.
    let interior = selection_row_span(6, start, end, cols, true);
    assert_eq!(span_width(interior), block_w);
    assert_ne!(
        span_width(interior),
        cols,
        "interior block row must NOT be a full-row highlight"
    );
}

/// A block selection's column endpoints are normalized, so dragging
/// up-and-left (end col < start col) still yields the same `min..=max` band.
#[test]
fn block_selection_normalizes_reversed_columns() {
    let cols = 80;
    // end column (5) is left of the start column (20).
    let span = selection_row_span(3, (2, 20), (6, 5), cols, true);
    assert_eq!(span, (5, 20));
}

/// Linear (normal drag) selection wraps full lines. The start row runs from
/// the anchor to the last column, interior rows span the full width, and the
/// end row runs from column 0 to the cursor.
#[test]
fn linear_selection_wraps_full_lines() {
    let cols = 120;
    let (start, end) = ((4, 10), (8, 25));
    assert_eq!(
        selection_row_span(4, start, end, cols, false),
        (10, cols - 1),
        "start row runs from the anchor to the last column"
    );
    assert_eq!(
        selection_row_span(6, start, end, cols, false),
        (0, cols - 1),
        "interior row spans the full width"
    );
    assert_eq!(
        selection_row_span(8, start, end, cols, false),
        (0, 25),
        "end row runs from column 0 to the cursor"
    );
    // A single-row linear selection stays within its endpoints.
    assert_eq!(selection_row_span(4, (4, 3), (4, 7), cols, false), (3, 7));
}

/// Grid-absolute lines are negative when scrolled into history; the span
/// logic must still compare rows correctly with `i32` endpoints.
#[test]
fn negative_scrollback_rows_compare_correctly() {
    let cols = 100;
    // Linear selection entirely in scrollback (rows -5..=-3).
    let (start, end) = ((-5, 8), (-3, 12));
    assert_eq!(
        selection_row_span(-5, start, end, cols, false),
        (8, cols - 1)
    );
    assert_eq!(
        selection_row_span(-4, start, end, cols, false),
        (0, cols - 1)
    );
    assert_eq!(selection_row_span(-3, start, end, cols, false), (0, 12));
    // Block selection over the same scrollback rows is a column band.
    assert_eq!(selection_row_span(-4, start, end, cols, true), (8, 12));
}

/// Drawing a selection must cost what is drawn, not what is selected.
///
/// `Ctrl+A` in a pane holding a million lines of build output is one
/// gesture. Walking `start..=end` and skipping the offscreen rows inside
/// the loop body would cost a million iterations on every repaint, blink,
/// and keystroke, to draw at most `screen_lines` quads.
#[test]
fn selection_drawing_visits_only_the_rows_on_screen() {
    use super::visible_selection_rows;
    let screen_rows = 40;

    // The whole scrollback selected, viewport at the bottom: only the
    // visible rows are visited, not the million behind them.
    let rows = visible_selection_rows(-1_000_000, 39, 0, screen_rows);
    assert_eq!(rows.clone().count(), screen_rows as usize);
    assert_eq!((*rows.start(), *rows.end()), (0, 39));

    // Scrolled up by 100: the window moves with the viewport, and the same
    // bounded number of rows is visited.
    let rows = visible_selection_rows(-1_000_000, 39, 100, screen_rows);
    assert_eq!((*rows.start(), *rows.end()), (-100, -61));
    assert_eq!(rows.count(), screen_rows as usize);

    // A selection that fits on screen is unchanged — clamping must not
    // narrow what was already visible.
    assert_eq!(
        {
            let r = visible_selection_rows(5, 9, 0, screen_rows);
            (*r.start(), *r.end())
        },
        (5, 9)
    );

    // Partial overlap keeps exactly the overlapping part.
    assert_eq!(
        {
            let r = visible_selection_rows(-10, 3, 0, screen_rows);
            (*r.start(), *r.end())
        },
        (0, 3),
        "the part scrolled out of view is clipped, the rest still draws"
    );
    assert_eq!(
        {
            let r = visible_selection_rows(35, 80, 0, screen_rows);
            (*r.start(), *r.end())
        },
        (35, 39)
    );

    // Entirely off screen: nothing to draw and nothing to walk.
    assert_eq!(
        visible_selection_rows(-500, -100, 0, screen_rows).count(),
        0
    );
    assert_eq!(visible_selection_rows(100, 500, 0, screen_rows).count(), 0);
    // Including the row just past each edge.
    assert_eq!(visible_selection_rows(-5, -1, 0, screen_rows).count(), 0);
    assert_eq!(visible_selection_rows(40, 45, 0, screen_rows).count(), 0);

    // Every row this yields maps into the viewport, for a spread of
    // offsets — the loop body's `debug_assert` depends on it.
    for display_offset in [0, 1, 7, 100, 1_000_000] {
        for (start, end) in [(-2_000_000, 39), (-50, 50), (-100, -100), (39, 39)] {
            for r in visible_selection_rows(start, end, display_offset, screen_rows) {
                let vrow = r + display_offset;
                assert!(
                    (0..screen_rows).contains(&vrow),
                    "offset {display_offset}, selection {start}..={end}: row {r} \
                         maps to viewport row {vrow}"
                );
                assert!(
                    (start..=end).contains(&r),
                    "clamping must not invent rows outside the selection"
                );
            }
        }
    }
}

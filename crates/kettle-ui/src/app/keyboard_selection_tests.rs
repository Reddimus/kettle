use super::{production_source, selection_buffer_bounds};

/// The whole-buffer bounds are grid-absolute: top is the oldest history line
/// (negative), column 0; bottom is the last active row, last column.
#[test]
fn buffer_bounds_span_scrollback_to_last_cell() {
    let (top, bottom) = selection_buffer_bounds(100, 24, 80);
    assert_eq!(top.line.0, -100);
    assert_eq!(top.column.0, 0);
    assert_eq!(bottom.line.0, 23);
    assert_eq!(bottom.column.0, 79);
}

/// Degenerate dims must not underflow (empty scrollback / 1-col / 1-row).
#[test]
fn buffer_bounds_are_saturating() {
    let (top, bottom) = selection_buffer_bounds(0, 1, 1);
    assert_eq!((top.line.0, top.column.0), (0, 0));
    assert_eq!((bottom.line.0, bottom.column.0), (0, 0));
    // No scrollback, single screen line: top == bottom row.
    let (top, bottom) = selection_buffer_bounds(0, 24, 80);
    assert_eq!(top.line.0, 0);
    assert_eq!(bottom.line.0, 23);
}

/// Source-drift guard: the new selection actions must actually mutate
/// `t.selection` (not silently no-op), and the Shift+click extend path must
/// stay wired into the mouse-press handler. A behavioral test would need a
/// live Term + window; pin the wiring at the source level.
#[test]
fn selection_actions_and_shift_click_are_wired() {
    let src = production_source();
    // SelectAll arm builds a full-buffer selection.
    let all = src
        .find("Action::SelectAll =>")
        .expect("SelectAll arm present");
    assert!(
        src[all..all + 1100].contains("t.selection = Some(sel);"),
        "SelectAll must set the term selection"
    );
    // SelectToTop/Bottom arm extends/sets the selection and scrolls.
    let edge = src
        .find("Action::SelectToTop | Action::SelectToBottom =>")
        .expect("SelectToTop/Bottom arm present");
    let edge_body = &src[edge..edge + 2100];
    assert!(
        edge_body.contains("sel.update(edge, side)") && edge_body.contains("t.scroll_display("),
        "SelectToTop/Bottom must extend the selection and reveal it by scrolling"
    );
    // Shift+click extension (existing behavior) must remain wired.
    assert!(
        src.contains("self.extend_selection_to_cursor(ws, area, 0)"),
        "Shift+click must still extend the selection via extend_selection_to_cursor"
    );
    assert!(
        src.contains("self.extend_selection_to_cursor(ws, area, bcode)"),
        "Shift+right-click must record the right button as gesture owner"
    );
}

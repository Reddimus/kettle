use super::cap_axis_cells;

#[test]
fn cap_axis_cells_respects_8192_texture_limit() {
    // Small cells × small request: no-op (request passes through).
    assert_eq!(cap_axis_cells(80, 8.0, 16.0), 80);
    // 72pt-ish cell (~90px tall): 200 rows × 90 = 18000 > 8192.
    // Cap: (8192 - chrome) / 90 ≈ 90 rows.
    let c = cap_axis_cells(200, 90.0, 0.0);
    assert!(c <= 91, "200×90px should cap near 91 rows, got {c}");
    assert!(c >= 80, "but shouldn't collapse below ~80, got {c}");
    // Chrome (window padding + tab bar) shrinks the body budget.
    let c2 = cap_axis_cells(200, 90.0, 200.0);
    assert!(c2 < c, "more chrome means fewer body cells: {c2} < {c}");
    // Floor at 1: even with absurd inputs that would yield 0 or
    // negative, the result is at least 1 (so a degenerate
    // screenshot is a tiny image, not a panic).
    assert_eq!(cap_axis_cells(50, 1e6, 0.0), 1);
    assert_eq!(cap_axis_cells(50, 50.0, 1e6), 1);
    // Zero / NaN-cell-px clamped via the .max(1.0) inside; doesn't
    // divide by zero.
    assert_eq!(cap_axis_cells(1, 0.0, 0.0), 1);
}

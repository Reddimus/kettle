#[test]
fn inline_placement_classes_are_interleaved_before_frame_budget() {
    assert_eq!(
        super::round_robin_bounded([vec![1, 2, 3], vec![10, 11, 12], vec![20, 21, 22]], 5),
        vec![1, 10, 20, 2, 11]
    );
    assert_eq!(
        super::round_robin_bounded([vec![1, 2], vec![], vec![20]], 99),
        vec![1, 20, 2]
    );
    assert!(super::round_robin_bounded([vec![1], vec![2], vec![3]], 0).is_empty());
}

/// Decreasing the font steps down to the renderer's own floor (5, see
/// `clamp_font_size`); a floor of 6 here once turned "decrease" at a
/// configured 5 into an increase.
#[test]
fn font_decrease_uses_the_renderer_floor() {
    let src = production_source();
    let arm = src
        .split("Action::DecreaseFontSize | Action::ZoomOutAll =>")
        .nth(1)
        .and_then(|s| s.split("_ => self.cfg.font_size").next())
        .expect("font step arm present");
    assert!(arm.contains("r.font_size() - 1.0"), "steps by one point");
    assert!(!arm.contains(".max("), "no floor above the renderer's own");

    let wheel = src
        .split("fn dispatch_wheel(")
        .nth(1)
        .and_then(|s| s.split("let new = if sign > 0 {").nth(1))
        .and_then(|s| s.split("r.set_font_size(new);").next())
        .expect("wheel font step present");
    assert!(wheel.contains("r.font_size() - 1.0"));
    assert!(
        !wheel.contains(".max("),
        "wheel must use the renderer floor"
    );
}


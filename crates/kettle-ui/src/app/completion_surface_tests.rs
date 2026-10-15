use super::{completion_anchor_col, completion_surface_available};

#[test]
fn completion_stays_on_the_live_primary_screen() {
    assert!(completion_surface_available(
        kettle_core::TermMode::empty(),
        0
    ));
    assert!(!completion_surface_available(
        kettle_core::TermMode::ALT_SCREEN,
        0
    ));
    assert!(!completion_surface_available(
        kettle_core::TermMode::empty(),
        1
    ));
}

#[test]
fn completion_anchor_uses_display_columns_not_unicode_scalar_count() {
    assert_eq!(completion_anchor_col(12, "git ch"), 6);
    assert_eq!(completion_anchor_col(12, "界 ch"), 7);
    assert_eq!(
        completion_anchor_col(3, "an unexpectedly wide prefix"),
        0,
        "malformed or stale geometry must clamp rather than underflow"
    );
}

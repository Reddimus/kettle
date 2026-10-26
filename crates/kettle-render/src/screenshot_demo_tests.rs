use super::{SCREENSHOT_DEMO_VERSION, screenshot_meter};

/// Offscreen fixtures display the current version, independent of README captures.
#[test]
fn screenshot_demo_version_tracks_crate_version() {
    assert_eq!(
        SCREENSHOT_DEMO_VERSION,
        env!("CARGO_PKG_VERSION"),
        "the --screenshot demo version must track the crate version, not a literal"
    );
    assert_ne!(
        SCREENSHOT_DEMO_VERSION, "0.1.0",
        "the offscreen demo must not advertise the legacy v0.1.0"
    );
    assert!(
        !SCREENSHOT_DEMO_VERSION.starts_with("0."),
        "the demo screenshot should advertise the real (>=1.0) product version, got {SCREENSHOT_DEMO_VERSION}"
    );
}

#[test]
fn demo_meters_share_a_capacity_and_match_their_labels() {
    assert_eq!(screenshot_meter(37, 100), ("|".repeat(12), "-".repeat(20)));
    assert_eq!(screenshot_meter(51, 320), ("|".repeat(5), "-".repeat(27)));
}

#[test]
fn demo_meters_round_and_bound_unavailable_or_extreme_values() {
    for (value, total, used) in [
        (0, 100, 0),
        (100, 100, 32),
        (1, 64, 1),
        (u32::MAX, 1, 32),
        (u32::MAX, u32::MAX, 32),
        (u32::MAX, 0, 0),
    ] {
        assert_eq!(
            screenshot_meter(value, total),
            ("|".repeat(used), "-".repeat(32 - used))
        );
    }
}

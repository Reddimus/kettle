use super::SCREENSHOT_DEMO_VERSION;

/// Drift guard. The README hero / UX showcase screenshots are
/// generated from the hardcoded `DebugScene::Default` scene, whose demo
/// `cargo test` compile line shows a `kettle v<version>` label. The version
/// comes from the crate (= workspace) version via `env!`, so a release bump
/// regenerates a correct screenshot. Guard that wiring so a future edit
/// can't reintroduce a hardcoded, stale label such as `kettle v0.1.0`.
#[test]
fn screenshot_demo_version_tracks_crate_version() {
    assert_eq!(
        SCREENSHOT_DEMO_VERSION,
        env!("CARGO_PKG_VERSION"),
        "the --screenshot demo version must track the crate version, not a literal"
    );
    assert_ne!(
        SCREENSHOT_DEMO_VERSION, "0.1.0",
        "the hero/showcase screenshot must not advertise the legacy v0.1.0"
    );
    assert!(
        !SCREENSHOT_DEMO_VERSION.starts_with("0."),
        "the demo screenshot should advertise the real (>=1.0) product version, got {SCREENSHOT_DEMO_VERSION}"
    );
}

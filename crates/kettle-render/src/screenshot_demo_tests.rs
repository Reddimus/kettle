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

#[test]
fn narrow_screenshot_rows_remain_unwrapped_grid_rows() {
    use super::*;
    let mut fonts = FontSystem::new();
    load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
    let mut buffer = screenshot_text_buffer(&mut fonts, Metrics::new(14.0, 18.0), (80.0, 400.0));
    buffer.set_text(
        "demo@kettle:~/projects/demo$ cargo test\nsecond row",
        &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
        Shaping::Advanced,
        None,
    );
    buffer.shape_until_scroll(&mut fonts, false);
    let lines: Vec<_> = buffer.layout_runs().map(|run| run.line_i).collect();
    assert_eq!(
        lines,
        [0, 1],
        "narrow screenshot soft-wrapped terminal rows"
    );
}

#[test]
fn screenshot_pane_ink_stays_above_the_caption() {
    let caption = (0.0, 176.0, 400.0, 24.0);
    for (x, width) in [(0.0, 200.0), (200.0, 200.0)] {
        let clip = super::capture::screenshot_body_clip(x, width, 32.0, 200.0, Some(caption.1));
        assert!(
            !super::rects_overlap((clip[0], clip[1], clip[2], clip[3]), caption),
            "pane ink overlaps the caption strip"
        );
        let plain = super::capture::screenshot_body_clip(x, width, 32.0, 200.0, None);
        assert_eq!(plain, [x, 32.0, width, 168.0]);
    }
}

#[test]
fn failed_offscreen_screenshot_preserves_destination() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("existing.png");
    std::fs::write(&out, b"previous PNG").unwrap();
    let result = super::capture::persist_offscreen_screenshot_with(&out, |writer| {
        writer.write_all(b"partial PNG")?;
        writer.flush()?;
        Err(anyhow::anyhow!("injected encoder failure"))
    });
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(&out).unwrap(),
        b"previous PNG",
        "failed encoding replaced the previous screenshot"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn offscreen_screenshot_atomically_replaces_destination() {
    use std::io::Write as _;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("existing.png");
    std::fs::write(&out, b"previous PNG").unwrap();
    super::capture::persist_offscreen_screenshot_with(&out, |writer| {
        writer.write_all(b"complete PNG")?;
        writer.flush()?;
        assert_eq!(std::fs::read(&out).unwrap(), b"previous PNG");
        Ok(())
    })
    .unwrap();
    assert_eq!(std::fs::read(&out).unwrap(), b"complete PNG");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn offscreen_screenshot_replaces_an_open_file_in_the_system_temp_directory() {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

    // The visual tests and --screenshot accept caller-selected destinations,
    // including open files beneath the system temp directory on Windows.
    let mut out = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
    out.write_all(b"previous PNG").unwrap();
    super::capture::persist_offscreen_screenshot_with(out.path(), |writer| {
        writer.write_all(b"complete PNG")?;
        assert_eq!(std::fs::read(out.path()).unwrap(), b"previous PNG");
        Ok(())
    })
    .unwrap();

    assert_eq!(std::fs::read(out.path()).unwrap(), b"complete PNG");
    out.seek(SeekFrom::Start(0)).unwrap();
    let mut previous = Vec::new();
    out.read_to_end(&mut previous).unwrap();
    assert_eq!(previous, b"previous PNG");
}

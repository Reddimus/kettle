#[test]
fn an_explicit_screenshot_overrides_only_transient_surface_guards() {
    let allowed = super::pending_screenshot_may_render_offscreen;

    assert!(allowed(true, Some(false), true, Some(true)));
    assert!(!allowed(false, Some(false), true, Some(true)), "no request");
    assert!(!allowed(true, Some(true), true, Some(true)), "minimized");
    assert!(
        !allowed(true, Some(false), false, Some(true)),
        "not yet shown"
    );
    assert!(
        !allowed(true, Some(false), true, Some(false)),
        "explicitly hidden"
    );
    assert!(
        allowed(true, None, true, None),
        "Wayland cannot report these states, so a shown window remains eligible for bounded capture"
    );

    let available = super::screenshot_window_accepts_control_capture;
    assert!(available(Some(false), true, Some(true)));
    assert!(!available(Some(true), true, Some(true)));
    assert!(!available(Some(false), false, Some(true)));
    assert!(!available(Some(false), true, Some(false)));
    assert!(available(None, true, None));
    assert_eq!(
        super::screenshot_window_availability(None, true, None),
        super::ScreenshotWindowAvailability::Unknown
    );
}

#[test]
fn control_screenshot_rejects_hidden_targets_and_cancels_timeouts() {
    let src = production_source();
    let body = src
        .split("fn ctl_screenshot(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// `send_text`").next())
        .expect("ctl_screenshot body");
    let availability = body
        .find("target_accepts_capture")
        .expect("target renderability must be checked");
    let queue = body
        .find("renderer.set_pending_screenshot")
        .expect("screenshot queue site");
    assert!(
        availability < queue,
        "a hidden/minimized target must be rejected before it can strand a pending request"
    );
    assert!(
        body.contains("RecvTimeoutError::Timeout")
            && body.contains("if cancellation.cancel()")
            && body.contains("done_rx.recv_timeout(std::time::Duration::from_secs(5))")
            && !body.contains("done_rx.recv()")
            && body.contains("target window is minimized, hidden, or not yet shown"),
        "timeouts must either win cancellation or bound the wait for an already-committed result, and unavailable targets must receive a prompt error"
    );
}

#[test]
fn control_screenshot_requires_a_visible_pane_crop() {
    let src = production_source();
    let body = src
        .split("fn ctl_screenshot(")
        .nth(1)
        .expect("ctl_screenshot body");
    let guard = body
        .find("if !full_window && crop.is_none() {")
        .expect("pane capture must reject a missing visible rectangle");
    let path = body.find("let (path, output_policy)").unwrap();
    let queue = body.find("renderer.set_pending_screenshot").unwrap();
    assert!(guard < path && path < queue);
    let rejection = &body[guard..path];
    assert!(rejection.contains("ec::BUSY"));
    assert!(rejection.contains("Text::CtlScreenshotPaneNotVisible"));
    assert!(rejection.contains("return;"));
}

#[test]
fn explicit_screenshot_crop_is_all_or_nothing_and_bounded() {
    assert_eq!(super::screenshot_crop(&serde_json::json!({})), Ok(None));
    assert_eq!(
        super::screenshot_crop(&serde_json::json!({
            "crop_x": 4,
            "crop_y": 5.5,
            "crop_width": 120,
            "crop_height": 36,
        })),
        Ok(Some((4.0, 5.5, 120.0, 36.0)))
    );
    for invalid in [
        serde_json::json!({"crop_x": 1}),
        serde_json::json!({
            "crop_x": -1,
            "crop_y": 0,
            "crop_width": 1,
            "crop_height": 1,
        }),
        serde_json::json!({
            "crop_x": 0,
            "crop_y": 0,
            "crop_width": 0,
            "crop_height": 1,
        }),
        serde_json::json!({
            "crop_x": "0",
            "crop_y": 0,
            "crop_width": 1,
            "crop_height": 1,
        }),
    ] {
        assert!(super::screenshot_crop(&invalid).is_err(), "{invalid}");
    }

    let source = production_source();
    let body = source
        .split("fn ctl_screenshot(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// `send_text`").next())
        .expect("ctl_screenshot body");
    assert!(
        body.contains("let crop = if let Some(crop) = explicit_crop")
            && body.contains("ScreenshotRequest {")
            && body.contains("explicit crop cannot be combined with pane")
            && body.contains("explicit crop cannot be combined with full_window=true"),
        "the parsed crop must reach the renderer without broadening its scope"
    );
}

/// Drift guard. `session_screenshot_path` is the
/// pure helper behind `Action::TakeScreenshot`. Mirrors the
/// `session_log_path` shape.
#[test]
fn session_screenshot_path_under_cache_kettle_shots() {
    use super::session_screenshot_path;
    let cache = std::path::Path::new("/home/u/.cache");
    let p = session_screenshot_path(1_716_422_400, 1234, Some(cache));
    assert_eq!(p.parent(), Some(cache.join("kettle/shots").as_path()));
    let name = p.file_stem().unwrap().to_str().unwrap();
    assert!(
        name.strip_prefix("kettle-1716422400-1234-")
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
    // Relative fallback when no cache dir resolves.
    let p = session_screenshot_path(1_716_422_400, 1234, None);
    assert_eq!(p.parent(), Some(std::path::Path::new("kettle-shots")));
    let name = p.file_stem().unwrap().to_str().unwrap();
    assert!(
        name.strip_prefix("kettle-1716422400-1234-")
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
    // .png extension is fixed (vs the `session_log_path` .log shape).
    assert_eq!(p.extension().and_then(|s| s.to_str()), Some("png"));
}

/// `validate_screenshot_path` must refuse an empty path and any destination
/// that already exists in any form, so the ctl `screenshot` method / the
/// `kettle_screenshot` MCP tool can never be pointed at an existing
/// sensitive file to silently overwrite it.
#[test]
fn validate_screenshot_path_rejects_empty_and_existing_destinations() {
    use super::validate_screenshot_path;

    assert!(validate_screenshot_path("").is_err());
    assert!(validate_screenshot_path("   ").is_err());

    let dir = tempfile::tempdir().expect("tempdir");
    // A brand-new path inside a real (existing) directory is fine.
    let fresh = dir.path().join("new-shot.png");
    assert_eq!(
        validate_screenshot_path(fresh.to_str().unwrap()).unwrap(),
        fresh
    );

    // An EXISTING regular file must be refused — this is the
    // "~/.bashrc" / "~/.ssh/authorized_keys" overwrite scenario.
    let existing_file = dir.path().join("already-here.png");
    std::fs::write(&existing_file, b"not a screenshot").expect("write fixture file");
    assert!(validate_screenshot_path(existing_file.to_str().unwrap()).is_err());

    // An EXISTING directory must also be refused.
    assert!(validate_screenshot_path(dir.path().to_str().unwrap()).is_err());
}

/// A pre-planted symlink must be refused even when its target doesn't
/// exist (a dangling symlink). `Path::exists` follows symlinks and would
/// report `false` for a dangling one, wrongly treating the destination as
/// free; `symlink_metadata` must be used instead so the attacker can't race
/// a "the target doesn't exist yet" window. Unix-only: creating a symlink on
/// Windows needs Developer Mode / admin, which isn't guaranteed in CI.
#[cfg(unix)]
#[test]
fn validate_screenshot_path_rejects_dangling_symlink() {
    use super::validate_screenshot_path;

    let dir = tempfile::tempdir().expect("tempdir");
    let link = dir.path().join("evil-link.png");
    let target = dir.path().join("does-not-exist-yet");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");
    assert!(!target.exists(), "sanity: the symlink target is dangling");
    assert!(
        validate_screenshot_path(link.to_str().unwrap()).is_err(),
        "a dangling symlink must still be refused, not silently followed/created through"
    );
}

#[test]
fn repeated_default_screenshots_have_distinct_destinations() {
    let first = super::session_screenshot_path(123, 42, None);
    let second = super::session_screenshot_path(123, 42, None);
    assert_ne!(
        first, second,
        "captures sharing a timestamp need distinct paths"
    );
}

#[test]
fn screenshot_completion_retains_pending_and_reports_failure_once() {
    let mut ws = WindowState::new(1, Mux::new());
    let (tx, rx) = std::sync::mpsc::channel();
    ws.screenshot_completion = Some(rx);
    assert!(super::poll_screenshot_completion(&mut ws).is_none());
    assert!(ws.screenshot_completion.is_some());
    tx.send(Err("destination exists".into())).unwrap();
    assert_eq!(
        super::poll_screenshot_completion(&mut ws),
        Some(Err("destination exists".into()))
    );
    assert!(super::poll_screenshot_completion(&mut ws).is_none());
}


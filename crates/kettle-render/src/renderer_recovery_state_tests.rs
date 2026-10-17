use super::{RendererRecoveryState, ScreenshotOutputPolicy, ScreenshotRequest};
use std::sync::Arc;

#[test]
fn clone_retains_live_overrides_and_screenshot_completion() {
    let (completion, finished) = std::sync::mpsc::channel();
    let expected_path = std::path::PathBuf::from("recovered-screenshot.png");
    let state = RendererRecoveryState {
        font_family: Arc::from("Live Runtime Font"),
        font_size: 19.5,
        cell_scale_w: 1.25,
        cell_scale_h: 1.5,
        pending_screenshot: Some(ScreenshotRequest {
            out_path: expected_path.clone(),
            output_policy: ScreenshotOutputPolicy::UserSelected,
            crop: Some((1.0, 2.0, 3.0, 4.0)),
            completion: Some(completion),
            cancellation: None,
            recovery_wake: None,
        }),
    };

    let retained = state.clone();
    drop(state);
    assert_eq!(retained.font_family.as_ref(), "Live Runtime Font");
    assert_eq!(retained.font_size, 19.5);
    assert_eq!((retained.cell_scale_w, retained.cell_scale_h), (1.25, 1.5));
    let request = retained.pending_screenshot.expect("queued screenshot");
    assert_eq!(request.out_path, expected_path);
    assert_eq!(request.output_policy, ScreenshotOutputPolicy::UserSelected);
    assert_eq!(request.crop, Some((1.0, 2.0, 3.0, 4.0)));
    request
        .completion
        .expect("completion sender")
        .send(Ok(expected_path.clone()))
        .unwrap();
    assert_eq!(finished.recv().unwrap(), Ok(expected_path));
}

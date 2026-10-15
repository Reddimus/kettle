use super::{
    MacosTitlebarPolicy, macos_titlebar_policy, production_source, rounded_window_corner_policy,
};

/// Both preflight rejections must stash, not just log.
///
/// A behavioural test here would need a real event loop and a real window,
/// so this pins the wiring instead. The failure it guards is quiet and
/// total: a rejected preflight leaves everything downstream behaving as if
/// there were no session, and the next save replaces the file with a single
/// fresh tab.
#[test]
fn a_rejected_session_preflight_stashes_the_file() {
    let src = production_source();
    for rejection in [
        "session.validated_restore_windows()",
        "validated_restore_surface_geometries(",
    ] {
        let at = src
            .find(rejection)
            .unwrap_or_else(|| panic!("{rejection} is no longer called from startup"));
        let arm = &src[at..src.len().min(at + 700)];
        assert!(
            arm.contains("stash_unrestorable_session"),
            "the {rejection} rejection must stash the session before anything overwrites it"
        );
    }
    assert!(
        !src.contains(r#"log::warn!("session restore rejected during preflight"#),
        "a warning alone was the whole bug: the file was gone seconds later"
    );
}

#[test]
fn decorated_windows_use_theme_matched_native_chrome() {
    assert_eq!(
        macos_titlebar_policy(false),
        MacosTitlebarPolicy::ThemeMatchedNative
    );
    let src = production_source();
    assert!(
        src.contains(
            "MacosTitlebarPolicy::ThemeMatchedNative => attrs.with_titlebar_transparent(false)"
        ),
        "decorated macOS windows must start with an opaque caption"
    );
    assert!(
        !src.contains("with_titlebar_transparent(true)"),
        "a transparent creation hint can expose the desktop before native material syncs"
    );
}

#[test]
fn borderless_windows_do_not_configure_a_native_titlebar() {
    assert_eq!(
        macos_titlebar_policy(true),
        MacosTitlebarPolicy::NotApplicable
    );
}

#[test]
fn only_live_decorated_macos_windows_receive_rounded_content_corners() {
    assert!(rounded_window_corner_policy(true, true, false));
    assert!(!rounded_window_corner_policy(true, false, false));
    assert!(!rounded_window_corner_policy(true, true, true));
    assert!(!rounded_window_corner_policy(false, true, false));

    let src = production_source();
    let corner_wiring = src
        .split("// AppKit clips a decorated window's bottom corners")
        .nth(1)
        .and_then(|rest| rest.split("let frame_result =").next())
        .expect("rounded-corner render wiring");
    assert!(
        corner_wiring.contains("window.fullscreen().is_some()"),
        "AppKit's green button bypasses Kettle's cached fullscreen action; \
             the corner policy must query winit's live native state"
    );
    assert!(
        !corner_wiring.contains("!ws.fullscreen"),
        "the cached action state desynchronizes after a native fullscreen transition"
    );
    assert!(
        corner_wiring.contains("window.is_decorated()"),
        "live reload can change cfg.borderless without changing an existing window; \
             the corner policy must query the native decoration state"
    );
    assert!(
        !corner_wiring.contains("self.cfg.borderless"),
        "configuration intent is not the existing window's decoration state"
    );
}

#[test]
fn production_wires_the_native_material_without_an_icon_override() {
    let src = production_source();
    assert!(
        !src.contains("setApplicationIconImage")
            && !src.contains("set_macos_application_icon_for_current_os"),
        "the bundle's AppIcon asset must be the one icon source for Finder, \
             Dock, app switching and the running application"
    );

    assert!(
        src.matches("NativeMaterial::install(").count() == 2,
        "both window creation paths must install the native material"
    );
    assert!(
        src.contains("material.sync(window, &self.cfg)"),
        "redraw must keep native material in step with runtime config changes"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn production_chrome_helper_keeps_native_geometry() {
    use super::apply_macos_window_chrome;

    let decorated = apply_macos_window_chrome(winit::window::Window::default_attributes(), false);
    let debug = format!("{decorated:?}");
    assert!(decorated.decorations);
    assert!(debug.contains("titlebar_transparent: false"), "{debug}");
    assert!(debug.contains("fullsize_content_view: false"), "{debug}");

    let borderless = apply_macos_window_chrome(winit::window::Window::default_attributes(), true);
    assert!(!borderless.decorations);
}

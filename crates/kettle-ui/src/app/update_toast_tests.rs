/// Source-drift guard: the `WindowEvent::Focused(f)` arm must sync
/// `focused_seq` to the focused window, or window-less events (ctl /
/// remote / update banner) misroute to a stale window.
#[test]
fn focused_event_updates_focused_seq() {
    let src = production_source();
    let arm = src
        .find("WindowEvent::Focused(f) =>")
        .expect("Focused arm present");
    let body = &src[arm..arm + 1100];
    assert!(
        body.contains("self.focused_seq = ws.seq"),
        "the Focused arm must set self.focused_seq = ws.seq when focused"
    );
}

#[test]
fn ctl_reports_update_notice_not_modal() {
    let src = production_source();
    assert!(!src.contains("\"update_available\""));
    assert!(src.contains("fields.insert(\"update_notice\".into(), update_notice.into());"));
}

/// While an update notice is active the palette lists its release notes and
/// Dismiss, findable in English from any language; otherwise neither.
#[test]
fn update_rows_present_iff_notice_active() {
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    let notice = crate::update_notice::Notice {
        kind: crate::update_notice::NoticeKind::Available,
        tag: "v5.2.0".into(),
    };
    let actions = |notice| {
        super::palette_entries(notice, &es)
            .into_iter()
            .map(|(_, action)| action)
            .collect::<Vec<_>>()
    };
    assert!(!actions(None).contains(&Action::OpenUpdate));
    let with = actions(Some(&notice));
    assert!(with.contains(&Action::OpenUpdate) && with.contains(&Action::DismissUpdate));
    let entries = super::palette_entries(Some(&notice), &es);
    let ranked = kettle_config::palette::rank_entries("release notes", &entries, &es);
    assert_eq!(entries[ranked[0]].1, Action::OpenUpdate);
}

#[test]
fn update_toast_announces_politely_and_exposes_explicit_controls() {
    let geometry = kettle_render::update_toast_geometry(
        (800.0, 600.0),
        30.0,
        1.0,
        (8.0, 16.0),
        8,
        "v6.0.0 is available.",
    )
    .unwrap();
    let toast = kettle_render::UpdateToastOverlay {
        geometry,
        message: "v6.0.0 is available".into(),
        details: "Detalles".into(),
        close: "x".into(),
        opacity: 1.0,
        focused: None,
    };
    let nodes = super::accessibility::update_toast_nodes(&toast, "Descartar");
    assert_eq!(nodes.len(), 3);
    assert_eq!(nodes[0].1.role(), accesskit::Role::Status);
    assert_eq!(nodes[0].1.live(), Some(accesskit::Live::Polite));
    assert!(!nodes[0].1.supports_action(accesskit::Action::Focus));
    for (_, button) in &nodes[1..] {
        assert_eq!(button.role(), accesskit::Role::Button);
        assert!(button.supports_action(accesskit::Action::Click));
        assert!(button.supports_action(accesskit::Action::Focus));
        assert!(button.bounds().is_some());
    }
    assert_eq!(nodes[1].1.label(), Some("Detalles"));
    assert_eq!(nodes[2].1.label(), Some("Descartar"));
}

#[test]
fn toast_claim_replies_keep_the_requesting_window_after_focus_moves() {
    let source = production_source();
    let dispatch = source
        .split("fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {")
        .nth(1)
        .and_then(|body| body.split("fn window_event(").next())
        .unwrap();
    let route = dispatch
        .split("UserEvent::UpdateToastClaimed { window, reply } => {")
        .nth(1)
        .and_then(|body| body.split("\n            UserEvent::").next())
        .expect("toast replies need their own window route, not focused-window fallback");
    assert!(route.contains("self.windows.remove(&window)"));
    assert!(route.contains("self.receive_update_toast(&mut ws, window, reply)"));
    assert!(route.contains("self.finish_window_dispatch(el, window, ws)"));
    assert!(route.contains("self.abandon_update_toast_claim(window, reply)"));
}

#[test]
fn finding_character_space_activates_focused_toast() {
    let key = winit::keyboard::Key::Character(" ".into());
    assert!(
        super::update_toast::owns_key(true, &key, winit::keyboard::ModifiersState::empty()),
        "focused toast must own character-form Space"
    );
    let mut closing = HashSet::new();
    let physical = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::Space);
    super::update_toast::guard_activation(&key, physical, &mut closing);
    assert!(closing.contains(&physical));
    let source = include_str!("window_events.rs");
    let branch = source
        .split("if toast_owns_key {")
        .nth(1)
        .unwrap()
        .split("if ws.ime_preedit")
        .next()
        .unwrap();
    assert!(branch.contains("confirmation_space_key(&key)"));
}


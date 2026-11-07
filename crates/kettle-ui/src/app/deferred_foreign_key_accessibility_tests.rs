#[test]
fn foreign_broadcast_deferred_enter_resumes_receiver_output_announcements() {
    use super::paste::{defer_authored_input, pop_ready_paste};
    use winit::keyboard::{Key, NamedKey};
    let (mut fixture, captures) = foreign_key_fixture();
    let accepted = fixture.ids[0];
    for id in &fixture.ids {
        fixture
            .ws
            .accessibility_output
            .borrow_mut()
            .entry(*id)
            .or_default();
    }
    observe_foreign_output(&fixture.ws, accepted);
    assert!(foreign_key_delivery(&mut fixture, &Key::Character("x".into()), Some("x")).accepted);
    assert_eq!(captures[0](), [b"x".to_vec()]);
    assert!(foreign_announcement_state(&fixture.ws, accepted).0);
    let untouched: Vec<_> = fixture.ids[1..]
        .iter()
        .map(|id| foreign_announcement_state(&fixture.ws, *id))
        .collect();
    let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
    parser.advance(
        &mut *fixture.ws.mux.panes[&accepted].term.term.lock().unwrap(),
        b"x",
    );
    let image = key_release_test_paste(
        accepted,
        super::QueuedPasteContent::Image {
            id: 77,
            fallback: String::new(),
            path: None,
        },
    );
    let mut queue = std::collections::VecDeque::from([image]);
    let enter =
        queued_foreign_accessibility_key(&fixture, &Key::Named(NamedKey::Enter), Some("\r"));
    assert_eq!(
        defer_authored_input(&mut queue, enter),
        Some(PaneInputResult::Queued)
    );
    assert!(pop_ready_paste(&mut queue).is_none());
    assert!(captures[0]().is_empty());
    assert!(
        foreign_announcement_state(&fixture.ws, accepted).0,
        "queue admission must not notify"
    );
    if let super::QueuedPasteContent::Image { path, .. } = &mut queue[0].content {
        *path = Some(Ok("/unused/image.png".into()));
    }
    let image = pop_ready_paste(&mut queue).unwrap();
    assert!(
        super::paste_paths_into_target(
            &mut fixture.ws.mux,
            image.target,
            &["/unused/image.png".into()],
            false
        )
        .unwrap()
        .accepted
    );
    let path_bytes = captures[0]();
    assert_eq!(path_bytes.len(), 1);
    assert!(String::from_utf8_lossy(&path_bytes[0]).contains("/unused/image.png"));
    let enter = pop_ready_paste(&mut queue).unwrap();
    assert_eq!(
        replay_foreign_accessibility_input(&mut fixture.ws, &enter),
        PaneInputResult::ReadOnly
    );
    assert_eq!(captures[0](), [b"\r".to_vec()]);
    for capture in &captures[1..] {
        assert!(capture().is_empty());
    }
    let submitted_row = foreign_announcement_state(&fixture.ws, accepted).1;
    parser.advance(
        &mut *fixture.ws.mux.panes[&accepted].term.term.lock().unwrap(),
        b"\r\nresult ready\r\n",
    );
    observe_foreign_output(&fixture.ws, accepted);
    let state = foreign_announcement_state(&fixture.ws, accepted);
    assert_eq!(
        state.3, "result ready",
        "accepted deferred Enter must let real grid output announce"
    );
    assert!(!state.0);
    assert_eq!(submitted_row, Some(0));
    assert_eq!(state.1, None, "advancing output releases the row marker");
    assert_eq!(state.4, 1);
    for (id, before) in fixture.ids[1..].iter().zip(untouched) {
        assert_eq!(foreign_announcement_state(&fixture.ws, *id), before);
    }
    eprintln!("FOREIGN_BROADCAST_DEFERRED_ENTER_A11Y_PASS");
}

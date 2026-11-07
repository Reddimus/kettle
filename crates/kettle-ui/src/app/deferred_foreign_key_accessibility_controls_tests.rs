fn foreign_key_receipt(ws: &mut WindowState, pane: u64) {
    ws.media_paste_receipt = Some(crate::window_state::MediaPasteReceiptState::new_image(
        pane,
        "retained.png".into(),
        crate::paste_image::PastedImagePreview {
            image: kettle_core::ImageData::solid(1, 1, [0, 0, 0, 255]).unwrap(),
            original_width: 1,
            original_height: 1,
        },
        false,
        true,
        std::time::Instant::now(),
    ));
}

#[test]
fn foreign_broadcast_deferred_coalescing_preserves_key_meaning_and_receipts() {
    use super::paste::{defer_authored_input, pop_ready_paste};
    use winit::keyboard::{Key, NamedKey};
    let enter = Key::Named(NamedKey::Enter);
    let edit = Key::Character("x".into());
    let arrow = Key::Named(NamedKey::ArrowUp);
    for (keys, expected, typing, row) in [
        (
            vec![(&enter, Some("\r")), (&edit, Some("x"))],
            &b"\rx"[..],
            true,
            Some(0),
        ),
        (
            vec![(&edit, Some("x")), (&enter, Some("\r"))],
            &b"x\r"[..],
            false,
            Some(0),
        ),
        (
            vec![(&enter, Some("\r")), (&arrow, None)],
            &b"\r\x1b[A"[..],
            false,
            Some(0),
        ),
        (
            vec![(&edit, Some("x")), (&arrow, None)],
            &b"x\x1b[A"[..],
            true,
            None,
        ),
    ] {
        let (mut fixture, captures) = foreign_key_fixture();
        let accepted = fixture.ids[0];
        let rejected = fixture.ids[1];
        for id in &fixture.ids {
            fixture
                .ws
                .accessibility_output
                .borrow_mut()
                .entry(*id)
                .or_default();
        }
        let rejected_before = foreign_announcement_state(&fixture.ws, rejected);
        foreign_key_receipt(&mut fixture.ws, accepted);
        let mut queue = std::collections::VecDeque::from([key_release_test_paste(
            accepted,
            super::QueuedPasteContent::Image {
                id: 1,
                fallback: String::new(),
                path: None,
            },
        )]);
        for (key, text) in keys {
            let queued = queued_foreign_accessibility_key(&fixture, key, text);
            assert_eq!(
                defer_authored_input(&mut queue, queued),
                Some(PaneInputResult::Queued)
            );
        }
        assert_eq!(
            queue.len(),
            2,
            "meaning metadata must preserve coalesced admission"
        );
        assert!(pop_ready_paste(&mut queue).is_none());
        assert_eq!(foreign_announcement_state(&fixture.ws, accepted).1, None);
        // Encoder failure releases the barrier too; it does not cancel later keys.
        if let super::QueuedPasteContent::Image { path, .. } = &mut queue[0].content {
            *path = Some(Err(std::io::Error::other("fixture encoder failure")));
        }
        assert!(matches!(
            pop_ready_paste(&mut queue).unwrap().content,
            super::QueuedPasteContent::Image {
                path: Some(Err(_)),
                ..
            }
        ));
        let input = pop_ready_paste(&mut queue).unwrap();
        assert_eq!(
            replay_foreign_accessibility_input(&mut fixture.ws, &input),
            PaneInputResult::ReadOnly
        );
        assert_eq!(captures[0](), [expected.to_vec()]);
        let state = foreign_announcement_state(&fixture.ws, accepted);
        assert_eq!((state.0, state.1), (typing, row));
        assert!(fixture.ws.media_paste_receipt.is_none());
        assert_eq!(
            foreign_announcement_state(&fixture.ws, rejected),
            rejected_before
        );
        foreign_key_receipt(&mut fixture.ws, rejected);
        assert_eq!(
            replay_foreign_accessibility_input(&mut fixture.ws, &input),
            PaneInputResult::ReadOnly
        );
        assert_eq!(
            fixture.ws.media_paste_receipt.as_ref().unwrap().pane_id,
            rejected
        );
    }
}

#[test]
fn foreign_broadcast_deferred_rejection_cancellation_empty_and_release_do_not_notify() {
    use super::paste::{defer_authored_input, pop_ready_paste};
    use winit::keyboard::{Key, NamedKey};
    for mode in [
        "failed",
        "read-only",
        "missing",
        "empty",
        "release",
        "cancelled",
        "oversize",
    ] {
        let (mut fixture, captures) = foreign_key_fixture();
        let accepted = fixture.ids[0];
        assert!(
            foreign_key_delivery(&mut fixture, &Key::Character("x".into()), Some("x")).accepted
        );
        assert_eq!(captures[0](), [b"x".to_vec()]);
        for id in &fixture.ids {
            fixture
                .ws
                .accessibility_output
                .borrow_mut()
                .entry(*id)
                .or_default();
        }
        foreign_key_receipt(&mut fixture.ws, accepted);
        let before: Vec<_> = fixture
            .ids
            .iter()
            .map(|id| foreign_announcement_state(&fixture.ws, *id))
            .collect();
        let mut key =
            queued_foreign_accessibility_key(&fixture, &Key::Named(NamedKey::Enter), Some("\r"));
        let super::QueuedPasteContent::Input {
            targets,
            prepare_input,
        } = &mut key.content
        else {
            unreachable!()
        };
        match mode {
            "read-only" => fixture.ws.mux.panes.get_mut(&accepted).unwrap().read_only = true,
            "missing" => {
                targets.clear();
                targets.push((u64::MAX, b"\r".to_vec()));
            }
            "empty" => {
                targets.clear();
                targets.push((accepted, Vec::new()));
            }
            "release" => *prepare_input = false,
            "oversize" => targets[0].1 = vec![b'x'; super::QUEUED_PASTE_BYTES_MAX + 1],
            _ => {}
        }
        let mut queue = std::collections::VecDeque::from([key_release_test_paste(
            accepted,
            super::QueuedPasteContent::Image {
                id: 1,
                fallback: String::new(),
                path: None,
            },
        )]);
        let admission = defer_authored_input(&mut queue, key);
        if mode == "oversize" {
            assert_eq!(admission, Some(PaneInputResult::Oversize));
            assert_eq!(queue.len(), 1);
        } else {
            assert_eq!(admission, Some(PaneInputResult::Queued));
            assert!(pop_ready_paste(&mut queue).is_none());
        }
        if mode == "failed" {
            drop(captures);
        }
        if !matches!(mode, "cancelled" | "oversize") {
            if let super::QueuedPasteContent::Image { path, .. } = &mut queue[0].content {
                *path = Some(Ok("unused.png".into()));
            }
            pop_ready_paste(&mut queue).unwrap();
            let input = pop_ready_paste(&mut queue).unwrap();
            let result = replay_foreign_accessibility_input(&mut fixture.ws, &input);
            if mode == "failed" {
                assert_eq!(result, PaneInputResult::Failed);
            }
            if mode == "read-only" {
                assert_eq!(result, PaneInputResult::ReadOnly);
            }
        }
        drop(queue);
        for (id, snapshot) in fixture.ids.iter().zip(before) {
            assert_eq!(
                foreign_announcement_state(&fixture.ws, *id),
                snapshot,
                "{mode}"
            );
        }
        assert_eq!(
            fixture.ws.accessibility_output.borrow().len(),
            fixture.ids.len(),
            "missing targets must not acquire announcement state"
        );
        // Empty accepted input retains the existing receipt-finalization semantics.
        if !matches!(mode, "empty") {
            assert_eq!(
                fixture.ws.media_paste_receipt.as_ref().unwrap().pane_id,
                accepted,
                "{mode}"
            );
        }
    }
}

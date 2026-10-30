#[cfg(unix)]
#[test]
fn foreign_path_paste_dismisses_only_an_accepting_receipt_pane() {
    for (member, read_only) in [(true, false), (true, true), (false, false)] {
        let cfg = kettle_config::Config {
            shell_integration: false,
            login_shell: false,
            ..kettle_config::Config::default()
        };
        let mut mux = Mux::new();
        for _ in 0..2 {
            mux.new_tab_with_geometry(
                &cfg,
                kettle_core::PtyGeometry::from_cell_size(80, 24, 8, 16),
                std::sync::Arc::new(|| {}),
                &["/bin/cat".to_owned()],
                None,
            )
            .expect("PTY fixture");
        }
        let receipt_pane = mux.active_focus().unwrap();
        for (id, pane) in &mut mux.panes {
            pane.group_name = Some(
                if *id == receipt_pane && !member {
                    "other"
                } else {
                    "g"
                }
                .into(),
            );
        }
        mux.panes.get_mut(&receipt_pane).unwrap().read_only = read_only;
        let mut ws = WindowState::new(2, mux);
        let now = std::time::Instant::now();
        ws.media_paste_receipt = Some(crate::window_state::MediaPasteReceiptState::new_image(
            receipt_pane,
            "old.png".into(),
            crate::paste_image::PastedImagePreview {
                image: kettle_core::ImageData::solid(1, 1, [0, 0, 0, 255]).unwrap(),
                original_width: 1,
                original_height: 1,
            },
            false,
            true,
            now,
        ));
        ws.pending_video_paste_receipt = Some(crate::window_state::PendingVideoPasteReceipt {
            pane_id: receipt_pane,
            generation: 1,
            request: crate::video_preview::VideoPasteRequest::from_user_paths(
                &[std::env::temp_dir().join("old.mp4")],
                crate::video_preview::VideoPasteSource::Drop,
            )
            .unwrap(),
            remote: false,
            prefer_top: true,
            created_at: now,
            previous_receipt: None,
        });
        let foreign = App::deliver_foreign_path_paste(
            &mut ws,
            &crate::mux::BroadcastScope::Group("g".into()),
            &["new.txt".into()],
            false,
        );
        ws.mux.kill_children();
        let accepted = member && !read_only;
        assert!(foreign.accepted, "another group member always accepts");
        assert_eq!(foreign.receipt_accepted, accepted);
        assert_eq!(ws.media_paste_receipt.is_none(), accepted);
        assert_eq!(ws.pending_video_paste_receipt.is_none(), accepted);
    }
}

#[cfg(unix)]
#[test]
fn image_paste_holds_enter_and_keeps_its_original_recipient() {
    use super::paste::{defer_authored_input, pop_ready_paste};

    let cfg = kettle_config::Config {
        shell_integration: false,
        login_shell: false,
        ..kettle_config::Config::default()
    };
    let mut mux = Mux::new();
    for _ in 0..2 {
        mux.new_tab_with_geometry(
            &cfg,
            kettle_core::PtyGeometry::from_cell_size(80, 24, 8, 16),
            std::sync::Arc::new(|| {}),
            &["/bin/cat".to_owned()],
            None,
        )
        .expect("PTY fixture");
    }
    let focused = mux.active_focus().unwrap();
    let target = *mux.panes.keys().find(|id| **id != focused).unwrap();
    let mut ws = WindowState::new(1, mux);
    let queued = |content| super::QueuedPaste {
        key_presses: Vec::new(),
        key_releases: Vec::new(),
        window: 1,
        target: Some(target),
        receipt_pane: Some(target),
        broadcast: crate::mux::BroadcastScope::Off,
        recipients: Vec::new(),
        content,
    };
    let mut queue = std::collections::VecDeque::from([queued(super::QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    let before = ws.mux.panes[&target].input_generation();
    let focused_before = ws.mux.panes[&focused].input_generation();
    assert_eq!(
        defer_authored_input(
            &mut queue,
            queued(super::QueuedPasteContent::Input {
                targets: vec![(target, b"\r".to_vec())],
                prepare_input: true,
            }),
        ),
        Some(PaneInputResult::Queued)
    );
    assert!(pop_ready_paste(&mut queue).is_none());
    assert_eq!(ws.mux.panes[&target].input_generation(), before);
    if let super::QueuedPasteContent::Image { path, .. } = &mut queue.front_mut().unwrap().content {
        *path = Some(Ok("/tmp/image.png".into()));
    }
    let image = pop_ready_paste(&mut queue).unwrap();
    assert!(matches!(
        image.content,
        super::QueuedPasteContent::Image { .. }
    ));
    assert!(
        super::paste_paths_into_target(
            &mut ws.mux,
            image.target,
            &["/tmp/image.png".into()],
            false,
        )
        .unwrap()
        .accepted
    );
    let after_path = ws.mux.panes[&target].input_generation();
    assert_ne!(after_path, before);
    let input = pop_ready_paste(&mut queue).unwrap();
    let super::QueuedPasteContent::Input {
        targets,
        prepare_input,
    } = input.content
    else {
        panic!("Enter must follow the image path");
    };
    let result = App::deliver_deferred_input(&mut ws, &targets, &[], prepare_input, false);
    let after_enter = ws.mux.panes[&target].input_generation();
    let focused_after = ws.mux.panes[&focused].input_generation();
    ws.mux.kill_children();
    assert_eq!(result, PaneInputResult::Queued);
    assert_ne!(after_enter, after_path);
    assert_eq!(
        focused_before, focused_after,
        "focus changes must not retarget Enter"
    );
}

/// A key held while a paste ahead of it is still encoding queues one input
/// per repeat. Each joins the entry before it, so the queue's 64 entries do
/// not drop typing; the bytes stay in order.
#[test]
fn held_keys_behind_an_encoding_paste_join_one_queue_entry() {
    let queued = |content| super::QueuedPaste {
        key_presses: Vec::new(),
        key_releases: Vec::new(),
        window: 1,
        target: Some(1),
        receipt_pane: Some(1),
        broadcast: crate::mux::BroadcastScope::Off,
        recipients: Vec::new(),
        content,
    };
    let mut queue = std::collections::VecDeque::from([queued(super::QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    for byte in (0..200u32).map(|n| b'a' + (n % 26) as u8) {
        assert_eq!(
            super::paste::defer_authored_input(
                &mut queue,
                queued(super::QueuedPasteContent::Input {
                    targets: vec![(1, vec![byte])],
                    prepare_input: true,
                }),
            ),
            Some(PaneInputResult::Queued)
        );
    }
    assert_eq!(queue.len(), 2);
    let super::QueuedPasteContent::Input { targets, .. } = &queue[1].content else {
        panic!("the typing follows the image");
    };
    let expected: Vec<u8> = (0..200u32).map(|n| b'a' + (n % 26) as u8).collect();
    assert_eq!(targets, &[(1, expected)]);
}

#[test]
fn an_accepted_press_keeps_its_release_when_the_paste_queue_fills() {
    use super::{QueuedPasteContent, TerminalKeyRelease, paste::*};
    let (mux, ids) = title_test_tab_mux(1);
    let pane = ids[0];
    let mut ws = WindowState::new(1, mux);
    enable_key_release_reporting(&ws.mux);
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
    let queued = |content| key_release_test_paste(pane, content);
    let mut queue = std::collections::VecDeque::from([queued(QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    for _ in 1..super::QUEUED_PASTES_MAX - 1 {
        queue.push_back(queued(QueuedPasteContent::Text("x".into())));
    }
    let release = TerminalKeyRelease::new(Arc::from(&b"release"[..]), false);
    let mut press = queued(QueuedPasteContent::Input {
        targets: vec![(pane, b"press".to_vec())],
        prepare_input: true,
    });
    press.key_presses.push((pane, release.clone()));
    assert_eq!(
        defer_authored_input(&mut queue, press),
        Some(PaneInputResult::Queued)
    );
    assert_eq!(queue.len(), super::QUEUED_PASTES_MAX);
    let mut releases = vec![(pane, key, release)];
    assert!(
        defer_key_releases(&mut queue, &mut releases),
        "an accepted press must retain its release despite a full queue"
    );
    assert_eq!(queue.len(), super::QUEUED_PASTES_MAX);
    assert!(releases.is_empty());
    assert_eq!(queue.back().unwrap().key_releases.len(), 1);
    assert!(pop_ready_paste(&mut queue).is_none());
    queue.pop_front(); // Complete the image that held the captured input.
    let before = ws.mux.panes[&pane].input_generation();
    let delivered = replay_key_release_test_queue(&mut ws, &mut queue);
    let after = ws.mux.panes[&pane].input_generation();
    ws.mux.kill_children();
    assert_eq!(delivered, b"pressrelease");
    assert_eq!(after, before + 2);
    assert!(queue.is_empty(), "a release is delivered exactly once");
}

#[test]
fn full_paste_queue_focus_loss_releases_an_accepted_press_once() {
    use super::{QueuedPasteContent, TerminalKeyRelease, paste::*};
    for deferred in [false, true] {
        for byte_full in [false, true] {
            let (mux, ids) = title_test_tab_mux(1);
            let pane = ids[0];
            let mut ws = WindowState::new(1, mux);
            enable_key_release_reporting(&ws.mux);
            let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
            let queued = |content| key_release_test_paste(pane, content);
            let release = TerminalKeyRelease::new(Arc::from(&b"release"[..]), !deferred);
            let before = ws.mux.panes[&pane].input_generation();
            if !deferred {
                assert_eq!(
                    ws.mux.panes[&pane].feed_input(b"press"),
                    PaneInputResult::Queued
                );
            }
            let mut queue = std::collections::VecDeque::from([queued(QueuedPasteContent::Image {
                id: 0,
                fallback: String::new(),
                path: None,
            })]);
            let mut press = queued(QueuedPasteContent::Input {
                targets: vec![(pane, b"press".to_vec())],
                prepare_input: true,
            });
            press.key_presses.push((pane, release.clone()));
            if deferred {
                assert_eq!(
                    defer_authored_input(&mut queue, press),
                    Some(PaneInputResult::Queued)
                );
            }
            if byte_full {
                let bytes =
                    super::QUEUED_PASTE_BYTES_MAX - if deferred { b"press".len() } else { 0 };
                queue.push_back(queued(QueuedPasteContent::Text("x".repeat(bytes))));
            } else {
                while queue.len() < super::QUEUED_PASTES_MAX {
                    queue.push_back(queued(QueuedPasteContent::Text("x".into())));
                }
            }
            let refused = TerminalKeyRelease::new(Arc::from(&b"unowed"[..]), false);
            let mut refused_press = queued(QueuedPasteContent::Input {
                targets: vec![(pane, b"refused".to_vec())],
                prepare_input: true,
            });
            refused_press.key_presses.push((pane, refused.clone()));
            assert_eq!(
                defer_authored_input(&mut queue, refused_press),
                Some(PaneInputResult::Oversize)
            );
            assert_eq!(
                App::release_keys_to(&ws.mux, &[(pane, key, refused)]),
                PaneInputResult::Queued
            );
            ws.terminal_owned_key_releases.insert(key);
            ws.terminal_key_recipients
                .entry(key)
                .or_default()
                .insert(pane);
            ws.terminal_key_release_bytes.insert((key, pane), release);
            let len = queue.len();
            let mut releases = Vec::new();
            super::release_terminal_owned_keys(&mut ws, |id, key, release| {
                releases.push((id, key, release))
            });
            assert_eq!(releases.len(), 1);
            assert!(defer_key_releases(&mut queue, &mut releases));
            assert!(releases.is_empty());
            super::release_terminal_owned_keys(&mut ws, |id, key, release| {
                releases.push((id, key, release))
            });
            assert!(
                releases.is_empty(),
                "focus loss consumes the obligation once"
            );
            assert_eq!(queue.len(), len, "releases need no paste queue capacity");
            assert!(ws.suppressed_key_releases.contains(&key));
            assert!(ws.terminal_key_release_bytes.is_empty());
            assert!(ws.terminal_key_recipients.is_empty());
            assert!(pop_ready_paste(&mut queue).is_none());
            // Finish the blocked image and free room for later authored input.
            queue.pop_front();
            if byte_full {
                // The last entry carries the release; retain it as the fence.
                if let QueuedPasteContent::Text(text) = &mut queue.back_mut().unwrap().content {
                    text.clear();
                }
            }
            assert_eq!(
                defer_authored_input(
                    &mut queue,
                    queued(QueuedPasteContent::Input {
                        targets: vec![(pane, b"after".to_vec())],
                        prepare_input: true,
                    })
                ),
                Some(PaneInputResult::Queued)
            );
            let mut delivered = if deferred {
                Vec::new()
            } else {
                b"press".to_vec()
            };
            delivered.extend(replay_key_release_test_queue(&mut ws, &mut queue));
            let after = ws.mux.panes[&pane].input_generation();
            #[cfg(unix)]
            let ordered_on_pty =
                wait_for_pane_screen(&ws.mux, pane, |text, _| text.contains("pressreleaseafter"));
            ws.mux.kill_children();
            assert_eq!(delivered, b"pressreleaseafter");
            assert_eq!(
                after,
                before + 3,
                "refused presses owe nothing and each release is delivered once"
            );
            assert!(queue.is_empty());
            #[cfg(unix)]
            assert!(
                ordered_on_pty,
                "the PTY must see the release after its press and before later input"
            );
        }
    }
}

#[test]
fn deferred_key_presses_bound_their_owed_release_recipients() {
    use super::{QueuedPasteContent, TerminalKeyRelease, paste::defer_authored_input};
    let queued = |content| key_release_test_paste(1, content);
    let mut queue =
        std::collections::VecDeque::from([queued(QueuedPasteContent::Text("x".into()))]);
    let mut press = queued(QueuedPasteContent::Input {
        targets: (0..1024).map(|id| (id, b"press".to_vec())).collect(),
        prepare_input: true,
    });
    press.key_presses = (0..1024)
        .map(|id| {
            (
                id,
                TerminalKeyRelease::new(Arc::from(&b"release"[..]), false),
            )
        })
        .collect();
    assert_eq!(
        defer_authored_input(&mut queue, press),
        Some(PaneInputResult::Queued)
    );
    let mut extra = queued(QueuedPasteContent::Input {
        targets: vec![(1, b"refused".to_vec())],
        prepare_input: true,
    });
    extra.key_presses.push((
        1,
        TerminalKeyRelease::new(Arc::from(&b"release"[..]), false),
    ));
    assert_eq!(
        defer_authored_input(&mut queue, extra),
        Some(PaneInputResult::Oversize)
    );
    assert_eq!(queue.len(), 2);
    assert_eq!(queue[1].key_presses.len(), 1024);
}

#[test]
fn deferred_authored_input_obeys_the_existing_queue_byte_limit() {
    let queued = |content| super::QueuedPaste {
        key_presses: Vec::new(),
        key_releases: Vec::new(),
        window: 1,
        target: Some(1),
        receipt_pane: Some(1),
        broadcast: crate::mux::BroadcastScope::Off,
        recipients: Vec::new(),
        content,
    };
    let mut queue = std::collections::VecDeque::from([queued(super::QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    assert_eq!(
        super::paste::defer_authored_input(
            &mut queue,
            queued(super::QueuedPasteContent::Input {
                targets: vec![(1, vec![b'x'; super::QUEUED_PASTE_BYTES_MAX + 1])],
                prepare_input: true,
            }),
        ),
        Some(PaneInputResult::Oversize)
    );
    assert_eq!(queue.len(), 1);
}

/// Every path paste is checked before it is formatted or delivered, so a
/// file name the target shell cannot take exactly authors no input.
#[test]
fn path_pastes_are_checked_before_they_are_formatted() {
    let source = include_str!("paste.rs");
    let body = source
        .split_once("pub(super) fn paste_paths_to(")
        .unwrap()
        .1
        .split("\n    }\n")
        .next()
        .unwrap();
    let check = body
        .find("if !self.paths_type_exactly(ws, &argv, &paths)")
        .unwrap();
    assert!(check < body.find("format_paths_for_paste").unwrap());
    assert!(check < body.find("deliver_path_paste").unwrap());
}

#[test]
fn clipboard_images_encode_off_the_window_thread_in_paste_order() {
    let source = production_source();
    let body = |name: &str| {
        source
            .split(&format!("\n    fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name} body"))
    };
    let image = body("paste_clipboard_image");
    assert!(
        image.contains("self.pasted_images.reserve_png(")
            && image.contains("encoder.submit(id, png, rgba)")
            && !image.contains("save_rgba"),
        "the window thread reserves the PNG and hands the encode to the encoder"
    );
    for name in ["paste_text", "paste_paths"] {
        assert!(
            body(name).contains("if !self.queued_pastes.is_empty()"),
            "{name} must wait behind an image still encoding"
        );
    }
    let deliver = body("deliver_queued_paste");
    assert!(
        deliver.contains(
            "(ws.mux.broadcast != broadcast || self.broadcast_recipients(&ws) != recipients)"
        ),
        "a queued broadcast paste must not reach a scope or panes changed since"
    );
}

#[test]
fn media_receipt_wiring_fails_closed_and_keeps_diagnostics_private() {
    let source = production_source();
    let confirmed = source
        .split("fn deliver_path_paste(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn show_media_paste_receipt").next())
        .expect("confirmed path delivery body");
    assert_eq!(confirmed.matches("delivery.receipt_accepted").count(), 2);
    assert_eq!(
        confirmed.matches("self.show_media_paste_receipt(").count(),
        4,
        "success, rejection, and a disappearing target must all consume the one-shot preview"
    );

    let show = source
        .split("fn show_media_paste_receipt(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("image receipt gate");
    for gate in [
        "paths.len() == 1",
        "if !accepted_by_receipt_pane",
        "receipt_pane.filter(|id| ws.mux.panes.contains_key(id))",
        "self.pasted_images.take_preview_for_path(&paths[0])",
        "video.filter(|_| self.cfg.paste_video_preview)",
    ] {
        assert!(show.contains(gate), "receipt gate lost {gate:?}");
    }
    let clipboard = source
        .split("fn paste_clipboard(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn paste_clipboard_image").next())
        .expect("clipboard paste entry point");
    let dropped = source
        .split("WindowEvent::DroppedFile(path) => {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::Focused").next())
        .expect("dropped-file entry point");
    assert!(
        clipboard.contains("VideoPasteSource::Clipboard")
            && dropped.contains("VideoPasteSource::Drop")
            && clipboard.contains("VideoPasteRequest::from_user_paths")
            && dropped.contains("VideoPasteRequest::from_user_paths")
            && !clipboard.contains("VideoPasteCandidate::from_user_paths")
            && !dropped.contains("VideoPasteCandidate::from_user_paths"),
        "video receipts must retain an explicit clipboard/drop provenance token"
    );

    let paths = source
        .split("fn paste_paths(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn deliver_path_paste").next())
        .expect("path paste body");
    assert!(
        paths.contains("if let Some(error) = rejection {")
            && paths.contains("discard_previews_for_paths(&paths)")
            && !paths.contains("dismiss_media_paste_receipt_on_input"),
        "an oversize bitmap path must release its one-shot preview"
    );

    let geometry = source
        .split("// Geometry diagnostics expose only the receipt's presentation state.")
        .nth(1)
        .and_then(|rest| rest.split("Response::ok(").next())
        .expect("ui_geometry receipt projection");
    let wait = source
        .split("fn about_to_wait_inner(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("about_to_wait receipt cleanup");
    assert!(
        wait.contains("!ws.mux.panes.contains_key(&receipt.pane_id)")
            && wait.contains("ws.media_paste_receipt = None;")
            && wait.contains("ws.pending_video_paste_receipt = None;"),
        "closing the owner pane must remove its receipt and pending validation"
    );
    assert!(
        wait.contains("let receipt_enabled")
            && wait.contains("ws.media_paste_receipt.take().is_some()")
            && wait.contains("!self.cfg.paste_video_preview")
            && wait.contains("ws.pending_video_paste_receipt = None;"),
        "turning previews off must release retained and pending receipt state"
    );
    let config_clear_at = wait
        .find("if !self.cfg.paste_video_preview")
        .expect("video-preview config clear");
    let pending_wait_at = wait
        .find("let pending_video_receipt_wait")
        .expect("pending video receipt wait");
    assert!(
        config_clear_at < pending_wait_at,
        "disabling previews must clear pending state before scheduling its wake"
    );
    assert!(
        wait.contains("pending.expired(now)")
            && wait.contains("video paste receipt abandoned: preview worker response timed out")
            && wait.contains("let pending_video_receipt_wait")
            && wait.contains("if let Some(next) = pending_video_receipt_wait"),
        "a missing worker result must expire pending state on a scheduled wake"
    );
    let gpu_recovery = wait
        .split("if self.gpu.as_ref().is_some_and(|g| g.is_lost())")
        .nth(1)
        .and_then(|rest| rest.split("if ws.pty_resize_retry.take_due(now)").next())
        .expect("GPU recovery scheduler");
    assert_eq!(
        gpu_recovery.matches("pending_video_receipt_wait").count(),
        2,
        "GPU recovery backoff must not postpone pending receipt cleanup"
    );
    let ready = source
        .split("fn user_event_inner(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("user event handler")
        .split("UserEvent::VideoPreviewReady {")
        .nth(1)
        .and_then(|rest| rest.split("UserEvent::Wakeup").next())
        .expect("video preview result handler");
    assert!(
        ready.contains("ws.finish_pending_video_preview(")
            && ready.contains("std::time::Instant::now(),"),
        "a worker reply goes through the window's expiry-checked finish"
    );

    let ctl_press = source
        .split("fn ctl_mouse_press(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("ctl mouse press body");
    assert!(
        ctl_press.contains("activate_media_paste_receipt_at("),
        "synthetic presses must not click through receipt chrome"
    );
    assert!(
        !ctl_press.contains("scrollbar_jump("),
        "ctl presses must not invent native scrollbar semantics ahead of terminal input"
    );

    let ctl_move = source
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("ctl mouse move body");
    assert!(
        ctl_move.contains("update_media_paste_receipt_hover(ws)")
            && !ctl_move.contains("sync_cursor_icon"),
        "synthetic motion may expand the receipt but must not retarget native chrome hover or the OS cursor"
    );

    let hit_test = source
        .split("fn media_paste_receipt_geometry(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("receipt hit-test projection");
    assert!(
        hit_test.contains("self.media_paste_receipt_projection(ws)?")
            && !hit_test.contains("self.overlay("),
        "receipt hit-testing must project only the receipt and completion, never all window chrome"
    );

    let activation = source
        .split("fn activate_media_paste_receipt_at(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("receipt activation body");
    assert!(
        activation.contains("receipt.right_gutter > 0.0")
            && activation.contains("pane_x + pane_w - receipt.right_gutter")
            && activation.contains("if bcode == 0 {")
            && activation.contains("MediaPasteReceiptHit::Dismiss")
            && activation.contains("MediaPasteReceiptHit::Body")
            && activation.contains("ws.media_paste_receipt = None;")
            && activation.contains("self.open_media_paste_receipt(ws)"),
        "receipt activation must preserve the scrollbar, consume covered clicks, and honor its primary actions"
    );
    let open = source
        .split("fn open_media_paste_receipt(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("shared receipt open body");
    assert!(
        open.contains("ws.mux.active_focus() == Some(receipt.pane_id)")
            && open.contains("MediaPasteReceiptKind::Image")
            && open.contains("self.pasted_images.path_still_matches(&path)")
            && open.contains("ws.media_paste_receipt = None;")
            && open.contains("NotifyTitleImagePreviewUnavailable"),
        "only a retained image may open, after rechecking pane and file identity"
    );
    let accessibility = source
        .split("fn handle_accessibility_action(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("accessibility action body");
    assert!(
        accessibility.contains("|(receipt, _)| receipt.openable")
            && accessibility.contains("self.open_media_paste_receipt(ws)")
            && accessibility.contains("ws.media_paste_receipt.take().is_some()"),
        "assistive clicks must open retained images and dismiss non-openable video receipts"
    );
    let input_dismiss = source
        .split("fn dismiss_media_paste_receipt_on_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("receipt input dismissal body");
    assert!(
        input_dismiss.contains("Self::take_media_paste_receipt_on_input(ws)")
            && source.contains("Self::dismiss_media_paste_receipt_on_input(ws);"),
        "later input must remove a receipt whose command-line claim may be stale"
    );
    let terminal_input = source
        .split("fn write_terminal_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("terminal input body");
    let broadcast_input = source
        .split("fn broadcast_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("broadcast input body");
    let text_paste = source
        .split("fn deliver_text_paste(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn paste_paths(").next())
        .expect("confirmed text paste body");
    let send_newline = source
        .split("Action::SendNewline =>")
        .nth(1)
        .and_then(|rest| rest.split("Action::EditConfig =>").next())
        .expect("send-newline action");
    let run_command = source
        .split("fn ctl_run_command(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// `run_command` is pending").next())
        .expect("run-command body");
    assert!(
        terminal_input.contains("dismiss_media_paste_receipt_after_input")
            && broadcast_input
                .matches("dismiss_media_paste_receipt_after_delivery")
                .count()
                == 2
            && text_paste
                .matches("dismiss_media_paste_receipt_after_delivery")
                .count()
                == 2
            && text_paste.contains("dismiss_media_paste_receipt_after_input")
            && send_newline.contains("write_focused_terminal_input")
            && source.contains("Self::dismiss_media_paste_receipt_after_input(ws, id, result)")
            && run_command.contains("dismiss_media_paste_receipt_for_pane"),
        "every successful authored-input route must invalidate the affected pane's receipt, including foreign broadcasts and control commands"
    );
    assert!(
        !terminal_input.contains("Self::dismiss_media_paste_receipt_on_input(ws);")
            && !text_paste.contains("Self::dismiss_media_paste_receipt_on_input(ws);")
            && broadcast_input.contains("receipt_pane"),
        "a rejected pane write must preserve its receipt instead of clearing it optimistically"
    );
    assert!(
        confirmed.contains("let previous = Self::take_media_paste_state_on_input(ws)")
            && show.contains("ws.media_paste_receipt = previous_receipt")
            && show.contains("ws.pending_video_paste_receipt = previous_pending")
            && show.contains("pending.merge_drop(pane_id, &request, now)"),
        "path delivery must restore rejected state and coalesce accepted adjacent drops before background validation"
    );
    assert!(
        geometry.contains("\"dismiss_rect\":")
            && geometry.contains("\"preview_rect\":")
            && geometry.contains("\"kind\":")
            && geometry.contains("\"openable\":")
            && !geometry.contains("\"path\":")
            && !geometry.contains("extension")
            && !geometry.contains("rgba"),
        "ui_geometry must expose safe shared controls, never retained paths or pixels"
    );

    let native_press = source
        .split("WindowEvent::MouseInput {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::MouseWheel").next())
        .expect("native mouse input body");
    let terminal_mouse = native_press
        .find("self.send_mouse(")
        .expect("terminal mouse path");
    let scrollbar = native_press
        .find("self.scrollbar_jump(")
        .expect("native scrollbar path");
    assert!(
        terminal_mouse < scrollbar,
        "terminal mouse reporting and pane gestures must retain priority over the scrollbar"
    );

    let focus = source
        .split("WindowEvent::Focused(f) => {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::Occluded").next())
        .expect("window focus handler");
    assert!(
        focus.contains("receipt.set_hover(false, std::time::Instant::now())"),
        "focus loss must not freeze the receipt lifetime"
    );

    let action = source
        .split("fn ctl_perform_action(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("ctl perform-action body");
    assert!(
        action.contains("name == \"focus_window\"")
            && action.contains("window.set_visible(true);")
            && action.contains("window.focus_window();"),
        "live UI automation needs an idempotent focus action that never hides its target"
    );
    assert!(
        action.find("name == \"focus_window\"") < action.find("Action::from_name(name)"),
        "the control-only focus action must not depend on the user keybind action table"
    );
}

#[test]
fn local_paste_limit_rejects_instead_of_silently_truncating() {
    let at_limit = "x".repeat(4 << 20);
    assert!(local_paste_within_limit(&at_limit));
    let over_limit = format!("{at_limit}y");
    assert!(!local_paste_within_limit(&over_limit));
    assert!(
        input_rejection_message(PaneInputResult::Oversize).is_some(),
        "oversized paste rejection must have visible feedback"
    );
}

#[test]
fn docked_paste_owners_are_dismissed_independently_on_input() {
    let now = std::time::Instant::now();
    let mut ws = WindowState::new(2, Mux::new());
    ws.media_paste_receipt = Some(crate::window_state::MediaPasteReceiptState::new_image(
        9,
        "old.png".into(),
        crate::paste_image::PastedImagePreview {
            image: kettle_core::ImageData::solid(1, 1, [0, 0, 0, 255]).unwrap(),
            original_width: 1,
            original_height: 1,
        },
        false,
        true,
        now,
    ));
    let mut donor = WindowState::new(1, Mux::new());
    donor.pending_video_paste_receipt = Some(crate::window_state::PendingVideoPasteReceipt {
        pane_id: 7,
        generation: 41,
        request: crate::video_preview::VideoPasteRequest::from_user_paths(
            &[std::env::current_dir().unwrap().join("missing.mp4")],
            crate::video_preview::VideoPasteSource::Drop,
        )
        .unwrap(),
        remote: false,
        prefer_top: true,
        created_at: now,
        previous_receipt: None,
    });
    let tab = crate::mux::Tab {
        root: crate::mux::Node::Leaf(7),
        focus: 7,
        title_override: None,
        zoomed: false,
        last_output_at: None,
        last_seen_at: None,
        bell: false,
        lanes: Default::default(),
    };
    ws.adopt_moved_paste_state(donor.take_moved_paste_state(&tab, now));
    App::dismiss_media_paste_receipt_after_input(&mut ws, 7, PaneInputResult::Backpressured);
    assert!(ws.expects_video_preview(7, 41));
    App::dismiss_media_paste_receipt_after_input(&mut ws, 7, PaneInputResult::Queued);
    assert!(
        !ws.expects_video_preview(7, 41),
        "accepted input must cancel its own docked preview"
    );
    assert_eq!(ws.media_paste_receipt.as_ref().unwrap().pane_id, 9);
    App::dismiss_media_paste_receipt_after_input(&mut ws, 9, PaneInputResult::Queued);
    assert!(ws.media_paste_receipt.is_none());
}

/// Census: a paste receipt follows its tab. Every function that detaches a
/// tab takes the tab's paste state before handing the tab on, and every
/// failed new-window handoff puts it back.
#[test]
fn every_tab_transfer_moves_its_paste_state() {
    let source = production_source();
    let mut transfers = 0;
    for chunk in source.split("\n    fn ").skip(1) {
        let name = chunk.split('(').next().unwrap_or_default().to_string();
        let body = chunk.split("\n    }\n").next().unwrap_or(chunk);
        if !body.contains(".mux.detach_tab(") {
            continue;
        }
        transfers += 1;
        // Each handoff needs its own take, after the previous handoff, so
        // one take cannot cover a second transfer in the same function.
        let mut handoffs: Vec<usize> = body
            .match_indices("WindowOpen::AdoptTab {")
            .filter(|(at, _)| !body[..*at].ends_with("Err("))
            .chain(body.match_indices("self.dock_tab_into("))
            .map(|(at, _)| at)
            .collect();
        handoffs.sort_unstable();
        let mut since = 0;
        for at in handoffs {
            assert!(
                body[since..at].contains(".take_moved_paste_state(&dt.tab,"),
                "{name} hands a tab on without its own paste state"
            );
            since = at;
        }
        assert_eq!(
            body.matches("Err(WindowOpen::AdoptTab { tab: dt, paste, .. })")
                .count(),
            body.matches(".restore_moved_paste_state(*paste);").count(),
            "{name} drops paste state on a failed handoff"
        );
        assert!(
            !body.contains("Err(WindowOpen::AdoptTab { tab: dt, .. })"),
            "{name} ignores paste state on a failed handoff"
        );
    }
    assert!(transfers >= 4, "census found only {transfers} transfers");
    // The receiving side adopts it, and a late video preview follows it.
    for needle in [
        "ws.mux.attach_tab(dt, None);\n                ws.adopt_moved_paste_state(*paste);",
        "let landed = target.mux.attach_tab(dt, Some(idx));\n        target.adopt_moved_paste_state(paste);",
        "video_preview_target_window(&self.windows, window_seq, pane_id, generation)",
    ] {
        assert!(source.contains(needle), "missing: {needle}");
    }
}

#[cfg(unix)]
#[test]
fn a_pending_wayland_drop_does_not_cross_another_paste() {
    let mut mux = Mux::new();
    mux.new_tab_with(
        &title_test_config(),
        80,
        24,
        8,
        16,
        Arc::new(|| {}),
        &["/bin/cat".to_owned()],
        None,
    )
    .expect("PTY fixture");
    let ws = WindowState::new(1, mux);
    let target = super::WaylandDropTarget::capture(&ws);
    let pane = &ws.mux.panes[&ws.mux.active_focus().unwrap()];
    assert_eq!(pane.feed_focus_report(true), PaneInputResult::Queued);
    assert!(target == super::WaylandDropTarget::capture(&ws));
    assert_eq!(pane.feed_input(b"new paste "), PaneInputResult::Queued);
    assert!(target != super::WaylandDropTarget::capture(&ws));
    ws.mux.kill_children();
}

/// Drift guard: pin the `copy_clipboard_decision`
/// policy. The four cases enumerate every (selection ×
/// smart_copy) combination so a future refactor that inverts
/// the smart_copy semantics or drops the empty-clobber case
/// fires this test before the regression ships.
#[test]
fn copy_clipboard_decision_smart_vs_clobber() {
    use super::copy_clipboard_decision;
    // Selection present + smart_copy = true: copy the selection.
    assert_eq!(
        copy_clipboard_decision(Some("hello".into()), true)
            .unwrap()
            .as_deref(),
        Some("hello")
    );
    // Selection present + smart_copy = false: still copy
    // (smart_copy only affects the no-selection branch).
    assert_eq!(
        copy_clipboard_decision(Some("hello".into()), false)
            .unwrap()
            .as_deref(),
        Some("hello")
    );
    // No selection + smart_copy = true (kettle default + Terminator
    // default): preserve existing clipboard — return None so the
    // caller skips the set_text call.
    assert_eq!(copy_clipboard_decision(None, true).unwrap(), None);
    // No selection + smart_copy = false: clobber clipboard with
    // empty string. Terminator's deliberate-UX-choice mode.
    assert_eq!(
        copy_clipboard_decision(None, false).unwrap().as_deref(),
        Some("")
    );
}

#[test]
fn putty_paste_source_honors_clipboard_toggle() {
    use super::{PasteSource, putty_paste_source};
    assert_eq!(putty_paste_source(false), PasteSource::Primary);
    assert_eq!(putty_paste_source(true), PasteSource::Clipboard);
}

#[test]
fn mouse_paste_routes_primary_and_putty_source() {
    let src = production_source();
    assert!(
        src.contains("if bcode == 1 && !self.cfg.disable_mouse_paste {\n                    self.paste_primary(ws);"),
        "middle-click paste must use paste_primary so X11 PRIMARY works"
    );
    assert!(
        src.contains("match putty_paste_source(self.cfg.putty_paste_style_source_clipboard)")
            && src.contains("PasteSource::Clipboard => self.paste_clipboard(ws)")
            && src.contains("PasteSource::Primary => self.paste_primary(ws)"),
        "PuTTY right-click paste must honor putty_paste_style_source_clipboard"
    );
}

#[test]
fn deferred_key_releases_fence_later_authored_input() {
    use super::{QueuedPaste, QueuedPasteContent, paste::defer_authored_input};
    let queued = |bytes: &[u8]| QueuedPaste {
        window: 1,
        target: Some(10),
        receipt_pane: None,
        broadcast: BroadcastScope::Off,
        recipients: Vec::new(),
        content: QueuedPasteContent::Input {
            targets: vec![(10, bytes.to_vec())],
            prepare_input: true,
        },
        key_presses: Vec::new(),
        key_releases: Vec::new(),
    };
    let mut press = queued(b"\x1b[1;1A");
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::ArrowUp);
    press.key_releases.push((
        10,
        key,
        super::TerminalKeyRelease::new(Arc::from(&b"\x1b[1;1:3A"[..]), true),
    ));
    let mut queue = std::collections::VecDeque::from([press]);
    assert_eq!(
        defer_authored_input(&mut queue, queued(b"typed")),
        Some(PaneInputResult::Queued)
    );
    assert_eq!(
        queue.len(),
        2,
        "later input must stay behind the saved release"
    );
    assert_eq!(queue[0].key_releases[0].2.bytes.as_ref(), b"\x1b[1;1:3A");
}

fn enable_key_release_reporting(mux: &Mux) {
    use alacritty_terminal::vte::ansi::{Handler, KeyboardModes};

    for pane in mux.panes.values() {
        pane.term
            .term
            .lock()
            .unwrap()
            .push_keyboard_mode(KeyboardModes::REPORT_EVENT_TYPES);
    }
}

#[test]
fn deferred_key_releases_only_follow_accepted_presses() {
    let (mux, rejected, accepted) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    enable_key_release_reporting(&ws.mux);
    let (mux, ids) = title_test_tab_mux(1);
    let foreign = ids[0];
    let mut other = WindowState::new(2, mux);
    enable_key_release_reporting(&other.mux);
    ws.mux.panes.get_mut(&rejected).unwrap().read_only = true;
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::ArrowUp);
    let presses: Vec<_> = [rejected, accepted, foreign]
        .into_iter()
        .map(|id| {
            (
                id,
                super::TerminalKeyRelease::new(Arc::from(&b"\x1b[1;1:3A"[..]), false),
            )
        })
        .collect();
    let targets: Vec<_> = presses
        .iter()
        .map(|(id, _)| (*id, b"\x1b[1;1A".to_vec()))
        .collect();
    let before = [
        ws.mux.panes[&rejected].input_generation(),
        ws.mux.panes[&accepted].input_generation(),
        other.mux.panes[&foreign].input_generation(),
    ];
    let local_result = App::deliver_deferred_input(&mut ws, &targets, &presses, true, false);
    let foreign_result = App::deliver_deferred_input(&mut other, &targets, &presses, true, false);
    // Physical release may arrive after replay, or wait on a later paste.
    let releases: Vec<_> = presses
        .into_iter()
        .map(|(id, release)| (id, key, release))
        .collect();
    assert_eq!(
        App::release_keys_to(&ws.mux, &releases),
        PaneInputResult::Queued
    );
    assert_eq!(
        App::release_keys_to(&other.mux, &releases),
        PaneInputResult::Queued
    );
    let after = [
        ws.mux.panes[&rejected].input_generation(),
        ws.mux.panes[&accepted].input_generation(),
        other.mux.panes[&foreign].input_generation(),
    ];
    ws.mux.kill_children();
    other.mux.kill_children();
    assert_eq!(local_result, PaneInputResult::ReadOnly);
    assert_eq!(foreign_result, PaneInputResult::Queued);
    assert_eq!(
        after,
        [before[0], before[1] + 2, before[2] + 2],
        "only panes that accepted the deferred press may receive its release"
    );
}

#[test]
fn deferred_key_releases_keep_an_accepted_press_when_a_repeat_is_rejected() {
    for immediate in [false, true] {
        let (mux, ids) = title_test_tab_mux(1);
        let pane = ids[0];
        let mut ws = WindowState::new(1, mux);
        enable_key_release_reporting(&ws.mux);
        let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::ArrowUp);
        let release = super::TerminalKeyRelease::new(Arc::from(&b"\x1b[1;1:3A"[..]), immediate);
        let targets = [(pane, b"\x1b[1;1A".to_vec())];
        let presses = [(pane, release.clone())];
        if immediate {
            assert_eq!(
                ws.mux.panes[&pane].feed_input(&targets[0].1),
                PaneInputResult::Queued
            );
        } else {
            assert_eq!(
                App::deliver_deferred_input(&mut ws, &targets, &presses, true, false),
                PaneInputResult::Queued
            );
        }
        ws.mux.panes.get_mut(&pane).unwrap().read_only = true;
        let repeat_result = App::deliver_deferred_input(&mut ws, &targets, &presses, true, false);
        ws.terminal_key_release_bytes.insert((key, pane), release);
        let mut releases = Vec::new();
        super::release_terminal_owned_keys(&mut ws, |id, key, release| {
            releases.push((id, key, release))
        });
        let before = ws.mux.panes[&pane].input_generation();
        assert_eq!(
            App::release_keys_to(&ws.mux, &releases),
            PaneInputResult::Queued
        );
        let after = ws.mux.panes[&pane].input_generation();
        ws.mux.kill_children();
        assert_eq!(repeat_result, PaneInputResult::ReadOnly);
        assert_eq!(
            after,
            before + 1,
            "a rejected repeat must not strand an accepted press"
        );
    }
}

#[test]
fn deferred_key_releases_remember_rejection_after_read_only_ends() {
    let (mux, ids) = title_test_tab_mux(1);
    let pane = ids[0];
    let mut ws = WindowState::new(1, mux);
    enable_key_release_reporting(&ws.mux);
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::ArrowUp);
    let release = super::TerminalKeyRelease::new(Arc::from(&b"\x1b[1;1:3A"[..]), false);
    ws.mux.panes.get_mut(&pane).unwrap().read_only = true;
    let result = App::deliver_deferred_input(
        &mut ws,
        &[(pane, b"\x1b[1;1A".to_vec())],
        &[(pane, release.clone())],
        true,
        false,
    );
    ws.mux.panes.get_mut(&pane).unwrap().read_only = false;
    let before = ws.mux.panes[&pane].input_generation();
    assert_eq!(
        App::release_keys_to(&ws.mux, &[(pane, key, release)]),
        PaneInputResult::Queued
    );
    let after = ws.mux.panes[&pane].input_generation();
    ws.mux.kill_children();
    assert_eq!(result, PaneInputResult::ReadOnly);
    assert_eq!(
        after, before,
        "releasing after replay must still honor press rejection"
    );
}

#[test]
fn deferred_key_releases_survive_coalesced_presses_and_pane_migration() {
    use super::{
        QueuedPaste, QueuedPasteContent,
        paste::{defer_authored_input, pop_ready_paste},
    };
    let (mux, ids) = title_test_tab_mux(1);
    let pane = ids[0];
    let mut ws = WindowState::new(1, mux);
    enable_key_release_reporting(&ws.mux);
    let queued = |content, key_presses| QueuedPaste {
        window: 1,
        target: Some(pane),
        receipt_pane: Some(pane),
        broadcast: BroadcastScope::Off,
        recipients: Vec::new(),
        content,
        key_presses,
        key_releases: Vec::new(),
    };
    let mut queue = std::collections::VecDeque::from([queued(
        QueuedPasteContent::Text("paste".into()),
        Vec::new(),
    )]);
    let mut releases = Vec::new();
    for (key, press, release) in [
        (
            winit::keyboard::KeyCode::ArrowUp,
            &b"\x1b[1;1A"[..],
            &b"\x1b[1;1:3A"[..],
        ),
        (
            winit::keyboard::KeyCode::ArrowDown,
            &b"\x1b[1;1B"[..],
            &b"\x1b[1;1:3B"[..],
        ),
    ] {
        let release = super::TerminalKeyRelease::new(Arc::from(release), false);
        assert_eq!(
            defer_authored_input(
                &mut queue,
                queued(
                    QueuedPasteContent::Input {
                        targets: vec![(pane, press.to_vec())],
                        prepare_input: true,
                    },
                    vec![(pane, release.clone())]
                )
            ),
            Some(PaneInputResult::Queued)
        );
        releases.push((pane, winit::keyboard::PhysicalKey::Code(key), release));
    }
    assert_eq!(queue.len(), 2);
    pop_ready_paste(&mut queue).unwrap();
    let input = pop_ready_paste(&mut queue).unwrap();
    let QueuedPasteContent::Input {
        targets,
        prepare_input,
    } = input.content
    else {
        panic!("coalesced presses must follow the paste");
    };
    // The pane leaves the original window before replay.
    let tab = ws.mux.detach_tab(0).unwrap();
    let mut other = WindowState::new(2, Mux::new());
    other.mux.attach_tab(tab, None);
    let before = other.mux.panes[&pane].input_generation();
    assert_eq!(
        App::deliver_deferred_input(&mut ws, &targets, &input.key_presses, prepare_input, false),
        PaneInputResult::Queued
    );
    assert_eq!(
        App::deliver_deferred_input(
            &mut other,
            &targets,
            &input.key_presses,
            prepare_input,
            false
        ),
        PaneInputResult::Queued
    );
    assert_eq!(
        App::release_keys_to(&other.mux, &releases),
        PaneInputResult::Queued
    );
    let after = other.mux.panes[&pane].input_generation();
    other.mux.kill_children();
    assert_eq!(
        after,
        before + 3,
        "one coalesced input must authorize both saved releases in the new window"
    );
}


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
        accessibility_input: Default::default(),
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
    let result = App::deliver_deferred_input(
        &mut ws,
        &targets,
        &[],
        prepare_input,
        false,
        Default::default(),
    );
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
        accessibility_input: Default::default(),
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
        accessibility_input: Default::default(),
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
        action.contains("name == \"focus_window\"") && action.contains("self.reveal_window(ws);"),
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
        scaled_zoom_owner: false,
        generated_group_name: None,
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
    let target = super::WaylandDropTarget::capture(&ws, Vec::new());
    let pane = &ws.mux.panes[&ws.mux.active_focus().unwrap()];
    assert_eq!(pane.feed_focus_report(true), PaneInputResult::Queued);
    assert!(target == super::WaylandDropTarget::capture(&ws, Vec::new()));
    assert_eq!(pane.feed_input(b"new paste "), PaneInputResult::Queued);
    assert!(target != super::WaylandDropTarget::capture(&ws, Vec::new()));
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
        accessibility_input: Default::default(),
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
    let local_result =
        App::deliver_deferred_input(&mut ws, &targets, &presses, true, false, Default::default());
    let foreign_result = App::deliver_deferred_input(
        &mut other,
        &targets,
        &presses,
        true,
        false,
        Default::default(),
    );
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
                App::deliver_deferred_input(
                    &mut ws,
                    &targets,
                    &presses,
                    true,
                    false,
                    Default::default()
                ),
                PaneInputResult::Queued
            );
        }
        ws.mux.panes.get_mut(&pane).unwrap().read_only = true;
        let repeat_result = App::deliver_deferred_input(
            &mut ws,
            &targets,
            &presses,
            true,
            false,
            Default::default(),
        );
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
        Default::default(),
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
        accessibility_input: Default::default(),
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
        App::deliver_deferred_input(
            &mut ws,
            &targets,
            &input.key_presses,
            prepare_input,
            false,
            Default::default()
        ),
        PaneInputResult::Queued
    );
    assert_eq!(
        App::deliver_deferred_input(
            &mut other,
            &targets,
            &input.key_presses,
            prepare_input,
            false,
            Default::default()
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

#[test]
fn coalesced_input_never_exceeds_one_pty_message() {
    use super::{QueuedPasteContent, defer_authored_input};
    let queued = |content| key_release_test_paste(7, content);
    let mut queue = std::collections::VecDeque::from([queued(QueuedPasteContent::Image {
        id: 1,
        fallback: String::new(),
        path: None,
    })]);
    let limit = kettle_core::term::MAX_LOCAL_ECHO_BYTES;
    let size = limit / 2 + 1;
    for byte in *b"ab" {
        assert_eq!(
            defer_authored_input(
                &mut queue,
                queued(QueuedPasteContent::Input {
                    targets: vec![(7, vec![byte; size])],
                    prepare_input: true,
                })
            ),
            Some(PaneInputResult::Queued)
        );
    }
    let mut delivered = Vec::new();
    for paste in &queue {
        if let QueuedPasteContent::Input { targets, .. } = &paste.content {
            for (_, bytes) in targets {
                assert!(
                    bytes.len() <= limit,
                    "admitted input must remain within the PTY message limit when replayed"
                );
                delivered.extend_from_slice(bytes);
            }
        }
    }
    assert_eq!(delivered.len(), size * 2);
    assert!(delivered[..size].iter().all(|byte| *byte == b'a'));
    assert!(delivered[size..].iter().all(|byte| *byte == b'b'));
}

#[test]
fn bounded_clipboard_errors_reject_paste_with_translated_feedback() {
    use kettle_i18n::{Language, Translator};
    for (error, expected) in [
        (
            arboard::Error::LimitExceeded,
            kettle_i18n::Text::NotifyBodyClipboardLimitExceeded,
        ),
        (
            arboard::Error::Timeout,
            kettle_i18n::Text::NotifyBodyClipboardTimeout,
        ),
    ] {
        let reason = super::paste::clipboard_paste_result(Err(error));
        assert!(
            reason.is_err(),
            "clipboard budget failure must reject paste with feedback"
        );
        let reason = reason.unwrap_err();
        assert_eq!(reason, expected);
        let en = Translator::new(Language::En).text(reason);
        let es = Translator::new(Language::Es).text(reason);
        assert!(en.contains("Clipboard"));
        assert_ne!(en, es);
    }
    assert_eq!(
        super::paste::clipboard_paste_result(Ok("text".into())).unwrap(),
        "text"
    );
    assert_eq!(
        super::paste::clipboard_paste_result(Err(arboard::Error::ContentNotAvailable)).unwrap(),
        ""
    );
}

#[test]
fn bounded_clipboard_extra_byte_rejects_instead_of_truncating() {
    use super::paste::clipboard_paste_result;
    assert!(clipboard_paste_result(Ok("x".repeat(4 << 20))).is_ok());
    assert_eq!(
        clipboard_paste_result(Ok("x".repeat((4 << 20) + 1))),
        Err(kettle_i18n::Text::NotifyBodyClipboardLimitExceeded)
    );
}

/// A paste question as `confirm_text_paste` asks it, with access keys.
fn paste_ask(policy: kettle_config::PasteProtection, unbracketed: bool) -> super::paste::PasteAsk {
    super::paste::PasteAsk {
        policy,
        unbracketed,
        access_keys: true,
        delayed_suspend: false,
        reasked: false,
    }
}

/// What a key press does to a dialog, as `confirm_dialog_key` computes it:
/// the handler only carries the outcome out.
fn key_outcome(
    dialog: &super::ConfirmDialogState,
    key: &str,
    mods: winit::keyboard::ModifiersState,
    stacked: bool,
) -> super::ConfirmKeyOutcome {
    use winit::keyboard::{Key, NamedKey};
    let key = match key {
        "Enter" => Key::Named(NamedKey::Enter),
        "Escape" => Key::Named(NamedKey::Escape),
        "Tab" => Key::Named(NamedKey::Tab),
        "Up" => Key::Named(NamedKey::ArrowUp),
        "Down" => Key::Named(NamedKey::ArrowDown),
        "Left" => Key::Named(NamedKey::ArrowLeft),
        "Right" => Key::Named(NamedKey::ArrowRight),
        "PageUp" => Key::Named(NamedKey::PageUp),
        "PageDown" => Key::Named(NamedKey::PageDown),
        "Home" => Key::Named(NamedKey::Home),
        "End" => Key::Named(NamedKey::End),
        "Space" => Key::Named(NamedKey::Space),
        letter => Key::Character(letter.into()),
    };
    super::confirm_key_outcome(
        dialog,
        &key,
        mods,
        super::ConfirmKeyContext {
            vim_menu_nav: true,
            stacked,
            macos: false,
        },
    )
}

/// The text activating a stop would paste, or `None` when it pastes nothing.
fn stop_paste(dialog: &super::ConfirmDialogState, index: usize) -> Option<String> {
    match super::confirm_stop_action(dialog, index)? {
        super::ConfirmStopAction::Dispatch(super::ConfirmAction::Paste { text, .. }) => Some(text),
        _ => None,
    }
}

/// The bytes a key press makes a pane receive: the outcome is carried out the
/// way the handler does, against a real pane with bracketed paste on.
fn key_bytes(dialog: &super::ConfirmDialogState, key: &str, stacked: bool) -> Vec<Vec<u8>> {
    use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};
    let (mut mux, ids) = title_test_tab_mux(1);
    let captured = mux.panes.get_mut(&ids[0]).unwrap().capture_input_for_test();
    mux.panes[&ids[0]]
        .term
        .term
        .lock()
        .unwrap()
        .set_private_mode(PrivateMode::Named(NamedPrivateMode::BracketedPaste));
    let modes = super::paste::text_paste_modes(mux.panes.iter().map(|(&id, pane)| (id, pane)));
    let outcome = key_outcome(
        dialog,
        key,
        winit::keyboard::ModifiersState::empty(),
        stacked,
    );
    if let super::ConfirmKeyOutcome::Activate(index) = outcome
        && let Some(text) = stop_paste(dialog, index)
    {
        super::paste::paste_text_into_target(&mut mux, Some(ids[0]), &text, &modes);
    }
    let bytes = captured();
    mux.kill_children();
    bytes
}

/// The question for `text` under the default policy, in English, as a
/// program without bracketed paste gets it.
fn paste_question(text: &str) -> super::ConfirmDialogState {
    super::text_paste_confirmation(
        Some(7),
        text,
        BroadcastScope::Off,
        vec![],
        paste_ask(kettle_config::PasteProtection::Auto, true),
        kettle_i18n::Translator::default(),
    )
}

fn preview_of(text: &str) -> super::paste::PastePreview {
    let anchor = crate::paste_safety::findings(text)
        .first()
        .map(|finding| finding.first);
    super::paste_preview(text, anchor, &kettle_i18n::Translator::default())
}

#[test]
fn paste_preview_is_short_and_cannot_interpret_controls() {
    assert_eq!(
        preview_of("echo hola\n\x1b[201~").text,
        "echo hola\nESC[201~"
    );
    assert_eq!(preview_of("español 界").text, "español 界");
    let preview = preview_of(&"x".repeat(LOCAL_PREVIEW_TEST_LEN)).text;
    assert_eq!(preview.chars().count(), 161);
    assert!(preview.ends_with('…'));
    let controls = preview_of(&"\x1b".repeat(1000));
    assert!(controls.text.chars().count() <= 161);
    // A token is never cut by the bound: 53 whole ESC, then the ellipsis.
    assert_eq!(controls.tokens.len(), 53);
    assert!(controls.text.ends_with("ESC…"));
    assert!(!preview_of("safe\u{202e}evil").text.contains('\u{202e}'));
}
const LOCAL_PREVIEW_TEST_LEN: usize = 1000;

#[test]
fn paste_as_one_line_removes_execution_controls_and_keeps_tabs() {
    assert_eq!(
        super::paste_as_one_line("a\r\nb\nc\rd\x03\x1b\0\te"),
        "a b c d   \te"
    );
    assert_eq!(super::paste_as_one_line("café 界"), "café 界");
    for byte in 0..32u8 {
        let text = super::paste_as_one_line(&(byte as char).to_string());
        assert_eq!(text.as_bytes(), if byte == b'\t' { b"\t" } else { b" " });
    }
}

#[test]
fn text_paste_confirmation_covers_all_entry_points_and_stays_targeted() {
    let source = include_str!("paste.rs");
    let submit = source
        .split("fn paste_text_to(")
        .nth(1)
        .unwrap()
        .split("fn confirm_text_paste(")
        .next()
        .unwrap();
    assert!(
        submit.find("local_paste_within_limit").unwrap()
            < submit.find("confirm_text_paste").unwrap()
    );
    assert!(
        submit.find("confirm_text_paste").unwrap() < submit.find("deliver_text_paste").unwrap()
    );
    let confirmation = source
        .split("fn confirm_text_paste(")
        .nth(1)
        .unwrap()
        .split("fn deliver_text_paste(")
        .next()
        .unwrap();
    assert!(
        confirmation
            .contains("text_paste_needs_confirmation(self.cfg.paste_protection, text, modes)")
    );
    assert!(submit.contains("let modes = self.text_paste_modes(ws, target)"));
    assert!(submit.contains("confirm_text_paste(ws, target, text, &modes)"));
    assert!(submit.contains("deliver_text_paste(ws, target, text, &modes)"));
    // What an answer does is `text_paste_dispatch`, tested in
    // `an_answer_does_not_cover_a_program_that_left_bracketed_paste_meanwhile`.
}

/// A pane with bracketed paste on and its input captured, the question Kettle
/// asks it about `text`, and the modes that question was asked for.
fn bracketed_question(
    text: &str,
    policy: kettle_config::PasteProtection,
) -> (
    Mux,
    u64,
    impl Fn() -> Vec<Vec<u8>>,
    super::ConfirmDialogState,
) {
    use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};
    let (mut mux, ids) = title_test_tab_mux(1);
    let pane = ids[0];
    let captured = mux.panes.get_mut(&pane).unwrap().capture_input_for_test();
    mux.panes[&pane]
        .term
        .term
        .lock()
        .unwrap()
        .set_private_mode(PrivateMode::Named(NamedPrivateMode::BracketedPaste));
    let modes = super::paste::text_paste_modes(mux.panes.iter().map(|(&id, pane)| (id, pane)));
    let dialog = super::text_paste_confirmation(
        Some(pane),
        text,
        BroadcastScope::Off,
        vec![],
        paste_ask(policy, modes.values().any(|bracketed| !bracketed)),
        kettle_i18n::Translator::default(),
    );
    (mux, pane, captured, dialog)
}

/// The recipients of a paste in `mux` as they are now, as the handler
/// samples them before it asks `text_paste_dispatch`.
fn paste_now(mux: &Mux, policy: kettle_config::PasteProtection) -> super::paste::PasteNow {
    let modes = super::paste::text_paste_modes(mux.panes.iter().map(|(&id, pane)| (id, pane)));
    let mut recipients = if mux.is_broadcast_on() {
        mux.broadcast_target_ids()
    } else {
        Vec::new()
    };
    recipients.sort_unstable();
    super::paste::PasteNow {
        broadcast: mux.broadcast.clone(),
        recipients,
        ask: paste_ask(policy, modes.values().any(|bracketed| !bracketed)),
        modes,
    }
}

#[test]
fn an_answer_does_not_cover_a_program_that_left_bracketed_paste_meanwhile() {
    use super::paste::{PasteAnswer, PasteDispatch, paste_answer, paste_text_into_target};
    use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};
    use kettle_config::PasteProtection::{Always, Auto, Off};
    let mode = PrivateMode::Named(NamedPrivateMode::BracketedPaste);
    let tr = kettle_i18n::Translator::default();
    let text = "echo one\x03\necho \x1b[31mtwo\n";
    // Activate stop `index` and carry out what `text_paste_dispatch` decides
    // as the handler does: a delivery reaches the pane through its encoder,
    // a second question is returned.
    let answer = |mux: &mut Mux, dialog: &super::ConfirmDialogState, index: usize, policy| {
        let Some(super::ConfirmStopAction::Dispatch(answered)) =
            super::confirm_stop_action(dialog, index)
        else {
            panic!("paste action")
        };
        let super::ConfirmAction::Paste { target, text, .. } = &answered else {
            panic!("paste action")
        };
        match super::paste::text_paste_dispatch(&answered, paste_now(mux, policy), tr) {
            PasteDispatch::Deliver(modes) => {
                paste_text_into_target(mux, *target, text, &modes);
                None
            }
            PasteDispatch::AskAgain(question) => Some(*question),
            PasteDispatch::Drop => panic!("the scope is unchanged"),
        }
    };

    // Asked with bracketed paste on: both controls are rows Kettle removes,
    // and the raw Paste is the last button, not a warning.
    let (mut mux, pane, captured, dialog) = bracketed_question(text, Always);
    let detail = &dialog.paste.as_ref().unwrap().detail;
    assert!(detail.findings.iter().all(|row| row.removed));
    let raw = dialog.buttons.len() - 1;
    assert!(!dialog.buttons[raw].destructive());

    // Answered in the mode it was asked for, the raw Paste arrives as the
    // rows said: bracketed, without ^C and ESC.
    assert!(answer(&mut mux, &dialog, raw, Always).is_none());
    assert_eq!(
        captured().concat(),
        b"\x1b[200~echo one\necho [31mtwo\n\x1b[201~"
    );

    // Broadcast turned on while the question was open: the answer covered
    // one pane, not a scope, and nothing is sent.
    mux.broadcast = BroadcastScope::Tab;
    let Some(super::ConfirmStopAction::Dispatch(answered)) =
        super::confirm_stop_action(&dialog, raw)
    else {
        panic!("paste action")
    };
    assert!(matches!(
        super::paste::text_paste_dispatch(&answered, paste_now(&mux, Always), tr),
        PasteDispatch::Drop
    ));
    mux.broadcast = BroadcastScope::Off;

    // The program turns bracketed paste off while the question is open. The
    // same answer would now send ^C and ESC and run both lines, so nothing
    // is sent and Kettle asks again.
    mux.panes[&pane].term.term.lock().unwrap().unset_private_mode(mode);
    for policy in [Always, Auto] {
        assert!(answer(&mut mux, &dialog, raw, policy).is_some());
    }
    // The sanitised choice still has two lines to run: asked again too.
    assert!(answer(&mut mux, &dialog, 1, Auto).is_some());
    assert!(captured().is_empty());
    // A single line that keeps a control is refused under either policy,
    // though `always` by its own rule asks only about line breaks.
    let modes = paste_now(&mux, Auto).modes;
    for policy in [Auto, Always] {
        assert_eq!(
            paste_answer(false, policy, "echo one\x03", &modes),
            PasteAnswer::AskAgain
        );
    }
    // Paste as one line has neither a line break nor a control left: it
    // reads the same in both modes and goes through.
    let one_line = (0..dialog.buttons.len())
        .find(|&index| {
            matches!(
                dialog.buttons[index],
                super::ConfirmButton::PasteOneLine { .. }
            )
        })
        .unwrap();
    assert!(answer(&mut mux, &dialog, one_line, Always).is_none());
    assert_eq!(captured().concat(), b"echo one  echo  [31mtwo ");
    // With the policy `off` Kettle does not ask at all.
    assert!(answer(&mut mux, &dialog, raw, Off).is_none());
    assert_eq!(captured().concat(), b"echo one\x03\recho \x1b[31mtwo\r");

    // The second question is asked for the modes of now: every control is a
    // live row and the raw Paste carries the warning colour. Its answer
    // stands, in this mode and after a return to bracketed paste.
    let again = answer(&mut mux, &dialog, raw, Always).unwrap();
    let detail = &again.paste.as_ref().unwrap().detail;
    assert!(!detail.findings.is_empty());
    assert!(detail.findings.iter().all(|row| !row.removed));
    assert!(again.buttons[raw].destructive());
    assert_eq!(stop_paste(&again, raw).as_deref(), Some(text));
    assert!(answer(&mut mux, &again, raw, Always).is_none());
    assert_eq!(captured().concat(), b"echo one\x03\recho \x1b[31mtwo\r");
    mux.panes[&pane].term.term.lock().unwrap().set_private_mode(mode);
    assert!(answer(&mut mux, &again, raw, Always).is_none());
    assert_eq!(
        captured().concat(),
        b"\x1b[200~echo one\necho [31mtwo\n\x1b[201~"
    );
    mux.kill_children();
}

#[test]
fn a_question_asked_again_says_first_that_the_answer_was_not_used() {
    use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};
    use kettle_i18n::{Language, Text as T, Translator};
    let text = "echo one\x03\necho two\n";
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let (mux, pane, captured, dialog) =
            bracketed_question(text, kettle_config::PasteProtection::Auto);
        let reasked = tr.text(T::ConfirmPasteReasonReasked);
        // The first question has no such sentence.
        assert!(!dialog.body.contains(reasked));
        mux.panes[&pane]
            .term
            .term
            .lock()
            .unwrap()
            .unset_private_mode(PrivateMode::Named(NamedPrivateMode::BracketedPaste));
        let Some(super::ConfirmStopAction::Dispatch(answered)) =
            super::confirm_stop_action(&dialog, dialog.buttons.len() - 1)
        else {
            panic!("paste action")
        };
        let super::paste::PasteDispatch::AskAgain(again) = super::paste::text_paste_dispatch(
            &answered,
            paste_now(&mux, kettle_config::PasteProtection::Auto),
            tr,
        ) else {
            panic!("asked again")
        };
        // The second one opens with why the answer was not used, then gives
        // the reason that holds now. A screen reader hears it first too.
        let lines: Vec<_> = again.body.lines().collect();
        assert_eq!(lines, [reasked, tr.text(T::ConfirmPasteReasonLines)]);
        let detail = &again.paste.as_ref().unwrap().detail;
        assert!(detail.description.starts_with(reasked));
        assert!(captured().is_empty());
        mux.kill_children();
    }
}

#[test]
fn a_removed_kind_never_takes_a_row_from_a_live_one() {
    use kettle_i18n::Text as T;
    let tr = kettle_i18n::Translator::default();
    // Six kinds, most serious first: the end marker, ^C, ^Z, ESC, U+202E and
    // BEL. A bracketed paste removes the first four.
    let text = "a\x1b[201~b\x03c\x1ad\x1b[31me\x07f\u{202e}g\n";
    let rows = |unbracketed: bool| {
        let dialog = super::text_paste_confirmation(
            Some(7),
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, unbracketed),
            tr,
        );
        let detail = dialog.paste.unwrap().detail;
        let rows: Vec<_> = detail
            .findings
            .iter()
            .map(|row| (row.meaning.clone(), row.removed))
            .collect();
        (rows, detail.more, detail.description)
    };
    let removed = |meaning: T| (tr.confirm_paste_finding_removed(tr.text(meaning)), true);
    let live = |meaning: T| (tr.text(meaning).to_string(), false);

    // Bracketed: U+202E and BEL still reach the program and each has a row,
    // in order of severity among the rows shown. ^Z and ESC, which Kettle
    // removes, are the two counted instead.
    let (shown, more, spoken) = rows(false);
    assert_eq!(
        shown,
        [
            removed(T::ConfirmPasteFindingEndMarker),
            removed(T::ConfirmPasteFindingInterrupt),
            live(T::ConfirmPasteFindingBidi),
            live(T::ConfirmPasteFindingBell),
        ]
    );
    assert_eq!(more, tr.confirm_paste_more_kinds(2));
    for (meaning, _) in &shown {
        assert!(spoken.contains(meaning.as_str()));
    }

    // Without bracketed paste only the marker is removed. It is why Kettle
    // asked and keeps its row; the rest are the most serious live kinds.
    let (shown, more, _) = rows(true);
    assert_eq!(
        shown,
        [
            removed(T::ConfirmPasteFindingEndMarker),
            live(T::ConfirmPasteFindingInterrupt),
            live(T::ConfirmPasteFindingSuspend),
            live(T::ConfirmPasteFindingEscape),
        ]
    );
    assert_eq!(more, tr.confirm_paste_more_kinds(2));
}

#[test]
fn paste_confirmation_buttons_keep_consent_targeted_in_each_language() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let dialog = super::text_paste_confirmation(
            Some(7),
            "one\ntwo",
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, true),
            tr,
        );
        assert_eq!(dialog.focus_idx, 0);
        assert_eq!(dialog.buttons.len(), 3);
        assert!(super::confirmation_button_action(&dialog, 0).is_none());
        assert!(super::confirmation_button_action(&dialog, 3).is_none());
        for (index, expected) in [(1, "one\ntwo"), (2, "one two")] {
            let super::ConfirmAction::Paste {
                target,
                text,
                broadcast,
                recipients,
                unbracketed: _,
            } = super::confirmation_button_action(&dialog, index).unwrap()
            else {
                panic!("paste action");
            };
            assert_eq!(target, Some(7));
            assert_eq!(text, expected);
            assert_eq!(broadcast, BroadcastScope::Off);
            assert!(recipients.is_empty());
        }
        let overlay = super::confirm_dialog_overlay(&dialog, &tr);
        for surface in [(800.0, 600.0), (240.0, 320.0)] {
            let layout =
                kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
            for (index, &(x, y, w, h)) in layout.buttons.iter().enumerate() {
                if w > 0.0 && h > 0.0 {
                    assert_eq!(
                        super::confirm_dialog_button_hit(&layout, x + w / 2.0, y + h / 2.0),
                        Some(index)
                    );
                }
            }
        }
    }
}

#[test]
fn paste_mode_change_preserves_the_checked_payload() {
    use super::paste::{paste_text_into_target, text_paste_modes, text_paste_needs_confirmation};
    use alacritty_terminal::vte::ansi::{Handler, NamedPrivateMode, PrivateMode};

    for scope in [
        BroadcastScope::Off,
        BroadcastScope::Tab,
        BroadcastScope::Group("race".into()),
    ] {
        let (mut mux, ids) = title_test_tab_mux(1);
        let target = ids[0];
        mux.broadcast = scope.clone();
        mux.panes.get_mut(&target).unwrap().group_name = Some("race".into());
        let captured = mux.panes.get_mut(&target).unwrap().capture_input_for_test();
        let mode = PrivateMode::Named(NamedPrivateMode::BracketedPaste);
        mux.panes[&target]
            .term
            .term
            .lock()
            .unwrap()
            .set_private_mode(mode);
        let modes = text_paste_modes(mux.panes.iter().map(|(&id, pane)| (id, pane)));
        let text = "echo first\necho second";
        assert!(!text_paste_needs_confirmation(
            kettle_config::PasteProtection::Auto,
            text,
            &modes
        ));
        // Deterministically schedule the PTY's disable after the decision and before encoding.
        mux.panes[&target]
            .term
            .term
            .lock()
            .unwrap()
            .unset_private_mode(mode);
        if scope == BroadcastScope::Off {
            assert_eq!(
                paste_text_into_target(&mut mux, Some(target), text, &modes),
                Some(PaneInputResult::Queued)
            );
        } else {
            assert!(mux.broadcast_paste_delivery(text, None, &modes).accepted);
        }
        let local = captured();
        if matches!(scope, BroadcastScope::Group(_)) {
            // A named group uses the same snapshot in a foreign window.
            assert!(
                mux.broadcast_paste_foreign_delivery(&scope, text, None, &modes)
                    .accepted
            );
            assert_eq!(captured(), local);
        }
        mux.kill_children();
        assert_eq!(
            local,
            [b"\x1b[200~echo first\necho second\x1b[201~".to_vec()],
            "{scope:?}"
        );
    }
}

#[test]
fn paste_controls_dialog_summarises_and_keeps_all_choices_accessible() {
    use super::{ConfirmKey as K, ConfirmKeyResult as R, confirm_dialog_keypress};
    use kettle_i18n::{Language, Text, Translator};
    let text = "one\r\ntwo\x1b[31mred\x1b[0m\x07";
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let mut dialog = super::text_paste_confirmation(
            Some(7),
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, true),
            tr,
        );
        assert_eq!(dialog.buttons.len(), 4);
        assert_eq!(dialog.focus_idx, 0);
        let detail = dialog.paste.as_ref().unwrap().detail.clone();
        assert!(detail.summary.contains(&tr.confirm_paste_lines(2)));
        assert!(detail.summary.contains(&tr.confirm_paste_escapes(2)));
        assert!(detail.summary.contains(&tr.confirm_paste_controls(1)));
        assert!(detail.preview.contains("ESC[31mredESC[0mBEL"));
        // The sanitised paste is the first choice after Cancel; the raw
        // paste comes last, in the warning colour.
        let super::ConfirmAction::Paste {
            target,
            text: clean,
            ..
        } = super::confirmation_button_action(&dialog, 1).unwrap()
        else {
            panic!("paste action")
        };
        assert_eq!(target, Some(7));
        assert_eq!(clean, "one\r\ntwored");
        let overlay = super::confirm_dialog_overlay(&dialog, &tr);
        assert_eq!(
            overlay
                .buttons
                .iter()
                .map(|button| button.destructive)
                .collect::<Vec<_>>(),
            [false, false, false, true]
        );
        let super::ConfirmAction::Paste { text: raw, .. } =
            super::confirmation_button_action(&dialog, 3).unwrap()
        else {
            panic!("paste action")
        };
        assert_eq!(raw, text);
        assert_eq!(confirm_dialog_keypress(0, 4, K::Enter), R::Activate(0));
        assert_eq!(confirm_dialog_keypress(0, 4, K::ShiftTab), R::Move(3));
        assert_eq!(confirm_dialog_keypress(3, 4, K::Tab), R::Move(0));
        assert_eq!(confirm_dialog_keypress(2, 4, K::Right), R::Move(3));
        assert_eq!(confirm_dialog_keypress(3, 4, K::Left), R::Move(2));
        assert_eq!(confirm_dialog_keypress(3, 4, K::Enter), R::Activate(3));
        assert_eq!(confirm_dialog_keypress(3, 4, K::Escape), R::Cancel);
        assert_eq!(confirm_dialog_keypress(3, 4, K::Yes), R::Confirm);
        assert_eq!(confirm_dialog_keypress(3, 4, K::No), R::Cancel);
        for surface in [(800.0, 600.0), (240.0, 320.0), (120.0, 140.0)] {
            for index in 0..4 {
                dialog.focus_idx = index;
                let overlay = super::confirm_dialog_overlay(&dialog, &tr);
                let layout =
                    kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
                dialog.scroll = layout.scroll_to_button(index);
                let overlay = super::confirm_dialog_overlay(&dialog, &tr);
                let layout =
                    kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
                let (x, y, w, h) = layout.buttons[index];
                assert!(w > 0.0 && h > 0.0);
                assert_eq!(
                    super::confirm_dialog_button_hit(&layout, x + w / 2.0, y + h / 2.0),
                    Some(index)
                );
                let projection = super::modal_accessibility_projection(
                    &kettle_render::Overlay {
                        confirm_dialog: Some(overlay.clone()),
                        ..Default::default()
                    },
                    surface,
                    (8.0, 16.0),
                    1.0,
                    &tr,
                );
                let node = &projection
                    .nodes
                    .iter()
                    .find(|(id, _)| Some(*id) == projection.focus)
                    .unwrap()
                    .1;
                assert_eq!(node.label(), Some(overlay.buttons[index].label.as_str()));
                assert_eq!(
                    node.bounds(),
                    Some(accesskit::Rect::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(x + w),
                        f64::from(y + h)
                    ))
                );
                assert!(
                    projection
                        .nodes
                        .iter()
                        .any(|(_, n)| n.role() == accesskit::Role::AlertDialog
                            && n.description() == Some(detail.description.as_str()))
                );
            }
        }
        let overlay = super::confirm_dialog_overlay(&dialog, &tr);
        assert_eq!(
            overlay.buttons[1].label,
            tr.text(Text::ConfirmPasteWithoutControls)
        );
    }
    for text in ["one\ntwo", "tabs\tonly"] {
        assert_eq!(
            super::text_paste_confirmation(
                None,
                text,
                BroadcastScope::Off,
                vec![],
                paste_ask(kettle_config::PasteProtection::Auto, true),
                Translator::default()
            )
            .buttons
            .len(),
            3
        );
    }
    for text in ["\x7f", "\u{85}", "\x1b", "\0"] {
        assert_eq!(
            super::text_paste_confirmation(
                None,
                text,
                BroadcastScope::Off,
                vec![],
                paste_ask(kettle_config::PasteProtection::Auto, true),
                Translator::default()
            )
            .buttons
            .len(),
            4
        );
    }
}

#[test]
fn paste_preview_spells_controls_in_ascii_and_shows_only_three_lines() {
    let preview = preview_of("a\x1b\x07\x18\x7f\u{85}\u{9b}\x08");
    assert_eq!(preview.text, "aESCBEL^XDELU+0085CSI^H");
    let tokens: Vec<_> = preview
        .tokens
        .iter()
        .map(|range| &preview.text[range.clone()])
        .collect();
    assert_eq!(tokens, ["ESC", "BEL", "^X", "DEL", "U+0085", "CSI", "^H"]);
    // No control picture: the U+2400 block draws as tiny letters that overlap.
    let preview = preview_of("a\x1bb\x07\x03");
    assert_eq!(preview.text, "aESCbBEL^C");
    assert!(
        !preview
            .text
            .chars()
            .any(|c| ('\u{2400}'..='\u{243f}').contains(&c))
    );
    assert_eq!(preview_of("1\r\n2\r3\n4secret").text, "1\n2\n3…");
    // The line break that ends the text draws no empty line of its own.
    assert_eq!(preview_of("echo a\n").text, "echo a");
    assert_eq!(preview_of("echo a\r\n").text, "echo a");
    assert_eq!(preview_of("a\n\nb\n").text, "a\n\nb");
    assert_eq!(preview_of("a\n\n").text, "a\n");
    assert_eq!(preview_of("safe\u{202e}evil").text, "safeU+202Eevil");
    assert_eq!(preview_of("\u{9d}0;t\u{9c}\u{90}").text, "OSC0;tSTDCS");
}

#[test]
fn a_tab_is_whitespace_in_the_preview_and_never_a_finding() {
    let preview = preview_of("a\tb\x03");
    // No caret alarm and no chip for the tab; the real control keeps both.
    assert_eq!(preview.text, "a    b^C");
    assert_eq!(preview.tokens.len(), 1);
    assert_eq!(preview.tokens[0], 6..8);
    assert!(!preview.text.contains("^I"));
    let tabs = preview_of("col1\tcol2\tcol3");
    assert_eq!(tabs.text, "col1    col2    col3");
    assert!(tabs.tokens.is_empty());
    assert_eq!(tabs.spoken, tabs.text);
    // A paste of tabs and line breaks alone has nothing to list.
    let dialog = paste_question("a\tb\nc\td");
    let detail = &dialog.paste.as_ref().unwrap().detail;
    assert!(detail.findings.is_empty() && detail.preview_tokens.is_empty());
    assert_eq!(dialog.buttons.len(), 3);
    assert!(
        !detail.description.contains("^I")
            && !detail.summary.iter().any(|part| part.contains("control"))
    );
}

#[test]
fn the_preview_window_moves_to_the_most_dangerous_control() {
    let marker = "\x1b[201~";
    // The capture's shape: a harmless line break first, the end marker far
    // below it. The opening window would stop long before the marker.
    for text in [
        format!("echo ok\n{}{marker}; rm -rf ~\n", "x".repeat(5000)),
        format!("{}{marker}tail", "x".repeat(5000)),
        format!("1\n2\n3\n4\n5\nsix{marker}"),
        format!("\x07{}\n{marker}", "y".repeat(400)),
        format!("\x03\x1a{}{marker}", "z".repeat(170)),
    ] {
        let preview = preview_of(&text);
        assert!(preview.text.starts_with('…'), "{:?}", preview.text);
        assert!(preview.text.contains("ESC[201~"), "{:?}", preview.text);
        assert!(preview.text.chars().count() <= 162);
        assert!(preview.text.matches('\n').count() <= 2);
        // The marker's introducer is a token of the moved window.
        assert!(
            preview
                .tokens
                .iter()
                .any(|range| &preview.text[range.clone()] == "ESC"
                    && preview.text[range.end..].starts_with("[201~"))
        );
    }
    // Forty characters of the marker's own line lead in, no more.
    let preview = preview_of(&format!("{}{marker}", "x".repeat(5000)));
    assert_eq!(preview.text, format!("…{}ESC[201~", "x".repeat(40)));
    // A line that starts closer than that starts the window.
    let preview = preview_of(&format!("{}\nab{marker}cd", "x".repeat(400)));
    assert_eq!(preview.text, "…abESC[201~cd");
    // The C1 form moves the window too, and reads "CSI 201~" in its chip.
    let preview = preview_of(&format!("{}\u{9b}201~", "x".repeat(5000)));
    assert!(preview.text.ends_with("CSI201~"), "{:?}", preview.text);
    // A less serious control that comes first does not hold the window.
    let preview = preview_of(&format!("\x07{}\x03", "x".repeat(300)));
    assert!(preview.text.ends_with("^C") && !preview.text.contains("BEL"));
    // A marker the opening window already shows whole leaves it at the top.
    let preview = preview_of(&format!("ab{marker}{}", "x".repeat(500)));
    assert!(preview.text.starts_with("abESC[201~xxx"));
    // One that the 160-character bound would cut moves it.
    let preview = preview_of(&format!("{}{marker}", "x".repeat(156)));
    assert!(preview.text.starts_with('…') && preview.text.ends_with("ESC[201~"));
    // Work is bounded however large the clipboard.
    let huge = format!("{}{marker}", "界".repeat(1 << 20));
    let preview = preview_of(&huge);
    assert_eq!(preview.text, format!("…{}ESC[201~", "界".repeat(40)));
    // A bad anchor is ignored, not sliced.
    let tr = kettle_i18n::Translator::default();
    for anchor in [(9999, 10000), (1, 2), (3, 2)] {
        assert_eq!(super::paste_preview("é\x03", Some(anchor), &tr).text, "é^C");
    }
}

#[test]
fn the_dialog_shows_a_late_end_marker_after_a_harmless_newline() {
    let text = format!("echo ok\n{}\x1b[201~; echo injected\n", "x".repeat(5000));
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        // Bracketed paste is on: the marker alone asked.
        let dialog = super::text_paste_confirmation(
            Some(7),
            &text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, false),
            tr,
        );
        let detail = &dialog.paste.as_ref().unwrap().detail;
        assert!(detail.preview.contains("ESC[201~; echo injected"));
        assert_eq!(detail.findings[0].token, "ESC[201~");
        // Kettle removes the marker whichever button is chosen; the row
        // says so and is not drawn as a live risk.
        assert_eq!(
            detail.findings[0].meaning,
            tr.confirm_paste_finding_removed(
                tr.text(kettle_i18n::Text::ConfirmPasteFindingEndMarker)
            )
        );
        assert!(detail.findings[0].removed);
        assert_eq!(dialog.body, tr.confirm_paste_reason_marker("ESC[201~"));
        // The chip survives layout at both widths.
        let overlay = super::confirm_dialog_overlay(&dialog, &tr);
        for surface in [(1280.0, 800.0), (520.0, 700.0)] {
            let layout =
                kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
            let detail = layout.detail.as_ref().unwrap();
            assert_eq!(detail.tokens.len(), 1);
            assert!(detail.preview_lines.concat().contains(" ESC [201~"));
        }
    }
}

#[test]
fn the_first_sentence_says_why_kettle_asked() {
    use kettle_config::PasteProtection::{Always, Auto};
    use kettle_i18n::{Language, Text as T, Translator};
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let body = |text: &str, policy, unbracketed| {
            super::text_paste_confirmation(
                None,
                text,
                BroadcastScope::Off,
                vec![],
                paste_ask(policy, unbracketed),
                tr,
            )
            .body
        };
        let lines = tr.text(T::ConfirmPasteReasonLines);
        // The 7-bit marker, bracketed paste on: that sentence alone.
        assert_eq!(
            body("x\x1b[201~y", Auto, false),
            tr.confirm_paste_reason_marker("ESC[201~")
        );
        // The C1 form is named as what it is, not as ESC[201~.
        let c1 = body("x\u{9b}201~y", Auto, false);
        assert_eq!(c1, tr.confirm_paste_reason_marker("CSI 201~"));
        assert!(c1.contains("CSI 201~") && !c1.contains("ESC[201~"));
        // Both forms: the one that ranks first.
        assert!(body("\u{9b}201~\x1b[201~", Auto, false).contains("ESC[201~"));
        // Bracketed paste off and line breaks: removing the marker does not
        // make the line breaks safe, and the dialog says so.
        for marker in ["\x1b[201~", "\u{9b}201~"] {
            let both = body(&format!("a\n{marker}b"), Auto, true);
            let (first, second) = both.split_once('\n').expect("two sentences");
            assert!(first.contains("201~"), "{first}");
            assert_eq!(second, lines);
            // Off, but nothing to run as Enter: the marker sentence alone.
            assert!(!body(&format!("a{marker}b"), Auto, true).contains('\n'));
            // On: the line breaks stay inside the bracket.
            assert!(!body(&format!("a\n{marker}b"), Always, false).contains('\n'));
        }
        // No marker.
        assert_eq!(body("one\ntwo", Auto, true), lines);
        assert_eq!(body("one\ntwo\x03", Always, true), lines);
        assert_eq!(
            body("one\x03two", Auto, true),
            tr.text(T::ConfirmPasteReasonControls)
        );
        assert_eq!(
            body("one\ntwo", Always, false),
            tr.text(T::ConfirmPasteReasonAlways)
        );
        assert!(body("one\ntwo", Always, false).contains(tr.text(T::SettingsValueAlways)));
    }
    let en = Translator::new(Language::En);
    assert!(
        en.confirm_paste_reason_marker("ESC[201~")
            .starts_with("This text contains ESC[201~, the code that ends a bracketed paste.")
    );
    assert!(
        Translator::new(Language::Es)
            .confirm_paste_reason_marker("CSI 201~")
            .starts_with("Este texto contiene CSI 201~, el código que termina")
    );
}

#[test]
fn the_summary_drops_zero_counts_and_names_a_trailing_line_break() {
    // The parts as one wide line shows them.
    let summary = |text: &str| {
        paste_question(text)
            .paste
            .unwrap()
            .detail
            .summary
            .join(" \u{b7} ")
    };
    // The captured command: one line that ends with a line break.
    assert_eq!(
        summary("printf 'x'\x1b[201~; echo injected\n"),
        "1\u{a0}line, ends with a line break \u{b7} 1\u{a0}escape sequence"
    );
    assert_eq!(summary("one\ntwo"), "2\u{a0}lines");
    assert_eq!(
        summary("one\r\ntwo\r\n"),
        "2\u{a0}lines, ends with a line break"
    );
    assert_eq!(
        summary("a\x03b"),
        "1\u{a0}line \u{b7} 1\u{a0}other control character"
    );
    assert_eq!(
        summary("a\x1b[31m\x1b[0m\x07\x07\n\n"),
        "2\u{a0}lines, ends with a line break \u{b7} 2\u{a0}escape sequences \u{b7} \
         2\u{a0}other control characters"
    );
    for text in ["one\ntwo", "a\x03b", "x\x1b[201~\n"] {
        let summary = summary(text);
        assert!(
            !summary.split(" \u{b7} ").any(|part| part.starts_with('0')),
            "{summary}"
        );
    }
    let es = super::text_paste_confirmation(
        None,
        "uno\n",
        BroadcastScope::Off,
        vec![],
        paste_ask(kettle_config::PasteProtection::Auto, true),
        kettle_i18n::Translator::new(kettle_i18n::Language::Es),
    );
    assert_eq!(
        es.paste.unwrap().detail.summary.join(" \u{b7} "),
        "1\u{a0}línea, termina con un salto de línea"
    );
}

#[test]
fn findings_list_each_kind_with_its_meaning_most_serious_first() {
    use kettle_i18n::{Language, Text as T, Translator};
    let text = "\x07\x08\x7f\x1b[0m\x04\x1c\x1a\x03\x03\u{9b}201~\x1b[201~\x01\u{85}";
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let dialog = super::text_paste_confirmation(
            None,
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, true),
            tr,
        );
        let detail = &dialog.paste.as_ref().unwrap().detail;
        let rows: Vec<_> = detail
            .findings
            .iter()
            .map(|row| (row.token.as_str(), row.meaning.as_str(), row.count.as_str()))
            .collect();
        // Four rows at most, then how many kinds are left.
        assert_eq!(
            rows,
            [
                (
                    "ESC[201~",
                    tr.confirm_paste_finding_removed(tr.text(T::ConfirmPasteFindingEndMarker))
                        .as_str(),
                    "\u{d7}1"
                ),
                (
                    "CSI 201~",
                    tr.confirm_paste_finding_removed(tr.text(T::ConfirmPasteFindingEndMarkerC1))
                        .as_str(),
                    "\u{d7}1"
                ),
                // Bracketed paste is off here, so these reach the program.
                ("^C", tr.text(T::ConfirmPasteFindingInterrupt), "\u{d7}2"),
                ("^Z", tr.text(T::ConfirmPasteFindingSuspend), "\u{d7}1"),
            ]
        );
        assert_eq!(
            detail
                .findings
                .iter()
                .map(|row| row.removed)
                .collect::<Vec<_>>(),
            [true, true, false, false]
        );
        assert_eq!(detail.more, tr.confirm_paste_more_kinds(8));
    }
    let rows = |text: &str| {
        paste_question(text)
            .paste
            .unwrap()
            .detail
            .findings
            .into_iter()
            .map(|row| (row.token, row.meaning))
            .collect::<Vec<_>>()
    };
    let row = |token: &str, meaning: &str| (token.to_string(), meaning.to_string());
    // Backspace and delete are separate rows.
    assert_eq!(
        rows("a\x08b\x7f"),
        [row("^H", "backspace"), row("DEL", "delete")]
    );
    assert_eq!(
        rows("\x1c\x04\x1b\x07"),
        [
            row("^\\", "quit (Ctrl+\\)"),
            row("^D", "end of input (Ctrl+D)"),
            row("ESC", "read as the Esc key or the start of a key sequence"),
            row("BEL", "bell"),
        ]
    );
    assert_eq!(
        rows("\x01\u{9d}\u{85}"),
        [
            // Kinds of one rank are listed by code point.
            row("U+0085", "8-bit control code"),
            row("OSC", "8-bit control code"),
            row("^A", "control character"),
        ]
    );
    assert!(paste_question("\x01").paste.unwrap().detail.more.is_empty());
    // A line break is a reason to ask, not a finding.
    assert!(rows("one\ntwo").is_empty());
}

#[test]
fn vim_y_never_chooses_a_paste_that_can_run_lines() {
    use super::{ConfirmButton, ConfirmKeyOutcome as O};
    use winit::keyboard::ModifiersState as M;
    let question = |text: &str, unbracketed: bool| {
        super::text_paste_confirmation(
            Some(7),
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, unbracketed),
            kettle_i18n::Translator::default(),
        )
    };
    let sanitising = |dialog: &super::ConfirmDialogState| {
        let index = dialog
            .buttons
            .iter()
            .position(|button| matches!(button, ConfirmButton::PasteWithoutControls { .. }))
            .unwrap();
        assert_eq!(dialog.paste.as_ref().unwrap().recommended, index);
        index
    };
    // What `y` and `Y` do on every focus, the link included.
    let answers = |dialog: &mut super::ConfirmDialogState| {
        let mut seen = Vec::new();
        for focus in 0..dialog.stop_count() {
            dialog.focus_idx = focus;
            for key in ["y", "Y"] {
                seen.push(key_outcome(dialog, key, M::empty(), false));
            }
        }
        seen
    };

    // A program without bracketed paste, and sanitised text that keeps its
    // line break: a `y` typed ahead of the dialog would run the line. It
    // does nothing, on every focus, and nothing reaches the pane.
    let text = "safe\x1b[201~; rm -rf ~\x03\n";
    let mut unbracketed = question(text, true);
    let sanitised = sanitising(&unbracketed);
    assert_eq!(
        stop_paste(&unbracketed, sanitised).as_deref(),
        Some("safe; rm -rf ~\n")
    );
    assert_eq!(unbracketed.yes_button(), None);
    assert!(answers(&mut unbracketed).iter().all(|&o| o == O::Ignore));
    assert!(key_bytes(&unbracketed, "y", false).is_empty());

    // The same text with every recipient bracketed: the line break is pasted
    // text, not Enter. `y` answers with the sanitising choice whatever is
    // focused, and the pane receives it inside the brackets.
    let mut dialog = question(text, false);
    assert_eq!(sanitising(&dialog), sanitised);
    assert!(
        answers(&mut dialog)
            .iter()
            .all(|&o| o == O::Activate(sanitised))
    );
    dialog.focus_idx = 3;
    assert_eq!(
        key_bytes(&dialog, "y", false),
        [b"\x1b[200~safe; rm -rf ~\n\x1b[201~".to_vec()]
    );

    // Without bracketed paste but on one line, the sanitised text has no
    // Enter in it: `y` takes it.
    let mut one_line = question("safe\x1b[201~; rm -rf ~\x03", true);
    let index = sanitising(&one_line);
    assert_eq!(stop_paste(&one_line, index).as_deref(), Some("safe; rm -rf ~"));
    assert!(
        answers(&mut one_line)
            .iter()
            .all(|&o| o == O::Activate(index))
    );

    // Without controls the recommended choice is the plain Paste, which is
    // the text as it is. A bare `y` must not send that: it does nothing, on
    // every focus, and nothing reaches the pane.
    let raw_lines = "echo one\necho two\n";
    let mut plain = paste_question(raw_lines);
    assert!(matches!(
        plain.buttons[plain.paste.as_ref().unwrap().recommended],
        ConfirmButton::Confirm { .. }
    ));
    assert_eq!(plain.yes_button(), None);
    assert!(answers(&mut plain).iter().all(|&o| o == O::Ignore));
    assert!(key_bytes(&plain, "y", false).is_empty());
    // Enter on the focused button still chooses it.
    plain.focus_idx = 1;
    assert_eq!(
        key_outcome(&plain, "Enter", M::empty(), false),
        O::Activate(1)
    );
    assert_eq!(stop_paste(&plain, 1).as_deref(), Some(raw_lines));

    // No bare key, on any focus, in any of these dialogs, sends the text as
    // it is. Where a recipient lacks bracketed paste, none sends a line
    // break either.
    for (dialog, raw, unbracketed) in [
        (&mut unbracketed, text, true),
        (&mut dialog, text, false),
        (&mut one_line, "safe\x1b[201~; rm -rf ~\x03", true),
        (&mut plain, raw_lines, true),
    ] {
        for focus in 0..dialog.stop_count() {
            dialog.focus_idx = focus;
            for code in 0x21u8..0x7f {
                let key = char::from(code).to_string();
                for mods in [M::empty(), M::SHIFT] {
                    if let O::Activate(index) = key_outcome(dialog, &key, mods, false) {
                        let sent = stop_paste(dialog, index);
                        assert_ne!(
                            sent.as_deref(),
                            Some(raw),
                            "bare {key:?} on focus {focus} sends the raw text"
                        );
                        assert!(
                            !(unbracketed && sent.is_some_and(|sent| sent.contains(['\n', '\r']))),
                            "bare {key:?} on focus {focus} sends a line to an unbracketed program"
                        );
                    }
                }
            }
        }
    }
    // `n` cancels; `h` and `l` only move focus.
    dialog.focus_idx = 1;
    assert_eq!(key_outcome(&dialog, "n", M::empty(), false), O::Cancel);
    assert_eq!(key_outcome(&dialog, "l", M::empty(), false), O::Focus(2));
    assert_eq!(key_outcome(&dialog, "h", M::empty(), false), O::Focus(0));
    // With `vim-menu-nav` off none of the letters does anything.
    for key in ["y", "n", "h", "l"] {
        let off = super::confirm_key_outcome(
            &dialog,
            &winit::keyboard::Key::Character(key.into()),
            M::empty(),
            super::ConfirmKeyContext {
                vim_menu_nav: false,
                stacked: false,
                macos: false,
            },
        );
        assert_eq!(off, O::Ignore, "{key}");
    }
    // Any other confirmation keeps `y` on its affirmative button.
    let close = super::ConfirmDialogState {
        heading: String::new(),
        body: String::new(),
        buttons: vec![
            ConfirmButton::Cancel,
            ConfirmButton::Confirm {
                label: "Close".into(),
                destructive: true,
            },
        ],
        focus_idx: 0,
        on_confirm: super::ConfirmAction::ClosePane(1),
        scroll: 0,
        armed_press: None,
        space_pressed: None,
        paste: None,
    };
    assert_eq!(key_outcome(&close, "y", M::empty(), false), O::Activate(1));
}

#[test]
fn one_button_is_recommended_and_cancel_keeps_focus() {
    for (text, styles) in [
        // (destructive, neutral) per button.
        (
            "a\x1b[201~b",
            vec![(false, true), (false, false), (false, true), (true, false)],
        ),
        (
            "one\ntwo",
            vec![(false, true), (false, false), (false, true)],
        ),
    ] {
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            let dialog = super::text_paste_confirmation(
                None,
                text,
                BroadcastScope::Off,
                vec![],
                paste_ask(kettle_config::PasteProtection::Auto, true),
                tr,
            );
            assert_eq!(dialog.focus_idx, 0);
            assert!(matches!(dialog.buttons[0], super::ConfirmButton::Cancel));
            let overlay = super::confirm_dialog_overlay(&dialog, &tr);
            assert_eq!(overlay.focus_idx, 0);
            assert_eq!(
                overlay
                    .buttons
                    .iter()
                    .map(|button| (button.destructive, button.neutral))
                    .collect::<Vec<_>>(),
                styles
            );
            // Exactly one button is neither neutral nor destructive: the
            // accent one. (An invalid flag would fail the comparison above:
            // one toolchain read the label's length for it.)
            let accent: Vec<_> = (0..overlay.buttons.len())
                .filter(|&index| {
                    !overlay.buttons[index].neutral && !overlay.buttons[index].destructive
                })
                .collect();
            assert_eq!(accent, [dialog.paste.as_ref().unwrap().recommended]);
        }
    }
    // A close question has no recommended button and keeps its colours.
    let close = confirmation_test_overlay();
    assert!(close.buttons.iter().all(|button| !button.neutral));
}

#[test]
fn access_keys_match_the_typed_letter_with_alt_and_do_not_exist_on_macos() {
    use winit::keyboard::{Key, ModifiersState as M, NamedKey};
    let target = super::confirm_access_key_target;
    let key = |letter: &str| Key::Character(letter.into());
    let text = "a\x1b[201~b";
    let dialog = paste_question(text);
    let paste = dialog.paste.as_ref().unwrap();
    assert_eq!(
        paste.access_keys,
        [Some('C'), Some('W'), Some('O'), Some('P')]
    );
    assert_eq!(paste.detail.link_access_key, Some('S'));
    // Alt and the letter the layout produced, in either case.
    for (letter, stop) in [("c", 0), ("w", 1), ("W", 1), ("o", 2), ("p", 3), ("s", 4)] {
        assert_eq!(target(&dialog, &key(letter), M::ALT, false), Some(stop));
        assert_eq!(
            target(&dialog, &key(letter), M::ALT | M::SHIFT, false),
            Some(stop)
        );
        // A bare letter does nothing: a key typed ahead cannot choose.
        assert_eq!(target(&dialog, &key(letter), M::empty(), false), None);
        assert_eq!(target(&dialog, &key(letter), M::SHIFT, false), None);
        // AltGr (Ctrl+Alt on Windows) and Super chords are not access keys.
        assert_eq!(
            target(&dialog, &key(letter), M::ALT | M::CONTROL, false),
            None
        );
        assert_eq!(
            target(&dialog, &key(letter), M::ALT | M::SUPER, false),
            None
        );
        // macOS has no access keys.
        assert_eq!(target(&dialog, &key(letter), M::ALT, true), None);
    }
    // Only the character counts. On AZERTY the key in the US W position
    // types z, and Alt+z must not reach "Paste without controls"; a layout
    // with no Latin letter on the key types something else entirely.
    for other in ["z", "q", "x", "ц", "ω", "1", "ww", ""] {
        assert_eq!(target(&dialog, &key(other), M::ALT, false), None, "{other}");
    }
    assert_eq!(
        target(&dialog, &Key::Named(NamedKey::Enter), M::ALT, false),
        None
    );
    // The same through the whole key map the handler runs: Alt+W is the
    // sanitised paste; a bare letter is nothing and reaches no pane. The map
    // takes a logical key and modifiers, and has no position to read.
    use super::ConfirmKeyOutcome as O;
    assert_eq!(key_outcome(&dialog, "w", M::ALT, false), O::Activate(1));
    assert_eq!(
        key_outcome(&dialog, "W", M::ALT | M::SHIFT, true),
        O::Activate(1)
    );
    assert_eq!(key_outcome(&dialog, "s", M::ALT, false), O::Activate(4));
    assert_eq!(stop_paste(&dialog, 1).as_deref(), Some("ab"));
    assert_eq!(stop_paste(&dialog, 4), None);
    for letter in ["w", "o", "p", "c", "s", "z"] {
        assert_eq!(
            key_outcome(&dialog, letter, M::empty(), false),
            O::Ignore,
            "{letter}"
        );
        assert!(key_bytes(&dialog, letter, false).is_empty(), "{letter}");
    }
    assert_eq!(key_outcome(&dialog, "z", M::ALT, false), O::Ignore);
    assert_eq!(
        key_outcome(&dialog, "w", M::ALT | M::CONTROL, false),
        O::Ignore
    );
    let on_macos = super::confirm_key_outcome(
        &dialog,
        &key("w"),
        M::ALT,
        super::ConfirmKeyContext {
            vim_menu_nav: true,
            stacked: false,
            macos: true,
        },
    );
    assert_eq!(on_macos, O::Ignore);

    // Spanish letters, each in its own label.
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    let es = super::text_paste_confirmation(
        None,
        text,
        BroadcastScope::Off,
        vec![],
        paste_ask(kettle_config::PasteProtection::Auto, true),
        tr,
    );
    for (letter, stop) in [("c", 0), ("s", 1), ("u", 2), ("p", 3), ("g", 4)] {
        assert_eq!(target(&es, &key(letter), M::ALT, false), Some(stop));
    }
    assert_eq!(target(&es, &key("w"), M::ALT, false), None);
    let overlay = super::confirm_dialog_overlay(&es, &tr);
    for button in &overlay.buttons {
        let key = button.access_key.expect("an access key");
        assert!(kettle_render::access_key_index(&button.label, key).is_some());
    }
    // Without access keys (macOS) nothing is drawn or answered.
    let mac = super::text_paste_confirmation(
        None,
        text,
        BroadcastScope::Off,
        vec![],
        super::paste::PasteAsk {
            access_keys: super::confirm_access_keys(true),
            ..paste_ask(kettle_config::PasteProtection::Auto, true)
        },
        kettle_i18n::Translator::default(),
    );
    let paste = mac.paste.as_ref().unwrap();
    assert!(paste.access_keys.iter().all(Option::is_none));
    assert_eq!(paste.detail.link_access_key, None);
    let overlay = super::confirm_dialog_overlay(&mac, &kettle_i18n::Translator::default());
    assert!(
        overlay
            .buttons
            .iter()
            .all(|button| button.access_key.is_none())
    );
    assert_eq!(target(&mac, &key("w"), M::ALT, false), None);
    assert!(super::confirm_access_keys(false) && !super::confirm_access_keys(true));
}

#[test]
fn focus_walks_the_link_then_the_buttons_and_arrows_follow_the_layout() {
    use super::{ConfirmKey as K, ConfirmKeyResult as R, ConfirmStops, confirm_stop_keypress};
    let dialog = paste_question("a\x1b[201~b");
    assert_eq!(dialog.link_stop(), Some(4));
    assert_eq!(dialog.stop_count(), 5);
    let stops = |stacked| ConfirmStops {
        buttons: 4,
        link: true,
        stacked,
    };
    const LINK: usize = 4;
    for stacked in [false, true] {
        let press = |focus, key| confirm_stop_keypress(focus, stops(stacked), key);
        // Tab: the link, Cancel, the three pastes, and round again.
        let mut focus = LINK;
        let mut order = Vec::new();
        for _ in 0..5 {
            let R::Move(next) = press(focus, K::Tab) else {
                panic!("Tab moves")
            };
            order.push(next);
            focus = next;
        }
        assert_eq!(order, [0, 1, 2, 3, LINK]);
        assert_eq!(press(0, K::ShiftTab), R::Move(LINK));
        assert_eq!(press(LINK, K::ShiftTab), R::Move(3));
        // Left and Right walk the same order and stop at its ends.
        assert_eq!(press(LINK, K::Left), R::Ignore);
        assert_eq!(press(LINK, K::Right), R::Move(0));
        assert_eq!(press(0, K::Left), R::Move(LINK));
        assert_eq!(press(2, K::Right), R::Move(3));
        assert_eq!(press(3, K::Right), R::Ignore);
        assert_eq!(press(LINK, K::Enter), R::Activate(LINK));
        assert_eq!(press(0, K::Enter), R::Activate(0));
        assert_eq!(press(LINK, K::Escape), R::Cancel);
    }
    // Stacked: Up and Down move one stop, whether or not the body scrolls.
    let press = |focus, key| confirm_stop_keypress(focus, stops(true), key);
    assert_eq!(press(0, K::Down), R::Move(1));
    assert_eq!(press(1, K::Down), R::Move(2));
    assert_eq!(press(2, K::Up), R::Move(1));
    assert_eq!(press(0, K::Up), R::Move(LINK));
    assert_eq!(press(LINK, K::Down), R::Move(0));
    assert_eq!(press(LINK, K::Up), R::Ignore);
    assert_eq!(press(3, K::Down), R::Ignore);
    // A row under the link: Up reaches the link, Down returns to Cancel.
    let press = |focus, key| confirm_stop_keypress(focus, stops(false), key);
    for button in 0..4 {
        assert_eq!(press(button, K::Up), R::Move(LINK));
        assert_eq!(press(button, K::Down), R::Ignore);
    }
    assert_eq!(press(LINK, K::Down), R::Move(0));
    assert_eq!(press(LINK, K::Up), R::Ignore);
    // A plain row of buttons has nothing above it: Up and Down scroll, as
    // they always did. A plain stack moves focus.
    let plain = |stacked| ConfirmStops {
        buttons: 2,
        link: false,
        stacked,
    };
    assert_eq!(confirm_stop_keypress(0, plain(false), K::Up), R::Scroll(-1));
    assert_eq!(
        confirm_stop_keypress(1, plain(false), K::Down),
        R::Scroll(1)
    );
    assert_eq!(confirm_stop_keypress(0, plain(true), K::Down), R::Move(1));
    assert_eq!(confirm_stop_keypress(1, plain(true), K::Up), R::Move(0));
    assert_eq!(confirm_stop_keypress(0, plain(true), K::Up), R::Ignore);
    // The same from real keys through the handler's key map, with the
    // layout's own answer to "are the buttons stacked".
    use super::{ConfirmKeyOutcome as O, ConfirmScroll};
    use winit::keyboard::ModifiersState as M;
    let tr = kettle_i18n::Translator::default();
    let overlay = super::confirm_dialog_overlay(&dialog, &tr);
    let mut dialog = dialog;
    for (surface, expect_stacked) in [((1280.0, 800.0), false), ((520.0, 700.0), true)] {
        let stacked =
            kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay).stacked;
        assert_eq!(stacked, expect_stacked);
        dialog.focus_idx = 0;
        assert_eq!(
            key_outcome(&dialog, "Up", M::empty(), stacked),
            O::Focus(LINK)
        );
        assert_eq!(
            key_outcome(&dialog, "Down", M::empty(), stacked),
            if stacked { O::Focus(1) } else { O::Ignore }
        );
        dialog.focus_idx = LINK;
        assert_eq!(
            key_outcome(&dialog, "Down", M::empty(), stacked),
            O::Focus(0)
        );
        assert_eq!(
            key_outcome(&dialog, "Tab", M::empty(), stacked),
            O::Focus(0)
        );
        assert_eq!(key_outcome(&dialog, "Tab", M::SHIFT, stacked), O::Focus(3));
        assert_eq!(
            key_outcome(&dialog, "Enter", M::empty(), stacked),
            O::Activate(LINK)
        );
        // Scrolling keeps its own keys in every layout.
        for (key, scroll) in [
            ("PageUp", ConfirmScroll::Pages(-1)),
            ("PageDown", ConfirmScroll::Pages(1)),
            ("Home", ConfirmScroll::Start),
            ("End", ConfirmScroll::End),
        ] {
            assert_eq!(
                key_outcome(&dialog, key, M::empty(), stacked),
                O::Scroll(scroll)
            );
        }
        assert_eq!(
            key_outcome(&dialog, "Escape", M::empty(), stacked),
            O::Cancel
        );
        assert_eq!(key_outcome(&dialog, "Space", M::empty(), stacked), O::Arm);
        assert_eq!(key_outcome(&dialog, "Space", M::ALT, stacked), O::Ignore);
    }
    // A plain row of buttons keeps Up and Down for scrolling.
    let close = super::ConfirmDialogState {
        heading: String::new(),
        body: String::new(),
        buttons: vec![
            super::ConfirmButton::Cancel,
            super::ConfirmButton::Confirm {
                label: "Close".into(),
                destructive: true,
            },
        ],
        focus_idx: 0,
        on_confirm: super::ConfirmAction::ClosePane(1),
        scroll: 0,
        armed_press: None,
        space_pressed: None,
        paste: None,
    };
    assert_eq!(
        key_outcome(&close, "Down", M::empty(), false),
        O::Scroll(ConfirmScroll::Lines(1))
    );
    assert_eq!(key_outcome(&close, "Down", M::empty(), true), O::Focus(1));
}

#[test]
fn the_settings_link_sends_nothing_and_opens_settings_at_paste_protection() {
    use kettle_i18n::{Language, Text as T, Translator};
    for (language, policy, value) in [
        (
            Language::En,
            kettle_config::PasteProtection::Auto,
            "Paste protection: Automatic",
        ),
        (
            Language::Es,
            kettle_config::PasteProtection::Always,
            "Protección al pegar: Siempre",
        ),
    ] {
        let tr = Translator::new(language);
        let dialog = super::text_paste_confirmation(
            Some(7),
            "a\x1b[201~b\n",
            BroadcastScope::Off,
            vec![],
            paste_ask(policy, false),
            tr,
        );
        let detail = &dialog.paste.as_ref().unwrap().detail;
        assert_eq!(detail.policy, value);
        assert_eq!(detail.link, tr.text(T::ConfirmPasteSettings));
        // The link is a focus stop that never starts focused and has no
        // paste action: activating it delivers no bytes.
        let link = dialog.link_stop().unwrap();
        assert_eq!(link, dialog.buttons.len());
        assert_ne!(dialog.focus_idx, link);
        assert!(super::confirmation_button_action(&dialog, link).is_none());
        assert_eq!(stop_paste(&dialog, link), None);
        assert!(matches!(
            super::confirm_stop_action(&dialog, link),
            Some(super::ConfirmStopAction::PasteSettings)
        ));
        // The pointer reaches it where it is painted.
        let overlay = super::confirm_dialog_overlay(&dialog, &tr);
        for surface in [(1280.0, 800.0), (520.0, 700.0)] {
            let layout =
                kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
            let (x, y, w, h) = layout.stop_rect(link).unwrap();
            assert!(w > 0.0 && h > 0.0);
            assert_eq!(
                super::confirm_dialog_button_hit(&layout, x + w / 2.0, y + h / 2.0),
                Some(link)
            );
            let armed = Some((link, (x, y, w, h)));
            assert_eq!(
                super::confirm_release_target(armed, &layout, x + 1.0, y + 1.0),
                Some(link)
            );
        }
    }
    // Settings opens on the Paste protection row of Behavior.
    let categories = crate::settings::categories(&[]);
    let mut nav = crate::settings::SettingsNav {
        category: 3,
        field: 1,
        capturing: true,
        search_focused: true,
        ..Default::default()
    };
    assert!(nav.focus_key(&categories, "paste-protection"));
    let field = &categories[nav.category].fields[nav.field];
    assert_eq!(field.key, "paste-protection");
    assert_eq!(field.label, T::SettingsFieldPasteProtection);
    assert_eq!(categories[nav.category].name, T::SettingsCategoryBehavior);
    assert!(!nav.capturing && !nav.search_focused);
    assert!(!nav.focus_key(&categories, "no-such-key"));
    assert_eq!(
        categories[nav.category].fields[nav.field].key,
        "paste-protection"
    );
}

#[test]
fn the_settings_link_is_cancel_then_settings_for_queued_input_and_broadcast() {
    use super::{ConfirmFollowUp, ConfirmStopAction, answer_confirm_stop, confirm_stop_action};
    // A paste question over a tab that broadcasts to two panes, each with
    // its input captured, as it stands when the user reaches for the link.
    let window = || {
        let (mut mux, ids) = title_test_tab_mux(2);
        mux.broadcast = BroadcastScope::Tab;
        let captures: Vec<_> = ids
            .iter()
            .map(|id| mux.panes.get_mut(id).unwrap().capture_input_for_test())
            .collect();
        let mut ws = super::WindowState::new(0, mux);
        ws.confirm_dialog = Some(super::text_paste_confirmation(
            None,
            "echo secret\x1b[201~; rm -rf ~\n",
            BroadcastScope::Tab,
            ids.clone(),
            paste_ask(kettle_config::PasteProtection::Auto, true),
            kettle_i18n::Translator::default(),
        ));
        (ws, captures)
    };
    let categories = crate::settings::categories(&[]);

    // While the question is unanswered, input that comes ready behind it is
    // refused, not delivered.
    let (mut by_link, link_captures) = window();
    let (mut by_cancel, cancel_captures) = window();
    assert!(super::paste::confirmation_refuses_queued_input(&by_link));

    // The link's action carries no paste and no consent to dispatch.
    let dialog = by_link.confirm_dialog.as_ref().unwrap();
    let link = dialog.link_stop().unwrap();
    assert!(matches!(
        confirm_stop_action(dialog, link),
        Some(ConfirmStopAction::PasteSettings)
    ));
    assert!(matches!(
        confirm_stop_action(dialog, 0),
        Some(ConfirmStopAction::Cancel)
    ));
    assert!(confirm_stop_action(dialog, link + 1).is_none());

    // The link answers as Cancel does, and leaves only Settings to open, on
    // the Paste protection row. The app then opens it as `open_settings`
    // and `focus_settings_key` do.
    let Some(ConfirmFollowUp::Settings(key)) = answer_confirm_stop(&mut by_link, link) else {
        panic!("the link opens Settings")
    };
    assert_eq!(key, "paste-protection");
    let mut nav = crate::settings::SettingsNav::default();
    assert!(nav.focus_key(&categories, key));
    by_link.settings_nav = Some(nav);
    assert!(answer_confirm_stop(&mut by_cancel, 0).is_none());
    // With the question gone, a second press answers nothing.
    assert!(answer_confirm_stop(&mut by_cancel, 0).is_none());

    for (ws, captures) in [(&by_link, &link_captures), (&by_cancel, &cancel_captures)] {
        // The question is gone with its text, target and broadcast consent.
        assert!(ws.confirm_dialog.is_none());
        // No pane of the broadcast received a byte.
        for captured in captures {
            assert!(captured().is_empty());
        }
        // Later input is judged on its own again, the same after either.
        assert!(!super::paste::confirmation_refuses_queued_input(ws));
        // The broadcast scope itself is the user's and is left alone.
        assert_eq!(ws.mux.broadcast, BroadcastScope::Tab);
    }
    // Only Settings differs between the two windows.
    assert!(by_cancel.settings_nav.is_none());
    let nav = by_link.settings_nav.as_ref().unwrap();
    assert_eq!(
        categories[nav.category].fields[nav.field].key,
        "paste-protection"
    );
    by_link.mux.kill_children();
    by_cancel.mux.kill_children();
}

#[test]
fn the_accessibility_tree_names_what_the_dialog_paints() {
    use accesskit::Role;
    use kettle_i18n::{Language, Text as T, Translator};
    let text = "echo ok\n\x1b[201~\x03 rm\x07 -rf ~\n";
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        for access_keys in [true, false] {
            let mut dialog = super::text_paste_confirmation(
                Some(7),
                text,
                BroadcastScope::Off,
                vec![],
                super::paste::PasteAsk {
                    access_keys,
                    ..paste_ask(kettle_config::PasteProtection::Auto, true)
                },
                tr,
            );
            let link_stop = dialog.link_stop().unwrap();
            for (surface, focus) in [
                ((1280.0, 800.0), 0),
                ((1280.0, 800.0), link_stop),
                ((520.0, 700.0), 2),
                ((520.0, 700.0), link_stop),
            ] {
                dialog.focus_idx = focus;
                let overlay = super::confirm_dialog_overlay(&dialog, &tr);
                let layout =
                    kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
                let projection = super::modal_accessibility_projection(
                    &kettle_render::Overlay {
                        confirm_dialog: Some(overlay.clone()),
                        ..Default::default()
                    },
                    surface,
                    (8.0, 16.0),
                    1.0,
                    &tr,
                );
                let node = |id| {
                    &projection
                        .nodes
                        .iter()
                        .find(|(node, _)| *node == id)
                        .expect("a node for the id")
                        .1
                };
                let rect = |(x, y, w, h): (f32, f32, f32, f32)| {
                    accesskit::Rect::new(
                        f64::from(x),
                        f64::from(y),
                        f64::from(x + w),
                        f64::from(y + h),
                    )
                };
                let dialog_node = node(projection.roots[0]);
                assert_eq!(dialog_node.role(), Role::AlertDialog);
                assert_eq!(dialog_node.label(), Some(tr.text(T::ConfirmPasteHeading)));
                assert_eq!(dialog_node.bounds(), Some(rect(layout.rect)));
                // What is read is the paste question's own description, not
                // the reason alone.
                let read = dialog.paste.as_ref().unwrap().detail.description.as_str();
                assert_eq!(dialog_node.description(), Some(read));
                assert_ne!(read, dialog.body);
                // Children in painted order: the link, then the four buttons.
                let children = dialog_node.children();
                assert_eq!(children.len(), 5);
                let link = node(children[0]);
                assert_eq!(link.role(), Role::Link);
                assert_eq!(link.label(), Some(tr.text(T::ConfirmPasteSettings)));
                assert_eq!(
                    link.bounds(),
                    Some(rect(layout.stop_rect(link_stop).unwrap()))
                );
                let said = link.description().unwrap();
                assert!(said.starts_with(tr.text(T::ConfirmPasteA11ySettings)));
                for (index, id) in children[1..].iter().enumerate() {
                    let button = node(*id);
                    assert_eq!(button.role(), Role::Button);
                    assert_eq!(button.label(), Some(overlay.buttons[index].label.as_str()));
                    assert_eq!(button.bounds(), Some(rect(layout.buttons[index])));
                    // The access key is on the node and in what is read, or
                    // on neither where the platform has none.
                    match overlay.buttons[index].access_key {
                        Some(key) => {
                            assert!(access_keys);
                            assert_eq!(button.access_key(), Some(key.to_string().as_str()));
                            assert_eq!(
                                button.description(),
                                Some(tr.confirm_paste_a11y_access_key(&key.to_string()).as_str())
                            );
                        }
                        None => {
                            assert!(!access_keys);
                            assert_eq!(button.access_key(), None);
                            assert_eq!(button.description(), None);
                        }
                    }
                }
                assert_eq!(link.access_key().is_some(), access_keys);
                assert_eq!(said.contains("Alt+"), access_keys);
                // Focus is the painted focus, the link included.
                let focused = if focus == link_stop {
                    children[0]
                } else {
                    children[focus + 1]
                };
                assert_eq!(projection.focus, Some(focused));
                // Its actions reach the same stop.
                assert_eq!(
                    super::confirmation_accessibility_action(
                        children[0],
                        accesskit::Action::Click,
                        dialog.stop_count()
                    ),
                    Some(super::ConfirmAccessibilityAction::Activate(link_stop))
                );
                assert_eq!(
                    super::confirmation_accessibility_action(
                        children[0],
                        accesskit::Action::Click,
                        dialog.buttons.len()
                    ),
                    None
                );
            }
        }
        // What is read: the reason, the summary, each finding, the policy,
        // then the preview, with every control as words.
        let dialog = super::text_paste_confirmation(
            Some(7),
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, true),
            tr,
        );
        let detail = &dialog.paste.as_ref().unwrap().detail;
        let said = &detail.description;
        let order = [
            dialog.body.clone(),
            detail.summary.join(" \u{b7} "),
            tr.confirm_paste_a11y_finding(
                "ESC[201~",
                &tr.confirm_paste_finding_removed(tr.text(T::ConfirmPasteFindingEndMarker)),
                1,
            ),
            tr.confirm_paste_a11y_finding("^C", tr.text(T::ConfirmPasteFindingInterrupt), 1),
            tr.confirm_paste_a11y_finding("BEL", tr.text(T::ConfirmPasteFindingBell), 1),
            detail.policy.clone(),
            tr.confirm_paste_a11y_preview("echo ok"),
        ];
        let mut from = 0;
        for part in &order {
            let at = said[from..]
                .find(part.as_str())
                .unwrap_or_else(|| panic!("{part:?} is missing or out of order in {said:?}"));
            from += at + part.len();
        }
        // A control is said as a control, never as the bare word or picture.
        let preview = &said[from..];
        let escape = tr.text(T::ConfirmPasteA11yEscape);
        assert!(preview.contains(&format!("{escape} [201~")), "{preview}");
        assert!(preview.contains(&tr.confirm_paste_a11y_control("Ctrl+C")));
        assert!(
            preview.contains(&tr.confirm_paste_a11y_control(tr.text(T::ConfirmPasteFindingBell)))
        );
        assert!(!preview.contains("ESC") && !preview.contains('\x1b'));
        assert!(!said.chars().any(|c| c.is_control() && c != '\n'));
    }
    let en = Translator::new(Language::En);
    assert_eq!(
        en.text(T::ConfirmPasteA11yEscape),
        "control character escape"
    );
    let spoken = preview_of("a\x1bb\x08\x7f\u{9b}c\x01").spoken;
    assert_eq!(
        spoken,
        "a control character escape b control character backspace control character delete \
         control character CSI c control character Ctrl+A"
    );
}

#[test]
fn control_output_carries_rectangles_and_fixed_labels_never_the_pasted_text() {
    // Every piece of text a paste question derives from the clipboard, or
    // from who is asked, carries a canary. None may reach `ui_geometry`.
    let canary = |name: &str| format!("CANARY-{name}-7f3a");
    let button = |label: &str, key: char| kettle_render::ConfirmDialogButton {
        label: label.into(),
        access_key: Some(key),
        ..Default::default()
    };
    let preview = format!("{} ESC[201~", canary("preview"));
    let token = preview.len() - 8..preview.len() - 5;
    let overlay = kettle_render::ConfirmDialogOverlay {
        heading: canary("heading"),
        body: canary("body"),
        buttons: vec![button("Cancel", 'C'), button("Paste without controls", 'W')],
        detail: Some(kettle_render::ConfirmDialogDetail {
            preview,
            preview_tokens: vec![token],
            summary: vec![canary("summary-lines"), canary("summary-controls")],
            findings: vec![kettle_render::ConfirmDialogFinding {
                token: canary("finding-token"),
                meaning: canary("finding-meaning"),
                count: canary("finding-count"),
                removed: false,
            }],
            more: canary("more"),
            policy: canary("policy"),
            link: "Paste settings…".into(),
            link_access_key: Some('S'),
            description: canary("description"),
        }),
        ..Default::default()
    };
    for surface in [(1280.0, 800.0), (420.0, 300.0)] {
        let layout = kettle_render::ConfirmDialogLayout::new(surface, (8.0, 16.0), 1.0, &overlay);
        let json = super::confirmation_json(&overlay, &layout);
        let text = serde_json::to_string(&json).unwrap();
        assert!(!text.contains("CANARY"), "{text}");
        assert!(!text.contains("7f3a") && !text.contains("201~"), "{text}");
        // What it does carry: rectangles, flags and the fixed labels.
        assert_eq!(json["buttons"][0]["label"], "Cancel");
        assert_eq!(json["buttons"][1]["label"], "Paste without controls");
        assert_eq!(json["buttons"][1]["access_key"], "W");
        assert_eq!(json["detail"]["link"]["label"], "Paste settings…");
        assert_eq!(json["detail"]["link"]["access_key"], "S");
        assert_eq!(json["detail"]["token_rects"].as_array().unwrap().len(), 1);
        assert_eq!(json["detail"]["finding_rects"].as_array().unwrap().len(), 2);
        assert!(json["detail"]["preview_rect"]["width"].is_number());
        // Every string in the output is one of those labels or keys.
        fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
            match value {
                serde_json::Value::String(text) => out.push(text.clone()),
                serde_json::Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
                serde_json::Value::Object(map) => map.values().for_each(|item| strings(item, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        strings(&json, &mut found);
        found.sort();
        assert_eq!(
            found,
            [
                "C",
                "Cancel",
                "Paste settings…",
                "Paste without controls",
                "S",
                "W"
            ]
        );
    }
    // The real projection of a real paste question, the same way.
    let secret = "printf 'zq-secret-zq'\x1b[201~; echo zq-injected-zq\n";
    let tr = kettle_i18n::Translator::default();
    let overlay = super::confirm_dialog_overlay(&paste_question(secret), &tr);
    let layout =
        kettle_render::ConfirmDialogLayout::new((1280.0, 800.0), (8.0, 16.0), 1.0, &overlay);
    let text = serde_json::to_string(&super::confirmation_json(&overlay, &layout)).unwrap();
    for private in [
        "zq-",
        "printf",
        "201~",
        "bracketed",
        "escape sequence",
        "Automatic",
    ] {
        assert!(!text.contains(private), "{private} reaches ctl: {text}");
    }
}

#[test]
fn nothing_before_the_marker_can_push_it_out_of_the_preview() {
    // The lead-in is bounded in drawn columns, the unit the window is cut
    // in. A tab draws four columns and a direction or 8-bit control six, so
    // a lead counted in characters could fill the window and cut the marker
    // off again. Every run length, each filler, both forms of the marker.
    for (marker, shown) in [("\x1b[201~", "ESC[201~"), ("\u{9b}201~", "CSI201~")] {
        for filler in [
            '\t', '\u{202e}', '\u{85}', '\u{2067}', 'x', '界', '\x01', ' ',
        ] {
            for run in (0..=200).chain([400, 5000]) {
                let fill: String = std::iter::repeat_n(filler, run).collect();
                // On the first line, and below a harmless one.
                for text in [
                    format!("{fill}{marker}; rm -rf ~"),
                    format!("echo ok\n{fill}{marker}; rm -rf ~\n"),
                    format!("a\nb\nc\nd\n{fill}{marker}"),
                ] {
                    // The dialog's own anchor: the most serious finding.
                    let anchor = crate::paste_safety::findings(&text)[0];
                    assert!(matches!(
                        anchor.kind,
                        crate::paste_safety::Kind::EndMarker
                            | crate::paste_safety::Kind::EndMarkerC1
                    ));
                    let preview = super::paste_preview(
                        &text,
                        Some(anchor.first),
                        &kettle_i18n::Translator::default(),
                    );
                    assert!(
                        preview.text.contains(shown),
                        "{run} x {filler:?} hides {shown}: {:?}",
                        preview.text
                    );
                    // The bounds still hold: 160 columns and the two
                    // ellipses, three lines.
                    assert!(preview.text.chars().count() <= 162, "{run} x {filler:?}");
                    assert!(preview.text.matches('\n').count() <= 2);
                    // The marker's introducer is one of the chips.
                    let chip = &shown[..3];
                    assert!(
                        preview
                            .tokens
                            .iter()
                            .any(|range| &preview.text[range.clone()] == chip
                                && preview.text[range.end..].starts_with(&shown[3..])),
                        "{run} x {filler:?}: no chip for {chip}"
                    );
                }
            }
        }
    }
    // The lead is forty columns, not forty characters: ten tabs, or six
    // direction overrides, and no more.
    let marker = "\x1b[201~";
    let tabs = preview_of(&format!("{}{marker}", "\t".repeat(300))).text;
    assert_eq!(tabs, format!("…{}ESC[201~", " ".repeat(40)));
    let bidi = preview_of(&format!("{}{marker}", "\u{202e}".repeat(300)));
    assert_eq!(bidi.text, format!("…{}ESC[201~", "U+202E".repeat(6)));
    assert_eq!(bidi.tokens.len(), 7);
}

#[test]
fn findings_and_the_raw_button_say_what_happens_in_this_mode() {
    use kettle_config::PasteProtection::{Always, Auto};
    use kettle_i18n::{Language, Text as T, Translator};
    let ask = |policy, unbracketed, delayed_suspend| super::paste::PasteAsk {
        delayed_suspend,
        ..paste_ask(policy, unbracketed)
    };
    let question = |text: &str, ask| {
        super::text_paste_confirmation(
            None,
            text,
            BroadcastScope::Off,
            vec![],
            ask,
            Translator::default(),
        )
    };
    let rows = |dialog: &super::ConfirmDialogState| {
        dialog
            .paste
            .as_ref()
            .unwrap()
            .detail
            .findings
            .iter()
            .map(|row| (row.token.clone(), row.meaning.clone(), row.removed))
            .collect::<Vec<_>>()
    };
    let raw_is_red = |dialog: &super::ConfirmDialogState| {
        let overlay = super::confirm_dialog_overlay(dialog, &Translator::default());
        overlay.buttons.last().unwrap().destructive
    };
    let row = |token: &str, meaning: &str, removed: bool| {
        (token.to_string(), meaning.to_string(), removed)
    };
    let text = "echo a\x1b[31m\x03\x1a\x1c\necho b\n";

    // Every recipient has bracketed paste on (the Always policy asked): the
    // encoder drops ESC and the three signal bytes, each row says so, and
    // the raw Paste is not the warning colour, since nothing it sends is a
    // control. The sanitised paste is still the recommended one.
    let bracketed = question(text, ask(Always, false, false));
    assert_eq!(
        rows(&bracketed),
        [
            row("^C", "interrupt (Ctrl+C); Kettle removes it", true),
            row("^Z", "suspend (Ctrl+Z); Kettle removes it", true),
            row("^\\", "quit (Ctrl+\\); Kettle removes it", true),
            row(
                "ESC",
                "read as the Esc key or the start of a key sequence; Kettle removes it",
                true
            ),
        ]
    );
    assert!(!raw_is_red(&bracketed));
    assert_eq!(bracketed.buttons.len(), 4);
    assert!(matches!(
        bracketed.buttons[bracketed.paste.as_ref().unwrap().recommended],
        super::ConfirmButton::PasteWithoutControls { .. }
    ));

    // A recipient without bracketed paste gets every one of them: the rows
    // are plain and the raw Paste is red. One such recipient under broadcast
    // is enough, which is what `unbracketed` means.
    let open = question(text, ask(Auto, true, false));
    assert_eq!(
        rows(&open),
        [
            row("^C", "interrupt (Ctrl+C)", false),
            row("^Z", "suspend (Ctrl+Z)", false),
            row("^\\", "quit (Ctrl+\\)", false),
            row(
                "ESC",
                "read as the Esc key or the start of a key sequence",
                false
            ),
        ]
    );
    assert!(raw_is_red(&open));

    // A control the encoder keeps in a bracketed paste stays a live row and
    // keeps the raw Paste red.
    let bell = question("a\x07\x1b\nb", ask(Always, false, false));
    assert_eq!(
        rows(&bell),
        [
            row(
                "ESC",
                "read as the Esc key or the start of a key sequence; Kettle removes it",
                true
            ),
            row("BEL", "bell", false),
        ]
    );
    assert!(raw_is_red(&bell));

    // The end marker is removed in every mode. Text that carries one was
    // written to break out of a paste, so its raw choice stays red even
    // when nothing else in it is a control.
    for unbracketed in [false, true] {
        let marked = question("x\x1b[201~y", ask(Auto, unbracketed, false));
        assert_eq!(
            rows(&marked),
            [row(
                "ESC[201~",
                "ends bracketed paste; Kettle removes it",
                true
            )]
        );
        assert!(raw_is_red(&marked));
    }

    // 0x19 is dropped only where it is the delayed-suspend byte.
    for delayed_suspend in [false, true] {
        let dialog = question("a\x19\nb", ask(Always, false, delayed_suspend));
        assert_eq!(rows(&dialog)[0].2, delayed_suspend);
        assert_eq!(raw_is_red(&dialog), !delayed_suspend);
    }

    // The rows agree with the encoder byte for byte: a kind is marked
    // removed exactly when a bracketed paste of it arrives without it.
    for byte in (0u8..0x20).chain([0x7f]) {
        if matches!(byte, b'\t' | b'\n' | b'\r') {
            continue;
        }
        for delayed_suspend in [false, true] {
            let text = format!("a{}\nb", char::from(byte));
            let dialog = question(&text, ask(Always, false, delayed_suspend));
            let sent = crate::input::paste_payload_for(&text, true, delayed_suspend);
            let survives = sent[6..sent.len() - 6].contains(&byte);
            assert_eq!(rows(&dialog)[0].2, !survives, "byte {byte:#04x}");
            assert_eq!(raw_is_red(&dialog), survives, "byte {byte:#04x}");
        }
    }

    // The screen reader hears the same rows.
    let said = &bracketed.paste.as_ref().unwrap().detail.description;
    assert!(said.contains("^C: interrupt (Ctrl+C); Kettle removes it, once"));
    let es = super::text_paste_confirmation(
        None,
        text,
        BroadcastScope::Off,
        vec![],
        ask(Always, false, false),
        Translator::new(Language::Es),
    );
    let es_tr = Translator::new(Language::Es);
    assert_eq!(
        es.paste.as_ref().unwrap().detail.findings[0].meaning,
        es_tr.confirm_paste_finding_removed(es_tr.text(T::ConfirmPasteFindingInterrupt))
    );
}

#[test]
fn direction_formatting_is_a_finding_that_the_sanitised_paste_removes() {
    use kettle_i18n::{Language, Text as T, Translator};
    // An override that shows `rm -rf ~ # ...` as something harmless.
    let text = "echo hi \u{202e}~ fr- mr\u{202c}\n";
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let dialog = super::text_paste_confirmation(
            Some(7),
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Auto, true),
            tr,
        );
        let detail = &dialog.paste.as_ref().unwrap().detail;
        // The preview's chips, the findings and the count agree.
        let chips: Vec<_> = detail
            .preview_tokens
            .iter()
            .map(|range| &detail.preview[range.clone()])
            .collect();
        assert_eq!(chips, ["U+202E", "U+202C"]);
        let rows: Vec<_> = detail
            .findings
            .iter()
            .map(|row| (row.token.as_str(), row.meaning.as_str(), row.removed))
            .collect();
        let meaning = tr.text(T::ConfirmPasteFindingBidi);
        assert_eq!(
            rows,
            [("U+202C", meaning, false), ("U+202E", meaning, false)]
        );
        assert!(detail.summary.contains(&tr.confirm_paste_controls(2)));
        // So the sanitising choice is offered, recommended, and removes
        // them; the raw Paste keeps them and is red.
        assert_eq!(dialog.buttons.len(), 4);
        let sanitised = dialog.paste.as_ref().unwrap().recommended;
        assert_eq!(
            stop_paste(&dialog, sanitised).as_deref(),
            Some("echo hi ~ fr- mr\n")
        );
        assert_eq!(stop_paste(&dialog, 3).as_deref(), Some(text));
        let overlay = super::confirm_dialog_overlay(&dialog, &tr);
        assert!(overlay.buttons[3].destructive);
        // Inside a bracketed paste too: the encoder does not touch them.
        let bracketed = super::text_paste_confirmation(
            Some(7),
            text,
            BroadcastScope::Off,
            vec![],
            paste_ask(kettle_config::PasteProtection::Always, false),
            tr,
        );
        assert!(!bracketed.paste.as_ref().unwrap().detail.findings[0].removed);
    }
    // The policy itself is unchanged: direction formatting alone does not
    // make Kettle ask. It is reported when something else did.
    let modes = std::collections::HashMap::from([(1u64, false)]);
    assert!(!super::paste::text_paste_needs_confirmation(
        kettle_config::PasteProtection::Auto,
        "echo \u{202e}hi",
        &modes
    ));
}

#[test]
fn every_paste_button_projects_its_own_flag_and_sends_its_own_text() {
    // rustc 1.99 to 1.102 on AArch64 at opt-level 1, the level tests and
    // debug builds use, compiled the projection's `matches!` on the
    // `destructive` payload into a read of that byte for every variant: a
    // paste button's flag came back as its label's length. The variant
    // matches that choose what is pasted are checked here beside the flags,
    // for every button of both dialog shapes, as values and as raw bytes.
    for (text, kinds, flags, sends) in [
        (
            "one\r\ntwo\x1b[31mred\x1b[0m\x07",
            ["Cancel", "WithoutControls", "OneLine", "Confirm"].as_slice(),
            [false, false, false, true].as_slice(),
            [
                None,
                Some("one\r\ntwored"),
                Some("one two [31mred [0m "),
                Some("one\r\ntwo\x1b[31mred\x1b[0m\x07"),
            ]
            .as_slice(),
        ),
        (
            "one\ntwo",
            ["Cancel", "Confirm", "OneLine"].as_slice(),
            [false, false, false].as_slice(),
            [None, Some("one\ntwo"), Some("one two")].as_slice(),
        ),
    ] {
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            let dialog = super::text_paste_confirmation(
                Some(7),
                text,
                BroadcastScope::Off,
                vec![],
                paste_ask(kettle_config::PasteProtection::Auto, true),
                tr,
            );
            let names: Vec<_> = dialog
                .buttons
                .iter()
                .map(|button| match button {
                    super::ConfirmButton::Cancel => "Cancel",
                    super::ConfirmButton::PasteWithoutControls { .. } => "WithoutControls",
                    super::ConfirmButton::PasteOneLine { .. } => "OneLine",
                    super::ConfirmButton::Confirm { .. } => "Confirm",
                })
                .collect();
            assert_eq!(names, kinds);
            let overlay = super::confirm_dialog_overlay(&dialog, &tr);
            // As bools and as bytes: an invalid bool prints as `true` or
            // `false` and still compares unequal to both.
            let projected: Vec<bool> = overlay.buttons.iter().map(|b| b.destructive).collect();
            assert_eq!(projected, flags);
            let bytes: Vec<u8> = projected.iter().map(|&flag| u8::from(flag)).collect();
            assert_eq!(
                bytes,
                flags.iter().map(|&flag| u8::from(flag)).collect::<Vec<_>>()
            );
            for button in &overlay.buttons {
                assert!(u8::from(button.neutral) <= 1 && u8::from(button.destructive) <= 1);
            }
            assert_eq!(
                dialog
                    .buttons
                    .iter()
                    .map(|b| b.destructive())
                    .collect::<Vec<_>>(),
                flags
            );
            // What each button sends, through the same matches the handler
            // uses.
            for (index, expected) in sends.iter().enumerate() {
                assert_eq!(
                    stop_paste(&dialog, index).as_deref(),
                    *expected,
                    "button {index} ({})",
                    names[index]
                );
            }
            assert_eq!(stop_paste(&dialog, dialog.buttons.len()), None);
        }
    }
}

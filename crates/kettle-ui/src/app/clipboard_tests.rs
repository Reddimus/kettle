#[test]
fn osc52_selection_uses_the_platform_selection_channel() {
    assert_eq!(
        osc52_clipboard_channel(ClipboardType::Clipboard),
        Osc52ClipboardChannel::Clipboard
    );

    #[cfg(target_os = "linux")]
    assert_eq!(
        osc52_clipboard_channel(ClipboardType::Selection),
        Osc52ClipboardChannel::Primary
    );
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        osc52_clipboard_channel(ClipboardType::Selection),
        Osc52ClipboardChannel::Clipboard
    );
}

#[test]
fn osc52_reads_answer_a_then_b_before_a_drain() {
    use kettle_core::{TermEvent, event::EventListener};
    let (tx, rx) = crossbeam_channel::bounded(1024);
    let proxy = kettle_core::EventProxy::new(tx, Arc::new(|| {}));
    for (text, encoded) in [("A", "QQ=="), ("B", "Qg==")] {
        proxy.send_event(TermEvent::ClipboardStore(
            ClipboardType::Clipboard,
            text.into(),
        ));
        proxy.send_event(TermEvent::ClipboardLoad(
            ClipboardType::Clipboard,
            Arc::new(move |text| {
                assert_eq!(
                    text,
                    if encoded == "QQ==" { "A" } else { "B" },
                    "OSC 52 read observed a later write"
                );
                format!("\x1b]52;c;{encoded}\x07")
            }),
        ));
    }
    struct Clipboard(String);
    impl super::Osc52Clipboard for Clipboard {
        fn store(&mut self, _: ClipboardType, text: String) {
            self.0 = text;
        }
        fn load(&mut self, _: ClipboardType) -> String {
            self.0.clone()
        }
    }
    let mut clipboard = Clipboard(String::new());
    let mut replies = Vec::new();
    for event in proxy
        .take_clipboard_events()
        .into_iter()
        .chain(rx.try_iter())
    {
        if let Some(reply) = super::apply_osc52_event(&mut clipboard, event) {
            replies.push(reply);
        }
    }
    assert_eq!(replies, ["\x1b]52;c;QQ==\x07", "\x1b]52;c;Qg==\x07"]);
}

#[test]
fn osc52_copy_scrubs_complete_cluster_before_its_byte_limit() {
    let marker: String = std::iter::once('\u{10eeee}')
        .chain(std::iter::repeat_n('\u{0305}', 8))
        .collect();
    assert_eq!(
        super::prepare_osc52_copy(format!("AAAAAA{marker}Z"), 16).unwrap(),
        "AAAAAA Z"
    );
    assert_eq!(super::prepare_osc52_copy("é界tail".into(), 4).unwrap(), "é");
    assert_eq!(
        super::prepare_osc52_copy(format!("A{marker}B"), 0).unwrap(),
        ""
    );
}

#[test]
fn osc52_da1_copy_capability_combines_policy_and_clipboard_availability() {
    use kettle_config::Osc52;

    let available = super::osc52_copy_is_available;
    assert!(!available(Osc52::Off, true));
    assert!(!available(Osc52::Paste, true));
    assert!(!available(Osc52::Copy, false));
    assert!(!available(Osc52::Both, false));
    assert!(available(Osc52::Copy, true));
    assert!(available(Osc52::Both, true));
}

#[test]
fn clamp_osc52_bounds_and_keeps_char_boundary() {
    use super::clamp_osc52;
    assert_eq!(clamp_osc52("hello", 1024), "hello"); // under cap → as-is
    assert_eq!(clamp_osc52("abcdef", 3), "abc"); // truncated
    // Never splits a multibyte char: "é" is 2 bytes; cap 1 → empty.
    assert_eq!(clamp_osc52("é", 1), "");
    assert_eq!(clamp_osc52("aé", 2), "a");
    assert_eq!(clamp_osc52("", 0), "");
    // Result always within the byte cap.
    let big = "x".repeat(5000);
    assert!(clamp_osc52(&big, 1000).len() <= 1000);
}

#[cfg(unix)]
#[test]
fn osc52_wire_reads_answer_a_then_b_before_a_drain() {
    let (mux, id, _) = title_test_split_mux();
    assert!(mux.panes[&id].term.apply_local_output(
        b"\x1b]52;c;QQ==\x07\x1b]52;c;?\x07\x1b]52;c;Qg==\x07\x1b]52;c;?\x07OSC52-DONE"
    ));
    assert!(wait_for_pane_screen(&mux, id, |text, _| text.contains("OSC52-DONE")));
    struct Clipboard(String);
    impl super::Osc52Clipboard for Clipboard {
        fn store(&mut self, _: ClipboardType, text: String) {
            self.0 = text;
        }
        fn load(&mut self, _: ClipboardType) -> String {
            self.0.clone()
        }
    }
    let mut clipboard = Clipboard(String::new());
    let replies: Vec<_> = mux.panes[&id]
        .term
        .take_clipboard_events()
        .into_iter()
        .filter_map(|event| super::apply_osc52_event(&mut clipboard, event))
        .collect();
    mux.kill_children();
    assert_eq!(
        replies,
        ["\x1b]52;c;QQ==\x07", "\x1b]52;c;Qg==\x07"],
        "parsed OSC 52 reads must answer A then B"
    );
}

#[test]
fn bounded_osc52_errors_reply_with_an_empty_payload() {
    struct Clipboard(Option<arboard::Error>);
    impl super::Osc52Clipboard for Clipboard {
        fn store(&mut self, _: ClipboardType, _: String) {
            unreachable!()
        }
        fn load(&mut self, _: ClipboardType) -> String {
            super::clipboard::osc52_read_result(Err(self.0.take().unwrap()))
        }
    }
    for error in [arboard::Error::LimitExceeded, arboard::Error::Timeout] {
        let mut clipboard = Clipboard(Some(error));
        let reply = super::apply_osc52_event(
            &mut clipboard,
            kettle_core::TermEvent::ClipboardLoad(
                ClipboardType::Clipboard,
                Arc::new(|text| {
                    assert!(
                        text.is_empty(),
                        "clipboard budget failure must produce an empty OSC 52 payload"
                    );
                    format!("\x1b]52;c;{text}\x07")
                }),
            ),
        );
        assert_eq!(reply.as_deref(), Some("\x1b]52;c;\x07"));
    }
}

#[test]
fn osc52_notice_throttle_is_per_pane_and_counts_unicode_characters() {
    use std::time::{Duration, Instant};
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let now = Instant::now();
        let mut first = crate::mux::Osc52NoticeState::default();
        let mut second = crate::mux::Osc52NoticeState::default();
        let prepared = super::prepare_osc52_copy("é界".into(), 100).unwrap();
        first.copied(now, true, prepared.chars().count(), || None, &tr);
        assert_eq!(first.message, Some(tr.osc52_copied(2)));
        first.message = None;
        first.copied(now + Duration::from_secs(4), true, 7, || None, &tr);
        assert_eq!(first.message, None);
        second.copied(now + Duration::from_secs(4), true, 7, || None, &tr);
        assert_eq!(second.message, Some(tr.osc52_copied(7)));
        first.copied(now + Duration::from_secs(5), true, 1, || None, &tr);
        assert_eq!(first.message, Some(tr.osc52_copied(1)));
        first.message = None;
        first.copied(now + Duration::from_secs(10), false, 5, || None, &tr);
        assert_eq!(first.message, None);
        first.copied(now + Duration::from_secs(10), true, 5, || None, &tr);
        assert_eq!(first.message, Some(tr.osc52_copied(5)));
    }
}

#[test]
fn osc52_policy_notices_are_once_per_direction_per_pane_and_accessible() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let mut state = crate::mux::Osc52NoticeState::default();
        for read in [true, false] {
            state.denied(read, || None, &tr);
            let message = state.message.take().unwrap();
            assert!(message.contains(if read {
                "osc52 = paste"
            } else {
                "osc52 = copy"
            }));
            assert!(!message.contains("secret"));
            state.denied(read, || None, &tr);
            assert_eq!(state.message, None);
            let toast = kettle_render::UpdateToastOverlay {
                geometry: kettle_render::persistent_notice_geometry(
                    (0.0, 0.0, 800.0, 600.0),
                    (8.0, 16.0),
                    3,
                    0,
                    10,
                )
                .unwrap(),
                message: message.clone(),
                details: String::new(),
                close: tr.text(kettle_i18n::Text::NoticeDismiss).into(),
                opacity: 1.0,
                focused: None,
            };
            let nodes = super::persistent_notice_nodes(Some(7), &toast);
            let node = &nodes
                .iter()
                .find(|(_, n)| n.role() == accesskit::Role::Status)
                .unwrap()
                .1;
            assert_eq!(node.label(), Some(message.as_str()));
            assert_eq!(node.live(), Some(accesskit::Live::Polite));
            assert_eq!(
                nodes
                    .iter()
                    .filter(|(_, n)| n.role() == accesskit::Role::Button)
                    .count(),
                1
            );
        }
        let mut other_pane = crate::mux::Osc52NoticeState::default();
        other_pane.denied(true, || None, &tr);
        assert!(other_pane.message.is_some());
    }
}

#[test]
fn osc52_policy_gates_backend_effects_and_only_successful_writes_notify() {
    use super::{Osc52Backend, Osc52Clipboard, PaneClipboard};
    use kettle_config::Osc52;
    use kettle_core::TermEvent;
    #[derive(Default)]
    struct Backend {
        text: String,
        sets: usize,
        gets: usize,
        fail: bool,
    }
    impl Osc52Backend for Backend {
        fn set(&mut self, _: ClipboardType, text: String) -> Result<(), arboard::Error> {
            self.sets += 1;
            if self.fail {
                return Err(arboard::Error::ContentNotAvailable);
            }
            self.text = text;
            Ok(())
        }
        fn get(&mut self, _: ClipboardType) -> Result<String, arboard::Error> {
            self.gets += 1;
            Ok(self.text.clone())
        }
    }
    let tr = kettle_i18n::Translator::default();
    for policy in [Osc52::Off, Osc52::Copy, Osc52::Paste, Osc52::Both] {
        let mut backend = Backend {
            text: "private".into(),
            ..Default::default()
        };
        let mut notices = crate::mux::Osc52NoticeState::default();
        {
            let mut clipboard = PaneClipboard {
                clipboard: Some(&mut backend),
                policy,
                notify: true,
                notices: &mut notices,
                tr: &tr,
                program: &|| None,
            };
            let reply = super::apply_osc52_event(
                &mut clipboard,
                TermEvent::ClipboardLoad(
                    ClipboardType::Clipboard,
                    Arc::new(|s| format!("reply:{s}")),
                ),
            );
            assert_eq!(
                reply.as_deref(),
                Some(if policy.can_paste() {
                    "reply:private"
                } else {
                    "reply:"
                })
            );
            clipboard.store(ClipboardType::Clipboard, "é界".into());
        }
        assert_eq!(backend.sets, usize::from(policy.can_copy()));
        assert_eq!(backend.gets, usize::from(policy.can_paste()));
        assert_eq!(notices.denied_read, !policy.can_paste());
        assert_eq!(notices.denied_write, !policy.can_copy());
        if policy.can_copy() {
            assert_eq!(notices.message, Some(tr.osc52_copied(2)));
        } else {
            assert!(notices.message.as_ref().unwrap().contains("osc52 = copy"));
        }
    }
    for (available, fail, notify) in [
        (false, false, true),
        (true, true, true),
        (true, false, false),
    ] {
        let mut backend = Backend {
            fail,
            ..Default::default()
        };
        let mut notices = crate::mux::Osc52NoticeState::default();
        let mut clipboard = PaneClipboard {
            clipboard: available.then_some(&mut backend),
            policy: Osc52::Copy,
            notify,
            notices: &mut notices,
            tr: &tr,
            program: &|| None,
        };
        clipboard.store(ClipboardType::Clipboard, "test".into());
        assert_eq!(notices.message, None);
        assert_eq!(notices.last_copy, None);
        assert_eq!(backend.sets, usize::from(available));
    }
}

#[test]
fn osc52_policy_notice_geometry_has_room_for_the_shared_word_wrapping() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        for message in [
            tr.text(kettle_i18n::Text::Osc52DeniedRead).to_string(),
            tr.text(kettle_i18n::Text::Osc52DeniedWrite).to_string(),
            tr.osc52_copied(123456),
            // The longest program label a notice can carry: 32 columns.
            tr.osc52_denied_read_by(&"W".repeat(32)),
            tr.osc52_denied_write_by(&"W".repeat(32)),
            tr.osc52_copied_by(&"W".repeat(32), 123456),
        ] {
            for width in [240.0, 400.0, 800.0] {
                let cell = (8.0, 16.0);
                let columns = ((width - 12.0_f32) / cell.0).floor() as usize - 1;
                let rows = kettle_render::toast_text_rows(&message, columns).unwrap();
                let geometry = kettle_render::persistent_notice_geometry(
                    (0.0, 0.0, width, 600.0),
                    cell,
                    rows,
                    0,
                    10,
                )
                .unwrap();
                assert!(geometry.message.3 >= rows as f32 * cell.1);
                assert_eq!(
                    geometry.hit_test(
                        geometry.close.0 + geometry.close.2 / 2.0,
                        geometry.close.1 + geometry.close.3 / 2.0
                    ),
                    Some(kettle_render::UpdateToastHit::Close)
                );
            }
        }
    }
}

#[test]
fn osc52_notices_name_the_program_when_it_is_known() {
    use std::time::{Duration, Instant};
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let now = Instant::now();
        let mut state = crate::mux::Osc52NoticeState::default();
        state.copied(now, true, 42, || Some("vim".into()), &tr);
        assert_eq!(state.message, Some(tr.osc52_copied_by("vim", 42)));
        // Unknown keeps the wording that names nobody.
        state.copied(now + Duration::from_secs(5), true, 1, || None, &tr);
        assert_eq!(state.message, Some(tr.osc52_copied(1)));
        for read in [true, false] {
            let mut named = crate::mux::Osc52NoticeState::default();
            named.denied(read, || Some("ssh".into()), &tr);
            let message = named.message.take().unwrap();
            assert_eq!(
                message,
                if read {
                    tr.osc52_denied_read_by("ssh")
                } else {
                    tr.osc52_denied_write_by("ssh")
                }
            );
            assert!(message.contains("ssh"));
            // The value to set is still named.
            assert!(message.contains(if read {
                "osc52 = paste"
            } else {
                "osc52 = copy"
            }));
            let mut unnamed = crate::mux::Osc52NoticeState::default();
            unnamed.denied(read, || None, &tr);
            assert_eq!(
                unnamed.message.as_deref(),
                Some(tr.text(if read {
                    kettle_i18n::Text::Osc52DeniedRead
                } else {
                    kettle_i18n::Text::Osc52DeniedWrite
                }))
            );
        }
    }
    let en = kettle_i18n::Translator::default();
    assert_eq!(
        en.osc52_copied_by("vim", 42),
        "vim copied 42 characters to the clipboard"
    );
}

#[test]
fn osc52_notices_look_the_program_up_only_when_one_is_shown() {
    use std::cell::Cell;
    use std::time::{Duration, Instant};
    let tr = kettle_i18n::Translator::default();
    let lookups = Cell::new(0);
    let program = || {
        lookups.set(lookups.get() + 1);
        Some("vim".to_string())
    };
    let now = Instant::now();
    let mut state = crate::mux::Osc52NoticeState::default();
    // Notices off, then a burst inside the 5 s window: one lookup in all.
    state.copied(now, false, 1, program, &tr);
    assert_eq!(lookups.get(), 0);
    for step in 0..100 {
        state.copied(
            now + Duration::from_millis(step * 10),
            true,
            1,
            program,
            &tr,
        );
    }
    assert_eq!(lookups.get(), 1);
    // A denial shows once per direction per pane, so it looks up once.
    for _ in 0..100 {
        state.denied(true, program, &tr);
        state.denied(false, program, &tr);
    }
    assert_eq!(lookups.get(), 3);
}

#[test]
fn osc52_notices_show_a_sanitised_bounded_name_and_never_the_arguments() {
    use super::{Osc52Backend, Osc52Clipboard, PaneClipboard};
    use kettle_config::Osc52;
    struct Backend;
    impl Osc52Backend for Backend {
        fn set(&mut self, _: ClipboardType, _: String) -> Result<(), arboard::Error> {
            Ok(())
        }
        fn get(&mut self, _: ClipboardType) -> Result<String, arboard::Error> {
            Ok(String::new())
        }
    }
    let tr = kettle_i18n::Translator::default();
    // What a hostile program can put in argv[0], and arguments that may hold
    // a secret: the label is the close question's, name only.
    let long = format!("/tmp/{}", "B".repeat(500));
    for (argv, expected) in [
        (vec!["/usr/bin/vim", "--cmd", "secret-token"], Some("vim")),
        (vec!["ssh: user@host [mux]", "-p", "2222"], Some("ssh")),
        // Invisible characters go; a control or direction override ends it.
        (
            vec![
                "/tmp/ev\u{200b}il\u{202e}gpj\x1b[31m\x07",
                "--password=hunter2",
            ],
            Some("evil"),
        ),
        (vec!["-zsh"], None),
        (vec!["\u{200b}\u{202e}"], None),
        (vec![long.as_str(), "--password=hunter2"], None),
    ] {
        let argv: Vec<String> = argv.into_iter().map(String::from).collect();
        let label = super::busy_program_label(7, |_| Some(argv.clone()), "/bin/zsh\n", None);
        if argv[0] == long {
            // Bounded: 32 columns at most, ending in an ellipsis.
            let label = label.clone().expect("a visible name");
            assert!(crate::text_editor::display_width(&label) <= 32, "{label:?}");
            assert!(label.starts_with("BBBB") && label.ends_with('…'));
        } else {
            assert_eq!(label.as_deref(), expected, "{argv:?}");
        }
        let mut notices = crate::mux::Osc52NoticeState::default();
        let program = || label.clone();
        let mut clipboard = PaneClipboard {
            clipboard: Some(&mut Backend),
            policy: Osc52::Copy,
            notify: true,
            notices: &mut notices,
            tr: &tr,
            program: &program,
        };
        clipboard.store(ClipboardType::Clipboard, "copied text".into());
        let copied = notices.message.take().expect("a copy notice");
        let _ = clipboard_load_denied(&mut notices, &tr, &program);
        let denied = notices.message.take().expect("a denial notice");
        for message in [copied, denied] {
            assert!(!message.contains("secret") && !message.contains("hunter2"));
            assert!(!message.contains("copied text"));
            assert!(!message.chars().any(|c| c.is_control()), "{message:?}");
            assert!(message.chars().count() < 200, "{message:?}");
            match &label {
                Some(label) => assert!(message.starts_with(label.as_str()), "{message}"),
                None => assert!(message.starts_with("A program "), "{message}"),
            }
        }
    }
}

/// A denied OSC 52 read through the pane clipboard, as the output path makes it.
fn clipboard_load_denied(
    notices: &mut crate::mux::Osc52NoticeState,
    tr: &kettle_i18n::Translator,
    program: &dyn Fn() -> Option<String>,
) -> String {
    use super::{Osc52Backend, Osc52Clipboard, PaneClipboard};
    struct Never;
    impl Osc52Backend for Never {
        fn set(&mut self, _: ClipboardType, _: String) -> Result<(), arboard::Error> {
            unreachable!("a denied read never writes")
        }
        fn get(&mut self, _: ClipboardType) -> Result<String, arboard::Error> {
            unreachable!("a denied read never reaches the clipboard")
        }
    }
    PaneClipboard {
        clipboard: Some(&mut Never),
        policy: kettle_config::Osc52::Copy,
        notify: true,
        notices,
        tr,
        program,
    }
    .load(ClipboardType::Clipboard)
}

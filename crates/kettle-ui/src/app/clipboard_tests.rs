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


/// The edge case that motivated the allow-list: nothing kettle sends for a
/// composing chord may gain an ESC prefix or an xterm `CSI 27` form, because
/// a client that negotiated neither prints those as literal characters.
#[test]
fn option_composition_keys_never_gain_an_escape_prefix() {
    use kettle_config::MacosOptionAsAlt;
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let raw = ModifiersState::ALT;
    let masked = macos_effective_modifiers(raw, MacosOptionAsAlt::None, true, false);
    assert!(!masked.alt_key(), "the shipped default masks bare Option");

    let encode = |key: &Key, text: Option<&str>| {
        let mods = macos_pty_modifiers(masked, raw, key);
        crate::input::encode(key, text, mods, TermMode::empty())
            .unwrap_or_else(|| panic!("no encoding for {key:?}"))
    };

    // Composing keys keep their unmodified bytes.
    assert_eq!(encode(&Key::Named(NamedKey::Space), None), vec![0x20]);
    assert_eq!(encode(&Key::Named(NamedKey::Tab), None), b"\t".to_vec());
    assert_eq!(encode(&Key::Named(NamedKey::Escape), None), vec![0x1b]);
    // A glyph macOS composed arrives as itself, not as ESC + glyph.
    assert_eq!(
        encode(&Key::Character("´".into()), Some("´")),
        "´".as_bytes().to_vec()
    );

    // And the keys the fix is for do change. Return composes nothing,
    // so Option+Return is Alt+Return: ESC CR, a newline in zsh, Codex and
    // Claude Code, where a masked Option would submit the line.
    assert_eq!(
        encode(&Key::Named(NamedKey::Enter), None),
        b"\x1b\r".to_vec()
    );
    assert_eq!(
        encode(&Key::Named(NamedKey::Backspace), None),
        vec![0x1b, 0x7f],
        "Alt+Backspace is readline's backward-kill-word"
    );
    assert_eq!(
        encode(&Key::Named(NamedKey::Delete), None),
        b"\x1b[3;3~".to_vec()
    );
    assert_eq!(
        encode(&Key::Named(NamedKey::ArrowLeft), None),
        b"\x1b[1;3D".to_vec()
    );
}

#[test]
fn empty_ime_preedit_normalizes_to_stable_absence() {
    assert_eq!(super::normalized_ime_preedit(String::new(), None), None);
    assert_eq!(
        super::normalized_ime_preedit("compose".into(), Some((1, 4))),
        Some(("compose".into(), Some((1, 4))))
    );
    assert_eq!(super::ime_preedit_focus("compose", Some((1, 4))), 4);
    assert_eq!(super::ime_preedit_focus("界x", Some((0, 3))), 3);
    // An invalid byte offset and a caret inside an extended grapheme both snap backward to a
    // safe visible boundary.
    assert_eq!(super::ime_preedit_focus("界x", Some((0, 2))), 0);
    assert_eq!(super::ime_preedit_focus("a\u{301}x", Some((0, 1))), 0);
}

/// A composition cancelled where it began leaves no owner behind, so the
/// next composition, in search, keeps its Commit. And one cut short by a
/// focus change without a Commit stops guarding once focus moves on again,
/// so a plain Commit later is typed. Both once dropped the text.
#[test]
fn a_finished_composition_leaves_no_stale_ime_owner() {
    let mut ws = WindowState::new(1, Mux::new());
    assert!(super::update_ime_preedit(&mut ws, "ka".into(), None));
    super::update_ime_preedit(&mut ws, String::new(), None);
    assert!(ws.ime_preedit_owner.is_none(), "a cancel ends the session");
    ws.search.open = true;
    ws.ime_focus_generation += 1;
    assert!(super::update_ime_preedit(&mut ws, "ki".into(), None));
    assert!(
        super::take_ime_commit(&mut ws),
        "search's own Commit is typed"
    );

    let mut ws = WindowState::new(1, Mux::new());
    assert!(super::update_ime_preedit(&mut ws, "ku".into(), None));
    super::end_modal_composition(&mut ws);
    super::update_ime_preedit(&mut ws, String::new(), None);
    ws.ime_focus_generation += 1;
    assert!(
        super::take_ime_commit(&mut ws),
        "a later plain Commit is typed"
    );
}

#[test]
fn empty_preedit_preserves_ownership_until_commit() {
    for clear_display in [false, true] {
        let mut ws = WindowState::new(1, Mux::new());
        assert!(super::update_ime_preedit(&mut ws, "compose".into(), None));
        let owner = ws.ime_preedit_owner;
        if clear_display {
            super::end_modal_composition(&mut ws);
            assert!(ws.ime_preedit.is_none());
        } else {
            ws.ime_focus_generation += 1;
        }
        ws.palette_input = Some((Default::default(), 0));
        super::update_ime_preedit(&mut ws, String::new(), None);
        let kept = |ws: &WindowState| ws.ime_preedit_owner.map(|s| (s.owner, s.generation));
        assert_eq!(kept(&ws), owner.map(|s| (s.owner, s.generation)));
        assert!(!super::take_ime_commit(&mut ws));
        assert!(ws.ime_preedit_owner.is_none());

        assert!(super::update_ime_preedit(&mut ws, "new".into(), None));
        super::update_ime_preedit(&mut ws, String::new(), None);
        assert!(super::take_ime_commit(&mut ws));
    }
}

#[test]
fn ime_composition_cannot_follow_focus_to_another_pane() {
    for late_preedit in [false, true] {
        let (mux, first, second) = title_test_split_mux();
        let mut ws = WindowState::new(1, mux);
        assert!(ws.mux.focus_pane(first));
        super::update_ime_preedit(&mut ws, "compose".into(), None);
        assert!(ws.mux.focus_pane(second));
        if late_preedit {
            super::update_ime_preedit(&mut ws, "composed".into(), None);
        }
        assert!(
            !super::take_ime_commit(&mut ws),
            "composition must not commit into a different pane"
        );
        super::update_ime_preedit(&mut ws, "new".into(), None);
        assert!(super::take_ime_commit(&mut ws));

        super::update_ime_preedit(&mut ws, "old".into(), None);
        assert!(ws.mux.focus_pane(first));
        super::end_modal_composition(&mut ws);
        super::update_ime_preedit(&mut ws, String::new(), None);
        assert!(ws.mux.focus_pane(second));
        super::end_modal_composition(&mut ws);
        assert!(
            !super::take_ime_commit(&mut ws),
            "returning to the pane must not revive an invalidated composition"
        );
    }
    let source = production_source();
    let focus = source
        .split("fn note_focus_change(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(focus.contains("end_modal_composition(ws);"));
}

#[test]
fn cancelled_pane_composition_allows_new_preedit_after_focus_moves() {
    let (mux, first, second) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    assert!(ws.mux.focus_pane(first));
    super::update_ime_preedit(&mut ws, "old".into(), None);
    assert!(ws.mux.focus_pane(second));
    super::end_modal_composition(&mut ws);
    super::update_ime_preedit(&mut ws, String::new(), None);
    super::update_ime_preedit(&mut ws, "new".into(), None);
    assert!(
        super::take_ime_commit(&mut ws),
        "a new composition after cancellation must be accepted"
    );
}

#[test]
fn cancelled_pane_composition_allows_fresh_commit_only_input() {
    let (mux, first, second) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    assert!(ws.mux.focus_pane(first));
    super::update_ime_preedit(&mut ws, "old".into(), None);
    assert!(ws.mux.focus_pane(second));
    super::end_modal_composition(&mut ws);
    // Cancellation ends the old preedit without sending a Commit.
    super::update_ime_preedit(&mut ws, String::new(), None);
    // Wayland sends an empty Preedit before even a commit-only input.
    super::update_ime_preedit(&mut ws, String::new(), None);
    assert!(
        super::take_ime_commit(&mut ws),
        "fresh commit-only input after cancellation must be accepted"
    );
    assert!(ws.ime_preedit_owner.is_none());
}

#[test]
fn empty_pane_preedit_still_rejects_late_commit_after_focus_moves() {
    let (mux, first, second) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    assert!(ws.mux.focus_pane(first));
    super::update_ime_preedit(&mut ws, "old".into(), None);
    assert!(ws.mux.focus_pane(second));
    super::end_modal_composition(&mut ws);
    super::update_ime_preedit(&mut ws, String::new(), None);
    assert!(
        !super::take_ime_commit(&mut ws),
        "an empty Preedit immediately followed by Commit still belongs to the old pane"
    );
    super::update_ime_preedit(&mut ws, String::new(), None);
    assert!(super::take_ime_commit(&mut ws));
}


#[test]
fn echo_bypass_timestamp_only_marks_windows_that_accepted_input() {
    let before = std::time::Instant::now();
    let input_time = before + std::time::Duration::from_millis(1);
    let mut accepted = Some(before);
    let mut rejected = Some(before);
    stamp_accepted_input(
        &mut accepted,
        PaneInputDelivery {
            result: PaneInputResult::ReadOnly,
            accepted: true,
            receipt_accepted: false,
            pending_accepted: false,
        },
        input_time,
    );
    stamp_accepted_input(
        &mut rejected,
        PaneInputDelivery {
            result: PaneInputResult::ReadOnly,
            accepted: false,
            receipt_accepted: false,
            pending_accepted: false,
        },
        input_time,
    );
    assert_eq!(accepted, Some(input_time));
    assert_eq!(rejected, Some(before));

    let source = production_source();
    for owner in ["fn broadcast_input(", "fn write_terminal_key_event("] {
        let body = source.split(owner).nth(1).expect("delivery helper exists");
        let end = body.find("\n    }\n").unwrap_or(body.len());
        assert!(
            body[..end].contains("stamp_accepted_input(&mut other.last_typed"),
            "{owner} must stamp every foreign window that accepted input"
        );
    }
}
#[test]
fn macos_option_modifier_is_masked_per_side_and_mode() {
    use kettle_config::MacosOptionAsAlt;
    use winit::keyboard::ModifiersState;

    let raw = ModifiersState::ALT | ModifiersState::SHIFT;
    let masked = ModifiersState::SHIFT;
    for (policy, left, right, expected) in [
        (MacosOptionAsAlt::None, true, false, masked),
        (MacosOptionAsAlt::None, false, true, masked),
        (MacosOptionAsAlt::Left, true, false, raw),
        (MacosOptionAsAlt::Left, false, true, masked),
        (MacosOptionAsAlt::Right, true, false, masked),
        (MacosOptionAsAlt::Right, false, true, raw),
        (MacosOptionAsAlt::Both, true, false, raw),
        (MacosOptionAsAlt::Both, false, true, raw),
    ] {
        assert_eq!(
            macos_effective_modifiers(raw, policy, left, right),
            expected,
            "policy={policy:?}, left={left}, right={right}"
        );
    }
}

/// The exemption list, both halves of it.
///
/// `macos-option-as-alt` decides whether Option composes text or acts as
/// Meta. That question only exists for keys macOS composes a character
/// from, and answering it for the rest is what made `⌥⌫` delete one
/// character. The negative half matters more than the positive one: adding
/// Enter, Space, Tab, Escape or any character key here is how kettle would
/// start printing escape text into a pane.
#[test]
fn only_keys_that_compose_no_text_treat_option_as_meta() {
    use winit::keyboard::{Key, NamedKey};

    for named in [
        NamedKey::Backspace,
        NamedKey::Delete,
        NamedKey::ArrowUp,
        NamedKey::ArrowDown,
        NamedKey::ArrowLeft,
        NamedKey::ArrowRight,
        NamedKey::Home,
        NamedKey::End,
        NamedKey::PageUp,
        NamedKey::PageDown,
        NamedKey::Insert,
        NamedKey::F1,
        NamedKey::F12,
        // Text "\r", but no composed character: Option+Return must be
        // Alt+Return, or it submits where a newline was asked for.
        NamedKey::Enter,
    ] {
        assert!(
            option_is_meta_for(&Key::Named(named)),
            "{named:?} composes no character, so Option must reach the encoder"
        );
    }

    // Option+Space composes a no-break space; Tab and Escape keep what
    // they send.
    for named in [NamedKey::Space, NamedKey::Tab, NamedKey::Escape] {
        assert!(
            !option_is_meta_for(&Key::Named(named)),
            "{named:?} produces text; restoring ALT would ESC-prefix it"
        );
    }
    for text in ["e", "a", "´", "8"] {
        assert!(
            !option_is_meta_for(&Key::Character(text.into())),
            "character keys are exactly what the policy protects ({text})"
        );
    }

    // The trap this list exists to avoid: winit reports the legacy C0
    // `\x08` from `NamedKey::Backspace.to_text()`, so the tempting
    // `to_text().is_none()` predicate excludes the very key at issue.
    assert_eq!(NamedKey::Backspace.to_text(), Some("\u{8}"));
}

/// Restoring Option at the encode seam, across every policy and both sides.
#[test]
fn option_reaches_the_encoder_for_exempt_keys_under_every_policy() {
    use kettle_config::MacosOptionAsAlt::{Both, Left, None as NoneAlt, Right};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let raw = ModifiersState::ALT;
    let backspace = Key::Named(NamedKey::Backspace);
    let letter = Key::Character("e".into());

    for (policy, left, right) in [
        (NoneAlt, true, false),
        (NoneAlt, false, true),
        (Left, false, true),
        (Right, true, false),
        (Both, true, false),
    ] {
        let masked = macos_effective_modifiers(raw, policy, left, right);
        // The whole bug, in one row: under the shipped default the mask
        // has already dropped ALT before the encoder ever runs.
        assert_eq!(
            macos_pty_modifiers(masked, raw, &backspace),
            ModifiersState::ALT,
            "Option must be Meta for Backspace under {policy:?} (left={left}, right={right})"
        );
        // …and must still not be, for a key macOS composes from.
        assert_eq!(
            macos_pty_modifiers(masked, raw, &letter),
            masked,
            "character keys follow the policy under {policy:?}"
        );
    }

    // No Option held: nothing to restore, whatever the key.
    let none = ModifiersState::empty();
    assert_eq!(macos_pty_modifiers(none, none, &backspace), none);
    // Other modifiers are carried through untouched.
    let shift = ModifiersState::SHIFT;
    assert_eq!(
        macos_pty_modifiers(shift, raw | ModifiersState::SHIFT, &backspace),
        shift | ModifiersState::ALT
    );
}

/// `backspace-binding` / `delete-binding` must replace only the
/// UNMODIFIED key.
///
/// Every case drives the real encoder and then the real remap, in the
/// order the key path uses them, so the expectations cannot drift from
/// what the terminal actually writes. `delete-binding` defaults to
/// `EscapeSequence`, so an unguarded Delete arm would rewrite *every*
/// modified Delete to the plain `CSI 3 ~`. `Ctrl+Delete` would then be
/// byte-identical to `Delete`, and no `<C-Del>` mapping could fire.
#[test]
fn a_modified_delete_keeps_its_modifier_through_the_binding_remap() {
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cfg = kettle_config::Config::default();
    let mode = TermMode::empty();
    let delete = Key::Named(NamedKey::Delete);
    let encode = |mods: ModifiersState| -> Vec<u8> {
        crate::input::encode_key_press(&delete, mods, mode).expect("Delete encodes")
    };

    // Precondition: the default really is the binding that rewrites, and
    // the encoder really does distinguish these. Without both, the
    // assertions below would hold for the wrong reason.
    assert_eq!(
        cfg.delete_binding,
        kettle_config::DeleteBinding::EscapeSequence,
        "this test is about the DEFAULT binding"
    );
    let plain = encode(ModifiersState::empty());
    assert_eq!(plain, b"\x1b[3~");

    for (label, mods) in [
        ("ctrl", ModifiersState::CONTROL),
        ("shift", ModifiersState::SHIFT),
        ("alt", ModifiersState::ALT),
        (
            "ctrl+shift",
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        ),
    ] {
        let encoded = encode(mods);
        assert_ne!(
            encoded, plain,
            "the encoder must distinguish {label}+Delete, or this test \
                 cannot detect the remap flattening it"
        );
        assert_eq!(
            apply_bs_del_binding(&cfg, &delete, encoded.clone()),
            encoded,
            "{label}+Delete must survive the binding remap unchanged"
        );
    }

    // The feature itself still works on the unmodified key.
    let ascii_del = kettle_config::Config {
        delete_binding: kettle_config::DeleteBinding::AsciiDel,
        ..kettle_config::Config::default()
    };
    assert_eq!(
        apply_bs_del_binding(&ascii_del, &delete, plain.clone()),
        vec![0x7f],
        "an unmodified Delete must still honour the binding"
    );
}

#[test]
fn only_modified_enter_requests_a_live_context_sample() {
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let enter = Key::Named(NamedKey::Enter);
    assert!(key_is_modified_enter(&enter, ModifiersState::SHIFT));
    assert!(key_is_modified_enter(&enter, ModifiersState::CONTROL));
    assert!(!key_is_modified_enter(&enter, ModifiersState::empty()));
    assert!(!key_is_modified_enter(
        &Key::Named(NamedKey::Tab),
        ModifiersState::SHIFT
    ));
}

/// Backspace's binding has the same contract, and the same trap one level
/// down. Its level-0 forms vary with control and alt, but a
/// `modifyOtherKeys` level-2 `Shift+Backspace` is a `CSI 27;2;8~` that a
/// bare `!control && !alt` guard would flatten to `0x7f`.
#[test]
fn a_modified_backspace_keeps_its_encoding_through_the_binding_remap() {
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cfg = kettle_config::Config {
        backspace_binding: kettle_config::BackspaceBinding::ControlH,
        ..kettle_config::Config::default()
    };
    let backspace = Key::Named(NamedKey::Backspace);
    let plain =
        crate::input::encode_key_press(&backspace, ModifiersState::empty(), TermMode::empty())
            .expect("Backspace encodes");
    assert_eq!(plain, vec![0x7f]);
    assert_eq!(
        apply_bs_del_binding(&cfg, &backspace, plain),
        vec![0x08],
        "an unmodified Backspace must honour `control-h`"
    );

    for (label, mods, mode) in [
        ("ctrl", ModifiersState::CONTROL, TermMode::empty()),
        ("alt", ModifiersState::ALT, TermMode::empty()),
        // Backspace opts out of level-1 encoding but opts in at level 2
        // for anything that is not bare Control, so this is the level
        // where Shift produces a distinct sequence.
        (
            "shift at modifyOtherKeys level 2",
            ModifiersState::SHIFT,
            TermMode::MODIFY_OTHER_KEYS_2,
        ),
    ] {
        let encoded =
            crate::input::encode_key_press(&backspace, mods, mode).expect("Backspace encodes");
        assert_ne!(
            encoded,
            vec![0x7f],
            "the encoder must distinguish {label}+Backspace, or this case \
                 cannot detect the remap flattening it"
        );
        assert_eq!(
            apply_bs_del_binding(&cfg, &backspace, encoded.clone()),
            encoded,
            "{label}+Backspace must survive the binding remap unchanged"
        );
    }
}

/// A key event queued behind a paste still being prepared keeps its press and
/// release on the same panes, as one sent at once does. A queued press once
/// recorded no recipients, so a release sent after the paste reached no pane,
/// and a queued release went to whatever had focus by then: either way a
/// Kitty-reporting program was left with a stuck key.
#[test]
fn queued_key_events_keep_press_and_release_on_the_same_panes() {
    let src = production_source();
    let body = src
        .split("fn defer_terminal_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("defer_terminal_input body");
    assert!(
        body.contains(".filter(|_| !prepare_input)")
            && body.contains("ws.terminal_key_recipients.remove(&event.physical_key)"),
        "a queued release takes the panes its press reached"
    );
    assert!(
        body.contains(".filter(|_| prepare_input)")
            && body.contains("result.is_some_and(|result| result.is_queued())")
            && body.contains(".entry(event.physical_key)"),
        "a press that was queued records the panes it will reach"
    );
}

/// Keep the shared owner resolver aligned with native keyboard dispatch.
#[test]
fn the_control_plane_modal_order_matches_the_real_key_handler() {
    let src = production_source();

    let resolver = src
        .split("fn top_modal(")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("top_modal present");
    let resolver_order: Vec<&str> = [
        ("ws.confirm_dialog.is_some()", "confirm"),
        ("ws.context_menu.is_some()", "context_menu"),
        ("ws.vi_mode.is_some()", "vi_mode"),
        ("ws.hint_state.is_some()", "hint"),
        ("ws.palette_input.is_some()", "palette"),
        ("ws.theme_picker_input.is_some()", "theme_picker"),
        // The compound condition, not just the text-edit half: dropping
        // the `settings_nav` guard would diverge from the real handler
        // while a substring probe stayed green.
        (
            "ws.settings_nav.is_some() && ws.settings_text_edit.is_some()",
            "settings_text",
        ),
        ("else if ws.settings_nav.is_some() {", "settings"),
        ("ws.layout_picker_input.is_some()", "layout_picker"),
        ("ws.ssh_input.is_some()", "ssh"),
        ("ws.editing_title.is_some()", "title_edit"),
        ("ws.search.open", "search"),
    ]
    .iter()
    .map(|(probe, name)| {
        assert!(resolver.contains(probe), "top_modal must test {probe}");
        (resolver.find(probe).expect("probe present"), *name)
    })
    .collect::<std::collections::BTreeMap<_, _>>()
    .into_values()
    .collect();

    // The same probes as they appear in the KeyboardInput arm, which runs
    // from the confirm dialog down to the terminal.
    let handler = src
        .split("let text = event.text.as_ref().map(|s| s.as_str());")
        .nth(1)
        .and_then(|rest| rest.split("fn ").next())
        .expect("KeyboardInput arm present");
    let handler_order: Vec<&str> = [
        ("if ws.confirm_dialog.is_some() {", "confirm"),
        ("if ws.context_menu.is_some() {", "context_menu"),
        ("if ws.vi_mode.is_some() {", "vi_mode"),
        ("if ws.hint_state.is_some() {", "hint"),
        ("if ws.palette_input.is_some() {", "palette"),
        ("if ws.theme_picker_input.is_some() {", "theme_picker"),
        (
            "if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {",
            "settings_text",
        ),
        ("if ws.settings_nav.is_some() {", "settings"),
        ("if ws.layout_picker_input.is_some() {", "layout_picker"),
        ("if ws.ssh_input.is_some() {", "ssh"),
        ("if ws.editing_title.is_some() {", "title_edit"),
        ("if ws.search.open {", "search"),
    ]
    .iter()
    .map(|(probe, name)| {
        assert!(handler.contains(probe), "the key handler must test {probe}");
        (handler.find(probe).expect("probe present"), *name)
    })
    .collect::<std::collections::BTreeMap<_, _>>()
    .into_values()
    .collect();

    assert_eq!(
        resolver_order, handler_order,
        "open_text_modal resolves modals in a different order than the real \
             key handler, so dispatch_ui_key would type into the wrong field"
    );
    // The Settings Theme row opens the theme picker over the panel, so the
    // picker must outrank every Settings branch or the panel eats its keys.
    let at = |name: &str| handler_order.iter().position(|n| *n == name).unwrap();
    assert!(
        at("theme_picker") < at("settings_text") && at("theme_picker") < at("settings"),
        "the theme picker must take keys before the Settings panel it covers"
    );

    // Matching order is not enough: dispatch_ui_key still has to send each
    // variant to *its own* handler. Swapping two arms in that match would
    // leave both orders identical above while typing palette keys into the
    // SSH launcher.
    let dispatch = src
        .split("fn ctl_dispatch_ui_key(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("ctl_dispatch_ui_key present");
    for (variant, handler) in [
        ("TextModal::Confirm", "self.confirm_dialog_key("),
        ("TextModal::Hint", "self.hint_key("),
        ("TextModal::Palette", "self.palette_key("),
        ("TextModal::ThemePicker", "self.theme_picker_key("),
        ("TextModal::SettingsText", "self.settings_text_key("),
        ("TextModal::Settings", "self.settings_key("),
        ("TextModal::LayoutPicker", "self.layout_picker_key("),
        ("TextModal::Ssh", "self.ssh_key("),
        ("TextModal::TitleEdit", "self.title_edit_key("),
        ("TextModal::Search", "self.search_key("),
    ] {
        // An arm runs to the next variant; the last one to the match's end.
        let arm = dispatch
            .split(&format!("{variant} =>"))
            .nth(1)
            .unwrap_or_else(|| panic!("dispatch_ui_key has no arm for {variant}"))
            .split("TextModal::")
            .next()
            .unwrap_or_default();
        assert!(
            arm.contains(handler),
            "dispatch_ui_key routes {variant} to `{arm}`, not {handler} — \
                 the control plane would type into a different field than the \
                 modal it reports"
        );
    }
}

/// In a Kettle modal, keys that activate, toggle or pick act once per
/// press, keys that move keep repeating, and a first press always acts.
#[test]
fn modal_activation_keys_act_once_per_press() {
    use super::{ModalRepeat::*, modal_repeat_is_inert};
    use winit::keyboard::{Key, NamedKey};
    let enter = Key::Named(NamedKey::Enter);
    let space = Key::Named(NamedKey::Space);
    let down = Key::Named(NamedKey::ArrowDown);
    let right = Key::Named(NamedKey::ArrowRight);
    let chr = |c: &str| Key::Character(c.into());
    let menu = ContextMenu { vim_nav: true };
    let plain_menu = ContextMenu { vim_nav: false };
    let toggle_row = Settings { steps: false };
    let number_row = Settings { steps: true };
    for modal in [ConfirmDialog, menu, ViMode, Hint, Palette, toggle_row] {
        for key in [&enter, &space, &down, &chr("v"), &chr("l")] {
            assert!(
                !modal_repeat_is_inert(modal, false, key, false),
                "{modal:?} {key:?} press"
            );
        }
    }
    for (modal, key, ctrl, inert) in [
        (menu, &enter, false, true),
        (menu, &space, false, true),
        (menu, &chr("l"), false, true),
        (menu, &chr("n"), false, true),
        (menu, &down, false, false),
        (menu, &right, false, false),
        (menu, &chr("j"), false, false),
        (menu, &chr("K"), false, false),
        (menu, &chr("d"), true, false),
        (menu, &chr("U"), true, false),
        (menu, &chr("d"), false, true),
        // Without vim-menu-nav, `j` and `k` are mnemonics.
        (plain_menu, &chr("j"), false, true),
        (plain_menu, &chr("d"), true, true),
        (plain_menu, &down, false, false),
        (ConfirmDialog, &enter, false, true),
        (ConfirmDialog, &chr("y"), false, true),
        (ConfirmDialog, &right, false, false),
        (Palette, &enter, false, true),
        (Palette, &chr("a"), false, false),
        (Palette, &down, false, false),
        (toggle_row, &enter, false, true),
        (toggle_row, &space, false, true),
        (toggle_row, &right, false, true),
        (toggle_row, &chr("h"), false, true),
        (toggle_row, &down, false, false),
        (number_row, &right, false, false),
        (number_row, &chr("l"), false, false),
        (number_row, &enter, false, true),
        (ViMode, &chr("v"), false, true),
        (ViMode, &chr("j"), false, false),
        (Hint, &chr("a"), false, true),
    ] {
        assert_eq!(
            modal_repeat_is_inert(modal, true, key, ctrl),
            inert,
            "{modal:?} {key:?} ctrl={ctrl}"
        );
    }
}

/// The key whose press closed or replaced a modal is remembered, so its
/// repeats are dropped: whether nothing is open afterwards, or another
/// modal took over (the palette running "Open layout picker"). A press the
/// modal kept is not remembered.
#[test]
fn a_key_that_closes_or_replaces_a_modal_stops_repeating() {
    use super::{KeyModal, drop_closing_key_repeat, remember_closing_key, top_modal};
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, PhysicalKey};

    let enter = PhysicalKey::Code(KeyCode::Enter);
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((Default::default(), 0));
    let owner = top_modal(&ws);
    assert_eq!(owner, Some(KeyModal::Palette));

    // An input-method composition is under way in the palette.
    ws.ime_preedit = Some(("ka".into(), None));
    ws.ime_preedit_owner = Some(crate::window_state::ImePreeditSession {
        owner: crate::window_state::ImePreeditOwner::Other,
        generation: ws.ime_focus_generation,
        ended: None,
        empty_after_focus: false,
    });
    let generation = ws.ime_focus_generation;

    // Typing keeps the palette open: nothing to drop, and the composition
    // goes on.
    remember_closing_key(&mut ws, owner, enter);
    assert!(ws.closing_keys.is_empty());
    assert_eq!(ws.ime_focus_generation, generation);
    assert!(ws.ime_preedit.is_some());

    // Enter runs "Open layout picker": the palette is replaced, and its
    // composition ends so a late Commit cannot type into the picker.
    ws.palette_input = None;
    ws.layout_picker_input = Some((Default::default(), 0));
    remember_closing_key(&mut ws, owner, enter);
    assert!(ws.closing_keys.contains(&enter));
    assert_ne!(ws.ime_focus_generation, generation);
    assert!(ws.ime_preedit.is_none());
    assert!(ws.ime_preedit_owner.is_some(), "the owner marker stays");
    assert!(
        drop_closing_key_repeat(&mut ws.closing_keys, enter, ElementState::Pressed, true),
        "the repeat must not reach the layout picker"
    );
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        enter,
        ElementState::Released,
        false
    ));
    assert!(ws.closing_keys.is_empty(), "the release ends it");

    // Escape closes the last modal: nothing open afterwards.
    let escape = PhysicalKey::Code(KeyCode::Escape);
    let owner = top_modal(&ws);
    ws.layout_picker_input = None;
    remember_closing_key(&mut ws, owner, escape);
    assert!(ws.closing_keys.contains(&escape));
    assert!(drop_closing_key_repeat(
        &mut ws.closing_keys,
        escape,
        ElementState::Pressed,
        true
    ));
    // A fresh press of Escape is never swallowed, and ends its
    // suppression.
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        escape,
        ElementState::Pressed,
        false
    ));
    assert!(ws.closing_keys.is_empty());
    // The Escape that cancels a keybind capture ends the capture mode,
    // so its repeats cannot go on to close Settings.
    ws.settings_nav = Some(crate::settings::SettingsNav {
        category: 0,
        field: 0,
        capturing: true,
    });
    let owner = top_modal(&ws);
    assert_eq!(owner, Some(KeyModal::SettingsCapture));
    ws.settings_nav.as_mut().unwrap().capturing = false;
    remember_closing_key(&mut ws, owner, escape);
    assert!(ws.closing_keys.contains(&escape));
}

/// Each held key that closed a modal stops repeating until its own
/// release: Enter runs a palette command that opens Settings, then
/// Escape, pressed while Enter is still down, closes Settings. Recording
/// Escape must not end Enter's suppression.
#[test]
fn every_held_closing_key_stops_repeating() {
    use super::{KeyModal, drop_closing_key_repeat, remember_closing_key, top_modal};
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, PhysicalKey};

    let enter = PhysicalKey::Code(KeyCode::Enter);
    let escape = PhysicalKey::Code(KeyCode::Escape);
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((Default::default(), 0));
    let owner = top_modal(&ws);
    ws.palette_input = None;
    ws.settings_nav = Some(crate::settings::SettingsNav {
        category: 0,
        field: 0,
        capturing: false,
    });
    remember_closing_key(&mut ws, owner, enter);
    let owner = top_modal(&ws);
    assert_eq!(owner, Some(KeyModal::Settings));
    ws.settings_nav = None;
    remember_closing_key(&mut ws, owner, escape);

    for key in [enter, escape] {
        assert!(
            drop_closing_key_repeat(&mut ws.closing_keys, key, ElementState::Pressed, true),
            "{key:?} closed a modal; its repeat must not reach the terminal"
        );
    }
    // Releasing Escape leaves Enter suppressed until its own release.
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        escape,
        ElementState::Released,
        false
    ));
    assert!(drop_closing_key_repeat(
        &mut ws.closing_keys,
        enter,
        ElementState::Pressed,
        true
    ));
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        enter,
        ElementState::Released,
        false
    ));
    assert!(ws.closing_keys.is_empty());
}

#[test]
fn keybind_rebind_persistence_reports_missing_path_and_writes_valid_path() {
    use super::persist_keybind_rebind;

    let stale = vec!["Ctrl+Shift+X".to_string()];
    assert!(
        !persist_keybind_rebind(None, &stale, "Ctrl+Shift+Y", "copy"),
        "a missing config path must be reported as not saved"
    );

    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    assert!(persist_keybind_rebind(
        Some(&path),
        &stale,
        "Ctrl+Shift+Y",
        "copy"
    ));
    let saved = std::fs::read_to_string(path).expect("saved keybind config");
    assert!(saved.contains("Ctrl+Shift+X=unbind"));
    assert!(saved.contains("Ctrl+Shift+Y=copy"));
}

#[test]
fn finding_synthetic_keys_reconcile_focus_before_batch() {
    let body = finding_body(include_str!("ctl.rs"), "ctl_send_keys");
    let sync = body.find("self.sync_pane_focus_reports(ws);");
    let write = body.find("feed_key_inputs(").unwrap();
    assert!(
        sync.is_some_and(|sync| sync < write),
        "ctl_send_keys must send the new pane's focus report before its key batch"
    );
}

#[test]
fn terminal_owned_key_focus_loss_sends_exactly_one_release() {
    let (mux, pane, _) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    let key = winit::keyboard::PhysicalKey::Code(winit::keyboard::KeyCode::ArrowUp);
    ws.terminal_owned_key_releases.insert(key);
    ws.terminal_key_recipients
        .entry(key)
        .or_default()
        .insert(pane);
    ws.terminal_key_release_bytes.insert(
        (key, pane),
        super::TerminalKeyRelease::new(Arc::from(&b"\x1b[1;1:3A"[..]), true),
    );
    assert_eq!(
        ws.mux.panes[&pane].feed_input(b"\x1b[1;1A"),
        PaneInputResult::Queued
    );
    let mut releases = Vec::new();
    super::release_terminal_owned_keys(&mut ws, |id, key, bytes| releases.push((id, key, bytes)));
    super::release_terminal_owned_keys(&mut ws, |id, key, bytes| releases.push((id, key, bytes)));
    assert_eq!(
        releases.len(),
        1,
        "focus loss must release a terminal-owned key exactly once"
    );
    assert_eq!(
        (releases[0].0, releases[0].1, releases[0].2.bytes.as_ref()),
        (pane, key, &b"\x1b[1;1:3A"[..])
    );
    assert!(
        ws.suppressed_key_releases.contains(&key),
        "the later physical release must be suppressed"
    );
    assert!(ws.terminal_key_recipients.is_empty());
    ws.mux.kill_children();
}


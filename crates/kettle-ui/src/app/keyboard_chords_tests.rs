#[test]
fn control_and_super_keep_option_available_for_chords() {
    use kettle_config::MacosOptionAsAlt;
    use winit::keyboard::ModifiersState;

    for chord in [
        ModifiersState::CONTROL | ModifiersState::ALT,
        ModifiersState::SUPER | ModifiersState::ALT,
        ModifiersState::CONTROL | ModifiersState::SUPER | ModifiersState::ALT,
    ] {
        for policy in [
            MacosOptionAsAlt::None,
            MacosOptionAsAlt::Left,
            MacosOptionAsAlt::Right,
            MacosOptionAsAlt::Both,
        ] {
            assert_eq!(
                macos_effective_modifiers(chord, policy, false, false),
                chord,
                "Ctrl/Super suppresses Option composition for {policy:?}"
            );
        }
    }
}

/// `backspace-binding` must not swallow the word delete.
///
/// That remap only ever replaces the UNMODIFIED encoding, and it tests the
/// encoded bytes rather than the modifier state — so restoring Option
/// cannot collide with it: `⌥⌫` is `ESC DEL`, which is not the plain form.
/// Worth pinning because restoring ALT also flips `uses_kitty_sequence`
/// true once a pane negotiates the Kitty protocol, which skips the remap
/// entirely; both routes have to reach the same word delete.
#[test]
fn a_backspace_binding_remap_still_leaves_the_word_delete_alone() {
    use kettle_config::{BackspaceBinding, Config, MacosOptionAsAlt};
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cfg = Config {
        backspace_binding: BackspaceBinding::ControlH,
        ..Config::default()
    };
    let key = Key::Named(NamedKey::Backspace);
    let raw = ModifiersState::ALT;
    let masked = macos_effective_modifiers(raw, MacosOptionAsAlt::None, true, false);
    let mods = macos_pty_modifiers(masked, raw, &key);

    // Legacy pane: ESC DEL survives the remap.
    let legacy = crate::input::encode(&key, None, mods, TermMode::empty()).expect("legacy");
    assert_eq!(
        apply_bs_del_binding(&cfg, &key, kettle_core::TermMode::empty(), legacy),
        vec![0x1b, 0x7f],
        "the remap replaces only the unmodified Backspace"
    );
    // …and an unmodified Backspace still honours the user's choice.
    let plain = crate::input::encode(&key, None, ModifiersState::empty(), TermMode::empty())
        .expect("plain");
    assert_eq!(apply_bs_del_binding(&cfg, &key, kettle_core::TermMode::empty(), plain), vec![0x08]);

    // Kitty pane: the chord becomes CSI-u and carries the alt bit there.
    let kitty = TermMode::KITTY_KEYBOARD_PROTOCOL | TermMode::DISAMBIGUATE_ESC_CODES;
    let sequence = crate::input::encode_key_press(&key, mods, kitty).expect("kitty");
    assert_eq!(
        sequence,
        b"\x1b[127;3u".to_vec(),
        "alt+backspace in a Kitty pane is CSI 127;3u"
    );
}

/// The PTY key path must read the key-aware modifiers, never `ws.mods`.
///
/// `write_terminal_key_event` is the single funnel for all three encode
/// calls — focused pane, local broadcast, foreign-window broadcast — plus
/// the modified-Enter probe. Reverting any one of them to `ws.mods` puts
/// `⌥⌫` back to deleting a single character in that path alone, which is
/// exactly the kind of partial regression a byte test would miss.
#[test]
fn the_pty_key_path_never_reads_the_masked_modifiers() {
    let src = production_source();
    let body = src
        .split("fn write_terminal_key_event(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("write_terminal_key_event body");

    assert_eq!(
        body.matches("pty_key_modifiers(ws, &event.logical_key)")
            .count(),
        1,
        "the key-aware modifiers are computed exactly once, at the top"
    );
    assert!(
        !body.contains("ws.mods"),
        "every modifier read in this function must go through \
             pty_key_modifiers, or Option stops reaching the encoder"
    );
    assert_eq!(
        body.matches("encode_key_event_for_mode(").count(),
        3,
        "focused pane, local broadcast and foreign broadcast must use the same modifiers"
    );
    assert_eq!(
        body.matches("key_release_bytes(").count(),
        3,
        "each press path must save its release with those same modifiers and mode"
    );
    let release = src
        .split("fn key_release_bytes(")
        .nth(1)
        .unwrap()
        .split("\n}")
        .next()
        .unwrap();
    assert!(release.contains("encode_key_event_for_mode(cfg, &release, mods, mode)"));
    let delivery = body
        .split("if !prepare_input {")
        .nth(1)
        .unwrap()
        .split("if ws.terminal_key_recipients.len()")
        .next()
        .unwrap();
    assert!(
        delivery.contains("terminal_key_release_bytes")
            && delivery.contains("send_or_defer_key_releases")
    );
    assert!(
        !delivery.contains("pty_key_modifiers"),
        "release delivery must use the saved press modifiers"
    );
}

/// `broadcast-default` picks the scope the group chord turns on.
///
/// The default maps to per-tab, so an unconfigured kettle keeps the
/// chord's per-tab behaviour.
#[test]
fn broadcast_default_picks_the_scope_the_group_chord_turns_on() {
    use crate::mux::BroadcastScope;
    use kettle_config::BroadcastDefault;

    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::default()),
        BroadcastScope::Tab,
        "an unconfigured kettle must keep the per-tab behaviour the chord \
             has always had"
    );
    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::All),
        BroadcastScope::All
    );
    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::Group),
        BroadcastScope::Tab
    );
    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::Off),
        BroadcastScope::Off
    );

    // The chord is a toggle, so the key that turns broadcast on also turns
    // it off. Drive the dispatch's own expression.
    let toggled = |current: BroadcastScope, on: BroadcastScope| {
        if current == on {
            BroadcastScope::Off
        } else {
            on
        }
    };
    let on = broadcast_scope_for_default(BroadcastDefault::Group);
    assert_eq!(
        toggled(BroadcastScope::Off, on.clone()),
        BroadcastScope::Tab
    );
    assert_eq!(
        toggled(BroadcastScope::Tab, on.clone()),
        BroadcastScope::Off,
        "pressing the chord again must turn broadcast off"
    );
    // From some other scope it selects the configured one rather than
    // toggling off — the user asked for this scope.
    assert_eq!(toggled(BroadcastScope::All, on), BroadcastScope::Tab);
    // And `off` means the chord genuinely cannot enable broadcast.
    let never = broadcast_scope_for_default(BroadcastDefault::Off);
    assert_eq!(
        toggled(BroadcastScope::Off, never.clone()),
        BroadcastScope::Off
    );
    assert_eq!(toggled(BroadcastScope::Tab, never), BroadcastScope::Off);

    // The dispatch must be running that expression, not a re-implementation.
    let src = production_source();
    let arm = src
        .split("Action::ToggleBroadcastAll => {")
        .nth(1)
        .expect("the broadcast toggle arm is present");
    assert!(
        arm.contains("broadcast_scope_for_default(self.cfg.broadcast_default)")
            && arm.contains("if ws.mux.broadcast == on"),
        "the toggle arm must read `broadcast-default` and compare against \
             the current scope"
    );
}

/// The bar keeps the keys a native find field keeps and hands every
/// other chord to Kettle's shortcuts, on both platforms.
#[test]
fn the_search_bar_keeps_editing_keys_and_passes_shortcuts_on() {
    use winit::keyboard::{Key, ModifiersState as M, NamedKey};
    let ch = |c: &str| Key::Character(c.into());
    let named = Key::Named;
    let claims =
        |key: &Key, mods: M, macos: bool| super::search_bar_claims(key, mods, macos, true, false);
    let (cmd, ctrl, alt, shift) = (M::SUPER, M::CONTROL, M::ALT, M::SHIFT);
    // macOS: the find field's own keys.
    for (key, mods) in [
        (named(NamedKey::Escape), M::empty()),
        (named(NamedKey::Enter), shift),
        (named(NamedKey::Tab), shift),
        (named(NamedKey::Backspace), cmd),
        (named(NamedKey::Backspace), alt),
        (named(NamedKey::Delete), cmd),
        (named(NamedKey::ArrowLeft), cmd | shift),
        (named(NamedKey::ArrowRight), alt | shift),
        (named(NamedKey::Home), shift),
        (ch("a"), cmd),
        (ch("c"), cmd),
        (ch("x"), cmd),
        (ch("v"), cmd),
        (ch("z"), cmd | shift),
        (ch("g"), cmd | shift),
        (ch("e"), ctrl),
        (ch("k"), ctrl),
    ] {
        assert!(claims(&key, mods, true), "macOS keeps {key:?} {mods:?}");
    }
    // macOS: Kettle's shortcuts.
    for (key, mods) in [
        (ch("t"), cmd),
        (ch("w"), cmd),
        (ch("="), cmd),
        (ch("1"), cmd),
        (ch("k"), cmd),
        (ch("p"), cmd | shift),
        (named(NamedKey::ArrowLeft), cmd | alt),
        (named(NamedKey::ArrowLeft), ctrl | cmd),
        (named(NamedKey::ArrowUp), cmd),
        (named(NamedKey::ArrowUp), shift),
        (named(NamedKey::PageUp), shift),
        (named(NamedKey::Enter), cmd),
        (named(NamedKey::F11), M::empty()),
        (ch(","), ctrl),
        (ch("k"), ctrl | shift),
        (named(NamedKey::Backspace), ctrl),
    ] {
        assert!(
            !claims(&key, mods, true),
            "macOS passes on {key:?} {mods:?}"
        );
    }
    // Linux: the find field's own keys.
    for (key, mods) in [
        (named(NamedKey::Backspace), ctrl),
        (named(NamedKey::ArrowLeft), ctrl | shift),
        (ch("a"), ctrl),
        (ch("x"), ctrl),
        (ch("c"), ctrl | shift),
        (ch("v"), ctrl | shift),
        (ch("g"), ctrl | shift),
        (ch("z"), ctrl | shift),
    ] {
        assert!(claims(&key, mods, false), "Linux keeps {key:?} {mods:?}");
    }
    // Linux: Kettle's shortcuts, including the Ctrl+Shift chords that
    // share a letter with an editing key.
    for (key, mods) in [
        (ch("a"), ctrl | shift),
        (ch("x"), ctrl | shift),
        (ch("t"), ctrl | shift),
        (ch("k"), ctrl | shift),
        (ch("e"), ctrl),
        (named(NamedKey::ArrowLeft), alt),
        (named(NamedKey::Home), ctrl),
        (named(NamedKey::PageDown), ctrl),
        (ch("1"), alt),
        (named(NamedKey::Backspace), alt),
        (named(NamedKey::Delete), alt),
    ] {
        assert!(
            !claims(&key, mods, false),
            "Linux passes on {key:?} {mods:?}"
        );
    }
    // A focused button takes Space; the query types it.
    assert!(super::search_bar_claims(
        &named(NamedKey::Space),
        M::empty(),
        true,
        false,
        false
    ));
    assert!(!super::search_bar_claims(
        &named(NamedKey::Space),
        M::empty(),
        true,
        true,
        false
    ));
    // Caret keys belong to the query, not to a focused button.
    assert!(claims(&named(NamedKey::ArrowLeft), M::empty(), true));
    assert!(!super::search_bar_claims(
        &named(NamedKey::ArrowLeft),
        M::empty(),
        true,
        false,
        false
    ));
}

/// An Alt chord bound to a shortcut runs it rather than typing: Linux's
/// Alt+1 goes to tab 1. AltGr (Ctrl+Alt) keeps composing characters, and
/// an unbound Alt chord still types what it produced.
#[test]
fn a_bound_alt_chord_runs_its_shortcut_instead_of_typing() {
    use super::search_bar_text as text;
    use winit::keyboard::ModifiersState as M;
    assert_eq!(text(Some("1"), M::ALT, true, false), None);
    assert_eq!(text(Some("1"), M::ALT, false, false), Some("1"));
    // Windows AltGr composes; elsewhere Ctrl+Alt is a real chord.
    assert_eq!(text(Some("@"), M::ALT | M::CONTROL, true, true), Some("@"));
    assert_eq!(text(Some("t"), M::ALT | M::CONTROL, true, false), None);
    assert_eq!(text(Some("a"), M::SHIFT, true, false), Some("a"));
    assert_eq!(text(None, M::ALT, false, false), None);
}

/// Drift guard. `search_key`'s catch-all must route typed text through the
/// shared modal rule and insert through its bounded editor.
///
/// The handler's explicit arms claim ⌘A/⌘C/⌘X/⌘V. A catch-all that filters
/// only control characters lets every other Command chord type its letter
/// into the query (`⌘Q` adds a `q`). Needs full App state; pin at the
/// source.
#[test]
fn search_key_filters_control_chars() {
    let src = production_source();
    let arm = src
        .split("fn search_key(")
        .nth(1)
        .and_then(|s| s.split("\n    }\n").next())
        .expect("search_key present");
    assert!(
        arm.contains("crate::modal_input::accept_text(text, ws.mods)"),
        "search_key must filter typed text through the shared modal rule, \
             or an unclaimed Command chord types its letter into the query"
    );
    assert!(
        arm.contains(".type_text(t, kettle_core::search::MAX_SEARCH_QUERY_BYTES)"),
        "search_key must type through the bounded editor"
    );
    assert!(
        !arm.contains("!t.chars().any(|c| c.is_control())"),
        "the inline control-character filter is now modal_input's job"
    );
    // The checks above only prove the catch-all is right. A *new*
    // `Key::Character` arm added above it with no guard would take
    // precedence and reintroduce the bug with every assertion still green,
    // so require every character arm in this handler to carry a condition.
    for (index, _) in arm.match_indices("Key::Character(") {
        let rest = &arm[index..];
        let head = rest.split("=>").next().unwrap_or_default();
        assert!(
            head.contains(" if "),
            "search_key has an unguarded `Key::Character` arm: `{}` — an \
                 arm without a modifier condition claims Command chords before \
                 the catch-all ever applies the shared rule",
            head.trim()
        );
    }
}

/// `parse_send_key` is the entire vocabulary an agent can press. Pin the
/// grammar: named keys (incl. the ones the keybind grammar lacks), chords
/// with every modifier alias, preserved character case, and loud failure
/// on typos.
#[test]
fn parse_send_key_grammar() {
    use super::parse_send_key;
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    let none = ModifiersState::empty();
    // Named keys the keybind grammar has no variant for.
    assert_eq!(
        parse_send_key("escape"),
        Some((none, Key::Named(NamedKey::Escape)))
    );
    assert_eq!(
        parse_send_key("backspace"),
        Some((none, Key::Named(NamedKey::Backspace)))
    );
    assert_eq!(
        parse_send_key("space"),
        Some((none, Key::Named(NamedKey::Space)))
    );
    // Chords, with aliases.
    assert_eq!(
        parse_send_key("ctrl+c"),
        Some((ModifiersState::CONTROL, Key::Character("c".into())))
    );
    assert_eq!(
        parse_send_key("shift+tab"),
        Some((ModifiersState::SHIFT, Key::Named(NamedKey::Tab)))
    );
    assert_eq!(
        parse_send_key("alt+enter"),
        Some((ModifiersState::ALT, Key::Named(NamedKey::Enter)))
    );
    assert_eq!(
        parse_send_key("control+shift+f5"),
        Some((
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            Key::Named(NamedKey::F5)
        ))
    );
    // Character case and its implied Shift are preserved — `G` (vim:
    // jump to end) is not an unmodified `g`.
    assert_eq!(
        parse_send_key("G"),
        Some((ModifiersState::SHIFT, Key::Character("G".into())))
    );
    assert_eq!(
        parse_send_key(":"),
        Some((none, Key::Character(":".into())))
    );
    // Shift+letter normalizes to the uppercase logical character while
    // retaining SHIFT. Legacy encoding still produces the uppercase byte,
    // while negotiated CSI-u preserves the actual chord. Super characters
    // remain valid tokens because the pane's live mode decides whether
    // Kitty CSI-u can encode them. The chord/CLI separator characters are
    // reachable via their names.
    assert_eq!(
        parse_send_key("shift+g"),
        Some((ModifiersState::SHIFT, Key::Character("G".into())))
    );
    assert_eq!(
        parse_send_key("super+x"),
        Some((ModifiersState::SUPER, Key::Character("x".into())))
    );
    assert_eq!(
        parse_send_key("plus"),
        Some((none, Key::Character("+".into())))
    );
    assert_eq!(
        parse_send_key("comma"),
        Some((none, Key::Character(",".into())))
    );
    assert_eq!(
        parse_send_key("ctrl+minus"),
        Some((ModifiersState::CONTROL, Key::Character("-".into())))
    );
    // Typos fail loudly instead of degrading.
    assert_eq!(parse_send_key("cttrl+c"), None, "typo'd modifier");
    assert_eq!(parse_send_key("ctrl+"), None, "missing key");
    assert_eq!(parse_send_key("f13"), None, "no such F-key");
    assert_eq!(parse_send_key("escape+x"), None, "named key as modifier");
}

#[test]
fn parse_ui_key_allows_app_shortcuts_and_rejects_inert_tokens() {
    use super::parse_ui_key;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    assert_eq!(
        parse_ui_key("cmd+a"),
        Some((ModifiersState::SUPER, Key::Character("a".into())))
    );
    assert_eq!(
        parse_ui_key("space"),
        Some((ModifiersState::empty(), Key::Character(" ".into())))
    );
    assert_eq!(
        parse_ui_key("shift+f3"),
        Some((ModifiersState::SHIFT, Key::Named(NamedKey::F3)))
    );
    assert_eq!(
        parse_ui_key("up"),
        Some((ModifiersState::empty(), Key::Named(NamedKey::ArrowUp)))
    );
    assert_eq!(
        parse_ui_key("down"),
        Some((ModifiersState::empty(), Key::Named(NamedKey::ArrowDown)))
    );
    assert_eq!(parse_ui_key("page_down"), None);
    assert_eq!(parse_ui_key("insert"), None);
}

#[test]
fn send_keys_caps_batch_tokens_and_encoded_bytes_before_growth() {
    let oversized = vec![serde_json::json!("a"); super::MAX_CTL_SEND_KEYS + 1];
    assert!(
        super::parse_ctl_send_key_batch(&oversized)
            .unwrap_err()
            .contains("must contain")
    );
    assert!(
        super::parse_ctl_send_key_batch(&[serde_json::json!(
            "x".repeat(super::MAX_CTL_SEND_KEY_TOKEN_BYTES + 1)
        )])
        .unwrap_err()
        .contains("key tokens")
    );

    let mut bytes = vec![b'x'; super::MAX_CTL_SEND_KEY_BYTES];
    assert!(super::extend_ctl_send_key_bytes(&mut bytes, b"y").is_err());
    assert_eq!(bytes.len(), super::MAX_CTL_SEND_KEY_BYTES);

    let src = production_source();
    let body = src
        .split("fn ctl_send_keys(")
        .nth(1)
        .and_then(|body| body.split("\n    /// Resolve the `pane` param").next())
        .expect("ctl_send_keys body");
    assert!(
        body.contains("p.feed_key_inputs(&encoded_keys)"),
        "send_keys must preserve per-key boundaries so every Tab advances one request"
    );
    assert!(
        !body.contains("p.feed_input(&bytes)"),
        "a flattened key batch hides Tab boundaries from completion sequencing"
    );
}

#[test]
fn ctl_send_keys_rejects_unencodable_super_tokens_before_writing() {
    let parsed = super::parse_ctl_send_key_batch(&[serde_json::json!("cmd+up")])
        .expect("cmd+up is a valid mode-dependent token");
    let (token, mods, key) = &parsed[0];
    assert_eq!(token, "cmd+up");
    assert_eq!(
        crate::input::encode_key_press(key, *mods, kettle_core::TermMode::empty()),
        None,
        "cmd+up must be unencodable before Kitty negotiation"
    );

    let src = production_source();
    let body = src
        .split("fn ctl_send_keys(")
        .nth(1)
        .and_then(|body| body.split("\n    /// Resolve the `pane` param").next())
        .expect("ctl_send_keys body");
    let encode = body
        .find("encode_key_press(key, *mods, mode)")
        .expect("send_keys encoder call");
    let reject = body
        .find("'{token}' has no legacy encoding")
        .expect("legacy Super rejection");
    let feed = body
        .find("p.feed_key_inputs(&encoded_keys)")
        .expect("pane input call");
    assert!(
        body.contains("for (token, mods, key) in &parsed")
            && body.contains("has not negotiated the Kitty keyboard protocol")
            && body.contains("negotiated Kitty keyboard mode")
            && body[reject..feed].contains("Response::err(req.id, ec::BAD_PARAMS, message)"),
        "ctl_send_keys must name the token and return BAD_PARAMS for every active mode"
    );
    assert!(
        encode < reject && reject < feed,
        "ctl_send_keys must reject an unencodable token before writing any parsed key"
    );
    // Ordering alone is not atomicity. A `p.feed_input(&b)` added inside
    // the encoding loop would sit before the rejection and satisfy every
    // check above, while `["a", "cmd+up"]` wrote `a` and then errored --
    // a partial batch, which is the exact failure this is here to prevent.
    // Pin that the whole body performs exactly ONE write, whatever it is
    // named.
    let writes = body.matches(".feed_").count();
    assert_eq!(
        writes, 1,
        "ctl_send_keys must write to the pane exactly once, after the whole \
             batch has been encoded; found {writes} write calls"
    );
    assert!(
        body[..reject].matches(".feed_").count() == 0,
        "no PTY write may precede the unencodable-token rejection"
    );
}

/// The encoded bytes must match what a human pressing the same keys
/// produces, through the same encoder and the same mode handling.
#[test]
fn send_keys_tokens_encode_like_gui_keystrokes() {
    use super::parse_send_key;
    use kettle_core::TermMode;
    let enc = |tok: &str, mode: TermMode| {
        let (mods, key) = parse_send_key(tok).expect(tok);
        crate::input::encode_key_press(&key, mods, mode)
    };
    let plain = TermMode::empty();
    assert_eq!(enc("escape", plain), Some(vec![0x1b]));
    assert_eq!(enc("ctrl+c", plain), Some(vec![0x03]));
    assert_eq!(enc("enter", plain), Some(vec![b'\r']));
    assert_eq!(enc("up", plain), Some(b"\x1b[A".to_vec()));
    // DECCKM application-cursor mode flips arrows to SS3 — the reason
    // send_keys reads the pane's LIVE mode (vim sets it).
    assert_eq!(enc("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
    assert_eq!(enc("shift+tab", plain), Some(b"\x1b[Z".to_vec()));
    assert_eq!(enc("G", plain), Some(b"G".to_vec()));
    assert_eq!(enc("cmd+up", plain), None);
    assert_eq!(
        enc("cmd+up", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[1;9A".to_vec())
    );
    assert_eq!(
        enc("escape", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[27u".to_vec()),
        "send_keys must honor Kitty keyboard negotiation"
    );
    assert_eq!(
        enc("ctrl+c", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[99;5u".to_vec())
    );
    assert_eq!(
        enc("ctrl+shift+c", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[99;6u".to_vec()),
        "synthetic chords must retain the same modifiers as GUI input"
    );
    assert_eq!(
        enc(
            "shift+g",
            TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ALTERNATE_KEYS
        ),
        Some(b"\x1b[103:71;2u".to_vec())
    );
    assert_eq!(
        enc(
            "G",
            TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ALTERNATE_KEYS
        ),
        Some(b"\x1b[103:71;2u".to_vec()),
        "bare uppercase tokens must retain Kitty's Shift semantics"
    );
    assert_eq!(
        enc(
            "ctrl+space",
            TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ASSOCIATED_TEXT
        ),
        Some(b"\x1b[32;5u".to_vec()),
        "synthetic Control chords must not invent associated text"
    );
}

/// Every named key a config can bind arrives from the winit key that
/// produces it: winit reports the space bar as `NamedKey::Space`, so the
/// `ctrl+shift+space` vi-mode default was dead until it was mapped.
#[test]
fn every_configurable_named_key_matches_its_real_key() {
    use super::{resolve_keybind_action, to_kkey};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let mut pairs = vec![
        ("up", NamedKey::ArrowUp),
        ("down", NamedKey::ArrowDown),
        ("left", NamedKey::ArrowLeft),
        ("right", NamedKey::ArrowRight),
        ("pageup", NamedKey::PageUp),
        ("pagedown", NamedKey::PageDown),
        ("home", NamedKey::Home),
        ("end", NamedKey::End),
        ("enter", NamedKey::Enter),
        ("tab", NamedKey::Tab),
        ("backspace", NamedKey::Backspace),
        ("delete", NamedKey::Delete),
        ("space", NamedKey::Space),
    ];
    let fkeys = [
        NamedKey::F1,
        NamedKey::F2,
        NamedKey::F3,
        NamedKey::F4,
        NamedKey::F5,
        NamedKey::F6,
        NamedKey::F7,
        NamedKey::F8,
        NamedKey::F9,
        NamedKey::F10,
        NamedKey::F11,
        NamedKey::F12,
    ];
    let fnames: Vec<String> = (1..=12).map(|n| format!("f{n}")).collect();
    pairs.extend(fnames.iter().map(String::as_str).zip(fkeys));
    for (name, named) in pairs {
        let config = kettle_config::Config::parse_text(&format!(
            "keybind = ctrl+alt+{name}=toggle_vi_mode\n"
        ));
        let trigger = config
            .keybinds_declared
            .iter()
            .next()
            .copied()
            .unwrap_or_else(|| panic!("`{name}` does not parse"));
        assert_eq!(
            to_kkey(&Key::Named(named)),
            Some(trigger.key),
            "`{name}` parses to {:?} but {named:?} does not map to it",
            trigger.key
        );
    }

    let defaults = kettle_config::keybinds::defaults();
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(
        resolve_keybind_action(
            &defaults,
            Some(&Key::Named(NamedKey::Space)),
            None,
            mods,
            None
        ),
        Some((
            Trigger::new(Mods::CTRL | Mods::SHIFT, KKey::Char(' ')),
            Action::ToggleViMode
        ))
    );
}

/// The chord that enters vi mode also leaves it, although vi mode takes
/// every key before the keybind table; other keys still drive it.
#[test]
fn the_vi_mode_chord_leaves_vi_mode() {
    use super::is_vi_mode_toggle_chord;
    use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

    let defaults = kettle_config::keybinds::defaults();
    let space = PhysicalKey::Code(KeyCode::Space);
    let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert!(is_vi_mode_toggle_chord(
        &defaults,
        &Key::Named(NamedKey::Space),
        &space,
        cs,
        None,
    ));
    assert!(!is_vi_mode_toggle_chord(
        &defaults,
        &Key::Named(NamedKey::Space),
        &space,
        ModifiersState::empty(),
        None,
    ));
    assert!(!is_vi_mode_toggle_chord(
        &defaults,
        &Key::Character("j".into()),
        &PhysicalKey::Code(KeyCode::KeyJ),
        ModifiersState::empty(),
        None,
    ));
    let src = production_source();
    let intercept = src
        .split("if ws.vi_mode.is_some() {\n                    if is_vi_mode_toggle_chord(")
        .nth(1)
        .expect("the keyboard vi-mode intercept must check the toggle chord");
    let exit = intercept
        .find("self.exit_vi_mode(ws)")
        .expect("the chord exits vi mode");
    assert!(
        intercept[..exit].contains("if !event.repeat {"),
        "a held chord must leave vi mode once, not flip it on every repeat"
    );
}

/// Auto-repeat of a held toggle chord runs the toggle once; repeat of a
/// motion or scroll chord keeps running it.
#[test]
fn toggles_run_once_per_press_while_held() {
    use super::toggles_once_per_press;
    use kettle_config::Action;
    for toggle in [
        Action::ToggleViMode,
        Action::ToggleZoom,
        Action::ScaledZoom,
        Action::ToggleFullscreen,
        Action::ToggleBroadcastAll,
        Action::TogglePaneReadOnly,
    ] {
        assert!(toggles_once_per_press(&toggle), "{toggle:?}");
    }
    for repeating in [
        Action::ScrollLineUp,
        Action::ScrollPageDown,
        Action::ResizeLeft,
        Action::IncreaseFontSize,
        Action::NextTab,
        Action::JumpPrevPrompt,
    ] {
        assert!(!toggles_once_per_press(&repeating), "{repeating:?}");
    }
    // Every toggle a config can bind is in the list.
    for name in kettle_config::keybinds::action_names() {
        if let Some(action) = Action::from_name(name)
            && format!("{action:?}").starts_with("Toggle")
        {
            assert!(toggles_once_per_press(&action), "{name} is a toggle");
        }
    }
    let src = production_source();
    assert!(
        src.contains("if !(event.repeat && toggles_once_per_press(&act)) {"),
        "the keyboard keybind path must skip toggle repeats"
    );
    assert!(
        src.contains("if !(repeat && toggles_once_per_press(&action)) {"),
        "the search bar's shortcut route must skip toggle repeats"
    );
    assert!(
        src.contains(
            "let inert = if settings_capture_active(ws) {\n                        event.repeat"
        ),
        "keybind capture must wait for a new press, not take the activating key's repeats"
    );
}

#[test]
fn keybind_resolver_uses_logical_before_physical_fallback() {
    use super::resolve_keybind_action;
    use kettle_config::{Action, Bindings, Key as KKey, Mods, Trigger};
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let kmods = Mods::CTRL | Mods::SHIFT;
    let mut bindings = Bindings::new();
    bindings.insert(Trigger::new(kmods, KKey::Char('=')), Action::ResetFontSize);
    bindings.insert(
        Trigger::new(kmods, KKey::Char('+')),
        Action::IncreaseFontSize,
    );

    let matched = resolve_keybind_action(
        &bindings,
        Some(&Key::Character("=".into())),
        Some(&PhysicalKey::Code(KeyCode::Equal)),
        mods,
        None,
    );
    assert_eq!(
        matched,
        Some((Trigger::new(kmods, KKey::Char('=')), Action::ResetFontSize))
    );
}

/// `new_tab = <Control><Shift>comma` imports as Ctrl+Shift+`,`, but with
/// Shift held a US keyboard reports `<`. The binding must still fire, or the
/// import (which replaces the default chord) leaves New Tab without one.
#[test]
fn imported_shifted_punctuation_binding_matches_the_shifted_character() {
    use super::resolve_keybind_action;
    use kettle_config::{Action, Config};
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    let cfg = Config::parse_text("[keybindings]\nnew_tab = <Control><Shift>comma\n");
    let matched = resolve_keybind_action(
        &cfg.keybinds,
        Some(&Key::Character("<".into())),
        Some(&PhysicalKey::Code(KeyCode::Comma)),
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        None,
    );
    assert_eq!(matched.map(|(_, action)| action), Some(Action::NewTab));
}

/// A Latin layout's own character wins over a US alias of the same key: on
/// Dvorak, Ctrl+W sits on the US comma key and must reach the shell rather
/// than open Settings (Ctrl+,). A key whose character is not ASCII, as
/// AZERTY's `à` on the 0 key, still finds its US alias.
#[test]
fn physical_aliases_never_replace_another_latin_character() {
    use super::resolve_keybind_action;
    use kettle_config::Action;
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    let defaults = kettle_config::keybinds::defaults();
    let ctrl = ModifiersState::CONTROL;
    let dvorak_ctrl_w = resolve_keybind_action(
        &defaults,
        Some(&Key::Character("w".into())),
        Some(&PhysicalKey::Code(KeyCode::Comma)),
        ctrl,
        None,
    );
    assert_ne!(
        dvorak_ctrl_w.map(|(_, action)| action),
        Some(Action::OpenSettings),
        "Dvorak Ctrl+W opened Settings"
    );
    let dvorak_ctrl_bracket = resolve_keybind_action(
        &defaults,
        Some(&Key::Character("]".into())),
        Some(&PhysicalKey::Code(KeyCode::Equal)),
        ctrl,
        None,
    );
    assert!(
        !matches!(
            dvorak_ctrl_bracket.map(|(_, action)| action),
            Some(Action::IncreaseFontSize | Action::ResetFontSize)
        ),
        "Dvorak Ctrl+] changed the font size"
    );
    let azerty_ctrl_zero = resolve_keybind_action(
        &defaults,
        Some(&Key::Character("à".into())),
        Some(&PhysicalKey::Code(KeyCode::Digit0)),
        ctrl,
        None,
    );
    assert_eq!(
        azerty_ctrl_zero.map(|(_, action)| action),
        Some(Action::ResetFontSize)
    );
}

#[test]
fn review_shifted_zero_bindings_match_us_keyboard() {
    use super::resolve_keybind_action;
    use kettle_config::Config;
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    for (config, expected) in [
        ("keybind = ctrl+shift+0=copy\n", Action::Copy),
        (
            "keybind = ctrl+shift+0=reset_font_size\n",
            Action::ResetFontSize,
        ),
        (
            "[keybindings]\nnew_tab = <Control><Shift>0\n",
            Action::NewTab,
        ),
    ] {
        let cfg = Config::parse_text(config);
        let matched = resolve_keybind_action(
            &cfg.keybinds,
            Some(&Key::Character(")".into())),
            Some(&PhysicalKey::Code(KeyCode::Digit0)),
            mods,
            None,
        );
        assert_eq!(
            matched.map(|(_, action)| action),
            Some(expected),
            "{config}"
        );
    }
}

#[test]
fn review_shifted_punctuation_uses_layout_base_key() {
    use super::resolve_keybind_action;
    use kettle_config::Config;
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};
    let cfg = Config::parse_text("[keybindings]\nnew_tab = <Control><Shift>comma\n");
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let physical = PhysicalKey::Code(KeyCode::Comma);
    let matched = resolve_keybind_action(
        &cfg.keybinds,
        Some(&Key::Character(";".into())),
        Some(&physical),
        mods,
        Some(&Key::Character(",".into())),
    );
    assert_eq!(matched.map(|(_, action)| action), Some(Action::NewTab));
    let dvorak = resolve_keybind_action(
        &cfg.keybinds,
        Some(&Key::Character("W".into())),
        Some(&physical),
        mods,
        Some(&Key::Character("w".into())),
    );
    let logical_only = resolve_keybind_action(
        &cfg.keybinds,
        Some(&Key::Character("W".into())),
        None,
        mods,
        Some(&Key::Character("w".into())),
    );
    assert_eq!(dvorak, logical_only, "Dvorak must keep its logical binding");
    assert_ne!(dvorak.map(|(_, action)| action), Some(Action::NewTab));
}

#[test]
fn keybind_resolver_honors_shifted_digit_rebinds_and_unbinds() {
    use super::resolve_keybind_action;
    use kettle_config::{Action, Config};
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let physical = PhysicalKey::Code(KeyCode::Digit1);
    for spelling in ["1", "!"] {
        for (action, expected) in [("copy", Some(Action::Copy)), ("unbind", None)] {
            let cfg = Config::parse_text(&format!("keybind = ctrl+shift+{spelling} = {action}\n"));
            for logical in ["1", "!"] {
                let matched = resolve_keybind_action(
                    &cfg.keybinds,
                    Some(&Key::Character(logical.into())),
                    Some(&physical),
                    mods,
                    None,
                );
                assert_eq!(
                    matched.map(|(_, action)| action),
                    expected,
                    "{spelling}={action}, logical {logical}"
                );
            }
        }
    }
}

#[test]
fn keybind_resolver_falls_back_to_physical_zoom_keys() {
    use super::resolve_keybind_action;
    use kettle_config::Action;
    use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

    let defaults = kettle_config::keybinds::defaults();
    let ctrl = ModifiersState::CONTROL;
    let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;

    for (physical, mods, expected) in [
        (
            PhysicalKey::Code(KeyCode::Equal),
            ctrl_shift,
            Action::IncreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::NumpadAdd),
            ctrl,
            Action::IncreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::Minus),
            ctrl,
            Action::DecreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::NumpadSubtract),
            ctrl,
            Action::DecreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::Digit0),
            ctrl,
            Action::ResetFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::Numpad0),
            ctrl,
            Action::ResetFontSize,
        ),
    ] {
        let matched = resolve_keybind_action(&defaults, None, Some(&physical), mods, None)
            .unwrap_or_else(|| panic!("{physical:?} should resolve"));
        assert_eq!(matched.1, expected);
    }
    assert_eq!(
        resolve_keybind_action(
            &defaults,
            None,
            Some(&PhysicalKey::Code(KeyCode::KeyA)),
            ctrl,
            None,
        ),
        None
    );
}

#[test]
fn adaptive_alt_focus_matches_only_the_default_directional_pairs() {
    use super::{adaptive_alt_focus_direction, adaptive_alt_focus_falls_through};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};

    for (key, action, direction) in [
        (KKey::Up, Action::FocusUp, (0, -1)),
        (KKey::Down, Action::FocusDown, (0, 1)),
        (KKey::Left, Action::FocusLeft, (-1, 0)),
        (KKey::Right, Action::FocusRight, (1, 0)),
    ] {
        assert_eq!(
            adaptive_alt_focus_direction(Trigger::new(Mods::ALT, key), &action, true),
            Some(direction)
        );
    }

    assert_eq!(
        adaptive_alt_focus_direction(Trigger::new(Mods::ALT, KKey::Up), &Action::FocusDown, true,),
        None,
        "a customized mismatched action must stay an application keybind"
    );
    assert_eq!(
        adaptive_alt_focus_direction(
            Trigger::new(Mods::CTRL | Mods::ALT, KKey::Up),
            &Action::FocusUp,
            true,
        ),
        None,
        "extra modifiers are not the adaptive default chord"
    );
    assert_eq!(
        adaptive_alt_focus_direction(Trigger::new(Mods::ALT, KKey::Up), &Action::FocusUp, false,),
        None,
        "macOS leaves Option+Arrow to the PTY and focuses with Cmd+Opt or Ctrl+Cmd"
    );

    let trigger = Trigger::new(Mods::ALT, KKey::Up);
    assert!(
        adaptive_alt_focus_falls_through(trigger, &Action::FocusUp, true, false),
        "no visible neighbour: the chord belongs to the program"
    );
    assert!(
        !adaptive_alt_focus_falls_through(trigger, &Action::FocusUp, true, true),
        "a visible neighbour keeps the chord as a focus move"
    );
    assert!(
        !adaptive_alt_focus_falls_through(trigger, &Action::FocusDown, true, false),
        "a customised mismatched pair never falls through, neighbour or not"
    );
    assert!(
        !adaptive_alt_focus_falls_through(trigger, &Action::FocusUp, false, false),
        "the adaptive rule is off on macOS even with nowhere to go"
    );
}

/// A zoomed multi-pane tab hides its siblings, so the mux reports no
/// visible neighbour and the adaptive chord must fall through to the
/// program. That is why `adaptive_alt_focus_falls_through` takes the mux's
/// visible-neighbour answer instead of a zoom flag. A zoom-flag guard
/// would keep the chord as a Kettle no-op and swallow Codex's
/// `Alt+Left`/`Alt+Right` word motion while `Ctrl+Shift+X` or the
/// `scaled_zoom` action is active.
#[test]
fn adaptive_alt_focus_falls_through_while_zoom_hides_siblings() {
    use super::adaptive_alt_focus_falls_through;
    use crate::mux::{Dir, Mux, Node, Tab};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};

    let area = (0.0, 0.0, 800.0, 600.0);
    let mut mux = Mux::new();
    mux.tabs.push(Tab {
        root: Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        },
        focus: 2,
        title_override: None,
        zoomed: false,
        lanes: Default::default(),
        last_output_at: None,
        last_seen_at: None,
        bell: false,
    });
    mux.active = 0;
    let left = (
        Trigger::new(Mods::ALT, KKey::Left),
        Action::FocusLeft,
        (-1, 0),
    );
    let right = (
        Trigger::new(Mods::ALT, KKey::Right),
        Action::FocusRight,
        (1, 0),
    );

    let route = |mux: &Mux, (trigger, action, (dx, dy)): &(Trigger, Action, (i32, i32))| {
        adaptive_alt_focus_falls_through(
            *trigger,
            action,
            true,
            mux.pane_in_direction(area, *dx, *dy).is_some(),
        )
    };

    assert!(
        !route(&mux, &left),
        "unzoomed: the right pane has a left neighbour, so Alt+Left moves focus"
    );
    assert!(
        route(&mux, &right),
        "unzoomed: nothing lies right of the right pane, so Alt+Right falls through"
    );

    mux.toggle_zoom();
    assert!(mux.is_zoomed());
    assert!(
        route(&mux, &left),
        "zoomed: the hidden left sibling is not a visible neighbour"
    );
    assert!(route(&mux, &right));

    mux.toggle_zoom();
    assert!(!route(&mux, &left), "leaving zoom restores the focus move");
}

/// A chord the config binds itself is Kettle's even when the line
/// restates the default pair, `keybind-yield = off` turns every rule off,
/// and binding one chord leaves the others' rules alone.
#[test]
fn a_chord_the_user_binds_is_never_yielded() {
    use super::{ProgramKeyRule::*, configured_program_key_rule};
    use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
    let shift_left = Trigger::new(Mods::SHIFT, KKey::Left);
    let shift_right = Trigger::new(Mods::SHIFT, KKey::Right);
    let rule = |cfg: &Config, trigger, action: &Action| {
        configured_program_key_rule(cfg, trigger, action, false)
    };

    let stock = Config::parse_text("");
    assert_eq!(
        rule(&stock, shift_left, &Action::ResizeLeft),
        Some(ProgramFirst)
    );

    let restated = Config::parse_text("keybind = shift+left=resize_left\n");
    assert_eq!(
        restated.keybinds.get(&shift_left),
        Some(&Action::ResizeLeft)
    );
    assert_eq!(rule(&restated, shift_left, &Action::ResizeLeft), None);
    assert_eq!(
        rule(&restated, shift_right, &Action::ResizeRight),
        Some(ProgramFirst),
        "binding one chord leaves the other defaults' rules alone"
    );

    let imported = Config::parse_text("resize_left = <Shift>Left\n");
    assert_eq!(
        imported.keybinds.get(&shift_left),
        Some(&Action::ResizeLeft)
    );
    assert_eq!(rule(&imported, shift_left, &Action::ResizeLeft), None);

    let unbound_then_default = Config::parse_text("keybind = shift+left=unbind\n");
    assert!(unbound_then_default.keybinds_declared.is_empty());

    let off = Config::parse_text("keybind-yield = off\n");
    assert_eq!(rule(&off, shift_left, &Action::ResizeLeft), None);
}

/// Only the shipped default (trigger, action) pairs yield: the same
/// chord bound to another action, or the action moved to another chord,
/// stays Kettle's, as `adaptive_alt_focus_direction` pins for Alt+Arrow.
#[test]
fn program_key_rules_match_only_exact_default_pairs() {
    use super::{ProgramKeyRule::*, program_key_rule};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};
    let shift = Mods::SHIFT;
    let ctrl = Mods::CTRL;
    let cs = Mods::CTRL | Mods::SHIFT;
    for (mods, key, action, rule) in [
        (shift, KKey::Left, Action::ResizeLeft, ProgramFirst),
        (shift, KKey::Right, Action::ResizeRight, ProgramFirst),
        (shift, KKey::Up, Action::ResizeUp, ProgramFirst),
        (shift, KKey::Down, Action::ResizeDown, ProgramFirst),
        (ctrl, KKey::Up, Action::JumpPrevPrompt, WhenKettleIdle),
        (ctrl, KKey::Down, Action::JumpNextPrompt, WhenKettleIdle),
        (shift, KKey::PageUp, Action::ScrollPageUp, WhenKettleIdle),
        (
            shift,
            KKey::PageDown,
            Action::ScrollPageDown,
            WhenKettleIdle,
        ),
        (cs, KKey::Up, Action::ScrollLineUp, WhenKettleIdle),
        (cs, KKey::Down, Action::ScrollLineDown, WhenKettleIdle),
        (ctrl, KKey::Home, Action::ScrollToTop, WhenKettleIdle),
        (ctrl, KKey::End, Action::ScrollToBottom, WhenKettleIdle),
        (shift, KKey::Home, Action::SelectToTop, WhenKettleIdle),
        (shift, KKey::End, Action::SelectToBottom, WhenKettleIdle),
        (ctrl, KKey::PageDown, Action::NextTab, WhenKettleIdle),
        (ctrl, KKey::PageUp, Action::PrevTab, WhenKettleIdle),
        (cs, KKey::PageDown, Action::MoveTabRight, WhenKettleIdle),
        (cs, KKey::PageUp, Action::MoveTabLeft, WhenKettleIdle),
        (
            Mods::ALT,
            KKey::Char('3'),
            Action::GotoTab(2),
            WhenKettleIdle,
        ),
        (
            cs,
            KKey::Char('n'),
            Action::FocusNext,
            WhenKettleIdleAndDistinct,
        ),
        (
            cs,
            KKey::Char('p'),
            Action::FocusPrev,
            WhenKettleIdleAndDistinct,
        ),
    ] {
        for macos in [false, true] {
            assert_eq!(
                program_key_rule(Trigger::new(mods, key), &action, macos),
                Some(rule),
                "{mods:?}+{key:?} -> {action:?}"
            );
        }
    }
    // macOS's directional focus chords, and only there: Alt+Arrow
    // elsewhere has its own adaptive rule.
    for mods in [Mods::ALT | Mods::SUPER, Mods::CTRL | Mods::SUPER] {
        for (key, action) in [
            (KKey::Up, Action::FocusUp),
            (KKey::Down, Action::FocusDown),
            (KKey::Left, Action::FocusLeft),
            (KKey::Right, Action::FocusRight),
        ] {
            let trigger = Trigger::new(mods, key);
            assert_eq!(
                program_key_rule(trigger, &action, true),
                Some(WhenKettleIdleAndDistinct),
                "{mods:?}+{key:?}"
            );
            assert_eq!(program_key_rule(trigger, &action, false), None);
        }
    }
    for (mods, key, action) in [
        (Mods::ALT, KKey::Left, Action::FocusLeft),
        (Mods::CTRL | Mods::ALT, KKey::Left, Action::FocusLeft),
        (Mods::ALT | Mods::SUPER, KKey::Left, Action::FocusRight),
        (cs, KKey::Char('n'), Action::FocusPrev),
        (ctrl, KKey::Char('n'), Action::FocusNext),
        (cs, KKey::Char('x'), Action::ToggleZoom),
    ] {
        for macos in [false, true] {
            assert_eq!(
                program_key_rule(Trigger::new(mods, key), &action, macos),
                None,
                "{mods:?}+{key:?} -> {action:?}"
            );
        }
    }
    // Other actions on the same chords, and the same actions elsewhere.
    for (mods, key, action) in [
        (shift, KKey::Left, Action::FocusLeft),
        (cs, KKey::Left, Action::ResizeLeft),
        (Mods::ALT, KKey::Left, Action::ResizeLeft),
        (ctrl, KKey::Up, Action::ScrollLineUp),
        (ctrl, KKey::Char('c'), Action::Copy),
        (cs, KKey::Char('c'), Action::Copy),
        (Mods::SUPER, KKey::Char('c'), Action::Copy),
        (shift, KKey::Home, Action::ScrollToTop),
        (ctrl, KKey::Char('-'), Action::DecreaseFontSize),
        (cs, KKey::Char('e'), Action::SplitRight),
    ] {
        for macos in [false, true] {
            assert_eq!(
                program_key_rule(Trigger::new(mods, key), &action, macos),
                None
            );
        }
    }
    // Platform chords: Cmd on macOS, Alt digits elsewhere, digits that
    // match their tab only.
    let cmd = Mods::SUPER;
    assert_eq!(
        program_key_rule(Trigger::new(cmd, KKey::Up), &Action::JumpPrevPrompt, true),
        Some(WhenKettleIdle)
    );
    assert_eq!(
        program_key_rule(Trigger::new(cmd, KKey::Up), &Action::JumpPrevPrompt, false),
        None
    );
    assert_eq!(
        program_key_rule(
            Trigger::new(cmd, KKey::Char('3')),
            &Action::GotoTab(2),
            false
        ),
        None
    );
    assert_eq!(
        program_key_rule(
            Trigger::new(cmd, KKey::Char('3')),
            &Action::GotoTab(2),
            true
        ),
        Some(WhenKettleIdle)
    );
    assert_eq!(
        program_key_rule(
            Trigger::new(Mods::ALT, KKey::Char('3')),
            &Action::GotoTab(0),
            false
        ),
        None
    );
}

/// Every rule names a chord this platform actually ships, and every
/// shipped chord that should yield has its rule, so a change to either
/// side cannot leave a rule matching nothing or a default without one.
#[test]
fn every_program_key_rule_is_a_shipped_default() {
    let macos = cfg!(target_os = "macos");
    // Labels name Alt and Super the way the platform's keyboard does.
    let (alt, cmd) = if macos {
        ("Opt", "Cmd")
    } else {
        ("Alt", "Super")
    };
    let defaults = kettle_config::keybinds::defaults();
    let mut yielding: Vec<String> = defaults
        .iter()
        .filter(|(trigger, action)| super::program_key_rule(**trigger, action, macos).is_some())
        .map(|(trigger, _)| trigger.label())
        .collect();
    yielding.sort();
    let mut expected: Vec<String> = [
        "Shift+Left",
        "Shift+Right",
        "Shift+Up",
        "Shift+Down",
        "Ctrl+Up",
        "Ctrl+Down",
        "Ctrl+Home",
        "Ctrl+End",
        "Shift+Home",
        "Shift+End",
        "Shift+PageUp",
        "Shift+PageDown",
        "Ctrl+Shift+Up",
        "Ctrl+Shift+Down",
        "Ctrl+PageUp",
        "Ctrl+PageDown",
        "Ctrl+Shift+PageUp",
        "Ctrl+Shift+PageDown",
        "Ctrl+Shift+N",
        "Ctrl+Shift+P",
    ]
    .iter()
    .map(|chord| chord.to_string())
    .chain((1..=9).map(|n| format!("{alt}+{n}")))
    .chain(
        macos
            .then(|| {
                [format!("{cmd}+Up"), format!("{cmd}+Down")]
                    .into_iter()
                    .chain((1..=9).map(|n| format!("{cmd}+{n}")))
                    .chain(
                        [format!("{alt}+{cmd}"), format!("Ctrl+{cmd}")]
                            .into_iter()
                            .flat_map(|m| {
                                ["Up", "Down", "Left", "Right"]
                                    .into_iter()
                                    .map(move |k| format!("{m}+{k}"))
                            }),
                    )
            })
            .into_iter()
            .flatten(),
    )
    .collect();
    expected.sort();
    assert_eq!(yielding, expected);
}

/// The adaptive Alt+Arrow fall-through obeys K1's rule that a chord the
/// config binds itself is Kettle's: the method's `enabled` flag, which the
/// pure predicates below treat as off, includes the declared-trigger check.
#[test]
fn adaptive_focus_respects_a_chord_the_user_binds() {
    let src = &super::app_source();
    let method = src
        .split("fn adaptive_focus_chord_falls_through(")
        .nth(1)
        .and_then(|s| s.split("\n    fn chord_falls_through(").next())
        .expect("adaptive_focus_chord_falls_through body");
    assert!(
        method.contains("!self.cfg.keybinds_declared.contains(&trigger)"),
        "a declared trigger must disable the adaptive fall-through"
    );
    assert!(
        method.contains("adaptive_alt_focus_direction(trigger, action, enabled)"),
        "the declared check must gate the direction lookup"
    );
    use super::adaptive_alt_focus_falls_through;
    use kettle_config::{Action, Key as KKey, Mods, Trigger};
    let alt_left = Trigger::new(Mods::ALT, KKey::Left);
    assert!(!adaptive_alt_focus_falls_through(
        alt_left,
        &Action::FocusLeft,
        false,
        false
    ));
}

/// A pane-focus chord with nowhere to go (a zoomed or single-pane tab, or
/// no pane on that side) goes to a program that owns the keyboard, but
/// only while the view is at the bottom and the program's keyboard mode
/// tells the chord apart from another key.
#[test]
fn focus_chords_with_nowhere_to_go_yield_only_when_distinct() {
    use super::{KettleActionState, ProgramKeyRule::WhenKettleIdleAndDistinct, program_gets_chord};
    use kettle_config::Action;
    let owns = claims(true);
    let nowhere = KettleActionState {
        chord_is_distinct: true,
        ..one_tab()
    };
    for action in [
        Action::FocusNext,
        Action::FocusPrev,
        Action::FocusUp,
        Action::FocusDown,
        Action::FocusLeft,
        Action::FocusRight,
    ] {
        let gets = |c: &kettle_core::KeyboardClaims, k: &KettleActionState| {
            program_gets_chord(WhenKettleIdleAndDistinct, &action, c, k)
        };
        assert!(gets(&owns, &nowhere), "{action:?} with nowhere to go");
        // A visible pane to move to keeps the chord Kettle's.
        let somewhere = KettleActionState {
            focus_target_exists: true,
            ..nowhere
        };
        assert!(!gets(&owns, &somewhere), "{action:?} with a target");
        // A mode that would send it as another key, or as nothing.
        let indistinct = KettleActionState {
            chord_is_distinct: false,
            ..nowhere
        };
        assert!(!gets(&owns, &indistinct), "{action:?} indistinct");
        // No program, or a view scrolled into history.
        assert!(!gets(&claims(false), &nowhere), "{action:?} no program");
        let scrolled = kettle_core::KeyboardClaims {
            history_size: 50,
            display_offset: 10,
            ..owns
        };
        assert!(!gets(&scrolled, &nowhere), "{action:?} scrolled back");
    }
}

/// The encoder decides distinctness: plain legacy mode sends Ctrl+Shift+N
/// as Ctrl+N and a Command chord as nothing; the kitty protocol and
/// modifyOtherKeys level 2 send each as itself.
#[test]
fn a_chord_is_distinct_only_where_the_encoder_tells_it_apart() {
    use super::chord_reaches_program_distinctly as distinct;
    use kettle_config::{Key as KKey, Mods, Trigger};
    use kettle_core::TermMode;
    let ctrl_shift_n = Trigger::new(Mods::CTRL | Mods::SHIFT, KKey::Char('n'));
    let cmd_opt_left = Trigger::new(Mods::ALT | Mods::SUPER, KKey::Left);
    let ctrl_cmd_up = Trigger::new(Mods::CTRL | Mods::SUPER, KKey::Up);
    let legacy = TermMode::empty();
    let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
    let mok2 = TermMode::MODIFY_OTHER_KEYS_2;
    assert!(!distinct(ctrl_shift_n, legacy), "legacy sends Ctrl+N");
    assert!(distinct(ctrl_shift_n, kitty));
    assert!(distinct(ctrl_shift_n, mok2));
    assert!(
        !distinct(ctrl_shift_n, TermMode::MODIFY_OTHER_KEYS_1),
        "level 1 keeps Ctrl+N"
    );
    // A real press carries its Control text, which without disambiguation
    // or report-all sends it as legacy Ctrl+N even under kitty flags.
    for flags in [
        TermMode::REPORT_EVENT_TYPES,
        TermMode::REPORT_ALTERNATE_KEYS,
    ] {
        assert!(!distinct(ctrl_shift_n, flags), "{flags:?} alone");
    }
    assert!(distinct(ctrl_shift_n, TermMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(distinct(
        ctrl_shift_n,
        TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES
    ));
    // An arrow carries no text, so event types alone encode it as itself.
    assert!(distinct(cmd_opt_left, TermMode::REPORT_EVENT_TYPES));
    for chord in [cmd_opt_left, ctrl_cmd_up] {
        assert!(!distinct(chord, legacy), "legacy has no Command bytes");
        assert!(
            !distinct(chord, mok2),
            "modifyOtherKeys has no Command bytes"
        );
        assert!(distinct(chord, kitty), "kitty has a super bit");
    }
}

/// Shift+Arrow resize goes to a program that owns the keyboard, never to
/// a shell idle at its prompt or a pane with no program modes.
#[test]
fn program_first_chords_follow_keyboard_ownership() {
    use super::{ProgramKeyRule::ProgramFirst, program_gets_chord};
    use kettle_config::Action;
    let resize = Action::ResizeLeft;
    assert!(program_gets_chord(
        ProgramFirst,
        &resize,
        &claims(true),
        &one_tab()
    ));
    assert!(!program_gets_chord(
        ProgramFirst,
        &resize,
        &claims(false),
        &one_tab()
    ));
    let idle_prompt = kettle_core::KeyboardClaims {
        keyboard_protocol: true,
        at_prompt: true,
        ..kettle_core::KeyboardClaims::default()
    };
    assert!(!program_gets_chord(
        ProgramFirst,
        &resize,
        &idle_prompt,
        &one_tab()
    ));
}

/// The other chords go to the program only when Kettle's action would do
/// nothing, and never while the view is scrolled into history.
#[test]
fn idle_kettle_chords_yield_only_when_the_action_does_nothing() {
    use super::{KettleActionState, ProgramKeyRule::WhenKettleIdle, program_gets_chord};
    use kettle_config::Action;
    let owns = claims(true);
    let gets = |action: Action, c: &kettle_core::KeyboardClaims, k: &KettleActionState| {
        program_gets_chord(WhenKettleIdle, &action, c, k)
    };
    // Prompt jumps.
    assert!(gets(Action::JumpPrevPrompt, &owns, &one_tab()));
    let prompt_above = kettle_core::KeyboardClaims {
        prompt_above: true,
        ..owns
    };
    assert!(!gets(Action::JumpPrevPrompt, &prompt_above, &one_tab()));
    assert!(gets(Action::JumpNextPrompt, &prompt_above, &one_tab()));
    // Scrolling at the bottom: nothing to scroll on the alternate screen
    // or without history; with history, only the downward chords.
    let alt = kettle_core::KeyboardClaims {
        alt_screen: true,
        ..owns
    };
    let history = kettle_core::KeyboardClaims {
        history_size: 500,
        ..owns
    };
    for action in [
        Action::ScrollToTop,
        Action::ScrollPageUp,
        Action::ScrollLineUp,
    ] {
        assert!(gets(action.clone(), &alt, &one_tab()), "{action:?} alt");
        assert!(gets(action.clone(), &owns, &one_tab()), "{action:?} empty");
        assert!(!gets(action.clone(), &history, &one_tab()), "{action:?}");
    }
    for action in [
        Action::ScrollToBottom,
        Action::ScrollPageDown,
        Action::ScrollLineDown,
    ] {
        assert!(gets(action.clone(), &history, &one_tab()), "{action:?}");
    }
    // Scrolled into history: every chord stays Kettle's, even at the top
    // where scrolling up would do nothing, and even with nothing to jump
    // to; the key would otherwise snap the view to the bottom.
    let scrolled_to_top = kettle_core::KeyboardClaims {
        history_size: 500,
        display_offset: 500,
        ..owns
    };
    for action in [
        Action::ScrollToTop,
        Action::ScrollPageUp,
        Action::ScrollLineUp,
        Action::ScrollPageDown,
        Action::JumpPrevPrompt,
        Action::JumpNextPrompt,
        Action::SelectToTop,
        Action::NextTab,
        Action::GotoTab(0),
    ] {
        assert!(
            !gets(action.clone(), &scrolled_to_top, &one_tab()),
            "{action:?} while scrolled back"
        );
    }
    // Shift+Home/End: a selection to extend keeps them Kettle's on either
    // screen; with none they go to the program on either screen.
    let extending = KettleActionState {
        selection_exists: true,
        ..one_tab()
    };
    for screen in [&alt, &history] {
        assert!(gets(Action::SelectToTop, screen, &one_tab()));
        assert!(!gets(Action::SelectToTop, screen, &extending));
        assert!(!gets(Action::SelectToBottom, screen, &extending));
    }
    // Tabs.
    let two_tabs = KettleActionState {
        tabs: 2,
        ..one_tab()
    };
    assert!(gets(Action::NextTab, &owns, &one_tab()));
    assert!(!gets(Action::NextTab, &owns, &two_tabs));
    assert!(!gets(Action::MoveTabLeft, &owns, &two_tabs));
    assert!(
        gets(Action::GotoTab(0), &owns, &two_tabs),
        "already the current tab"
    );
    assert!(!gets(Action::GotoTab(1), &owns, &two_tabs));
    assert!(gets(Action::GotoTab(4), &owns, &two_tabs), "no such tab");
    // Chords without a rule never count as idle.
    assert!(!gets(Action::Copy, &owns, &one_tab()));
    // None of it without a program.
    assert!(!gets(Action::NextTab, &claims(false), &one_tab()));
}

/// Real keys and the `dispatch_keybind` diagnostic share one routing
/// decision; the search bar keeps only the adaptive focus rule, since the
/// program cannot receive a key while the bar has the keyboard.
#[test]
fn keyboard_and_control_routes_share_the_program_key_decision() {
    let src = production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_string()
    };
    assert!(
        body("chord_falls_through").contains("|| self.chord_goes_to_program(ws, trigger, action)"),
        "both routes must consult the program-key decision"
    );
    let decision = body("chord_goes_to_program");
    for guard in [
        "configured_program_key_rule(&self.cfg, trigger, action, cfg!(target_os = \"macos\"))",
        "ws.mux.is_broadcast_on()",
        "pane.read_only || pane.pty_input_failed()",
        "pane.term.keyboard_claims()",
        "program_gets_chord(rule, action, &claims, &kettle)",
    ] {
        assert!(
            decision.contains(guard),
            "chord_goes_to_program lost {guard}"
        );
    }
    assert_eq!(
        src.matches("self.chord_falls_through(ws, trigger, &")
            .count(),
        2
    );
    let search = src
        .split("fn search_bar_shortcut(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("search_bar_shortcut");
    assert!(search.contains("self.adaptive_focus_chord_falls_through(ws, trigger, &action)"));
    assert!(!search.contains("chord_falls_through(ws, trigger, &action) ||"));
    assert!(!search.contains("self.chord_falls_through("));
    assert_eq!(
        src.matches("self.adaptive_focus_chord_falls_through(ws, trigger, &action)")
            .count(),
        1,
        "only the search bar calls the adaptive rule alone"
    );
}

#[test]
fn keybind_chord_is_safe_rejects_modless_text_keys() {
    use super::keybind_chord_is_safe;
    use kettle_config::{Key as KKey, Mods};
    // Modifier-less text/essential keys are REFUSED (the soft-brick).
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Char('a')));
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Enter));
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Tab));
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Up));
    // A modifier-less F-key is fine (produces no text — F1=help etc.).
    assert!(keybind_chord_is_safe(Mods::empty(), KKey::F(1)));
    // Any modifier makes the chord safe.
    assert!(keybind_chord_is_safe(Mods::CTRL, KKey::Char('a')));
    assert!(keybind_chord_is_safe(Mods::ALT, KKey::Enter));
    assert!(keybind_chord_is_safe(Mods::SHIFT, KKey::Char('z')));
}

/// Drift guard. Rebinding a Settings keybind row to a chord already bound
/// to a DIFFERENT action must not silently steal it. Like the
/// modifier-safety check before it, which refuses to apply silently (it
/// notifies and stays in capture mode), the conflict check must ask first
/// instead of falling straight into `keybinds.insert`. A behavioral test
/// needs a live App + WindowState + Settings overlay, so this pins the
/// shape at the source level.
#[test]
fn keybind_rebind_conflict_is_gated_behind_confirmation() {
    let src = production_source();
    let capture_arm = src
        .split("if !keybind_chord_is_safe(mods, kk) {")
        .nth(1)
        .and_then(|s| s.split("\n        match key {").next())
        .expect("chord-capture body present");
    assert!(
        capture_arm.contains("self.cfg.keybinds.get(&trig).filter(|v| **v != act).cloned()"),
        "a conflicting existing binding must be looked up before applying the rebind"
    );
    let capture_arm_normalized = capture_arm.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        capture_arm_normalized.contains("self.install_confirm_dialog(")
            && capture_arm.contains("ConfirmAction::RebindKeybind {"),
        "a conflicting rebind must route through a confirm dialog, not apply immediately"
    );
    assert!(
        capture_arm.contains("self.apply_keybind_rebind(trig, act, action);"),
        "a non-conflicting rebind must still apply immediately via the shared helper"
    );
    // The confirm dialog's dispatch must finish the SAME way the
    // direct path does — no separate, divergent apply logic.
    let dispatch = src
        .split("fn dispatch_confirm_action_arms(")
        .nth(1)
        .and_then(|s| s.split("\n    }\n").next())
        .expect("dispatch_confirm_action_arms body present");
    assert!(
        dispatch.contains("ConfirmAction::RebindKeybind {")
            && dispatch.contains("self.apply_keybind_rebind(trig, act, action_name);"),
        "confirming a rebind must apply via apply_keybind_rebind, matching the direct path"
    );
}

/// A rebind saves its unbinds and its new binding in one edit. At the size
/// cap the whole edit is refused, so the old chord stays bound; saving the
/// unbind alone left the action with no chord at all after a reload.
#[test]
fn keybind_rebind_at_the_size_cap_leaves_the_config_unchanged() {
    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    let mut original = String::from("keybind = Ctrl+Alt+R=new_tab\n#");
    original.push_str(&"x".repeat(1024 * 1024 - original.len() - 1));
    original.push('\n');
    std::fs::write(&path, &original).unwrap();

    let stale = vec!["Ctrl+Alt+R".to_string()];
    assert!(!super::persist_keybind_rebind(
        Some(&path),
        &stale,
        "Ctrl+Alt+Y",
        "new_tab",
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}


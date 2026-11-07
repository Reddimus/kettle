use super::*;
use crate::quick_terminal::{Effect, Model};

#[test]
fn review_quick_cold_startup_hook_preserves_normal_session() {
    let cfg = Config {
        restore_session: kettle_config::RestoreSession::Always,
        ..tests::title_test_config()
    };
    let startup = crate::Options {
        quick_terminal: true,
        ..Default::default()
    };
    let model = Model::for_startup(startup.quick_terminal, 1);
    assert!(Model::for_startup(false, 1).window.is_none());
    let dir = kettle_test_support::private_tempdir("kettle-quick-startup-session-");
    let path = dir.path().join("session.json");
    let (normal_mux, _) = tests::title_test_tab_mux(1);
    let normal = WindowState::new(9, normal_mux);
    let saved =
        App::snapshot_session(&normal, std::iter::empty(), None, &VecDeque::from([9])).unwrap();
    crate::session::save_to_path(&saved, &path).unwrap();
    let before = std::fs::read(&path).unwrap();

    let (mux, _) = tests::title_test_tab_mux(1);
    let mut quick = WindowState::new(1, mux);
    let lua = crate::LuaEngine::new("Default").unwrap();
    lua.eval_str("kettle.on('startup', function() kettle.exec_action('split_right') end)")
        .unwrap();
    lua.fire_event(&crate::LuaEvent::Startup);
    let commands = lua.drain_commands();
    assert_eq!(commands.len(), 1);
    for command in commands {
        let crate::LuaCommand::ExecAction(name) = command else {
            panic!("expected startup action")
        };
        assert_eq!(Action::from_name(&name), Some(Action::SplitRight));
        quick
            .mux
            .split_with(
                Dir::Horizontal,
                &cfg,
                80,
                24,
                8,
                16,
                Arc::new(|| {}),
                tests::title_test_argv(),
                None,
            )
            .unwrap();
        assert_eq!(quick.mux.panes.len(), 2);
        assert!(should_restore_session(
            startup.restore,
            cfg.restore_session,
            false
        ));
        if let Some(snapshot) = App::snapshot_session(
            &quick,
            std::iter::empty(),
            model.window,
            &VecDeque::from([1]),
        ) {
            crate::session::save_to_path(&snapshot, &path).unwrap();
        }
    }
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "startup split overwrote the normal session"
    );
    // A normal window created later still saves, excluding the quick workspace.
    let snapshot = App::snapshot_session(
        &quick,
        std::iter::once(&normal),
        model.window,
        &VecDeque::from([1, 9]),
    )
    .unwrap();
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(snapshot.windows[0].tabs.len(), 1);

    let startup = include_str!("startup.rs");
    assert!(
        startup.contains(
            "model: crate::quick_terminal::Model::for_startup(startup.quick_terminal, 1)"
        )
    );
    let save = include_str!("sessions.rs");
    assert!(save.contains("let Some(s) = Self::snapshot_session("));
    let resumed = include_str!("../app.rs")
        .split_once("fn resumed(&mut self,")
        .unwrap()
        .1
        .split("fn user_event(")
        .next()
        .unwrap();
    assert!(
        resumed.contains(
            "let show_initial_quick = self.startup.quick_terminal && ws.window.is_none();"
        )
    );
    assert!(resumed.contains(
        "if show_initial_quick && !self.quick_terminal.model.visible && self.gpu.is_some()"
    ));
}

#[test]
fn review_quick_reopen_after_hide_tracks_visibility_and_autohide() {
    let mut model = Model::default();
    model.window = Some(1);
    assert_eq!(model.toggle(true), Effect::Show);
    assert_eq!(model.toggle(false), Effect::Hide { return_focus: true });
    assert_eq!(model.reveal(true), Effect::Show);
    assert!(
        model.visible,
        "reopen revealed the native window but left the model hidden"
    );
    assert_eq!(model.reveal(false), Effect::None);
    assert_eq!(model.toggle(false), Effect::Hide { return_focus: true });
    assert_eq!(model.reveal(true), Effect::Show);
    assert_eq!(
        model.focus_lost(true, true),
        Effect::Hide {
            return_focus: false
        }
    );
    assert!(!model.visible);

    let focus = include_str!("windowing.rs")
        .split("fn focus_or_open_window(")
        .nth(1)
        .unwrap()
        .split("fn drain_activation_requests(")
        .next()
        .unwrap();
    assert!(focus.contains("self.toggle_quick_terminal(event_loop)"));
    assert!(focus.contains("self.reveal_window(&mut ws)"));
    let reveal = include_str!("quick_terminal.rs")
        .split("fn reveal_window(")
        .nth(1)
        .unwrap()
        .split("fn toggle_window_visibility(")
        .next()
        .unwrap();
    assert!(
        reveal
            .find("self.quick_terminal.model.reveal(can_return)")
            .unwrap()
            < reveal.find("window.set_visible(true)").unwrap()
    );
    for (source, call) in [
        (include_str!("output.rs"), "self.reveal_window(ws)"),
        (include_str!("ctl.rs"), "self.reveal_window(ws)"),
        (include_str!("windowing.rs"), "self.reveal_window(target)"),
        (
            include_str!("actions.rs"),
            "self.toggle_window_visibility(ws)",
        ),
        (
            include_str!("automation.rs"),
            "self.toggle_window_visibility(ws)",
        ),
    ] {
        assert!(source.contains(call));
    }
}

#[test]
fn review_s4_capture_composing_option_a() {
    use kettle_config::keybinds::{Key as K, Mods, Trigger};
    let mut ws = WindowState::new(1, Mux::new());
    #[cfg(target_os = "macos")]
    {
        ws.macos_raw_mods = ModifiersState::ALT;
        ws.mods = macos_effective_modifiers(
            ModifiersState::ALT,
            kettle_config::MacosOptionAsAlt::None,
            false,
            false,
        );
        assert!(ws.mods.is_empty());
    }
    #[cfg(not(target_os = "macos"))]
    {
        ws.mods = ModifiersState::empty();
    }
    let composed = Key::Character("\u{e5}".into());
    let unmodified = Key::Character("a".into());
    assert_eq!(
        super::modals::capture_global_hotkey(
            &composed,
            Some(&unmodified),
            ModifiersState::ALT,
            ws.mods,
        ),
        crate::settings::HotkeyCapture::Value(Some(Trigger::new(Mods::ALT, K::Char('a'))))
    );
    let shifted_mods = ModifiersState::ALT | ModifiersState::SHIFT;
    assert_eq!(
        super::modals::capture_global_hotkey(
            &Key::Character("\u{c5}".into()),
            Some(&unmodified),
            shifted_mods,
            ModifiersState::SHIFT,
        ),
        crate::settings::HotkeyCapture::Value(Some(Trigger::new(
            Mods::ALT | Mods::SHIFT,
            K::Char('a'),
        )))
    );
    // Outside composing Option, keep the symbol actually typed, even when
    // the layout's unmodified character is different.
    assert_eq!(
        super::modals::capture_global_hotkey(
            &Key::Character("+".into()),
            Some(&Key::Character("=".into())),
            shifted_mods,
            shifted_mods,
        ),
        crate::settings::HotkeyCapture::Value(Some(Trigger::new(
            Mods::ALT | Mods::SHIFT,
            K::Char('+'),
        )))
    );
}

#[test]
fn review_quick_capture_preserves_physical_option() {
    use kettle_config::keybinds::{Key as K, Mods, Trigger};
    use winit::keyboard::ModifiersState as M;
    let mut ws = WindowState::new(1, Mux::new());
    #[cfg(target_os = "macos")]
    {
        ws.macos_raw_mods = M::ALT;
        ws.mods =
            macos_effective_modifiers(M::ALT, kettle_config::MacosOptionAsAlt::None, false, false);
        assert!(ws.mods.is_empty());
    }
    #[cfg(not(target_os = "macos"))]
    {
        ws.mods = M::ALT;
    }
    for (key, expected) in [
        (NamedKey::Escape, K::Escape),
        (NamedKey::Backspace, K::Backspace),
    ] {
        assert_eq!(
            super::modals::capture_global_hotkey(
                &Key::Named(key),
                None,
                held_modifiers(&ws),
                ws.mods,
            ),
            crate::settings::HotkeyCapture::Value(Some(Trigger::new(Mods::ALT, expected)))
        );
    }
    let source = include_str!("modals.rs");
    assert!(
        source.contains(
            "match capture_global_hotkey(key, unmodified_key, held_modifiers(ws), ws.mods)"
        )
    );
}

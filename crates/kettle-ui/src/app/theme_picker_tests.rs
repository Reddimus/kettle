use super::{
    begin_theme_picker, end_modal_composition, modal_accessibility_projection,
    picker_overlay_context_menu, production_source, theme_picker_list, theme_picker_matches,
    theme_picker_selection, theme_picker_start, theme_preview_change,
};
use crate::mux::Mux;
use crate::window_state::{ImePreeditOwner, ImePreeditSession, ThemePickerState, WindowState};
use kettle_config::Theme;
use kettle_i18n::{Language, Translator};

const EN: Translator = Translator::new(Language::En);
const ES: Translator = Translator::new(Language::Es);

fn state(query: &str, selected: usize, opened_on: &str) -> ThemePickerState {
    ThemePickerState {
        query: query.into(),
        selected,
        opened_on: opened_on.to_string(),
    }
}

/// With no query the picker lists the opening theme's appearance first, in
/// look order, and the other appearance after it, so the arrows step
/// between similar palettes and cross from dark to light only at the end.
#[test]
fn an_empty_query_lists_the_opening_appearance_first_in_look_order() {
    for opened_on in ["TokyoNight Night", "TokyoNight Day"] {
        let dark = Theme::by_name(opened_on).is_dark();
        let matches = theme_picker_matches("", opened_on, &EN);
        let names: Vec<&str> = matches.iter().map(|(name, _)| *name).collect();
        let first = Theme::look_order(dark);
        assert_eq!(names.len(), Theme::list().len());
        assert_eq!(&names[..first.len()], first, "{opened_on}");
        assert_eq!(
            &names[first.len()..],
            Theme::look_order(!dark),
            "{opened_on}"
        );
        for (name, is_dark) in &matches {
            assert_eq!(Theme::by_name(name).is_dark(), *is_dark, "{name}");
        }
        assert_eq!(names[theme_picker_start(opened_on)], opened_on);
        // A blank query is no query.
        assert_eq!(theme_picker_matches("   ", opened_on, &EN), matches);
    }
}

#[test]
fn a_query_ranks_matching_themes_and_says_when_none_match() {
    // "Dracula+" scores the same and comes first in look order, but the
    // theme the query names exactly wins.
    let matches = theme_picker_matches("dracula", "TokyoNight Night", &EN);
    assert_eq!(matches.first().map(|(name, _)| *name), Some("Dracula"));
    assert!(matches.iter().any(|(name, _)| *name == "Dracula+"));
    for (name, _) in &matches {
        assert!(
            kettle_config::fuzzy::score("dracula", name).is_some(),
            "{name}"
        );
    }
    assert!(matches.len() < Theme::list().len());

    let none = theme_picker_list("zzqqxxjj", 3, "TokyoNight Night", &ES);
    assert_eq!(none.rows.len(), 1);
    assert_eq!(none.rows[0].label, "Ningún tema coincide");
    assert!(!none.rows[0].enabled);
    assert_eq!(
        theme_picker_list("zzqqxxjj", 0, "TokyoNight Night", &EN).rows[0].label,
        "No matching theme"
    );
    assert_eq!(
        theme_picker_selection(&state("zzqqxxjj", 3, "TokyoNight Night"), &EN),
        None
    );
}

/// The theme the picker opened on keeps its tick while the preview changes
/// the live theme, and every row names its appearance.
#[test]
fn rows_tick_the_opening_theme_and_name_their_appearance() {
    let list = theme_picker_list("tokyonight", 0, "TokyoNight Night", &EN);
    assert!(list.rows.len() > 1);
    for row in &list.rows {
        // Hints are padded on the left to one width (the swatch column).
        let hint = row.hint.trim_start();
        let appearance = hint.strip_prefix("✓ ").unwrap_or(hint);
        assert_eq!(hint.starts_with("✓ "), row.label == "TokyoNight Night");
        let expected = if Theme::by_name(&row.label).is_dark() {
            "dark"
        } else {
            "light"
        };
        assert_eq!(appearance, expected, "{}", row.label);
    }
    let es = theme_picker_list("tokyonight", 0, "TokyoNight Night", &ES);
    let hint = |name: &str| {
        es.rows
            .iter()
            .find(|row| row.label == name)
            .map(|row| row.hint.trim_start())
    };
    assert_eq!(hint("TokyoNight Night"), Some("✓ oscuro"));
    assert_eq!(hint("TokyoNight Day"), Some("claro"));
}

fn live(name: &str) -> (String, Theme) {
    (name.to_string(), Theme::by_name(name))
}

/// Apply the preview's next change as the App would, returning the theme
/// the window then wears.
fn settle(ws: &mut WindowState, wearing: &str) -> String {
    let cfg = kettle_config::Config {
        theme_name: wearing.to_string(),
        ..Default::default()
    };
    theme_preview_change(ws, &cfg, &EN)
        .map(|(name, _)| name)
        .unwrap_or_else(|| wearing.to_string())
}

/// The picker previews its selection, shows its opening theme while
/// nothing matches without dropping it, and puts it back on Esc.
/// Reopening over a running preview opens on the theme from before it.
#[test]
fn the_preview_restores_the_theme_the_picker_opened_on() {
    let opened = "TokyoNight Night";
    let mut ws = WindowState::new(0, Mux::new());
    assert!(begin_theme_picker(&mut ws, live(opened)).is_none());
    let mut wearing = settle(&mut ws, opened);
    assert_eq!(wearing, opened, "it opens on the current theme");

    ws.theme_picker_input.as_mut().unwrap().selected += 1;
    wearing = settle(&mut ws, &wearing);
    let previewed = wearing.clone();
    assert_ne!(previewed, opened, "a step previews the next theme");

    // Opening again (from the palette, say) cancels the running preview.
    ws.theme_picker_input = None;
    let restore = begin_theme_picker(&mut ws, live(&wearing));
    assert_eq!(restore.map(|(name, _)| name).as_deref(), Some(opened));
    wearing = opened.to_string();
    let state = ws.theme_picker_input.as_ref().unwrap();
    assert_eq!(
        state.opened_on, opened,
        "the new picker ticks the theme from before"
    );
    assert_eq!(theme_picker_selection(state, &EN), Some(opened));

    // Nothing matches: the window wears the opening theme again, and the
    // baseline survives for Esc even if something else changes the theme.
    ws.theme_picker_input.as_mut().unwrap().selected += 1;
    wearing = settle(&mut ws, &wearing);
    let state = ws.theme_picker_input.as_mut().unwrap();
    state.query = "zzqqxxjj".into();
    state.selected = 0;
    wearing = settle(&mut ws, &wearing);
    assert_eq!(wearing, opened);
    assert_eq!(settle(&mut ws, "Dracula"), opened, "the baseline was kept");

    // Esc: the picker closes and the opening theme comes back, once.
    ws.theme_picker_input = None;
    assert_eq!(settle(&mut ws, &previewed), opened);
    assert!(ws.theme_preview.is_none());
    assert!(
        theme_preview_change(
            &mut ws,
            &kettle_config::Config {
                theme_name: opened.to_string(),
                ..Default::default()
            },
            &EN
        )
        .is_none()
    );
}

/// Enter keeps the previewed theme: nothing puts the opening one back.
#[test]
fn a_kept_theme_stays() {
    let mut ws = WindowState::new(0, Mux::new());
    begin_theme_picker(&mut ws, live("TokyoNight Night"));
    ws.theme_picker_input.as_mut().unwrap().selected += 1;
    let kept = settle(&mut ws, "TokyoNight Night");
    // `commit_theme` drops the baseline, then the picker closes.
    ws.theme_preview = None;
    ws.theme_picker_input = None;
    assert!(
        theme_preview_change(
            &mut ws,
            &kettle_config::Config {
                theme_name: kept.to_string(),
                ..Default::default()
            },
            &EN
        )
        .is_none()
    );
}

/// A composition a closing modal owned ends, so a late Commit cannot type
/// into what it covered; the owner marker stays to recognise it.
#[test]
fn closing_a_modal_ends_its_composition() {
    let mut ws = WindowState::new(0, Mux::new());
    ws.ime_preedit = Some(("ka".into(), None));
    ws.ime_preedit_owner = Some(ImePreeditSession {
        ended: None,
        empty_after_focus: false,
        owner: ImePreeditOwner::Other,
        generation: ws.ime_focus_generation,
    });
    let before = ws.ime_focus_generation;
    end_modal_composition(&mut ws);
    assert_ne!(ws.ime_focus_generation, before);
    assert!(ws.ime_preedit.is_none());
    assert!(ws.ime_preedit_owner.is_some());

    // Every key that closes a modal ends its composition there, on the
    // real keyboard path and on the control plane's.
    let src = production_source();
    let remember = src
        .split("fn remember_closing_key(")
        .nth(1)
        .and_then(|body| body.split("\n}").next())
        .expect("remember_closing_key");
    assert!(remember.contains("end_modal_composition(ws);"));
    let dispatch = src
        .split("if open_text_modal(ws) != Some(modal) {")
        .nth(1)
        .and_then(|body| body.split("break;").next())
        .expect("the control plane stops when its modal closes");
    assert!(dispatch.contains("end_modal_composition(ws);"));
    // Holding the Enter that opened the picker from Settings must not
    // pick a theme: its repeats are gated like the palette's.
    let branch = src
        .split(
            "if ws.theme_picker_input.is_some() {\n                    if !modal_repeat_is_inert(",
        )
        .nth(1)
        .and_then(|rest| rest.split("return;").next())
        .expect("the theme picker's key branch gates repeats");
    assert!(branch.contains("ModalRepeat::Palette,"));
    assert!(branch.contains("self.theme_picker_key(ws, &event.logical_key, text);"));
}

/// Screen readers hear the picker's name in the UI's language, and each
/// row's appearance (and the opening theme's tick) as its description.
#[test]
fn the_picker_reads_out_its_name_and_each_rows_appearance() {
    let projection = |tr: &Translator| {
        let list = theme_picker_list("tokyonight", 0, "TokyoNight Night", tr);
        let overlay = kettle_render::Overlay {
            theme_picker_query: Some("tokyonight".into()),
            context_menu: picker_overlay_context_menu(
                [None, Some(list), None, None],
                (800.0, 600.0),
                (8.0, 16.0),
                0.0,
            ),
            ..kettle_render::Overlay::default()
        };
        modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, tr)
    };
    let es = projection(&ES);
    assert!(
        es.nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Selector de temas"))
    );
    let described = |name: &str| {
        es.nodes
            .iter()
            .find(|(_, node)| node.label() == Some(name))
            .and_then(|(_, node)| node.description().map(str::to_string))
    };
    assert_eq!(described("TokyoNight Night").as_deref(), Some("✓ oscuro"));
    assert_eq!(described("TokyoNight Day").as_deref(), Some("claro"));
    assert!(
        projection(&EN)
            .nodes
            .iter()
            .any(|(_, node)| node.label() == Some("Theme picker"))
    );
}

/// The preview and Enter use the row the list highlights, including when
/// the selection is past the end of a narrowed list.
#[test]
fn the_selection_is_the_highlighted_row() {
    let opened_on = "TokyoNight Night";
    for (query, selected) in [
        ("", theme_picker_start(opened_on) + 1),
        ("tokyo", 2),
        ("tokyo", 999),
    ] {
        let picked = theme_picker_selection(&state(query, selected, opened_on), &EN);
        let list = theme_picker_list(query, selected, opened_on, &EN);
        assert_eq!(
            picked,
            Some(list.rows[list.selected].label.as_str()),
            "{query:?} {selected}"
        );
    }
    assert_eq!(
        theme_picker_selection(&state("", theme_picker_start(opened_on), opened_on), &EN),
        Some(opened_on)
    );
}

#[test]
fn builtin_theme_picker_localizes_labels_and_keeps_selection_identifiers() {
    for (name, label) in [
        ("Builtin Dark", "Oscuro integrado"),
        ("Builtin Light", "Claro integrado"),
    ] {
        let list = theme_picker_list(name, 0, name, &ES);
        assert_eq!(list.rows[0].label, label);
        assert!(list.rows[0].hint.starts_with("✓ "));
        assert_eq!(
            theme_picker_selection(&state(name, 0, name), &EN),
            Some(name)
        );
    }
}

#[test]
fn picker_scroll_strips_leave_the_selected_row_fully_visible() {
    let list = theme_picker_list("", 20, "Builtin Dark", &EN);
    let menu = super::picker_context_menu(list, (320.0, 160.0), (8.0, 16.0), 0.0).unwrap();
    let (_, content_h) = kettle_render::menu::scroll_content(
        menu.rows.len() as f32 * 28.0,
        menu.panel_h_clamped,
        28.0,
    );
    let visible = (content_h / 28.0).floor() as usize;
    assert!((menu.scroll_offset..menu.scroll_offset + visible).contains(&menu.highlight));
}

#[test]
fn regression_translated_theme_names_find_canonical_selections() {
    for (name, label) in [
        ("Builtin Dark", "Oscuro integrado"),
        ("Builtin Light", "Claro integrado"),
    ] {
        for query in [label.to_string(), label.to_uppercase(), name.to_string()] {
            let list = theme_picker_list(&query, 0, name, &ES);
            assert!(list.rows[0].enabled, "no match for {query}");
            assert_eq!(list.rows[0].label, label);
            assert!(list.rows[0].hint.starts_with("✓ "));
            assert_eq!(
                theme_picker_selection(&state(&query, 0, name), &ES),
                Some(name)
            );
        }
    }
}

#[test]
fn regression_picker_scroll_strips_have_pointer_and_wheel_routes() {
    let source = production_source();
    let motion = source
        .split("WindowEvent::CursorMoved")
        .last()
        .unwrap()
        .split("WindowEvent::MouseInput")
        .next()
        .unwrap();
    assert!(
        motion.contains("self.picker_pointer"),
        "picker arrows need a hover route"
    );
    let press = source
        .split("let bcode = match button {")
        .nth(1)
        .unwrap()
        .split("WindowEvent::MouseWheel")
        .next()
        .unwrap();
    assert!(
        press.contains("self.picker_pointer"),
        "picker arrows need a click route"
    );
    let wheel = include_str!("pointer.rs")
        .split("fn dispatch_wheel(")
        .nth(1)
        .unwrap();
    assert!(
        wheel.contains("self.picker_pointer"),
        "picker needs a wheel route before modal swallow"
    );
    let ctl = include_str!("ctl.rs")
        .split("fn ctl_mouse_press(")
        .nth(1)
        .unwrap();
    assert!(
        ctl.contains("self.picker_pointer"),
        "synthetic presses share the picker route"
    );
}

#[test]
fn picker_arrows_and_wheel_scroll_all_four_modal_projections() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config {
        ssh_hosts: (0..40)
            .map(|i| (format!("host-{i}"), format!("ssh host-{i}")))
            .collect(),
        ..Default::default()
    };
    let surface = (320.0, 160.0);
    let cell = (8.0, 16.0);
    let project = |ws: &WindowState| {
        picker_context_menu(
            open_picker_list(ws, &cfg, &ES, None).unwrap(),
            surface,
            cell,
            0.0,
        )
        .unwrap()
    };
    for kind in 0..4 {
        let mut ws = WindowState::new(0, Mux::new());
        match kind {
            0 => ws.palette_input = Some((Default::default(), 0)),
            1 => ws.theme_picker_input = Some(state("", 0, "Builtin Dark")),
            2 => {
                ws.layout_picker_entries = (0..40).map(|i| format!("layout-{i}")).collect();
                ws.layout_picker_input = Some((Default::default(), 0));
            }
            _ => ws.ssh_input = Some((Default::default(), 0)),
        }
        assert!(ws.context_menu.is_none());
        let menu = project(&ws);
        assert_eq!(menu.scroll_offset, 0);
        ws.cursor.x = f64::from(menu.anchor.0 + menu.panel_w_clamped * 0.5);
        ws.cursor.y = f64::from(menu.anchor.1 + menu.panel_h_clamped - 0.25);
        assert!(apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Press
        ));
        let menu = project(&ws);
        assert_eq!(menu.scroll_offset, 1, "picker {kind}: bottom click");
        assert!(apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Hover
        ));
        let menu = project(&ws);
        assert_eq!(menu.scroll_offset, 2, "picker {kind}: bottom hover");
        ws.cursor.y = f64::from(menu.anchor.1 + 0.25);
        assert!(apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Hover
        ));
        let menu = project(&ws);
        assert_eq!(menu.scroll_offset, 1, "picker {kind}: top hover");
        assert!(apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Wheel(3)
        ));
        let menu = project(&ws);
        assert_eq!(menu.scroll_offset, 4, "picker {kind}: wheel down");
        assert!(apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Wheel(-100)
        ));
        let menu = project(&ws);
        assert_eq!(menu.scroll_offset, 0, "picker {kind}: wheel up clamps");
        assert!(apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Press
        ));
        assert_eq!(
            project(&ws).scroll_offset,
            0,
            "disabled top arrow consumes press"
        );
        ws.cursor.x = f64::from(menu.anchor.0 - 1.0);
        assert!(!apply_picker_pointer(
            &mut ws,
            &menu,
            cell.1,
            PickerPointer::Press
        ));
        assert!(
            open_picker_list(&ws, &cfg, &ES, None).is_some(),
            "picker stays open"
        );
        assert!(ws.context_menu.is_none());
    }
}

#[test]
fn translated_theme_query_previews_its_canonical_theme() {
    for (label, canonical) in [
        ("Oscuro integrado", "Builtin Dark"),
        ("Claro integrado", "Builtin Light"),
    ] {
        let mut ws = WindowState::new(0, Mux::new());
        begin_theme_picker(&mut ws, live("Dracula"));
        ws.theme_picker_input.as_mut().unwrap().query = label.into();
        let cfg = kettle_config::Config {
            theme_name: "Dracula".into(),
            ..Default::default()
        };
        let (preview, _) = theme_preview_change(&mut ws, &cfg, &ES).unwrap();
        assert_eq!(preview, canonical);
    }
}

#[test]
fn closing_a_preview_owner_restores_the_shared_baseline() {
    let source = production_source();
    let finish = source.split("fn finish_window_dispatch(").nth(1).unwrap();
    let close = finish
        .split("if take_window_close_request(")
        .nth(1)
        .unwrap();
    assert!(close.find("cancel_theme_preview(&mut ws)").unwrap() < close.find("drop(ws)").unwrap());
    let mut owner = WindowState::new(1, Mux::new());
    let mut survivor = WindowState::new(2, Mux::new());
    let baseline = (
        "TokyoNight Night".to_string(),
        Theme::by_name("TokyoNight Night"),
    );
    begin_theme_picker(&mut owner, baseline.clone());
    owner.theme_picker_input.as_mut().unwrap().query = "Dracula".into();
    let preview = theme_preview_change(
        &mut owner,
        &kettle_config::Config {
            theme_name: baseline.0.clone(),
            ..Default::default()
        },
        &EN,
    )
    .unwrap();
    assert_ne!(preview.0, baseline.0);
    let restored =
        super::cancel_theme_preview(&mut owner).expect("closing must restore the preview baseline");
    assert!(owner.theme_picker_input.is_none());
    assert!(owner.theme_preview.is_none());
    begin_theme_picker(&mut survivor, restored);
    assert_eq!(survivor.theme_preview.as_ref().unwrap().0, baseline.0);
}

#[test]
fn picker_result_presses_select_the_visible_row() {
    use super::{PickerPointer, apply_picker_pointer, open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config {
        ssh_hosts: vec![
            ("alpha".into(), "ssh alpha".into()),
            ("beta".into(), "ssh beta".into()),
        ],
        ..Default::default()
    };
    for kind in 0..4 {
        let mut ws = WindowState::new(0, Mux::new());
        match kind {
            0 => ws.palette_input = Some((Default::default(), 0)),
            1 => ws.theme_picker_input = Some(state("", 0, "Builtin Dark")),
            2 => {
                ws.layout_picker_entries = vec!["alpha".into(), "beta".into()];
                ws.layout_picker_input = Some((Default::default(), 0));
            }
            _ => ws.ssh_input = Some((Default::default(), 0)),
        }
        let project = |ws: &WindowState| {
            picker_context_menu(
                open_picker_list(ws, &cfg, &ES, None).unwrap(),
                (800.0, 600.0),
                (8.0, 16.0),
                0.0,
            )
            .unwrap()
        };
        let menu = project(&ws);
        assert_eq!(menu.anchor, (176.0, 78.0));
        assert_eq!(menu.picker_header_height, 30.0);
        let row_h = 16.0 + kettle_render::menu::ROW_PAD;
        let (inset, _) = kettle_render::menu::scroll_content(
            menu.rows.len() as f32 * row_h,
            menu.panel_h_clamped,
            row_h,
        );
        ws.cursor.x = f64::from(menu.anchor.0 + 10.0);
        ws.cursor.y = f64::from(menu.anchor.1 + inset + row_h * 1.5);
        assert!(
            apply_picker_pointer(&mut ws, &menu, 16.0, PickerPointer::Press),
            "picker result rows must accept primary presses"
        );
        assert_eq!(
            project(&ws).highlight,
            1,
            "the pressed row must become selected"
        );
    }
}

#[test]
fn picker_result_activation_uses_native_and_control_enter_routing() {
    for source in [include_str!("window_events.rs"), include_str!("ctl.rs")] {
        let code = kettle_test_support::code_only(source)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(code.contains("PickerPointer::Press, Some(event_loop)"));
    }
    let body = kettle_test_support::code_only(include_str!("pickers.rs"));
    for handler in [
        "self.palette_key(ws, &key, None, event_loop)",
        "self.theme_picker_key(ws, &key, None)",
        "self.layout_picker_key(ws, &key, None)",
        "self.ssh_key(ws, &key, None)",
    ] {
        assert!(
            body.contains(handler),
            "picker activation must share {handler}"
        );
    }
}

#[test]
fn theme_pick_persists_the_active_side_independently_of_preview_lightness() {
    use kettle_config::{Config, ThemeMode, persist_config_toggle};
    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    for (mode, os, key) in [
        (ThemeMode::Auto, Some(false), "light-theme"),
        (ThemeMode::Auto, Some(true), "dark-theme"),
        (ThemeMode::Auto, None, "dark-theme"),
        (ThemeMode::Light, Some(true), "light-theme"),
        (ThemeMode::Dark, Some(false), "dark-theme"),
        (ThemeMode::Explicit, Some(false), "theme"),
    ] {
        let token = match mode {
            ThemeMode::Auto => "auto",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
            ThemeMode::Explicit => "explicit",
        };
        std::fs::write(&path, format!("theme = Dracula\ntheme-mode = {token}\n")).unwrap();
        let mut cfg = Config::parse_text(&std::fs::read_to_string(&path).unwrap());
        // A preview can be either light or dark; it must never change the destination.
        for preview in ["TokyoNight Day", "TokyoNight Night"] {
            cfg.theme = cfg.theme_by_name(preview);
            assert_eq!(super::theme_pick_key(&cfg, os), key);
        }
        persist_config_toggle(&path, key, "Alabaster").unwrap();
        let mut reloaded = Config::parse_text(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(reloaded.theme_mode, mode);
        reloaded.resolve_theme_mode(os);
        assert_eq!(reloaded.theme_name, "Alabaster");
        if key != "theme" {
            let other_dark = key == "light-theme";
            reloaded.theme_mode = if other_dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
            reloaded.resolve_theme_mode(None);
            let defaults = Config::default();
            assert_eq!(
                reloaded.theme_name,
                if other_dark {
                    defaults.dark_theme
                } else {
                    defaults.light_theme
                }
            );
        }
    }
    let source = production_source();
    let commit = source
        .split("fn commit_theme(")
        .nth(1)
        .unwrap()
        .split("fn ssh_key(")
        .next()
        .unwrap();
    assert!(commit.contains("let key = self.theme_pick_key();"));
    assert!(commit.contains("self.persist_pref(key, name)"));
    assert!(commit.contains("self.cfg.light_theme = name.to_string()"));
    assert!(commit.contains("self.cfg.dark_theme = name.to_string()"));
    let settings = source
        .split("fn settings_adjust(")
        .nth(1)
        .unwrap()
        .split("fn ")
        .next()
        .unwrap();
    assert!(settings.contains("self.theme_pick_key()"));
    assert!(settings.contains("self.settings_store(ws, key_str, new_val)"));
    let store = source
        .split("fn settings_store(")
        .nth(1)
        .unwrap()
        .split("fn ")
        .next()
        .unwrap();
    assert!(store.contains("self.persist_pref(key_str, &new_val)"));
}

#[test]
fn theme_picker_side_header_is_localized_and_shared_with_geometry() {
    for tr in [&EN, &ES] {
        for key in ["light-theme", "dark-theme", "theme"] {
            let prefix = super::theme_picker_appearance_prefix(key, tr);
            assert!(!prefix.is_empty());
            for width in [160.0, 320.0, 800.0] {
                let field = kettle_render::TextFieldOverlay {
                    picker_header: true,
                    text: "Tokyo".into(),
                    cursor_byte: 5,
                    selection: None,
                    horizontal_scroll: 0,
                    rect: (0.0, 0.0, width, 32.0),
                    prefix: prefix.clone(),
                    hint: String::new(),
                    accent_palette: 5,
                };
                let [prefix_rect, editor, hint] = kettle_render::text_field_rects(&field, 8.0);
                assert_eq!(editor.0, prefix_rect.0 + prefix_rect.2);
                assert!(editor.2 > 0.0);
                assert!(editor.0 + editor.2 <= hint.0 + 0.01);
            }
        }
    }
    assert!(super::theme_picker_appearance_prefix("dark-theme", &EN).contains("dark theme:"));
    assert!(super::theme_picker_appearance_prefix("light-theme", &ES).contains("tema claro:"));
    let source = production_source();
    assert!(
        source.contains("theme_picker_appearance_prefix(self.theme_pick_key(), &self.ui_text)")
    );
}

#[test]
fn system_switch_rebases_picker_escape_without_changing_query() {
    let mut ws = WindowState::new(1, Mux::new());
    begin_theme_picker(&mut ws, live("TokyoNight Night"));
    ws.theme_picker_input.as_mut().unwrap().query = "Alabaster".into();
    let baseline = live("TokyoNight Day");
    super::rebase_theme_preview(&mut ws, &baseline);
    assert_eq!(&*ws.theme_picker_input.as_ref().unwrap().query, "Alabaster");
    ws.theme_picker_input = None;
    let cfg = kettle_config::Config::parse_text("theme = Alabaster");
    let restore = theme_preview_change(&mut ws, &cfg, &EN).unwrap();
    assert_eq!(restore.0, baseline.0);
    assert_eq!(restore.1.background, baseline.1.background);
}

/// Each theme row's swatches are that theme's background, foreground and
/// ANSI red through cyan, in that order; later builds reuse the same colours.
#[test]
fn theme_rows_show_the_themes_own_colours() {
    let hex =
        |value: u32| kettle_config::Rgb::new((value >> 16) as u8, (value >> 8) as u8, value as u8);
    let dracula = [
        0x282a36, 0xf8f8f2, 0xff5555, 0x50fa7b, 0xf1fa8c, 0xbd93f9, 0xff79c6, 0x8be9fd,
    ]
    .map(hex);
    for _ in 0..2 {
        let list = theme_picker_list("dracula", 0, "TokyoNight Night", &EN);
        assert_eq!(list.rows[0].label, "Dracula");
        assert_eq!(list.rows[0].swatches, Some(dracula));
        for row in &list.rows {
            let theme = Theme::by_name(&row.label);
            let p = theme.palette;
            assert_eq!(
                row.swatches,
                Some([
                    theme.background,
                    theme.foreground,
                    p[1],
                    p[2],
                    p[3],
                    p[4],
                    p[5],
                    p[6]
                ]),
                "{}",
                row.label
            );
        }
    }
    let every = theme_picker_list("", 0, "TokyoNight Night", &EN);
    assert_eq!(every.rows.len(), Theme::list().len());
    assert!(every.rows.iter().all(|row| row.swatches.is_some()));
}

/// Only theme rows carry swatches: the other pickers, the theme picker's
/// no-match row and every other row the app builds have none.
#[test]
fn rows_other_than_themes_carry_no_swatches() {
    let layouts = ["dev".to_string()];
    let hosts = [("gpu".to_string(), "dev@gpu".to_string())];
    let lists = [
        super::command_picker_list("", 0, &kettle_config::keybinds::defaults(), &EN, None, &[]),
        super::command_picker_list(
            "zzqqxxjj",
            0,
            &kettle_config::keybinds::defaults(),
            &EN,
            None,
            &[],
        ),
        super::layout_picker_list("", 0, &layouts, &EN),
        super::layout_picker_list("zzqqxxjj", 0, &layouts, &EN),
        super::ssh_picker_list("", 0, &hosts, &EN),
        super::ssh_picker_list("", 0, &[], &EN),
        theme_picker_list("zzqqxxjj", 0, "TokyoNight Night", &EN),
    ];
    for list in lists {
        assert!(!list.rows.is_empty());
        for row in &list.rows {
            assert_eq!(row.swatches, None, "{}", row.label);
        }
    }
    // Context menus, submenus and accessibility rows are built in place; the
    // theme picker's list is the one place that fills the field.
    let src = production_source();
    assert_eq!(src.matches("swatches: Some(").count(), 1);
    let picker = src
        .split("fn theme_picker_list(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("theme_picker_list's body");
    assert!(picker.contains("swatches: Some("));
}

/// A narrow picker drops a row's swatches before it shortens the theme name
/// or its hint, so the row then shows (and a screen reader reads) exactly
/// what it would without swatches. The fit keeps them only while the whole
/// row, swatches included, fits the width the renderer measures.
#[test]
fn a_narrow_picker_drops_swatches_before_the_name() {
    use unicode_width::UnicodeWidthStr as _;

    let list = theme_picker_list("tokyonight", 0, "TokyoNight Night", &EN);
    for template in &list.rows {
        let row = || kettle_render::ContextMenuRow {
            swatches: template.swatches,
            matched_chars: None,
            label: template.label.clone(),
            separator: false,
            enabled: true,
            hint: template.hint.clone(),
        };
        let full = kettle_render::menu_row_chars(&row());
        let bare = full - kettle_render::menu::swatch_columns();
        assert_eq!(bare, template.label.width() + template.hint.width() + 2);
        for columns in 0..=full + 2 {
            let mut fitted = row();
            super::fit_context_menu_row(&mut fitted, columns);
            let mut plain = row();
            plain.swatches = None;
            super::fit_context_menu_row(&mut plain, columns);
            assert_eq!(fitted.swatches.is_some(), columns >= full, "{columns}");
            assert_eq!(
                (&fitted.label, &fitted.hint),
                (&plain.label, &plain.hint),
                "{columns}"
            );
            if columns >= bare {
                assert_eq!(fitted.label, template.label, "{columns}: name kept whole");
                assert_eq!(fitted.hint, template.hint, "{columns}");
            }
        }
    }

    // The real windows: the strip is one column, so either every row keeps it
    // (all fit) or none does; names that fit without it stay whole.
    let originals = theme_picker_list("tokyonight", 0, "TokyoNight Night", &EN);
    let (mut kept, mut dropped) = (0, 0);
    for (surface, cell) in [
        ((1280.0, 820.0), (8.0, 16.0)),
        ((640.0, 820.0), (8.0, 16.0)),
        ((640.0, 820.0), (10.0, 20.0)),
    ] {
        let list = theme_picker_list("tokyonight", 0, "TokyoNight Night", &EN);
        let menu = picker_overlay_context_menu([None, Some(list), None, None], surface, cell, 0.0)
            .expect("the window fits the picker");
        let columns = kettle_render::menu::panel_columns(menu.panel_w_clamped, cell.0);
        let all_fit = originals
            .rows
            .iter()
            .all(|row| kettle_render::menu_row_chars(row) <= columns);
        for (row, original) in menu.rows.iter().zip(&originals.rows) {
            assert_eq!(row.swatches.is_some(), all_fit, "{}", original.label);
            if !all_fit {
                // Without strips the alignment padding goes too, so it never
                // costs a name its columns.
                assert!(!row.hint.starts_with(' '), "{:?}", row.hint);
            }
            if original.label.width() + original.hint.width() + 2 <= columns {
                assert_eq!(row.label, original.label);
            }
        }
        if all_fit { kept += 1 } else { dropped += 1 }
    }
    assert!(kept > 0 && dropped > 0, "{kept} kept, {dropped} dropped");
}

/// The hints of theme rows share one width, so every row's strip starts in the
/// same column whatever its tick mark or appearance word.
#[test]
fn theme_row_strips_form_one_column() {
    use unicode_width::UnicodeWidthStr as _;

    for tr in [&EN, &ES] {
        let list = theme_picker_list("", 0, "TokyoNight Night", tr);
        let widths: std::collections::BTreeSet<_> =
            list.rows.iter().map(|row| row.hint.width()).collect();
        assert_eq!(widths.len(), 1, "{widths:?}");
        assert!(
            list.rows
                .iter()
                .any(|row| row.hint.trim_start().starts_with('✓'))
        );
    }
}

/// Swatches are decorative: the picker's options read the same names and
/// descriptions with or without them, at any window width.
#[test]
fn swatches_leave_the_accessible_rows_unchanged() {
    for surface in [(1280.0, 820.0), (640.0, 820.0), (360.0, 400.0)] {
        let read = |strip: bool| {
            let mut list = theme_picker_list("tokyonight", 0, "TokyoNight Night", &EN);
            if strip {
                for row in &mut list.rows {
                    row.swatches = None;
                }
            }
            let overlay = kettle_render::Overlay {
                theme_picker_query: Some("tokyonight".into()),
                context_menu: picker_overlay_context_menu(
                    [None, Some(list), None, None],
                    surface,
                    (8.0, 16.0),
                    0.0,
                ),
                ..kettle_render::Overlay::default()
            };
            modal_accessibility_projection(&overlay, surface, (8.0, 16.0), 1.0, &EN)
                .nodes
                .iter()
                .map(|(_, node)| {
                    (
                        node.label().map(str::to_string),
                        node.description().map(str::to_string),
                    )
                })
                .collect::<Vec<_>>()
        };
        let with = read(false);
        assert!(
            with.iter()
                .any(|(label, _)| label.as_deref() == Some("TokyoNight Night"))
        );
        assert_eq!(with, read(true), "{surface:?}");
    }
}

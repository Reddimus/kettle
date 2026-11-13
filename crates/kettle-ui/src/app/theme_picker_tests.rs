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
        slot: None,
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
            open_picker_list(ws, &cfg, "dark-theme", &ES, None).unwrap(),
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
            open_picker_list(&ws, &cfg, "dark-theme", &ES, None).is_some(),
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
                open_picker_list(ws, &cfg, "dark-theme", &ES, None).unwrap(),
                (800.0, 600.0),
                (8.0, 16.0),
                0.0,
            )
            .unwrap()
        };
        let menu = project(&ws);
        assert_eq!(menu.anchor, (176.0, 78.0));
        assert_eq!(menu.picker_header_height, 30.0);
        let row_h = kettle_render::menu::row_height(16.0);
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
    assert!(commit.contains("let key = plan.key;"));
    let enter = source
        .split("fn theme_picker_key(")
        .nth(1)
        .unwrap()
        .split("fn commit_theme(")
        .next()
        .unwrap();
    assert!(enter.contains("theme_commit_plan(state, &self.cfg, self.theme_active_dark())"));
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

/// A schedule picks the side the window wears whatever `theme-mode` says,
/// so a pick saves to that side's key in every mode: a fixed theme or a
/// forced side under a schedule would otherwise write a key the schedule
/// overrides, and the pick would be lost. The status line then checks the
/// worn side, marks it, and Enter keeps the pick.
#[test]
fn a_scheduled_pick_saves_to_the_side_the_schedule_wears_in_every_mode() {
    use kettle_config::ThemeMode;
    use kettle_render::ThemeSlot;
    let mut cfg = kettle_config::Config::default();
    cfg.theme_schedule = kettle_config::parse_theme_schedule("19:00 dark,07:00 light");
    assert!(cfg.theme_schedule.is_some());
    for mode in [
        ThemeMode::Explicit,
        ThemeMode::Light,
        ThemeMode::Dark,
        ThemeMode::Auto,
    ] {
        cfg.theme_mode = mode;
        for (dark, key, slot) in [
            (false, "light-theme", ThemeSlot::Light),
            (true, "dark-theme", ThemeSlot::Dark),
        ] {
            assert_eq!(super::theme_pick_key(&cfg, Some(dark)), key, "{mode:?}");
            let picker = state("", 0, "TokyoNight Night");
            let save = super::theme_picker_save_key(Some(&picker), key);
            assert_eq!(save, key, "{mode:?}");
            assert_eq!(
                super::theme_commit_plan(&picker, &cfg, Some(dark)),
                super::ThemeCommitPlan {
                    key,
                    restore_active: false
                },
                "{mode:?}"
            );
            assert_eq!(
                super::theme_picker_enter_hint(save, key),
                kettle_i18n::Text::PickerHintKeep
            );
            let footer = super::theme_picker_footer(&picker, &cfg, save, &EN);
            assert_eq!(footer.editing, Some(slot), "{mode:?}");
            let slots = kettle_render::theme_footer_slots(&footer, &EN);
            assert_eq!(slots.len(), 2);
            for (side, _, checked) in slots {
                assert_eq!(checked, side == slot, "{mode:?} {side:?}");
            }
            // The other side, chosen on the status line, saves its own key
            // and puts the window back on the side the schedule wears.
            let mut other = picker.clone();
            other.slot = Some(match slot {
                ThemeSlot::Dark => ThemeSlot::Light,
                ThemeSlot::Light => ThemeSlot::Dark,
            });
            let plan = super::theme_commit_plan(&other, &cfg, Some(dark));
            assert_ne!(plan.key, key, "{mode:?}");
            assert_ne!(plan.key, "theme", "{mode:?}");
            assert!(plan.restore_active, "{mode:?}");
        }
    }
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
        source.contains(
            "theme_picker_appearance_prefix(self.theme_picker_key_for(ws), &self.ui_text)"
        )
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
            toggle: None,
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

/// The footer says what decides the theme: a schedule when one is set (it
/// overrides following the system), the system otherwise, and the side a
/// pick saves to is the one `theme_key` names.
#[test]
fn the_theme_picker_footer_names_the_schedule_over_the_system() {
    use kettle_render::{ThemeFollow, ThemeSlot};
    let picker = state("", 0, "TokyoNight Night");
    let mut cfg = kettle_config::Config::default();
    let footer = super::theme_picker_footer(&picker, &cfg, "dark-theme", &EN);
    assert_eq!(footer.follow, ThemeFollow::System);
    assert_eq!(footer.editing, Some(ThemeSlot::Dark));
    assert_eq!(footer.dark_theme, cfg.dark_theme);
    assert_eq!(footer.light_theme, cfg.light_theme);
    cfg.theme_schedule = kettle_config::parse_theme_schedule("19:00 dark,07:00 light");
    assert!(cfg.theme_schedule.is_some());
    let footer = super::theme_picker_footer(&picker, &cfg, "light-theme", &EN);
    assert_eq!(footer.follow, ThemeFollow::Schedule);
    assert_eq!(footer.editing, Some(ThemeSlot::Light));
    cfg.theme_schedule = None;
    cfg.theme_mode = kettle_config::ThemeMode::Explicit;
    let footer = super::theme_picker_footer(&picker, &cfg, "theme", &EN);
    assert_eq!(footer.follow, ThemeFollow::Fixed);
    assert_eq!(footer.editing, None);
    assert_eq!(footer.fixed_theme, "TokyoNight Night");
}

/// The footer's sample is the highlighted theme with the user's colour
/// overrides, so moving the selection changes the card's colours.
#[test]
fn the_theme_picker_sample_follows_the_selection_with_overrides() {
    let cfg = kettle_config::Config::parse_text("palette = 4=#123456\n");
    let first = super::theme_picker_footer(
        &state("dracula", 0, "TokyoNight Night"),
        &cfg,
        "dark-theme",
        &EN,
    );
    let second = super::theme_picker_footer(
        &state("nord", 0, "TokyoNight Night"),
        &cfg,
        "dark-theme",
        &EN,
    );
    assert_ne!(first.sample.background, second.sample.background);
    for footer in [&first, &second] {
        assert_eq!(
            footer.sample.palette[4],
            kettle_config::Rgb::parse("#123456").unwrap()
        );
    }
}

/// On a 1280x820 window at 1x and 2x, in English and Spanish, the theme
/// picker shows its sample card and both status lines and still paints at
/// least 12 whole rows, while the command palette keeps 8. The card's top
/// edge lies under the last painted row, and the whole card, footer
/// included, is the picker's.
#[test]
fn the_theme_picker_fills_a_tall_window_with_rows_and_its_footer() {
    use super::{command_picker_list, open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config::default();
    for (cell, top) in [((8.4, 17.0), 32.0), ((16.8, 34.0), 64.0)] {
        let scale = cell.0 / 8.4;
        let surface = (1280.0 * scale, 820.0 * scale);
        let row_h = kettle_render::menu::row_height(cell.1);
        for tr in [&EN, &ES] {
            let mut ws = WindowState::new(0, Mux::new());
            ws.theme_picker_input = Some(state("", 0, "TokyoNight Night"));
            let list = open_picker_list(&ws, &cfg, "dark-theme", tr, None).unwrap();
            let rows = list.rows.len();
            let menu = picker_context_menu(list, surface, cell, top).unwrap();
            let footer = menu.picker_footer.as_ref().expect("a footer");
            assert!(footer.show_sample, "{scale}x: card");
            assert_eq!(footer.status_lines, 2, "{scale}x");
            let (_, content_h) = kettle_render::menu::scroll_content(
                rows as f32 * row_h,
                menu.panel_h_clamped,
                row_h,
            );
            let painted = (content_h / row_h + 0.001).floor() as usize;
            assert!(painted >= 12, "{scale}x: {painted} rows");
            assert!(painted <= 14, "{scale}x: {painted} rows");
            assert!(
                (content_h - painted as f32 * row_h).abs() < 0.5,
                "{scale}x: no blank part-row"
            );
            let card = menu.card_rect();
            assert!(
                card.1 + card.3 <= surface.1 - 0.0,
                "{scale}x: the card fits"
            );
            let layout = menu.picker_footer_layout(tr, cell).unwrap();
            assert!(layout.rect.1 >= menu.anchor.1 + menu.panel_h_clamped - 0.01);
            assert!((layout.rect.1 + layout.rect.3 - (card.1 + card.3)).abs() < 0.01);
            assert!(layout.contrast_button.is_some(), "{scale}x");
        }
        let palette = picker_context_menu(
            command_picker_list("", 0, &kettle_config::keybinds::defaults(), &EN, None, &[]),
            surface,
            cell,
            top,
        )
        .unwrap();
        assert!(palette.picker_footer.is_none());
        let total = palette.rows.len();
        let (_, content_h) = kettle_render::menu::scroll_content(
            total as f32 * row_h,
            palette.panel_h_clamped,
            row_h,
        );
        assert_eq!(
            (content_h / row_h + 0.001).floor() as usize,
            8,
            "{scale}x palette"
        );
    }
}

/// A short window drops the sample card first, then a status line, and the
/// list keeps at least four rows; a window of 600 pixels already shows the
/// card.
#[test]
fn a_short_window_drops_the_theme_sample_before_the_rows() {
    use super::{open_picker_list, picker_context_menu};
    let cfg = kettle_config::Config::default();
    let cell = (8.4, 17.0);
    let row_h = kettle_render::menu::row_height(cell.1);
    let mut seen = std::collections::BTreeSet::new();
    for height in (220..=820).step_by(20) {
        let mut ws = WindowState::new(0, Mux::new());
        ws.theme_picker_input = Some(state("", 0, "TokyoNight Night"));
        let list = open_picker_list(&ws, &cfg, "dark-theme", &EN, None).unwrap();
        let rows = list.rows.len();
        let Some(menu) = picker_context_menu(list, (1280.0, height as f32), cell, 32.0) else {
            continue;
        };
        let (_, content_h) =
            kettle_render::menu::scroll_content(rows as f32 * row_h, menu.panel_h_clamped, row_h);
        let painted = (content_h / row_h + 0.001).floor() as usize;
        let parts = menu.picker_footer.as_ref().map_or((false, 0), |footer| {
            (footer.show_sample, footer.status_lines)
        });
        if parts.1 > 0 {
            assert!(painted >= 4, "{height}: footer with {painted} rows");
        }
        if height >= 600 {
            assert!(parts.0, "{height}: no sample card");
        }
        let card = menu.card_rect();
        assert!(card.1 + card.3 <= height as f32 + 0.01, "{height}");
        seen.insert(parts);
    }
    assert!(seen.contains(&(true, 2)), "{seen:?}");
    assert!(
        seen.contains(&(false, 2)) || seen.contains(&(false, 1)),
        "{seen:?}"
    );
}

/// A press on the contrast button steps the setting; a press elsewhere on
/// the footer stays in the picker; the rows are not the footer's. The
/// pointer path saves the step through the Settings store.
#[test]
fn the_contrast_button_takes_its_press_and_saves_through_settings() {
    use super::{FooterPress, open_picker_list, picker_context_menu, theme_footer_press};
    let cfg = kettle_config::Config::default();
    let cell = (8.4, 17.0);
    let mut ws = WindowState::new(0, Mux::new());
    ws.theme_picker_input = Some(state("", 0, "TokyoNight Night"));
    let menu = picker_context_menu(
        open_picker_list(&ws, &cfg, "dark-theme", &EN, None).unwrap(),
        (1280.0, 820.0),
        cell,
        32.0,
    )
    .unwrap();
    let layout = menu.picker_footer_layout(&EN, cell).unwrap();
    let at = |x: f32, y: f32| winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y));
    let (bx, by, bw, bh) = layout.contrast_button.unwrap();
    assert_eq!(
        theme_footer_press(&menu, &EN, cell, at(bx + bw * 0.5, by + bh * 0.5)),
        Some(FooterPress::Contrast)
    );
    let (cx, cy, _, _) = layout.card.unwrap();
    assert_eq!(
        theme_footer_press(&menu, &EN, cell, at(cx + 4.0, cy + 4.0)),
        Some(FooterPress::Footer)
    );
    let row = menu.visible_row_rect(0).unwrap();
    assert_eq!(
        theme_footer_press(&menu, &EN, cell, at(row.0 + 4.0, row.1 + 4.0)),
        None
    );
    let src = production_source();
    let pointer = src
        .split("fn picker_pointer(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(pointer.contains("self.step_picker_contrast(ws)"));
    let step = src
        .split("fn step_picker_contrast(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(step.contains("\"minimum-contrast\""));
    assert!(step.contains("self.settings_store("));
    // A press anywhere on the card counts as inside the picker.
    let target = src
        .split("fn modal_press_target(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(target.contains("rect_contains(menu.card_rect(), x, y)"));
}

/// A screen reader hears the footer as one line: the readout with the
/// lowest colour's number, the mode, both sides and the contrast setting,
/// inside the picker's dialog.
#[test]
fn the_theme_picker_footer_has_an_accessible_text() {
    let cfg = kettle_config::Config::default();
    for tr in [&EN, &ES] {
        let mut ws = WindowState::new(0, Mux::new());
        ws.theme_picker_input = Some(state("", 0, "TokyoNight Night"));
        let list = super::open_picker_list(&ws, &cfg, "dark-theme", tr, None).unwrap();
        let menu = super::picker_context_menu(list, (1280.0, 820.0), (8.4, 17.0), 32.0).unwrap();
        let overlay = kettle_render::Overlay {
            theme_picker_query: Some(String::new()),
            context_menu: Some(menu),
            ..Default::default()
        };
        let projection =
            modal_accessibility_projection(&overlay, (1280.0, 820.0), (8.4, 17.0), 1.0, tr);
        let labels: Vec<String> = projection
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::Label)
            .filter_map(|(_, node)| node.label().map(str::to_string))
            .collect();
        let text = labels
            .iter()
            .find(|label| label.contains(tr.text(kettle_i18n::Text::PickerThemeDarkSlot)))
            .unwrap_or_else(|| panic!("no footer text in {labels:?}"));
        assert!(
            text.contains("TokyoNight Night") && text.contains("Kettle Day"),
            "{text}"
        );
        assert!(
            text.contains(tr.text(kettle_i18n::Text::SettingsValueFollowSystem)),
            "{text}"
        );
        assert!(
            text.contains(&kettle_render::contrast_button_text(0.0, tr)),
            "{text}"
        );
    }
}

/// Following a dark system, choosing the light side on the status line opens
/// the list on the light theme, ticks and highlights it, and makes Enter save
/// `light-theme` and put the window back on its dark theme. Choosing the
/// dark side again saves and keeps the pick as before. A fixed theme has no
/// sides, so `theme` is saved whatever was chosen.
#[test]
fn choosing_the_light_side_saves_light_theme_and_keeps_the_dark_one_on() {
    use super::{ThemeCommitPlan, switch_theme_picker_slot, theme_commit_plan};
    use kettle_render::ThemeSlot;
    let mut cfg = kettle_config::Config::default();
    assert_eq!(cfg.theme_mode, kettle_config::ThemeMode::Auto);
    assert_ne!(cfg.light_theme, cfg.dark_theme);
    let dark = Some(true);
    let mut picker = state("", 0, &cfg.dark_theme.clone());
    picker.selected = theme_picker_start(&cfg.dark_theme);
    assert_eq!(
        theme_commit_plan(&picker, &cfg, dark),
        ThemeCommitPlan {
            key: "dark-theme",
            restore_active: false
        }
    );
    assert!(switch_theme_picker_slot(
        &mut picker,
        &cfg,
        ThemeSlot::Light,
        &EN
    ));
    assert_eq!(picker.slot, Some(ThemeSlot::Light));
    assert_eq!(picker.opened_on, cfg.light_theme);
    assert_eq!(
        theme_picker_selection(&picker, &EN),
        Some(cfg.light_theme.as_str())
    );
    assert_eq!(
        theme_commit_plan(&picker, &cfg, dark),
        ThemeCommitPlan {
            key: "light-theme",
            restore_active: true
        }
    );
    assert!(!switch_theme_picker_slot(
        &mut picker,
        &cfg,
        ThemeSlot::Light,
        &EN
    ));
    // The status line and the hint follow the chosen side.
    let key = super::theme_picker_save_key(Some(&picker), "dark-theme");
    assert_eq!(key, "light-theme");
    let footer = super::theme_picker_footer(&picker, &cfg, key, &EN);
    assert_eq!(footer.editing, Some(ThemeSlot::Light));
    assert_eq!(
        super::theme_picker_enter_hint(key, "dark-theme"),
        kettle_i18n::Text::PickerHintSaveForLight
    );
    assert_eq!(
        super::theme_picker_enter_hint("dark-theme", "dark-theme"),
        kettle_i18n::Text::PickerHintKeep
    );
    // The longer Enter hint still leaves the query its room in the
    // narrowest header (the header drops a hint that does not fit beside
    // the query), in Spanish, at 1x and 2x; the painter and the projection
    // (IME, hit test and screen reader) build the same hint.
    for tr in [&EN, &ES] {
        for (cw, width) in [(8.4, 360.0), (16.8, 720.0), (8.4, 720.0)] {
            let field = kettle_render::TextFieldOverlay {
                picker_header: true,
                text: "Tokyo".into(),
                cursor_byte: 5,
                selection: None,
                horizontal_scroll: 0,
                rect: (0.0, 0.0, width, 32.0),
                prefix: super::theme_picker_appearance_prefix(key, tr),
                hint: super::picker_hint(tr, &super::theme_picker_hints(key, "dark-theme")),
                accent_palette: 5,
            };
            let [prefix, editor, hint] = kettle_render::text_field_rects(&field, cw);
            assert!(editor.2 >= 6.0 * cw, "{width} {cw}: {editor:?}");
            assert!(prefix.0 + prefix.2 <= editor.0 + 0.01);
            assert!(editor.0 + editor.2 <= hint.0 + 0.01);
            assert!(hint.0 + hint.2 <= width + 0.01);
        }
    }
    let source = production_source();
    for builder in ["fn modal_text_projection(", "fn overlay("] {
        let body = source.split(builder).nth(1).unwrap();
        let body = body.split("\n    }\n").next().unwrap();
        assert!(
            body.contains(
                "theme_picker_hints(self.theme_picker_key_for(ws), self.theme_pick_key())"
            ),
            "{builder}"
        );
    }
    assert!(switch_theme_picker_slot(
        &mut picker,
        &cfg,
        ThemeSlot::Dark,
        &EN
    ));
    assert_eq!(picker.opened_on, cfg.dark_theme);
    assert_eq!(
        theme_commit_plan(&picker, &cfg, dark),
        ThemeCommitPlan {
            key: "dark-theme",
            restore_active: false
        }
    );
    cfg.theme_mode = kettle_config::ThemeMode::Explicit;
    picker.slot = Some(ThemeSlot::Light);
    assert_eq!(
        theme_commit_plan(&picker, &cfg, dark),
        ThemeCommitPlan {
            key: "theme",
            restore_active: false
        }
    );
}

/// The window previews the light side while it is edited, Esc puts back the
/// theme from before the picker, and a pick for the side the window is not on
/// leaves the baseline in place, so closing the picker puts that theme back.
/// `commit_theme` drops the baseline and wears the pick only for the side
/// the window is on.
#[test]
fn a_pick_for_the_other_side_puts_the_window_back_on_its_theme() {
    let cfg = kettle_config::Config::default();
    let mut ws = WindowState::new(0, Mux::new());
    begin_theme_picker(&mut ws, live(&cfg.dark_theme));
    let picker = ws.theme_picker_input.as_mut().unwrap();
    super::switch_theme_picker_slot(picker, &cfg, kettle_render::ThemeSlot::Light, &EN);
    let previewed = settle(&mut ws, &cfg.dark_theme);
    assert_eq!(previewed, cfg.light_theme);
    // Enter for the light side: the picker closes and the baseline stays.
    ws.theme_picker_input = None;
    assert_eq!(settle(&mut ws, &previewed), cfg.dark_theme);
    let source = production_source();
    let commit = source
        .split("fn commit_theme(")
        .nth(1)
        .unwrap()
        .split("fn ssh_key(")
        .next()
        .unwrap();
    let keep = commit
        .split("if !plan.restore_active {")
        .nth(1)
        .expect("only a pick for the active side is worn")
        .split('}')
        .next()
        .unwrap();
    assert!(keep.contains("ws.theme_preview = None;"));
    assert!(keep.contains("self.set_runtime_theme_name(ws, name);"));
    assert_eq!(commit.matches("ws.theme_preview = None;").count(), 1);
    let back = commit
        .split("} else {")
        .nth(1)
        .expect("the other side's branch");
    assert!(back.contains("self.sync_theme_preview(ws);"));
    assert_eq!(commit.matches("set_runtime_theme").count(), 1);
}

/// A press on a side of the status line chooses that side; the pointer and
/// a screen reader's click both switch the picker through
/// `switch_theme_slot`, and a screen reader hears each side as a radio
/// button, checked on the side a pick saves to, over its painted words.
#[test]
fn each_side_of_the_status_line_takes_a_press_and_a_screen_reader_click() {
    use super::{FooterPress, open_picker_list, picker_context_menu, theme_footer_press};
    use kettle_render::ThemeSlot;
    let cfg = kettle_config::Config::default();
    let cell = (8.4, 17.0);
    for tr in [&EN, &ES] {
        let mut ws = WindowState::new(0, Mux::new());
        ws.theme_picker_input = Some(state("", 0, "TokyoNight Night"));
        let menu = picker_context_menu(
            open_picker_list(&ws, &cfg, "dark-theme", tr, None).unwrap(),
            (1280.0, 820.0),
            cell,
            32.0,
        )
        .unwrap();
        let layout = menu.picker_footer_layout(tr, cell).unwrap();
        let at = |x: f32, y: f32| winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y));
        assert_eq!(layout.slot_chips.len(), 2);
        for &((x, y, w, h), slot) in &layout.slot_chips {
            assert_eq!(
                theme_footer_press(&menu, tr, cell, at(x + w * 0.5, y + h * 0.5)),
                Some(FooterPress::Slot(slot))
            );
        }
        let overlay = kettle_render::Overlay {
            theme_picker_query: Some(String::new()),
            context_menu: Some(menu),
            ..Default::default()
        };
        let projection = modal_accessibility_projection(&overlay, (1280.0, 820.0), cell, 1.0, tr);
        let radios: Vec<_> = projection
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == accesskit::Role::RadioButton)
            .collect();
        assert_eq!(radios.len(), 2);
        for ((id, node), &(rect, slot)) in radios.iter().zip(&layout.slot_chips) {
            assert_eq!(super::accessibility_theme_slot(*id), Some(slot));
            let label = node.label().unwrap();
            let side = tr.text(match slot {
                ThemeSlot::Dark => kettle_i18n::Text::PickerThemeDarkSlot,
                ThemeSlot::Light => kettle_i18n::Text::PickerThemeLightSlot,
            });
            assert!(label.starts_with(side), "{label}");
            assert_eq!(
                node.toggled(),
                Some((slot == ThemeSlot::Dark).into()),
                "{label}"
            );
            let bounds = node.bounds().unwrap();
            assert_eq!(bounds.x0, f64::from(rect.0));
            assert_eq!(bounds.x1, f64::from(rect.0 + rect.2));
            assert!(node.supports_action(accesskit::Action::Click));
            let dialog = projection
                .nodes
                .iter()
                .find(|(_, node)| node.role() == accesskit::Role::Dialog)
                .unwrap();
            assert!(dialog.1.children().contains(id));
        }
    }
    let src = production_source();
    let pointer = src
        .split("fn picker_pointer(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(pointer.contains("FooterPress::Slot(slot) => self.switch_theme_slot(ws, slot)"));
    let action = src
        .split("fn handle_accessibility_action(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(action.contains("accessibility_theme_slot(request.target_node)"));
    assert!(action.contains("self.switch_theme_slot(ws, slot);"));
    // A screen reader's click is a user event, past `window_event`'s
    // preview sync, so switching previews the side's theme itself.
    let switch = src
        .split("fn switch_theme_slot(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(switch.contains("self.sync_theme_preview(ws);"));
}

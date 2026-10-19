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
        query: query.to_string(),
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
        let matches = theme_picker_matches("", opened_on);
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
        assert_eq!(theme_picker_matches("   ", opened_on), matches);
    }
}

#[test]
fn a_query_ranks_matching_themes_and_says_when_none_match() {
    // "Dracula+" scores the same and comes first in look order, but the
    // theme the query names exactly wins.
    let matches = theme_picker_matches("dracula", "TokyoNight Night");
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
        theme_picker_selection(&state("zzqqxxjj", 3, "TokyoNight Night")),
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
        let appearance = row.hint.strip_prefix("✓ ").unwrap_or(&row.hint);
        assert_eq!(row.hint.starts_with("✓ "), row.label == "TokyoNight Night");
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
            .map(|row| row.hint.as_str())
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
    theme_preview_change(ws, wearing)
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
    assert_eq!(theme_picker_selection(state), Some(opened));

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
    assert!(theme_preview_change(&mut ws, opened).is_none());
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
    assert!(theme_preview_change(&mut ws, &kept).is_none());
}

/// A composition a closing modal owned ends, so a late Commit cannot type
/// into what it covered; the owner marker stays to recognise it.
#[test]
fn closing_a_modal_ends_its_composition() {
    let mut ws = WindowState::new(0, Mux::new());
    ws.ime_preedit = Some(("ka".into(), None));
    ws.ime_preedit_owner = Some(ImePreeditSession {
        ended: None,
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
        let picked = theme_picker_selection(&state(query, selected, opened_on));
        let list = theme_picker_list(query, selected, opened_on, &EN);
        assert_eq!(
            picked,
            Some(list.rows[list.selected].label.as_str()),
            "{query:?} {selected}"
        );
    }
    assert_eq!(
        theme_picker_selection(&state("", theme_picker_start(opened_on), opened_on)),
        Some(opened_on)
    );
}

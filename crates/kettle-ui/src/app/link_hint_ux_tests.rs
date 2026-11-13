use super::*;
use kettle_i18n::{Language, Translator};

#[test]
fn link_open_gesture_follows_platform_setting_and_language() {
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    assert_eq!(link_open_gesture(&en, false, true), "⌘-click to open");
    assert_eq!(link_open_gesture(&en, false, false), "Ctrl+click to open");
    assert_eq!(link_open_gesture(&en, true, true), "Click to open");
    assert_eq!(link_open_gesture(&es, false, false), "Ctrl+clic para abrir");
    assert_eq!(link_open_gesture(&es, true, false), "Hacer clic para abrir");
}

#[test]
fn hover_is_dismissed_until_pointer_motion_and_bound_to_the_viewport() {
    let mut ws = WindowState::new(1, Mux::new());
    ws.window_focused = true;
    ws.native_pointer = Some(winit::dpi::PhysicalPosition::new(10.0, 20.0));
    assert!(!link_hover_is_current(&ws));
    ws.link_hover_key = Some(current_links_scan_key(&ws));
    assert!(link_hover_is_current(&ws));
    ws.window_focused = false;
    assert!(!link_hover_is_current(&ws));
    ws.window_focused = true;
    ws.native_pointer = None;
    assert!(!link_hover_is_current(&ws));
    ws.native_pointer = Some(winit::dpi::PhysicalPosition::new(10.0, 20.0));
    ws.link_hover_key.as_mut().unwrap().3 = Some(10);
    assert!(
        !link_hover_is_current(&ws),
        "a scroll dismisses the old hover"
    );
    dismiss_link_hover(&mut ws);
    assert!(
        !link_hover_is_current(&ws),
        "typing stays dismissed across redraws"
    );
}

#[test]
fn labels_prefer_blank_cells_before_then_after_the_match() {
    assert_eq!(hint_label_column(8, 18, 1, 80, |col| col == 7), 7);
    assert_eq!(
        hint_label_column(8, 18, 2, 80, |col| (6..8).contains(&col)),
        6
    );
    assert_eq!(hint_label_column(8, 18, 2, 80, |col| col == 7), 8);
    assert_eq!(hint_label_column(8, 18, 1, 80, |_| false), 8);
    assert_eq!(hint_label_column(0, 18, 1, 80, |col| col == 19), 19);
    assert_eq!(hint_label_column(0, 79, 1, 80, |_| false), 0);
    // A two-letter label with one blank cell before the match goes after
    // it rather than over its first letters.
    assert_eq!(
        hint_label_column(8, 18, 2, 80, |col| col == 7 || (19..=20).contains(&col)),
        19
    );
    // Before still wins when both sides have room.
    assert_eq!(
        hint_label_column(8, 18, 2, 80, |col| (6..8).contains(&col) || col > 18),
        6
    );
    // No room after the right edge: cover the start.
    assert_eq!(hint_label_column(8, 79, 2, 80, |col| col == 7), 8);
}

#[test]
fn hint_legend_explains_plain_alternate_and_preview_actions_in_both_languages() {
    use HintLegendAction::*;
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let legend = hint_legend_text(&tr, &[OpenUrl, Copy], &[Copy, OpenPath]);
        assert!(legend.contains(tr.text(kettle_i18n::Text::HintActionOpenUrl)));
        assert!(legend.contains(tr.text(kettle_i18n::Text::HintActionOpenPath)));
        assert!(legend.contains("Shift+"));
        assert!(legend.contains("Esc"));
        let preview = hint_legend_text(&tr, &[Preview], &[Preview]);
        assert!(preview.contains(tr.text(kettle_i18n::Text::HintActionPreview)));
        assert!(!preview.contains("Shift"));
        assert!(preview.contains("Esc"));
    }
}

#[test]
fn hint_context_menu_and_palette_offer_the_action_with_the_live_shortcut() {
    let tr = Translator::new(Language::En);
    let mut items = Vec::new();
    append_link_hints_menu_item(&mut items, false, tr);
    assert!(items.is_empty());
    append_link_hints_menu_item(&mut items, true, tr);
    assert!(matches!(
        items.as_slice(),
        [ContextMenuItem::Item {
            action: Action::HintMode,
            enabled: true,
            ..
        }]
    ));
    let grouped = grouped_terminal_menu(items, tr);
    assert!(matches!(
        grouped.first(),
        Some(ContextMenuItem::Item {
            action: Action::HintMode,
            ..
        })
    ));
    let trigger = kettle_config::keybinds::parse_trigger("ctrl+shift+u").unwrap();
    let bindings = [(trigger, Action::HintMode)].into_iter().collect();
    let picker = command_picker_list("Quick-select hints", 0, &bindings, &tr, None, &[]);
    assert_eq!(picker.rows[0].label, "Quick-select hints");
    assert_eq!(
        picker.rows[0].hint,
        if cfg!(target_os = "macos") {
            "⌃⇧U"
        } else {
            "Ctrl+Shift+U"
        }
    );
}

#[test]
fn accessibility_exposes_hover_target_tooltip_legend_and_label_bounds() {
    let tr = Translator::new(Language::En);
    let pill = kettle_render::link_status_layout(
        (0.0, 0.0, 800.0, 400.0),
        (8.0, 16.0),
        "https://x.test/path",
        "Ctrl+click to open",
        &[(0.0, 0.0, 100.0, 16.0)],
    )
    .unwrap();
    let legend = kettle_render::hint_legend_layout(
        (0.0, 0.0, 800.0, 400.0),
        (8.0, 16.0),
        "Label: copy text · Esc to cancel",
    )
    .unwrap();
    let overlay = Overlay {
        pane_statuses: vec![pill, legend],
        hint_labels: vec![HintLabel {
            row: 1,
            col: 3,
            label: "a".into(),
            dim: false,
        }],
        hint_bounds: vec![(24.0, 16.0, 8.0, 16.0)],
        ..Overlay::default()
    };
    let projected = modal_accessibility_projection(&overlay, (800.0, 400.0), (8.0, 16.0), 1.0, &tr);
    let link = projected
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Link)
        .unwrap();
    assert_eq!(link.1.label(), Some("https://x.test/path"));
    assert!(!link.1.described_by().is_empty());
    let tooltip = projected
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::Tooltip)
        .unwrap();
    assert!(tooltip.1.label().unwrap().contains("Ctrl+click to open"));
    assert!(tooltip.1.bounds().is_some());
    let hints = projected
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::ListBox)
        .unwrap();
    assert!(hints.1.description().unwrap().contains("Esc to cancel"));
    assert_eq!(hints.1.live(), Some(accesskit::Live::Polite));
    let label = projected
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::ListBoxOption)
        .unwrap();
    assert_eq!(
        label.1.bounds(),
        Some(accesskit::Rect::new(24.0, 16.0, 32.0, 32.0))
    );
}

#[test]
fn osc8_hints_use_the_destination_instead_of_the_caption() {
    let link = kettle_core::Link {
        row: 0,
        start_col: 0,
        end_col: 9,
        group: 0,
        uri: "https://real.test".into(),
        explicit: true,
    };
    let hints = hint_spans_with_links(kettle_core::hints::detect(&["https://x"]), &[link]);
    assert_eq!(hints.len(), 1);
    assert_eq!(hints[0].text, "https://real.test");
    let caption = kettle_core::Link {
        row: 0,
        start_col: 0,
        end_col: 9,
        group: 0,
        uri: "https://real.test".into(),
        explicit: true,
    };
    assert_eq!(hint_spans_with_links(Vec::new(), &[caption]).len(), 1);
    assert_eq!(
        link_status_target("https://x/\n\u{202e}secret"),
        "https://x/\u{fffd}\u{fffd}secret"
    );
}

#[test]
fn hover_dismissal_events_clear_state_before_any_early_return() {
    // Wheel sub-detents and IME/modal keys can return before dispatching input.
    let source = include_str!("window_events.rs");
    for event in [
        "CursorLeft { .. }",
        "MouseWheel { delta, phase, .. }",
        "Focused(f)",
    ] {
        let arm = source
            .split_once(&format!("WindowEvent::{event} => {{"))
            .unwrap()
            .1;
        assert!(
            arm.trim_start().starts_with("dismiss_link_hover(ws);"),
            "{event}"
        );
    }
}

#[test]
fn typing_dismisses_hover_but_opening_modifiers_and_releases_do_not() {
    let ctrl = Key::Named(NamedKey::Control);
    let physical = PhysicalKey::Code(KeyCode::ControlLeft);
    assert!(!key_dismisses_link_hover(
        ElementState::Pressed,
        &ctrl,
        physical
    ));
    assert!(!key_dismisses_link_hover(
        ElementState::Released,
        &Key::Character("a".into()),
        PhysicalKey::Code(KeyCode::KeyA)
    ));
    assert!(key_dismisses_link_hover(
        ElementState::Pressed,
        &Key::Character("a".into()),
        PhysicalKey::Code(KeyCode::KeyA)
    ));
}

#[test]
fn neighboring_labels_cannot_both_claim_the_same_blank_cell() {
    let mut occupied = std::collections::HashSet::new();
    let first = hint_label_column(0, 5, 1, 80, |col| col == 6);
    occupied.insert(first);
    let second = hint_label_column(7, 12, 1, 80, |col| col == 6 && !occupied.contains(&col));
    assert_eq!((first, second), (6, 7));
}

#[test]
fn link_hover_uses_the_physical_pointer_and_excludes_padding() {
    let mut ws = WindowState::new(1, Mux::new());
    ws.window_focused = true;
    ws.links_scan_key = Some(current_links_scan_key(&ws));
    ws.link_hover_key = ws.links_scan_key.clone();
    ws.links.push(kettle_core::Link {
        row: 1,
        start_col: 1,
        end_col: 3,
        uri: "https://native.test".into(),
        group: 0,
        explicit: false,
    });
    ws.cursor = winit::dpi::PhysicalPosition::new(300.0, 100.0);
    ws.native_pointer = Some(winit::dpi::PhysicalPosition::new(120.0, 75.0));
    let pane = (100.0, 50.0, 400.0, 200.0);
    assert_eq!(
        link_under_native_pointer(&ws, pane, (8.0, 16.0), (4.0, 4.0))
            .unwrap()
            .uri,
        "https://native.test"
    );
    for pointer in [(102.0, 75.0), (120.0, 52.0), (99.0, 75.0), (500.0, 75.0)] {
        ws.native_pointer = Some(winit::dpi::PhysicalPosition::new(pointer.0, pointer.1));
        assert!(link_under_native_pointer(&ws, pane, (8.0, 16.0), (4.0, 4.0)).is_none());
    }
}

fn link(row: usize, start_col: usize, end_col: usize, group: usize) -> kettle_core::Link {
    kettle_core::Link {
        row,
        start_col,
        end_col,
        uri: format!("https://group{group}.test"),
        group,
        explicit: false,
    }
}

fn selected(start: (i32, usize), end: (i32, usize), is_block: bool) -> kettle_core::SelectionRange {
    use kettle_core::{Column, Line, Point};
    kettle_core::SelectionRange::new(
        Point::new(Line(start.0), Column(start.1)),
        Point::new(Line(end.0), Column(end.1)),
        is_block,
    )
}

#[test]
fn a_hovered_link_meets_the_selection_on_any_of_its_rows() {
    // Group 0 wraps: columns 70..=79 of row 2, then 0..=9 of row 3.
    let links = [link(2, 70, 79, 0), link(3, 0, 9, 0), link(5, 10, 20, 1)];
    // One row: the selection runs into the link's first cells.
    assert!(hover_group_intersects_selection(
        &links,
        0,
        &selected((2, 60), (2, 72), false),
        0
    ));
    // Only the wrapped second row is selected.
    assert!(hover_group_intersects_selection(
        &links,
        0,
        &selected((3, 5), (4, 0), false),
        0
    ));
    // Selected text beside the link, not on it.
    assert!(!hover_group_intersects_selection(
        &links,
        1,
        &selected((5, 0), (5, 9), false),
        0
    ));
    assert!(!hover_group_intersects_selection(
        &links,
        0,
        &selected((4, 0), (6, 0), false),
        0
    ));
    // Scrolled back 10 lines: viewport row 5 is grid line -5.
    assert!(hover_group_intersects_selection(
        &links,
        1,
        &selected((-5, 12), (-5, 14), false),
        10
    ));
    assert!(!hover_group_intersects_selection(
        &links,
        1,
        &selected((5, 12), (5, 14), false),
        10
    ));
    // A block over rows 4..=6 whose columns stop short of the link.
    assert!(!hover_group_intersects_selection(
        &links,
        1,
        &selected((4, 0), (6, 5), true),
        0
    ));
    // The same rows as a stream selection cover the whole of row 5.
    assert!(hover_group_intersects_selection(
        &links,
        1,
        &selected((4, 0), (6, 5), false),
        0
    ));
}

#[test]
fn a_selection_drag_hovers_no_link_and_release_restores_it() {
    let mut ws = WindowState::new(1, Mux::new());
    ws.window_focused = true;
    ws.links_scan_key = Some(current_links_scan_key(&ws));
    ws.link_hover_key = ws.links_scan_key.clone();
    ws.links.push(link(1, 1, 3, 0));
    ws.native_pointer = Some(winit::dpi::PhysicalPosition::new(120.0, 75.0));
    let pane = (100.0, 50.0, 400.0, 200.0);
    let hovered = |ws: &WindowState| {
        link_under_native_pointer(ws, pane, (8.0, 16.0), (4.0, 4.0)).map(|link| link.group)
    };
    assert_eq!(hovered(&ws), Some(0));
    // A press that has not moved past the drag threshold is still a click.
    ws.selecting = true;
    assert_eq!(hovered(&ws), Some(0));
    ws.selection_dragged = true;
    assert_eq!(
        hovered(&ws),
        None,
        "a drag shows no underline, status hint or hand cursor"
    );
    ws.selecting = false;
    ws.selection_dragged = false;
    assert_eq!(
        hovered(&ws),
        Some(0),
        "after release the link under the pointer is hovered again"
    );
}

/// The link under the pointer is not hovered while a drag selects, and
/// nothing else redraws or resets the cursor when the button comes up. The
/// release that ends a drag does both, so the hint and hand cursor return
/// without the pointer moving.
#[test]
fn releasing_a_selection_drag_restores_the_hovered_link_at_once() {
    let source = include_str!("window_events.rs");
    let release = source
        .split_once("if selection_release_matches(ws.selection_button, bcode) {")
        .expect("native selection release")
        .1;
    let release = release
        .split_once("\n                }\n")
        .expect("release body")
        .0;
    let dragged = release
        .find("let dragged = ws.selection_dragged;")
        .expect("the drag flag is read before the gesture clears it");
    let finish = release
        .find("self.finish_selection_gesture(ws);")
        .expect("the gesture is finished");
    let restore = release
        .split_once("if dragged {")
        .expect("a drag restores the hover")
        .1;
    assert!(dragged < finish);
    assert!(restore.contains("self.sync_cursor_icon(ws);"));
    assert!(restore.contains("w.request_redraw();"));
}

#[test]
fn a_selected_hovered_link_loses_its_underline_until_the_selection_clears() {
    let links = [link(2, 70, 79, 0), link(3, 0, 9, 0), link(5, 10, 20, 1)];
    let hover =
        |rects: &[kettle_render::LinkRect]| rects.iter().map(|r| r.hover).collect::<Vec<_>>();
    // A selection over the wrapped link's second row only.
    let selection = Some((selected((3, 0), (3, 4), false), 0));
    assert_eq!(
        hover(&link_rects(&links, Some(0), selection)),
        [false, false, false],
        "no segment of a selected hovered link is underlined"
    );
    assert_eq!(
        hover(&link_rects(&links, Some(1), selection)),
        [false, false, true],
        "a hovered link outside the selection keeps its underline"
    );
    // Select All, then the selection is cleared.
    let all = Some((selected((0, 0), (9, 79), false), 0));
    assert_eq!(
        hover(&link_rects(&links, Some(1), all)),
        [false, false, false]
    );
    assert_eq!(
        hover(&link_rects(&links, Some(1), None)),
        [false, false, true],
        "the underline returns once nothing is selected"
    );
    let rects = link_rects(&links, None, all);
    assert_eq!(
        (rects[0].col, rects[0].row, rects[0].width),
        (70, 2, 10),
        "every link keeps its rect"
    );
}

//! The hint that Shift keeps a click Kettle's while a program uses the mouse.
use super::accessibility::{
    ACCESSIBILITY_ROOT_ID, accessibility_modal_id, modal_accessibility_projection,
    modal_focus_in_tree, nest_context_menu_accessibility,
};
#[cfg(unix)]
use super::{
    Config, PtyGeometry, drain_pane_events, program_tracks_mouse, raise_mouse_override_hint,
    shift_menu_hint_due,
};
use super::{
    ContextMenu, ContextMenuItem, ContextMenuLevel, ContextMenuRow, ContextMenuState, MouseReport,
    Mux, Overlay, SUBMENU_SIGN, clamp_context_menu_panel, context_menu_info_columns,
    context_menu_item_columns, filter_disabled, fit_context_menu_row, grouped_terminal_menu,
    item_is_dispatchable, pane_tracks_mouse, prepend_mouse_override_row, production_source,
    right_click_hint_due, wrap_menu_info,
};
use crate::window_state::{MouseOverrideHint, WindowState};
use kettle_config::Action;
use kettle_core::TermMode;
use unicode_width::UnicodeWidthStr as _;

/// What a plain right-click raises in one window, given what became of its
/// press. `report` is what `send_mouse_to` answered for that pane.
fn right_click(
    hint: &mut MouseOverrideHint,
    pane: u64,
    hinted: &mut bool,
    report: MouseReport,
    now: std::time::Instant,
) -> bool {
    right_click_hint_due(2, report, false, false) && hint.right_click_reported(pane, hinted, now)
}

/// A plain right-click the program was sent raises the hint. One on a pane
/// with no mouse reporting was Kettle's own (the menu opened), and one Kettle
/// kept from the program (a search bar over it) reached nobody, so neither
/// does; nor does Shift, or another button.
#[test]
fn a_reported_plain_right_click_raises_the_hint_and_no_other_does() {
    let now = std::time::Instant::now();
    assert!(pane_tracks_mouse(TermMode::MOUSE_REPORT_CLICK, false));
    assert!(pane_tracks_mouse(
        TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE,
        false
    ));
    assert!(!pane_tracks_mouse(TermMode::empty(), false));
    assert!(
        !pane_tracks_mouse(TermMode::MOUSE_REPORT_CLICK, true),
        "a read-only pane's program gets no clicks"
    );

    let mut hint = MouseOverrideHint::default();
    let mut hinted = false;
    // No mouse reporting: the press is declined and Kettle opens its menu.
    assert!(!right_click(
        &mut hint,
        1,
        &mut hinted,
        MouseReport::Declined,
        now
    ));
    // The search bar swallowed the press.
    assert!(!right_click(
        &mut hint,
        1,
        &mut hinted,
        MouseReport::Swallowed,
        now
    ));
    assert!(!right_click_hint_due(2, MouseReport::Written, true, false));
    assert!(!right_click_hint_due(0, MouseReport::Written, false, false));
    assert!(!hinted);
    assert_eq!(hint.pane(), None);

    assert!(right_click(
        &mut hint,
        1,
        &mut hinted,
        MouseReport::Written,
        now
    ));
    assert_eq!(hint.pane(), Some(1));
    assert!(hinted);
}

/// With `putty-paste-style` on, Shift+right-click pastes into the program
/// rather than opening the menu, so a reported right-click raises no hint
/// that would tell the user to hold Shift.
#[test]
fn putty_paste_style_raises_no_hint() {
    assert!(right_click_hint_due(2, MouseReport::Written, false, false));
    assert!(!right_click_hint_due(2, MouseReport::Written, false, true));
}

/// The second reported right-click, in the same pane or in another pane of
/// the window within 30 s, raises nothing.
#[test]
fn the_throttle_suppresses_a_second_hint() {
    let now = std::time::Instant::now();
    let mut hint = MouseOverrideHint::default();
    let (mut first, mut second) = (false, false);
    assert!(right_click(
        &mut hint,
        1,
        &mut first,
        MouseReport::Written,
        now
    ));
    let soon = now + std::time::Duration::from_secs(2);
    assert!(!right_click(
        &mut hint,
        1,
        &mut first,
        MouseReport::Written,
        soon
    ));
    assert!(!right_click(
        &mut hint,
        2,
        &mut second,
        MouseReport::Written,
        soon
    ));
    let later = now + crate::window_state::MOUSE_OVERRIDE_HINT_GAP;
    assert!(right_click(
        &mut hint,
        2,
        &mut second,
        MouseReport::Written,
        later
    ));
}

/// Shift+right-click over a program that uses the mouse opens the menu with
/// the hint as its first row, a line that does nothing. Over a pane with no
/// mouse reporting the menu is unchanged.
#[test]
fn the_shift_menu_over_a_tracking_pane_leads_with_the_hint() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let rows = || {
            grouped_terminal_menu(
                [Action::Copy, Action::Paste]
                    .into_iter()
                    .map(|action| ContextMenuItem::Item {
                        label: tr.text(kettle_i18n::Text::MenuCopy),
                        action,
                        enabled: true,
                    })
                    .collect(),
                tr,
            )
        };
        let text = tr.text(kettle_i18n::Text::MouseOverrideHint);

        let mut menu = rows();
        prepend_mouse_override_row(
            &mut menu,
            pane_tracks_mouse(TermMode::MOUSE_REPORT_CLICK, false),
            200,
            tr,
        );
        let menu = filter_disabled(menu);
        assert!(
            matches!(&menu[0], ContextMenuItem::Info { label, .. } if label == text),
            "{:?}",
            menu.first().map(std::mem::discriminant)
        );
        assert!(matches!(menu[1], ContextMenuItem::Separator));
        assert!(!item_is_dispatchable(&menu[0]));
        assert_eq!(
            menu.iter().position(item_is_dispatchable),
            Some(2),
            "the highlight starts below the hint"
        );
        // The panel is measured from the same translated text it paints.
        assert!(context_menu_item_columns(&menu[0], "") >= text.width());

        let plain = rows();
        let mut menu = rows();
        prepend_mouse_override_row(
            &mut menu,
            pane_tracks_mouse(TermMode::empty(), false),
            200,
            tr,
        );
        assert_eq!(menu.len(), plain.len());
        assert!(matches!(
            menu[0],
            ContextMenuItem::Item {
                action: Action::Copy,
                ..
            }
        ));
        assert!(
            !menu
                .iter()
                .any(|item| matches!(item, ContextMenuItem::Info { .. }))
        );
    }
}

/// What the menu paints for `menu` on a surface `surface_w` pixels wide:
/// each row's label after the panel is measured, clamped to the surface and
/// each row fitted to it, as `show_context_menu` and the row projection do.
/// Also the panel's natural width, before the clamp.
fn painted_labels(menu: &[ContextMenuItem], surface_w: f32, cell_w: f32) -> (Vec<String>, f32) {
    let widest = menu
        .iter()
        .map(|item| context_menu_item_columns(item, ""))
        .max()
        .unwrap_or(0);
    let panel_w = kettle_render::menu::panel_width(widest as f32, cell_w);
    let (clamped_w, _) = clamp_context_menu_panel((panel_w, 1.0), (surface_w, 600.0));
    let columns = kettle_render::menu::panel_columns(clamped_w, cell_w);
    let labels = menu
        .iter()
        .filter_map(|item| match item {
            ContextMenuItem::Info { label, .. } => {
                let mut row = ContextMenuRow {
                    swatches: None,
                    matched_chars: None,
                    label: label.clone(),
                    separator: false,
                    enabled: false,
                    hint: String::new(),
                };
                fit_context_menu_row(&mut row, columns);
                Some(row.label)
            }
            _ => None,
        })
        .collect();
    (labels, panel_w)
}

/// On an 80-column window the hint is painted whole, in English and in
/// Spanish: it wraps to the menu's own width rather than running past the
/// panel and losing its end to an ellipsis, and it leaves the menu as wide
/// as it was. A window too narrow for the other rows wraps it to the window.
#[test]
fn the_shift_menu_hint_wraps_to_fit_instead_of_being_cut_off() {
    const WIDE: &str = "A menu row sixty columns wide, wider than a forty-column view";
    let cell_w = 8.0;
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let text = tr.text(kettle_i18n::Text::MouseOverrideHint);
        for (window_columns, labels) in [
            (
                80.0,
                vec![
                    tr.text(kettle_i18n::Text::MenuCopy),
                    tr.text(kettle_i18n::Text::MenuPaste),
                    tr.text(kettle_i18n::Text::MenuSplitRight),
                ],
            ),
            (40.0, vec![tr.text(kettle_i18n::Text::MenuCopy), WIDE]),
        ] {
            let surface_w = window_columns * cell_w;
            let rows: Vec<ContextMenuItem> = labels
                .into_iter()
                .map(|label| ContextMenuItem::Item {
                    label,
                    action: Action::Copy,
                    enabled: true,
                })
                .collect();
            let widest = rows
                .iter()
                .map(|item| context_menu_item_columns(item, ""))
                .max()
                .unwrap_or(0);
            let (_, plain_w) = painted_labels(&rows, surface_w, cell_w);
            let mut menu = rows;
            prepend_mouse_override_row(
                &mut menu,
                true,
                context_menu_info_columns(widest, surface_w, cell_w),
                tr,
            );
            let menu = filter_disabled(menu);
            let lines: Vec<&str> = menu
                .iter()
                .filter_map(|item| match item {
                    ContextMenuItem::Info { label, .. } => Some(label.as_str()),
                    _ => None,
                })
                .collect();
            assert!(lines.len() > 1, "{language:?} {window_columns}: {lines:?}");
            let (painted, hinted_w) = painted_labels(&menu, surface_w, cell_w);
            assert_eq!(
                painted, lines,
                "{language:?} {window_columns}: nothing is cut"
            );
            assert_eq!(painted.join(" "), text, "{language:?} {window_columns}");
            assert!(!painted.iter().any(|line| line.contains('\u{2026}')));
            assert!(
                hinted_w <= plain_w,
                "{language:?} {window_columns}: the hint widened the menu"
            );
        }
    }
    assert_eq!(wrap_menu_info("one two three", 7), ["one two", "three"]);
    assert_eq!(wrap_menu_info("a longword b", 3), ["a", "longword", "b"]);
    assert_eq!(wrap_menu_info("a b", 0), ["a", "b"]);
    assert!(wrap_menu_info("", 10).is_empty());
}

/// The pieces above are where the pointer, the menu, the notice and the
/// scheduler use them.
#[test]
fn the_hint_is_wired_into_reports_menus_notices_and_expiry() {
    let src = production_source();
    let report = src
        .split("fn report_mouse(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("report_mouse");
    let sent = report
        .find("let report = self.send_mouse_to(ws, pane_id, MouseAt::Pointer(rect), btn, pressed, motion);")
        .expect("the press is sent first");
    let raised = report
        .find("if !self.ctl_driving {\n                raise_mouse_override_hint(\n                    ws,\n                    pane_id,\n                    (btn, report),\n                    &self.cfg,\n                    std::time::Instant::now(),\n                );")
        .expect("a reported press raises the hint");
    assert!(sent < raised);

    let menu = src
        .split("fn open_context_menu(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("open_context_menu");
    let grouped = menu
        .find("let mut items = grouped_terminal_menu(items, tr);")
        .expect("grouping");
    let row = menu
        .find("prepend_mouse_override_row(&mut items, shift_over_tracking, info_columns, tr);")
        .expect("the hint row");
    assert!(grouped < row, "the row leads the grouped menu");
    assert!(menu.contains("let info_columns = self.context_menu_info_columns(ws, &items);"));
    assert!(menu.contains(
        "let shift_over_tracking = shift_menu_hint_due(ws, offer_copied, in_focused_pane, &self.cfg);"
    ));

    assert!(src.contains("let mouse_hint = ws.mouse_override_hint.pane() == Some(id);"));
    assert!(src.contains(".text(kettle_i18n::Text::MouseOverrideHint)"));
    assert!(src.contains(
        "if !primary && pane.osc52_notice.message.take().is_none() {\n                        ws.mouse_override_hint.dismiss();"
    ));
    assert!(src.contains("if ws.mouse_override_hint.expire(now) {"));
    assert!(src.contains("if let Some(next) = mouse_hint_wait {"));
    assert!(src.contains(
        "if pane.mouse_override_hinted\n            && let Ok(term) = pane.term.term.try_lock()\n            && !super::pointer::pane_tracks_mouse(*term.mode(), false)\n        {\n            pane.mouse_override_hinted = false;"
    ));
}

/// The panel the menu paints for `items` at `anchor`, as
/// `context_menu_overlay` builds it, tall enough to show every row.
fn painted_panel(items: &[ContextMenuItem], anchor: (f32, f32)) -> ContextMenu {
    let rows = items
        .iter()
        .map(|item| {
            let (label, enabled, hint) = match item {
                ContextMenuItem::Item { label, enabled, .. } => (label.to_string(), *enabled, ""),
                ContextMenuItem::Submenu { label, .. } => (label.clone(), true, SUBMENU_SIGN),
                ContextMenuItem::Info { label, .. } => (label.clone(), false, ""),
                _ => (String::new(), false, ""),
            };
            ContextMenuRow {
                swatches: None,
                matched_chars: None,
                label,
                separator: matches!(item, ContextMenuItem::Separator),
                enabled,
                hint: hint.to_string(),
            }
        })
        .collect();
    ContextMenu {
        picker_header_height: 0.0,
        anchor,
        rows,
        highlight: items.iter().position(item_is_dispatchable).unwrap_or(0),
        scroll_offset: 0,
        panel_w_clamped: 200.0,
        panel_h_clamped: 2000.0,
        a11y_label: None,
    }
}

/// What assistive technology gets for the menu of `kind` in `projection`:
/// the item nodes its Menu node holds, in order.
fn projected_items(
    projection: &super::accessibility::ModalAccessibilityProjection,
    kind: u64,
) -> Vec<(accesskit::NodeId, &accesskit::Node)> {
    let node = |id| {
        projection
            .nodes
            .iter()
            .find(|(node_id, _)| *node_id == id)
            .map(|(_, node)| node)
    };
    let menu = node(accessibility_modal_id(kind, 0)).expect("menu node");
    menu.children()
        .iter()
        .map(|id| (*id, node(*id).expect("every child is projected")))
        .collect()
}

/// A screen reader gets the Shift hint the menu wraps over several lines as
/// one item named with the whole sentence, not as a run of fragments, both
/// in the menu Shift+right-click opens and once a submenu of it is open and
/// it is the parent panel. That item covers every painted line and does
/// nothing; the rows after it keep their ids, so a click on one still finds
/// its row.
#[test]
fn a_screen_reader_reads_the_wrapped_shift_hint_as_one_sentence() {
    let row_h = 16.0 + kettle_render::menu::ROW_PAD;
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let text = tr.text(kettle_i18n::Text::MouseOverrideHint);
        let leaf = ContextMenuItem::Item {
            label: tr.text(kettle_i18n::Text::MenuSplitRight),
            action: Action::SplitRight,
            enabled: true,
        };
        let mut menu = vec![
            ContextMenuItem::Item {
                label: tr.text(kettle_i18n::Text::MenuCopy),
                action: Action::Copy,
                enabled: true,
            },
            ContextMenuItem::Submenu {
                label: "Split".into(),
                items: vec![leaf.clone()],
            },
        ];
        prepend_mouse_override_row(&mut menu, true, 16, tr);
        let menu = filter_disabled(menu);
        let lines = wrap_menu_info(text, 16);
        assert!(lines.len() > 2, "{language:?}: {lines:?}");
        let copy = lines.len() + 1;
        let split = copy + 1;

        let check = |projection: &super::accessibility::ModalAccessibilityProjection,
                     kind: u64,
                     anchor: (f32, f32)| {
            let items = projected_items(projection, kind);
            let labels: Vec<Option<&str>> = items.iter().map(|(_, node)| node.label()).collect();
            assert_eq!(
                labels,
                [
                    Some(text),
                    Some(tr.text(kettle_i18n::Text::MenuCopy)),
                    Some("Split")
                ],
                "{language:?} kind {kind}: one node for the sentence"
            );
            for line in &lines {
                assert!(
                    !projection
                        .nodes
                        .iter()
                        .any(|(_, node)| node.label() == Some(line.as_str())),
                    "{language:?} kind {kind}: the fragment {line:?} is read on its own"
                );
            }
            let (id, hint) = items[0];
            assert_eq!(id, accessibility_modal_id(kind, 1));
            assert_eq!(hint.role(), accesskit::Role::MenuItem);
            assert!(hint.is_disabled());
            assert!(!hint.is_hidden());
            assert_eq!(
                hint.bounds(),
                Some(accesskit::Rect::new(
                    f64::from(anchor.0),
                    f64::from(anchor.1),
                    f64::from(anchor.0 + 200.0),
                    f64::from(anchor.1 + row_h * lines.len() as f32),
                )),
                "{language:?} kind {kind}: the item covers every line"
            );
            assert_eq!(items[1].0, accessibility_modal_id(kind, copy + 1));
            assert_eq!(items[2].0, accessibility_modal_id(kind, split + 1));
        };

        // Open on its own.
        let mut ws = WindowState::new(1, Mux::new());
        ws.context_menu = Some(ContextMenuState {
            anchor: (40.0, 60.0),
            anchor_scale: 1.0,
            items: menu.clone(),
            highlight: copy,
            drill_stack: Vec::new(),
            scroll_offset: 0,
            typeahead_buf: String::new(),
            typeahead_until: None,
        });
        let overlay = Overlay {
            context_menu: Some(painted_panel(&menu, (40.0, 60.0))),
            ..Overlay::default()
        };
        let mut projection =
            modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &tr);
        nest_context_menu_accessibility(&mut projection, &ws, &overlay, 16.0);
        check(&projection, 2, (40.0, 60.0));
        assert_eq!(projection.focus, Some(accessibility_modal_id(2, copy + 1)));

        // Split is open, and the terminal menu is its parent panel.
        ws.context_menu = Some(ContextMenuState {
            anchor: (240.0, 60.0),
            anchor_scale: 1.0,
            items: vec![leaf.clone()],
            highlight: 0,
            drill_stack: vec![ContextMenuLevel {
                anchor: (40.0, 60.0),
                anchor_scale: 1.0,
                items: menu.clone(),
                highlight: split,
                scroll_offset: 0,
            }],
            scroll_offset: 0,
            typeahead_buf: String::new(),
            typeahead_until: None,
        });
        let overlay = Overlay {
            context_menu: Some(painted_panel(std::slice::from_ref(&leaf), (240.0, 60.0))),
            context_menu_parents: vec![painted_panel(&menu, (40.0, 60.0))],
            ..Overlay::default()
        };
        let mut projection =
            modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &tr);
        nest_context_menu_accessibility(&mut projection, &ws, &overlay, 16.0);
        check(&projection, 40, (40.0, 60.0));
    }
}

/// The tree `accessibility_tree` hands the platform for `projection` alone:
/// a window holding the modal's roots, focused where the modal's focus can
/// be named, with the tree info the first update carries.
fn finished_tree(
    projection: super::accessibility::ModalAccessibilityProjection,
) -> accesskit::TreeUpdate {
    let mut root = accesskit::Node::new(accesskit::Role::Window);
    root.set_children(projection.roots);
    let mut nodes = projection.nodes;
    nodes.push((ACCESSIBILITY_ROOT_ID, root));
    let focus = modal_focus_in_tree(projection.focus, &nodes).unwrap_or(ACCESSIBILITY_ROOT_ID);
    accesskit::TreeUpdate {
        nodes,
        tree: Some(accesskit::TreeInfo::new(ACCESSIBILITY_ROOT_ID)),
        tree_id: accesskit::TreeId::ROOT,
        focus,
    }
}

/// Hovering a line of the wrapped Shift hint other than its first
/// highlights that line's row, whose node the join took out of the tree. The
/// focus moves to the sentence, the item that holds that line, and the tree a
/// screen reader is handed names a focus it holds: AccessKit panics on one it
/// does not, with assistive technology running. This holds for the open menu
/// and once the menu is a parent panel and the hover leaves the submenu for
/// it.
#[test]
fn hovering_a_continued_line_of_the_hint_keeps_the_tree_valid() {
    for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
        let tr = kettle_i18n::Translator::new(language);
        let leaf = ContextMenuItem::Item {
            label: tr.text(kettle_i18n::Text::MenuSplitRight),
            action: Action::SplitRight,
            enabled: true,
        };
        let mut menu = vec![
            ContextMenuItem::Item {
                label: tr.text(kettle_i18n::Text::MenuCopy),
                action: Action::Copy,
                enabled: true,
            },
            ContextMenuItem::Submenu {
                label: "Split".into(),
                items: vec![leaf.clone()],
            },
        ];
        prepend_mouse_override_row(&mut menu, true, 16, tr);
        let menu = filter_disabled(menu);
        let lines = wrap_menu_info(tr.text(kettle_i18n::Text::MouseOverrideHint), 16);
        assert!(lines.len() > 2, "{language:?}: {lines:?}");
        let split = lines.len() + 2;
        for hovered in [1, lines.len() - 1] {
            for from_parent in [false, true] {
                let mut state = ContextMenuState {
                    anchor: (40.0, 60.0),
                    anchor_scale: 1.0,
                    items: menu.clone(),
                    highlight: split,
                    drill_stack: Vec::new(),
                    scroll_offset: 0,
                    typeahead_buf: String::new(),
                    typeahead_until: None,
                };
                if from_parent {
                    // Split is open, and the hover lands on the parent panel:
                    // `update_menu_highlight_from_cursor` leaves the submenu.
                    assert!(state.enter_submenu(split));
                    assert_eq!(state.drill_stack.len(), 1);
                    assert!(state.leave_submenu());
                }
                state.highlight = hovered;
                let mut panel = painted_panel(&menu, (40.0, 60.0));
                panel.highlight = hovered;
                let mut ws = WindowState::new(1, Mux::new());
                ws.context_menu = Some(state);
                let overlay = Overlay {
                    context_menu: Some(panel),
                    ..Overlay::default()
                };
                let mut projection =
                    modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &tr);
                nest_context_menu_accessibility(&mut projection, &ws, &overlay, 16.0);
                let first = accessibility_modal_id(2, 1);
                assert_eq!(
                    projection.focus,
                    Some(first),
                    "{language:?} line {hovered} from_parent {from_parent}: the sentence takes the focus"
                );
                assert!(
                    projection.nodes.iter().any(|(id, _)| *id == first),
                    "{language:?}: the sentence is projected"
                );
                let update = finished_tree(projection);
                assert_eq!(update.focus, first);
                assert!(
                    update.nodes.iter().any(|(id, _)| *id == update.focus),
                    "{language:?} line {hovered} from_parent {from_parent}: focus {:?} is not in the tree",
                    update.focus
                );
                let _ = accesskit_consumer::Tree::new(update, true);
            }
        }
    }
}

/// The modal's focus reaches the tree only while its node is there; else
/// the open menu takes it, and with no menu the caller's default applies.
#[test]
fn a_modal_focus_outside_the_tree_falls_back_to_the_menu() {
    let menu = accessibility_modal_id(2, 0);
    let row = accessibility_modal_id(2, 3);
    let gone = accessibility_modal_id(2, 2);
    let node = || accesskit::Node::new(accesskit::Role::MenuItem);
    let both = [(menu, node()), (row, node())];
    assert_eq!(modal_focus_in_tree(Some(row), &both), Some(row));
    assert_eq!(modal_focus_in_tree(Some(gone), &both), Some(menu));
    assert_eq!(modal_focus_in_tree(None, &both), None);
    assert_eq!(modal_focus_in_tree(Some(gone), &both[1..]), None);
}

/// An Info line of its own reading `text`: the hint's single line on a menu
/// wide enough for it, relabelled.
fn info_line(text: &str) -> ContextMenuItem {
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let mut items = Vec::new();
    prepend_mouse_override_row(&mut items, true, usize::MAX, tr);
    let mut line = items.remove(0);
    if let ContextMenuItem::Info { label, .. } = &mut line {
        *label = text.into();
    }
    line
}

/// Info lines of their own, such as the About panel's, are each read as
/// they are painted: only lines the menu wrapped from one sentence join.
#[test]
fn separate_info_lines_are_read_one_by_one() {
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let menu = vec![
        info_line("kettle 9.9.9"),
        info_line("Up to date"),
        ContextMenuItem::Separator,
        ContextMenuItem::Item {
            label: "Copy version",
            action: Action::Copy,
            enabled: true,
        },
    ];
    let mut ws = WindowState::new(1, Mux::new());
    ws.context_menu = Some(ContextMenuState {
        anchor: (40.0, 60.0),
        anchor_scale: 1.0,
        items: menu.clone(),
        highlight: 3,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        typeahead_buf: String::new(),
        typeahead_until: None,
    });
    let overlay = Overlay {
        context_menu: Some(painted_panel(&menu, (40.0, 60.0))),
        ..Overlay::default()
    };
    let mut projection =
        modal_accessibility_projection(&overlay, (800.0, 600.0), (8.0, 16.0), 1.0, &tr);
    nest_context_menu_accessibility(&mut projection, &ws, &overlay, 16.0);
    let labels: Vec<Option<&str>> = projected_items(&projection, 2)
        .iter()
        .map(|(_, node)| node.label())
        .collect();
    assert_eq!(
        labels,
        [
            Some("kettle 9.9.9"),
            Some("Up to date"),
            Some("Copy version")
        ]
    );
}

/// A window holding one real pane, a PTY running `cat`, whose program has
/// turned mouse reporting on (DECSET 1000) as Vim or htop does.
#[cfg(unix)]
fn tracking_window() -> (WindowState, u64) {
    let cfg = Config {
        shell_integration: false,
        login_shell: false,
        ..Config::default()
    };
    let mut mux = Mux::new();
    mux.new_tab_with_geometry(
        &cfg,
        PtyGeometry::from_cell_size(80, 24, 8, 16),
        std::sync::Arc::new(|| {}),
        &["/bin/cat".into()],
        None,
    )
    .expect("PTY fixture");
    let pane = mux.active_focus().unwrap();
    let ws = WindowState::new(1, mux);
    set_mouse_reporting(&ws, pane, true);
    (ws, pane)
}

/// The pane's program turns mouse reporting on or off.
#[cfg(unix)]
fn set_mouse_reporting(ws: &WindowState, pane: u64, on: bool) {
    let mut parser: alacritty_terminal::vte::ansi::Processor =
        alacritty_terminal::vte::ansi::Processor::new();
    let seq: &[u8] = if on { b"\x1b[?1000h" } else { b"\x1b[?1000l" };
    parser.advance(&mut *ws.mux.panes[&pane].term.term.lock().unwrap(), seq);
}

#[cfg(unix)]
fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready() {
        assert!(
            std::time::Instant::now() < deadline,
            "PTY fixture timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// What `report_mouse` does after a press it sent a pane whose terminal
/// has mouse reporting on: a plain right-click the program got raises the
/// hint on that pane, marks it and asks for a new accessibility tree; Shift,
/// another button or `putty-paste-style` raise nothing. Once the program
/// turns reporting off, the event drain ends the pane's session, so it
/// raises the hint again when the program turns reporting back on.
#[cfg(unix)]
#[test]
fn a_right_click_a_tracking_pane_got_raises_the_hint_until_its_program_lets_go() {
    let (mut ws, pane) = tracking_window();
    let cfg = Config::default();
    let putty = Config {
        putty_paste_style: true,
        ..Config::default()
    };
    let now = std::time::Instant::now();
    assert!(program_tracks_mouse(&ws, pane));
    ws.accessibility_pending = false;

    ws.mods = winit::keyboard::ModifiersState::SHIFT;
    assert!(!raise_mouse_override_hint(
        &mut ws,
        pane,
        (2, MouseReport::Written),
        &cfg,
        now
    ));
    ws.mods = winit::keyboard::ModifiersState::empty();
    for (press, cfg) in [
        ((0, MouseReport::Written), &cfg),
        ((2, MouseReport::Declined), &cfg),
        ((2, MouseReport::Swallowed), &cfg),
        ((2, MouseReport::Written), &putty),
    ] {
        assert!(!raise_mouse_override_hint(&mut ws, pane, press, cfg, now));
    }
    assert_eq!(ws.mouse_override_hint.pane(), None);
    assert!(!ws.mux.panes[&pane].mouse_override_hinted);
    assert!(!ws.accessibility_pending);

    assert!(raise_mouse_override_hint(
        &mut ws,
        pane,
        (2, MouseReport::Written),
        &cfg,
        now
    ));
    assert_eq!(ws.mouse_override_hint.pane(), Some(pane));
    assert!(ws.mux.panes[&pane].mouse_override_hinted);
    assert!(ws.accessibility_pending);

    // Past the window's gap, the same session still has had its hint.
    let later = now + crate::window_state::MOUSE_OVERRIDE_HINT_GAP;
    assert!(ws.mouse_override_hint.expire(later));
    drain_pane_events(&mut ws, cfg.exit_action, false, |_, _| None);
    assert!(ws.mux.panes[&pane].mouse_override_hinted);
    assert!(!raise_mouse_override_hint(
        &mut ws,
        pane,
        (2, MouseReport::Written),
        &cfg,
        later
    ));

    set_mouse_reporting(&ws, pane, false);
    assert!(!program_tracks_mouse(&ws, pane));
    wait_until(|| {
        drain_pane_events(&mut ws, cfg.exit_action, false, |_, _| None);
        !ws.mux.panes[&pane].mouse_override_hinted
    });
    set_mouse_reporting(&ws, pane, true);
    assert!(raise_mouse_override_hint(
        &mut ws,
        pane,
        (2, MouseReport::Written),
        &cfg,
        later
    ));
    assert_eq!(ws.mouse_override_hint.pane(), Some(pane));
}

/// What `open_context_menu` asks before it leads the menu with the hint, of
/// a window whose focused pane is real: Shift opened the menu inside that
/// pane, and its program has mouse reporting on. A menu opened from the
/// keyboard, a click outside the pane, a read-only pane and a program that
/// turned reporting off all go without it.
#[cfg(unix)]
#[test]
fn the_shift_menu_over_a_tracking_pane_takes_the_hint() {
    let (mut ws, pane) = tracking_window();
    let cfg = Config::default();
    assert!(shift_menu_hint_due(&ws, true, true, &cfg));
    assert!(
        !shift_menu_hint_due(&ws, false, true, &cfg),
        "a menu opened from the keyboard"
    );
    assert!(!shift_menu_hint_due(&ws, true, false, &cfg));
    ws.mux.panes.get_mut(&pane).unwrap().read_only = true;
    assert!(!shift_menu_hint_due(&ws, true, true, &cfg));
    ws.mux.panes.get_mut(&pane).unwrap().read_only = false;
    set_mouse_reporting(&ws, pane, false);
    assert!(!shift_menu_hint_due(&ws, true, true, &cfg));
}

/// With `putty-paste-style` on, Shift+right-click pastes, so no terminal
/// menu leads with the hint over a tracking pane: not one a control client
/// opens with Shift held, whose press is not turned into a paste, and not
/// one opened from the keyboard.
#[cfg(unix)]
#[test]
fn putty_paste_style_leaves_the_hint_out_of_the_menu() {
    let (ws, _) = tracking_window();
    let putty = Config {
        putty_paste_style: true,
        ..Config::default()
    };
    assert!(shift_menu_hint_due(&ws, true, true, &Config::default()));
    assert!(!shift_menu_hint_due(&ws, true, true, &putty));
    assert!(!shift_menu_hint_due(&ws, false, true, &putty));
}

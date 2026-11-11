//! The hint that Shift keeps a click Kettle's while a program uses the mouse.
use super::{
    ContextMenuItem, ContextMenuRow, MouseReport, clamp_context_menu_panel,
    context_menu_info_columns, context_menu_item_columns, filter_disabled, fit_context_menu_row,
    grouped_terminal_menu, item_is_dispatchable, pane_tracks_mouse, prepend_mouse_override_row,
    production_source, right_click_hint_due, wrap_menu_info,
};
use crate::window_state::MouseOverrideHint;
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
            matches!(&menu[0], ContextMenuItem::Info { label } if label == text),
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
            ContextMenuItem::Info { label } => {
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
                    ContextMenuItem::Info { label } => Some(label.as_str()),
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
        .find("if !self.ctl_driving\n                && right_click_hint_due(\n                    btn,\n                    report,\n                    ws.mods.shift_key(),\n                    self.cfg.putty_paste_style,\n                )\n            {\n                self.raise_mouse_override_hint(ws, pane_id);")
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
        "let shift_over_tracking = offer_copied\n            && in_focused_pane\n            && ws\n                .mux\n                .active_focus()\n                .is_some_and(|pane| self.pane_program_tracks_mouse(ws, pane));"
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

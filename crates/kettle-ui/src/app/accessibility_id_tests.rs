//! AccessKit ids: every node the window hands a screen reader has an id no
//! other node can have, whatever its pane, row, card or control.

use super::*;

/// Every AccessKit id the window hands out is its own: the new kept-history
/// and viewing-history ids beside the pane, text row, button, scrollbar,
/// output, tab, notice, lane, card, modal, completion, search, receipt,
/// toast and window-control ids, for small and large pane ids.
#[test]
fn accessibility_ids_do_not_collide() {
    use super::super::notification_toast::{
        ACCESSIBILITY_NOTIFICATION_DISMISS_ID, ACCESSIBILITY_NOTIFICATION_SHOW_ID,
        ACCESSIBILITY_NOTIFICATION_STATUS_ID,
    };
    use kettle_render::{SearchControl, WindowControl};
    let mut ids: Vec<(String, accesskit::NodeId)> = Vec::new();
    let mut add = |what: String, id: accesskit::NodeId| ids.push((what, id));
    let panes = [1_u64, 2, 3, 7, 8, 255, 4096, 65_535, 1 << 20];
    for pane in panes {
        add(format!("pane {pane}"), accessibility_pane_id(pane));
        add(format!("text {pane}"), accessibility_text_id(pane));
        add(format!("button {pane}"), accessibility_scrollback_id(pane));
        add(format!("kept {pane}"), accessibility_kept_history_id(pane));
        add(
            format!("viewing {pane}"),
            accessibility_viewing_history_id(pane),
        );
        add(
            format!("scrollbar {pane}"),
            accessibility_scrollbar_id(pane),
        );
        for serial in [0, 1] {
            add(
                format!("output {pane} {serial}"),
                accessibility_output_id(pane, serial),
            );
        }
        for part in 0..4 {
            add(
                format!("notice {pane} {part}"),
                persistent_notice_id(Some(pane), part),
            );
        }
        for part in 0..LANE_ACCESSIBILITY_PARTS {
            if let Some(id) = accessibility_lane_id(pane, part) {
                add(format!("lane {pane} {part}"), id);
            }
        }
        for close in [false, true] {
            add(
                format!("tab {pane} {close}"),
                accessibility_tab_id(pane, close),
            );
        }
    }
    for part in 0..4 {
        add(
            format!("window notice {part}"),
            persistent_notice_id(None, part),
        );
    }
    for _ in 0..64 {
        add("row".into(), next_terminal_accessibility_row_id());
    }
    for instance in [1_u64, 2, 1000, (1 << 20) + 1] {
        add(format!("card {instance}"), accessibility_card_id(instance));
    }
    for kind in 0..12 {
        for index in 0..24 {
            add(
                format!("modal {kind} {index}"),
                accessibility_modal_id(kind, index),
            );
        }
        add(
            format!("modal text {kind}"),
            accessibility_modal_text_run_id(kind),
        );
    }
    for position in 0..64 {
        add(
            format!("completion {position}"),
            accessibility_completion_row_id(position),
        );
    }
    for control in SearchControl::ALL {
        add(
            format!("search {control:?}"),
            accessibility_search_control_id(control),
        );
    }
    for control in [
        WindowControl::Minimize,
        WindowControl::Maximize,
        WindowControl::Close,
    ] {
        add(
            format!("window control {control:?}"),
            window_control_accessibility_id(control),
        );
    }
    for (what, id) in [
        ("root", ACCESSIBILITY_ROOT_ID),
        ("search container", ACCESSIBILITY_SEARCH_CONTAINER_ID),
        (
            "completion container",
            ACCESSIBILITY_COMPLETION_CONTAINER_ID,
        ),
        ("receipt", ACCESSIBILITY_MEDIA_RECEIPT_ID),
        ("receipt dismiss", ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID),
        ("update details", ACCESSIBILITY_UPDATE_DETAILS_ID),
        ("update dismiss", ACCESSIBILITY_UPDATE_DISMISS_ID),
        ("header find", ACCESSIBILITY_HEADER_FIND_ID),
        ("notification status", ACCESSIBILITY_NOTIFICATION_STATUS_ID),
        ("notification show", ACCESSIBILITY_NOTIFICATION_SHOW_ID),
        (
            "notification dismiss",
            ACCESSIBILITY_NOTIFICATION_DISMISS_ID,
        ),
        ("search text", ACCESSIBILITY_SEARCH_TEXT_ID),
        ("search status", ACCESSIBILITY_SEARCH_STATUS_ID),
        ("search count", ACCESSIBILITY_SEARCH_COUNT_ID),
        ("update status", ACCESSIBILITY_UPDATE_STATUS_ID),
        ("font size", ACCESSIBILITY_FONT_SIZE_ID),
        ("tab list", ACCESSIBILITY_TAB_LIST_ID),
        ("tab new", ACCESSIBILITY_TAB_NEW_ID),
        ("tab menu", ACCESSIBILITY_TAB_MENU_ID),
        ("tab settings", ACCESSIBILITY_TAB_SETTINGS_ID),
    ] {
        add(what.into(), id);
    }
    let mut seen: std::collections::HashMap<u64, &str> = std::collections::HashMap::new();
    for (what, id) in &ids {
        if let Some(other) = seen.insert(id.0, what) {
            panic!("{what} and {other} share id {:#x}", id.0);
        }
    }
    assert!(ids.len() > 800);
}

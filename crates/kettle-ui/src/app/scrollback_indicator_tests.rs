//! The jump-to-bottom button, the "Start of kept history" label and the
//! "Viewing history" status, as the pointer, AccessKit and `kettle ctl
//! ui_geometry` see them: the rects the renderer paints, and nothing a
//! screen reader would hear twice.

use super::accessibility::{scrollback_accessibility_nodes, viewing_history_pane};
use super::{
    kept_history_label_json, production_source, rect_contains, scrollback_pill_hit,
    scrollback_pill_json,
};
use crate::window_state::WindowState;
use accesskit::{Action, Live, Node, Role};
use kettle_i18n::{Language, Translator};
use kettle_render::{HistoryView, Rect4, ScrollbackPillState};

const PANE: u64 = 7;

fn terminal(scale: f32) -> Rect4 {
    (40.0 * scale, 30.0 * scale, 800.0 * scale, 920.0 * scale)
}

fn pill(
    scale: f32,
    offset: usize,
    hint: Option<&str>,
    tr: &Translator,
) -> kettle_render::ScrollbackPill {
    kettle_render::scrollback_pill(
        terminal(scale),
        offset,
        scale,
        (8.0 * scale, 16.0 * scale),
        6.0,
        hint,
        tr,
    )
    .expect("a button")
}

fn kept(scale: f32, tr: &Translator) -> kettle_render::KeptHistoryLabel {
    kettle_render::kept_history_label(
        terminal(scale),
        HistoryView {
            offset: 10_000,
            size: 10_000,
            origin: 90_000,
        },
        scale,
        (8.0 * scale, 16.0 * scale),
        6.0,
        tr,
    )
    .expect("a label")
}

fn rect_of(node: &Node) -> Rect4 {
    let bounds = node.bounds().expect("bounds");
    (
        bounds.x0 as f32,
        bounds.y0 as f32,
        (bounds.x1 - bounds.x0) as f32,
        (bounds.y1 - bounds.y0) as f32,
    )
}

fn json_rect(value: &serde_json::Value) -> Rect4 {
    let field = |name: &str| value[name].as_f64().expect(name) as f32;
    (field("x"), field("y"), field("width"), field("height"))
}

fn by_role(nodes: &[(accesskit::NodeId, Node)], role: Role) -> Vec<&Node> {
    nodes
        .iter()
        .filter(|(_, node)| node.role() == role)
        .map(|(_, node)| node)
        .collect()
}

/// The button the renderer paints is the one the pointer hits, AccessKit
/// bounds and `ui_geometry` reports, at 1x and 2x, with its key and
/// without. AccessKit names the key as the button's shortcut; `ui_geometry`
/// gives the chord, the rule before it and the state the button paints.
#[test]
fn the_pointer_accessibility_and_ctl_read_the_painted_jump_button() {
    let tr = Translator::default();
    for scale in [1.0_f32, 2.0] {
        for hint in [Some("Ctrl+End"), None] {
            let pill = pill(scale, 360, hint, &tr);
            let (x, y, w, h) = pill.rect;
            let context = format!("scale {scale} hint {hint:?}");

            // The pointer: inside the painted rect and nowhere else.
            let hit = |px: f32, py: f32| scrollback_pill_hit([(PANE, pill.clone())], (px, py));
            for (px, py) in [
                (x, y),
                (x + w / 2.0, y + h / 2.0),
                (x + w - 0.5, y + h - 0.5),
            ] {
                assert_eq!(hit(px, py), Some(PANE), "{context}: ({px}, {py})");
                assert!(rect_contains(pill.rect, px, py));
            }
            for (px, py) in [
                (x - 1.0, y + h / 2.0),
                (x + w, y + h / 2.0),
                (x + w / 2.0, y - 1.0),
                (x + w / 2.0, y + h),
            ] {
                assert_eq!(hit(px, py), None, "{context}: ({px}, {py})");
            }

            // AccessKit: a button with those bounds, a Click action and the
            // chord as its shortcut.
            let nodes = scrollback_accessibility_nodes(
                PANE,
                terminal(scale),
                Some(&pill),
                None,
                false,
                &tr,
            );
            assert_eq!(nodes.len(), 1, "{context}");
            let button = &nodes[0].1;
            assert_eq!(button.role(), Role::Button, "{context}");
            assert_eq!(rect_of(button), pill.rect, "{context}");
            assert_eq!(
                button.label(),
                Some("Return to live output (360 lines above bottom)")
            );
            assert_eq!(button.keyboard_shortcut(), hint, "{context}");
            assert!(button.supports_action(Action::Click), "{context}");

            // `ui_geometry`: the same rect, the chord and its rule.
            for state in [
                ScrollbackPillState::Rest,
                ScrollbackPillState::Hovered,
                ScrollbackPillState::Pressed,
            ] {
                let json = scrollback_pill_json(PANE, &pill, state);
                assert_eq!(json["pane"], PANE, "{context}");
                assert_eq!(json_rect(&json["rect"]), pill.rect, "{context}");
                assert_eq!(json["state"], state.name(), "{context}");
                match &pill.hint {
                    Some(shown) => {
                        assert_eq!(json["hint"]["text"], shown.text.as_str(), "{context}");
                        assert_eq!(json_rect(&json["hint"]["divider"]), shown.divider);
                    }
                    None => assert!(json["hint"].is_null(), "{context}"),
                }
            }
        }
    }
    // The count is not in `ui_geometry`: geometry only.
    let json = scrollback_pill_json(PANE, &pill(1.0, 360, None, &tr), ScrollbackPillState::Rest);
    assert!(!json.to_string().contains("360"));
}

/// The label is a text node bounded where it is painted, read as what it
/// shows, and `ui_geometry` reports the same rect, at 1x and 2x, in English
/// and Spanish.
#[test]
fn accessibility_and_ctl_read_the_painted_kept_history_label() {
    for (tr, text) in [
        (
            Translator::default(),
            "Start of kept history \u{b7} 10,000 lines",
        ),
        (
            Translator::new(Language::Es),
            "Inicio del historial conservado \u{b7} 10,000 l\u{ed}neas",
        ),
    ] {
        for scale in [1.0_f32, 2.0] {
            let label = kept(scale, &tr);
            assert_eq!(label.label, text);
            let nodes = scrollback_accessibility_nodes(
                PANE,
                terminal(scale),
                None,
                Some(&label),
                false,
                &tr,
            );
            assert_eq!(nodes.len(), 1);
            let node = &nodes[0].1;
            assert_eq!(node.role(), Role::Label);
            // Named by its value too, as Orca and Narrator read a label.
            assert_eq!(node.label(), Some(text));
            assert_eq!(node.value(), Some(text));
            assert_eq!(rect_of(node), label.rect, "scale {scale}");
            assert!(!node.supports_action(Action::Click));
            assert_eq!(node.live(), None, "the label is not announced");

            let json = kept_history_label_json(PANE, &label);
            assert_eq!(json["pane"], PANE);
            assert_eq!(json_rect(&json["rect"]), label.rect, "scale {scale}");
            assert_eq!(json["counted"], true);
        }
    }
}

/// One polite "Viewing history" status while the pane is the focused one
/// and scrolled back, and none otherwise. It carries no count and keeps the
/// terminal's bounds, so the node is identical from one offset to the next,
/// across a change in the count's digits and with or without the button
/// and the label beside it: scrolling further announces nothing again.
#[test]
fn the_viewing_history_status_does_not_change_as_the_view_scrolls() {
    for (tr, text) in [
        (Translator::default(), "Viewing history"),
        (Translator::new(Language::Es), "Viendo el historial"),
    ] {
        let status = |offset: Option<usize>, viewing: bool| {
            let pill = offset.map(|offset| pill(1.0, offset, Some("Ctrl+End"), &tr));
            let nodes = scrollback_accessibility_nodes(
                PANE,
                terminal(1.0),
                pill.as_ref(),
                None,
                viewing,
                &tr,
            );
            let found: Vec<_> = nodes
                .into_iter()
                .filter(|(_, node)| node.role() == Role::Status)
                .collect();
            assert!(found.len() <= 1, "one status at most");
            found.into_iter().next()
        };
        assert!(status(Some(360), false).is_none(), "an unfocused pane");
        assert!(status(None, false).is_none(), "live output");
        let (id, node) = status(Some(360), true).expect("a status");
        assert_eq!(node.label(), Some(text));
        assert_eq!(node.live(), Some(Live::Polite));
        assert_eq!(rect_of(&node), terminal(1.0));
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
        for offset in [1, 361, 999, 1_000, 10_000] {
            assert_eq!(
                status(Some(offset), true),
                Some((id, node.clone())),
                "{offset} lines back"
            );
        }
        // A pane too small for the button still says it is in history.
        assert_eq!(status(None, true), Some((id, node.clone())));

        // Beside the button and the label, each once, the button's name
        // carrying the count the status leaves out.
        let pill = pill(1.0, 10_000, Some("Ctrl+End"), &tr);
        let label = kept(1.0, &tr);
        let all = scrollback_accessibility_nodes(
            PANE,
            terminal(1.0),
            Some(&pill),
            Some(&label),
            true,
            &tr,
        );
        assert_eq!(all.len(), 3);
        assert_eq!(by_role(&all, Role::Button).len(), 1);
        assert_eq!(by_role(&all, Role::Label).len(), 1);
        assert_eq!(by_role(&all, Role::Status), vec![&node]);
        let mut ids: Vec<_> = all.iter().map(|(id, _)| *id).collect();
        ids.sort_by_key(|id| id.0);
        ids.dedup();
        assert_eq!(ids.len(), 3, "three distinct nodes");
        // Another pane's nodes are other nodes.
        let other = scrollback_accessibility_nodes(
            PANE + 1,
            terminal(1.0),
            Some(&pill),
            Some(&label),
            true,
            &tr,
        );
        assert!(other.iter().all(|(id, _)| !ids.contains(id)));
    }
}

/// Two real panes, the first focused. A pane kills its shell when dropped.
struct Panes {
    ws: WindowState,
    focused: u64,
    other: u64,
}

impl Panes {
    fn new() -> Self {
        let (mux, focused, other) = super::tests::title_test_split_mux();
        let panes = Self {
            ws: WindowState::new(1, mux),
            focused,
            other,
        };
        for pane in [focused, other] {
            let mut term = panes.ws.mux.panes[&pane].term.term.lock().unwrap();
            let mut parser: alacritty_terminal::vte::ansi::Processor = Default::default();
            parser.advance(&mut *term, "history\r\n".repeat(2_000).as_bytes());
        }
        panes
    }

    fn scroll(&self, pane: u64, to: usize) {
        let mut term = self.ws.mux.panes[&pane].term.term.lock().unwrap();
        term.scroll_display(kettle_core::Scroll::Bottom);
        term.scroll_display(kettle_core::Scroll::Delta(to as i32));
        assert_eq!(term.grid().display_offset(), to);
    }
}

impl Drop for Panes {
    fn drop(&mut self) {
        self.ws.mux.kill_children();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !self
            .ws
            .mux
            .panes
            .values()
            .all(|pane| pane.term.child_exited())
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// The status belongs to the focused pane while it is scrolled back: not to
/// a pane at its live output, not to a scrolled-back pane that is not
/// focused, and to the same pane at every offset.
#[test]
fn only_the_focused_scrolled_back_pane_is_viewing_history() {
    let mut panes = Panes::new();
    let (focused, other) = (panes.focused, panes.other);
    assert_eq!(panes.ws.mux.active_focus(), Some(focused));
    assert_eq!(viewing_history_pane(&panes.ws), None, "live output");
    panes.scroll(other, 360);
    assert_eq!(
        viewing_history_pane(&panes.ws),
        None,
        "only the unfocused pane is scrolled back"
    );
    for offset in [1, 360, 361, 999, 1_000] {
        panes.scroll(focused, offset);
        assert_eq!(viewing_history_pane(&panes.ws), Some(focused), "{offset}");
    }
    panes.scroll(focused, 0);
    assert_eq!(viewing_history_pane(&panes.ws), None, "back at live output");
    // Focus moves to the pane that is still scrolled back.
    assert!(panes.ws.mux.focus_pane(other));
    assert_eq!(viewing_history_pane(&panes.ws), Some(other));
}

/// The tree, the pointer, both input paths and the frame take the button,
/// the label and their state from those shared helpers.
#[test]
fn the_jump_button_and_label_are_wired_to_the_painted_geometry() {
    let source: String = production_source().split_whitespace().collect();
    for needle in [
        // One layout call each, fed the live bindings.
        "kettle_render::scrollback_pill_hint(&self.cfg.keybinds).as_deref(),",
        "kettle_render::HistoryView::of(&*pane.term.term.lock().ok()?);",
        // AccessKit.
        "self.scrollback_pill_for(ws,pane_id,partition.terminal).as_ref(),",
        "self.kept_history_label_for(ws,pane_id,partition.terminal).as_ref(),",
        "viewing_history_pane(ws)==Some(pane_id),",
        "viewing_history_pane(ws).hash(&muthasher);",
        "pill.hint.as_ref().map(|hint|&hint.text).hash(&muthasher);",
        // `ui_geometry`.
        "Some(scrollback_pill_json(*pane,&pill,state))",
        "Some(kept_history_label_json(*pane,&label))",
        "\"kept_history_labels\".into(),",
        // The frame paints the state the pointer set.
        "scrollback_pill_hovered:ws.scrollback_pill_hover,",
        "scrollback_pill_pressed:ws.scrollback_pill_press,",
        // The hand, the hover fill, the press and the release all take the
        // one target, and it is the covers' rule.
        "letpill_hover=self.scrollback_pill_target(ws,cursor_x,cursor_y);",
        ".or(pill_hover.map(|_|CursorIcon::Pointer))",
        "letreached=self.scrollback_pill_target(ws,px,py);Self::press_reached_scrollback_pill(ws,reached,button)",
        "letreached=self.scrollback_pill_target(ws,px,py);Self::release_reached_scrollback_pill(ws,reached,button)",
        "scrollback_pill_reach(Some(hit),&self.scrollback_pill_covers(ws,px,py),(px,py),)",
    ] {
        assert!(source.contains(needle), "missing: {needle}");
    }
    for (needle, count, why) in [
        (
            "kettle_render::scrollback_pill(",
            1,
            "one place lays the button out",
        ),
        (
            "kettle_render::kept_history_label(",
            1,
            "one place lays the label out",
        ),
        (
            "ifself.release_scrollback_pill(ws,px,py,bcode){",
            2,
            "the native and the ctl release both end the press",
        ),
        (
            "ifself.press_scrollback_pill(ws,px,py,bcode){",
            2,
            "the native and the ctl press both arm it",
        ),
        (
            "scrollback_pill_hovered:ws.scrollback_pill_hover,",
            2,
            "both overlays carry the hover",
        ),
        (
            "self.scrollback_pill_target(",
            3,
            "the hover, the press and the release ask the one target",
        ),
    ] {
        assert_eq!(source.matches(needle).count(), count, "{why}");
    }
    // A press no longer scrolls: only its release, or AccessKit's Click.
    assert_eq!(
        source
            .matches("Self::return_pane_to_live(ws,pressed);")
            .count(),
        1
    );
    assert_eq!(
        source
            .matches("Self::return_pane_to_live(ws,pane);")
            .count(),
        1
    );
}

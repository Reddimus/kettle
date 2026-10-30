/// A preview lane takes no keys until the user gives it the keyboard
/// with `focus_preview`: while lanes show, every key goes on to the
/// terminal beside them, so nothing typed is lost to a lane.
#[test]
fn a_preview_lane_takes_no_keys_until_given_the_keyboard() {
    let source = super::production_source();
    let owns = source
        .split("let ui_owns_key = ")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("ui_owns_key");
    assert!(owns.contains("|| ws.preview_focus.is_some()"), "{owns}");
    assert!(!owns.contains("lane"), "{owns}");
    assert_eq!(
        source.matches("ws.preview_focus = Some(").count(),
        1,
        "only focus_preview gives a lane the keyboard"
    );
    assert!(source.contains("Action::FocusPreview => self.focus_preview(ws),"));
    assert!(
        !source.contains("fn media_viewer_takes_key("),
        "no key list of its own"
    );
}
/// Plain motion over a lane reaches no program, though a drag a press in
/// the terminal began still does; lanes stay drawn under modals; under a
/// dialog or menu a lane leaves the accessibility tree and answers no
/// action; and a lane with no room to show at all is refused with a
/// notice, leaving its item unseen.
#[test]
fn a_lane_is_out_of_reach_where_it_cannot_be_seen_or_pressed() {
    let source = super::production_source();
    let body = |name: &str| {
        source
            .split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name} present"))
    };
    let report = body("report_mouse");
    let held = report.find("MouseAt::Held {").expect("held drags");
    let motion = report
        .find("if motion && self.preview_lane_under_pointer(ws) {\n            return MouseReport::Declined;")
        .expect("motion over a lane is declined");
    let pointer = report
        .find("MouseAt::Pointer(rect)")
        .expect("pointer reports");
    assert!(held < motion && motion < pointer);
    let overlay = body("overlay");
    assert!(overlay.contains("let media_lanes = self.preview_lane_projections(ws);"));
    assert_eq!(
        overlay
            .lines()
            .filter(|line| line.trim() == "media_lanes,")
            .count(),
        2,
        "both the search and non-search overlays must retain preview lanes"
    );
    let tree = body("accessibility_tree");
    assert!(tree.contains("let lanes_reachable = !self.non_search_modal_open(ws);"));
    assert!(tree.contains(".filter(|_| lanes_reachable)"));
    assert!(
        body("handle_accessibility_action").contains("if !self.preview_is_reachable(ws, pane)")
    );
    assert!(body("preview_is_reachable").contains("!self.non_search_modal_open(ws)"));
    assert!(body("preview_is_reachable").contains("ws.preview_overlay.is_none_or"));
    let show = body("show_media_item");
    let refused = show
        .find("if !shown && !had_panel {")
        .expect("no room is refused");
    let notice = show
        .find("kettle_i18n::Text::NotifyTitlePreviewNoRoom")
        .expect("with a notice");
    let viewed = show
        .find("state.media_shelf.viewed(item);")
        .expect("viewed");
    assert!(refused < notice && notice < viewed);
}

/// The shelf's own shortcut opens the focused pane's lane on its newest
/// item, or closes that lane if it has one, once per press.
#[test]
fn the_shelf_shortcut_toggles_the_focused_panes_lane() {
    assert!(super::toggles_once_per_press(
        &kettle_config::keybinds::Action::OpenMediaShelf
    ));
    let source = super::production_source();
    let open = source
        .split("fn open_media_shelf(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("open_media_shelf");
    let close = open
        .find("if ws.preview_panels.contains_key(&pane) {\n            self.close_preview(ws, pane);\n            return;")
        .expect("the shortcut closes the focused pane's lane");
    let show = open
        .find("self.show_media_item(ws, pane, item);")
        .expect("otherwise it shows the newest item");
    assert!(close < show);
}

/// A release opens the card it pressed only over that very card, and
/// only while nothing that owns the pointer has opened over it.
#[test]
fn a_card_release_opens_only_the_card_it_pressed() {
    let nonce = kettle_core::InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
    let other = kettle_core::InlineNonce::new([6, 5, 4, 3, 2, 1]).unwrap();
    let card = |rect| kettle_render::PaintedCard {
        pane: 7,
        nonce,
        rect,
    };
    let here = card([40.0, 16.0, 96.0, 48.0]);
    let below = card([40.0, 96.0, 96.0, 48.0]);
    assert!(super::card_release_opens((7, nonce), Some(here), false));
    assert!(super::card_release_opens((7, nonce), Some(below), false));
    assert!(!super::card_release_opens((7, nonce), Some(here), true));
    assert!(!super::card_release_opens((7, nonce), None, false));
    assert!(!super::card_release_opens((7, other), Some(here), false));
    assert!(!super::card_release_opens((8, nonce), Some(here), false));
}

/// Each inline card on screen is a button assistive technology can name
/// and press: its own node id, apart from every other node's and never
/// its nonce, its item's title and the viewer's detail in either
/// language, the part of it on screen as bounds, and Focus and Click.
#[test]
fn a_card_is_a_button_assistive_technology_can_open() {
    use accesskit::{Action, NodeId, Role};
    use kettle_i18n::{Language, Text, Translator};
    let id = super::accessibility_card_id(42);
    assert_eq!(super::accessibility_card_instance(id), Some(42));
    let last = crate::media::MAX_CARD_INSTANCE - 1;
    assert_eq!(
        super::accessibility_card_instance(super::accessibility_card_id(last)),
        Some(last)
    );
    for other in [
        super::ACCESSIBILITY_ROOT_ID,
        super::accessibility_pane_id(42),
        super::accessibility_text_id(42),
        super::accessibility_modal_id(1, 2),
        super::ACCESSIBILITY_MEDIA_RECEIPT_ID,
        super::ACCESSIBILITY_SEARCH_CONTAINER_ID,
        super::ACCESSIBILITY_COMPLETION_CONTAINER_ID,
        NodeId(super::ACCESSIBILITY_CARD_ID_MASK),
        NodeId(super::ACCESSIBILITY_CARD_ID_MASK | super::ACCESSIBILITY_MODAL_ID_MASK | 42),
    ] {
        assert_eq!(super::accessibility_card_instance(other), None, "{other:?}");
    }
    for language in [Language::En, Language::Es] {
        let tr = Translator::new(language);
        let node = super::card_accessibility_node(
            [40.0, 16.0, 96.0, 48.0],
            "Click test",
            "Image · 640×480 · From this pane",
            &tr,
        );
        assert_eq!(node.role(), Role::Button);
        let label = node.label().expect("label");
        assert!(
            label.contains("Click test") && label.contains("640×480"),
            "{label}"
        );
        // A sender's path never brings a card's marks along.
        let marked = super::card_accessibility_node(
            [0.0, 0.0, 8.0, 8.0],
            "plot",
            "Image · 1×1 · /tmp/\u{10eeee}\u{0305}\u{030d}/tool",
            &tr,
        );
        let marked = marked.label().expect("label");
        assert!(
            !marked.contains('\u{10eeee}') && !marked.contains('\u{0305}'),
            "{marked:?}"
        );
        assert_eq!(node.description(), Some(tr.text(Text::CardA11yOpen)));
        assert!(node.supports_action(Action::Click) && node.supports_action(Action::Focus));
        let bounds = node.bounds().expect("bounds");
        assert_eq!(
            (bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            (40.0, 16.0, 136.0, 64.0)
        );
    }
}

/// In quick select a card on screen is one target, labelled at its first
/// visible cell, among the text targets in reading order; text found in
/// a card's rows is its marks, not something to pick.
#[test]
fn a_card_is_a_quick_select_target_and_text_inside_it_is_not() {
    use kettle_core::hints::{HintSpan, Kind};
    let nonce = kettle_core::InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
    let span = |row, start, text: &str| HintSpan {
        row,
        start,
        end: start + text.len() - 1,
        kind: Kind::Url,
        text: text.to_string(),
        bounded: true,
    };
    let cards = [((2, 4), (9, 30), nonce)];
    let found = super::hint_targets_with_cards(
        vec![
            span(10, 0, "https://after.example"),
            span(3, 5, "https://inside.example"),
            span(2, 31, "https://beside.example"),
            span(1, 0, "https://before.example"),
        ],
        &cards,
    );
    let found: Vec<(usize, usize, String)> = found
        .into_iter()
        .map(|(row, col, what)| {
            let what = match what {
                super::HintWhat::Card(card) => {
                    assert_eq!(card, nonce);
                    "card".to_string()
                }
                super::HintWhat::Text { text, .. } => text,
            };
            (row, col, what)
        })
        .collect();
    assert_eq!(
        found,
        [
            (1, 0, "https://before.example".to_string()),
            (2, 4, "card".to_string()),
            (2, 31, "https://beside.example".to_string()),
            (10, 0, "https://after.example".to_string()),
        ]
    );
}

/// A click, a hint and assistive technology all open a card through one
/// door that refuses under a dialog, a menu or the viewer and checks the
/// item is still on the shelf; the tree names only cards on screen, by
/// instance, and a key press returns focus from a card to its pane.
#[test]
fn every_way_to_open_a_card_goes_through_one_door() {
    let source = super::production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let body = |name: &str| {
        let start = [format!("fn {name}("), format!("fn {name}<")]
            .iter()
            .find_map(|head| source.find(head.as_str()))
            .unwrap_or_else(|| panic!("{name}"));
        let rest = &source[start..];
        rest[..rest[1..].find(" fn ").map_or(rest.len(), |end| end + 1)].to_string()
    };
    let guard = "if self.pointer_modal_open(ws) {";
    let open = body("open_card");
    assert!(
        open.contains(guard)
            && open.contains("state.media_shelf.items().iter().any(|it| it.id == item)")
    );
    let reachable = body("accessible_cards");
    assert!(reachable.contains(guard));
    // Only cards in panes the active tab shows now, whatever the last
    // frame drew.
    assert!(reachable.contains(
        "let shown: Vec<u64> = ws .mux .leaves(ws.mux.active, self.area(ws)) .into_iter() .map(|(pane, _)| pane) .collect();"
    ));
    assert!(reachable.contains(".filter(|card| shown.contains(&card.pane))"));
    assert!(body("release_card").contains("self.open_card(ws, pane, nonce);"));
    // A card menu row acts only while the card still shows the item the
    // menu named, and its Open row is a click.
    let act = body("act_on_media");
    assert!(act.contains(
        "if self.media.cards.item(target.pane, target.nonce) != Some(target.item) { return; }"
    ));
    assert!(act.contains(".find(|it| it.id == target.item && it.generation == target.generation)"));
    assert!(
        act.contains("MediaMenuAction::View => { self.open_card(ws, target.pane, target.nonce); }")
    );
    assert!(body("dispatch_context_menu_click").contains(
        "ContextMenuClick::Media { target, action } => { ws.context_menu = None; self.act_on_media(ws, target, action);"
    ));
    assert!(
        body("act_hint")
            .contains("HintWhat::Card(nonce) => { self.open_card(ws, h.pane, *nonce); return; }")
    );
    let hints = body("collect_hints");
    assert!(hints.contains("hint_targets_with_cards(hints::detect_rows(g, &lines, cols), &cards)"));
    // Cards come from the same grid as the text, under the same lock.
    assert!(hints.contains("snap.capture_with_card_marks(&t, true);"));
    assert!(hints.contains(".inline_cards .placements(&snap)"));
    assert!(!hints.contains("painted_cards"));
    let tree = body("accessibility_tree");
    assert!(tree.contains("let cards = self.accessible_cards(ws);"));
    assert!(tree.contains("cards.iter().filter(|(_, card, _)| card.pane == pane_id)"));
    assert!(tree.contains("ws.card_focus .map(accessibility_card_id) .into_iter() .chain(focused.map(accessibility_pane_id))"));
    assert!(
        body("accessibility_key")
            .contains("for (instance, card, item) in self.accessible_cards(ws) {")
    );
    let action = body("handle_accessibility_action");
    assert!(
        action
            .contains("if let Some(instance) = accessibility_card_instance(request.target_node) {")
    );
    assert!(
        action.contains("&& let Some((pane, nonce)) = ws.card_sightings.by_instance(instance) {")
    );
    assert!(action.contains("AccessibilityAction::Click => { self.open_card(ws, pane, nonce); }"));
    assert!(
        source.contains(
            "if event.state == ElementState::Pressed && ws.card_focus.take().is_some() {"
        )
    );
    assert!(source.contains("if typing && ws.card_focus.take().is_some() {"));
    // Focus answers only a card the tree names, and leaves a card on a
    // pane focus change or a press.
    assert!(action.contains(
        "let reachable = self .accessible_cards(ws) .iter() .any(|(card, _, _)| *card == instance); if reachable && let Some((pane, nonce)) = ws.card_sightings.by_instance(instance) {"
    ));
    assert!(body("note_focus_change").contains(
        "// Focus moved to a pane, so not to a card any more. if ws.card_focus.take().is_some() {"
    ));
    assert_eq!(
        source
            .matches("if ws.card_focus.take().is_some() { ws.accessibility_pending = true; }")
            .count(),
        3,
        "a pane focus change, a native press and a control press"
    );
    // A native press of any button leaves the card, side buttons too.
    let native = source
        .split("WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {")
        .nth(1)
        .expect("native press");
    let cleared = native
        .find("if ws.card_focus.take().is_some()")
        .expect("native clear");
    assert!(
        cleared
            < native
                .find("extra_mouse_sgr(button)")
                .expect("side buttons")
    );
    assert!(action.contains(
        "AccessibilityAction::Focus => { // The card's pane takes the keyboard with it, so text // typed next goes where the screen reader is. Search // would keep the keys, so it closes, remembering its // query. self.close_search(ws); let pre = self.focus_key(ws); ws.mux.focus_pane(pane); self.note_focus_change(ws, pre); ws.card_focus = Some(instance); }"
    ));
    // Automation finds cards on screen by instance, never by card id.
    let geometry = body("ctl_ui_geometry");
    assert!(geometry.contains("\"inline_cards\".into(), serde_json::Value::Array(inline_cards)"));
    assert!(geometry.contains("\"instance\": target.card_sightings.instance(card),"));
    assert!(!geometry.contains("\"nonce\""));
}

/// The first card a user ever sees carries the tip that a click opens
/// it, from the first frame showing it, until a card is opened or the
/// tip's time is up, which wakes the loop; the tip state loads at start.
#[test]
fn the_first_card_shows_the_tip_until_opened_or_timed_out() {
    let source = super::production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(source.contains("cards_tip: crate::media::CardsTip::load(),"));
    assert!(source.contains("let cards_uncovered = !self.pointer_modal_open(ws);"));
    assert!(source.contains(
        "let tip_begun = if presented && cards_uncovered { self.cards_tip.begin( renderer.shown_cards().map(|card| (card.pane, card.nonce)),"
    ));
    let begun = source
        .find("drop(panes); if let Some((pane, nonce)) = tip_begun")
        .expect("set after the views");
    assert!(source[begun..].contains("state.inline_cards.set_tip(Some(nonce));"));
    assert!(source.contains(
        "self.show_media_item(ws, pane, item); // Opening a card is what the tip taught. self.end_cards_tip(ws);"
    ));
    assert!(source.contains(
        "if self.cards_tip.until().is_some_and(|until| until <= now) { self.end_cards_tip(ws); }"
    ));
    assert!(source.contains(
        "if let Some(next) = cards_tip_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }"
    ));
    assert!(source.contains("state.inline_cards.set_tip(None);"));
}

/// A card takes its primary press before anything else in the window,
/// on the native and control paths alike, after only the dialogs and
/// the preview lanes that own every press; its release is the card's too.
#[test]
fn a_card_takes_its_press_and_release_before_anything_behind_it() {
    let source = super::production_source();
    let source = source.as_str();
    fn after<'a>(text: &'a str, from: &str) -> &'a str {
        text.split_once(from).unwrap_or_else(|| panic!("{from}")).1
    }
    let native = after(
        source,
        "WindowEvent::MouseInput {\n                state: ElementState::Pressed,",
    );
    let viewer = native
        .find("if self.press_preview_lane(ws, px, py, bcode) {")
        .expect("lane");
    let card = native
        .find("if self.press_card(ws, px, py, bcode) {")
        .expect("card");
    let bar = native.find("let bar = self.tab_bar(ws);").expect("tab bar");
    assert!(viewer < card && card < bar);
    let ctl = after(source, "    fn ctl_mouse_press(");
    let viewer = ctl
        .find("if self.press_preview_lane(ws, px, py, bcode) {")
        .expect("ctl lane");
    let card = ctl
        .find("if self.press_card(ws, px, py, bcode) {")
        .expect("ctl card");
    let bar = ctl
        .find("if tab_bar_pointer_region_contains(&bar, px, py)")
        .expect("ctl tab bar");
    assert!(viewer < card && card < bar);
    let release = after(
        source,
        "WindowEvent::MouseInput {\n                state: ElementState::Released,",
    );
    let card = release
        .find("if self.release_card(ws, px, py, bcode) {")
        .expect("release");
    let drag = release
        .find("if self.release_pane_drag(ws, bcode, event_loop) {")
        .expect("pane drag");
    let report = release
        .find("if self.send_mouse(ws, bcode, false, false) {")
        .expect("mouse report");
    assert!(card < drag && card < report);
    let ctl_release = after(source, "    fn ctl_mouse_release(");
    let card = ctl_release
        .find("if self.release_card(ws, px, py, bcode) {")
        .expect("ctl release");
    let report = ctl_release
        .find("let mut handled = self.send_mouse(ws, bcode, false, false);")
        .expect("ctl report");
    assert!(card < report);
    // Only a primary or secondary press without Shift, and only on a
    // settled card; the secondary one opens the card's menu.
    let press = after(source, "    fn press_card(");
    assert!(press.contains("if !matches!(bcode, 0 | 2) || ws.mods.shift_key() {"));
    assert!(press.contains("return self.open_card_menu(ws, card, (px, py));"));
    assert!(press.contains("ws.card_sightings.settled(card, std::time::Instant::now())"));
    // Its release opens nothing once a dialog or the viewer is up.
    let release = after(source, "    fn release_card(");
    assert!(release.contains("let covered = self.pointer_modal_open(ws);"));
    assert!(release.contains("card_release_opens((pane, nonce), over, covered)"));
    // A primary press ends a card press whose release went elsewhere,
    // before anything can take it, natively and over control.
    let reset = "if bcode == 0 {\n            ws.card_press = None;\n        }";
    let native_reset = native
        .find(&reset.replace("\n        ", "\n                "))
        .expect("native reset");
    let menu = native
        .find("&& let Some(click) = self.context_menu_click_action(ws, bcode)")
        .expect("menu");
    assert!(native_reset < menu);
    let ctl_reset = ctl.find(reset).expect("ctl reset");
    let ctl_menu = ctl
        .find("&& let Some(click) = self.context_menu_click_action(ws, bcode)")
        .expect("ctl menu");
    assert!(ctl_reset < ctl_menu);
    // So does anything else that ends the pointer's gestures.
    let focus = after(source, "WindowEvent::Focused(f) => {");
    let focus = &focus[..focus.find("WindowEvent::").unwrap_or(focus.len())];
    assert!(focus.contains("ws.card_press = None;"));
    for gesture_end in [
        "    fn open_search(",
        "    fn show_context_menu(",
        "    fn close_all_modals(",
        "    fn install_confirm_dialog(",
    ] {
        let body = after(source, gesture_end);
        let body = &body[..body.find("\n    }\n").unwrap_or(body.len())];
        assert!(body.contains("ws.card_press = None;"), "{gesture_end}");
    }
    // A card has been seen once a frame showing it reached the screen.
    assert!(source.contains(
        "let presented = matches!(&frame_result, Ok(FrameOutcome::Presented));\n        if presented\n            && ws\n                .card_sightings\n                .note(renderer.painted_cards(), std::time::Instant::now())"
    ));
}

/// A Mermaid diagram an agent shows gets a card with its rendered poster,
/// from inline source (no file name) or from a file, captioned as one.
#[test]
fn a_mermaid_item_gets_a_card_from_source_or_file() {
    let owner = kettle_ctl::process::ProcessIdentity::new_for_tests(41, 410);
    let image = kettle_core::ImageData::new(64, 48, vec![9; 64 * 48 * 4]).unwrap();
    let now = std::time::Instant::now();
    let mut ledger = crate::media::CardLedger::default();
    let mut pane_cards = kettle_render::InlineCards::default();
    for (name, caption) in [
        (None, "mermaid 64x48"),
        (Some("flow.mmd"), "flow.mmd - mermaid 64x48"),
    ] {
        let delivery = super::register_card(
            &mut ledger,
            (3, &mut pane_cards, (80, 40), 0),
            (8.0, 16.0),
            (9, 0),
            (kettle_media::MediaKind::Mermaid.into(), None, None),
            &image,
            crate::media::InlineDraft {
                #[cfg(target_os = "linux")]
                launch: None,
                owner,
                harness: kettle_render::CardHarness::ClaudeHook,
                name: name.map(Into::into),
                printer: None,
            },
            now,
        )
        .expect("a card");
        assert_eq!(delivery.message.split('\n').next_back(), Some(caption));
    }
    assert_eq!(pane_cards.len(), 2);
    assert_eq!(ledger.cards_of(3, (9, 0)).len(), 2);
}

/// A verified harness's card is registered in its pane with the message
/// its harness prints; anything that cannot hold leaves the shelf alone.
#[test]
fn a_card_is_registered_with_the_message_its_harness_prints() {
    use kettle_core::InlineMarker;
    let owner = kettle_ctl::process::ProcessIdentity::new_for_tests(40, 400);
    let image = kettle_core::ImageData::new(64, 48, vec![9; 64 * 48 * 4]).unwrap();
    let draft = || crate::media::InlineDraft {
        #[cfg(target_os = "linux")]
        launch: None,
        owner,
        harness: kettle_render::CardHarness::ClaudeHook,
        name: Some("plot.png".into()),
        printer: None,
    };
    let now = std::time::Instant::now();
    let mut ledger = crate::media::CardLedger::default();
    let mut pane_cards = kettle_render::InlineCards::default();
    let delivery = super::register_card(
        &mut ledger,
        (3, &mut pane_cards, (80, 40), 0),
        (8.0, 16.0),
        (9, 0),
        (kettle_media::MediaKind::Raster.into(), None, None),
        &image,
        draft(),
        now,
    )
    .expect("a card");
    assert!(delivery.fits());
    assert_eq!(pane_cards.len(), 1);
    let lines: Vec<&str> = delivery.message.split('\n').collect();
    assert_eq!(lines.last(), Some(&"plot.png - raster 64x48"));
    let first: Vec<char> = lines[1].chars().skip(1).take(8).collect();
    let marker = InlineMarker::decode(&first).expect("the first cell's marks");
    assert_eq!((marker.row, marker.column), (0, 0));
    assert!(pane_cards.contains(marker.nonce));
    assert_eq!(ledger.item(3, marker.nonce), Some(9));
    // A pane too small for a card, or a harness past its limit, gets none.
    assert_eq!(
        super::register_card(
            &mut ledger,
            (3, &mut pane_cards, (80, 4), 0),
            (8.0, 16.0),
            (9, 0),
            (kettle_media::MediaKind::Raster.into(), None, None),
            &image,
            draft(),
            now,
        )
        .err(),
        Some(kettle_ctl::show::InlineRefusal::TooSmall)
    );
    for _ in 0..crate::media::HARNESS_CARDS_PER_SECOND {
        let _ = super::register_card(
            &mut ledger,
            (3, &mut pane_cards, (80, 40), 0),
            (8.0, 16.0),
            (9, 0),
            (kettle_media::MediaKind::Raster.into(), None, None),
            &image,
            draft(),
            now,
        );
    }
    let before = pane_cards.len();
    assert_eq!(
        super::register_card(
            &mut ledger,
            (3, &mut pane_cards, (80, 40), 0),
            (8.0, 16.0),
            (9, 0),
            (kettle_media::MediaKind::Raster.into(), None, None),
            &image,
            draft(),
            now,
        )
        .err(),
        Some(kettle_ctl::show::InlineRefusal::Limit),
        "four a second"
    );
    assert_eq!(pane_cards.len(), before, "nothing registered");
}

/// `kettle show --inline` gets a card only when it runs in the pane, writes
/// to the pane's own terminal from its foreground, and no full-screen program
/// owns the screen; the card is the pane's own child's. Real panes, real
/// processes: the pane's child is the command here.
#[cfg(unix)]
#[test]
fn a_command_line_card_needs_the_panes_own_terminal_and_foreground() {
    use kettle_ctl::process::StandardOutput;
    use kettle_ctl::show::InlineRefusal;
    let cfg = kettle_config::Config {
        shell_integration: false,
        login_shell: false,
        ..kettle_config::Config::default()
    };
    let spawn = |script: &str| {
        let mut mux = crate::mux::Mux::new();
        let argv = ["/bin/sh", "-c", script].map(String::from).to_vec();
        mux.new_tab_with_geometry(
            &cfg,
            kettle_core::PtyGeometry::from_cell_size(100, 30, 8, 16),
            std::sync::Arc::new(|| {}),
            &argv,
            None,
        )
        .ok()
        .map(|_| mux)
    };
    let Some(plain) = spawn("exec sleep 30") else {
        eprintln!("skipping: no PTY");
        return;
    };
    let pane = plain.panes.values().next().unwrap();
    let root = pane.caller_root.expect("the pane's child");
    let terminal = crate::ctl_server::CliTerminal::of(root).expect("its terminal");
    assert!(matches!(terminal.output, StandardOutput::Device(_)));
    assert_eq!(
        super::media::cli_card_owner(pane, true, Some(terminal)),
        Ok(root)
    );
    // Not in the pane, writing elsewhere, or behind another process group.
    assert_eq!(
        super::media::cli_card_owner(pane, false, Some(terminal)),
        Err(InlineRefusal::Unverified)
    );
    let null = kettle_ctl::process::device_number(&std::fs::metadata("/dev/null").unwrap());
    for output in [StandardOutput::Other, StandardOutput::Device(null)] {
        let elsewhere = crate::ctl_server::CliTerminal { output, ..terminal };
        assert_eq!(
            super::media::cli_card_owner(pane, true, Some(elsewhere)),
            Err(InlineRefusal::NotTerminal)
        );
    }
    assert_eq!(
        super::media::cli_card_owner(pane, true, None),
        Err(InlineRefusal::NotTerminal)
    );
    let background = crate::ctl_server::CliTerminal {
        group: terminal.group.wrapping_add(1),
        ..terminal
    };
    assert_eq!(
        super::media::cli_card_owner(pane, true, Some(background)),
        Err(InlineRefusal::Background)
    );
    plain.kill_children();
    // A full-screen program owns its pane's screen.
    let Some(full) = spawn("printf '\\033[?1049h'; exec sleep 30") else {
        return;
    };
    let pane = full.panes.values().next().unwrap();
    let alternate = kettle_test_support::poll_until(std::time::Duration::from_secs(10), || {
        pane.term
            .term
            .lock()
            .ok()
            .filter(|term| term.mode().contains(kettle_core::TermMode::ALT_SCREEN))
            .map(|_| ())
    });
    assert!(alternate.is_some(), "the program took the screen");
    let root = pane.caller_root.expect("the pane's child");
    let terminal = crate::ctl_server::CliTerminal::of(root).expect("its terminal");
    assert_eq!(
        super::media::cli_card_owner(pane, true, Some(terminal)),
        Err(InlineRefusal::AlternateScreen)
    );
    full.kill_children();
}

/// The App gives a card only to a verified route, retires cards on its
/// tick and when their item is replaced or dropped, and keeps a painted
/// card's pixels.
#[test]
fn cards_are_for_verified_harnesses_and_retire_with_their_item() {
    let source = super::production_source();
    let body = |name: &str| {
        source
            .split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{name} present"))
    };
    let show = body("ctl_show");
    assert!(show.contains("(Some(target), Some(owner)) if route.verified =>"));
    // Each harness's card is placed and sized as that harness prints it.
    assert!(show.contains("harness: printed_by(target),"));
    assert!(source.contains(
        "kettle_ctl::show::InlineTarget::CodexHook => kettle_render::CardHarness::CodexHook,"
    ));
    assert!(
        body("ctl_show").contains("name: card_name,"),
        "a card's caption names only a file, never a title the sender chose"
    );
    // A drawn item and a file shown as itself go through one door.
    assert!(body("finish_show").contains("self.publish_show("));
    assert!(body("publish_show").contains(".publish(item, shown, &visible)"));
    let finish = body("publish_show");
    let retired = finish.find("cards.retire(").expect("replaced cards retire");
    let registered = finish.find("prepare_card(").expect("a card is prepared");
    assert!(
        retired < registered,
        "old cards retire before a new one registers"
    );
    assert!(finish.contains("result.inline = delivery;"));
    assert!(body("media_tick").contains("self.retire_cards(ws);"));
    let retire = body("retire_cards");
    assert!(retire.contains("kettle_ctl::process::identity(owner.pid()) != Ok(*owner)"));
    assert!(retire.contains("pane.inline_cards.remove(nonce);"));
    let visible = source
        .split("fn media_visible_items<'a>(")
        .nth(1)
        .and_then(|rest| rest.split("\nfn ").next())
        .expect("media_visible_items");
    // Eviction spares what the next frame draws, presented or not.
    assert!(visible.contains(".drawn_cards()"));
}

/// The sender line knows where its program's path sits, whatever the
/// language's word order and whatever the path holds.
#[test]
fn the_sender_line_finds_its_program_in_any_word_order() {
    let path = "/opt/Unverified sender/bin/tool";
    let signer = "Example (pid 7) Corp";
    for template in [
        |program: &str, signer: Option<&str>| {
            format!(
                "Unverified sender: {program} (pid 7), signed by {}",
                signer.unwrap()
            )
        },
        |program: &str, signer: Option<&str>| {
            format!("{} signed {program} (pid 7)", signer.unwrap())
        },
    ] {
        let sender = super::media_viewer_sender_line(template, path, Some(signer));
        let program = sender.program.clone().expect("the program is found");
        let signed = sender.signer.clone().expect("the signer is found");
        assert_eq!(&sender.text[program], path, "{}", sender.text);
        assert_eq!(&sender.text[signed], signer, "{}", sender.text);
    }
    let unsigned = super::media_viewer_sender_line(
        |program: &str, _: Option<&str>| format!("Unverified sender: {program}"),
        path,
        None,
    );
    assert_eq!(unsigned.signer, None);
    assert!(unsigned.program.is_some());
}

/// A screen reader hears a lane's item, its place on the shelf and,
/// when its pixels were released, that too; the tree is republished when
/// either changes, and each lane's nodes are its pane's alone.
#[test]
fn a_preview_lane_speaks_its_place_and_republishes_on_change() {
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let mut lane = test_lane_panel();
    let (label, description) = super::media_lane_accessibility(&lane, &tr, None);
    assert!(
        label.contains("2 of 3") && label.contains("Plot"),
        "{label}"
    );
    assert_eq!(description, "Keys still go to the terminal");
    // What the lane last did is said in place of where keys go.
    let (_, description) =
        super::media_lane_accessibility(&lane, &tr, Some("The preview took too long"));
    assert_eq!(description, "The preview took too long");
    lane.image = None;
    let (_, description) = super::media_lane_accessibility(&lane, &tr, None);
    assert!(description.starts_with("Released."), "{description}");
    let source = super::production_source();
    let key = source
        .split("fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("accessibility_key");
    assert!(key.contains(
        "media_lane_accessibility(lane, &self.ui_text, announced.as_deref()).hash(&mut hasher);"
    ));
    assert!(key.contains("let announced = self.lane_announcement(ws, lane.pane);"));
    // A press on play or sound renames its button: the tree is
    // published again though nothing else changed.
    assert!(key.contains("lane.play.hash(&mut hasher);"));
    assert!(key.contains("lane.sound.hash(&mut hasher);"));
    assert!(key.contains("lane.collapsed.hash(&mut hasher);"));

    // Lane node ids round-trip, keep clear of every other kind of node,
    // and a pane id too large for the space has no lane nodes.
    for (pane, part) in [
        (1, 0),
        (7, 5),
        (7, super::LANE_SCRUBBER_PART),
        (super::MAX_LANE_ACCESSIBILITY_PANE - 1, 31),
    ] {
        let id = super::accessibility_lane_id(pane, part).unwrap();
        assert_eq!(super::accessibility_lane_part(id), Some((pane, part)));
        assert_eq!(super::accessibility_card_instance(id), None);
        assert_ne!(id, super::accessibility_pane_id(pane));
    }
    // The next lane's parts start past the last of this one's.
    assert_ne!(
        super::accessibility_lane_id(7, 31),
        super::accessibility_lane_id(8, 0)
    );
    assert_eq!(
        super::accessibility_lane_id(super::MAX_LANE_ACCESSIBILITY_PANE, 0),
        None
    );
    assert_eq!(super::accessibility_lane_id(1, 32), None);
    assert_eq!(
        super::accessibility_lane_part(super::accessibility_pane_id(3)),
        None
    );
    assert_eq!(
        super::accessibility_lane_part(super::accessibility_card_id(3)),
        None
    );
}

/// A video preview requested before its tab moved reaches the window the
/// tab is in now, and only for the exact pane and generation.
#[test]
fn video_preview_completion_finds_the_window_its_tab_moved_to() {
    use super::video_preview_target_window;
    let pending = |pane| crate::window_state::PendingVideoPasteReceipt {
        pane_id: pane,
        generation: 41,
        request: crate::video_preview::VideoPasteRequest::from_user_paths(
            &[std::env::current_dir().unwrap().join("missing.mp4")],
            crate::video_preview::VideoPasteSource::Drop,
        )
        .unwrap(),
        remote: false,
        prefer_top: true,
        created_at: std::time::Instant::now(),
        previous_receipt: None,
    };
    let mut windows = std::collections::BTreeMap::new();
    windows.insert(1, WindowState::new(1, crate::mux::Mux::new()));
    let mut torn = WindowState::new(2, crate::mux::Mux::new());
    torn.pending_video_paste_receipt = Some(pending(7));
    windows.insert(2, torn);
    assert_eq!(video_preview_target_window(&windows, 1, 7, 41), Some(2));
    assert_eq!(video_preview_target_window(&windows, 2, 7, 41), Some(2));
    assert_eq!(video_preview_target_window(&windows, 1, 7, 40), None);
    assert_eq!(video_preview_target_window(&windows, 1, 8, 41), None);
}

#[test]
fn get_state_reports_media_availability_without_waiting() {
    use kettle_media::client::{FileIdentity, UnavailableCause, WorkerClient, WorkerPlatform};
    use std::path::Path;

    let body = production_source()
        .split("fn ctl_get_state")
        .nth(1)
        .and_then(|s| s.split("fn ctl_list_tabs").next())
        .expect("ctl_get_state body present")
        .to_owned();
    assert!(body.contains("\"media\": ctl_media_state(self.startup.media.as_deref()),"));

    /// Checking holds until the test releases the channel.
    struct Checked(
        std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        Result<FileIdentity, UnavailableCause>,
    );
    impl WorkerPlatform for Checked {
        fn worker_path(&self) -> Result<&Path, UnavailableCause> {
            Ok(Path::new("/install/kettle-media-worker"))
        }
        fn inspect(&self, _: &Path) -> Result<FileIdentity, UnavailableCause> {
            let _ = self.0.lock().unwrap().recv();
            self.1
        }
        fn verify(&self, _: &Path) -> Result<(), UnavailableCause> {
            Ok(())
        }
        fn spawn(&self, _: &Path) -> std::io::Result<kettle_media::client::SpawnedWorker> {
            panic!("get_state must not start a worker")
        }
        fn guard_pipe_writes(&self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let video = serde_json::json!({
        "decoders": if cfg!(target_os = "macos") {
            serde_json::json!(["avfoundation", "ffmpeg"])
        } else if cfg!(target_os = "linux") {
            serde_json::json!(["ffmpeg"])
        } else {
            serde_json::json!([])
        },
        "gstreamer": "not_used",
    });
    let with_video = |mut state: serde_json::Value| {
        state["video"] = video.clone();
        state
    };
    assert_eq!(
        super::ctl_media_state(None),
        with_video(
            serde_json::json!({ "availability": "unavailable", "reason": "not_configured" })
        )
    );
    let (release, held) = std::sync::mpsc::channel();
    let client = WorkerClient::new(
        kettle_media::BuildId::from_embedded("5.0.0", "0123456789abcdef").unwrap(),
        Box::new(Checked(
            std::sync::Mutex::new(held),
            Err(UnavailableCause::WorkerMissing),
        )),
    );
    // The check is still held: the answer comes back at once.
    assert_eq!(
        super::ctl_media_state(Some(&client)),
        with_video(serde_json::json!({ "availability": "checking" }))
    );
    drop(release);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = super::ctl_media_state(Some(&client));
        if state["availability"] != "checking" {
            assert_eq!(
                state,
                with_video(
                    serde_json::json!({ "availability": "unavailable", "reason": "worker_missing" })
                )
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the check never finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let (release, held) = std::sync::mpsc::channel();
    drop(release);
    let available = WorkerClient::new(
        kettle_media::BuildId::from_embedded("5.0.0", "0123456789abcdef").unwrap(),
        Box::new(Checked(
            std::sync::Mutex::new(held),
            Ok(FileIdentity {
                dev: 1,
                ino: 2,
                size: 3,
                mtime_seconds: 4,
                mtime_nanos: 5,
                ctime_seconds: 6,
                ctime_nanos: 7,
            }),
        )),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = super::ctl_media_state(Some(&available));
        if state["availability"] != "checking" {
            assert_eq!(
                state,
                with_video(serde_json::json!({ "availability": "available" }))
            );
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "availability never settled"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[test]
fn finding_enlarged_viewer_covers_sibling_lanes() {
    let body = finding_body(include_str!("preview.rs"), "preview_lane_projections");
    assert!(
        body.contains("open.theater || open.fullscreen"),
        "fullscreen and theater projections must exclude sibling lanes"
    );
}


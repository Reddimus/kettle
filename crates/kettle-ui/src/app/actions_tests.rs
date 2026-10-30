/// Reset and Clear Scrollback change Kettle's terminal. They used to type
/// `ESC c` and `CSI 3 J` into the child, where zsh and bash read `ESC c` as
/// capitalize-word. Read-only still resets, as Terminator's Reset calls
/// `vte_terminal_reset`, which ignores `input-enabled`.
#[cfg(unix)]
#[test]
fn reset_and_clear_scrollback_change_the_terminal_not_the_child() {
    // One process prints the lines and then a marker with no newline, so
    // once the marker shows no output is still arriving. Lines typed into
    // `cat` would come back twice, as the line discipline's echo and as
    // `cat`'s copy, and Linux can interleave the two mid-line.
    let script = r#"i=0; while [ $i -lt 40 ]; do echo "line $i"; i=$((i+1)); done; printf lines-done; exec cat"#;
    let argv = ["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    let waker: kettle_core::Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    mux.new_tab_with(&title_test_config(), 80, 24, 8, 16, waker, &argv, None)
        .expect("spawn the pane");
    let target = mux.active_focus().expect("target pane id");
    mux.panes.get_mut(&target).unwrap().read_only = true;
    assert!(
        wait_for_pane_screen(&mux, target, |text, history| {
            text.contains("lines-done") && history > 0
        }),
        "the shell did not print its lines"
    );
    let before = mux.panes[&target].term.screen_text(0).unwrap().text;

    super::apply_terminal_control(&mux, &[target], b"\x1b[3J");
    assert!(
        wait_for_pane_screen(&mux, target, |text, history| history == 0 && text == before),
        "Clear Scrollback must drop the history and keep the screen"
    );

    super::apply_terminal_control(&mux, &[target], b"\x1bc");
    assert!(
        wait_for_pane_screen(&mux, target, |text, _| !text.contains("line 39")),
        "Reset must clear the terminal"
    );

    // `cat`'s line discipline echoes any byte the child received, so the
    // controls would appear as `^[` before this marker.
    mux.panes[&target].term.write(b"after\n");
    assert!(wait_for_pane_screen(&mux, target, |text, _| text.contains("after")));
    let text = mux.panes[&target].term.screen_text(0).unwrap().text;
    assert!(
        !text.contains("^["),
        "a control reached the child: {text:?}"
    );
}

#[test]
fn single_group_edit_never_follows_focus_to_a_sibling() {
    let (mut mux, target, sibling) = title_test_split_mux();
    let mut edit = begin_title_edit(
        &mux,
        TitleEditScope::Group,
        GroupBulkScope::Single,
        "window",
    )
    .expect("group edit opens");
    edit.input = "fleet".into();

    assert!(mux.focus_pane(sibling), "focus drifted while edit was open");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.panes[&target].group_name.as_deref(), Some("fleet"));
    assert_eq!(mux.panes[&sibling].group_name, None);

    let (mut mux, target, sibling) = title_test_split_mux();
    let mut edit = begin_title_edit(
        &mux,
        TitleEditScope::Group,
        GroupBulkScope::Single,
        "window",
    )
    .expect("group edit opens");
    edit.input = "dead-target-group".into();
    mux.panes.get_mut(&target).unwrap().closed = true;
    assert!(!mux.reap().mux_empty, "the sibling keeps the mux alive");
    assert_eq!(mux.active_focus(), Some(sibling));
    let mut open_edit = Some(edit.clone());
    assert!(clear_stale_title_edit(&mux, &mut open_edit));
    assert!(
        open_edit.is_none(),
        "a dead group target cannot retain its modal"
    );
    assert!(!apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.panes[&sibling].group_name, None);
}

#[test]
fn global_group_autoclean_keeps_foreign_members_and_clears_empty_scopes() {
    let scope = BroadcastScope::Group("fleet".into());
    let populated = HashSet::from(["fleet"]);
    assert!(
        !group_scope_is_globally_stale(true, &scope, &populated),
        "a member in another window keeps the process-wide group alive"
    );
    assert!(group_scope_is_globally_stale(true, &scope, &HashSet::new()));
    assert!(!group_scope_is_globally_stale(
        false,
        &scope,
        &HashSet::new()
    ));
    assert!(!group_scope_is_globally_stale(
        true,
        &BroadcastScope::All,
        &HashSet::new()
    ));

    let source = production_source();
    for owner in [
        "fn apply_title_edit(",
        "fn handle_action(",
        "fn redraw(",
        "fn about_to_wait_inner(",
    ] {
        let body = source
            .split(owner)
            .nth(1)
            .unwrap_or_else(|| panic!("{owner} must exist"));
        let end = body.find("\n    }\n").unwrap_or(body.len());
        assert!(
            body[..end].contains("self.hoover_groups(ws);"),
            "{owner} must globally clean group-name edits and completed pane/tab mutations"
        );
    }
    assert!(
        source.contains("drop(ws);\n            self.hoover_mapped_groups();"),
        "closing a whole window must clean scopes in the surviving windows"
    );
}

/// Both user-input paths have to cross the window boundary, not just the
/// keystroke one. A paste is user input under the same scope as a
/// keystroke, and `broadcast_paste_delivery` is just as window-local.
/// Unless the paste path walks the other windows too, a broadcast types
/// to the whole group but pastes to half of it.
#[test]
fn every_broadcast_input_ingress_crosses_the_window_boundary() {
    let src = production_source();
    for (path, entry) in [
        ("keystroke", "fn broadcast_input("),
        ("key event", "fn write_terminal_key_event("),
        ("text paste", "fn deliver_text_paste("),
        ("path paste", "fn deliver_path_paste("),
    ] {
        let body = src
            .split(entry)
            .nth(1)
            .unwrap_or_else(|| panic!("{path} path: {entry} not found"));
        let end = body.find("\n    }\n").unwrap_or(body.len());
        let body = &body[..end];
        assert!(
            body.contains("scope_crosses_windows") && body.contains("self.windows.values_mut()"),
            "the {path} path must walk the other windows when the scope \
                 reaches across them"
        );
    }
    // The paste needles now also pin the ordering contract: an active
    // broadcast must take its existing group path before either helper can
    // interpret the optional single-pane target.
    for (entry, targeted_delivery) in [
        ("fn deliver_text_paste(", "paste_text_into_target"),
        ("fn deliver_path_paste(", "paste_paths_into_target"),
    ] {
        let body = src.split(entry).nth(1).expect("paste delivery body");
        let end = body.find("\n    }\n").unwrap_or(body.len());
        let body = &body[..end];
        let broadcast = body
            .find("if ws.mux.is_broadcast_on()")
            .expect("paste delivery keeps broadcast semantics");
        let targeted = body
            .find(targeted_delivery)
            .expect("paste delivery has single-pane delivery");
        assert!(
            broadcast < targeted,
            "broadcast must branch before the pinned single-pane delivery"
        );
        for stale_focus_lookup in [".focused()", "focused_mode(ws)", "focused_argv()"] {
            assert!(
                !body.contains(stale_focus_lookup),
                "single-pane paste delivery must not resolve through {stale_focus_lookup}"
            );
        }
    }
    let dropped = src
        .split("WindowEvent::DroppedFile(path) => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::Focused").next())
        .expect("dropped-file arm");
    assert!(
        dropped.contains("VideoPasteSource::Drop")
            && dropped.contains("self.paste_paths(ws, paths, true, video.flatten());"),
        "dropped files must use the same cross-window path fanout as clipboard files"
    );
    let clear_history = src
        .split("Action::ClearHistory => {")
        .nth(1)
        .and_then(|body| body.split("Action::Reset =>").next())
        .expect("ClearHistory arm");
    assert!(
        clear_history.contains("other.mux.foreign_target_ids(&scope)"),
        "broadcast-aware terminal actions must not stop at the source mux"
    );
}

#[test]
fn group_hoover_skips_the_common_case_before_allocating_names() {
    let src = production_source();
    let body = src
        .split("fn hoover_groups(")
        .nth(1)
        .and_then(|rest| rest.split("fn hoover_mapped_groups(").next())
        .expect("group hoover body");
    let guard = body
        .find("any_group_scope_needs_hoover")
        .expect("cheap guard");
    let scan = body.find("populated_groups").expect("group-name scan");
    assert!(guard < scan);
    assert!(src.contains("filter_map(|pane| pane.group_name.as_deref())"));
}

/// The group actions must actually GROUP, not just resolve to a name.
///
/// The config-side test proves `group_all` maps to `Action::GroupAll`; it
/// says nothing about what the dispatch does, so deleting or no-oping the
/// arm would leave it green. Building a real window needs a live App, so
/// the wiring is pinned here.
#[test]
fn the_group_all_actions_reach_a_dispatch_that_groups() {
    let src = production_source();
    let arm_head = [
        "Action::GroupAll | Action::Ungroup",
        "All | Action::ToggleGroupAll => {",
    ]
    .concat();
    let at = src
        .find(arm_head.as_str())
        .expect("GroupAll/UngroupAll/ToggleGroupAll must have a dispatch arm");
    let arm: String = src[at..].chars().take(2400).collect();
    assert!(
        arm.contains("p.group_name = group.clone();"),
        "the arm must write the group onto panes, not merely exist"
    );
    assert!(
        arm.contains("kettle_ui_group_all_name()"),
        "and use Terminator's own fixed name"
    );
    // Terminator's scope is its whole terminal collection, across windows.
    assert!(
        arm.contains("self.windows.values_mut()"),
        "grouping must reach every window, not just the focused one"
    );
    // The toggle asks the INVOKING pane, matching window.py:940-945.
    assert!(
        arm.contains("ws.mux.focused()"),
        "the toggle must test the focused pane, not the whole set"
    );
}

/// Drift guard on the pane count behind every close prompt: it decides
/// whether `ask-before-closing = multiple-terminals` fires at all, so
/// undercounting a nested split silently closes panes without asking.
#[test]
fn count_leaves_for_nested_splits() {
    use crate::mux::{Dir, Node};
    // Single leaf.
    let leaf = Node::Leaf(1);
    assert_eq!(leaf.leaf_ids().len(), 1);
    // Two-way split.
    let split = Node::Split {
        dir: Dir::Horizontal,
        ratio: 0.5,
        a: Box::new(Node::Leaf(1)),
        b: Box::new(Node::Leaf(2)),
    };
    assert_eq!(split.leaf_ids().len(), 2);
    // Three-way nested split (a is a split, b is a leaf).
    let nested = Node::Split {
        dir: Dir::Vertical,
        ratio: 0.5,
        a: Box::new(Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        }),
        b: Box::new(Node::Leaf(3)),
    };
    assert_eq!(nested.leaf_ids().len(), 3);
    // Four-way (both a and b are splits).
    let four = Node::Split {
        dir: Dir::Horizontal,
        ratio: 0.5,
        a: Box::new(Node::Split {
            dir: Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        }),
        b: Box::new(Node::Split {
            dir: Dir::Vertical,
            ratio: 0.5,
            a: Box::new(Node::Leaf(3)),
            b: Box::new(Node::Leaf(4)),
        }),
    };
    assert_eq!(four.leaf_ids().len(), 4);
}

#[test]
fn foreign_broadcast_scrolls_request_a_redraw() {
    let src = production_source();
    for (method, foreign, accepted) in [
        (
            "fn broadcast_input(",
            "broadcast_write_foreign_delivery(",
            "delivery.accepted",
        ),
        (
            "fn write_terminal_key_event(",
            "broadcast_encoded_foreign(",
            "foreign.accepted",
        ),
    ] {
        let body = src
            .split(method)
            .nth(1)
            .unwrap()
            .split("\n    }\n")
            .next()
            .unwrap();
        let tail = body.split(foreign).nth(1).unwrap();
        assert!(
            tail.contains("other.window")
                && tail.contains("request_redraw()")
                && tail.contains(accepted),
            "accepted foreign input that scrolls must repaint its window: {method}"
        );
    }
}

#[test]
fn finding_captured_input_survives_broadcast_change() {
    let body = finding_body(include_str!("paste.rs"), "deliver_queued_paste");
    assert!(
        body.contains("!matches!(content, QueuedPasteContent::Input { .. })"),
        "captured key recipients must bypass paste scope validation"
    );
}


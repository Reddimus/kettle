/// A pane hears a focus report only when the pane holding the keyboard
/// changes, once per change.
#[test]
fn focus_reports_follow_the_pane_that_holds_the_keyboard() {
    use super::focus_report_transition as transition;
    assert_eq!(
        transition(Some(1), Some(1)),
        (None, None),
        "no change, no report"
    );
    assert_eq!(transition(None, None), (None, None));
    assert_eq!(
        transition(Some(1), Some(2)),
        (Some(1), Some(2)),
        "pane to pane"
    );
    assert_eq!(
        transition(Some(1), None),
        (Some(1), None),
        "window lost focus"
    );
    assert_eq!(
        transition(None, Some(2)),
        (None, Some(2)),
        "window regained focus"
    );
}
/// Every dispatch re-inserts its window through `finish_window_dispatch`,
/// so the reports are reconciled there, and the OS focus arm no longer
/// reports on its own (it would report twice, and miss pane changes).
#[test]
fn focus_reports_are_reconciled_after_every_dispatch() {
    let source = production_source();
    let finish = source
        .split("fn finish_window_dispatch(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("finish_window_dispatch body");
    let sync = finish
        .find("self.sync_pane_focus_reports(&mut ws);")
        .expect("finish_window_dispatch reconciles focus reports");
    let insert = finish
        .rfind("self.windows.insert(seq, ws);")
        .expect("finish_window_dispatch re-inserts the window");
    assert!(sync < insert, "reconcile before the window is stored again");
    let focus_arm = source
        .split("WindowEvent::Focused(f) => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("focus event arm");
    assert!(!focus_arm.contains("feed_focus_report"));
    let sync_body = source
        .split("fn sync_pane_focus_reports(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("focus report sync");
    assert!(sync_body.contains("(ws.os_focus_known && ws.window_focused)"));
    assert!(sync_body.contains("TermMode::FOCUS_IN_OUT"));
}

/// Census: a focus change and input to a pane can happen in one dispatch
/// (a Lua batch, a click on another pane, a control request), so every
/// function that writes input to a pane reconciles the focus reports first
/// and a newly focused pane hears `CSI I` before its first byte. The
/// `paste_*_into_target` helpers are called only from functions that do.
#[test]
fn every_pane_input_writer_reconciles_focus_reports_first() {
    let source = production_source();
    let mut checked = 0;
    for chunk in source.split("\n    fn ").skip(1) {
        let name = chunk.split('(').next().unwrap_or_default().to_string();
        if name == "deliver_deferred_input" {
            continue;
        }
        let body = chunk.split("\n    }\n").next().unwrap_or(chunk);
        let first_write = ["feed_input(", "feed_input_shared(", "feed_key_inputs("]
            .iter()
            .filter_map(|needle| body.find(needle))
            .min();
        let Some(first_write) = first_write else {
            continue;
        };
        checked += 1;
        let sync = body.find("self.sync_pane_focus_reports(");
        assert!(
            sync.is_some_and(|sync| sync < first_write),
            "{name} writes pane input without reconciling focus reports first"
        );
    }
    // Keys and actions now write through `deliver_deferred_input`, so fewer
    // functions write directly; every caller of it is checked below.
    assert!(checked >= 8, "census found only {checked} writers");
    for caller in [
        "paste_text_into_target(&mut ws.mux",
        "paste_paths_into_target(&mut ws.mux",
        "Self::deliver_deferred_input(",
    ] {
        let owners: Vec<_> = source
            .split("\n    fn ")
            .skip(1)
            .filter(|chunk| !chunk.starts_with("deliver_deferred_input("))
            .map(|chunk| chunk.split("\n    }\n").next().unwrap_or(chunk))
            .filter(|body| body.contains(caller))
            .collect();
        assert!(!owners.is_empty(), "{caller} has no caller");
        for owner in owners {
            let name = owner.split('(').next().unwrap_or_default();
            let sync = owner.find("self.sync_pane_focus_reports(");
            assert!(
                sync.is_some_and(|sync| sync < owner.find(caller).unwrap()),
                "{name} calls {caller} without reconciling focus reports first"
            );
        }
    }
}

#[test]
fn every_armed_pane_is_cleared_only_on_focus_loss() {
    let mut cleared = Vec::new();
    super::clear_completions_on_focus_loss(true, [10, 20, 30], |pane| cleared.push(pane));
    assert!(
        cleared.is_empty(),
        "gaining focus must not discard a fresh completion"
    );
    super::clear_completions_on_focus_loss(false, [10, 20, 30], |pane| cleared.push(pane));
    assert_eq!(
        cleared,
        [10, 20, 30],
        "losing focus must disarm every pane reached by broadcast input"
    );
}

#[test]
fn restarts_run_focus_change_cleanup() {
    let src = production_source();
    let body = src
        .split("fn process_pending_pane_restarts(")
        .nth(1)
        .unwrap()
        .split("\n    }\n")
        .next()
        .unwrap();
    assert!(
        body.contains("self.note_focus_change(ws, pre);"),
        "a replacement tab must clean up the old focus owner"
    );
    assert!(
        body.find("let pre = self.focus_key(ws);").unwrap()
            < body.find("ws.mux.new_tab_with_").unwrap()
    );
}


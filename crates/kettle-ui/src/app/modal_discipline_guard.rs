use super::{production_source, session_write_target};

#[test]
fn confirmation_owns_presses_before_settings() {
    let src = production_source();
    let flat: String = src.split_whitespace().collect();
    assert_eq!(
        flat.matches(
            "ws.held_buttons.pressed(bcode,None,(0,0));ifself.confirm_mouse_press(ws,event_loop,bcode){return"
        )
        .count(),
        2,
        "native and control presses must reach confirmation before underlying UI"
    );
    assert!(flat.contains(
        "ws.held_buttons.pressed(sgr,None,(0,0));ifself.confirm_mouse_press(ws,event_loop,sgr){return;"
    ));
    let helper = src
        .split_once("fn confirm_mouse_press(")
        .unwrap()
        .1
        .split("\n    fn ")
        .next()
        .unwrap();
    let helper: String = helper.split_whitespace().collect();
    assert!(helper.contains("ifws.confirm_dialog.is_none(){returnfalse;}"));
    assert!(helper.contains("ifbcode==0&&letSome(result)=self.confirm_button_click_result(ws)"));
    assert!(helper.contains("self.cancel_confirm_dialog(ws)"));
    assert!(helper.contains("self.dispatch_confirm_action(ws,to_run,event_loop)"));
    assert!(
        helper.ends_with("true}"),
        "an active confirmation must also consume non-button presses"
    );
}

#[test]
fn confirm_dialog_is_tracked_as_a_modal() {
    let src = production_source();
    let body = |name: &str| -> String {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("fn {name} not found"));
        let rest = &src[start..];
        let end = rest.find("\n    }").expect("fn end");
        rest[..end].to_string()
    };
    assert!(
        body("close_all_modals").contains("self.cancel_confirm_dialog(ws)"),
        "close_all_modals must clear confirm_dialog so it can't stack under \
             another overlay"
    );
    // `any_modal_open` and `pointer_modal_open` both derive from the one
    // `non_search_modal_open` list, so the confirm dialog only needs to be
    // counted there to gate keyboard and pointer input alike.
    assert!(
        body("non_search_modal_open").contains("ws.confirm_dialog.is_some()"),
        "non_search_modal_open must count the confirm dialog so input doesn't fall \
             through to the terminal behind it"
    );
    assert!(
        body("any_modal_open").contains("self.pointer_modal_open(ws)")
            && body("pointer_modal_open").contains("self.non_search_modal_open(ws)"),
        "both modal predicates must derive from the shared non-search list"
    );
}

/// `ask-before-closing` is only worth anything if EVERY way to close
/// something asks. Otherwise one stray click on a tab's ✕ or a middle-click
/// on a tab can make a window full of running work vanish, however the
/// setting is configured.
///
/// Each close site is checked for the `confirm_close` gate that raises the
/// prompt. The gate itself decides whether to prompt; what this pins is
/// that the decision is asked at all.
///
/// `WindowEvent::CloseRequested` (the titlebar ✕ and Alt+F4) is the one
/// deliberate exception — see the assertion below for why — and it is
/// pinned to its own, narrower contract rather than exempted.
#[test]
fn every_close_gesture_asks_before_closing() {
    let src = production_source();
    // Slice from a marker to the end of its enclosing arm/statement,
    // deliberately short so an inserted close below the window still fails.
    let after = |marker: &str, len: usize| -> String {
        let at = src
            .find(marker)
            .unwrap_or_else(|| panic!("close site `{marker}` not found"));
        src[at..].chars().take(len).collect()
    };

    // The titlebar ✕ and Alt+F4 are the ONE exception, and it is
    // deliberate: they are OS window-destroy requests rather than Kettle
    // commands, so they close without asking. `ask-before-closing = always`
    // is the opt-in that restores the question, and pinning that gate here
    // is what stops the exception from silently widening into "the ✕ never
    // asks, whatever you configured".
    let close_requested = after("WindowEvent::CloseRequested => {", 2400);
    // Pin the whole comparison, not just the enum name. Checking only that
    // `AskBeforeClosing::Always` appears somewhere in the arm would pass a
    // reversed `!=`, which inverts the entire policy: `always` would close
    // silently while every other setting prompted.
    let gate = [
        "self.cfg.ask_before_closing ",
        "== kettle_config::AskBeforeClosing::Always",
    ]
    .concat();
    assert!(
        close_requested.contains(&gate),
        "the titlebar ✕ / Alt+F4 close must prompt exactly when \
             `ask-before-closing` IS `always`"
    );
    assert!(
        close_requested.contains("self.confirm_close("),
        "the `always` branch of the ✕ close must route through the shared \
             confirm_close gate rather than raising its own dialog"
    );
    // Both tab-bar close gestures (the ✕ hit and middle-click) funnel
    // through one hit test, written once for real mouse input and once for
    // the ctl/agent path. Neither may skip the prompt. The needle is
    // assembled at runtime so this test does not match its own source.
    let needle = ["let close = bcode", " == 1 ||"].concat();
    let close_sites: Vec<_> = src.match_indices(needle.as_str()).collect();
    assert_eq!(
        close_sites.len(),
        2,
        "expected exactly two tab-close hit tests (GUI + ctl); a new one \
             must be gated too"
    );
    for (at, _) in close_sites {
        // Wide enough to reach each hit test's confirm_close call.
        let body: String = src[at..].chars().take(1400).collect();
        assert!(
            body.contains("self.confirm_close("),
            "a tab ✕ / middle-click close must ask before closing"
        );
        assert!(
            body.contains("ConfirmAction::CloseTabHolding("),
            "the ✕ can target a tab that is not the focused one, so the \
                 prompt must name that tab by a pane it holds — CloseTab would \
                 close the focused tab instead"
        );
    }
}

/// Drift guard. The confirm-dialog dispatch must honor
/// the close return values like the keybind paths: `close_tab()` /
/// `close_focused()` returning `true` means the last tab / pane closed, so
/// the window must `request_window_close` immediately instead of deferring
/// a tick and painting an empty frame. A behavioral test needs an event
/// loop; pin the honored returns at the source level.
#[test]
fn confirm_close_honors_close_returns() {
    let src = production_source();
    let start = src
        .find("fn dispatch_confirm_action_arms(")
        .expect("dispatch_confirm_action_arms not found");
    let rest = &src[start..];
    let end = rest.find("\n    }").expect("fn end");
    let body = &rest[..end];
    let tab_close = body
        .find("let last = ws.mux.close_tab_at(idx);")
        .expect("confirmed tab close must honor the close result");
    let tab_event = body
        .find("self.fire_tab_close_event(ws, idx);")
        .expect("confirmed tab close must emit its lifecycle event");
    let tab_exit = body
        .find("if last {")
        .expect("confirmed tab close must exit on the last tab");
    // Scoped to the TAB arm. The pane arm below also calls
    // `request_window_close`, so searching the whole dispatch would pass
    // with the call deleted from this arm.
    let tab_arm = &body[tab_close..];
    let tab_arm = &tab_arm[..tab_arm
        .find("ConfirmAction::ClosePane")
        .unwrap_or(tab_arm.len())];
    assert!(
        body.contains("ws.mux.tab_index_of_any_pane(&panes)")
            && tab_arm.contains("self.request_window_close(ws.seq);")
            && tab_close < tab_event
            && tab_event < tab_exit,
        "a confirmed tab close must re-resolve its target, emit the event \
             on every outcome, then exit when the last tab closes"
    );
    assert!(
        body.contains(concat!("let was_last = ws.mux.", "close_focused();"))
            && body.contains("if was_last {"),
        "ClosePane dispatch must exit when close_focused() reports the last pane"
    );
    // The confirmed pane close must act on the pane the PROMPT was raised
    // for, which means re-focusing that pane BEFORE closing. `close_focused`
    // acts on whatever is focused now, and while the prompt was up the
    // target's own shell may have exited and promoted a sibling — so
    // without the re-focus, confirming closes the sibling.
    let refocus = body
        .find(concat!("ws.mux.", "focus_pane(target)"))
        .expect("confirmed ClosePane must re-focus its recorded target");
    let pane_close = body
        .find(concat!("let was_last = ws.mux.", "close_focused();"))
        .expect("ClosePane close call");
    assert!(
        refocus < pane_close,
        "the re-focus must come BEFORE the close, or the close still acts \
             on whatever happens to be focused"
    );
    let viewing = body
        .find("let viewing = ws.mux.tab_anchor_panes(ws.mux.active);")
        .expect("remember the viewed tab by its panes");
    let restore = body
        .find("ws.mux.tab_index_of_any_pane(&viewing)")
        .expect("resolve the viewed tab after closing");
    assert!(viewing < refocus && pane_close < restore);
}

/// `split_auto` must actually pick an axis.
///
/// Every user-facing description says it picks by aspect ratio, and
/// Terminator splits along the pane's longer axis. If auto is treated as
/// plain `SplitDown`, a wider-than-tall pane stacks instead of splitting
/// side by side.
///
/// A behavioural test needs a live window, so this pins that the dispatch
/// consults the chooser rather than sharing `SplitDown`'s arm, and that the
/// chooser reads the pane's shape.
#[test]
fn split_auto_picks_an_axis_instead_of_always_splitting_down() {
    let src = production_source();

    let arm = src
        .split("Action::SplitDown | Action::SplitUp | Action::SplitAuto => {")
        .nth(1)
        .expect("the split-down/auto arm must exist")
        .split("\n            Action::")
        .next()
        .expect("arm body");
    assert!(
        arm.contains("if action == Action::SplitAuto"),
        "the arm must distinguish auto from down — sharing the arm outright \
             is what made `split_auto` a plain `split_down`"
    );
    assert!(
        arm.contains("self.auto_split_dir(ws)"),
        "auto must ask for a direction rather than hardcoding one"
    );

    let chooser = src
        .split("fn auto_split_dir(&self, ws: &WindowState) -> Dir {")
        .nth(1)
        .expect("auto_split_dir must exist")
        .split("\n    fn ")
        .next()
        .expect("chooser body");
    assert!(
        chooser.contains(".leaves(ws.mux.active, area)") && chooser.contains("*id == focus"),
        "the choice must come from the focused pane's leaf, not the window"
    );
    assert!(
        chooser.contains("rect.2 > rect.3")
            && chooser.contains("Dir::Horizontal")
            && chooser.contains("Dir::Vertical"),
        "a wider-than-tall pane must split horizontally and a taller one \
             vertically"
    );
}

/// Quitting must not erase the workspace you launched from.
///
/// `close_window` deliberately empties the mux before saving so the
/// SESSION it writes is the empty one — "this window is finished, do not
/// bring it back". Sending that same empty snapshot to a NAMED LAYOUT would
/// wipe the workspace, and the next `--layout NAME` would open a single
/// default pane. Terminator only ever writes a layout from an explicit
/// Add/Refresh.
///
/// The whole routing decision, as a table.
#[test]
fn an_empty_snapshot_never_overwrites_a_named_layout() {
    use super::SessionWriteTarget::{DefaultSession, NamedLayout, Skip};

    // The regression: empty snapshot, named layout, layout writable.
    assert_eq!(
        session_write_target(Some("dev"), true, false, true),
        Skip,
        "closing a window must not write an empty layout over the \
             workspace it was launched from"
    );
    // ...and the same for a non-empty one, which is the case that must
    // keep working — otherwise the guard above could be implemented by
    // never writing layouts at all.
    assert_eq!(
        session_write_target(Some("dev"), true, false, false),
        NamedLayout("dev"),
        "a real arrangement must still be saved to its layout"
    );

    // A launch override means this window was never restored from that
    // layout, so it does not get to rewrite it either way.
    assert_eq!(session_write_target(Some("dev"), false, false, false), Skip);
    assert_eq!(session_write_target(Some("dev"), false, false, true), Skip);

    // session.json keeps its existing contract: written only in restore
    // mode, and `close_window`'s deliberate clear still reaches it. That
    // intent belongs to the session, not to a named workspace.
    assert_eq!(
        session_write_target(None, true, true, false),
        DefaultSession
    );
    assert_eq!(
        session_write_target(None, true, true, true),
        DefaultSession,
        "clearing the default session on close is deliberate and must \
             survive this fix"
    );
    assert_eq!(session_write_target(None, true, false, false), Skip);
    assert_eq!(session_write_target(None, true, false, true), Skip);
}

/// Answering a confirmation must end with the same resize tail every
/// keybind and menu action gets, and that tail must live in exactly one
/// place.
///
/// `dispatch_confirm_action` is a second entry point into the same state
/// changes. Without the tail, a confirmed close collapses the layout while
/// the surviving PTYs keep their old size. Pinning the tail to the wrapper,
/// and asserting the arms do NOT carry their own, stops a future arm from
/// dropping it. A behavioral check needs a live event loop and a window;
/// the file's other confirm-dispatch guards are source-level for the same
/// reason.
#[test]
fn confirm_dispatch_resizes_once_for_every_arm() {
    let src = production_source();
    // Built at runtime so this guard cannot match its own source.
    let resize = ["self.", "resize_all(ws);"].concat();

    let body_of = |name: &str| -> String {
        let start = src.find(name).unwrap_or_else(|| panic!("{name} not found"));
        let rest = &src[start..];
        let end = rest.find("\n    }").expect("fn end");
        rest[..end].to_string()
    };

    let wrapper = body_of("fn dispatch_confirm_action(");
    let arms = body_of("fn dispatch_confirm_action_arms(");
    assert!(
        wrapper.contains(&resize),
        "the confirm dispatch must end with the resize tail, or every \
             confirmed close leaves the surviving panes' PTYs at their \
             pre-close size"
    );
    assert!(
        wrapper.contains("if !self.window_close_pending(ws.seq)"),
        "a window on its way out has nothing left to resize"
    );
    assert!(
        !arms.contains(&resize),
        "the tail belongs to the wrapper alone; an arm carrying its own \
             copy is how the next arm ends up without one"
    );
    // The wrapper must actually delegate, or the assertions above pass
    // against a function that does nothing.
    assert!(
        wrapper.contains("self.dispatch_confirm_action_arms(ws, action);"),
        "the wrapper must delegate to the arms"
    );
    assert!(
        arms.contains("ConfirmAction::ClosePane(target)"),
        "the arms function must be the one holding the match"
    );
}

/// A split that fails to spawn has to say so.
///
/// Both split arms used to log at `warn`, which the default level hides.
/// The user then reports "the split did not load", which is also what a
/// pane that spawned and died looks like, so the two get conflated. Every
/// split arm routes its error through the one reporter that raises the
/// level and fires a notification.
#[test]
fn every_split_arm_reports_a_failure_the_user_can_see() {
    let source = production_source();
    // Built at runtime so this guard cannot match its own source.
    let report = ["report_split_", "failure("].concat();
    let swallowed = ["could not split pane (", "right): {e}"].concat();

    // One definition plus one call per arm: the two split actions and the
    // remote reconnect, which opens its pane as a split.
    assert_eq!(
        source.matches(&report).count(),
        4,
        "every split arm must report through the shared reporter; add the \
             call when you add an arm"
    );
    assert!(
        !source.contains(&swallowed),
        "a split failure logged inline is invisible at the default level"
    );
    let body = source
        .split("pub(crate) fn report_split_failure(")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("report_split_failure body");
    assert!(
        body.contains("log::error!") && body.contains("fire_notify("),
        "the reporter must both raise the log level and surface a notice"
    );
}

/// A pane that exits on its own must resize whatever inherits its space.
///
/// `Mux::reap` prunes the dead pane and `reap_tabs` promotes the sibling
/// into the whole rectangle. The renderer paints from a live layout, so
/// the survivor *looks* right immediately; its PTY does not move, gets no
/// `SIGWINCH`, and a full-screen TUI keeps drawing into the old box. An
/// explicit close like `Ctrl+Shift+W` avoids this only because it falls
/// through to `handle_action`'s tail.
///
/// Source-level because the behavior needs a live event loop, a window and
/// a real child process; `just split-exit-resize-smoke` is the behavioral
/// half. Every needle is built at runtime and matched against
/// `production_source()`, so the guard can match neither its own text nor
/// any test.
#[test]
fn every_reap_site_schedules_the_survivor_resize() {
    let source = production_source();
    let reap_call = ["ws.mux.", "reap()"].concat();
    let title_clear = [
        "let stale_title = clear_stale_title_edit(&ws.mux, ",
        "&mut ws.editing_title);",
    ]
    .concat();
    // Pin the gate itself, not merely the scheduling lines. A title edit
    // alone also triggers those lines, so their presence does not prove
    // that the reap's own answer reaches the condition.
    let gate = ["if reaped.layout_", "changed || stale_title {"].concat();
    let schedule = ["ws.pending_", "resize = true;"].concat();
    let repaint = ["window.request_", "redraw();"].concat();
    // `clear_stale_title_edit` mutates `ws.editing_title`. Behind a `||`
    // it stops running whenever a pane died, stranding a title edit on a
    // pane that no longer exists. The `let` binding above is what keeps
    // the call unconditional; this needle catches the reordering.
    let short_circuit = ["|| clear_stale_", "title_edit"].concat();

    let sites = source.split(&reap_call).skip(1).count();
    assert!(
        sites >= 2,
        "expected the redraw and about-to-wait reaps; the needle stopped \
             matching, so this guard was about to pass vacuously"
    );
    let scheduled = source
        .split(&reap_call)
        .skip(1)
        .filter(|tail| {
            // Room for the drag reset and the comment ahead of the gate.
            let window = &tail[..tail.len().min(900)];
            window.contains(&title_clear)
                && window.contains(&gate)
                && window.contains(&schedule)
                && window.contains(&repaint)
                && !window.contains(&short_circuit)
        })
        .count();
    assert_eq!(
        scheduled, sites,
        "every reap must mark a resize and request the frame that flushes \
             it, or the pane that inherited the dead pane's space keeps the \
             rows and columns it had inside the split"
    );
}

#[test]
fn pane_close_lifecycle_events_precede_pty_teardown() {
    let source = production_source();
    // Construct these needles so this guard does not count its own source.
    let close_call = ["ws.mux.", "close_focused();"].concat();
    let focus_capture = ["let closing_pane = ws.mux.", "active_focus();"].concat();
    let lifecycle_call = ["self.fire_pane_close_event", "(ws, id);"].concat();
    // The confirmed close names its target explicitly rather than reading
    // the current focus, so it spells both of these differently.
    let confirm_capture = ["if !ws.mux.focus", "_pane(target) {"].concat();
    let confirm_event = ["self.fire_pane_close_event", "(ws, target);"].concat();
    let close_positions = source
        .match_indices(&close_call)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    assert_eq!(
        close_positions.len(),
        2,
        "every close_focused call site must be lifecycle-audited"
    );
    // Each site is checked against the region since the PREVIOUS site, not
    // against the whole prefix. Over the whole prefix, the confirmed-close
    // arm could match the direct-close arm's capture/event pair further up
    // the file and pass without its own lifecycle event.
    let mut region_start = 0usize;
    for close_position in close_positions {
        let region = &source[region_start..close_position];
        let capture_position = region
            .rfind(&focus_capture)
            .or_else(|| region.rfind(&confirm_capture))
            .expect("pane id captured before close, in this arm");
        let event_position = region
            .rfind(&lifecycle_call)
            .or_else(|| region.rfind(&confirm_event))
            .expect("pane-close lifecycle event present, in this arm");
        assert!(
            capture_position < event_position,
            "pane-close callbacks must run while the addressed PTY still exists"
        );
        region_start = close_position;
    }
}

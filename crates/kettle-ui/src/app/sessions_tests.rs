#[test]
fn failed_additional_window_restore_preserves_source_session() {
    let src = super::production_source();
    let restore = src
        .split_once("for (sw, surface) in rest.iter().zip(&surfaces[1..]) {")
        .unwrap()
        .1
        .split("// Focus comes home to window 1")
        .next()
        .unwrap();
    let flat: String = restore.split_whitespace().collect();
    assert!(
        flat.contains(".is_err(){self.stash_unrestorable_session("),
        "every additional-window open failure must preserve the loaded session"
    );
}

/// A shell `cd`, a dragged divider and a renamed tab all change what
/// `session.json` should say without any gesture that saves it, so a sweep
/// picks them up. The sweep is affordable because it only runs on a turn
/// the event loop was taking anyway. As a wakeup deadline, it would wake an
/// idle kettle every two seconds forever, and the idle CPU figure the perf
/// suite publishes would go with it.
#[test]
fn the_session_sweep_rides_existing_turns_and_never_schedules_one() {
    let t0 = std::time::Instant::now();
    assert!(
        session_sweep_due(None, t0, true),
        "the first turn sweeps, so a workspace that changes and then goes \
             quiet is still written"
    );
    assert!(!session_sweep_due(Some(t0), t0, true));
    assert!(!session_sweep_due(
        Some(t0),
        t0 + super::SESSION_SWEEP - std::time::Duration::from_millis(1),
        true
    ));
    assert!(session_sweep_due(Some(t0), t0 + super::SESSION_SWEEP, true));
    // A timestamp from the future (a paused VM, a mid-turn re-entry)
    // must not make every turn a sweep.
    assert!(!session_sweep_due(
        Some(t0 + super::SESSION_SWEEP),
        t0,
        true
    ));

    // An empty window is never swept, however overdue the clock says it
    // is. Its snapshot is `{"tabs":[],"windows":[]}`, and a window is
    // empty exactly when it has not opened yet or is on its way out —
    // writing then would erase the session about to be restored. The
    // deliberate erasures (`close_window`, the reap above this call) save
    // on their own and are not affected.
    assert!(!session_sweep_due(None, t0, false));
    assert!(!session_sweep_due(
        Some(t0),
        t0 + super::SESSION_SWEEP * 1000,
        false
    ));

    // And the constant is named in exactly two places: its own
    // declaration and the predicate above. A third mention would mean it
    // had found its way into a deadline. Comment lines are dropped first so
    // prose about the constant does not count as a use of it.
    let code = production_source()
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        code.matches("SESSION_SWEEP").count(),
        2,
        "SESSION_SWEEP should be named only by its declaration and \
             `session_sweep_due`; a third reference means it reached a wakeup"
    );
    let wait = code
        .split("fn about_to_wait_inner(")
        .nth(1)
        .expect("about_to_wait_inner present");
    assert!(
        wait.contains("session_sweep_due(self.last_session_sweep, now, !ws.mux.tabs.is_empty())"),
        "the sweep must be driven from the event-loop turn itself, and must \
             pass the window's own emptiness so it cannot overwrite a saved \
             session with a window that has nothing in it"
    );
}

/// The default-session restore is opt-in. The same predicate gates BOTH
/// the startup `load()` and the `save()` so they can't drift apart; an
/// unconditional save lets a fresh window clobber the saved layout. (F,F),
/// the default, means do NOT touch session.json; any opt-in (`--restore`
/// one-shot OR `restore-session = true`) turns both load and save back on.
#[test]
fn restore_session_gate_truth_table() {
    assert!(
        !should_restore_session(false, false, false),
        "fresh default: no load, no save"
    );
    assert!(
        should_restore_session(true, false, false),
        "--restore one-shot"
    );
    assert!(
        should_restore_session(false, true, false),
        "restore-session = true"
    );
    assert!(should_restore_session(true, true, false));
}

/// #575: after an update, the new build's first window opens while the old
/// version's windows are still running. The session is still theirs, so the
/// new build neither restores it (duplicating their tabs) nor overwrites it.
#[test]
fn config_restore_and_save_wait_for_another_build_to_exit() {
    for (startup_restore, cfg_restore) in [(false, true), (true, false), (true, true)] {
        assert!(!should_restore_session(startup_restore, cfg_restore, true));
        assert!(should_restore_session(startup_restore, cfg_restore, false));
    }
}

#[test]
fn later_saved_windows_do_not_depend_on_the_first_restore_succeeding() {
    let src = production_source();
    let restore = src
        .split("if let Some((first, _)) = wins.split_first() {")
        .nth(1)
        .expect("multi-window startup restore");
    let (before_loop, _) = restore
        .split_once(" in rest.iter()")
        .expect("additional windows are attempted");
    assert!(!before_loop.contains("if outcome.restored_any {"));
    assert!(restore.contains("outcome.restored_any"));
}

/// Drift guard: in `load_startup_session()`, the explicit restore paths
/// (`--tab-handoff-fd`, `--tab-handoff`, `--layout`) must resolve BEFORE
/// the opt-in default-session branch, so a stale `session.json` can never
/// override an explicit launch target.
#[test]
fn explicit_restore_paths_precede_default_session() {
    let src = production_source();
    // The LOAD gate specifically (`} else if should_restore_session(...)`),
    // not the save path's call with the same args earlier in the file.
    let gate = src
        .find("else if should_restore_session(\n            self.startup.restore")
        .expect("the load gate is `} else if should_restore_session(self.startup.restore, ...)`");
    let before = &src[..gate];
    for marker in [
        "self.startup.tab_handoff_fd",
        "self.startup.tab_handoff.as_deref()",
        "self.startup.layout.as_deref()",
    ] {
        assert!(
            before.contains(marker),
            "{marker} must be resolved BEFORE the opt-in default-session branch \
                 (explicit launch targets outrank a stale session)"
        );
    }
    // And the SAVE side must be gated by the same predicate (no clobber).
    // The routing now lives in `session_write_target`, so this checks both
    // halves: that `save_session` feeds it the predicate, and that the
    // default-session arm is the only thing that predicate unlocks.
    assert!(
        src.contains(
            "should_restore_session(\n                self.startup.restore,\n                self.cfg.restore_session,\n                self.other_builds.running(),\n            ),"
        ),
        "save_session must pass should_restore_session into the write \
             routing — symmetric with the load gate (audit M1)"
    );
    let routing = src
        .split("fn session_write_target(")
        .nth(1)
        .and_then(|body| body.split("\n}").next())
        .expect("session_write_target body");
    assert!(
        routing.contains("None if restore_mode => SessionWriteTarget::DefaultSession"),
        "the default session.json write must be the restore-mode arm"
    );
    assert!(
        routing.contains("Some(_) if snapshot_is_empty => SessionWriteTarget::Skip"),
        "an empty snapshot must never overwrite a named layout — closing a \
             window would erase the workspace it was launched from"
    );
}

#[test]
fn implicit_zoom_exits_restore_the_saved_font() {
    let src = production_source();
    for (start, end) in [
        (
            "Action::SplitRight | Action::SplitLeft => {",
            "Action::SplitDown",
        ),
        (
            "Action::SplitDown | Action::SplitUp | Action::SplitAuto => {",
            "Action::ClosePane",
        ),
        ("Action::DuplicatePane => {", "Action::NextTheme"),
    ] {
        let arm = src.split_once(start).unwrap().1.split_once(end).unwrap().0;
        assert!(arm.contains("} else {\n                    self.restore_scaled_zoom_font(ws);"));
    }
    let rotation = src
        .split_once("Action::RotateCw | Action::RotateCcw => {")
        .unwrap()
        .1;
    let restore = rotation.find("self.restore_scaled_zoom_font(ws);").unwrap();
    let resize = rotation.find("self.resize_all(ws);").unwrap();
    assert!(restore < resize);
}

#[test]
fn review_unread_startup_snapshot_disables_saving() {
    let mut failed = false;
    let snapshot = super::sessions::loaded_startup_snapshot(
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "held layout lock",
        )),
        &mut failed,
    );
    assert!(snapshot.is_none());
    assert!(failed, "an unread snapshot must disable automatic saving");
    let source: String = include_str!("sessions.rs").split_whitespace().collect();
    assert!(
        source
            .contains("fnsave_session(&self,ws:&WindowState){ifself.session_load_failed{return;}"),
        "automatic saves must stop before taking a default snapshot"
    );
    // Both named layouts and the default restored session need this guard.
    for layout in [Some("dev"), None] {
        let target = if failed {
            super::SessionWriteTarget::Skip
        } else {
            super::session_write_target(layout, true, true, false)
        };
        assert_eq!(target, super::SessionWriteTarget::Skip);
    }
}


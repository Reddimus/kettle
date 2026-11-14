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
/// picks them up. It rides turns the event loop takes anyway, at most once per
/// interval, and the one wake it may ask for is the end of that interval.
#[test]
fn the_session_sweep_rides_existing_turns_at_most_once_per_interval() {
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
}

/// The wake a change is owed is the end of the interval that the last sweep
/// started, and nothing else asks for one: not a turn with nothing new, not a
/// sweep that is already due (it runs this turn), not a launch that saves
/// nothing, and not before the first sweep has run.
#[test]
fn session_sweep_wake_truth_table() {
    use super::sessions::{SessionWriteTarget, autosave_eligible, session_sweep_wake};
    use kettle_config::RestoreSession::{Always, Never};
    let t0 = std::time::Instant::now();
    let sweep = super::SESSION_SWEEP;
    let half = sweep / 2;
    assert_eq!(
        session_sweep_wake(Some(t0), t0 + half, true, true),
        Some(t0 + sweep)
    );
    assert_eq!(
        session_sweep_wake(Some(t0), t0, true, true),
        Some(t0 + sweep)
    );
    // Untouched: nothing happened since the sweep, so idle stays idle.
    assert_eq!(session_sweep_wake(Some(t0), t0 + half, false, true), None);
    // Due: the sweep runs on this turn, never from a deadline in the past.
    assert_eq!(session_sweep_wake(Some(t0), t0 + sweep, true, true), None);
    assert_eq!(
        session_sweep_wake(Some(t0), t0 + sweep * 3, true, true),
        None
    );
    // No sweep yet: the first turn with a window that has tabs sweeps.
    assert_eq!(session_sweep_wake(None, t0, true, true), None);
    assert_eq!(session_sweep_wake(Some(t0), t0 + half, true, false), None);

    // What makes a launch ineligible.
    let target = |restore_mode| super::session_write_target(None, true, restore_mode, false);
    let restore_off = target(should_restore_session(false, Never, false));
    let another_build = target(should_restore_session(false, Always, true));
    let saving = target(should_restore_session(false, Always, false));
    assert_eq!(restore_off, SessionWriteTarget::Skip);
    assert_eq!(another_build, SessionWriteTarget::Skip);
    assert!(autosave_eligible(false, &saving, true));
    assert!(!autosave_eligible(false, &restore_off, true), "restore off");
    assert!(!autosave_eligible(false, &another_build, true), "another build runs");
    assert!(!autosave_eligible(true, &saving, true), "the load failed");
    assert!(!autosave_eligible(false, &saving, false), "no window has tabs");
}

/// Drives `SessionSweep` the way `about_to_wait` does, on a fake clock: each
/// turn gives every window `take_due` and then asks for the app's one
/// deadline. Returns the times the sweep saved, the state each one saw, and
/// the deadline left when the events stop.
fn model_sweep_turns(
    events: &[(f64, u32)],
    until: f64,
    eligible: bool,
) -> (Vec<(f64, u32)>, Option<f64>) {
    let t0 = std::time::Instant::now();
    let at = |secs: f64| t0 + std::time::Duration::from_secs_f64(secs);
    let secs = |instant: std::time::Instant| instant.duration_since(t0).as_secs_f64();
    let mut sweep = super::sessions::SessionSweep::default();
    let mut saves = Vec::new();
    let mut state = 0;
    let mut deadline = None;
    let mut next_event = 0;
    loop {
        // The loop wakes for the next event or the deadline, whichever is
        // first; a wake for the deadline delivers no event.
        let event_time = events.get(next_event).map(|&(time, _)| time);
        let now = match (event_time, deadline) {
            (Some(event), Some(wake)) if wake < event => wake,
            (Some(event), _) => event,
            (None, Some(wake)) => wake,
            (None, None) => break,
        };
        if now > until {
            break;
        }
        if event_time == Some(now) {
            state = events[next_event].1;
            next_event += 1;
            sweep.touch();
        }
        // The sweep runs on time whatever the launch saves; `save_session`
        // is what writes nothing for an ineligible one.
        if sweep.take_due(at(now), true) && eligible {
            saves.push((now, state));
        }
        deadline = sweep.deadline(at(now), false, || eligible).map(secs);
    }
    (saves, deadline)
}

/// The trailing edge: a change made after the last sweep and before Kettle
/// goes quiet reaches the session within one interval, and once it has, no
/// deadline is left to keep waking an idle Kettle.
#[test]
fn a_change_before_going_quiet_is_swept_within_the_interval() {
    let (saves, left) = model_sweep_turns(&[(0.0, 1), (0.4, 2), (1.1, 3)], 60.0, true);
    let interval = super::SESSION_SWEEP.as_secs_f64();
    assert_eq!(saves.first(), Some(&(0.0, 1)), "the first turn sweeps");
    let last = saves.last().copied().unwrap();
    assert_eq!(last.1, 3, "the state at 1.1 s was never written: {saves:?}");
    assert!(last.0 <= interval + 1e-9, "written only at {} s", last.0);
    assert_eq!(saves.len(), 2, "one trailing sweep, not a loop: {saves:?}");
    assert_eq!(left, None, "a deadline outlived the change");

    // Idle: one event, one sweep, nothing after it.
    let (saves, left) = model_sweep_turns(&[(5.0, 1)], 60.0, true);
    assert_eq!(saves, [(5.0, 1)]);
    assert_eq!(left, None);

    // A launch that saves nothing never asks to be woken.
    let (saves, left) = model_sweep_turns(&[(0.0, 1), (0.4, 2)], 60.0, false);
    assert!(saves.is_empty());
    assert_eq!(left, None);

    // Events that keep coming are swept on their own turns, at most once
    // per interval, and the wake adds only the one after the last of them.
    let events: Vec<_> = (0..100).map(|i| (f64::from(i) * 0.1, i as u32 + 1)).collect();
    let (saves, left) = model_sweep_turns(&events, 60.0, true);
    for pair in saves.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= interval - 1e-9, "{saves:?}");
    }
    assert_eq!(saves.last().unwrap().1, 100);
    assert_eq!(left, None);
}

/// Source guards for the wake: one deadline per app, from the turn's events.
#[test]
fn the_session_sweep_wake_is_armed_only_by_events_and_cleared_only_by_the_sweep() {
    // Comment lines are dropped first so prose about the constant does not
    // count as a use of it.
    let code = production_source()
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        code.matches("SESSION_SWEEP").count(),
        3,
        "SESSION_SWEEP should be named only by its declaration, \
             `session_sweep_due` and `session_sweep_wake`"
    );
    let wait = code
        .split("fn about_to_wait_inner(")
        .nth(1)
        .expect("about_to_wait_inner present");
    assert!(
        wait.contains("if self.session_sweep.take_due(now, !ws.mux.tabs.is_empty()) {\n            self.save_session(ws);"),
        "the sweep must be driven from the event-loop turn itself, and must \
             pass the window's own emptiness so it cannot overwrite a saved \
             session with a window that has nothing in it"
    );
    let app = code
        .split("fn about_to_wait(&mut self")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("about_to_wait body");
    let per_window = app.find("self.about_to_wait_inner(&mut ws, event_loop)").unwrap();
    let wake = app.find("sweep.deadline(").expect("the app asks for the wake");
    assert!(per_window < wake, "the wake is asked for after every window's turn");
    assert!(
        app[wake..].contains("if let Some(deadline) = session_wake {\n            earliest_deadline ="),
        "the wake merges into the turn's earliest deadline"
    );
    let window_event = code
        .split("fn window_event(&mut self")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    let redraw = window_event
        .split("if matches!(&event, WindowEvent::RedrawRequested) {")
        .nth(1)
        .unwrap();
    let (redraw_arm, other_arm) = redraw.split_once("} else {").unwrap();
    assert!(!redraw_arm.contains("touch()"), "a redraw would re-arm the wake every frame");
    assert!(other_arm.split("};").next().unwrap().contains("self.session_sweep.touch();"));
    let user_event = code
        .split("fn user_event(&mut self")
        .nth(1)
        .and_then(|body| body.split("match ev {").next())
        .unwrap();
    assert!(user_event.contains("self.session_sweep.touch();"));
    // Only `take_due` (the sweep) and a deadline whose time came and went
    // without one clear the flag; nothing outside the type can.
    let sweep_type = code
        .split("struct SessionSweep {")
        .nth(1)
        .and_then(|body| body.split("\nfn session_sweep_wake(").next())
        .unwrap();
    assert_eq!(sweep_type.matches("self.touched = false;").count(), 3);
    assert_eq!(code.matches("touched = false").count(), 3);
    assert!(!code.contains("session_sweep.touched"));
}

/// The same predicate gates both default-session load and save. Disabling
/// restore must leave session.json untouched so a fresh window cannot clobber
/// the saved layout. --restore enables both for one launch.
#[test]
fn restore_session_gate_truth_table() {
    assert!(
        !should_restore_session(false, kettle_config::RestoreSession::Never, false),
        "explicit false: no load, no save"
    );
    assert!(
        should_restore_session(true, kettle_config::RestoreSession::Never, false),
        "--restore one-shot"
    );
    assert!(
        should_restore_session(false, kettle_config::RestoreSession::Always, false),
        "restore-session = true"
    );
    assert!(should_restore_session(
        true,
        kettle_config::RestoreSession::Always,
        false
    ));
}

/// Linux has no system window-persistence setting: `auto` keeps the session,
/// as Ptyxis does. Windows still opens fresh.
#[test]
fn auto_restore_follows_the_platform_default() {
    let auto = should_restore_session(false, kettle_config::RestoreSession::Auto, false);
    if cfg!(target_os = "linux") {
        assert!(auto);
    } else if cfg!(windows) {
        assert!(!auto);
    }
    assert!(!should_restore_session(
        false,
        kettle_config::RestoreSession::Auto,
        true
    ));
}

/// #575: after an update, the new build's first window opens while the old
/// version's windows are still running. The session is still theirs, so the
/// new build neither restores it (duplicating their tabs) nor overwrites it.
#[test]
fn config_restore_and_save_wait_for_another_build_to_exit() {
    for (startup_restore, cfg_restore) in [(false, true), (true, false), (true, true)] {
        let cfg_restore = if cfg_restore {
            kettle_config::RestoreSession::Always
        } else {
            kettle_config::RestoreSession::Never
        };
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
            .contains("fnsave_session(&self,ws:&WindowState){self.clear_disabled_content();ifself.session_load_failed{return;}"),
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

#[test]
fn repeated_restore_recovery_keeps_session_saving_enabled() {
    let temp = kettle_test_support::private_tempdir("kettle-repeat-restore-");
    let path = temp.path().join("session.json");
    crate::session::write_test_session_file(&path, br#"{"tabs":[],"active":0}"#);
    let (_, identity) = crate::session::load_snapshot_result(&path)
        .unwrap()
        .unwrap();
    let mut source = Some(path.clone());
    let mut failed = false;
    for _ in 0..2 {
        super::sessions::recover_startup_snapshot(
            &mut source,
            Some(&identity),
            &mut failed,
            "window restore failed",
        );
    }
    assert!(
        !failed,
        "successful recovery must remain successful after another window fails"
    );
    assert!(!path.exists());
}

#[test]
fn saved_windows_fit_restore_budget_in_focus_order() {
    let mut windows: Vec<_> = (1..=17)
        .map(|seq| {
            let window: crate::session::SWindow = serde_json::from_value(serde_json::json!({
                "tabs": [{"root": {"Leaf": {"cwd": null}}, "title_override": seq.to_string()}]
            }))
            .unwrap();
            (seq, window)
        })
        .collect();
    windows.push((
        18,
        crate::session::SWindow {
            tabs: Vec::new(),
            active: 0,
            geometry: None,
        },
    ));
    let mut focus_order = std::collections::VecDeque::new();
    for seq in [16, 3, 17, 18] {
        super::sessions::record_window_focus(&mut focus_order, seq);
    }
    let windows = super::sessions::session_windows_for_save(windows, &focus_order);
    assert_eq!(
        windows.len(),
        16,
        "session saves must retain at most 16 nonempty windows"
    );
    let titles: Vec<_> = windows
        .iter()
        .map(|w| w.tabs[0].title_override.as_deref().unwrap())
        .collect();
    assert_eq!(
        titles,
        [
            "17", "3", "16", "1", "2", "4", "5", "6", "7", "8", "9", "10", "11", "12", "13", "14"
        ]
    );
    let session = crate::session::Session {
        tabs: windows[0].tabs.clone(),
        active: windows[0].active,
        windows,
        theme: None,
    };
    let dir = crate::test_tempdir();
    let path = dir.path().join("session.json");
    crate::session::save_to_path(&session, &path).expect("save bounded session");
    let loaded: crate::session::Session =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(loaded.validated_restore_windows().unwrap().len(), 16);
    assert_eq!(loaded.tabs[0].title_override.as_deref(), Some("17"));
}

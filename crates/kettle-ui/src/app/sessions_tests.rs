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
        wait.contains("if self.session_sweep.take_due(now, !ws.mux.tabs.is_empty()) {\n            self.save_session_as(ws, SaveKind::Sweep);"),
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
    // The LOAD gate specifically (`} else if self.default_session_restoring()`),
    // not the save path's use of the same predicate earlier in the file.
    let gate = src
        .find("} else if self.default_session_restoring() {")
        .expect("the load gate is `} else if self.default_session_restoring()`");
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
        src.contains("self.named_layout_writable,\n            self.default_session_restoring(),\n            false,"),
        "save_session must pass the load gate's predicate into the write \
             routing — symmetric with the load gate (audit M1)"
    );
    assert!(src.contains(
        "should_restore_session(self.startup.restore, self.cfg.restore_session, false)\n            && !self.default_session_elsewhere()"
    ));
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
            .contains("fnsave_session_as(&self,ws:&WindowState,kind:SaveKind){self.clear_disabled_content();ifself.session_load_failed{return;}"),
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

/// The content key changes with everything a snapshot depends on and with
/// nothing else, so an unchanged key can keep the file.
#[test]
fn content_key_tracks_every_input() {
    use crate::session_content::{ContentKey, PaneContentKey};
    let pane = |id, output, geometry, remote| PaneContentKey {
        id,
        output,
        geometry,
        remote,
        remote_end: None,
    };
    let key = |hash: u8, lines, remote, windows: Vec<(u64, Vec<PaneContentKey>)>| {
        ContentKey::new([hash; 32], lines, remote, windows)
    };
    let base = || vec![(1, vec![pane(7, 10, 2, false), pane(8, 4, 1, false)])];
    let reference = key(0, 1000, false, base());
    assert_eq!(reference, key(0, 1000, false, base()), "nothing changed");
    let mut variants = vec![
        ("layout", key(1, 1000, false, base())),
        ("restore-scrollback-lines", key(0, 999, false, base())),
        ("restore-scrollback-remote", key(0, 1000, true, base())),
        (
            "window order",
            key(0, 1000, false, vec![(2, base()[0].1.clone())]),
        ),
        (
            "pane order",
            key(
                0,
                1000,
                false,
                vec![(1, vec![pane(8, 4, 1, false), pane(7, 10, 2, false)])],
            ),
        ),
    ];
    for (name, change) in [
        ("output generation", pane(7, 11, 2, false)),
        ("geometry generation", pane(7, 10, 3, false)),
        ("remote", pane(7, 10, 2, true)),
        ("pane id", pane(9, 10, 2, false)),
    ] {
        variants.push((
            name,
            key(0, 1000, false, vec![(1, vec![change, pane(8, 4, 1, false)])]),
        ));
    }
    variants.push((
        "where a remote session ended",
        key(
            0,
            1000,
            false,
            vec![(
                1,
                vec![
                    PaneContentKey {
                        remote_end: Some(40),
                        ..pane(7, 10, 2, false)
                    },
                    pane(8, 4, 1, false),
                ],
            )],
        ),
    ));
    for (name, variant) in variants {
        assert_ne!(variant, reference, "the key ignores {name}");
    }
}

/// A save keeps the output file only when the key matches the last snapshot
/// submitted and the writer has not found that file gone. The exit save
/// always takes a fresh one.
#[test]
fn only_an_unchanged_key_keeps_the_output_file() {
    use super::sessions::{ContentThrottle, SaveKind};
    let t0 = std::time::Instant::now();
    let key = content_key(0, 1);
    for kind in [SaveKind::Gesture, SaveKind::Sweep] {
        let mut throttle = ContentThrottle::default();
        assert!(throttle.decide(&key, t0, kind, false), "nothing submitted yet");
        assert!(!throttle.decide(&key, t0, kind, false), "unchanged");
        assert!(throttle.decide(&key, t0, kind, true), "the file went stale");
        assert!(!throttle.pending);
    }
    let mut throttle = ContentThrottle::default();
    assert!(throttle.decide(&key, t0, SaveKind::Gesture, false));
    assert!(throttle.decide(&key, t0, SaveKind::Exit, false));
}

/// A key for one pane in one window, with layout `layout` and output
/// generation `output`.
fn content_key(layout: u8, output: u64) -> crate::session_content::ContentKey {
    content_key_with(layout, 1000, false, output)
}

fn content_key_with(
    layout: u8,
    lines: usize,
    remote: bool,
    output: u64,
) -> crate::session_content::ContentKey {
    crate::session_content::ContentKey::new(
        [layout; 32],
        lines,
        remote,
        vec![(
            1,
            vec![crate::session_content::PaneContentKey {
                id: 1,
                output,
                geometry: 0,
                remote: false,
                remote_end: None,
            }],
        )],
    )
}

/// While output keeps changing, a sweep replaces its snapshot at most every
/// 30 seconds. Once it stops, the next two sweeps write it: the first sees
/// it changed since the sweep before, the second sees it still.
#[test]
fn streaming_content_waits_30_s_and_writes_once_quiet() {
    use super::sessions::{CONTENT_STREAM_INTERVAL, ContentThrottle, SaveKind};
    assert_eq!(CONTENT_STREAM_INTERVAL, std::time::Duration::from_secs(30));
    let t0 = std::time::Instant::now();
    let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    let sweep = SaveKind::Sweep;

    // Truth table, from a write at 0 with output generation 1.
    let fresh = || {
        let mut throttle = ContentThrottle::default();
        assert!(throttle.decide(&content_key(0, 1), at(0), sweep, false));
        throttle
    };
    let mut throttle = fresh();
    assert!(!throttle.decide(&content_key(0, 2), at(2), sweep, false), "streaming");
    assert!(throttle.pending, "a write put off asks for the next sweep");
    assert!(throttle.decide(&content_key(0, 2), at(4), sweep, false), "quiet");
    assert!(!throttle.pending);
    let mut throttle = fresh();
    for (secs, output) in [(2, 2), (4, 3), (28, 4)] {
        assert!(!throttle.decide(&content_key(0, output), at(secs), sweep, false));
    }
    assert!(throttle.decide(&content_key(0, 5), at(30), sweep, false), "30 s since");
    let mut throttle = fresh();
    assert!(!throttle.decide(&content_key(0, 1), at(40), sweep, false), "unchanged");
    assert!(!throttle.pending, "nothing is owed");

    // 70 s of output that changes before every 2 s sweep, then none.
    let mut throttle = ContentThrottle::default();
    let mut writes = Vec::new();
    for secs in (0..=70).step_by(2) {
        if throttle.decide(&content_key(0, secs), at(secs), sweep, false) {
            writes.push(secs);
        }
    }
    assert_eq!(writes, [0, 30, 60], "at most three writes while it streams");
    let mut settled = Vec::new();
    for secs in (72..=110).step_by(2) {
        if throttle.decide(&content_key(0, 70), at(secs), sweep, false) {
            settled.push(secs);
        }
    }
    assert_eq!(settled, [72], "one write within two sweeps of the last output");
    assert!(!throttle.pending);
}

/// The interval is for output only. A layout change, or a change to what
/// the settings let the file hold, writes at once: the file must stay bound
/// to the layout, and a privacy change must reach the disk.
#[test]
fn layout_and_privacy_changes_bypass_the_content_throttle() {
    use super::sessions::{ContentThrottle, SaveKind};
    let t0 = std::time::Instant::now();
    let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    for kind in [SaveKind::Sweep, SaveKind::Gesture] {
        for (name, changed) in [
            ("layout", content_key_with(1, 1000, false, 2)),
            ("restore-scrollback-lines", content_key_with(0, 10, false, 2)),
            ("restore-scrollback-remote", content_key_with(0, 1000, true, 2)),
        ] {
            let mut throttle = ContentThrottle::default();
            assert!(throttle.decide(&content_key(0, 1), at(0), kind, false));
            assert!(!throttle.decide(&content_key(0, 2), at(1), kind, false));
            assert!(throttle.decide(&changed, at(2), kind, false), "{name} waited");
        }
    }
}

/// The exit save writes what the interval put off.
#[test]
fn the_exit_save_writes_pending_content() {
    use super::sessions::{ContentThrottle, SaveKind};
    let t0 = std::time::Instant::now();
    let at = |secs: u64| t0 + std::time::Duration::from_secs(secs);
    let mut throttle = ContentThrottle::default();
    assert!(throttle.decide(&content_key(0, 1), at(0), SaveKind::Sweep, false));
    assert!(!throttle.decide(&content_key(0, 2), at(5), SaveKind::Sweep, false));
    assert!(throttle.pending);
    assert!(throttle.decide(&content_key(0, 2), at(6), SaveKind::Exit, false));
    assert!(!throttle.pending);
}

/// A gesture while output streams (a copy, a font size change, a held key
/// repeating one) follows the interval too. Two of them with no output
/// between are not "quiet": only the sweep's fixed interval can say that.
#[test]
fn gestures_do_not_bypass_the_content_throttle() {
    use super::sessions::{ContentThrottle, SaveKind};
    let t0 = std::time::Instant::now();
    let at = |millis: u64| t0 + std::time::Duration::from_millis(millis);
    let mut throttle = ContentThrottle::default();
    assert!(throttle.decide(&content_key(0, 1), at(0), SaveKind::Sweep, false));
    let mut writes = 0;
    for step in 1..290u64 {
        let output = step / 3 + 2;
        writes += usize::from(throttle.decide(
            &content_key(0, output),
            at(step * 100),
            SaveKind::Gesture,
            false,
        ));
    }
    assert_eq!(writes, 0, "gestures wrote inside the interval");
    assert!(throttle.pending);
    assert!(throttle.decide(&content_key(0, 99), at(30_000), SaveKind::Gesture, false));
}

/// The sweep, its wake and the throttle together, on a fake clock: output
/// every 200 ms for 70 s, then none. The saved output is never more than one
/// interval plus one sweep behind while it streams, it settles within two
/// sweeps, and no wake is left once it has.
#[test]
fn streaming_output_is_written_every_30_s_and_settles_within_two_sweeps() {
    use super::sessions::{ContentThrottle, SaveKind, SessionSweep};
    let t0 = std::time::Instant::now();
    let at = |millis: u64| t0 + std::time::Duration::from_millis(millis);
    let millis = |instant: std::time::Instant| instant.duration_since(t0).as_millis() as u64;
    let mut sweep = SessionSweep::default();
    let mut throttle = ContentThrottle::default();
    let mut output = 0;
    let mut writes = Vec::new();
    let mut deadline = None;
    let mut next_event = Some(0u64);
    loop {
        let now = match (next_event, deadline) {
            (Some(event), Some(wake)) if wake < event => wake,
            (Some(event), _) => event,
            (None, Some(wake)) => wake,
            (None, None) => break,
        };
        assert!(now < 200_000, "the wake never settled");
        if next_event == Some(now) {
            output += 1;
            sweep.touch();
            next_event = (now < 70_000).then_some(now + 200);
        }
        if sweep.take_due(at(now), true)
            && throttle.decide(&content_key(0, output), at(now), SaveKind::Sweep, false)
        {
            writes.push((now, output));
        }
        deadline = sweep
            .deadline(at(now), throttle.pending, || true)
            .map(millis);
    }
    let streaming: Vec<_> = writes.iter().filter(|(time, _)| *time <= 70_000).collect();
    assert!(streaming.len() <= 3, "{writes:?}");
    for pair in streaming.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= 30_000, "{writes:?}");
        assert!(pair[1].0 - pair[0].0 <= 32_000, "{writes:?}");
    }
    let last = *writes.last().unwrap();
    assert_eq!(last.1, output, "the last output was never written: {writes:?}");
    assert!(last.0 <= 70_000 + 4_000, "settled only at {} ms", last.0);
    assert_eq!(writes.len(), streaming.len() + 1, "{writes:?}");
}

/// Source guards for the save: the key is read before any grid is walked,
/// the grids are walked only when it changed, and the key's panes come in
/// the snapshot's order under the snapshot's remote rule.
#[test]
fn the_content_key_is_read_before_the_grids_and_decides_the_walk() {
    let src = production_source();
    let save = src
        .split("fn save_session_as(&self, ws: &WindowState, kind: SaveKind) {")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("save_session_as body");
    let key = save.find("w.mux.content_key_panes()").unwrap();
    let decision = save
        .find("let write = self.content_throttle.borrow_mut().decide(")
        .unwrap();
    let walk = save.find("w.mux.snapshot_content(&self.cfg, &mut budget)").unwrap();
    assert!(key < decision && decision < walk, "the key must decide the walk");
    assert_eq!(save.matches("snapshot_content(").count(), 1);
    let mux = include_str!("../mux.rs");
    for body in ["fn content_key_panes(&self)", "fn snapshot_content("] {
        let body = mux
            .split(body)
            .nth(1)
            .and_then(|body| body.split("\n    }\n").next())
            .unwrap();
        assert!(body.contains("pane_saves_as_remote(pane)"), "{body}");
        assert!(body.contains("tab.root") && body.contains("leaf_ids()"), "{body}");
    }
    let exit = src
        .split("fn save_session_on_exit(&mut self) {")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(exit.contains("self.save_session_as(&ws, SaveKind::Exit);"));
}

/// A write the throttle put off asks for the sweep's wake, as a change does:
/// otherwise output that stops streaming would wait for the next event.
#[test]
fn a_put_off_content_write_asks_for_the_sweeps_wake() {
    let src = production_source();
    let app = src
        .split("fn about_to_wait(&mut self")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("about_to_wait body");
    let flat: String = app.split_whitespace().collect();
    assert!(flat.contains(
        "letpending=self.content_throttle.borrow().pending;letsession_wake=sweep.deadline(std::time::Instant::now(),pending,"
    ));
}

/// The owner lock decides which process restores and writes the default
/// session, for that process's life. Another holder makes a launch a
/// secondary that never takes over; another build's primary makes it wait
/// and take over once that build has gone (#575); a separate launch never
/// takes the lock at all.
#[test]
fn default_session_claim_truth_table() {
    use super::sessions::DefaultSessionClaim;
    let dir = kettle_test_support::private_tempdir("kettle-session-owner-");
    let lock = dir.path().join("config").join("session.owner.lock");
    let claim = |separate, another_build| {
        DefaultSessionClaim::claim(separate, another_build, true, Some(lock.as_path()))
    };

    let mut separate = claim(true, false);
    assert!(matches!(separate, DefaultSessionClaim::Separate));
    assert!(!separate.owns(false));
    let mut owner = claim(false, false);
    assert!(matches!(owner, DefaultSessionClaim::Owner { _lock: Some(_) }), "{owner:?}");
    assert!(owner.owns(false));

    let mut secondary = claim(false, false);
    assert!(matches!(secondary, DefaultSessionClaim::Secondary), "{secondary:?}");
    let mut waiting = claim(false, true);
    assert!(matches!(waiting, DefaultSessionClaim::Waiting(_)), "{waiting:?}");
    assert!(!waiting.owns(true), "the other build still runs");
    assert!(!waiting.owns(false), "the lock is still held");

    drop(owner);
    assert!(!secondary.owns(false), "a secondary never takes over");
    assert!(!waiting.owns(true));
    assert!(waiting.owns(false), "the waiting build takes over once alone");
    assert!(matches!(waiting, DefaultSessionClaim::Owner { _lock: Some(_) }));
    assert!(matches!(claim(false, false), DefaultSessionClaim::Secondary));
    drop(waiting);
    assert!(matches!(claim(false, false), DefaultSessionClaim::Owner { _lock: Some(_) }));
    // No config directory: nothing to lock, and nothing else to coordinate.
    assert!(matches!(
        DefaultSessionClaim::claim(false, false, true, None),
        DefaultSessionClaim::Owner { _lock: None }
    ));
}

/// `-e` and `-d` never restore the default session, and now never write it
/// either: a launch override used to replace the saved session with its one
/// window on its first sweep. Profiles, `--config`, `--new-process`, the
/// quick terminal and `--layout` stay out of it the same way.
#[test]
fn a_launch_override_never_writes_the_default_session() {
    use super::sessions::{SessionWriteTarget, separate_launch};
    let options = |edit: fn(&mut crate::Options)| {
        let mut options = crate::Options::default();
        edit(&mut options);
        options
    };
    for (name, launch) in [
        ("-e", options(|o| o.command = Some(vec!["htop".into()]))),
        ("-d", options(|o| o.cwd = Some("/tmp".into()))),
        ("--profile/--config/--new-process", options(|o| o.separate_process = true)),
        ("quick terminal", options(|o| o.quick_terminal = true)),
        ("--layout", options(|o| o.layout = Some("dev".into()))),
    ] {
        assert!(separate_launch(&launch), "{name}");
    }
    for launch in [options(|_| {}), options(|o| o.restore = true)] {
        assert!(!separate_launch(&launch));
    }
    // A launch that does not own the session saves nothing to it and plans
    // no restore; a named layout keeps its own file.
    let restore = kettle_config::RestoreSession::Always;
    assert_eq!(
        super::session_write_target(None, true, should_restore_session(true, restore, true), false),
        SessionWriteTarget::Skip
    );
    assert_eq!(
        super::session_write_target(Some("dev"), true, should_restore_session(true, restore, true), false),
        SessionWriteTarget::NamedLayout("dev")
    );
    let cfg = kettle_config::Config {
        restore_session: restore,
        ..Default::default()
    };
    assert!(super::startup_plan(&options(|_| {}), &cfg, false).may_restore);
    assert!(!super::startup_plan(&options(|_| {}), &cfg, true).may_restore);
    assert!(super::startup_plan(&options(|o| o.layout = Some("dev".into())), &cfg, true).may_restore);
}

/// A secondary, a launch override or a profile neither restores, writes nor
/// deletes the default session's files: `restore-scrollback-lines = 0` in a
/// profile must not erase the main process's output file.
#[test]
fn profile_and_secondary_processes_leave_the_default_session_alone() {
    use super::sessions::content_cleanup_due;
    for (lines, restore_mode) in [(0, true), (0, false), (1000, false)] {
        assert!(content_cleanup_due(true, false, lines, restore_mode));
        assert!(!content_cleanup_due(false, false, lines, restore_mode), "not this launch's");
        assert!(!content_cleanup_due(false, true, lines, restore_mode), "a layout it did not load");
    }
    assert!(!content_cleanup_due(true, false, 1000, true), "output saving is on");
    assert!(content_cleanup_due(true, true, 0, false));
    assert!(!content_cleanup_due(true, true, 1000, false), "a layout does not follow restore mode");

    // Every default-session gate asks who owns it: the save target, the
    // load, the cleanup, and the removal when restore is off.
    let src = production_source();
    let body = |start: &str| {
        src.split(start)
            .nth(1)
            .and_then(|body| body.split("\n    }\n").next())
            .unwrap_or_else(|| panic!("{start}"))
            .to_string()
    };
    assert!(body("fn autosave_target(&self)").contains("self.default_session_restoring(),"));
    let clear = body("fn clear_disabled_content(&self)");
    assert!(clear.contains("None => !self.default_session_elsewhere(),"));
    assert!(clear.contains("Some(_) => self.named_layout_writable,"));
    assert!(clear.starts_with(" {\n        if self.other_builds.running() {\n            return;"));
    assert!(clear.find("content_cleanup_due(").unwrap() < clear.find("remove_content(").unwrap());
    let load = body("fn load_startup_session(&mut self)");
    assert!(load.contains("} else if self.default_session_restoring() {"));
    let load_gate = load.split("} else if self.default_session_restoring() {").next().unwrap();
    assert!(!load_gate.contains("other_builds"), "the load must ask who owns the session");
    assert!(load.contains("self.remove_disabled_default_content();"));
    let exit = body("fn save_session_on_exit(&mut self)");
    assert!(exit.contains("self.save_session_as(&ws, SaveKind::Exit);"));
    let run = body("pub fn run_with(mut startup: crate::Options)");
    let claim = run.find("DefaultSessionClaim::claim(").unwrap();
    assert!(claim < run.find("let plan = startup_plan(&startup, &initial_cfg, session_elsewhere);").unwrap());
}

/// `kettle --restore` in a launch that cannot have the session says why, in
/// the UI language.
#[test]
fn a_refused_restore_is_explained_in_english_and_spanish() {
    use super::sessions::{DefaultSessionClaim, restore_refusal};
    use kettle_i18n::{Language, Text, Translator};
    let owner = DefaultSessionClaim::Owner { _lock: None };
    assert_eq!(restore_refusal(true, &owner, false), None);
    assert_eq!(restore_refusal(false, &DefaultSessionClaim::Secondary, true), None);
    assert_eq!(restore_refusal(false, &DefaultSessionClaim::Separate, true), None);
    let owned = restore_refusal(true, &DefaultSessionClaim::Secondary, true).unwrap();
    assert_eq!(owned, Text::NotifySessionNotRestoredOwnedBody);
    assert_eq!(restore_refusal(true, &owner, true), Some(owned), "another build runs");
    let separate = restore_refusal(true, &DefaultSessionClaim::Separate, true).unwrap();
    assert_eq!(separate, Text::NotifySessionNotRestoredSeparateBody);

    let en = Translator::new(Language::En);
    assert_eq!(en.text(Text::NotifySessionNotRestored), "Session not restored");
    assert_eq!(
        en.text(owned),
        "Another Kettle process already has the saved session open, so this window opens without it."
    );
    assert_eq!(
        en.text(separate),
        "Windows opened with --profile, --new-process, -e or -d never restore the saved session."
    );
    let es = Translator::new(Language::Es);
    assert_eq!(es.text(Text::NotifySessionNotRestored), "No se restauró la sesión");
    assert_eq!(
        es.text(owned),
        "Otro proceso de Kettle ya tiene abierta la sesión guardada, así que esta ventana se abre sin ella."
    );
    assert_eq!(
        es.text(separate),
        "Las ventanas abiertas con --profile, --new-process, -e o -d nunca restauran la sesión guardada."
    );
    // Shown from the first window's startup, before anything is restored.
    let src = production_source();
    let resumed = src
        .split("fn resumed_inner(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {")
        .nth(1)
        .unwrap();
    let notice = resumed.find("restore_refusal(explicit_restore,").unwrap();
    assert!(notice < resumed.find("self.load_startup_session()").unwrap());
    let shown: String = resumed[notice..].split_whitespace().collect();
    assert!(shown.contains(
        "fire_notify(self.ui_text.text(kettle_i18n::Text::NotifySessionNotRestored),self.ui_text.text(body),"
    ));
}

/// Q4: a pane stays remote for its saved output after `ssh` exits. Its
/// remote session's rows stay out while any of them is still in the grid,
/// and the rows after them save like any local pane's. With remote saving on,
/// everything saves, marked remote so turning it off later drops it.
#[test]
fn remote_output_stays_out_of_the_saved_content_after_ssh_exits() {
    use crate::mux::{RemoteRows, remote_rows};
    // Never remote: everything, local.
    assert_eq!(remote_rows(false, None, 0, false), RemoteRows::All { remote: false });
    // Remote session ended at document row 40, rows from 10 still there.
    assert_eq!(remote_rows(false, Some(40), 10, false), RemoteRows::From(40));
    assert_eq!(remote_rows(false, Some(40), 10, true), RemoteRows::All { remote: true });
    // Every row from before 40 has left the grid (scrollback clear, reset,
    // eviction): the pane is local again.
    assert_eq!(remote_rows(false, Some(40), 40, false), RemoteRows::All { remote: false });
    assert_eq!(remote_rows(false, Some(40), 90, true), RemoteRows::All { remote: false });
    // Remote now, with remote saving on.
    assert_eq!(remote_rows(true, None, 0, true), RemoteRows::All { remote: true });

    // Where the boundary is set: when a pane stops counting as remote, and
    // when restore replays output saved as remote; and the snapshot uses it.
    let remote = include_str!("remote.rs");
    let update = remote
        .split("fn update_window_remote_contexts(")
        .nth(1)
        .and_then(|body| body.split("\n}\n").next())
        .unwrap();
    let was = update.find("let was_remote = crate::mux::pane_saves_as_remote(pane);").unwrap();
    let probe = update.find("pane.remote_context = probe.remote.clone();").unwrap();
    let noted = update
        .find("if was_remote && !crate::mux::pane_saves_as_remote(pane) {\n            pane.note_remote_output_ended();")
        .unwrap();
    assert!(was < probe && probe < noted);
    let mux = include_str!("../mux.rs");
    let build = mux.split("fn build_node(").nth(1).unwrap();
    assert!(build.contains("replayed_remote = pane.remote;"));
    assert!(build.contains("if replayed_remote {\n                        p.note_remote_output_ended();"));
    let snapshot = mux
        .split("pub(crate) fn snapshot_content(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(snapshot.contains("match remote_rows("));
    assert!(snapshot.contains("kettle_core::restore::snapshot_from("));
    // A boundary that moves changes what is saved, so the key must see it.
    let key = mux
        .split("pub(crate) fn content_key_panes(&self)")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(key.contains("remote_end: pane.remote_output_end,"));
}

/// A launch with restore mode off claims nothing and creates no lock file:
/// a `kettle --restore` beside it owns the session instead of being told
/// another process has it open, and smoke runs with throwaway `--config`
/// files leave nothing in the config directory. Turned on later, restore
/// mode claims the lock then, under the same rules.
#[test]
fn a_launch_with_restore_off_claims_nothing() {
    use super::sessions::DefaultSessionClaim;
    let dir = kettle_test_support::private_tempdir("kettle-session-unclaimed-");
    let lock = dir.path().join("config").join("session.owner.lock");
    let mut off = DefaultSessionClaim::claim(false, false, false, Some(lock.as_path()));
    assert!(matches!(off, DefaultSessionClaim::Unclaimed(_)), "{off:?}");
    assert!(!lock.exists(), "a launch that cannot save created the lock");
    let restore = DefaultSessionClaim::claim(false, false, true, Some(lock.as_path()));
    assert!(matches!(restore, DefaultSessionClaim::Owner { _lock: Some(_) }), "{restore:?}");
    // Restore mode turned on in the first launch: it finds the session owned.
    assert!(!off.owns(false));
    assert!(matches!(off, DefaultSessionClaim::Secondary));
    drop(restore);
    let mut later = DefaultSessionClaim::claim(false, false, false, Some(lock.as_path()));
    assert!(later.owns(false), "an unclaimed launch claims once restore mode is on");
    assert!(matches!(later, DefaultSessionClaim::Owner { _lock: Some(_) }));

    // Only `--restore` asks who owns the session at startup.
    let src = production_source();
    assert!(src.contains("let elsewhere = explicit_restore && self.default_session_elsewhere();"));
    let run = src.split("pub fn run_with(mut startup: crate::Options)").nth(1).unwrap();
    let flat: String = run.split_whitespace().collect();
    assert!(flat.contains("letsession_elsewhere=another_build_running||!restoring||!default_session.owns(another_build_running);"));
}

/// With restore mode off, a leftover output file is removed only while no
/// process owns the session, under a lock held just for the removal; with no
/// file, nothing is created.
#[test]
fn a_restore_off_launch_removes_leftover_output_only_while_unowned() {
    use super::sessions::remove_unclaimed_content;
    let dir = kettle_test_support::private_tempdir("kettle-session-unclaimed-cleanup-");
    let session = dir.path().join("session.json");
    let content = crate::session_content::path(&session);
    let lock = dir.path().join("session.owner.lock");
    assert!(!remove_unclaimed_content(&lock, &session));
    assert!(!lock.exists(), "nothing to remove created a lock");
    kettle_state::atomic_replace(&content, b"old output", kettle_state::AtomicWriteOptions::PRIVATE)
        .unwrap();
    let held = kettle_state::ExclusiveFileLock::try_acquire(&lock).unwrap().unwrap();
    assert!(!remove_unclaimed_content(&lock, &session));
    assert!(content.exists(), "removed another process's output");
    drop(held);
    assert!(remove_unclaimed_content(&lock, &session));
    assert!(!content.exists());
    assert!(
        kettle_state::ExclusiveFileLock::try_acquire(&lock).unwrap().is_some(),
        "the cleanup kept the lock"
    );
    let src = production_source();
    let clear = src
        .split("fn clear_disabled_content(&self)")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    assert!(clear.contains("None if !restoring => {\n                self.remove_disabled_default_content();"));
}

/// A temporary config directory with `session.json`'s place, the default
/// config file, and two other config files.
struct ConfigSessions {
    _dir: kettle_test_support::PrivateTempDir,
    default_session: std::path::PathBuf,
    default_config: std::path::PathBuf,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
}

fn config_sessions(prefix: &str) -> ConfigSessions {
    let dir = kettle_test_support::private_tempdir(prefix);
    let config_dir = dir.path().join("kettle");
    std::fs::create_dir_all(&config_dir).unwrap();
    let sessions = ConfigSessions {
        default_session: config_dir.join("session.json"),
        default_config: config_dir.join("config"),
        a: dir.path().join("a.conf"),
        b: dir.path().join("b.conf"),
        _dir: dir,
    };
    for file in [&sessions.default_config, &sessions.a, &sessions.b] {
        std::fs::write(file, "").unwrap();
    }
    sessions
}

fn config_launch(config: &std::path::Path) -> crate::Options {
    crate::Options {
        config: Some(config.to_path_buf()),
        explicit_config: true,
        ..Default::default()
    }
}

fn titled_session(title: &str) -> crate::session::Session {
    serde_json::from_value(serde_json::json!({
        "tabs": [{"root": {"Leaf": {"cwd": null}}, "title_override": title}],
        "active": 0,
    }))
    .unwrap()
}

/// `kettle --config A` restores A's own session, not the default one.
#[test]
fn a_config_launch_restores_its_own_session() {
    use super::sessions::launch_session_path_with;
    let t = config_sessions("kettle-config-restore-");
    let launch = |options: &crate::Options| {
        launch_session_path_with(options, Some(&t.default_session), Some(&t.default_config))
            .unwrap()
    };
    let own = launch(&config_launch(&t.a));
    assert_eq!(
        own,
        crate::session::config_session_path(&t.default_session, Some(&t.default_config), &t.a)
    );
    assert_ne!(own, t.default_session);
    // Without `--config` (a default launch, or a profile's config file) the
    // launch's file is the default session.
    assert_eq!(launch(&crate::Options::default()), t.default_session);
    let profile = crate::Options {
        config: Some(t.a.clone()),
        separate_process: true,
        ..Default::default()
    };
    assert_eq!(launch(&profile), t.default_session);
    assert!(super::sessions::separate_launch(&profile));
    assert!(!super::sessions::separate_launch(&config_launch(&t.a)));

    crate::session::save_to_path(&titled_session("A"), &own).unwrap();
    crate::session::save_to_path(&titled_session("default"), &t.default_session).unwrap();
    let (restored, _) = crate::session::load_snapshot_result(&own).unwrap().unwrap();
    assert_eq!(restored.tabs[0].title_override.as_deref(), Some("A"));

    // And startup loads from the launch's file.
    let src = production_source();
    let load = src
        .split("fn load_startup_session(&mut self)")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .unwrap();
    let flat: String = load.split_whitespace().collect();
    assert!(flat.contains(
        "self.restore_source=self.session_path.clone();let(session,identity)=loaded_startup_snapshot(matchself.session_path.as_deref(){Some(path)=>crate::session::load_snapshot_result(path),"
    ));
}

/// A `--config` launch writes its own files and never reads, writes or
/// locks the default session's.
#[test]
fn a_config_launch_never_reads_or_writes_the_default_files() {
    use super::sessions::{DefaultSessionClaim, launch_session_path_with};
    let t = config_sessions("kettle-config-files-");
    let own = launch_session_path_with(
        &config_launch(&t.a),
        Some(&t.default_session),
        Some(&t.default_config),
    )
    .unwrap();
    // The writer and the claim, built as startup builds them.
    let mut writer = crate::session::SessionWriter::new(Some(own.clone()), || {}).unwrap();
    let session = titled_session("A");
    let content = crate::session_content::ContentSnapshot::new(
        &session,
        vec![vec![vec![crate::session_content::PaneContent {
            remote: false,
            lines: vec![("MARK-A".into(), false, vec![])],
        }]]],
    );
    writer.submit_job(crate::session::SaveJob {
        session: Some(session.clone()),
        layout: None,
        content: crate::session::ContentJob::Write(content),
    });
    writer.finish();
    let claim = DefaultSessionClaim::claim(
        false,
        false,
        true,
        Some(&crate::session::owner_lock_path(&own)),
    );
    assert!(matches!(claim, DefaultSessionClaim::Owner { _lock: Some(_) }), "{claim:?}");
    assert!(crate::session_content::load(&own, &session).is_some());
    for name in ["session.json", "session.content", "session.json.lock", "session.owner.lock"] {
        assert!(
            !t.default_session.with_file_name(name).exists(),
            "a --config launch touched {name}"
        );
    }

    // Every default-session path comes from the launch's one file.
    let src = production_source();
    assert_eq!(
        src.matches("Session::path()").count(),
        1,
        "only launch_session_path may name the default session file"
    );
    let run = src
        .split("pub fn run_with(mut startup: crate::Options)")
        .nth(1)
        .unwrap();
    let flat: String = run.split_whitespace().collect();
    assert!(flat.contains("letsession_path=launch_session_path(&startup);"));
    assert!(flat.contains(".map(crate::session::owner_lock_path)"));
    assert!(flat.contains("crate::session::SessionWriter::new(session_path.clone(),"));
    assert!(flat.contains("session_path,"));
}

/// Two `--config A` processes follow the default session's owner rules on
/// A's own lock, and other config files and the default session are not in
/// the way.
#[test]
fn a_same_config_secondary_leaves_it_alone() {
    use super::sessions::{DefaultSessionClaim, restore_refusal};
    let t = config_sessions("kettle-config-owner-");
    let lock = |config: Option<&std::path::Path>| {
        let session = match config {
            Some(config) => crate::session::config_session_path(
                &t.default_session,
                Some(&t.default_config),
                config,
            ),
            None => t.default_session.clone(),
        };
        crate::session::owner_lock_path(&session)
    };
    let claim =
        |lock: &std::path::Path| DefaultSessionClaim::claim(false, false, true, Some(lock));
    let first = claim(&lock(Some(&t.a)));
    assert!(matches!(first, DefaultSessionClaim::Owner { _lock: Some(_) }), "{first:?}");
    let mut second = claim(&lock(Some(&t.a)));
    assert!(matches!(second, DefaultSessionClaim::Secondary), "{second:?}");
    assert!(!second.owns(false));
    assert_eq!(
        restore_refusal(true, &second, true),
        Some(kettle_i18n::Text::NotifySessionNotRestoredOwnedBody)
    );
    for other in [lock(Some(&t.b)), lock(None)] {
        let owner = claim(&other);
        assert!(matches!(owner, DefaultSessionClaim::Owner { _lock: Some(_) }), "{other:?}");
    }
    drop(first);
    assert!(!second.owns(false), "a secondary never takes over");
}

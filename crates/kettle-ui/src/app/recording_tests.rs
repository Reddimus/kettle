/// Drift guard (dev-record completeness). Redraw and close-time
/// drains destructively consume one shared output channel, so both must fan
/// out through the same recorder + Lua dispatcher before panes disappear.
#[test]
fn recorder_output_flushed_before_reap_and_on_close() {
    let src = production_source();
    assert!(
        src.contains("fn flush_recorder_output(&mut self, ws: &mut WindowState)"),
        "the recorder-output flush helper must exist"
    );

    // Keyed on the CALL SITES, not on the prose beside them. A comment is
    // not a contract; the three functions that must flush are.
    for owner in [
        "fn redraw(&mut self, ws: &mut WindowState) {",
        "fn close_window_now(&mut self, ws: &mut WindowState, drop_panes: DropPanes) {",
        "fn about_to_wait_inner(",
    ] {
        let body = src
            .split(owner)
            .nth(1)
            .unwrap_or_else(|| panic!("{owner} must exist"))
            .split("\n    }\n")
            .next()
            .expect("function body");
        assert!(
            body.contains("self.flush_recorder_output(ws);"),
            "{owner} must flush recorder output before panes are dropped, \
                 or a just-exited shell's last lines never reach the trace"
        );
    }
    assert_eq!(
        src.matches("self.flush_recorder_output(ws);").count(),
        3,
        "exactly three production sites flush; a fourth means a new close \
             path appeared and this guard has not been told about it"
    );

    let regular_drain = src
        .split("fn drain_events(&mut self, ws: &mut WindowState)")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("regular output drain body");
    assert!(
        regular_drain.contains("self.dispatch_output_chunks(ws, output_chunks);"),
        "redraw-time output must use the shared fan-out dispatcher"
    );
    let close_drain = src
        .split("fn drain_recorder_output_once")
        .nth(1)
        .and_then(|body| body.split("fn recording_status").next())
        .expect("close-time output drain body");
    assert!(
        close_drain.contains("self.dispatch_output_chunks(ws, tail);"),
        "close-time output must use the shared fan-out dispatcher"
    );

    // Whether a window close was asked about first must not change what
    // gets recorded. All three window-close paths share one teardown, so
    // the recording tail cannot depend on the gesture: the titlebar
    // ✕ / Alt+F4, the `close_window` action, and the confirmed prompt.
    // Anchored on the DEFINITION, not the full signature, so an added
    // parameter cannot send the split to the wrong region. The needle
    // is assembled at runtime so this test's own mentions cannot stand
    // in for it.
    let teardown_fn = ["    fn close_window", "_now(&mut self"].concat();
    let teardown = src
        .split(teardown_fn.as_str())
        .nth(1)
        .and_then(|body| body.split("\n    /// ").next())
        .expect("close_window_now body");
    for expected in [
        "self.flush_recorder_output(ws);",
        "rec.begin_finish();",
        "self.save_session(ws);",
        "self.request_window_close(ws.seq);",
    ] {
        assert!(
            teardown.contains(expected),
            "the shared window teardown must still do {expected:?}"
        );
    }
    // Assembled at runtime so the count does not include this test's own
    // source.
    let call = ["self.close_window", "_now(ws, "].concat();
    assert_eq!(
        src.matches(call.as_str()).count(),
        3,
        "exactly three window-close paths (CloseRequested, Action::CloseWindow, \
             ConfirmAction::CloseWindow) must share the teardown — a fourth that \
             hand-rolls it would silently drop the recording tail"
    );
    // ORDER, not just presence. The flush walks `ws.mux.panes` and reads
    // each pane's output sidechannel, so clearing the mux first hands it an
    // empty map and loses the whole tail. The teardown clears the mux
    // itself, after the flush.
    let flush = teardown
        .find("self.flush_recorder_output(ws);")
        .expect("teardown flush");
    // Needle assembled at runtime so this test's own two mentions of it do
    // not count as production call sites.
    let clear_call = ["ws.mux.close", "_window();"].concat();
    let clear = teardown
        .find(clear_call.as_str())
        .expect("the teardown must own the pane clearing so it cannot precede the flush");
    assert!(
        flush < clear,
        "the recorder flush must run BEFORE the panes it reads are dropped"
    );
    assert_eq!(
        src.matches(clear_call.as_str()).count(),
        1,
        "only the shared teardown may clear the mux — a call site that does \
             it first empties the panes the flush reads from"
    );
}

/// Every modal branch of the keyboard path records the key that closed it,
/// so the key's repeats never reach the terminal it uncovered, and the
/// modals with activation keys gate their repeats.
#[test]
fn every_modal_branch_records_its_closing_key() {
    let src = production_source();
    assert_eq!(
        src.matches("remember_closing_key(ws, key_modal, event.physical_key);")
            .count(),
        13,
        "confirm, context menu, vi, hint, palette, theme picker, settings \
             text, settings, layout picker, ssh, title edit, search and a \
             preview lane holding the keyboard"
    );
    for gate in [
        "ModalRepeat::ConfirmDialog,",
        "ModalRepeat::ContextMenu {",
        "ModalRepeat::ViMode,",
        "ModalRepeat::Hint,",
        "ModalRepeat::Palette,",
        "ModalRepeat::Settings { steps },",
    ] {
        assert!(src.contains(gate), "{gate} must gate its repeats");
    }
    assert!(src.contains("if drop_closing_key_repeat(\n                    &mut ws.closing_keys,"));
    // The Escape that cancels a pane, tab or torn-window drag is consumed
    // the same way.
    assert_eq!(
        src.matches("ws.closing_keys.insert(event.physical_key);")
            .count(),
        3,
        "pane drag, tab drag and torn-window drag cancels"
    );
}

/// The adaptive edge case deliberately bypasses `handle_action` and falls
/// through to normal terminal input. Pin the production wiring as well as
/// the two pure helpers: once a repeated press reaches the PTY, the shared
/// ownership ledger must be updated before its encoded bytes are queued so
/// the eventual release cannot be swallowed by later geometry changes.
#[test]
fn adaptive_alt_focus_fallthrough_records_terminal_ownership_before_writing() {
    let src = production_source();
    let routing = src
        .split("&& !self.chord_falls_through(ws, trigger, &act)")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::RedrawRequested").next())
        .expect("keyboard routing block");
    let fallthrough = routing
        .find("self.handle_action(ws, act, event_loop);")
        .expect("a chord that stays Kettle's must still run its action");
    let terminal_ownership = routing
        .find("track_terminal_key_press(")
        .expect("fall-through input must record terminal key ownership");
    let terminal_write = routing
        .find("self.write_terminal_key_event(ws, &event, true)")
        .expect("fall-through input must reach the terminal");
    assert!(
        fallthrough < terminal_ownership && terminal_ownership < terminal_write,
        "the shared routing must decide the UI branch first, then record PTY \
             ownership before writing the fall-through press"
    );
    assert_eq!(
        routing.matches("track_terminal_key_press(").count(),
        1,
        "the keyboard fall-through path must have one shared ownership update"
    );
}

#[test]
fn recording_window_title_suffix_is_ascii_safe() {
    use super::{
        RECORDING_ERROR_TITLE_SUFFIX, RECORDING_LIMIT_TITLE_SUFFIX,
        RECORDING_OVERLOAD_TITLE_SUFFIX, RECORDING_RAW_TITLE_SUFFIX, RECORDING_TITLE_SUFFIX,
        activation_recording_available, effective_record_status, recording_window_title,
    };
    use crate::dev_record::RecordStatus;

    assert_eq!(effective_record_status(None, false), None);
    assert_eq!(
        effective_record_status(None, true),
        Some(RecordStatus::IoError)
    );
    assert_eq!(
        effective_record_status(Some(RecordStatus::LimitReached), true),
        Some(RecordStatus::LimitReached)
    );
    assert!(activation_recording_available(false, None));
    assert!(activation_recording_available(
        true,
        Some(RecordStatus::Recording)
    ));
    assert!(!activation_recording_available(
        true,
        Some(RecordStatus::LimitReached)
    ));
    assert!(!activation_recording_available(
        true,
        Some(RecordStatus::Overloaded)
    ));
    assert!(!activation_recording_available(
        true,
        Some(RecordStatus::IoError)
    ));
    assert_eq!(
        recording_window_title("kettle".to_string(), None, false),
        "kettle"
    );
    assert_eq!(
        recording_window_title("kettle".to_string(), Some(RecordStatus::Recording), false),
        "kettle [REC]"
    );
    // Raw keystroke capture gets its own visible suffix.
    assert_eq!(
        recording_window_title("kettle".to_string(), Some(RecordStatus::Recording), true),
        "kettle [REC RAW]"
    );
    assert_eq!(
        recording_window_title("kettle".to_string(), Some(RecordStatus::LimitReached), true),
        "kettle [REC LIMIT]"
    );
    assert_eq!(
        recording_window_title("kettle".to_string(), Some(RecordStatus::IoError), false),
        "kettle [REC ERROR]"
    );
    assert_eq!(
        recording_window_title("kettle".to_string(), Some(RecordStatus::Overloaded), false),
        "kettle [REC INCOMPLETE]"
    );
    assert!(
        [
            RECORDING_TITLE_SUFFIX,
            RECORDING_RAW_TITLE_SUFFIX,
            RECORDING_LIMIT_TITLE_SUFFIX,
            RECORDING_OVERLOAD_TITLE_SUFFIX,
            RECORDING_ERROR_TITLE_SUFFIX
        ]
        .iter()
        .all(|suffix| suffix.is_ascii())
    );
    assert!(
        !RECORDING_TITLE_SUFFIX.contains('●'),
        "native title-bar text must avoid host-font-dependent symbol glyphs"
    );
}

/// Drift guard. `session_log_path` is the helper
/// behind `Action::ToggleSessionLog`. Verify:
///   - lives under `<cache>/kettle/logs/`
///   - filename includes both the unix-secs (for sort) and pid
///     plus a sequence for each logging session
///   - falls back to a relative dir when no cache dir resolves
#[test]
fn session_log_path_under_cache_kettle_logs() {
    use super::session_log_path;
    let cache = std::path::Path::new("/home/u/.cache");
    let p = session_log_path(1_716_422_400, 9876, Some(cache));
    assert_eq!(p.parent(), Some(cache.join("kettle/logs").as_path()));
    let filename = p.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("kettle-1716422400-9876-"));
    assert!(filename.ends_with(".log"));
    // No cache dir resolved → relative fallback (still gets the
    // log written somewhere instead of erroring out).
    let p = session_log_path(1_716_422_400, 9876, None);
    assert_eq!(p.parent(), Some(std::path::Path::new("kettle-logs")));
    assert_eq!(p.extension().and_then(|s| s.to_str()), Some("log"));
}

#[test]
fn session_log_paths_are_unique_within_one_second() {
    let paths: std::collections::HashSet<_> = (0..128)
        .map(|_| super::session_log_path(1_716_422_400, 9876, None))
        .collect();
    assert_eq!(paths.len(), 128);
}

#[test]
fn default_ui_entry_points_honor_recording_config() {
    use kettle_core::record::RecordingTarget;

    let root = crate::test_tempdir();
    let config = root.path().join("config");
    let recordings = root.path().join("casts");
    // Configs are written privately, as Kettle writes them: the trusted
    // reader refuses a group-writable file, which a umask of 002 makes.
    let write = |text: &str| {
        kettle_state::atomic_replace(
            &config,
            text.as_bytes(),
            kettle_state::AtomicWriteOptions::PRIVATE,
        )
        .unwrap();
    };
    write(&format!(
        "record = on\nrecord-dir = {}\nrecord-raw-input = on\n",
        recordings.display()
    ));
    let startup = App::default_startup_options(Some(config.clone()));
    let crate::RecordingPolicy::Enabled { target, raw_input } = startup.recording else {
        panic!("default UI launch must resolve record = on without a supplied policy");
    };
    assert_eq!(target, RecordingTarget::Directory(recordings));
    assert!(raw_input);
    assert_eq!(startup.config, Some(config.clone()));

    write("record = off\nrecord-raw-input = on\n");
    assert!(matches!(
        App::default_startup_options(Some(config)).recording,
        crate::RecordingPolicy::Disabled
    ));

    // Exercise the headless startup seam above and pin both public callers to it.
    let library = kettle_test_support::production_source(include_str!("../lib.rs"));
    let run = library.split("pub fn run()").nth(1).unwrap();
    assert!(run.split("\n}").next().unwrap().contains("App::run()"));
    let app = production_source();
    let run = app.split("pub fn run()").nth(1).unwrap();
    assert!(
        run.split("\n    }")
            .next()
            .unwrap()
            .contains("Self::default_startup_options(Config::default_path())")
    );
}

#[test]
fn default_ui_startup_uses_default_recording_directory() {
    let root = crate::test_tempdir();
    let config = root.path().join("config");
    kettle_state::atomic_replace(
        &config,
        b"record = on\n",
        kettle_state::AtomicWriteOptions::PRIVATE,
    )
    .unwrap();
    let startup = App::default_startup_options(Some(config));
    let expected = kettle_config::Config::default_path()
        .and_then(|path| path.parent().map(|parent| parent.join("recordings")));
    match (startup.recording, expected) {
        (crate::RecordingPolicy::Enabled { target, raw_input }, Some(directory)) => {
            assert_eq!(
                target,
                kettle_core::record::RecordingTarget::Directory(directory)
            );
            assert!(!raw_input);
        }
        (crate::RecordingPolicy::Disabled, None) => {}
        unexpected => panic!("unexpected default recording policy: {unexpected:?}"),
    }
}

#[test]
fn default_ui_startup_without_config_does_not_record() {
    let root = crate::test_tempdir();
    for config in [None, Some(root.path().join("missing"))] {
        assert!(matches!(
            App::default_startup_options(config).recording,
            crate::RecordingPolicy::Disabled
        ));
    }
}

#[test]
fn launch_recorder_precedes_restored_sibling_windows() {
    let src = production_source();
    let startup = src
        .split("fn resumed_inner(")
        .nth(1)
        .unwrap()
        .split("\nimpl ")
        .next()
        .unwrap();
    let recorder = startup.find("self.recorder = Some(rec);").unwrap();
    let sibling = startup.find(".open_window(").unwrap();
    assert!(
        recorder < sibling,
        "attach recording before a restored sibling can drain output"
    );
    let windows = include_str!("windowing.rs");
    let setup = windows
        .split("mux.lua_output_subscribed = self.lua_engine.is_some();")
        .nth(1)
        .unwrap()
        .split("let mut ws = WindowState::new")
        .next()
        .unwrap();
    assert!(
        setup.contains("self.recorder.is_some()")
            && setup.contains("crate::RecordingPolicy::Enabled { .. }")
    );
}

#[test]
fn review_recording_header_follows_restore_and_precedes_siblings() {
    let src = production_source();
    let startup = finding_body(&src, "resumed_inner");
    let restore = startup.find(".restore_geometry(").unwrap();
    let resize = startup[restore..].find("self.resize_all(ws);").unwrap() + restore;
    let header = startup.find("Recorder::start_target_async(").unwrap();
    let recorder = startup.find("self.recorder = Some(rec);").unwrap();
    let sibling = startup.find(".open_window(").unwrap();
    assert!(
        restore < resize && resize < header && header < recorder && recorder < sibling,
        "record the actual restored grid before any sibling can drain output"
    );
}


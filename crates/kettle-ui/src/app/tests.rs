const EN_TEXT: kettle_i18n::Translator = kettle_i18n::Translator::new(kettle_i18n::Language::En);

/// Confirmation buttons are hit-tested with the metrics the renderer paints
/// them with: the chrome font's advance and line height, not the terminal
/// cell. With `cell-width = 0.5` the two differ by half, and a click on empty
/// space could confirm a destructive action.
#[test]
fn confirmation_hits_use_the_painted_chrome_metrics() {
    let source = include_str!("modals.rs");
    for function in [
        "pub(super) fn confirm_button_hovered(",
        "pub(super) fn confirm_button_click_result(",
    ] {
        let body = source
            .split_once(function)
            .unwrap()
            .1
            .split("confirm_dialog_button_hit(")
            .next()
            .unwrap();
        assert!(
            body.contains(concat!("self.overlay_text_", "cell_width(ws)"))
                && body.contains(concat!("self.overlay_text_", "line_height(ws)"))
                && !body.contains("cell_px("),
            "{function}"
        );
    }
}

#[test]
fn link_hits_require_cursor_containment() {
    let rect = (400.0, 40.0, 400.0, 560.0);
    assert!(!super::rect_contains(rect, 200.0, 80.0));
    assert!(!super::rect_contains(rect, 800.0, 80.0));
    assert!(super::rect_contains(rect, 400.0, 80.0));

    let src = super::production_source();
    let hit = src
        .split("fn link_at_cursor")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("link hit test");
    let guard = hit
        .find("if !rect_contains(rect, ws.cursor.x as f32, ws.cursor.y as f32)")
        .expect("link hits reject pointers outside the focused terminal");
    let cell = hit
        .find("self.cursor_cell_in_rect(ws, rect)")
        .expect("cell coordinates are resolved after containment");
    assert!(guard < cell);
}

#[test]
fn multi_click_count_is_scoped_to_a_pane() {
    let (mux, target, sibling) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    assert_eq!(App::click_count(&mut ws, 5, 7), 1);
    assert_eq!(App::click_count(&mut ws, 5, 7), 2);
    assert!(ws.mux.focus_pane(sibling));
    assert_eq!(App::click_count(&mut ws, 5, 7), 1);
    assert_eq!(App::click_count(&mut ws, 5, 7), 2);
    assert!(ws.mux.focus_pane(target));
    assert_eq!(App::click_count(&mut ws, 5, 7), 1);
}

#[cfg(unix)]
#[test]
fn foreign_path_paste_dismisses_only_an_accepting_receipt_pane() {
    for (member, read_only) in [(true, false), (true, true), (false, false)] {
        let cfg = kettle_config::Config {
            shell_integration: false,
            login_shell: false,
            ..kettle_config::Config::default()
        };
        let mut mux = Mux::new();
        for _ in 0..2 {
            mux.new_tab_with_geometry(
                &cfg,
                kettle_core::PtyGeometry::from_cell_size(80, 24, 8, 16),
                std::sync::Arc::new(|| {}),
                &["/bin/cat".to_owned()],
                None,
            )
            .expect("PTY fixture");
        }
        let receipt_pane = mux.active_focus().unwrap();
        for (id, pane) in &mut mux.panes {
            pane.group_name = Some(
                if *id == receipt_pane && !member {
                    "other"
                } else {
                    "g"
                }
                .into(),
            );
        }
        mux.panes.get_mut(&receipt_pane).unwrap().read_only = read_only;
        let mut ws = WindowState::new(2, mux);
        let now = std::time::Instant::now();
        ws.media_paste_receipt = Some(crate::window_state::MediaPasteReceiptState::new_image(
            receipt_pane,
            "old.png".into(),
            crate::paste_image::PastedImagePreview {
                image: kettle_core::ImageData::solid(1, 1, [0, 0, 0, 255]).unwrap(),
                original_width: 1,
                original_height: 1,
            },
            false,
            true,
            now,
        ));
        ws.pending_video_paste_receipt = Some(crate::window_state::PendingVideoPasteReceipt {
            pane_id: receipt_pane,
            generation: 1,
            request: crate::video_preview::VideoPasteRequest::from_user_paths(
                &[std::env::temp_dir().join("old.mp4")],
                crate::video_preview::VideoPasteSource::Drop,
            )
            .unwrap(),
            remote: false,
            prefer_top: true,
            created_at: now,
            previous_receipt: None,
        });
        let foreign = App::deliver_foreign_path_paste(
            &mut ws,
            &crate::mux::BroadcastScope::Group("g".into()),
            &["new.txt".into()],
            false,
        );
        ws.mux.kill_children();
        let accepted = member && !read_only;
        assert!(foreign.accepted, "another group member always accepts");
        assert_eq!(foreign.receipt_accepted, accepted);
        assert_eq!(ws.media_paste_receipt.is_none(), accepted);
        assert_eq!(ws.pending_video_paste_receipt.is_none(), accepted);
    }
}

#[cfg(unix)]
#[test]
fn image_paste_holds_enter_and_keeps_its_original_recipient() {
    use super::paste::{defer_authored_input, pop_ready_paste};

    let cfg = kettle_config::Config {
        shell_integration: false,
        login_shell: false,
        ..kettle_config::Config::default()
    };
    let mut mux = Mux::new();
    for _ in 0..2 {
        mux.new_tab_with_geometry(
            &cfg,
            kettle_core::PtyGeometry::from_cell_size(80, 24, 8, 16),
            std::sync::Arc::new(|| {}),
            &["/bin/cat".to_owned()],
            None,
        )
        .expect("PTY fixture");
    }
    let focused = mux.active_focus().unwrap();
    let target = *mux.panes.keys().find(|id| **id != focused).unwrap();
    let mut ws = WindowState::new(1, mux);
    let queued = |content| super::QueuedPaste {
        window: 1,
        target: Some(target),
        receipt_pane: Some(target),
        broadcast: crate::mux::BroadcastScope::Off,
        recipients: Vec::new(),
        content,
    };
    let mut queue = std::collections::VecDeque::from([queued(super::QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    let before = ws.mux.panes[&target].input_generation();
    let focused_before = ws.mux.panes[&focused].input_generation();
    assert_eq!(
        defer_authored_input(
            &mut queue,
            queued(super::QueuedPasteContent::Input {
                targets: vec![(target, b"\r".to_vec())],
                prepare_input: true,
            }),
        ),
        Some(PaneInputResult::Queued)
    );
    assert!(pop_ready_paste(&mut queue).is_none());
    assert_eq!(ws.mux.panes[&target].input_generation(), before);
    if let super::QueuedPasteContent::Image { path, .. } = &mut queue.front_mut().unwrap().content {
        *path = Some(Ok("/tmp/image.png".into()));
    }
    let image = pop_ready_paste(&mut queue).unwrap();
    assert!(matches!(
        image.content,
        super::QueuedPasteContent::Image { .. }
    ));
    assert!(
        super::paste_paths_into_target(
            &mut ws.mux,
            image.target,
            &["/tmp/image.png".into()],
            false,
        )
        .unwrap()
        .accepted
    );
    let after_path = ws.mux.panes[&target].input_generation();
    assert_ne!(after_path, before);
    let input = pop_ready_paste(&mut queue).unwrap();
    let super::QueuedPasteContent::Input {
        targets,
        prepare_input,
    } = input.content
    else {
        panic!("Enter must follow the image path");
    };
    let result = App::deliver_deferred_input(&mut ws, &targets, prepare_input, false);
    let after_enter = ws.mux.panes[&target].input_generation();
    let focused_after = ws.mux.panes[&focused].input_generation();
    ws.mux.kill_children();
    assert_eq!(result, PaneInputResult::Queued);
    assert_ne!(after_enter, after_path);
    assert_eq!(
        focused_before, focused_after,
        "focus changes must not retarget Enter"
    );
}

/// A key held while a paste ahead of it is still encoding queues one input
/// per repeat. Each joins the entry before it, so the queue's 64 entries do
/// not drop typing; the bytes stay in order.
#[test]
fn held_keys_behind_an_encoding_paste_join_one_queue_entry() {
    let queued = |content| super::QueuedPaste {
        window: 1,
        target: Some(1),
        receipt_pane: Some(1),
        broadcast: crate::mux::BroadcastScope::Off,
        recipients: Vec::new(),
        content,
    };
    let mut queue = std::collections::VecDeque::from([queued(super::QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    for byte in (0..200u32).map(|n| b'a' + (n % 26) as u8) {
        assert_eq!(
            super::paste::defer_authored_input(
                &mut queue,
                queued(super::QueuedPasteContent::Input {
                    targets: vec![(1, vec![byte])],
                    prepare_input: true,
                }),
            ),
            Some(PaneInputResult::Queued)
        );
    }
    assert_eq!(queue.len(), 2);
    let super::QueuedPasteContent::Input { targets, .. } = &queue[1].content else {
        panic!("the typing follows the image");
    };
    let expected: Vec<u8> = (0..200u32).map(|n| b'a' + (n % 26) as u8).collect();
    assert_eq!(targets, &[(1, expected)]);
}

#[test]
fn deferred_authored_input_obeys_the_existing_queue_byte_limit() {
    let queued = |content| super::QueuedPaste {
        window: 1,
        target: Some(1),
        receipt_pane: Some(1),
        broadcast: crate::mux::BroadcastScope::Off,
        recipients: Vec::new(),
        content,
    };
    let mut queue = std::collections::VecDeque::from([queued(super::QueuedPasteContent::Image {
        id: 0,
        fallback: String::new(),
        path: None,
    })]);
    assert_eq!(
        super::paste::defer_authored_input(
            &mut queue,
            queued(super::QueuedPasteContent::Input {
                targets: vec![(1, vec![b'x'; super::QUEUED_PASTE_BYTES_MAX + 1])],
                prepare_input: true,
            }),
        ),
        Some(PaneInputResult::Oversize)
    );
    assert_eq!(queue.len(), 1);
}

#[cfg(unix)]
#[test]
fn reload_metadata_errors_are_not_missing_configs() {
    use std::os::unix::fs::symlink;

    let dir = kettle_test_support::private_tempdir("kettle-reload-metadata-");
    let path = dir.join("config");
    assert!(
        super::config_reload::read_config_for_reload(
            &path,
            kettle_config::ConfigTrust::ExplicitPath,
        )
        .unwrap()
        .is_none()
    );
    symlink("config", &path).unwrap();
    assert!(!path.exists());
    assert!(path.try_exists().is_err());
    assert!(
        super::config_reload::read_config_for_reload(
            &path,
            kettle_config::ConfigTrust::ExplicitPath,
        )
        .is_err()
    );
}

/// Every path paste is checked before it is formatted or delivered, so a
/// file name the target shell cannot take exactly authors no input.
#[test]
fn path_pastes_are_checked_before_they_are_formatted() {
    let source = include_str!("paste.rs");
    let body = source
        .split_once("pub(super) fn paste_paths_to(")
        .unwrap()
        .1
        .split("\n    }\n")
        .next()
        .unwrap();
    let check = body
        .find("if !self.paths_type_exactly(ws, &argv, &paths)")
        .unwrap();
    assert!(check < body.find("format_paths_for_paste").unwrap());
    assert!(check < body.find("deliver_path_paste").unwrap());
}

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

#[test]
fn startup_plan_truth_table() {
    use kettle_config::WindowState::{Fullscreen, Hidden, Maximise, Normal};
    for command in [false, true] {
        for cwd in [false, true] {
            for restore_bits in 0u8..32 {
                if !cfg!(unix) && restore_bits & 4 != 0 {
                    continue;
                }
                for state in [Normal, Hidden, Maximise, Fullscreen] {
                    for position in 0u8..4 {
                        let options = crate::Options {
                            command: command.then(|| vec!["/bin/true".into()]),
                            cwd: cwd.then(|| std::path::PathBuf::from("/tmp")),
                            layout: (restore_bits & 1 != 0).then(|| "test".into()),
                            tab_handoff: (restore_bits & 2 != 0).then(|| "/tmp/handoff".into()),
                            tab_handoff_fd: {
                                #[cfg(unix)]
                                {
                                    use std::os::fd::IntoRawFd;
                                    (restore_bits & 4 != 0).then(|| {
                                        let (socket, _peer) =
                                            std::os::unix::net::UnixStream::pair().unwrap();
                                        // SAFETY: into_raw_fd gave up the only other owner.
                                        unsafe { crate::TabHandoff::adopt(socket.into_raw_fd()) }
                                            .unwrap()
                                    })
                                }
                                #[cfg(not(unix))]
                                {
                                    None
                                }
                            },
                            restore: restore_bits & 8 != 0,
                            ..Default::default()
                        };
                        let cfg = kettle_config::Config {
                            restore_session: restore_bits & 16 != 0,
                            window_state: state,
                            window_position_x: (position & 1 != 0).then_some(10),
                            window_position_y: (position & 2 != 0).then_some(10),
                            ..Default::default()
                        };
                        let override_launch = command || cwd;
                        let restore = !override_launch && restore_bits != 0;
                        assert_eq!(
                            super::startup_plan(&options, &cfg, false),
                            super::StartupPlan {
                                launch_override: override_launch,
                                may_restore: restore,
                                pre_launch: !restore
                                    && matches!(state, Normal | Hidden)
                                    && position == 0,
                            },
                            "command={command} cwd={cwd} restore={restore_bits} state={state:?} position={position}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn display_arithmetic_matches_winit_at_one_two_and_three_x() {
    use super::{StartupMonitor, fits_monitor};
    for scale in [1.0, 2.0, 3.0] {
        for (width, height) in [(1920usize, 1080usize), (1371, 773)] {
            let monitor = StartupMonitor::from_display_points(width, height, scale);
            assert_eq!(
                monitor,
                StartupMonitor::from_physical(
                    (width as f64 * scale).round() as u32,
                    (height as f64 * scale).round() as u32,
                    scale
                )
            );
            assert_eq!(monitor.logical, (width as f64, height as f64));
            let limit = (
                ((width as f64 * super::STARTUP_MONITOR_WIDTH_FRACTION).floor() * scale) as u32,
                ((height as f64 * super::STARTUP_MONITOR_HEIGHT_FRACTION).floor() * scale) as u32,
            );
            assert!(fits_monitor(limit, Some(monitor)));
            assert!(fits_monitor((limit.0 + 1, limit.1 + 1), Some(monitor)));
            assert!(!fits_monitor((limit.0 + 2, limit.1), Some(monitor)));
            assert!(!fits_monitor((limit.0, limit.1 + 2), Some(monitor)));
            assert!(!fits_monitor(limit, None));
        }
    }
}

#[test]
fn pre_launch_spawn_sits_between_app_build_and_run_app() {
    let src = super::production_source();
    let run = src
        .split_once("pub fn run_with(")
        .unwrap()
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    let order = [
        run.find("EventLoop::<UserEvent>::with_user_event().build()")
            .unwrap(),
        run.find("let plan = startup_plan(&startup, &initial_cfg, another_build_running);")
            .unwrap(),
        run.find("let mut app = App {").unwrap(),
        run.find("startup_trace::Phase::AppBuilt").unwrap(),
        run.find("app.start_first_pane_before_launch()").unwrap(),
        run.find("event_loop.run_app(&mut app)").unwrap(),
        run.find("app.pasted_images.cleanup();").unwrap(),
        // Copies handed to the image viewer, the document viewer and the
        // video player go on exit too.
        run.find("app.viewer_copies").unwrap(),
        run.find("app.document_copies.close();").unwrap(),
        run.find("app.video_copies.close();\n        if let Err(error) = &result {")
            .unwrap(),
        run.find("app.runtime_tracker.stop();").unwrap(),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]));
    assert!(run.contains("Err(anyhow::anyhow!(error))"));
    assert!(run.contains("launch_override: plan.launch_override,"));
    let early = src
        .split_once("fn start_first_pane_before_launch(")
        .unwrap()
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    assert!(early.contains("#[cfg(not(target_os = \"macos\"))]"));
    assert!(early.contains("if !self.pre_launch_eligible"));
    assert!(early.contains("self.startup_surface(&ws, Some(monitor))"));
    assert!(early.contains("fits_monitor(size, Some(monitor))"));
    assert!(early.contains("self.windows.insert(1, ws);"));
    let flat: String = early.split_whitespace().collect();
    assert!(
        flat.find("self.prepare_startup_fonts(&mutws,monitor.scaleasf32)")
            .unwrap()
            < flat
                .find("self.spawn_first_tab(&mutws,self.launch_override)")
                .unwrap()
    );
}

#[test]
fn resumed_preserves_the_launch_override_and_pre_launch_pane() {
    let src = super::production_source();
    let body = src
        .split_once("fn resumed_inner(")
        .unwrap()
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    assert!(body.contains("let has_launch_override = self.launch_override;"));
    assert!(!body.contains("self.startup.command.is_some()"));
    assert!(!body.contains("self.startup.cwd.is_some()"));
    assert!(body.contains("ws.pre_launch.is_some() || has_launch_override"));
    assert!(body.contains("self.named_layout_writable = !has_launch_override;"));
    assert!(body.contains("crate::startup_trace::note_monitor_match(matched);"));
    let flat: String = body.split_whitespace().collect();
    assert!(
        flat.contains("ifws.pre_launch.is_none(){self.prepare_startup_fonts(ws,startup_scale);}")
    );
    assert!(flat.contains("ifws.pre_launch.is_none()&&spawned_early&&letErr(e)=self.spawn_first_tab(ws,has_launch_override)"));
    assert!(flat.contains("letrestored=ifspawned_early{true}"));
    for message in ["failed to create window: {e}", "renderer init failed: {e}"] {
        let failure = body
            .split_once(message)
            .unwrap()
            .1
            .split_once("return;")
            .unwrap()
            .0;
        assert!(
            failure.find("ws.mux.kill_children();").unwrap()
                < failure.find("event_loop.exit();").unwrap()
        );
    }
    let wait = src
        .split_once("fn about_to_wait(&mut self,")
        .unwrap()
        .1
        .split_once("// C1-DISPATCH-END")
        .unwrap()
        .0;
    assert!(
        wait.find("pane.term.poll_startup_geometry()").unwrap()
            < wait.find("self.about_to_wait_inner(").unwrap()
    );
    assert!(wait.contains("current.min(deadline)"));
}

use super::{
    ACCESSIBILITY_COMPLETION_CONTAINER_ID, ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID,
    ACCESSIBILITY_MEDIA_RECEIPT_ID, AUTOMATION_RETRY_MIN, App, AutomationRetry, ContextMenuItem,
    GroupBulkScope, MAX_REMOTE_COMMANDS_PER_BATCH, Osc52ClipboardChannel, PaneDrag,
    ParsedRemoteCommandBatch, PendingLuaCommand, PendingLuaCommands, PendingRemoteCommand,
    RemoteBatchClaim, SplitDrag, TitleEditScope, accessibility_completion_row_id,
    apply_bs_del_binding, apply_mux_title_edit, apply_output_generation_outcome,
    apply_remote_title_transition, argv_is_nonlocal_client, assign_mnemonics,
    background_is_dark_enough_to_reveal_early, begin_title_edit, broadcast_scope_for_default,
    cached_pane_cursor_blinking, claim_remote_command_file, clear_stale_title_edit,
    command_picker_list, context_menu_item_columns, context_menu_max_scroll_offset,
    context_menu_scroll_for_highlight, context_menu_snapshot_reuse_safe,
    context_menu_surface_can_fit_row, count_rows_fitting, ctl_input_error, filter_disabled,
    find_menu_row_y, fit_context_menu_row, group_scope_is_globally_stale, input_rejection_message,
    key_is_modified_enter, layout_picker_list, local_paste_within_limit, macos_effective_modifiers,
    macos_pty_modifiers, modal_swallows_pointer, option_is_meta_for, osc52_clipboard_channel,
    output_generation_advanced, output_wakeup_needs_paint, pane_cursor_blinking_with,
    pane_snapshot_keys_match, parse_remote_command_batch, picker_context_menu,
    picker_overlay_context_menu, picker_scroll_offset, production_source, rank_layouts,
    rank_ssh_hosts, sanitize_native_window_title, sanitize_title, selected_ssh_target,
    selection_kind, session_sweep_due, should_notify_input_rejection, should_poll_remote_window,
    should_restore_session, should_reveal_after_renderer_init,
    should_reveal_before_first_surface_frame, ssh_picker_list, stage_applied_remote_probe,
    stage_output_generations_for_frame, stage_remote_targets, stamp_accepted_input,
    startup_inner_size_px, typeahead_match,
};
use crate::mux::{BroadcastScope, Dir, Mux, PaneInputDelivery, PaneInputResult, PaneTitleOrigin};
use crate::window_state::WindowState;
use kettle_config::Action;
use kettle_core::{ClipboardType, SelectionType, event::OutputWakeGate};
use kettle_render::{ContextMenuRow, FrameOutcome, PaneSnapshot};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

fn title_test_config() -> kettle_config::Config {
    kettle_config::Config {
        shell_integration: false,
        login_shell: false,
        ..kettle_config::Config::default()
    }
}

fn title_test_argv() -> Vec<String> {
    #[cfg(unix)]
    {
        vec!["/bin/cat".to_string()]
    }
    #[cfg(windows)]
    {
        vec!["cmd.exe".to_string(), "/d".to_string(), "/q".to_string()]
    }
}

fn title_test_split_mux() -> (Mux, u64, u64) {
    let cfg = title_test_config();
    let argv = title_test_argv();
    let waker: kettle_core::Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    mux.new_tab_with(&cfg, 80, 24, 8, 16, waker.clone(), &argv, None)
        .expect("spawn title target pane");
    let target = mux.active_focus().expect("target pane id");
    mux.split_with(Dir::Horizontal, &cfg, 80, 24, 8, 16, waker, argv, None)
        .expect("spawn title sibling pane");
    let sibling = mux.active_focus().expect("sibling pane id");
    assert!(mux.focus_pane(target));
    (mux, target, sibling)
}

fn title_test_tab_mux(count: usize) -> (Mux, Vec<u64>) {
    let cfg = title_test_config();
    let argv = title_test_argv();
    let waker: kettle_core::Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    let mut panes = Vec::new();
    for _ in 0..count {
        mux.new_tab_with(&cfg, 80, 24, 8, 16, waker.clone(), &argv, None)
            .expect("spawn title test tab");
        panes.push(mux.active_focus().expect("tab pane id"));
    }
    (mux, panes)
}

#[cfg(unix)]
fn wait_for_pane_screen(mux: &Mux, pane: u64, done: impl Fn(&str, usize) -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if let Some(screen) = mux.panes[&pane].term.screen_text(0)
            && done(&screen.text, screen.history_size)
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

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
fn pane_title_edit_never_follows_focus_to_a_sibling() {
    let (mut mux, target, sibling) = title_test_split_mux();
    mux.panes.get_mut(&target).unwrap().title = "pane-a-before".into();
    mux.panes.get_mut(&sibling).unwrap().title = "pane-b-before".into();
    let mut edit = begin_title_edit(&mux, TitleEditScope::Pane, GroupBulkScope::Single, "window")
        .expect("pane edit opens");
    edit.input = "pane-a-after".into();

    assert!(mux.focus_pane(sibling), "focus drifted while edit was open");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.panes[&target].title, "pane-a-after");
    assert_eq!(mux.panes[&sibling].title, "pane-b-before");

    let (mut mux, target, sibling) = title_test_split_mux();
    mux.panes.get_mut(&sibling).unwrap().title = "survivor".into();
    let mut edit = begin_title_edit(&mux, TitleEditScope::Pane, GroupBulkScope::Single, "window")
        .expect("pane edit opens");
    edit.input = "dead-target-title".into();
    mux.panes.get_mut(&target).unwrap().closed = true;
    assert!(!mux.reap().mux_empty, "the sibling keeps the mux alive");
    assert_eq!(mux.active_focus(), Some(sibling));
    let mut open_edit = Some(edit.clone());
    assert!(clear_stale_title_edit(&mux, &mut open_edit));
    assert!(open_edit.is_none(), "a dead pane cannot retain its modal");
    assert!(!apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.panes[&sibling].title, "survivor");

    // The behavioral helper above is wired immediately after every
    // production reap, before either path can return on an empty mux.
    let source = production_source();
    // Needle the call, not the binding it lands in. Built at runtime so
    // this guard cannot match its own source.
    let reap_call = ["ws.mux.", "reap()"].concat();
    let reap_sites = source.matches(&reap_call).count();
    assert_eq!(reap_sites, 2, "update this guard when a reap site is added");
    assert_eq!(
        source
            .split(&reap_call)
            .skip(1)
            .filter(|tail| tail[..tail.len().min(600)]
                .contains("clear_stale_title_edit(&ws.mux, &mut ws.editing_title)"))
            .count(),
        reap_sites,
        "every reap path must dismiss a title edit whose pinned target died"
    );
}

/// `reap` has to report a removal, and only a removal.
///
/// Both call sites turn `layout_changed` into a `pending_resize` plus a
/// `request_redraw()`. A version that answered "yes" on every pass would
/// look correct in the split-close case and quietly re-drive `resize_all`
/// on every frame forever, which nothing else in the suite would catch.
/// The second row below is the one that pins that.
///
/// It lives here rather than in `mux::node_tests` because `Pane` owns a
/// `Terminal` and cannot be built without a PTY, and `title_test_split_mux`
/// is the existing helper that spawns one. The name is inherited from its
/// first caller; what it builds is a plain two-pane split.
#[test]
fn reap_reports_whether_it_removed_a_pane() {
    let (mut mux, target, sibling) = title_test_split_mux();

    mux.panes.get_mut(&target).unwrap().closed = true;
    let first = mux.reap();
    assert!(
        first.layout_changed,
        "the dead pane's space was handed over"
    );
    assert!(!first.mux_empty, "the sibling keeps the mux alive");

    let second = mux.reap();
    assert!(
        !second.layout_changed,
        "a pass that reaps nothing must not ask for a resize, or every \
             idle frame re-drives resize_all"
    );
    assert!(!second.mux_empty);

    mux.panes.get_mut(&sibling).unwrap().closed = true;
    let last = mux.reap();
    assert!(last.layout_changed);
    assert!(last.mux_empty, "the last pane leaves nothing behind");

    // The degenerate case: empty is not the same fact as changed.
    let empty = Mux::new().reap();
    assert!(!empty.layout_changed);
    assert!(empty.mux_empty);
}

#[test]
fn tab_title_edit_uses_its_anchor_after_focus_and_index_drift() {
    let (mut mux, panes) = title_test_tab_mux(3);
    let [before, target, sibling] = panes.as_slice() else {
        panic!("three test tabs")
    };
    assert!(mux.focus_pane(*target));
    let mut edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "window")
        .expect("tab edit opens");
    edit.input = "target-tab".into();

    let before_idx = mux.tab_index_of_any_pane(&[*before]).unwrap();
    assert!(!mux.close_tab_at(before_idx), "other tabs remain");
    assert!(mux.focus_pane(*sibling), "focus drifted to another tab");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    let target_idx = mux.tab_index_of_any_pane(&[*target]).unwrap();
    let sibling_idx = mux.tab_index_of_any_pane(&[*sibling]).unwrap();
    assert_eq!(
        mux.tabs[target_idx].title_override.as_deref(),
        Some("target-tab")
    );
    assert_eq!(mux.tabs[sibling_idx].title_override, None);

    // A tab anchor contains every opening pane, so one pane can be reaped
    // without killing an edit over the tab that its sibling still owns.
    let (mut mux, exiting, survivor) = title_test_split_mux();
    let mut edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "window")
        .expect("split-tab edit opens");
    edit.input = "split-tab-survives".into();
    mux.panes.get_mut(&exiting).unwrap().closed = true;
    assert!(
        !mux.reap().mux_empty,
        "the split sibling keeps the tab alive"
    );
    assert_eq!(mux.active_focus(), Some(survivor));
    let mut open_edit = Some(edit.clone());
    assert!(!clear_stale_title_edit(&mux, &mut open_edit));
    assert!(open_edit.is_some(), "the target tab still exists");
    assert!(apply_mux_title_edit(&mut mux, &edit));
    let survivor_tab = mux.tab_index_of_any_pane(&[survivor]).unwrap();
    assert_eq!(
        mux.tabs[survivor_tab].title_override.as_deref(),
        Some("split-tab-survives")
    );

    let (mut mux, panes) = title_test_tab_mux(2);
    let target = panes[0];
    let sibling = panes[1];
    assert!(mux.focus_pane(target));
    let mut edit = begin_title_edit(&mux, TitleEditScope::Tab, GroupBulkScope::Single, "window")
        .expect("tab edit opens");
    edit.input = "dead-tab-title".into();
    let target_idx = mux.tab_index_of_any_pane(&[target]).unwrap();
    assert!(!mux.close_tab_at(target_idx), "the sibling tab remains");
    assert_eq!(mux.active_focus(), Some(sibling));
    let mut open_edit = Some(edit.clone());
    assert!(clear_stale_title_edit(&mux, &mut open_edit));
    assert!(open_edit.is_none(), "a dead tab cannot retain its modal");
    assert!(!apply_mux_title_edit(&mut mux, &edit));
    assert_eq!(mux.tabs[0].title_override, None);
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
fn bulk_group_edits_keep_pinned_tab_and_live_window_semantics() {
    let (mut mux, first, second) = title_test_split_mux();
    let cfg = title_test_config();
    let argv = title_test_argv();
    let waker: kettle_core::Waker = Arc::new(|| {});
    mux.new_tab_with(&cfg, 80, 24, 8, 16, waker, &argv, None)
        .expect("spawn other group tab");
    let other = mux.active_focus().expect("other tab pane");

    assert!(mux.focus_pane(first));
    let mut tab_edit = begin_title_edit(&mux, TitleEditScope::Group, GroupBulkScope::Tab, "window")
        .expect("tab group edit opens");
    tab_edit.input = "tab-fleet".into();
    assert!(mux.focus_pane(other), "focus drifted to another tab");
    assert!(apply_mux_title_edit(&mut mux, &tab_edit));
    assert_eq!(mux.panes[&first].group_name.as_deref(), Some("tab-fleet"));
    assert_eq!(mux.panes[&second].group_name.as_deref(), Some("tab-fleet"));
    assert_eq!(mux.panes[&other].group_name, None);

    let mut window_edit = begin_title_edit(
        &mux,
        TitleEditScope::Group,
        GroupBulkScope::Window,
        "window",
    )
    .expect("window group edit opens");
    window_edit.input = "window-fleet".into();
    assert!(apply_mux_title_edit(&mut mux, &window_edit));
    assert!(
        mux.panes
            .values()
            .all(|pane| pane.group_name.as_deref() == Some("window-fleet"))
    );

    window_edit.input.clear();
    assert!(apply_mux_title_edit(&mut mux, &window_edit));
    assert!(
        mux.panes.values().all(|pane| pane.group_name.is_none()),
        "an empty GroupWindow edit still clears every live pane"
    );
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
        let end = body.find("\n    fn ").unwrap_or(body.len());
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

#[test]
fn echo_bypass_timestamp_only_marks_windows_that_accepted_input() {
    let before = std::time::Instant::now();
    let input_time = before + std::time::Duration::from_millis(1);
    let mut accepted = Some(before);
    let mut rejected = Some(before);
    stamp_accepted_input(
        &mut accepted,
        PaneInputDelivery {
            result: PaneInputResult::ReadOnly,
            accepted: true,
            receipt_accepted: false,
        },
        input_time,
    );
    stamp_accepted_input(
        &mut rejected,
        PaneInputDelivery {
            result: PaneInputResult::ReadOnly,
            accepted: false,
            receipt_accepted: false,
        },
        input_time,
    );
    assert_eq!(accepted, Some(input_time));
    assert_eq!(rejected, Some(before));

    let source = production_source();
    for owner in ["fn broadcast_input(", "fn write_terminal_key_event("] {
        let body = source.split(owner).nth(1).expect("delivery helper exists");
        let end = body.find("\n    fn ").unwrap_or(body.len());
        assert!(
            body[..end].contains("stamp_accepted_input(&mut other.last_typed"),
            "{owner} must stamp every foreign window that accepted input"
        );
    }
}

#[test]
fn clipboard_images_encode_off_the_window_thread_in_paste_order() {
    let source = production_source();
    let body = |name: &str| {
        source
            .split(&format!("\n    fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name} body"))
    };
    let image = body("paste_clipboard_image");
    assert!(
        image.contains("self.pasted_images.reserve_png(")
            && image.contains("encoder.submit(id, png, rgba)")
            && !image.contains("save_rgba"),
        "the window thread reserves the PNG and hands the encode to the encoder"
    );
    for name in ["paste_text", "paste_paths"] {
        assert!(
            body(name).contains("if !self.queued_pastes.is_empty()"),
            "{name} must wait behind an image still encoding"
        );
    }
    let deliver = body("deliver_queued_paste");
    assert!(
        deliver.contains(
            "if ws.mux.broadcast != broadcast || self.broadcast_recipients(&ws) != recipients {"
        ),
        "a queued broadcast paste must not reach a scope or panes changed since"
    );
}

#[test]
fn media_receipt_wiring_fails_closed_and_keeps_diagnostics_private() {
    let source = production_source();
    let confirmed = source
        .split("fn deliver_path_paste(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn show_media_paste_receipt").next())
        .expect("confirmed path delivery body");
    assert_eq!(confirmed.matches("delivery.receipt_accepted").count(), 2);
    assert_eq!(
        confirmed.matches("self.show_media_paste_receipt(").count(),
        4,
        "success, rejection, and a disappearing target must all consume the one-shot preview"
    );

    let show = source
        .split("fn show_media_paste_receipt(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("image receipt gate");
    for gate in [
        "paths.len() == 1",
        "if !accepted_by_receipt_pane",
        "receipt_pane.filter(|id| ws.mux.panes.contains_key(id))",
        "self.pasted_images.take_preview_for_path(&paths[0])",
        "video.filter(|_| self.cfg.paste_video_preview)",
    ] {
        assert!(show.contains(gate), "receipt gate lost {gate:?}");
    }
    let clipboard = source
        .split("fn paste_clipboard(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn paste_clipboard_image").next())
        .expect("clipboard paste entry point");
    let dropped = source
        .split("WindowEvent::DroppedFile(path) => {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::Focused").next())
        .expect("dropped-file entry point");
    assert!(
        clipboard.contains("VideoPasteSource::Clipboard")
            && dropped.contains("VideoPasteSource::Drop")
            && clipboard.contains("VideoPasteRequest::from_user_paths")
            && dropped.contains("VideoPasteRequest::from_user_paths")
            && !clipboard.contains("VideoPasteCandidate::from_user_paths")
            && !dropped.contains("VideoPasteCandidate::from_user_paths"),
        "video receipts must retain an explicit clipboard/drop provenance token"
    );

    let paths = source
        .split("fn paste_paths(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn deliver_path_paste").next())
        .expect("path paste body");
    assert!(
        paths.contains("if !within_limit {")
            && paths.contains("discard_previews_for_paths(&paths)")
            && !paths.contains("dismiss_media_paste_receipt_on_input"),
        "an oversize bitmap path must release its one-shot preview"
    );

    let geometry = source
        .split("// Geometry diagnostics expose only the receipt's presentation state.")
        .nth(1)
        .and_then(|rest| rest.split("Response::ok(").next())
        .expect("ui_geometry receipt projection");
    let wait = source
        .split("fn about_to_wait_inner(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("about_to_wait receipt cleanup");
    assert!(
        wait.contains("!ws.mux.panes.contains_key(&receipt.pane_id)")
            && wait.contains("ws.media_paste_receipt = None;")
            && wait.contains("ws.pending_video_paste_receipt = None;"),
        "closing the owner pane must remove its receipt and pending validation"
    );
    assert!(
        wait.contains("let receipt_enabled")
            && wait.contains("ws.media_paste_receipt.take().is_some()")
            && wait.contains("!self.cfg.paste_video_preview")
            && wait.contains("ws.pending_video_paste_receipt = None;"),
        "turning previews off must release retained and pending receipt state"
    );
    let config_clear_at = wait
        .find("if !self.cfg.paste_video_preview")
        .expect("video-preview config clear");
    let pending_wait_at = wait
        .find("let pending_video_receipt_wait")
        .expect("pending video receipt wait");
    assert!(
        config_clear_at < pending_wait_at,
        "disabling previews must clear pending state before scheduling its wake"
    );
    assert!(
        wait.contains("pending.expired(now)")
            && wait.contains("video paste receipt abandoned: preview worker response timed out")
            && wait.contains("let pending_video_receipt_wait")
            && wait.contains("if let Some(next) = pending_video_receipt_wait"),
        "a missing worker result must expire pending state on a scheduled wake"
    );
    let gpu_recovery = wait
        .split("if self.gpu.as_ref().is_some_and(|g| g.is_lost())")
        .nth(1)
        .and_then(|rest| rest.split("if ws.pty_resize_retry.take_due(now)").next())
        .expect("GPU recovery scheduler");
    assert_eq!(
        gpu_recovery.matches("pending_video_receipt_wait").count(),
        2,
        "GPU recovery backoff must not postpone pending receipt cleanup"
    );
    let ready = source
        .split("fn user_event_inner(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("user event handler")
        .split("UserEvent::VideoPreviewReady {")
        .nth(1)
        .and_then(|rest| rest.split("UserEvent::Wakeup").next())
        .expect("video preview result handler");
    assert!(
        ready.contains("ws.finish_pending_video_preview(")
            && ready.contains("std::time::Instant::now(),"),
        "a worker reply goes through the window's expiry-checked finish"
    );

    let ctl_press = source
        .split("fn ctl_mouse_press(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("ctl mouse press body");
    assert!(
        ctl_press.contains("activate_media_paste_receipt_at("),
        "synthetic presses must not click through receipt chrome"
    );
    assert!(
        !ctl_press.contains("scrollbar_jump("),
        "ctl presses must not invent native scrollbar semantics ahead of terminal input"
    );

    let ctl_move = source
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("ctl mouse move body");
    assert!(
        ctl_move.contains("update_media_paste_receipt_hover(ws)")
            && !ctl_move.contains("sync_cursor_icon"),
        "synthetic motion may expand the receipt but must not retarget native chrome hover or the OS cursor"
    );

    let hit_test = source
        .split("fn media_paste_receipt_geometry(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("receipt hit-test projection");
    assert!(
        hit_test.contains("self.media_paste_receipt_projection(ws)?")
            && !hit_test.contains("self.overlay("),
        "receipt hit-testing must project only the receipt and completion, never all window chrome"
    );

    let activation = source
        .split("fn activate_media_paste_receipt_at(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("receipt activation body");
    assert!(
        activation.contains("receipt.right_gutter > 0.0")
            && activation.contains("pane_x + pane_w - receipt.right_gutter")
            && activation.contains("if bcode == 0 {")
            && activation.contains("MediaPasteReceiptHit::Dismiss")
            && activation.contains("MediaPasteReceiptHit::Body")
            && activation.contains("ws.media_paste_receipt = None;")
            && activation.contains("self.open_media_paste_receipt(ws)"),
        "receipt activation must preserve the scrollbar, consume covered clicks, and honor its primary actions"
    );
    let open = source
        .split("fn open_media_paste_receipt(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("shared receipt open body");
    assert!(
        open.contains("ws.mux.active_focus() == Some(receipt.pane_id)")
            && open.contains("MediaPasteReceiptKind::Image")
            && open.contains("self.pasted_images.path_still_matches(&path)")
            && open.contains("ws.media_paste_receipt = None;")
            && open.contains("NotifyTitleImagePreviewUnavailable"),
        "only a retained image may open, after rechecking pane and file identity"
    );
    let accessibility = source
        .split("fn handle_accessibility_action(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("accessibility action body");
    assert!(
        accessibility.contains("|(receipt, _)| receipt.openable")
            && accessibility.contains("self.open_media_paste_receipt(ws)")
            && accessibility.contains("ws.media_paste_receipt.take().is_some()"),
        "assistive clicks must open retained images and dismiss non-openable video receipts"
    );
    let input_dismiss = source
        .split("fn dismiss_media_paste_receipt_on_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("receipt input dismissal body");
    assert!(
        input_dismiss.contains("Self::take_media_paste_receipt_on_input(ws)")
            && source.contains("Self::dismiss_media_paste_receipt_on_input(ws);"),
        "later input must remove a receipt whose command-line claim may be stale"
    );
    let terminal_input = source
        .split("fn write_terminal_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("terminal input body");
    let broadcast_input = source
        .split("fn broadcast_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("broadcast input body");
    let text_paste = source
        .split("fn deliver_text_paste(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn paste_paths(").next())
        .expect("confirmed text paste body");
    let send_newline = source
        .split("Action::SendNewline =>")
        .nth(1)
        .and_then(|rest| rest.split("Action::EditConfig =>").next())
        .expect("send-newline action");
    let run_command = source
        .split("fn ctl_run_command(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// `run_command` is pending").next())
        .expect("run-command body");
    assert!(
        terminal_input.contains("dismiss_media_paste_receipt_after_input")
            && broadcast_input
                .matches("dismiss_media_paste_receipt_after_delivery")
                .count()
                == 2
            && text_paste
                .matches("dismiss_media_paste_receipt_after_delivery")
                .count()
                == 2
            && text_paste.contains("dismiss_media_paste_receipt_after_input")
            && send_newline.contains("write_focused_terminal_input")
            && source.contains("Self::dismiss_media_paste_receipt_after_input(ws, id, result)")
            && run_command.contains("dismiss_media_paste_receipt_for_pane"),
        "every successful authored-input route must invalidate the affected pane's receipt, including foreign broadcasts and control commands"
    );
    assert!(
        !terminal_input.contains("Self::dismiss_media_paste_receipt_on_input(ws);")
            && !text_paste.contains("Self::dismiss_media_paste_receipt_on_input(ws);")
            && broadcast_input.contains("receipt_pane"),
        "a rejected pane write must preserve its receipt instead of clearing it optimistically"
    );
    assert!(
        confirmed.contains("let previous = Self::take_media_paste_state_on_input(ws)")
            && show.contains("ws.media_paste_receipt = previous_receipt")
            && show.contains("ws.pending_video_paste_receipt = previous_pending")
            && show.contains("pending.merge_drop(pane_id, &request, now)"),
        "path delivery must restore rejected state and coalesce accepted adjacent drops before background validation"
    );
    assert!(
        geometry.contains("\"dismiss_rect\":")
            && geometry.contains("\"preview_rect\":")
            && geometry.contains("\"kind\":")
            && geometry.contains("\"openable\":")
            && !geometry.contains("\"path\":")
            && !geometry.contains("extension")
            && !geometry.contains("rgba"),
        "ui_geometry must expose safe shared controls, never retained paths or pixels"
    );

    let native_press = source
        .split("WindowEvent::MouseInput {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::MouseWheel").next())
        .expect("native mouse input body");
    let terminal_mouse = native_press
        .find("self.send_mouse(")
        .expect("terminal mouse path");
    let scrollbar = native_press
        .find("self.scrollbar_jump(")
        .expect("native scrollbar path");
    assert!(
        terminal_mouse < scrollbar,
        "terminal mouse reporting and pane gestures must retain priority over the scrollbar"
    );

    let focus = source
        .split("WindowEvent::Focused(f) => {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::Occluded").next())
        .expect("window focus handler");
    assert!(
        focus.contains("receipt.set_hover(false, std::time::Instant::now())"),
        "focus loss must not freeze the receipt lifetime"
    );

    let action = source
        .split("fn ctl_perform_action(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("ctl perform-action body");
    assert!(
        action.contains("name == \"focus_window\"")
            && action.contains("window.set_visible(true);")
            && action.contains("window.focus_window();"),
        "live UI automation needs an idempotent focus action that never hides its target"
    );
    assert!(
        action.find("name == \"focus_window\"") < action.find("Action::from_name(name)"),
        "the control-only focus action must not depend on the user keybind action table"
    );
}

#[test]
fn macos_option_modifier_is_masked_per_side_and_mode() {
    use kettle_config::MacosOptionAsAlt;
    use winit::keyboard::ModifiersState;

    let raw = ModifiersState::ALT | ModifiersState::SHIFT;
    let masked = ModifiersState::SHIFT;
    for (policy, left, right, expected) in [
        (MacosOptionAsAlt::None, true, false, masked),
        (MacosOptionAsAlt::None, false, true, masked),
        (MacosOptionAsAlt::Left, true, false, raw),
        (MacosOptionAsAlt::Left, false, true, masked),
        (MacosOptionAsAlt::Right, true, false, masked),
        (MacosOptionAsAlt::Right, false, true, raw),
        (MacosOptionAsAlt::Both, true, false, raw),
        (MacosOptionAsAlt::Both, false, true, raw),
    ] {
        assert_eq!(
            macos_effective_modifiers(raw, policy, left, right),
            expected,
            "policy={policy:?}, left={left}, right={right}"
        );
    }
}

#[test]
fn control_and_super_keep_option_available_for_chords() {
    use kettle_config::MacosOptionAsAlt;
    use winit::keyboard::ModifiersState;

    for chord in [
        ModifiersState::CONTROL | ModifiersState::ALT,
        ModifiersState::SUPER | ModifiersState::ALT,
        ModifiersState::CONTROL | ModifiersState::SUPER | ModifiersState::ALT,
    ] {
        for policy in [
            MacosOptionAsAlt::None,
            MacosOptionAsAlt::Left,
            MacosOptionAsAlt::Right,
            MacosOptionAsAlt::Both,
        ] {
            assert_eq!(
                macos_effective_modifiers(chord, policy, false, false),
                chord,
                "Ctrl/Super suppresses Option composition for {policy:?}"
            );
        }
    }
}

/// The exemption list, both halves of it.
///
/// `macos-option-as-alt` decides whether Option composes text or acts as
/// Meta. That question only exists for keys macOS composes a character
/// from, and answering it for the rest is what made `⌥⌫` delete one
/// character. The negative half matters more than the positive one: adding
/// Enter, Space, Tab, Escape or any character key here is how kettle would
/// start printing escape text into a pane.
#[test]
fn only_keys_that_compose_no_text_treat_option_as_meta() {
    use winit::keyboard::{Key, NamedKey};

    for named in [
        NamedKey::Backspace,
        NamedKey::Delete,
        NamedKey::ArrowUp,
        NamedKey::ArrowDown,
        NamedKey::ArrowLeft,
        NamedKey::ArrowRight,
        NamedKey::Home,
        NamedKey::End,
        NamedKey::PageUp,
        NamedKey::PageDown,
        NamedKey::Insert,
        NamedKey::F1,
        NamedKey::F12,
        // Text "\r", but no composed character: Option+Return must be
        // Alt+Return, or it submits where a newline was asked for.
        NamedKey::Enter,
    ] {
        assert!(
            option_is_meta_for(&Key::Named(named)),
            "{named:?} composes no character, so Option must reach the encoder"
        );
    }

    // Option+Space composes a no-break space; Tab and Escape keep what
    // they send.
    for named in [NamedKey::Space, NamedKey::Tab, NamedKey::Escape] {
        assert!(
            !option_is_meta_for(&Key::Named(named)),
            "{named:?} produces text; restoring ALT would ESC-prefix it"
        );
    }
    for text in ["e", "a", "´", "8"] {
        assert!(
            !option_is_meta_for(&Key::Character(text.into())),
            "character keys are exactly what the policy protects ({text})"
        );
    }

    // The trap this list exists to avoid: winit reports the legacy C0
    // `\x08` from `NamedKey::Backspace.to_text()`, so the tempting
    // `to_text().is_none()` predicate excludes the very key at issue.
    assert_eq!(NamedKey::Backspace.to_text(), Some("\u{8}"));
}

/// Restoring Option at the encode seam, across every policy and both sides.
#[test]
fn option_reaches_the_encoder_for_exempt_keys_under_every_policy() {
    use kettle_config::MacosOptionAsAlt::{Both, Left, None as NoneAlt, Right};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let raw = ModifiersState::ALT;
    let backspace = Key::Named(NamedKey::Backspace);
    let letter = Key::Character("e".into());

    for (policy, left, right) in [
        (NoneAlt, true, false),
        (NoneAlt, false, true),
        (Left, false, true),
        (Right, true, false),
        (Both, true, false),
    ] {
        let masked = macos_effective_modifiers(raw, policy, left, right);
        // The whole bug, in one row: under the shipped default the mask
        // has already dropped ALT before the encoder ever runs.
        assert_eq!(
            macos_pty_modifiers(masked, raw, &backspace),
            ModifiersState::ALT,
            "Option must be Meta for Backspace under {policy:?} (left={left}, right={right})"
        );
        // …and must still not be, for a key macOS composes from.
        assert_eq!(
            macos_pty_modifiers(masked, raw, &letter),
            masked,
            "character keys follow the policy under {policy:?}"
        );
    }

    // No Option held: nothing to restore, whatever the key.
    let none = ModifiersState::empty();
    assert_eq!(macos_pty_modifiers(none, none, &backspace), none);
    // Other modifiers are carried through untouched.
    let shift = ModifiersState::SHIFT;
    assert_eq!(
        macos_pty_modifiers(shift, raw | ModifiersState::SHIFT, &backspace),
        shift | ModifiersState::ALT
    );
}

/// The edge case that motivated the allow-list: nothing kettle sends for a
/// composing chord may gain an ESC prefix or an xterm `CSI 27` form, because
/// a client that negotiated neither prints those as literal characters.
#[test]
fn option_composition_keys_never_gain_an_escape_prefix() {
    use kettle_config::MacosOptionAsAlt;
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let raw = ModifiersState::ALT;
    let masked = macos_effective_modifiers(raw, MacosOptionAsAlt::None, true, false);
    assert!(!masked.alt_key(), "the shipped default masks bare Option");

    let encode = |key: &Key, text: Option<&str>| {
        let mods = macos_pty_modifiers(masked, raw, key);
        crate::input::encode(key, text, mods, TermMode::empty())
            .unwrap_or_else(|| panic!("no encoding for {key:?}"))
    };

    // Composing keys keep their unmodified bytes.
    assert_eq!(encode(&Key::Named(NamedKey::Space), None), vec![0x20]);
    assert_eq!(encode(&Key::Named(NamedKey::Tab), None), b"\t".to_vec());
    assert_eq!(encode(&Key::Named(NamedKey::Escape), None), vec![0x1b]);
    // A glyph macOS composed arrives as itself, not as ESC + glyph.
    assert_eq!(
        encode(&Key::Character("´".into()), Some("´")),
        "´".as_bytes().to_vec()
    );

    // And the keys the fix is for do change. Return composes nothing,
    // so Option+Return is Alt+Return: ESC CR, a newline in zsh, Codex and
    // Claude Code, where a masked Option would submit the line.
    assert_eq!(
        encode(&Key::Named(NamedKey::Enter), None),
        b"\x1b\r".to_vec()
    );
    assert_eq!(
        encode(&Key::Named(NamedKey::Backspace), None),
        vec![0x1b, 0x7f],
        "Alt+Backspace is readline's backward-kill-word"
    );
    assert_eq!(
        encode(&Key::Named(NamedKey::Delete), None),
        b"\x1b[3;3~".to_vec()
    );
    assert_eq!(
        encode(&Key::Named(NamedKey::ArrowLeft), None),
        b"\x1b[1;3D".to_vec()
    );
}

/// The search bar's word-wise editing is bound to Option on macOS and is
/// gated only on Backspace, Delete and the horizontal arrows, each a key
/// `option_is_meta_for` exempts. Reading the masked `ws.mods` there would
/// make `delete_word_backward` and `move_left(by_word)` unreachable on the
/// shipped default.
#[test]
fn the_search_bar_word_modifier_reads_the_physical_option_key() {
    let src = production_source();
    let body = src
        .split("fn search_key(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("search_key body");
    let word_modifier = body
        .split("let word_modifier =")
        .nth(1)
        .and_then(|rest| rest.split(';').next())
        .expect("search_key computes a word modifier");
    assert!(
        word_modifier.contains("option_physically_held(ws)"),
        "macOS word editing must read the physical Option key, not the \
             masked modifiers: {word_modifier}"
    );
    assert!(
        !word_modifier.contains("ws.mods.alt_key()"),
        "the masked value is exactly what made these motions dead"
    );
}

/// `backspace-binding` must not swallow the word delete.
///
/// That remap only ever replaces the UNMODIFIED encoding, and it tests the
/// encoded bytes rather than the modifier state — so restoring Option
/// cannot collide with it: `⌥⌫` is `ESC DEL`, which is not the plain form.
/// Worth pinning because restoring ALT also flips `uses_kitty_sequence`
/// true once a pane negotiates the Kitty protocol, which skips the remap
/// entirely; both routes have to reach the same word delete.
#[test]
fn a_backspace_binding_remap_still_leaves_the_word_delete_alone() {
    use kettle_config::{BackspaceBinding, Config, MacosOptionAsAlt};
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cfg = Config {
        backspace_binding: BackspaceBinding::ControlH,
        ..Config::default()
    };
    let key = Key::Named(NamedKey::Backspace);
    let raw = ModifiersState::ALT;
    let masked = macos_effective_modifiers(raw, MacosOptionAsAlt::None, true, false);
    let mods = macos_pty_modifiers(masked, raw, &key);

    // Legacy pane: ESC DEL survives the remap.
    let legacy = crate::input::encode(&key, None, mods, TermMode::empty()).expect("legacy");
    assert_eq!(
        apply_bs_del_binding(&cfg, &key, legacy),
        vec![0x1b, 0x7f],
        "the remap replaces only the unmodified Backspace"
    );
    // …and an unmodified Backspace still honours the user's choice.
    let plain = crate::input::encode(&key, None, ModifiersState::empty(), TermMode::empty())
        .expect("plain");
    assert_eq!(apply_bs_del_binding(&cfg, &key, plain), vec![0x08]);

    // Kitty pane: the chord becomes CSI-u and carries the alt bit there.
    let kitty = TermMode::KITTY_KEYBOARD_PROTOCOL | TermMode::DISAMBIGUATE_ESC_CODES;
    let sequence = crate::input::encode_key_press(&key, mods, kitty).expect("kitty");
    assert_eq!(
        sequence,
        b"\x1b[127;3u".to_vec(),
        "alt+backspace in a Kitty pane is CSI 127;3u"
    );
}

/// The PTY key path must read the key-aware modifiers, never `ws.mods`.
///
/// `write_terminal_key_event` is the single funnel for all three encode
/// calls — focused pane, local broadcast, foreign-window broadcast — plus
/// the modified-Enter probe. Reverting any one of them to `ws.mods` puts
/// `⌥⌫` back to deleting a single character in that path alone, which is
/// exactly the kind of partial regression a byte test would miss.
#[test]
fn the_pty_key_path_never_reads_the_masked_modifiers() {
    let src = production_source();
    let body = src
        .split("fn write_terminal_key_event(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("write_terminal_key_event body");

    assert_eq!(
        body.matches("pty_key_modifiers(ws, &event.logical_key)")
            .count(),
        1,
        "the key-aware modifiers are computed exactly once, at the top"
    );
    assert!(
        !body.contains("ws.mods"),
        "every modifier read in this function must go through \
             pty_key_modifiers, or Option stops reaching the encoder"
    );
    assert_eq!(
        body.matches("encode_key_event_for_mode(").count(),
        4,
        "focused pane, local broadcast, foreign broadcast and retained releases \
             must use the same modifiers"
    );
}

/// A modal question has to outrank the overlay that asked it. Rebinding a
/// key onto a chord that is already taken raises a confirm dialog from
/// inside the Settings overlay. If the Settings arms claimed the keyboard
/// first, in either key path, the dialog could not be answered while the
/// panel that raised it stayed open. The panel's dim backdrop must also
/// stop above the dialog, or it greys out the question.
#[test]
fn a_confirm_dialog_outranks_every_overlay_including_the_one_that_raised_it() {
    let src = production_source();

    // Named-key path: the confirm branch comes before every other overlay,
    // and before the settings branches in particular.
    // Slice the router itself first: these same branch conditions appear
    // in the mouse handlers, so comparing whole-file positions would
    // compare the wrong pair and pass for the wrong reason.
    let router = src
        .split("let text = event.text.as_ref().map(|s| s.as_str());")
        .nth(1)
        .expect("the key router opens by binding `text`");
    let confirm = router
        .find("if ws.confirm_dialog.is_some() {")
        .expect("the confirm-modal key handler is present");
    for later in [
        "if ws.context_menu.is_some() {",
        "if ws.settings_nav.is_some() {",
        "if ws.palette_input.is_some() {",
    ] {
        let at = router
            .find(later)
            .unwrap_or_else(|| panic!("expected overlay branch {later:?} in the key router"));
        assert!(
            confirm < at,
            "the confirm modal must be handled before {later:?}, or the \
                 question it asks cannot be answered"
        );
    }

    // Character path: the same, so typed text cannot land in a settings
    // field while a modal is waiting on an answer.
    let chars = src
        .split("let key = Key::Character(text.into());")
        .nth(1)
        .expect("the character router builds a Character key first");
    let confirm_arm = chars
        .find("if ws.confirm_dialog.is_some() {")
        .expect("the character router must swallow text while a modal is up");
    for later in ["ws.palette_input.is_some()", "ws.settings_nav.is_some()"] {
        let at = chars
            .find(later)
            .unwrap_or_else(|| panic!("expected {later:?} in the character router"));
        assert!(
            confirm_arm < at,
            "the modal arm must precede {later:?} in the character router"
        );
    }

    // And it stays legible: the confirm bar is queued with the menu chrome
    // (so it clears the base overlay quads), and the settings panel's dim
    // backdrop — pushed to the same list afterwards, therefore drawn over
    // it — stops above the bar rather than greying it out.
    let render =
        kettle_test_support::production_source(include_str!("../../../kettle-render/src/lib.rs"));
    for guarded_arm in [
        "overlay.confirm_dialog.is_none()\n            && let Some(search) = overlay.search.as_ref()",
        "overlay.confirm_dialog.is_none()\n            && let Some(q) = &overlay.palette_query",
        "overlay.confirm_dialog.is_none()\n            && let Some(q) = &overlay.theme_picker_query",
        "overlay.confirm_dialog.is_none()\n            && let Some(q) = &overlay.layout_picker_query",
        "overlay.confirm_dialog.is_none()\n            && let Some(q) = &overlay.ssh_query",
        "overlay.confirm_dialog.is_none()\n            && let Some(edit) = &overlay.edit_title",
    ] {
        assert!(
            render.contains(guarded_arm),
            "the renderer must suppress {guarded_arm} while a confirm dialog owns the keyboard"
        );
    }
    assert!(
        render.contains("menu_q.push(rect(0.0, sh - bar_h, sw, bar_h, theme.palette[1], 1.0));"),
        "the confirm bar must be queued into the menu-chrome pass, and \
             painted opaque so its AA contrast guarantee against palette[1] \
             is not diluted by whatever terminal content sits underneath"
    );
    assert!(
        render.contains("menu_q.push(rect(0.0, 0.0, sw, dim_h, theme.background, backdrop_a));")
            && render.contains("let dim_h = if overlay.confirm_dialog.is_some() {"),
        "the settings backdrop must stop above a live confirm bar; a \
             full-height dim would cover the question being asked"
    );
}

/// A config reload has to push the scrollback budget into panes that are
/// already open. Otherwise editing `scrollback` or `scrollback-bytes`,
/// through the file or the Settings overlay's two rows, reloads the value
/// and changes nothing visible.
#[test]
fn a_reload_carries_the_scrollback_budget_into_live_panes() {
    let src = production_source();
    let apply = src
        .split("fn apply_reloaded_config(")
        .nth(1)
        .expect("apply_reloaded_config present");
    let end = apply.find("\n    fn ").unwrap_or(apply.len());
    assert!(
        apply[..end]
            .contains("set_scrollback_limits(self.cfg.scrollback, self.cfg.scrollback_bytes)"),
        "the reload must hand both scrollback settings to the live panes, \
             or the Settings rows are inert until restart"
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
        let end = body.find("\n    fn ").unwrap_or(body.len());
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
        let end = body.find("\n    fn ").unwrap_or(body.len());
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

/// `broadcast-default` picks the scope the group chord turns on.
///
/// The default maps to per-tab, so an unconfigured kettle keeps the
/// chord's per-tab behaviour.
#[test]
fn broadcast_default_picks_the_scope_the_group_chord_turns_on() {
    use crate::mux::BroadcastScope;
    use kettle_config::BroadcastDefault;

    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::default()),
        BroadcastScope::Tab,
        "an unconfigured kettle must keep the per-tab behaviour the chord \
             has always had"
    );
    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::All),
        BroadcastScope::All
    );
    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::Group),
        BroadcastScope::Tab
    );
    assert_eq!(
        broadcast_scope_for_default(BroadcastDefault::Off),
        BroadcastScope::Off
    );

    // The chord is a toggle, so the key that turns broadcast on also turns
    // it off. Drive the dispatch's own expression.
    let toggled = |current: BroadcastScope, on: BroadcastScope| {
        if current == on {
            BroadcastScope::Off
        } else {
            on
        }
    };
    let on = broadcast_scope_for_default(BroadcastDefault::Group);
    assert_eq!(
        toggled(BroadcastScope::Off, on.clone()),
        BroadcastScope::Tab
    );
    assert_eq!(
        toggled(BroadcastScope::Tab, on.clone()),
        BroadcastScope::Off,
        "pressing the chord again must turn broadcast off"
    );
    // From some other scope it selects the configured one rather than
    // toggling off — the user asked for this scope.
    assert_eq!(toggled(BroadcastScope::All, on), BroadcastScope::Tab);
    // And `off` means the chord genuinely cannot enable broadcast.
    let never = broadcast_scope_for_default(BroadcastDefault::Off);
    assert_eq!(
        toggled(BroadcastScope::Off, never.clone()),
        BroadcastScope::Off
    );
    assert_eq!(toggled(BroadcastScope::Tab, never), BroadcastScope::Off);

    // The dispatch must be running that expression, not a re-implementation.
    let src = production_source();
    let arm = src
        .split("Action::ToggleBroadcastAll => {")
        .nth(1)
        .expect("the broadcast toggle arm is present");
    assert!(
        arm.contains("broadcast_scope_for_default(self.cfg.broadcast_default)")
            && arm.contains("if ws.mux.broadcast == on"),
        "the toggle arm must read `broadcast-default` and compare against \
             the current scope"
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

/// `backspace-binding` / `delete-binding` must replace only the
/// UNMODIFIED key.
///
/// Every case drives the real encoder and then the real remap, in the
/// order the key path uses them, so the expectations cannot drift from
/// what the terminal actually writes. `delete-binding` defaults to
/// `EscapeSequence`, so an unguarded Delete arm would rewrite *every*
/// modified Delete to the plain `CSI 3 ~`. `Ctrl+Delete` would then be
/// byte-identical to `Delete`, and no `<C-Del>` mapping could fire.
#[test]
fn a_modified_delete_keeps_its_modifier_through_the_binding_remap() {
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cfg = kettle_config::Config::default();
    let mode = TermMode::empty();
    let delete = Key::Named(NamedKey::Delete);
    let encode = |mods: ModifiersState| -> Vec<u8> {
        crate::input::encode_key_press(&delete, mods, mode).expect("Delete encodes")
    };

    // Precondition: the default really is the binding that rewrites, and
    // the encoder really does distinguish these. Without both, the
    // assertions below would hold for the wrong reason.
    assert_eq!(
        cfg.delete_binding,
        kettle_config::DeleteBinding::EscapeSequence,
        "this test is about the DEFAULT binding"
    );
    let plain = encode(ModifiersState::empty());
    assert_eq!(plain, b"\x1b[3~");

    for (label, mods) in [
        ("ctrl", ModifiersState::CONTROL),
        ("shift", ModifiersState::SHIFT),
        ("alt", ModifiersState::ALT),
        (
            "ctrl+shift",
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        ),
    ] {
        let encoded = encode(mods);
        assert_ne!(
            encoded, plain,
            "the encoder must distinguish {label}+Delete, or this test \
                 cannot detect the remap flattening it"
        );
        assert_eq!(
            apply_bs_del_binding(&cfg, &delete, encoded.clone()),
            encoded,
            "{label}+Delete must survive the binding remap unchanged"
        );
    }

    // The feature itself still works on the unmodified key.
    let ascii_del = kettle_config::Config {
        delete_binding: kettle_config::DeleteBinding::AsciiDel,
        ..kettle_config::Config::default()
    };
    assert_eq!(
        apply_bs_del_binding(&ascii_del, &delete, plain.clone()),
        vec![0x7f],
        "an unmodified Delete must still honour the binding"
    );
}

#[test]
fn only_modified_enter_requests_a_live_context_sample() {
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let enter = Key::Named(NamedKey::Enter);
    assert!(key_is_modified_enter(&enter, ModifiersState::SHIFT));
    assert!(key_is_modified_enter(&enter, ModifiersState::CONTROL));
    assert!(!key_is_modified_enter(&enter, ModifiersState::empty()));
    assert!(!key_is_modified_enter(
        &Key::Named(NamedKey::Tab),
        ModifiersState::SHIFT
    ));
}

/// Backspace's binding has the same contract, and the same trap one level
/// down. Its level-0 forms vary with control and alt, but a
/// `modifyOtherKeys` level-2 `Shift+Backspace` is a `CSI 27;2;8~` that a
/// bare `!control && !alt` guard would flatten to `0x7f`.
#[test]
fn a_modified_backspace_keeps_its_encoding_through_the_binding_remap() {
    use kettle_core::TermMode;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let cfg = kettle_config::Config {
        backspace_binding: kettle_config::BackspaceBinding::ControlH,
        ..kettle_config::Config::default()
    };
    let backspace = Key::Named(NamedKey::Backspace);
    let plain =
        crate::input::encode_key_press(&backspace, ModifiersState::empty(), TermMode::empty())
            .expect("Backspace encodes");
    assert_eq!(plain, vec![0x7f]);
    assert_eq!(
        apply_bs_del_binding(&cfg, &backspace, plain),
        vec![0x08],
        "an unmodified Backspace must honour `control-h`"
    );

    for (label, mods, mode) in [
        ("ctrl", ModifiersState::CONTROL, TermMode::empty()),
        ("alt", ModifiersState::ALT, TermMode::empty()),
        // Backspace opts out of level-1 encoding but opts in at level 2
        // for anything that is not bare Control, so this is the level
        // where Shift produces a distinct sequence.
        (
            "shift at modifyOtherKeys level 2",
            ModifiersState::SHIFT,
            TermMode::MODIFY_OTHER_KEYS_2,
        ),
    ] {
        let encoded =
            crate::input::encode_key_press(&backspace, mods, mode).expect("Backspace encodes");
        assert_ne!(
            encoded,
            vec![0x7f],
            "the encoder must distinguish {label}+Backspace, or this case \
                 cannot detect the remap flattening it"
        );
        assert_eq!(
            apply_bs_del_binding(&cfg, &backspace, encoded.clone()),
            encoded,
            "{label}+Backspace must survive the binding remap unchanged"
        );
    }
}

#[test]
fn input_rejections_have_distinct_rpc_semantics() {
    use kettle_ctl::protocol::error_codes as ec;

    assert_eq!(ctl_input_error(PaneInputResult::Queued), None);
    assert_eq!(
        ctl_input_error(PaneInputResult::ReadOnly).map(|error| error.0),
        Some(ec::READ_ONLY)
    );
    assert_eq!(
        ctl_input_error(PaneInputResult::Backpressured).map(|error| error.0),
        Some(ec::BUSY)
    );
    assert_eq!(
        ctl_input_error(PaneInputResult::Oversize).map(|error| error.0),
        Some(ec::BAD_PARAMS)
    );
    assert_eq!(
        ctl_input_error(PaneInputResult::Failed).map(|error| error.0),
        Some(ec::INTERNAL)
    );
}

#[test]
fn input_rejection_feedback_is_visible_but_throttled() {
    let now = std::time::Instant::now();
    assert!(input_rejection_message(PaneInputResult::Queued).is_none());
    assert!(input_rejection_message(PaneInputResult::ReadOnly).is_none());
    assert!(input_rejection_message(PaneInputResult::Backpressured).is_some());
    assert!(should_notify_input_rejection(
        None,
        PaneInputResult::Backpressured,
        now
    ));
    assert!(!should_notify_input_rejection(
        Some((PaneInputResult::Backpressured, now)),
        PaneInputResult::Backpressured,
        now + std::time::Duration::from_secs(1)
    ));
    assert!(
        should_notify_input_rejection(
            Some((PaneInputResult::Backpressured, now)),
            PaneInputResult::Oversize,
            now + std::time::Duration::from_secs(1)
        ),
        "a different failure class must be surfaced immediately"
    );
    assert!(should_notify_input_rejection(
        Some((PaneInputResult::Backpressured, now)),
        PaneInputResult::Backpressured,
        now + std::time::Duration::from_secs(3)
    ));
}

#[test]
fn local_paste_limit_rejects_instead_of_silently_truncating() {
    let at_limit = "x".repeat(4 << 20);
    assert!(local_paste_within_limit(&at_limit));
    let over_limit = format!("{at_limit}y");
    assert!(!local_paste_within_limit(&over_limit));
    assert!(
        input_rejection_message(PaneInputResult::Oversize).is_some(),
        "oversized paste rejection must have visible feedback"
    );
}

#[test]
fn pending_lua_fifo_preserves_calls_order_and_exact_byte_accounting() {
    let mut pending = PendingLuaCommands::default();
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(&b"first"[..]),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::Notify {
            origin_window: 1,
            title: "notice".into(),
            body: "body".into(),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::ExecAction {
            origin_window: 1,
            action: Action::NewTab,
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::SetTheme {
            origin_window: 1,
            name: "Default".into(),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(&b"second"[..]),
        }),
        PaneInputResult::Queued
    );
    assert_eq!(pending.len(), 5);
    assert_eq!(pending.send_bytes, 11);

    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::SendText { bytes, .. }) if bytes.as_ref() == b"first"
    ));
    assert_eq!(pending.send_bytes, 6);
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::Notify {
            title,
            body,
            ..
        }) if title == "notice" && body == "body"
    ));
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::ExecAction {
            action: Action::NewTab,
            ..
        })
    ));
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::SetTheme { name, .. }) if name == "Default"
    ));
    assert!(matches!(
        pending.pop_front(),
        Some(PendingLuaCommand::SendText { bytes, .. }) if bytes.as_ref() == b"second"
    ));
    assert_eq!(pending.send_bytes, 0);
    assert!(pending.is_empty());
}

#[test]
fn pending_lua_fifo_keeps_large_sends_separate_and_bounded() {
    let mut pending = PendingLuaCommands::default();
    for _ in 0..8 {
        assert_eq!(
            pending.push(PendingLuaCommand::SendText {
                origin_window: 1,
                target_pane: None,
                bytes: Arc::from(vec![b'x'; 1 << 20]),
            }),
            PaneInputResult::Queued
        );
    }
    assert_eq!(pending.len(), 8);
    assert_eq!(pending.send_bytes, 8 << 20);
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(vec![b'y'; 1]),
        }),
        PaneInputResult::Backpressured
    );
    assert_eq!(
        pending.push(PendingLuaCommand::SendText {
            origin_window: 1,
            target_pane: None,
            bytes: Arc::from(vec![b'z'; (1 << 20) + 1]),
        }),
        PaneInputResult::Oversize
    );
    assert_eq!(pending.len(), 8);
    assert_eq!(pending.send_bytes, 8 << 20);
}

#[test]
fn automation_retry_is_deadline_driven_and_resets_after_progress() {
    let now = std::time::Instant::now();
    let mut retry = AutomationRetry::default();
    assert!(retry.is_ready(now));

    retry.backpressured(now);
    assert!(!retry.is_ready(now));
    assert_eq!(retry.remaining(now), Some(AUTOMATION_RETRY_MIN));
    assert!(retry.is_ready(now + AUTOMATION_RETRY_MIN));

    retry.backpressured(now + AUTOMATION_RETRY_MIN);
    assert_eq!(
        retry.remaining(now + AUTOMATION_RETRY_MIN),
        Some(AUTOMATION_RETRY_MIN * 2)
    );
    retry.progressed();
    assert!(retry.is_ready(now));
    retry.backpressured(now);
    assert_eq!(retry.remaining(now), Some(AUTOMATION_RETRY_MIN));
}

#[test]
fn remote_batch_parser_preserves_operation_order_and_payload_boundaries() {
    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch("send-text first\\nsecond\r\nnew-tab\ntoggle-window\n", 7)
    else {
        panic!("three operations are below the batch cap");
    };
    assert_eq!(unknown_lines, 0);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::SendText {
            origin_window: 7,
            target_pane: None,
            bytes,
        }) if bytes.as_ref() == b"first\nsecond"
    ));
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 7 })
    ));
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::ToggleWindow { origin_window: 7 })
    ));
    assert!(commands.is_empty());
}

#[test]
fn remote_batch_json_send_text_round_trips_adversarial_text_exactly() {
    let payload = "literal \\n; actual LF follows\nCR follows\rNUL follows\0toggle-window\nnew-tab\n\
             send-text injected";
    let batch = format!(
        "send-text-json {}\nnew-tab\n",
        serde_json::to_string(payload).unwrap()
    );

    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch(&batch, 11)
    else {
        panic!("two operations are below the batch cap");
    };
    assert_eq!(unknown_lines, 0);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::SendText {
            origin_window: 11,
            target_pane: None,
            bytes,
        }) if bytes.as_ref() == payload.as_bytes()
    ));
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 11 })
    ));
    assert!(
        commands.is_empty(),
        "command-looking payload lines must not become spool operations"
    );
}

#[test]
fn remote_batch_parser_coalesces_malformed_json_as_unknown_lines() {
    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch(
        "send-text-json \"unterminated\nsend-text-json 7\nnew-tab\n",
        12,
    )
    else {
        panic!("one valid operation is below the batch cap");
    };

    assert_eq!(unknown_lines, 2);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 12 })
    ));
    assert!(commands.is_empty());
}

#[test]
fn remote_batch_parser_accepts_exact_operation_cap_and_rejects_all_on_overflow() {
    let exact = "new-tab\n".repeat(MAX_REMOTE_COMMANDS_PER_BATCH);
    let ParsedRemoteCommandBatch::Accepted {
        commands,
        unknown_lines,
    } = parse_remote_command_batch(&exact, 7)
    else {
        panic!("the exact operation cap must be accepted");
    };
    assert_eq!(commands.len(), MAX_REMOTE_COMMANDS_PER_BATCH);
    assert_eq!(unknown_lines, 0);

    let mut over = String::from("unknown-before\nsend-text must-not-run\n");
    over.push_str(&exact);
    over.push_str("unknown-after\n");
    assert!(matches!(
        parse_remote_command_batch(&over, 7),
        ParsedRemoteCommandBatch::RejectedTooMany {
            operation_count,
            unknown_lines: 2,
        } if operation_count == MAX_REMOTE_COMMANDS_PER_BATCH + 1
    ));
}

#[test]
fn remote_batch_parser_coalesces_unknown_line_count() {
    let ParsedRemoteCommandBatch::Accepted {
        mut commands,
        unknown_lines,
    } = parse_remote_command_batch(
        "unknown-one\n# comment\nnew-tab\nunknown-two\nunknown-three\n",
        9,
    )
    else {
        panic!("one recognized operation is below the batch cap");
    };
    assert_eq!(unknown_lines, 3);
    assert!(matches!(
        commands.pop_front(),
        Some(PendingRemoteCommand::NewTab { origin_window: 9 })
    ));
    assert!(commands.is_empty());
}

#[test]
fn delayed_remote_claim_is_owned_by_the_focused_window() {
    assert!(!should_poll_remote_window(false, true, false, 1, 2));
    assert!(should_poll_remote_window(false, true, false, 2, 2));
    assert!(!should_poll_remote_window(false, false, true, 1, 2));
    assert!(should_poll_remote_window(false, false, true, 2, 2));
    // Once the claimed FIFO has an origin, every window gets a chance to
    // reach the matching head regardless of which one is focused now.
    assert!(should_poll_remote_window(true, false, false, 1, 2));
}

#[test]
fn remote_batch_claim_leaves_spool_unchanged_while_sender_lock_is_held() {
    use std::io::{Read as _, Seek as _, Write as _};

    let dir = crate::test_tempdir();
    let path = dir.path().join("remote.cmd");
    let mut file = kettle_state::open_private_file(&path).unwrap();
    file.write_all(b"send-text exact\n").unwrap();
    file.flush().unwrap();
    drop(file);

    let lock_path = kettle_state::remote_command_lock_path(&path);
    let lock = kettle_state::ExclusiveFileLock::acquire(&lock_path).unwrap();
    assert_eq!(
        claim_remote_command_file(&path).unwrap(),
        RemoteBatchClaim::Busy
    );
    let mut preserved = kettle_state::open_existing_private_file(&path).unwrap();
    let mut bytes = Vec::new();
    preserved.seek(std::io::SeekFrom::Start(0)).unwrap();
    preserved.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"send-text exact\n");
    drop(preserved);
    drop(lock);

    assert_eq!(
        claim_remote_command_file(&path).unwrap(),
        RemoteBatchClaim::Claimed("send-text exact\n".to_string())
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
}

#[test]
fn malformed_remote_batch_is_cleared_once_instead_of_polled_forever() {
    use std::io::Write as _;

    let dir = crate::test_tempdir();
    let path = dir.path().join("remote.cmd");
    let mut file = kettle_state::open_private_file(&path).unwrap();
    file.write_all(&[0xff, 0xfe]).unwrap();
    file.flush().unwrap();
    drop(file);

    assert_eq!(
        claim_remote_command_file(&path).unwrap(),
        RemoteBatchClaim::RejectedMalformed
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
}

#[test]
fn every_lua_event_and_url_handler_dispatch_drains_commands() {
    let source = production_source();
    let raw_fire = [".fire_", "event("].concat();
    assert_eq!(
        source.matches(&raw_fire).count(),
        1,
        "all raw LuaEngine::fire_event calls must stay inside fire_lua_event"
    );
    let open_url = source
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .and_then(|rest| rest.split("fn paste_clipboard").next())
        .expect("open_url helper");
    let handler = open_url
        .find("try_url_handler(uri)")
        .expect("Lua URL handler dispatch");
    let drain = open_url
        .find("self.drain_lua_hook_commands(ws, \"URL handler\")")
        .expect("Lua URL handler drain");
    let handled_return = open_url
        .find("UrlHandlerOutcome::Handled => return,")
        .expect("claimed URL early return");
    assert!(
        handler < drain && drain < handled_return,
        "URL-handler callback commands must drain before a claimed handler returns"
    );
    // A handler may hand back a DIFFERENT URL than the one clicked, and it
    // was fed untrusted terminal text to produce it — so the rewritten URL
    // has to clear the same allowlist the original did, not inherit its
    // approval.
    let rewritten = open_url
        .find("UrlHandlerOutcome::Open(target)")
        .expect("rewritten URL arm");
    let recheck = open_url
        .find("if !kettle_core::links::is_safe_url(target)")
        .expect("rewritten URL must be re-checked against the allowlist");
    assert!(
        rewritten < recheck,
        "a Lua-supplied URL must be allowlist-checked before it is opened"
    );
}

#[test]
fn refused_terminal_url_warning_never_contains_the_payload() {
    let hostile = "javascript:alert('private-token=sekret')\nforged-log-line";
    let mut messages = Vec::new();

    super::warn_refused_terminal_url(hostile, |message| messages.push(message.to_string()));

    assert_eq!(messages.len(), 1);
    assert!(!messages[0].contains(hostile));
    assert!(!messages[0].contains("sekret"));
    assert_eq!(
        messages[0],
        "refused to open an unsafe URL from terminal output"
    );
}

#[test]
fn output_generations_commit_only_after_presentation() {
    let mut committed = std::collections::HashMap::from([(1, 10), (2, 20)]);
    let mut candidate = std::collections::HashMap::from([(1, 11), (2, 21)]);

    for outcome in [
        FrameOutcome::RetryLater,
        FrameOutcome::Occluded,
        FrameOutcome::SurfaceLost,
    ] {
        assert!(!apply_output_generation_outcome(
            outcome,
            &mut committed,
            &mut candidate,
        ));
        assert_eq!(
            committed,
            std::collections::HashMap::from([(1, 10), (2, 20)])
        );
        assert_eq!(
            candidate,
            std::collections::HashMap::from([(1, 11), (2, 21)])
        );
    }

    assert!(apply_output_generation_outcome(
        FrameOutcome::Presented,
        &mut committed,
        &mut candidate,
    ));
    assert_eq!(
        committed,
        std::collections::HashMap::from([(1, 11), (2, 21)])
    );
    assert_eq!(
        candidate,
        std::collections::HashMap::from([(1, 10), (2, 20)])
    );
}

#[test]
fn output_activity_uses_the_lock_free_generation_edge() {
    assert!(!output_generation_advanced(None, 7));
    assert!(!output_generation_advanced(Some(7), 7));
    assert!(output_generation_advanced(Some(7), 8));
    assert!(
        output_generation_advanced(Some(u64::MAX), 0),
        "generation wrap is still a change"
    );
}

#[test]
fn remote_poll_stages_checked_out_and_stored_windows_without_stale_roots() {
    let target = |pid| {
        Some(kettle_remote::RemoteProbeTarget {
            pid,
            foreground_pid: Some(pid),
            allow_native_cwd: true,
        })
    };
    let mut scratch = vec![target(999).unwrap()];
    stage_remote_targets(
        &mut scratch,
        [target(10), None, target(11)],
        [target(20), target(21), None],
    );
    assert_eq!(
        scratch.iter().map(|target| target.pid).collect::<Vec<_>>(),
        [10, 11, 20, 21]
    );

    stage_remote_targets(
        &mut scratch,
        [target(30)],
        std::iter::empty::<Option<kettle_remote::RemoteProbeTarget>>(),
    );
    assert_eq!(
        scratch.iter().map(|target| target.pid).collect::<Vec<_>>(),
        [30]
    );
}

#[test]
fn omitted_remote_probe_preserves_shell_until_pane_identity_changes() {
    let probe = kettle_remote::RemoteProbe {
        remote: None,
        native_cwd: None,
        foreground_shell: Some(kettle_remote::ShellLaunch {
            argv: vec!["bash".into(), "-i".into()],
            cwd: Some("/tmp".into()),
        }),
        foreground_process: None,
    };
    let mut applied = std::collections::HashMap::from([
        ((1, 10), probe.clone()),
        ((2, 20), probe.clone()),
        ((3, 30), probe.clone()),
        ((4, 40), probe.clone()),
    ]);
    let mut live = HashSet::from([(99, 99)]);
    super::retain_live_remote_probes(
        &mut applied,
        &mut live,
        [(1, Some(10)), (2, Some(21)), (4, None)],
    );
    assert_eq!(
        applied,
        std::collections::HashMap::from([((1, 10), probe.clone())])
    );
    let cleared = kettle_remote::RemoteProbe {
        foreground_shell: None,
        ..probe
    };
    assert!(stage_applied_remote_probe(&mut applied, 1, 10, &cleared));
    assert!(applied[&(1, 10)].foreground_shell.is_none());
}

#[test]
fn remote_snapshot_compares_against_ui_applied_state_not_worker_history() {
    let old = kettle_remote::RemoteProbe {
        remote: None,
        native_cwd: Some("old".into()),
        foreground_shell: None,
        foreground_process: None,
    };
    let new = kettle_remote::RemoteProbe {
        remote: None,
        native_cwd: Some("new".into()),
        foreground_shell: None,
        foreground_process: None,
    };
    let mut applied = std::collections::HashMap::from([((70, 7), old)]);

    assert!(stage_applied_remote_probe(&mut applied, 70, 7, &new));
    assert!(!stage_applied_remote_probe(&mut applied, 70, 7, &new));

    assert!(
        stage_applied_remote_probe(&mut applied, 71, 7, &new),
        "a second pane object sharing/reusing the pid needs its own first install"
    );
}

#[test]
fn native_cwd_is_suppressed_for_nonlocal_launch_clients() {
    for argv0 in [
        "wsl.exe",
        "C:\\Windows\\System32\\OpenSSH\\ssh.exe",
        "DOCKER.EXE",
        "/usr/bin/podman",
        "kubectl",
        "lxc-attach",
    ] {
        assert!(
            argv_is_nonlocal_client(&[argv0.into()]),
            "nonlocal client {argv0}"
        );
    }
    assert!(!argv_is_nonlocal_client(&["pwsh.exe".into()]));
}

#[test]
fn osc52_selection_uses_the_platform_selection_channel() {
    assert_eq!(
        osc52_clipboard_channel(ClipboardType::Clipboard),
        Osc52ClipboardChannel::Clipboard
    );

    #[cfg(target_os = "linux")]
    assert_eq!(
        osc52_clipboard_channel(ClipboardType::Selection),
        Osc52ClipboardChannel::Primary
    );
    #[cfg(not(target_os = "linux"))]
    assert_eq!(
        osc52_clipboard_channel(ClipboardType::Selection),
        Osc52ClipboardChannel::Clipboard
    );
}

#[test]
fn hover_generation_candidate_preserves_racing_output_as_pending() {
    let committed = std::collections::HashMap::from([(1, 10), (2, 20)]);
    let cached = [(1, 10, 120, 40)];
    let live_after_race = [(1, 11), (2, 21)];
    let mut candidate = std::collections::HashMap::new();

    stage_output_generations_for_frame(&mut candidate, &committed, Some(&cached), live_after_race);
    assert_eq!(candidate, committed);

    stage_output_generations_for_frame(&mut candidate, &committed, None, live_after_race);
    assert_eq!(candidate, std::collections::HashMap::from(live_after_race));
}

#[test]
fn stale_presented_output_wakeup_rearms_without_losing_a_race() {
    let wakes = Arc::new(AtomicUsize::new(0));
    let wake_count = wakes.clone();
    let gate = OutputWakeGate::new(Arc::new(move || {
        wake_count.fetch_add(1, Ordering::SeqCst);
    }));
    let generation = AtomicU64::new(0);

    // The first output schedules the redraw. The frame reopens the latch,
    // then output racing that frame schedules another wake but is included
    // in the generation that successfully presents.
    assert!(gate.request());
    gate.acknowledge();
    generation.store(1, Ordering::Release);
    assert!(gate.request());
    let seen_generation = 1;
    assert_eq!(wakes.load(Ordering::SeqCst), 2);

    assert!(
        !output_wakeup_needs_paint(
            || generation.load(Ordering::Acquire) != seen_generation,
            || gate.acknowledge(),
        ),
        "the queued event is stale because its generation already presented"
    );

    generation.store(2, Ordering::Release);
    assert!(
        gate.request(),
        "rearming the stale event must let future output own a wake"
    );
    assert_eq!(wakes.load(Ordering::SeqCst), 3);

    // Output between the first no-damage sample and rearm cannot enqueue a
    // duplicate while the old wake is pending. The post-rearm sample must
    // therefore retain it for this event instead of returning stale.
    let raced_generation = AtomicU64::new(2);
    let raced_seen = 2;
    assert!(output_wakeup_needs_paint(
        || raced_generation.load(Ordering::Acquire) != raced_seen,
        || {
            raced_generation.store(3, Ordering::Release);
            assert!(!gate.request());
            gate.acknowledge();
        },
    ));
}

#[test]
fn dirty_output_wakeup_keeps_latch_closed_until_frame_snapshot() {
    let rearms = AtomicUsize::new(0);
    assert!(output_wakeup_needs_paint(
        || true,
        || {
            rearms.fetch_add(1, Ordering::SeqCst);
        },
    ));
    assert_eq!(
        rearms.load(Ordering::SeqCst),
        0,
        "dirty output must remain latched across the deferred paint budget"
    );
}

#[test]
fn pane_snapshot_reuse_fails_closed_on_output_layout_or_order_changes() {
    let cached = [(1, 10, 120, 40), (2, 20, 80, 24)];
    assert!(pane_snapshot_keys_match(&cached, cached));
    assert!(!pane_snapshot_keys_match(
        &cached,
        [(1, 11, 120, 40), (2, 20, 80, 24)]
    ));
    assert!(!pane_snapshot_keys_match(
        &cached,
        [(1, 10, 121, 40), (2, 20, 80, 24)]
    ));
    assert!(!pane_snapshot_keys_match(
        &cached,
        [(2, 20, 80, 24), (1, 10, 120, 40)]
    ));
    assert!(!pane_snapshot_keys_match(&cached, [(1, 10, 120, 40)]));
}

#[test]
fn cached_cursor_blink_lookup_tracks_the_active_snapshot() {
    let keys = [(11, 1, 80, 24), (22, 2, 100, 30)];
    let mut snapshots = [PaneSnapshot::default(), PaneSnapshot::default()];
    snapshots[0].cursor_blinking = true;

    assert_eq!(
        cached_pane_cursor_blinking(Some(11), &keys, &snapshots),
        Some(true)
    );
    assert_eq!(
        cached_pane_cursor_blinking(Some(22), &keys, &snapshots),
        Some(false)
    );
    assert_eq!(
        cached_pane_cursor_blinking(Some(99), &keys, &snapshots),
        None
    );
    assert_eq!(
        cached_pane_cursor_blinking(Some(22), &keys, &snapshots[..1]),
        None
    );

    let live_reads = std::cell::Cell::new(0);
    let blink = pane_cursor_blinking_with(Some(22), &keys, &snapshots, true, |_| {
        live_reads.set(live_reads.get() + 1);
        Some(true)
    });
    assert!(!blink);
    assert_eq!(
        live_reads.get(),
        0,
        "validated snapshot lookup must not fall through to the live Term"
    );
}

/// The early reveal exists to buy back renderer-init time, but only where
/// the pre-paint frame is indistinguishable from what will be painted.
/// A light theme must keep the old hide-until-painted behaviour: revealing
/// it early trades a late window for a BLACK flash, which is the same bad
/// deal the policy was written to avoid.
#[test]
fn only_a_background_indistinguishable_from_black_reveals_early() {
    use kettle_config::Rgb;
    let dark = [
        (Rgb { r: 0, g: 0, b: 0 }, "pure black"),
        (
            Rgb {
                r: 16,
                g: 16,
                b: 16,
            },
            "#101010, the benchmark profile",
        ),
        (
            Rgb {
                r: 24,
                g: 24,
                b: 24,
            },
            "the threshold itself",
        ),
    ];
    for (bg, what) in dark {
        assert_eq!(
            background_is_dark_enough_to_reveal_early(bg),
            cfg!(target_os = "windows"),
            "{what} is imperceptible against black, so it reveals early on                  Windows -- and nowhere else, because that is the only platform                  where the pre-paint frame was measured to be black"
        );
    }
    let keep_hiding = [
        (
            Rgb {
                r: 240,
                g: 232,
                b: 216,
            },
            "a light/solarized background",
        ),
        (
            Rgb {
                r: 255,
                g: 255,
                b: 255,
            },
            "pure white",
        ),
        (
            Rgb {
                r: 25,
                g: 24,
                b: 24,
            },
            "one level past the threshold",
        ),
        (
            Rgb {
                r: 16,
                g: 16,
                b: 120,
            },
            "dark on two channels, blue on one",
        ),
        (
            Rgb {
                r: 40,
                g: 44,
                b: 52,
            },
            "a typical dark-but-visible theme",
        ),
    ];
    for (bg, what) in keep_hiding {
        assert!(
            !background_is_dark_enough_to_reveal_early(bg),
            "{what} would flash black; it must wait for the first paint"
        );
    }
}

#[test]
fn context_menu_snapshot_reuse_rejects_live_pointer_gestures() {
    let mut ws = WindowState::new(1, Mux::new());
    assert!(context_menu_snapshot_reuse_safe(&ws));

    ws.selecting = true;
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.selecting = false;

    ws.scrollbar_drag_offset = Some(0.0);
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.scrollbar_drag_offset = None;

    ws.dragging_split = Some(SplitDrag {
        path: Vec::new(),
        dir: Dir::Horizontal,
    });
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.dragging_split = None;

    ws.held_buttons.pressed(0, Some(1), (0, 0));
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.held_buttons.take(0);

    ws.tab_drag_active = true;
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.tab_drag_active = false;

    ws.tab_drag_press = Some((0.0, 0.0));
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.tab_drag_press = None;

    ws.drag_press = Some((0.0, 0.0));
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.drag_press = None;

    // An ARMED pane drag counts, not just a live one: the press has already
    // claimed the pointer, and the very next CursorMoved can promote it and
    // rewrite the layout under a reused snapshot.
    ws.pane_drag = Some(PaneDrag {
        pane: 1,
        origin: (0.0, 0.0),
        live: false,
        rename_on_click: false,
        target: None,
    });
    assert!(!context_menu_snapshot_reuse_safe(&ws));
    ws.pane_drag = None;

    ws.detach_drag = crate::detach::DragState::on_mouse_down_on_tab(0);
    assert!(!context_menu_snapshot_reuse_safe(&ws));
}

#[test]
fn watcher_accepts_content_and_name_changes_to_exact_path_only() {
    use notify::EventKind;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RemoveKind,
        RenameMode,
    };

    let watched = std::path::Path::new("/tmp/kettle-config.toml");
    let event = |kind| notify::Event::new(kind).add_path(watched.to_path_buf());
    for kind in [
        EventKind::Any,
        EventKind::Create(CreateKind::File),
        EventKind::Modify(ModifyKind::Data(DataChange::Content)),
        EventKind::Modify(ModifyKind::Name(RenameMode::Any)),
        EventKind::Remove(RemoveKind::File),
    ] {
        assert!(super::watcher_event_matches(&event(kind), watched));
    }
    for kind in [
        EventKind::Access(AccessKind::Any),
        EventKind::Access(AccessKind::Read),
        EventKind::Access(AccessKind::Open(AccessMode::Read)),
        EventKind::Access(AccessKind::Close(AccessMode::Read)),
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions)),
        EventKind::Other,
    ] {
        assert!(!super::watcher_event_matches(&event(kind), watched));
    }
    let unrelated = notify::Event::new(EventKind::Modify(ModifyKind::Any))
        .add_path(std::path::PathBuf::from("/tmp/session.json"));
    assert!(!super::watcher_event_matches(&unrelated, watched));
}

#[cfg(target_os = "linux")]
#[test]
fn profile_switches_retarget_live_reload() {
    use std::sync::{Arc, atomic::AtomicBool, mpsc};
    use std::time::Duration;

    let source = production_source();
    let refresh = source
        .split_once("fn refresh_config_watcher(")
        .unwrap()
        .1
        .split_once("/// Read and install")
        .unwrap()
        .0;
    assert!(refresh.contains("self.config_path.clone()"));
    assert!(!refresh.contains("self.startup.config"));
    assert_eq!(source.matches("self.switch_profile(ws, p);").count(), 2);

    let root = kettle_test_support::private_tempdir("kettle-profile-watch-");
    let original = root.join("config");
    let profiles = root.join("profiles");
    std::fs::create_dir(&profiles).unwrap();
    let profile = profiles.join("work.config");
    std::fs::write(&original, "").unwrap();
    std::fs::write(&profile, "").unwrap();
    let (tx, rx) = mpsc::channel();
    let _watcher = super::config_watcher(
        &profile,
        kettle_config::ConfigTrust::VerifyDirectory,
        Arc::new(AtomicBool::new(false)),
        move || tx.send(()).is_ok(),
    )
    .unwrap();
    std::fs::write(&original, "font-size = 13\n").unwrap();
    assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    std::fs::write(&profile, "font-size = 14\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[cfg(unix)]
#[test]
fn config_watch_paths_follow_leaf_symlink_retargets() {
    use std::os::unix::fs::symlink;

    let root = kettle_test_support::private_tempdir("kettle-config-watch-link-");
    let config_dir = root.join("config-dir");
    let dotfiles = root.join("dotfiles");
    std::fs::create_dir(&config_dir).unwrap();
    std::fs::create_dir(&dotfiles).unwrap();
    let first = dotfiles.join("first");
    let second = dotfiles.join("second");
    std::fs::write(&first, "").unwrap();
    std::fs::write(&second, "").unwrap();
    let link = config_dir.join("config");
    symlink(&first, &link).unwrap();
    for target in [&first, &second] {
        let paths = super::config_watch_paths(&link).unwrap();
        assert!(paths.contains(&std::path::absolute(&link).unwrap()));
        assert!(paths.contains(&std::fs::canonicalize(target).unwrap()));
        let event = notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(std::fs::canonicalize(target).unwrap());
        assert!(
            paths
                .iter()
                .any(|path| super::watcher_event_matches(&event, path))
        );
        if target == &first {
            std::fs::remove_file(&link).unwrap();
            symlink(&second, &link).unwrap();
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn symlinked_config_target_events_reload() {
    use std::os::unix::fs::symlink;
    use std::sync::{Arc, atomic::AtomicBool, mpsc};
    use std::time::Duration;

    let root = kettle_test_support::private_tempdir("kettle-config-watch-target-");
    let links = root.join("links");
    let targets = root.join("targets");
    std::fs::create_dir(&links).unwrap();
    std::fs::create_dir(&targets).unwrap();
    let target = targets.join("config");
    let link = links.join("config");
    std::fs::write(&target, "").unwrap();
    symlink(&target, &link).unwrap();
    let (tx, rx) = mpsc::channel();
    let _watcher = super::config_watcher(
        &link,
        kettle_config::ConfigTrust::ExplicitPath,
        Arc::new(AtomicBool::new(false)),
        move || tx.send(()).is_ok(),
    )
    .unwrap();
    std::fs::write(&target, "font-size = 15\n").unwrap();
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn watcher_handle_survives_only_a_successful_registration() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Candidate(Arc<AtomicUsize>);
    impl Drop for Candidate {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    let dropped = Arc::new(AtomicUsize::new(0));
    let kept = super::registered_watcher(Candidate(dropped.clone()), |_| Ok::<_, ()>(()));
    assert!(kept.is_ok());
    assert_eq!(dropped.load(Ordering::Relaxed), 0);
    drop(kept);
    assert_eq!(dropped.load(Ordering::Relaxed), 1);

    let refused = super::registered_watcher(Candidate(dropped.clone()), |_| Err::<(), _>(()));
    assert!(refused.is_err());
    assert_eq!(
        dropped.load(Ordering::Relaxed),
        2,
        "a failed subscription retained its unusable handle"
    );
}

#[test]
fn automatic_and_explicit_lua_paths_keep_their_trust_provenance() {
    let root = kettle_test_support::private_tempdir("kettle-init-lua-source-");
    let default_config = root.join("config");
    let automatic = root.join("init.lua");
    std::fs::write(&automatic, "return true\n").unwrap();

    assert_eq!(
        super::resolved_init_lua_script(None, Some(default_config.clone())),
        Some((
            automatic.clone(),
            kettle_config::ConfigTrust::VerifyDirectory
        ))
    );

    let explicit = root.join("project.lua");
    assert_eq!(
        super::resolved_init_lua_script(Some(explicit.clone()), Some(default_config),),
        Some((explicit, kettle_config::ConfigTrust::ExplicitPath)),
        "an explicit path wins even when the automatic script exists"
    );
}

#[test]
fn watcher_gate_coalesces_rearms_and_recovers_from_send_failure() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let pending = AtomicBool::new(false);
    let sends = AtomicUsize::new(0);
    assert!(super::queue_watcher_event(&pending, || {
        sends.fetch_add(1, Ordering::Relaxed);
        true
    }));
    assert!(!super::queue_watcher_event(&pending, || {
        sends.fetch_add(1, Ordering::Relaxed);
        true
    }));
    assert_eq!(sends.load(Ordering::Relaxed), 1);

    pending.store(false, Ordering::Release);
    assert!(!super::queue_watcher_event(&pending, || false));
    assert!(!pending.load(Ordering::Acquire));
    assert!(super::queue_watcher_event(&pending, || true));
}

#[test]
fn watcher_gate_allows_only_one_racing_sender() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    let pending = Arc::new(AtomicBool::new(false));
    let sends = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let pending = pending.clone();
            let sends = sends.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                super::queue_watcher_event(&pending, || {
                    sends.fetch_add(1, Ordering::Relaxed);
                    true
                });
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(sends.load(Ordering::Relaxed), 1);
}

#[test]
fn config_reload_font_policy_preserves_runtime_zoom() {
    assert_eq!(super::reloaded_font_size(false, 13.0, Some(19.0)), 19.0);
    assert_eq!(super::reloaded_font_size(false, 13.0, None), 13.0);
    assert_eq!(super::reloaded_font_size(true, 15.0, Some(19.0)), 15.0);
    assert_eq!(
        super::CONFIG_RELOAD_DEBOUNCE,
        std::time::Duration::from_millis(75)
    );
}

#[test]
fn config_reload_synchronizes_open_search_preferences() {
    let now = std::time::Instant::now();
    let mut search = crate::search_input::SearchState {
        open: true,
        editor: crate::search_input::SearchEditor::from_text("(".into(), 4096),
        compiled_revision: Some(0),
        status: kettle_render::SearchStatus::Invalid,
        ..crate::search_input::SearchState::default()
    };

    assert!(super::sync_reloaded_search_preferences(
        &mut search,
        false,
        kettle_config::SearchCaseSensitivity::Smart,
        true,
        now,
    ));
    assert!(!search.wrap);
    assert!(search.invert);
    assert_eq!(search.status, kettle_render::SearchStatus::Invalid);
    assert!(search.unlimited_retry_at.is_none());

    assert!(!super::sync_reloaded_search_preferences(
        &mut search,
        false,
        kettle_config::SearchCaseSensitivity::Always,
        true,
        now,
    ));
    assert_eq!(
        search.case_mode,
        kettle_config::SearchCaseSensitivity::Always
    );
    assert_eq!(search.status, kettle_render::SearchStatus::Searching);
    assert_eq!(search.revision, 1);
}

#[test]
fn visible_projection_limit_is_derived_and_clears_with_the_viewport() {
    let mut search = crate::search_input::SearchState {
        status: kettle_render::SearchStatus::Match,
        visible_truncated: true,
        ..crate::search_input::SearchState::default()
    };
    assert_eq!(
        super::effective_search_status(&search),
        kettle_render::SearchStatus::Limited
    );
    search.visible_truncated = false;
    assert_eq!(
        super::effective_search_status(&search),
        kettle_render::SearchStatus::Match
    );
    search.status = kettle_render::SearchStatus::Limited;
    assert_eq!(
        super::effective_search_status(&search),
        kettle_render::SearchStatus::Limited
    );
}

#[test]
fn output_continuation_keeps_progress_and_defers_a_definitive_miss() {
    let token = kettle_core::SearchScanToken {
        query_revision: 3,
        output_generation: 9,
        layout: kettle_core::SearchLayout {
            columns: 80,
            screen_lines: 24,
            history_size: 10_000,
        },
    };
    let mut job = crate::search_input::BackgroundSearch {
        token: kettle_core::SearchScanToken {
            output_generation: 8,
            ..token
        },
        direction: kettle_core::SearchDirection::Forward,
        cursor: kettle_core::SearchPoint::new(-4_000, 0),
        edge: kettle_core::SearchPoint::new(23, 79),
        wrap_anchor: kettle_core::SearchPoint::new(0, 0),
        wrapped: false,
        nearby: false,
        navigation: false,
        had_focus: false,
        output_drifted: false,
    };
    super::continue_background_after_output(&mut job, token, -10_000, 23, 79);
    assert_eq!(job.cursor, kettle_core::SearchPoint::new(-4_000, 0));
    assert_eq!(job.edge, kettle_core::SearchPoint::new(23, 79));
    assert_eq!(job.token, token);
    assert!(job.output_drifted);

    let mut nearby_job = job;
    nearby_job.nearby = true;
    let mut nearby_search = crate::search_input::SearchState {
        revision: 3,
        background: Some(nearby_job),
        ..crate::search_input::SearchState::default()
    };
    assert!(!super::should_start_nearby_search(&nearby_search));
    nearby_search.background.as_mut().unwrap().nearby = false;
    assert!(!super::should_start_nearby_search(&nearby_search));
    nearby_search.background = None;
    assert!(super::should_start_nearby_search(&nearby_search));
    nearby_search.typing_scanned_revision = Some(3);
    assert!(!super::should_start_nearby_search(&nearby_search));

    let now = std::time::Instant::now();
    nearby_job.output_drifted = false;
    nearby_search.typing_scanned_revision = None;
    nearby_search.unlimited_retry_at = Some(now);
    assert!(!super::prepare_initial_search_retry_after_output(
        &mut nearby_search,
        &nearby_job,
        nearby_job.wrap_anchor,
    ));
    assert_eq!(nearby_search.typing_scanned_revision, Some(3));
    assert!(nearby_search.unlimited_retry_at.is_none());
    assert!(!super::should_start_nearby_search(&nearby_search));

    nearby_job.output_drifted = true;
    let retry_at = now + std::time::Duration::from_millis(500);
    nearby_search.unlimited_retry_at = Some(retry_at);
    assert!(super::prepare_initial_search_retry_after_output(
        &mut nearby_search,
        &nearby_job,
        nearby_job.wrap_anchor,
    ));
    assert!(nearby_search.quiet_retry_pending);
    assert_eq!(nearby_search.unlimited_retry_at, Some(retry_at));
    assert!(!super::should_start_nearby_search(&nearby_search));

    let (status, retry_at) = super::finished_background_search(&job, std::time::Instant::now());
    assert_eq!(status, kettle_render::SearchStatus::Searching);
    assert!(retry_at.is_some());

    job.navigation = true;
    job.direction = kettle_core::SearchDirection::Reverse;
    job.had_focus = true;
    assert!(!super::should_retry_output_drift(&job));
    let original_anchor = kettle_core::SearchPoint::new(-99, 7);
    let mut search = crate::search_input::SearchState {
        anchor: Some(original_anchor),
        typing_scanned_revision: Some(3),
        ..crate::search_input::SearchState::default()
    };
    assert!(!super::prepare_initial_search_retry_after_output(
        &mut search,
        &job,
        kettle_core::SearchPoint::new(0, 0),
    ));
    assert_eq!(search.anchor, Some(original_anchor));
    assert_eq!(search.typing_scanned_revision, Some(3));
    let (status, retry_at) = super::finished_background_search(&job, std::time::Instant::now());
    assert_eq!(status, kettle_render::SearchStatus::Limited);
    assert!(retry_at.is_none());

    job.navigation = false;
    let retry_anchor = kettle_core::SearchPoint::new(-20, 0);
    assert!(super::prepare_initial_search_retry_after_output(
        &mut search,
        &job,
        retry_anchor,
    ));
    assert_eq!(search.anchor, Some(retry_anchor));
    assert_eq!(search.typing_scanned_revision, Some(3));
    assert!(search.quiet_retry_pending);
    search.unlimited_retry_at = Some(std::time::Instant::now());
    assert!(super::activate_quiet_search_retry(&mut search));
    assert!(!search.quiet_retry_pending);
    assert!(search.typing_scanned_revision.is_none());
    // The nearby scan goes first, and the full scan is already due behind
    // it, so a nearby miss cannot leave the search stuck at `Searching`.
    assert!(super::should_start_nearby_search(&search));
    assert!(
        search
            .unlimited_retry_at
            .is_some_and(|deadline| deadline <= std::time::Instant::now())
    );

    search.quiet_retry_pending = true;
    search.unlimited_retry_at = Some(std::time::Instant::now());
    super::cancel_deferred_search_retry(&mut search);
    assert!(!search.quiet_retry_pending);
    assert!(search.unlimited_retry_at.is_none());

    job.navigation = true;
    job.output_drifted = false;
    let (status, retry_at) = super::finished_background_search(&job, std::time::Instant::now());
    assert_eq!(status, kettle_render::SearchStatus::Start);
    assert!(retry_at.is_none());
}

/// Turning updates off in Settings, or any other reload, refreshes the update
/// notice at once. It once waited for the next focus change or hourly tick,
/// so a focused window kept its chip for up to an hour.
#[test]
fn a_config_reload_refreshes_the_update_notice() {
    let src = production_source();
    let body = src
        .split("fn load_reloaded_config(&mut self) -> bool {")
        .nth(1)
        .and_then(|body| body.split("\n    }\n").next())
        .expect("load_reloaded_config body");
    let assigned = body
        .find("self.cfg = new;")
        .expect("the new config is applied");
    let refreshed = body
        .find("self.request_update_facts(true);")
        .expect("a reload refreshes the update notice");
    assert!(assigned < refreshed, "facts are read with the new policy");
}

#[test]
fn watched_reload_loads_once_then_applies_every_window() {
    let src = production_source();
    let body = src
        .split("fn reload_config_windows(&mut self)")
        .nth(1)
        .and_then(|body| body.split("/// Scan every pane's recent output").next())
        .expect("reload_config_windows body");
    assert_eq!(body.matches("self.load_reloaded_config()").count(), 1);
    assert!(body.contains("self.windows.keys().copied().collect()"));
    assert!(body.contains("self.apply_reloaded_config(&mut ws"));
}

#[test]
fn accessibility_initial_tree_is_complete_and_pane_ids_are_stable() {
    let update = App::initial_accessibility_tree(&kettle_i18n::Translator::default());
    assert_eq!(update.focus, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(update.nodes.len(), 1);
    let (root_id, root) = &update.nodes[0];
    assert_eq!(*root_id, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(root.role(), accesskit::Role::Window);
    assert_eq!(root.label(), Some("Kettle terminal"));
    let es =
        App::initial_accessibility_tree(&kettle_i18n::Translator::new(kettle_i18n::Language::Es));
    assert_eq!(es.nodes[0].1.label(), Some("Terminal Kettle"));

    let tree = update.tree.expect("initial update must establish a tree");
    assert_eq!(tree.root, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(tree.toolkit_name.as_deref(), Some("Kettle"));
    assert_eq!(
        tree.toolkit_version.as_deref(),
        Some(env!("CARGO_PKG_VERSION"))
    );

    let first = super::accessibility_pane_id(1);
    let text = super::accessibility_text_id(1);
    assert_ne!(first, super::ACCESSIBILITY_ROOT_ID);
    assert_eq!(first, super::accessibility_pane_id(1));
    assert_ne!(first, super::accessibility_pane_id(2));
    assert_ne!(first, text);
    assert_eq!(text, super::accessibility_text_id(1));
}

#[test]
fn confirm_dialog_accessibility_focuses_the_selected_button() {
    let overlay = kettle_render::Overlay {
        confirm_dialog: Some(kettle_render::ConfirmDialogOverlay {
            prompt: "Close 3 running panes?".to_string(),
            buttons: vec![
                kettle_render::ConfirmDialogButton {
                    label: "Cancel".to_string(),
                    destructive: false,
                },
                kettle_render::ConfirmDialogButton {
                    label: "Close".to_string(),
                    destructive: true,
                },
            ],
            focus_idx: 1,
        }),
        ..kettle_render::Overlay::default()
    };

    let projection = super::modal_accessibility_projection(
        &overlay,
        (800.0, 600.0),
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    let dialog = projection
        .nodes
        .iter()
        .find(|(_, node)| node.role() == accesskit::Role::AlertDialog)
        .expect("confirm dialog node");
    assert_eq!(dialog.1.label(), Some("Close 3 running panes?"));
    let focused = projection
        .nodes
        .iter()
        .find(|(id, _)| *id == projection.focus.expect("modal focus"))
        .expect("focused modal node");
    assert_eq!(focused.1.role(), accesskit::Role::Button);
    assert_eq!(focused.1.label(), Some("Close"));
}

#[test]
fn context_menu_accessibility_exposes_actions_and_validates_targets() {
    use super::{ContextMenuItem, ContextMenuState, accessibility_context_menu_item_index};
    let mut ws = WindowState::new(1, Mux::new());
    ws.context_menu = Some(ContextMenuState {
        anchor: (0.0, 0.0),
        anchor_scale: 1.0,
        items: vec![
            ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: true,
            },
            ContextMenuItem::Separator,
            ContextMenuItem::Item {
                label: "Paste",
                action: Action::Paste,
                enabled: false,
            },
            ContextMenuItem::Submenu {
                label: "Theme".into(),
                items: Vec::new(),
            },
        ],
        highlight: 0,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        scroll_stack: Vec::new(),
        typeahead_buf: String::new(),
        typeahead_until: None,
    });
    let overlay = kettle_render::Overlay {
        context_menu: Some(kettle_render::ContextMenu {
            anchor: (0.0, 0.0),
            a11y_label: None,
            rows: [
                ("Copy", false, true),
                ("", true, false),
                ("Paste", false, false),
                ("Theme", false, true),
            ]
            .into_iter()
            .map(
                |(label, separator, enabled)| kettle_render::ContextMenuRow {
                    label: label.into(),
                    separator,
                    enabled,
                    hint: String::new(),
                },
            )
            .collect(),
            highlight: 0,
            scroll_offset: 0,
            panel_w_clamped: 200.0,
            panel_h_clamped: 120.0,
        }),
        ..kettle_render::Overlay::default()
    };
    let projection = super::modal_accessibility_projection(&overlay, (800.0, 600.0), &EN_TEXT);
    for index in [0, 2, 3] {
        let id = super::accessibility_modal_id(2, index + 1);
        let (_, node) = projection.nodes.iter().find(|(at, _)| *at == id).unwrap();
        let enabled = index != 2;
        assert_eq!(node.supports_action(accesskit::Action::Focus), enabled);
        assert_eq!(node.supports_action(accesskit::Action::Click), enabled);
        assert_eq!(
            accessibility_context_menu_item_index(id, &ws),
            enabled.then_some(index)
        );
    }
    for id in [
        super::accessibility_modal_id(2, 0),
        super::accessibility_modal_id(2, 2),
        super::accessibility_modal_id(2, 5),
        super::accessibility_modal_id(1, 1),
        super::accessibility_modal_id(3, 1),
    ] {
        assert_eq!(accessibility_context_menu_item_index(id, &ws), None);
    }
    let copy = super::accessibility_modal_id(2, 1);
    ws.palette_input = Some((String::new(), 0));
    assert_eq!(accessibility_context_menu_item_index(copy, &ws), None);
    ws.palette_input = None;
    ws.context_menu = None;
    assert_eq!(accessibility_context_menu_item_index(copy, &ws), None);

    let source = super::production_source();
    let handler = source
        .split_once("fn handle_accessibility_action(")
        .unwrap()
        .1;
    let menu = handler
        .split_once("accessibility_context_menu_item_index(")
        .unwrap()
        .1
        .split("ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID")
        .next()
        .unwrap();
    assert!(menu.contains("self.set_context_menu_highlight(ws, index)"));
    assert!(menu.contains("self.context_menu_activate(ws, event_loop)"));
}

#[test]
fn confirm_dialog_accessibility_button_ids_round_trip() {
    for index in 0..3 {
        let id = super::accessibility_modal_id(1, index + 1);
        assert_eq!(super::accessibility_confirm_button_index(id), Some(index));
    }
    assert_eq!(
        super::accessibility_confirm_button_index(super::accessibility_modal_id(1, 0)),
        None
    );
    assert_eq!(
        super::accessibility_confirm_button_index(super::accessibility_modal_id(5, 2)),
        None
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

#[test]
fn layout_picker_reads_the_directory_only_when_it_opens() {
    let src = production_source();
    let overlay = src
        .split("fn overlay(")
        .nth(1)
        .and_then(|rest| rest.split("fn redraw(").next())
        .expect("overlay body");
    assert!(!overlay.contains("Session::list_layouts()"));
    assert!(src.contains("ws.layout_picker_entries = crate::session::Session::list_layouts();"));
}

#[test]
fn ui_geometry_exposes_modal_accessibility_and_settings_scroll_without_values() {
    let src = production_source();
    let body = src
        .split("fn ctl_ui_geometry(")
        .nth(1)
        .and_then(|rest| rest.split("fn ctl_perform_action(").next())
        .expect("ui_geometry body");
    for needle in [
        "kettle_render::settings_panel_geometry",
        "\"first_line\":",
        "\"visible_lines\":",
        "\"focused_rect\":",
        "\"accessibility\": accessibility",
        "\"render_uploads\": target.renderer.as_ref().map(",
        "\"frames_presented\": u.frames_presented",
        "\"buffer_writes\": u.buffer_writes",
        "\"text_prepares\": u.text_prepares",
        "\"cursor_blink\": self.cursor_blink_diagnostics(target),",
        "\"renderer\": if layer.is_active() { \"layer\" } else { \"gpu\" },",
        "\"layer_rect\":",
        "\"phase_on\": phase_on,",
        "\"next_edge_ms\":",
        "\"handoffs\": layer.handoffs,",
        "\"exits\": layer.exits,",
        "\"mapped_uploads\": u.mapped_uploads",
        "\"mapped_writes\": u.mapped_writes",
        "\"chrome_prepares\": u.chrome_prepares",
    ] {
        assert!(body.contains(needle), "ui_geometry lost {needle:?}");
    }
    assert!(
        !body.contains("\"value\":"),
        "settings diagnostics must not expose config values"
    );
}

#[test]
fn about_never_claims_current_without_a_verified_result() {
    // No record and no install facts: About says the status is unknown
    // rather than up to date.
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    assert!(
        crate::update_notice::fact_rows(&crate::update_notice::UpdateFacts::default(), &tr)
            .is_empty()
    );
    assert_eq!(
        tr.text(kettle_i18n::Text::AboutUpdateUnknown),
        "Update status unknown"
    );
}

#[test]
fn inline_placement_classes_are_interleaved_before_frame_budget() {
    assert_eq!(
        super::round_robin_bounded([vec![1, 2, 3], vec![10, 11, 12], vec![20, 21, 22]], 5),
        vec![1, 10, 20, 2, 11]
    );
    assert_eq!(
        super::round_robin_bounded([vec![1, 2], vec![], vec![20]], 99),
        vec![1, 20, 2]
    );
    assert!(super::round_robin_bounded([vec![1], vec![2], vec![3]], 0).is_empty());
}

/// An OSC-set title is neutralized before it reaches the OS titlebar, tab,
/// or status bar. Control chars and Unicode bidi-override format chars
/// become spaces, and the length is capped.
#[test]
fn sanitize_title_neutralizes_control_bidi_and_caps_length() {
    // C0/C1 control chars -> spaces.
    assert_eq!(sanitize_title("a\x07b\x1b\nc"), "a b  c");
    // U+202E (RLO) and friends -> spaces (Alt-Tab / titlebar spoofing).
    assert_eq!(sanitize_title("safe\u{202e}evil"), "safe evil");
    assert_eq!(sanitize_title("\u{2066}\u{2069}x"), "  x");
    // Plain text is untouched.
    assert_eq!(
        sanitize_title("~/projects/kettle — main"),
        "~/projects/kettle — main"
    );
    // Length is capped at 512 chars.
    let long = "x".repeat(5000);
    assert_eq!(sanitize_title(&long).chars().count(), 512);
}

/// `poll_remote_contexts` must sanitize
/// `kettle_remote::format_remote_title`'s output exactly like the OSC-2
/// `Title` path does. The host, user, and container strings it
/// interpolates come verbatim from a scanned process's argv, as untrusted
/// as an OSC-set title. Without this, a maliciously named SSH host or
/// container could smuggle bidi overrides or control characters into the
/// OS titlebar, Alt-Tab, the tab label, and the accessibility tree.
#[test]
fn remote_title_is_sanitized_like_osc_titles() {
    let evil_host = kettle_remote::RemoteContext::Ssh {
        host: "safe\u{202e}evil".to_string(),
        user: None,
        options: kettle_remote::SshOptions::default(),
    };
    let raw = kettle_remote::format_remote_title(&evil_host);
    assert!(
        raw.contains('\u{202e}'),
        "sanity: format_remote_title itself does no filtering"
    );
    let sanitized = sanitize_title(&raw);
    assert!(
        !sanitized.contains('\u{202e}') && !sanitized.chars().any(|c| c.is_control()),
        "sanitize_title(format_remote_title(..)) must neutralize bidi/control chars: {sanitized:?}"
    );

    let evil_container = kettle_remote::RemoteContext::Container {
        runtime: kettle_remote::ContainerRuntime::Docker,
        container: "evil\x1b]0;pwned\x07".to_string(),
        options: kettle_remote::ContainerOptions::default(),
    };
    let sanitized = sanitize_title(&kettle_remote::format_remote_title(&evil_container));
    assert!(
        !sanitized.chars().any(|c| c.is_control()),
        "container-name control chars must be neutralized: {sanitized:?}"
    );
}

#[test]
fn remote_generated_title_restores_its_prior_origin_on_exit() {
    let first = kettle_remote::RemoteContext::Ssh {
        host: "one.example".to_string(),
        user: Some("dev".to_string()),
        options: kettle_remote::SshOptions::default(),
    };
    let second = kettle_remote::RemoteContext::Ssh {
        host: "two.example".to_string(),
        user: None,
        options: kettle_remote::SshOptions::default(),
    };
    let mut title = "kettle".to_string();
    let mut placeholder = true;
    let mut origin = PaneTitleOrigin::Placeholder;
    let mut before_remote = None;

    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some(&first),
    );
    assert_eq!(origin, PaneTitleOrigin::Remote);
    assert!(!placeholder);
    assert_eq!(
        before_remote,
        Some(("kettle".to_string(), PaneTitleOrigin::Placeholder))
    );

    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some(&second),
    );
    assert_eq!(
        before_remote,
        Some(("kettle".to_string(), PaneTitleOrigin::Placeholder)),
        "remote-to-remote changes must not overwrite the local fallback"
    );

    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        None,
    );
    assert_eq!(title, "kettle");
    assert!(placeholder);
    assert_eq!(origin, PaneTitleOrigin::Placeholder);
    assert!(before_remote.is_none());

    title = "nvim project".to_string();
    placeholder = false;
    origin = PaneTitleOrigin::Osc;
    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        None,
    );
    assert_eq!(title, "nvim project", "non-remote OSC titles are retained");
    assert_eq!(origin, PaneTitleOrigin::Osc);
}

/// Native OS titlebars do not reliably use kettle's terminal font fallback,
/// so Private Use / Nerd Font glyphs are stripped there only.
#[test]
fn native_window_title_strips_private_use_glyphs_only() {
    assert_eq!(
        sanitize_native_window_title("\u{f015} kevim — kettle"),
        "kevim — kettle"
    );
    assert_eq!(
        sanitize_native_window_title("\u{f120} SPI-1 — kettle"),
        "SPI-1 — kettle"
    );
    assert_eq!(
        sanitize_native_window_title("\u{e795} projects \u{e0b0} kettle"),
        "projects  kettle"
    );
    assert_eq!(
        sanitize_native_window_title("\u{f0000} build \u{100000} — kettle"),
        "build  — kettle"
    );
    assert_eq!(
        sanitize_native_window_title("世界 😀 — kettle"),
        "世界 😀 — kettle"
    );
    assert_eq!(sanitize_native_window_title("\u{f015}\u{e0b0}"), "kettle");
}

/// Source-drift guard: the `WindowEvent::Focused(f)` arm must sync
/// `focused_seq` to the focused window, or window-less events (ctl /
/// remote / update banner) misroute to a stale window.
#[test]
fn focused_event_updates_focused_seq() {
    let src = production_source();
    let arm = src
        .find("WindowEvent::Focused(f) =>")
        .expect("Focused arm present");
    let body = &src[arm..arm + 1100];
    assert!(
        body.contains("self.focused_seq = ws.seq"),
        "the Focused arm must set self.focused_seq = ws.seq when focused"
    );
}

#[test]
fn software_recovery_title_notice_is_translated() {
    use kettle_i18n::{Language, Text, Translator};

    let src = production_source();
    let desired = src
        .split("fn desired_window_title(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .unwrap();
    // rustfmt may split the call across lines; compare without whitespace.
    let compact: String = desired.split_whitespace().collect();
    assert!(compact.contains("self.ui_text.text(kettle_i18n::Text::WindowTitleSoftwareFallback)"));
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    let key = Text::ALL
        .iter()
        .copied()
        .find(|key| en.text(*key) == "software rendering (GPU unavailable)")
        .expect("software fallback notice is in the typed catalogue");
    assert_eq!(es.text(key), "renderizado por software (GPU no disponible)");
}

/// Source-drift guard: while the GPU device is lost the redraw guard must
/// reset the output pacer + snapshot `seen_output_gen` so a streaming pane
/// can't spin the loop. Recovery itself is driven from `about_to_wait_inner`
/// on a bounded backoff.
#[test]
fn gpu_lost_quiesces_and_schedules_recovery() {
    let src = production_source();
    let guard = src
        .find("if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {")
        .expect("gpu_lost redraw guard present");
    let body = &src[guard..guard + 1400];
    assert!(
        body.contains("ws.output_pacer.reset()") && body.contains("ws.seen_output_gen.clear()"),
        "the gpu_lost redraw guard must reset pacing + snapshot seen_output_gen"
    );
    let wait = src
        .find("fn about_to_wait_inner(")
        .expect("about_to_wait_inner present");
    let wait_body = &src[wait..];
    // about_to_wait_inner is the last method in the impl, so the next
    // column-0 `}` closes the impl (excludes the test module below).
    let end = wait_body
        .find("\n}\n")
        .map(|i| i + 2)
        .unwrap_or(wait_body.len());
    assert!(
        wait_body[..end].contains("self.try_recover_gpu(ws,")
            && wait_body[..end].contains("gpu_recovery_backoff("),
        "about_to_wait_inner must drive GPU recovery while the device is lost"
    );
}

#[test]
fn live_gpu_context_uses_event_loop_owned_display_handle() {
    let src = production_source();
    let resumed = src
        .find("fn resumed_inner(")
        .expect("resumed_inner present");
    let resumed_tail = &src[resumed..];
    let resumed_body = &resumed_tail[..resumed_tail
        .find("\n    fn ")
        .expect("end of resumed_inner")];
    assert!(
        resumed_body.contains("Renderer::new_with_display_handle(")
            && resumed_body.contains("event_loop.owned_display_handle()"),
        "initial GPU creation must retain the event-loop display connection, not window 1"
    );

    let recovery = src
        .find("fn try_recover_gpu(")
        .expect("try_recover_gpu present");
    let recovery_tail = &src[recovery..];
    let recovery_end = recovery_tail
        .find("\n    }\n")
        .expect("try_recover_gpu end");
    let recovery_body = &recovery_tail[..recovery_end];
    assert!(
        recovery_body.contains("Renderer::new_with_escalation_and_display_handle(")
            && recovery_body.contains("event_loop.owned_display_handle()"),
        "GPU recovery must rebuild with the event-loop display connection"
    );
    assert!(
        recovery_body.contains("Self::stash_renderer_for_gpu_recovery(ws)")
            && recovery_body.contains("Self::stash_renderer_for_gpu_recovery(other)")
            && recovery_body.matches("restore_recovery_state(").count() == 2
            && recovery_body
                .matches("finish_gpu_renderer_recovery(")
                .count()
                == 2,
        "GPU recovery must retain, restore, and reflow every window around the atomic rebuild"
    );
}

/// The App owns each window's accent and may change it while the window
/// has no renderer (a config reload or a tear-off during GPU recovery), so
/// every renderer that replaces a window's renderer must be given it. A
/// renderer-held copy restored from before the change would draw the old
/// accent until the next config change.
#[test]
fn every_replacement_renderer_receives_the_window_accent() {
    let src = production_source();
    let body = |name: &str| {
        let start = src.find(name).unwrap_or_else(|| panic!("{name} present"));
        let tail = &src[start..];
        let end = tail[1..]
            .find("\n    fn ")
            .map_or(tail.len(), |end| end + 1);
        tail[..end].to_owned()
    };
    let finish = body("fn finish_gpu_renderer_recovery(");
    assert!(
        finish.contains("crate::window_accent::apply_accent(ws);"),
        "GPU recovery must apply each window's current accent claim"
    );
    let surface = body("fn try_recover_window_renderer(");
    let installed = surface
        .find("ws.renderer = Some(replacement);")
        .expect("surface recovery installs its replacement");
    assert!(
        surface[installed..].contains("crate::window_accent::apply_accent(ws);"),
        "surface recovery must apply the window's current accent claim"
    );
    // Every recovery path that installs a renderer finishes through one of
    // the two functions above.
    let recover = body("fn try_recover_gpu(");
    assert_eq!(
        recover.matches(".renderer = Some(").count(),
        2,
        "try_recover_gpu installs the primary and secondary renderers"
    );
    assert_eq!(recover.matches("finish_gpu_renderer_recovery(").count(), 2);
}

#[test]
fn gpu_recovery_backoff_grows_and_caps() {
    let ms = |attempt| super::gpu_recovery_backoff(attempt).as_millis();
    assert_eq!(ms(1), 500);
    assert_eq!(ms(2), 1000);
    assert_eq!(ms(3), 2000);
    assert_eq!(ms(4), 4000);
    assert_eq!(ms(50), 4000);
    for attempt in 1..20 {
        assert!(super::gpu_recovery_backoff(attempt) <= super::gpu_recovery_backoff(attempt + 1));
    }
}

#[test]
fn gpu_recovery_set_is_all_or_nothing_with_injected_factory() {
    let mut calls = Vec::new();
    let result = super::build_recovery_set([(2, "a"), (3, "b"), (4, "c")], |value| {
        calls.push(value);
        if value == "b" {
            Err("unsupported surface")
        } else {
            Ok(value.to_ascii_uppercase())
        }
    });

    assert_eq!(calls, ["a", "b", "c"]);
    assert_eq!(result, Err(vec![(3, "unsupported surface")]));

    let rebuilt = super::build_recovery_set([(2, 20), (3, 30)], Ok::<_, ()>).unwrap();
    assert_eq!(rebuilt, [(2, 20), (3, 30)]);
}

#[test]
fn gpu_recovery_snapshot_survives_failed_attempts_until_commit() {
    let mut retained = None;
    super::retain_renderer_recovery_snapshot(&mut retained, Some("live-font-zoom"));
    assert_eq!(retained, Some("live-font-zoom"));

    // Failed candidates cannot replace the state captured from the last
    // live renderer, even after that renderer has been dropped.
    super::retain_renderer_recovery_snapshot(&mut retained, None);
    assert_eq!(retained, Some("live-font-zoom"));
    super::retain_renderer_recovery_snapshot(&mut retained, Some("candidate-defaults"));
    assert_eq!(retained, Some("live-font-zoom"));

    // Successful commit is the only point that clears the retained state.
    retained = None;
    super::retain_renderer_recovery_snapshot(&mut retained, Some("post-recovery-live"));
    assert_eq!(retained, Some("post-recovery-live"));
}

#[test]
fn output_wakeup_quiesces_for_every_render_hidden_state_and_repair() {
    let hidden = |occluded, minimized, shown, visible| {
        super::render_hidden_from_observations(occluded, minimized, shown, visible)
    };

    assert!(!hidden(false, false, true, true));
    assert!(hidden(true, false, true, true), "occluded");
    assert!(hidden(false, true, true, true), "minimized");
    assert!(hidden(false, false, true, false), "explicitly invisible");
    assert!(
        !hidden(false, false, false, false),
        "startup windows intentionally begin invisible before first reveal"
    );

    for render_hidden in [
        hidden(true, false, true, true),
        hidden(false, true, true, true),
        hidden(false, false, true, false),
    ] {
        assert!(super::output_wakeup_must_quiesce(
            render_hidden,
            false,
            false
        ));
    }
    assert!(super::output_wakeup_must_quiesce(false, true, false));
    assert!(
        super::output_wakeup_must_quiesce(false, false, true),
        "renderer/GPU unavailable"
    );
    assert!(!super::output_wakeup_must_quiesce(false, false, false));
}

#[test]
fn an_explicit_screenshot_overrides_only_transient_surface_guards() {
    let allowed = super::pending_screenshot_may_render_offscreen;

    assert!(allowed(true, Some(false), true, Some(true)));
    assert!(!allowed(false, Some(false), true, Some(true)), "no request");
    assert!(!allowed(true, Some(true), true, Some(true)), "minimized");
    assert!(
        !allowed(true, Some(false), false, Some(true)),
        "not yet shown"
    );
    assert!(
        !allowed(true, Some(false), true, Some(false)),
        "explicitly hidden"
    );
    assert!(
        allowed(true, None, true, None),
        "Wayland cannot report these states, so a shown window remains eligible for bounded capture"
    );

    let available = super::screenshot_window_accepts_control_capture;
    assert!(available(Some(false), true, Some(true)));
    assert!(!available(Some(true), true, Some(true)));
    assert!(!available(Some(false), false, Some(true)));
    assert!(!available(Some(false), true, Some(false)));
    assert!(available(None, true, None));
    assert_eq!(
        super::screenshot_window_availability(None, true, None),
        super::ScreenshotWindowAvailability::Unknown
    );
}

#[test]
fn control_screenshot_rejects_hidden_targets_and_cancels_timeouts() {
    let src = production_source();
    let body = src
        .split("fn ctl_screenshot(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// `send_text`").next())
        .expect("ctl_screenshot body");
    let availability = body
        .find("target_accepts_capture")
        .expect("target renderability must be checked");
    let queue = body
        .find("renderer.set_pending_screenshot")
        .expect("screenshot queue site");
    assert!(
        availability < queue,
        "a hidden/minimized target must be rejected before it can strand a pending request"
    );
    assert!(
        body.contains("RecvTimeoutError::Timeout")
            && body.contains("if cancellation.cancel()")
            && body.contains("done_rx.recv_timeout(std::time::Duration::from_secs(5))")
            && !body.contains("done_rx.recv()")
            && body.contains("target window is minimized, hidden, or not yet shown"),
        "timeouts must either win cancellation or bound the wait for an already-committed result, and unavailable targets must receive a prompt error"
    );
}

#[test]
fn control_screenshot_requires_a_visible_pane_crop() {
    let src = production_source();
    let body = src
        .split("fn ctl_screenshot(")
        .nth(1)
        .expect("ctl_screenshot body");
    let guard = body
        .find("if !full_window && crop.is_none() {")
        .expect("pane capture must reject a missing visible rectangle");
    let path = body.find("let (path, output_policy)").unwrap();
    let queue = body.find("renderer.set_pending_screenshot").unwrap();
    assert!(guard < path && path < queue);
    let rejection = &body[guard..path];
    assert!(rejection.contains("ec::BUSY"));
    assert!(rejection.contains("Text::CtlScreenshotPaneNotVisible"));
    assert!(rejection.contains("return;"));
}

#[test]
fn explicit_screenshot_crop_is_all_or_nothing_and_bounded() {
    assert_eq!(super::screenshot_crop(&serde_json::json!({})), Ok(None));
    assert_eq!(
        super::screenshot_crop(&serde_json::json!({
            "crop_x": 4,
            "crop_y": 5.5,
            "crop_width": 120,
            "crop_height": 36,
        })),
        Ok(Some((4.0, 5.5, 120.0, 36.0)))
    );
    for invalid in [
        serde_json::json!({"crop_x": 1}),
        serde_json::json!({
            "crop_x": -1,
            "crop_y": 0,
            "crop_width": 1,
            "crop_height": 1,
        }),
        serde_json::json!({
            "crop_x": 0,
            "crop_y": 0,
            "crop_width": 0,
            "crop_height": 1,
        }),
        serde_json::json!({
            "crop_x": "0",
            "crop_y": 0,
            "crop_width": 1,
            "crop_height": 1,
        }),
    ] {
        assert!(super::screenshot_crop(&invalid).is_err(), "{invalid}");
    }

    let source = production_source();
    let body = source
        .split("fn ctl_screenshot(")
        .nth(1)
        .and_then(|rest| rest.split("\n    /// `send_text`").next())
        .expect("ctl_screenshot body");
    assert!(
        body.contains("let crop = if let Some(crop) = explicit_crop")
            && body.contains("ScreenshotRequest {")
            && body.contains("explicit crop cannot be combined with pane")
            && body.contains("explicit crop cannot be combined with full_window=true"),
        "the parsed crop must reach the renderer without broadening its scope"
    );
}

#[test]
fn hidden_output_sidechannels_keep_transport_wakes_without_enabling_paints() {
    assert!(!super::output_wake_transport_enabled(true, false));
    assert!(super::output_wake_transport_enabled(true, true));
    assert!(super::output_wake_transport_enabled(false, false));
    assert!(super::output_wake_transport_enabled(false, true));

    let wakes = Arc::new(AtomicUsize::new(0));
    let wakes_for_gate = wakes.clone();
    let gate = OutputWakeGate::new(Arc::new(move || {
        wakes_for_gate.fetch_add(1, Ordering::SeqCst);
    }));
    gate.set_enabled(super::output_wake_transport_enabled(true, true));
    for expected in 1..=32 {
        assert!(gate.request());
        assert_eq!(wakes.load(Ordering::SeqCst), expected);
        // Model the hidden-window handler's rearm-before-drain order:
        // every later lossless chunk receives another event-loop wake.
        gate.acknowledge();
    }
}

#[test]
fn optional_throttle_is_safe_and_immediate_at_clock_origin() {
    use std::time::{Duration, Instant};

    let origin = Instant::now();
    let interval = Duration::from_millis(200);
    assert!(super::throttle_elapsed(None, origin, interval));
    assert!(!super::throttle_elapsed(Some(origin), origin, interval));
    assert!(!super::throttle_elapsed(
        Some(origin + Duration::from_millis(1)),
        origin,
        interval
    ));
    assert!(super::throttle_elapsed(
        Some(origin),
        origin + interval,
        interval
    ));
}

/// A `cd` whose prompt redraw lands inside the throttle window must still
/// be polled. Dropping that poll left new panes in the old directory until
/// something redrew, which on an unfocused window can be never.
#[test]
fn a_throttled_process_poll_is_deferred_not_dropped() {
    use super::{REMOTE_POLL_INTERVAL, remote_poll_deferred_until};
    use std::time::{Duration, Instant};

    let last = Instant::now();
    assert_eq!(remote_poll_deferred_until(None, last), None);
    assert_eq!(
        remote_poll_deferred_until(Some(last), last + Duration::from_millis(50)),
        Some(last + REMOTE_POLL_INTERVAL)
    );
    assert_eq!(
        remote_poll_deferred_until(Some(last), last + REMOTE_POLL_INTERVAL),
        None
    );

    let src = super::production_source();
    let wake = src
        .split("fn about_to_wait(&mut self")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("about_to_wait body");
    assert!(
        wake.contains("self.poll_remote_contexts(&mut ws)"),
        "about_to_wait must run the deferred poll"
    );
    assert!(
        wake.contains("if let Some(due) = self.remote_poll_due"),
        "about_to_wait must wake for the deferred poll"
    );
}

#[test]
fn a_flipped_theme_schedule_asks_for_one_redraw() {
    assert!(super::theme_schedule_redraw_due(None, Some(true)));
    assert!(super::theme_schedule_redraw_due(None, Some(false)));
    assert!(!super::theme_schedule_redraw_due(None, None));
    assert!(!super::theme_schedule_redraw_due(Some(true), Some(true)));
    assert!(!super::theme_schedule_redraw_due(Some(false), None));
    assert!(super::theme_schedule_redraw_due(Some(true), Some(false)));
    assert!(super::theme_schedule_redraw_due(Some(false), Some(true)));
    let wait = super::until_next_wall_minute();
    assert!(wait > std::time::Duration::ZERO && wait <= std::time::Duration::from_secs(60));
}

#[test]
fn a_theme_schedule_redraws_whoever_blinks_the_cursor() {
    // With the cursor blinking on the window server's layer, no blink
    // frame comes to apply a scheduled theme, so the wait loop must check
    // the schedule itself, at every minute, whatever the blink is doing.
    let src = production_source();
    let body = src
        .split_once("fn about_to_wait_inner(")
        .expect("about_to_wait_inner")
        .1;
    let due = body
        .split_once("let theme_schedule_due =")
        .expect("theme schedule check")
        .1
        .split_once(';')
        .expect("statement")
        .0;
    assert!(due.contains("theme_schedule_redraw_due("));
    assert!(
        !due.contains("blink"),
        "the check must not depend on the blink"
    );
    assert!(
        body.contains("|| theme_schedule_due)"),
        "a flip must request a redraw"
    );
    assert!(
        body.contains("if let Some(next) = theme_schedule_wait {"),
        "the loop must wake for the schedule"
    );
    assert!(body.contains(".then(until_next_wall_minute)"));
}

#[test]
fn cursor_blink_stops_after_the_timeout_only_while_visible() {
    use std::time::Duration;
    let timeout = Some(Duration::from_secs(10));
    assert!(!super::cursor_blink_timed_out(
        Duration::from_secs(9),
        timeout,
        true
    ));
    assert!(super::cursor_blink_timed_out(
        Duration::from_secs(10),
        timeout,
        true
    ));
    // A hidden phase keeps going until the next toggle shows the cursor.
    assert!(!super::cursor_blink_timed_out(
        Duration::from_secs(60),
        timeout,
        false
    ));
    // `cursor-blink-timeout = 0` blinks for as long as the window is focused.
    assert!(!super::cursor_blink_timed_out(
        Duration::from_secs(3600),
        None,
        true
    ));
}

/// Drives the scheduler's two pure steps through 20 simulated seconds of
/// idle time: the blink must toggle until the timeout, stop within one
/// half-period after it, and rest with the cursor visible.
#[test]
fn an_idle_blink_stops_visible_within_one_half_period_of_the_timeout() {
    use std::time::Duration;
    let interval = Duration::from_millis(530);
    let timeout = Some(Duration::from_secs(10));
    let tick = Duration::from_millis(10);
    let mut now = Duration::ZERO;
    let (mut blink_on, mut last_blink) = (true, Duration::ZERO);
    let mut last_toggle = Duration::ZERO;
    while now < Duration::from_secs(20) {
        let timed_out = super::cursor_blink_timed_out(now, timeout, blink_on);
        let active = !timed_out && super::cursor_blink_active(true, true, true);
        if let Some(next) =
            super::next_cursor_blink_phase(active, now - last_blink, interval, blink_on)
        {
            blink_on = next;
            last_blink = now;
            last_toggle = now;
        }
        now += tick;
    }
    assert!(blink_on, "an idle blink must rest visible");
    assert!(
        last_toggle >= Duration::from_secs(10) - interval
            && last_toggle <= Duration::from_secs(10) + interval,
        "last toggle at {last_toggle:?}"
    );
}

/// The first window opens at exactly `window-width` x `window-height`
/// cells, whatever the cell size, padding, and chrome. The old 8x16 guess
/// opened a 120x36 request at 123x35.
#[test]
fn a_startup_surface_holds_exactly_the_requested_grid() {
    use kettle_config::{StatusBarMode, TabBarPos};
    let pad = (8.0, 8.0);
    let strip = 180.0;
    for pos in [
        TabBarPos::Top,
        TabBarPos::Bottom,
        TabBarPos::Left,
        TabBarPos::Right,
    ] {
        for status in [
            StatusBarMode::Off,
            StatusBarMode::Top,
            StatusBarMode::Bottom,
        ] {
            for tab_bar_h in [0.0, 25.5] {
                for hundredths in (500..3000).step_by(7) {
                    let cw = hundredths as f32 / 100.0;
                    let ch = cw * 2.1;
                    let bands = super::ContentBands {
                        tab_bar_h,
                        status_bar_h: if matches!(status, StatusBarMode::Off) {
                            0.0
                        } else {
                            ch + 6.0
                        },
                        ..Default::default()
                    };
                    for grid in [(20, 5), (80, 24), (120, 36), (211, 57), (300, 100)] {
                        let surface =
                            super::surface_for_grid(grid, (cw, ch), pad, bands, pos, status, strip);
                        let (_, _, w, h) =
                            super::content_rect_for_with_strip(surface, bands, pos, status, strip);
                        assert_eq!(
                            super::grid_dims_px((w, h), (cw, ch), pad),
                            grid,
                            "cell {cw}x{ch}, {pos:?}, status {status:?}, surface {surface:?}"
                        );
                    }
                }
            }
        }
    }
}

/// A fresh window starts its first pane before it creates the window and
/// the GPU, so the shell starts while they initialize. The renderer then
/// takes over the fonts the pane was sized with.
#[test]
fn a_fresh_window_starts_its_first_pane_before_the_window_and_gpu() {
    let src = super::production_source();
    let body = src
        .split_once("fn resumed_inner(")
        .expect("resumed_inner")
        .1;
    let spawn = body
        .find("self.spawn_first_tab(ws, has_launch_override)")
        .expect("early spawn");
    let window = body
        .find("event_loop.create_window(attrs)")
        .expect("window creation");
    let gpu = body
        .find("Renderer::new_with_display_handle(")
        .expect("renderer init");
    assert!(spawn < window && window < gpu);
    assert!(body[gpu..].contains("ws.startup_fonts.take(),"));
}

/// The system fonts load on their own thread from the start of
/// `run_with`, so enumerating them overlaps the event loop's start
/// (AppKit's launch, on macOS) instead of delaying the first pane, and
/// window 1 only measures them. The thread starts before anything can
/// fail or read the config, learns the family once the command-line
/// overrides are in, and is joined before the first pane spawns.
#[test]
fn the_fonts_load_while_the_event_loop_starts() {
    let src = super::production_source();
    let run = src
        .split_once("pub fn run_with(mut startup: crate::Options) -> Result<()> {")
        .expect("run_with")
        .1;
    let run = &run[..run.find("event_loop.run_app(&mut app)").expect("run_app")];
    let start = run
        .find("let mut font_preload = crate::font_preload::FontPreload::start();")
        .expect("run_with starts the font preload");
    let trace = run
        .find("let _trace = crate::startup_trace::FlushOnDrop;")
        .expect("trace guard");
    assert!(
        trace < start
            && run[trace..start]
                .lines()
                .skip(1)
                .all(|line| line.trim().is_empty() || line.trim_start().starts_with("//")),
        "the preload starts right after the trace guard"
    );
    for later in [
        "crate::paste_image::sweep_stale();",
        "disable_app_state_restoration();",
        "EventLoop::<UserEvent>::with_user_event().build()",
        "Config::read_from_with_trust(",
    ] {
        let at = run.find(later).unwrap_or_else(|| panic!("missing {later}"));
        assert!(start < at, "the preload starts before {later}");
    }
    let loaded = run
        .find("crate::startup_trace::mark(crate::startup_trace::Phase::ConfigLoaded);")
        .expect("config loaded");
    let request = run
        .find("font_preload.request_family(&initial_cfg.font_family);")
        .expect("run_with sends the configured family");
    assert!(
        loaded < request,
        "the family is final once the overrides are in"
    );
    assert!(run[request..].contains("font_preload: Some(font_preload),"));
    assert_eq!(src.matches("FontPreload::start()").count(), 1);

    // Window 1 takes the preload before it sizes and spawns its first
    // pane; no other window waits for it. The measured fonts wait in
    // `ws.startup_fonts` for the renderer, which would otherwise load
    // them a second time on the main thread.
    assert_eq!(src.matches("self.font_preload.take()").count(), 1);
    let resumed = src
        .split_once("fn resumed_inner(")
        .expect("resumed_inner")
        .1;
    let take = resumed
        .find("self.prepare_startup_fonts(ws, startup_scale);")
        .expect("window 1 measures the preloaded fonts");
    let helper = src
        .split_once("fn prepare_startup_fonts(")
        .unwrap()
        .1
        .split_once("\n    fn ")
        .unwrap()
        .0;
    assert!(helper.contains("self.font_preload.take()"));
    assert!(helper.contains("ws.startup_fonts = Some(fonts);"));
    let spawn = resumed
        .find("self.spawn_first_tab(ws, has_launch_override)")
        .expect("early spawn");
    assert!(take < spawn);
}

#[test]
fn the_blink_scheduler_honors_the_idle_timeout_and_every_activity_source() {
    let production = super::production_source();
    assert_eq!(
        production.matches("last_blink_activity =").count(),
        1,
        "only reset_blink_phase may restart the blink timeout"
    );
    assert!(!production.contains("note_blink_output_activity"));
    // Every paste channel (keys, menu, middle click, right click, file
    // drop, broadcast) delivers through these two. A paste restarts the
    // blink even with the pointer, and without a keystroke.
    for delivery in ["fn deliver_text_paste(", "fn deliver_path_paste("] {
        let body = production
            .split_once(delivery)
            .expect("paste delivery")
            .1
            .split_once("\n    }\n")
            .expect("end of paste delivery")
            .0;
        assert!(
            body.contains("self.reset_blink_phase(ws);"),
            "{delivery} must restart the blink"
        );
    }
    let src = &super::app_source();
    let scheduler = src
        .split_once("let pane_blink = self.pane_cursor_blinking(ws, cached_menu_snapshots);")
        .expect("blink scheduler")
        .1
        .split_once("let blink_interval")
        .expect("end of blink gate")
        .0;
    assert!(scheduler.contains("cursor_blink_timed_out("));
    assert!(scheduler.contains("&& !blink_timed_out"));
    // The window server's blink replaces the scheduler's, never both.
    assert!(scheduler.contains("let layer_blink = ws.blink_layer.is_active();"));
    assert!(scheduler.contains("&& !layer_blink"));
    let reset = src
        .split_once("fn reset_blink_phase(&mut self, ws: &mut WindowState) {")
        .expect("reset_blink_phase")
        .1
        .split_once("\n    }\n")
        .expect("end of reset_blink_phase")
        .0;
    assert!(reset.contains("ws.last_blink_activity = now;"));
    // New activity ends a layer blink planned from the old activity.
    assert!(
        reset.contains("if ws.blink_layer.is_active()")
            && reset.contains("window.request_redraw();")
    );
    let toggle = src
        .split_once("Action::ToggleCursorBlink => {")
        .expect("blink toggle")
        .1
        .split_once("Action::ToggleCopyOnSelect")
        .expect("end of blink toggle")
        .0;
    assert!(toggle.contains("self.reset_blink_phase(ws);"));
    let reload = production
        .split_once(
            "fn apply_reloaded_config(&mut self, ws: &mut WindowState, font_size_changed: bool) {",
        )
        .expect("apply_reloaded_config")
        .1
        .split_once("\n    fn ")
        .expect("end of apply_reloaded_config")
        .0;
    assert!(reload.trim_start().starts_with("// A config change"));
    assert!(!reload.contains("fn reload_config("));
    assert!(reload.contains("self.reset_blink_phase(ws);"));
}

#[test]
fn cursor_blink_wakes_only_when_config_pane_and_focus_allow_it() {
    for configured in [false, true] {
        for pane_requests_blink in [false, true] {
            for focused in [false, true] {
                assert_eq!(
                    super::cursor_blink_active(configured, pane_requests_blink, focused),
                    configured && pane_requests_blink && focused
                );
            }
        }
    }
    let interval = std::time::Duration::from_millis(530);
    assert_eq!(
        super::next_cursor_blink_phase(
            true,
            interval - std::time::Duration::from_millis(1),
            interval,
            true
        ),
        None
    );
    assert_eq!(
        super::next_cursor_blink_phase(true, interval, interval, true),
        Some(false)
    );
    assert_eq!(
        super::next_cursor_blink_phase(true, interval, interval, false),
        Some(true)
    );
    assert_eq!(
        super::next_cursor_blink_phase(false, interval * 10, interval, true),
        None
    );
}

/// The scheduler hands the blink to the window server only at an edge
/// that hid the cursor, after it advanced the phase and recorded whether
/// the half-period was quiet, and before it requests the edge's redraw,
/// which becomes the hand-off frame.
#[test]
fn the_scheduler_hands_off_only_at_an_edge_that_hides_the_cursor() {
    let src = super::production_source();
    let body = src
        .split_once("fn about_to_wait_inner(")
        .expect("about_to_wait_inner")
        .1;
    let edge = body.find("ws.last_blink = now;").expect("blink edge");
    let quiet = body
        .find(".note_edge(now, crate::cursor_blink::quiet_window(blink_interval));")
        .expect("quiet half-period");
    let gate = body
        .find("crate::cursor_blink::handoff_allowed(")
        .expect("hand-off gate");
    let request = body
        .find("ws.blink_layer.request_handoff();")
        .expect("hand-off request");
    let redraw = body.find("|| blink_due").expect("blink redraw");
    assert!(edge < quiet && quiet < gate && gate < request && request < redraw);
    let eligibility = src
        .split_once("fn pane_cursor_layer_eligible(")
        .expect("cursor eligibility")
        .1
        .split_once("fn pane_cursor_blinking(")
        .expect("end of eligibility")
        .0;
    for needle in [
        "ws.vi_mode.is_none()",
        "TermMode::VI",
        "CursorShape::Hidden",
        "content.display_offset",
        ".contains(&row)",
    ] {
        assert!(
            eligibility.contains(needle),
            "cursor eligibility lost {needle}"
        );
    }
    let start = src
        .split_once("fn start_layer_blink(")
        .expect("start layer")
        .1
        .split_once("fn hide_cursor_layer(")
        .expect("end of start layer")
        .0;
    assert!(start.contains("ws.blink_layer.failed_handoff();"));
    assert!(body[..gate].contains("&& Self::pane_cursor_layer_eligible(ws);"));
    let inputs = &body[gate..request];
    for needle in [
        "self.cfg.macos_cursor_blink_layer",
        "edge_hides_cursor: next_blink == Some(false),",
        "blink_active,",
        "cursor_eligible,",
        "|| pending_video_receipt_wait.is_some()",
        "quiet: blink_quiet,",
        "ime_preedit: ws.ime_preedit.is_some(),",
        "modal_steady: modal_holds_cursor_steady(ws),",
        "|| ws.accessibility_pending",
        "is_some_and(Renderer::has_pending_screenshot)",
    ] {
        assert!(inputs.contains(needle), "the hand-off gate lost {needle:?}");
    }
}

/// A frame ends the window server's blink only when it presents: the
/// layer hides in the `Presented` arm, inside the transaction that
/// `begin_exit_frame` opens before the render call and `end_exit_frame`
/// commits after the outcome match, so the frame and the hide land
/// together. The phase is materialized before anything reads it.
#[test]
fn a_layer_blink_ends_only_with_a_presented_frame() {
    let src = super::production_source();
    let redraw = src
        .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
        .expect("redraw")
        .1
        .split_once("\n    }\n")
        .expect("end of redraw")
        .0;
    assert!(
        redraw
            .trim_start()
            .starts_with("// A frame while the window server blinks")
    );
    assert!(redraw.contains("Self::materialize_layer_blink(ws, std::time::Instant::now());"));
    let handoff = redraw
        .split_once("let blink_handoff =")
        .expect("redraw handoff recheck")
        .1;
    assert!(
        handoff
            .split_once("let Some(renderer) = ws.renderer.as_mut()")
            .expect("renderer borrow")
            .0
            .contains("&& self.cfg.macos_cursor_blink_layer")
    );
    let phase = src
        .split_once("fn materialize_layer_blink(")
        .expect("materialize helper")
        .1
        .split_once("fn ensure_cursor_layer(")
        .expect("end of materialize helper")
        .0;
    assert!(phase.contains(".materialize(ws.blink_on, ws.last_blink, now)"));
    let begin = redraw
        .find("layer.begin_exit_frame();")
        .expect("exit frame opens");
    let render = redraw
        .find("render_frame_with_status_and_pre_present(")
        .expect("render call");
    let presented = redraw
        .find("FrameOutcome::Presented => {")
        .expect("presented arm");
    let retry = redraw
        .find("FrameOutcome::RetryLater => {")
        .expect("retry arm");
    let lost = redraw
        .find("FrameOutcome::SurfaceLost => {")
        .expect("lost arm");
    let end = redraw
        .find("layer.end_exit_frame();")
        .expect("exit frame closes");
    assert!(begin < render && render < presented && presented < retry && lost < end);
    assert_eq!(redraw.matches("layer.hide();").count(), 1);
    let hide = redraw.find("layer.hide();").expect("hide");
    assert!(
        presented < hide && hide < retry,
        "hide() only for a presented frame"
    );
    let record = redraw
        .find("ws.blink_layer.exited(exit_time);")
        .expect("total exit time");
    let start = redraw
        .find("let exit_started =")
        .expect("exit timer starts");
    let measured = redraw
        .find("let exit_time = exit_started.elapsed();")
        .expect("exit timer ends");
    assert!(start < begin && end < measured && measured < record);
    assert!(redraw.contains("let exit_time = exit_started.elapsed();"));
    assert!(redraw[record..].contains("Self::retire_cursor_layer(ws);"));
    let reload = src
        .split_once("fn apply_reloaded_config(")
        .expect("config reload")
        .1
        .split_once("\n    }\n")
        .expect("end of config reload")
        .0;
    assert!(reload.contains("!self.cfg.macos_cursor_blink_layer && !ws.blink_layer.is_active()"));
    assert_eq!(
        redraw
            .matches("ws.blink_layer.take_pending_handoff()")
            .count(),
        1
    );
}

/// Window changes that move or invalidate the patch hide the layer at the
/// event, without waiting for a frame that may retry: a new size (the
/// Metal layer stretches its last frame but not the patch), a new scale,
/// focus loss and occlusion. A Resized that keeps the surface size, or
/// a degenerate one, leaves the blink alone.
#[test]
fn window_changes_hide_the_layer_without_a_frame() {
    let src = super::production_source();
    let arm = |head: &str, end: &str| -> String {
        src.split_once(head)
            .unwrap_or_else(|| panic!("{head}"))
            .1
            .split_once(end)
            .unwrap_or_else(|| panic!("{end}"))
            .0
            .to_string()
    };
    let resized = arm(
        "WindowEvent::Resized(size) => {",
        "WindowEvent::ScaleFactorChanged",
    );
    let degenerate = resized
        .find("ws.dpi_resize_surface_suspended = true;")
        .expect("degenerate return");
    let changed = resized
        .find(".is_none_or(|r| r.surface_size() != (size.width, size.height))")
        .expect("size comparison");
    let hide = resized
        .find("Self::hide_cursor_layer(ws);")
        .expect("resize hides the layer");
    let layout = resized
        .find("ws.dpi_resize.on_event(")
        .expect("resize coalescer");
    assert!(degenerate < changed && changed < hide && hide < layout);
    let scale = arm(
        "WindowEvent::ScaleFactorChanged { scale_factor, .. } => {",
        "WindowEvent::ThemeChanged",
    );
    assert!(
        scale
            .find("Self::hide_cursor_layer(ws);")
            .expect("scale hides")
            < scale
                .find("ws.dpi_resize.on_event(")
                .expect("scale coalescer")
    );
    let focus = arm("WindowEvent::Focused(f) => {", "rec.record_marker(");
    assert!(
        focus
            .split_once("if !f {")
            .expect("focus loss")
            .1
            .contains("Self::hide_cursor_layer(ws);")
    );
    let occluded = arm("WindowEvent::Occluded(occluded) => {", "if !occluded {");
    assert!(occluded.contains("if occluded {\n                    Self::hide_cursor_layer(ws);"));
}

/// C1 can retain old layer contents after failed patch presentation.
/// Only the latest Presented patch may show, and Lost must reattach later.
#[test]
fn cursor_patch_uses_the_presented_scene_and_hides_failed_contents() {
    let src = super::production_source();
    let redraw = src
        .split_once("fn redraw(&mut self, ws: &mut WindowState) {")
        .expect("redraw")
        .1
        .split_once("\n    }\n")
        .expect("end redraw")
        .0;
    let render = redraw
        .find("let frame_result = renderer.render_frame_with_status_and_pre_present(")
        .expect("render");
    let gate = redraw
        .find("matches!(&frame_result, Ok(FrameOutcome::Presented))")
        .expect("present gate");
    let patch = redraw
        .find("renderer.present_cursor_patch(&self.cfg)")
        .expect("patch");
    let outcome = redraw.find("match frame_result {").expect("outcome");
    assert!(render < gate && gate < patch && patch < outcome);
    assert!(!redraw[render..patch].contains("renderer.set_"));
    let start = src
        .split_once("fn start_layer_blink(")
        .expect("start")
        .1
        .split_once("fn hide_cursor_layer(")
        .expect("end start")
        .0;
    let hide = start.find("layer.hide();").expect("hide old contents");
    let result = start.find("match patch {").expect("patch outcome");
    let show = start.find("match layer.show(").expect("show");
    assert!(hide < result && result < show);
    assert!(start.contains("CursorPatchOutcome::Presented(rect) => rect,"));
    let ineligible = start
        .split_once("CursorPatchOutcome::Ineligible(reason) => {")
        .expect("ineligible")
        .1
        .split_once("CursorPatchOutcome::Failed(reason) => {")
        .expect("failure");
    assert!(ineligible.0.contains("return;"));
    let failed = ineligible.1.split_once("let rect_px =").expect("rect").0;
    assert!(failed.contains("return;"));
    assert!(failed.contains("if reason == CursorPatchFailure::Lost {\n                    Self::retire_cursor_layer(ws);"));
    assert!(start.contains("let rect_px = [rect.x, rect.y, rect.width, rect.height];"));
    let retire = src
        .split_once("fn retire_cursor_layer(")
        .expect("retire")
        .1
        .split_once("fn broadcast_input(")
        .expect("end retire")
        .0;
    assert!(
        retire.find("Self::hide_cursor_layer(ws);").expect("hide")
            < retire
                .find("renderer.detach_cursor_layer();")
                .expect("detach")
    );
    assert!(retire.contains("let layer = ws.cursor_layer.take();"));
}

/// `ui_geometry` reads draw no frame, and a connection that attached no
/// pane draws none when it closes, so `kettle ctl ui_geometry` can watch a
/// window-server blink without ending it.
#[test]
fn ui_geometry_reads_draw_no_frame() {
    use kettle_ctl::protocol::Method;
    for method in Method::ALL {
        assert_eq!(
            super::ctl_method_is_pure_read(method),
            matches!(
                method,
                Method::UiGeometry | Method::AgentLaunch | Method::CardPickup
            ),
            "{method:?}"
        );
    }
    let src = super::production_source();
    let drain = src
        .split_once("fn drain_ctl(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {")
        .expect("drain_ctl")
        .1
        .split_once("\n    fn handle_ctl_request(")
        .expect("end of drain_ctl")
        .0;
    assert!(
        drain.contains(
            "if !request.internal_probe() && !ctl_method_is_pure_read(request.method()) {"
        )
    );
    assert!(drain.contains("needs_redraw |= !panes.is_empty();"));
    assert!(!drain.contains("the agent badge may have cleared"));
}

#[test]
fn empty_ime_preedit_normalizes_to_stable_absence() {
    assert_eq!(super::normalized_ime_preedit(String::new(), None), None);
    assert_eq!(
        super::normalized_ime_preedit("compose".into(), Some((1, 4))),
        Some(("compose".into(), Some((1, 4))))
    );
    assert_eq!(super::ime_preedit_focus("compose", Some((1, 4))), 4);
    assert_eq!(super::ime_preedit_focus("界x", Some((0, 3))), 3);
    // An invalid byte offset and a caret inside an extended grapheme both snap backward to a
    // safe visible boundary.
    assert_eq!(super::ime_preedit_focus("界x", Some((0, 2))), 0);
    assert_eq!(super::ime_preedit_focus("a\u{301}x", Some((0, 1))), 0);
}

/// A composition cancelled where it began leaves no owner behind, so the
/// next composition, in search, keeps its Commit. And one cut short by a
/// focus change without a Commit stops guarding once focus moves on again,
/// so a plain Commit later is typed. Both once dropped the text.
#[test]
fn a_finished_composition_leaves_no_stale_ime_owner() {
    let mut ws = WindowState::new(1, Mux::new());
    assert!(super::update_ime_preedit(&mut ws, "ka".into(), None));
    super::update_ime_preedit(&mut ws, String::new(), None);
    assert!(ws.ime_preedit_owner.is_none(), "a cancel ends the session");
    ws.search.open = true;
    ws.ime_focus_generation += 1;
    assert!(super::update_ime_preedit(&mut ws, "ki".into(), None));
    assert!(
        super::take_ime_commit(&mut ws),
        "search's own Commit is typed"
    );

    let mut ws = WindowState::new(1, Mux::new());
    assert!(super::update_ime_preedit(&mut ws, "ku".into(), None));
    super::end_modal_composition(&mut ws);
    super::update_ime_preedit(&mut ws, String::new(), None);
    ws.ime_focus_generation += 1;
    assert!(
        super::take_ime_commit(&mut ws),
        "a later plain Commit is typed"
    );
}

#[test]
fn empty_preedit_preserves_ownership_until_commit() {
    for clear_display in [false, true] {
        let mut ws = WindowState::new(1, Mux::new());
        assert!(super::update_ime_preedit(&mut ws, "compose".into(), None));
        let owner = ws.ime_preedit_owner;
        if clear_display {
            super::end_modal_composition(&mut ws);
            assert!(ws.ime_preedit.is_none());
        } else {
            ws.ime_focus_generation += 1;
        }
        ws.palette_input = Some((String::new(), 0));
        super::update_ime_preedit(&mut ws, String::new(), None);
        let kept = |ws: &WindowState| ws.ime_preedit_owner.map(|s| (s.owner, s.generation));
        assert_eq!(kept(&ws), owner.map(|s| (s.owner, s.generation)));
        assert!(!super::take_ime_commit(&mut ws));
        assert!(ws.ime_preedit_owner.is_none());

        assert!(super::update_ime_preedit(&mut ws, "new".into(), None));
        super::update_ime_preedit(&mut ws, String::new(), None);
        assert!(super::take_ime_commit(&mut ws));
    }
}

#[test]
fn control_selection_cap_preserves_utf8_boundaries() {
    let exact = "x".repeat(super::MAX_CTL_SELECTION_BYTES);
    assert_eq!(super::cap_ctl_selection(exact.clone()), (exact, false));

    let oversized = format!("{}é", "x".repeat(super::MAX_CTL_SELECTION_BYTES - 1));
    let (capped, truncated) = super::cap_ctl_selection(oversized);
    assert!(truncated);
    assert_eq!(capped.len(), super::MAX_CTL_SELECTION_BYTES - 1);
    assert!(capped.is_char_boundary(capped.len()));
}

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

#[test]
fn the_shelf_badge_counts_unseen_items_only() {
    assert_eq!(super::media_shelf_badge(0), "");
    assert_eq!(super::media_shelf_badge(3), "▣3 ");
}

/// A preview lane owns every press and wheel over it, on the native and
/// the ctl paths alike, ahead of inline cards, the receipt and the
/// terminal; only the primary button acts on its controls.
#[test]
fn a_preview_lane_owns_presses_and_wheels_over_it() {
    let source = super::production_source();
    let body = |name: &str| {
        source
            .split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name} present"))
    };
    for name in ["ctl_mouse_press", "window_event_inner"] {
        let press = body(name);
        let lane = press
            .find("self.press_preview_lane(ws, px, py, bcode)")
            .unwrap_or_else(|| panic!("{name} asks the lanes"));
        let card = press
            .find("self.press_card(ws, px, py, bcode)")
            .unwrap_or_else(|| panic!("{name} asks the cards"));
        let receipt = press
            .find("self.activate_media_paste_receipt_at(ws, px, py, bcode)")
            .unwrap_or_else(|| panic!("{name} asks the receipt"));
        assert!(
            lane < card && lane < receipt,
            "{name}: the lane is asked first"
        );
    }
    assert!(
        body("window_event_inner").contains("if !self.press_preview_lane(ws, px, py, sgr)"),
        "a side button over a lane goes nowhere"
    );
    assert!(body("dispatch_wheel").contains("if self.lane_takes_wheel(ws) {"));
    let press = body("press_preview_lane");
    assert!(
        press.contains("if bcode != 0 {\n            return true;\n        }"),
        "every press over a lane is consumed; only the primary button acts"
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
            .and_then(|rest| rest.split("\n    fn ").next())
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

/// Wheel motion over a lane is dropped before it is banked, so no part of
/// it scrolls the terminal beside it.
#[test]
fn a_preview_lane_drops_wheel_motion_before_it_is_banked() {
    let source = super::production_source();
    let ctl = source
        .split("fn ctl_mouse_wheel_delta(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("ctl_mouse_wheel_delta");
    let native = source
        .split("WindowEvent::MouseWheel { delta, phase, .. } => {")
        .nth(1)
        .expect("native wheel");
    for (path, body) in [("ctl", ctl), ("native", native)] {
        let gate = body
            .find("if self.lane_takes_wheel(ws) {\n")
            .unwrap_or_else(|| panic!("{path} asks the lanes"));
        let feed = body
            .find("ws.wheel.feed(")
            .unwrap_or_else(|| panic!("{path} banks motion"));
        assert!(gate < feed, "{path}: the lanes are asked before banking");
        assert!(
            body[gate..feed].contains("ws.wheel.reset();"),
            "{path}: the residue is dropped"
        );
    }
}

/// A lane is no modal: dialogs, menus and other modals sit over it and
/// leave it where it is, and the event loop closes a lane only when it
/// has nothing to show.
#[test]
fn a_modal_leaves_the_preview_lanes_in_place() {
    let source = super::production_source();
    let body = |name: &str| {
        source
            .split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name} present"))
    };
    for name in [
        "close_all_modals",
        "install_confirm_dialog",
        "show_context_menu",
        "commit_ime_text",
    ] {
        let modal = body(name);
        assert!(
            !modal.contains("preview_panels"),
            "{name} keeps pinned panels"
        );
        if name == "close_all_modals" {
            assert!(modal.contains("if let Some(open) = ws.preview_overlay"));
            assert!(modal.contains("self.close_preview(ws, open.pane)"));
        } else {
            assert!(
                !modal.contains("close_preview"),
                "{name} keeps pinned lanes"
            );
        }
    }
    assert!(body("about_to_wait_inner").contains("self.prune_previews(ws);"));
    let prune = body("prune_previews");
    assert!(prune.contains("&& self.preview_item(ws, *pane).is_some()"));
    assert!(prune.contains("ws.mux.retain_lanes(|pane| live.contains(&pane))"));
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
        .and_then(|rest| rest.split("\n    fn ").next())
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

/// A settled card under the pointer gets the pointing hand and the
/// hover outline, after dialogs and chrome but before split seams, and
/// never with Shift or under the viewer; a card under a still pointer gets them when
/// it settles or moves there, and a pointer that left the window has
/// none.
#[test]
fn a_settled_card_under_the_pointer_shows_it_takes_a_click() {
    let source = super::production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(source.contains(
        "let card = ws .native_pointer .filter(|_| !ws.mods.shift_key() && !self.pointer_modal_open(ws))"
    ));
    assert!(source.contains(
        "let settled = card.filter(|card| ws.card_sightings.settled(Some(*card), now).is_some());"
    ));
    assert!(source.contains(".and_then(|card| ws.card_sightings.settles_at(&card, now));"));
    assert!(source.contains(
        ".or(chrome) // A card takes a press before a split seam does, so its hand // wins where the two overlap. .or(card_hover) // A lane takes every press over it. .or_else(|| self.lane_cursor_icon(ws)) .or_else(|| self.split_seam_hover_icon(ws)) .unwrap_or_else("
    ));
    assert!(source.contains(
        "if ws.card_hover_stale || ws.card_settle_wake.is_some_and(|at| at <= now) { ws.card_hover_stale = false; self.sync_cursor_icon(ws); }"
    ));
    assert!(source.contains(
        "if let Some(next) = card_settle_wait { wait = Some(wait.map_or(next, |current| current.min(next))); }"
    ));
    assert!(
        source.contains(
            "ws.native_pointer = None; ws.card_hover = None; ws.card_settle_wake = None;"
        )
    );
    assert!(source.contains("ws.accessibility_pending = true; ws.card_hover_stale = true;"));
    assert!(source.contains("let card_hover = ws.card_hover;"));
    // The icon, native buttons and the wheel all follow the physical
    // pointer, even after a control client moved the cursor.
    assert!(source.contains(
        "fn sync_cursor_icon(&mut self, ws: &mut WindowState) { let Some(pointer) = ws.native_pointer else { return; }; let control = std::mem::replace(&mut ws.cursor, pointer); let media_receipt_hovered = self.media_paste_receipt_hovered(ws); self.sync_cursor_icon_with_media_receipt_hover(ws, media_receipt_hovered); ws.cursor = control; }"
    ));
    for handler in [
        "WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {",
        "WindowEvent::MouseInput { state: ElementState::Released, button, .. } => {",
        "WindowEvent::MouseWheel { delta, phase, .. } => {",
    ] {
        assert!(
            source.contains(&format!("{handler} ws.resume_native_pointer();")),
            "{handler}"
        );
    }
    // The viewer opens on the item published last.
    assert!(source.contains(".and_then(|state| state.media_shelf.latest())"));
    // A control client's moves never light a card the user is not
    // pointing at.
    let ctl = source
        .split("fn ctl_send_mouse_for_window(")
        .nth(1)
        .and_then(|rest| rest.split(" fn ").next())
        .expect("ctl send_mouse");
    assert!(!ctl.contains("native_pointer"));
    assert!(source.contains("ws.cursor = position; ws.native_pointer = Some(position);"));
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
        .find("&& let Some(drag) = ws.pane_drag.take()")
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
        let body = &body[..body.find("\n    fn ").unwrap_or(body.len())];
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
            .and_then(|rest| rest.split("\n    fn ").next())
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
    let registered = finish.find("register_card(").expect("a card registers");
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

#[test]
fn preview_accessibility_key_tracks_controls_and_video_position() {
    let source = super::production_source();
    let key_body = source
        .split_once("fn accessibility_key(")
        .unwrap()
        .1
        .split("fn publish_accessibility_if_due(")
        .next()
        .unwrap();
    assert!(key_body.contains("accessibility_lane_controls_key("));
    let geometry = |lane: &kettle_render::MediaLanePanel| {
        kettle_render::media_lane_geometry(lane, (8.4, 16.4), (8.4, 16.4)).unwrap()
    };
    let key = |lane: &kettle_render::MediaLanePanel| {
        super::accessibility_lane_controls_key(lane, Some(&geometry(lane)))
    };
    let mut lane = test_lane_panel();
    lane.source = Some(kettle_render::MediaLaneSource::default());
    let rendered = key(&lane);
    lane.mode = kettle_render::MediaLaneMode::Source;
    assert_ne!(rendered, key(&lane));
    lane.mode = kettle_render::MediaLaneMode::Rendered;
    lane.position = (1, 3);
    assert_ne!(rendered, key(&lane));
    lane.position = (2, 3);
    lane.pin_label = Some("Pin".into());
    assert_ne!(rendered, key(&lane));
    lane.pin_label = None;
    let mut moved = geometry(&lane);
    moved.close.0 += 1.0;
    assert_ne!(
        rendered,
        super::accessibility_lane_controls_key(&lane, Some(&moved))
    );
    let mut fewer = geometry(&lane);
    assert!(fewer.copy.take().is_some());
    assert_ne!(
        rendered,
        super::accessibility_lane_controls_key(&lane, Some(&fewer))
    );
    assert_ne!(
        rendered,
        super::accessibility_lane_controls_key(&lane, None)
    );

    lane.scrubber = Some(kettle_render::MediaLaneScrubber {
        position_ms: 1_000,
        duration_ms: 10_000,
        label: String::new(),
    });
    assert!(geometry(&lane).scrubber.is_some());
    let playing = key(&lane);
    assert_ne!(rendered, playing);
    lane.scrubber.as_mut().unwrap().position_ms = 2_000;
    assert_ne!(playing, key(&lane));
    let later = key(&lane);
    lane.scrubber.as_mut().unwrap().duration_ms = 20_000;
    assert_ne!(later, key(&lane));
    let changed_duration = key(&lane);
    lane.scrubber = None;
    assert_ne!(changed_duration, key(&lane));
    assert_eq!(rendered, key(&lane));
}

pub(super) fn test_lane_panel() -> kettle_render::MediaLanePanel {
    kettle_render::MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        sound: None,
        frame: None,
        scrubber: None,
        pane: 7,
        rect: (0.0, 360.0, 800.0, 240.0),
        collapsed: false,
        title: "Plot".into(),
        detail: "Image · 64×48".into(),
        sender: kettle_render::MediaLaneSender {
            text: "From this pane".into(),
            program: None,
            signer: None,
        },
        hint: "Keys still go to the terminal".into(),
        position: (2, 3),
        page: None,
        document: false,
        image: Some(kettle_core::ImageData::new(1, 1, vec![0; 4]).unwrap()),
        status: "Released.".into(),
        canvas: kettle_render::MediaCanvas::Theme,
        open_outside: false,
        mode: kettle_render::MediaLaneMode::Rendered,
        source: None,
        copy: true,

        reload: false,

        notice: None,
        view: kettle_render::MediaViewport::FIT,
        tile: None,
        edge: None,
    }
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
        .and_then(|rest| rest.split("\n    fn ").next())
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

/// `get_state`'s caller is verified in the nearest live pane whose child
/// is one of its ancestors. Hints and failures never verify.
#[test]
fn caller_report_names_the_nearest_live_pane() {
    use crate::ctl_server::{CallerEvidence, PaneHint};
    use kettle_ctl::identity::UnverifiedReason;
    use kettle_ctl::process::ProcessIdentity;
    let id = |pid, start| ProcessIdentity::new_for_tests(pid, start);
    // caller 30 → tool 31 → inner pane's shell 20 → outer pane's shell 10.
    let chain = vec![id(30, 300), id(31, 290), id(20, 200), id(10, 100)];
    let root = |root, pane, window| crate::media::PaneRoot { root, pane, window };
    let roots = [root(id(10, 100), 1, 1), root(id(20, 200), 7, 2)];
    let panes = [(1, 1), (7, 2), (9, 2)];
    let report = |caller| super::ctl_caller_report(&caller, &roots, &panes, 4242);

    let verified = report(CallerEvidence::for_tests(Ok(chain.clone()), None));
    assert_eq!(verified["verified"], true);
    assert_eq!(verified["pane"], 7, "the nearest pane wins");
    assert_eq!(verified["window"], 2);
    assert_eq!(verified["reason"], serde_json::Value::Null);
    assert_eq!(verified["peer"]["pid"], 30);
    assert_eq!(verified["peer"]["start_token"], "300");

    // A root whose start differs is another process that reused the pid.
    let reused = super::ctl_caller_report(
        &CallerEvidence::for_tests(Ok(vec![id(30, 300), id(20, 201)]), None),
        &roots,
        &panes,
        4242,
    );
    assert_eq!(reused["verified"], false);
    assert_eq!(reused["reason"], "no_pane");
    assert_eq!(reused["pane"], serde_json::Value::Null);

    let failed = report(CallerEvidence::for_tests(
        Err(UnverifiedReason::ClaimMismatch),
        Some(PaneHint {
            pane: 9,
            kettle_pid: 4242,
        }),
    ));
    assert_eq!(failed["verified"], false);
    assert_eq!(failed["reason"], "claim_mismatch");
    assert_eq!(failed["peer"], serde_json::Value::Null);
    assert_eq!(
        failed["hint"]["pane"], 9,
        "a hint for this Kettle is reported"
    );
    assert_eq!(failed["hint"]["window"], 2);

    // A hint naming another Kettle, or a pane this one does not have, is
    // dropped; a hint never verifies.
    for hint in [
        PaneHint {
            pane: 9,
            kettle_pid: 1,
        },
        PaneHint {
            pane: 99,
            kettle_pid: 4242,
        },
    ] {
        let dropped = report(CallerEvidence::for_tests(
            Err(UnverifiedReason::MissingClaim),
            Some(hint),
        ));
        assert_eq!(dropped["hint"], serde_json::Value::Null, "{hint:?}");
    }
    let unchecked = report(CallerEvidence::default());
    assert_eq!(unchecked["verified"], false);
    assert_eq!(unchecked["reason"], "missing_claim");
}

#[test]
fn control_pages_are_bounded_and_snapshot_consistent() {
    let values: Vec<serde_json::Value> = (0..5).map(serde_json::Value::from).collect();
    let first = kettle_ctl::protocol::Request {
        v: 1,
        id: 1,
        method: "list_panes".into(),
        params: serde_json::json!({"limit": 2}),
        caller: None,
    };
    let response = super::ctl_page_values(&first, "panes", values.clone());
    assert!(response.ok);
    assert_eq!(response.result["panes"].as_array().unwrap().len(), 2);
    assert_eq!(response.result["next_cursor"], "2");
    assert_eq!(response.result["truncated"], true);

    let second = kettle_ctl::protocol::Request {
        id: 2,
        params: serde_json::json!({
            "cursor": response.result["next_cursor"],
            "snapshot": response.result["snapshot"],
            "limit": 2,
        }),
        ..first
    };
    let response = super::ctl_page_values(&second, "panes", values);
    assert_eq!(response.result["panes"], serde_json::json!([2, 3]));

    let stale = kettle_ctl::protocol::Request {
        id: 3,
        params: serde_json::json!({"cursor": "2", "snapshot": "stale"}),
        ..second
    };
    let response = super::ctl_page_values(&stale, "panes", vec![serde_json::json!(1)]);
    assert_eq!(
        response.error.unwrap().code,
        kettle_ctl::protocol::error_codes::STALE_SNAPSHOT
    );
}

#[test]
fn control_inventory_limit_stops_enumeration_at_the_first_overage() {
    let pulled = std::cell::Cell::new(0usize);
    let counts = std::iter::from_fn(|| {
        pulled.set(pulled.get() + 1);
        Some(super::MAX_CTL_COLLECTION_ITEMS + 1)
    });
    assert_eq!(
        super::ctl_bounded_sum(counts, super::MAX_CTL_COLLECTION_ITEMS),
        None
    );
    assert_eq!(
        pulled.get(),
        1,
        "the collection cap must stop the producer, not just reject after collection"
    );
    assert_eq!(super::ctl_bounded_sum([2, 3, 5], 10), Some(10));
}

/// Startup drift guard: the window is hidden only while renderer init runs,
/// then revealed for every visible startup state. `window_state = hidden`
/// still stays hidden.
#[test]
fn window_revealed_after_renderer_init_for_visible_states_only() {
    use kettle_config::WindowState;
    assert!(should_reveal_after_renderer_init(WindowState::Normal));
    assert!(should_reveal_after_renderer_init(WindowState::Maximise));
    assert!(should_reveal_after_renderer_init(WindowState::Fullscreen));
    assert!(!should_reveal_after_renderer_init(WindowState::Hidden));
}

/// Surface backends such as macOS Metal need the native window ordered on
/// screen before their first drawable can exist. Pin the portable policy
/// independently of the host running this unit test.
#[test]
fn visible_surface_requirement_breaks_the_hidden_first_frame_cycle() {
    use kettle_config::WindowState;
    assert!(should_reveal_before_first_surface_frame(
        WindowState::Normal,
        false,
        true
    ));
    assert!(!should_reveal_before_first_surface_frame(
        WindowState::Hidden,
        false,
        true
    ));
    assert!(!should_reveal_before_first_surface_frame(
        WindowState::Normal,
        true,
        true
    ));
    assert!(!should_reveal_before_first_surface_frame(
        WindowState::Normal,
        false,
        false
    ));
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
fn startup_keeps_the_remote_spool_while_another_build_runs() {
    let dir = crate::test_tempdir();
    let path = dir.path().join("remote.cmd");
    std::fs::write(&path, b"new-tab\n").unwrap();
    super::initialize_remote_spool(&path, true).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new-tab\n");
    super::initialize_remote_spool(&path, false).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"");
}

#[test]
fn desktop_notifications_never_call_the_os_backend_on_the_ui_thread() {
    let src = production_source();
    let body = src
        .split("pub(crate) fn fire_notify(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("fire_notify body");
    assert!(body.contains("crate::queue_desktop_notification("));
    assert!(
        !src.contains("notify_rust::"),
        "the UI event-loop module must not be able to call a platform notifier"
    );
}

#[test]
fn protocol_notifications_are_wired_to_ui_dispatch() {
    let src = production_source();
    assert!(
        src.contains("drain_protocol_notifications()"),
        "drain_events must consume OSC 9/777 notification requests"
    );
    assert!(
        src.contains("fire_notify(&ev.title, &ev.body);"),
        "protocol notifications must use the desktop notification helper"
    );
    assert!(
        src.contains("\"protocol_notification\""),
        "protocol notifications must be visible to control/event subscribers"
    );
}

#[test]
fn startup_geometry_cells_convert_to_inner_size() {
    let mut cfg = kettle_config::Config {
        window_width: Some(120),
        window_height: Some(40),
        ..Default::default()
    };
    assert_eq!(
        startup_inner_size_px(&cfg),
        Some((976, 680)),
        "120x40 cells + default padding + default top tab bar"
    );

    cfg.tab_bar = kettle_config::TabBarMode::Auto;
    cfg.status_bar = kettle_config::StatusBarMode::Bottom;
    assert_eq!(
        startup_inner_size_px(&cfg),
        Some((976, 678)),
        "auto tab bar is hidden for a one-tab startup; status bar reserves its strip"
    );

    cfg.window_height = None;
    cfg.tab_bar = kettle_config::TabBarMode::Always;
    cfg.tab_bar_pos = kettle_config::TabBarPos::Left;
    assert_eq!(
        startup_inner_size_px(&cfg),
        Some((1156, 758)),
        "missing height comes from the 45-row default grid; vertical tabs reserve width"
    );
    cfg.window_width = None;
    cfg.window_height = Some(30);
    cfg.tab_bar_pos = kettle_config::TabBarPos::Top;
    assert_eq!(
        startup_inner_size_px(&cfg),
        Some((1296, 542)),
        "missing width comes from the 160-column default grid"
    );
    cfg.window_height = None;
    assert_eq!(
        startup_inner_size_px(&cfg),
        None,
        "neither side set: the monitor-fitted default takes over"
    );
}

/// A fresh window with no configured size opens at the 160x45 default
/// grid (1296x760 logical px with the default chrome), fitted to 90 % x
/// 85 % of the monitor. The numbers below are the ones the docs quote.
#[test]
fn default_startup_size_targets_the_agent_grid_and_fits_the_monitor() {
    use super::{
        STARTUP_DEFAULT_COLS, STARTUP_DEFAULT_ROWS, StartupMonitor, default_startup_inner_size,
        startup_inner_size,
    };
    let cfg = kettle_config::Config::default();
    assert_eq!((STARTUP_DEFAULT_COLS, STARTUP_DEFAULT_ROWS), (160, 45));
    assert_eq!(
        default_startup_inner_size(&cfg, None),
        (1296.0, 760.0),
        "160x45 cells + 8 px padding + the default top tab bar, no monitor known"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((1920.0, 1080.0))),
        (1296.0, 760.0),
        "a 1080p monitor fits the whole target (about 152x41 cells at the 14 px font)"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((1366.0, 768.0))),
        (1229.0, 652.0),
        "a 1366x768 laptop clamps both axes: about 144 columns at the default font"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((3440.0, 1440.0))),
        (1296.0, 760.0),
        "an ultrawide gets the same terminal-sized window, not a monitor fraction"
    );
    assert_eq!(
        default_startup_inner_size(&cfg, Some((640.0, 480.0))),
        (576.0, 408.0),
        "a monitor smaller than the target wins outright; there is no floor"
    );

    // The explicit path is honoured as typed and never monitor-fitted.
    let explicit = kettle_config::Config {
        window_width: Some(400),
        window_height: Some(200),
        ..Default::default()
    };
    let tiny = Some(StartupMonitor::from_physical(640, 480, 1.0));
    assert_eq!(startup_inner_size(&explicit, tiny), (3216.0, 3240.0));
    assert_eq!(
        startup_inner_size(&cfg, tiny),
        (576.0, 408.0),
        "the default path is the one that fits the monitor"
    );

    // HiDPI: the size is logical, so a 2x display describes the same
    // logical monitor and gets the same grid as a 1x one.
    let hidpi = StartupMonitor::from_physical(3840, 2160, 2.0);
    assert_eq!(hidpi.logical, (1920.0, 1080.0));
    assert_eq!(hidpi.scale, 2.0);
    assert_eq!(
        startup_inner_size(&cfg, Some(hidpi)),
        startup_inner_size(&cfg, Some(StartupMonitor::from_physical(1920, 1080, 1.0)))
    );
    assert_eq!(
        startup_inner_size(&explicit, Some(hidpi)),
        startup_inner_size(
            &explicit,
            Some(StartupMonitor::from_physical(1920, 1080, 1.0))
        ),
        "an explicit window-width means the same columns on every DPI"
    );
    let degenerate = StartupMonitor::from_physical(1920, 1080, 0.0);
    assert_eq!(
        degenerate.scale, 1.0,
        "a bogus scale factor falls back to 1x"
    );
}

/// Both window constructors must size through the shared startup rule in
/// logical pixels; a `PhysicalSize` at startup would bring back the
/// half-columns-on-HiDPI bug, and skipping the monitor would lose the fit.
#[test]
fn window_constructors_size_through_the_shared_startup_rule() {
    let src = production_source();
    let attrs = src
        .split("fn window_attributes(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("window_attributes body");
    assert!(
        attrs.contains("let (w, h) = startup_inner_size(&self.cfg, monitor);")
            && attrs.contains("attrs.with_inner_size(winit::dpi::LogicalSize::new(w, h));"),
        "window_attributes must apply the shared startup size as a LogicalSize"
    );
    assert!(
        !attrs.contains("PhysicalSize"),
        "startup sizing must not build a PhysicalSize"
    );
    assert_eq!(
        src.matches("self.window_attributes(").count(),
        2,
        "exactly the two window constructors build attributes"
    );
    assert!(
        src.contains(
            "let monitor = startup_monitor(event_loop);\n        let mut attrs = self.window_attributes(state, monitor);"
        ) && src.contains("self.window_attributes(self.cfg.window_state, monitor)"),
        "both constructors must pass the startup monitor"
    );
    assert_eq!(
        src.matches("self.startup_surface(").count(),
        3,
        "pre-launch and both constructors use the exact configured-grid rule"
    );
    assert!(
        src.contains("let (w, h) = startup_inner_size(&self.cfg, monitor);\n            let scale = monitor.map_or(1.0, |m| m.scale);"),
        "the restore planner's fallback surface must be the same rule in physical pixels"
    );
}

#[test]
fn later_saved_windows_do_not_depend_on_the_first_restore_succeeding() {
    let src = production_source();
    let restore = src
        .split("if let Some((first, rest)) = wins.split_first() {")
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

/// Drift guard: a mouse press / wheel is swallowed
/// whenever a non-context-menu modal is open, so it can't fall through to
/// the tab bar / pane focus / mouse-tracking behind the dialog. A lone
/// context menu is the one exception — it owns its click/scroll paths and a
/// right-click relocates it — and with nothing open the pointer passes
/// through as normal.
#[test]
fn modal_swallows_pointer_except_lone_context_menu() {
    // A non-context-menu modal (e.g. search/palette/settings) is open.
    assert!(modal_swallows_pointer(true, false));
    // A lone context menu does NOT swallow — its own paths handle it.
    assert!(!modal_swallows_pointer(true, true));
    // Nothing open: the pointer falls through to tabs/panes normally.
    assert!(!modal_swallows_pointer(false, false));
    // Defensive: even if both were somehow set, the context-menu
    // exclusion wins (its dedicated handling ran first).
    assert!(!modal_swallows_pointer(true, true));
}

/// The search bar reserves a lane below the grid; the grid stays
/// mouse-interactive while it is open (Terminator parity). A press is
/// routed by where it lands, motion and release by whether an editor drag
/// is live, and a closed bar routes nothing.
#[test]
fn search_bar_blocks_new_reports_but_preserves_captured_drags() {
    use super::search_bar_blocks_mouse_report as blocks;
    for pressed in [false, true] {
        for motion in [false, true] {
            for held in [false, true] {
                assert!(!blocks(false, pressed, motion, held));
            }
        }
    }
    assert!(blocks(true, true, true, false));
    assert!(blocks(true, true, false, false));
    assert!(blocks(true, true, false, true));
    assert!(!blocks(true, true, true, true));
    assert!(!blocks(true, false, false, true));
    assert!(!blocks(true, false, false, false));
}

/// The bar shows the last result while a new search is only waiting for
/// the typing pause or has run briefly; a scan that is really slow says so.
#[test]
fn searching_is_shown_only_once_a_scan_is_actually_slow() {
    use super::displayed_search_status;
    use kettle_render::SearchStatus;
    let t0 = std::time::Instant::now();
    let ms = std::time::Duration::from_millis;
    let mut search = crate::search_input::SearchState {
        editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
        status: SearchStatus::NoMatch,
        ..Default::default()
    };
    search.note_edit(t0);
    assert_eq!(
        displayed_search_status(&search, t0 + ms(450)),
        SearchStatus::NoMatch
    );
    // The full scan starts when the pause ends and clears its deadline,
    // as `update_search` does. The grace runs 200 ms from there.
    search.unlimited_retry_at = None;
    assert_eq!(
        displayed_search_status(&search, t0 + ms(650)),
        SearchStatus::NoMatch
    );
    assert_eq!(
        displayed_search_status(&search, t0 + ms(750)),
        SearchStatus::Searching
    );

    // Finished searches are shown as automation sees them.
    search.status = SearchStatus::Match;
    search.visible_truncated = true;
    assert_eq!(
        displayed_search_status(&search, t0 + ms(10)),
        SearchStatus::Limited
    );
}

/// The bar keeps the keys a native find field keeps and hands every
/// other chord to Kettle's shortcuts, on both platforms.
#[test]
fn the_search_bar_keeps_editing_keys_and_passes_shortcuts_on() {
    use winit::keyboard::{Key, ModifiersState as M, NamedKey};
    let ch = |c: &str| Key::Character(c.into());
    let named = Key::Named;
    let claims =
        |key: &Key, mods: M, macos: bool| super::search_bar_claims(key, mods, macos, true, false);
    let (cmd, ctrl, alt, shift) = (M::SUPER, M::CONTROL, M::ALT, M::SHIFT);
    // macOS: the find field's own keys.
    for (key, mods) in [
        (named(NamedKey::Escape), M::empty()),
        (named(NamedKey::Enter), shift),
        (named(NamedKey::Tab), shift),
        (named(NamedKey::Backspace), cmd),
        (named(NamedKey::Backspace), alt),
        (named(NamedKey::Delete), cmd),
        (named(NamedKey::ArrowLeft), cmd | shift),
        (named(NamedKey::ArrowRight), alt | shift),
        (named(NamedKey::Home), shift),
        (ch("a"), cmd),
        (ch("c"), cmd),
        (ch("x"), cmd),
        (ch("v"), cmd),
        (ch("z"), cmd | shift),
        (ch("g"), cmd | shift),
        (ch("e"), ctrl),
        (ch("k"), ctrl),
    ] {
        assert!(claims(&key, mods, true), "macOS keeps {key:?} {mods:?}");
    }
    // macOS: Kettle's shortcuts.
    for (key, mods) in [
        (ch("t"), cmd),
        (ch("w"), cmd),
        (ch("="), cmd),
        (ch("1"), cmd),
        (ch("k"), cmd),
        (ch("p"), cmd | shift),
        (named(NamedKey::ArrowLeft), cmd | alt),
        (named(NamedKey::ArrowLeft), ctrl | cmd),
        (named(NamedKey::ArrowUp), cmd),
        (named(NamedKey::ArrowUp), shift),
        (named(NamedKey::PageUp), shift),
        (named(NamedKey::Enter), cmd),
        (named(NamedKey::F11), M::empty()),
        (ch(","), ctrl),
        (ch("k"), ctrl | shift),
        (named(NamedKey::Backspace), ctrl),
    ] {
        assert!(
            !claims(&key, mods, true),
            "macOS passes on {key:?} {mods:?}"
        );
    }
    // Linux: the find field's own keys.
    for (key, mods) in [
        (named(NamedKey::Backspace), ctrl),
        (named(NamedKey::ArrowLeft), ctrl | shift),
        (ch("a"), ctrl),
        (ch("x"), ctrl),
        (ch("c"), ctrl | shift),
        (ch("v"), ctrl | shift),
        (ch("g"), ctrl | shift),
        (ch("z"), ctrl | shift),
    ] {
        assert!(claims(&key, mods, false), "Linux keeps {key:?} {mods:?}");
    }
    // Linux: Kettle's shortcuts, including the Ctrl+Shift chords that
    // share a letter with an editing key.
    for (key, mods) in [
        (ch("a"), ctrl | shift),
        (ch("x"), ctrl | shift),
        (ch("t"), ctrl | shift),
        (ch("k"), ctrl | shift),
        (ch("e"), ctrl),
        (named(NamedKey::ArrowLeft), alt),
        (named(NamedKey::Home), ctrl),
        (named(NamedKey::PageDown), ctrl),
        (ch("1"), alt),
        (named(NamedKey::Backspace), alt),
        (named(NamedKey::Delete), alt),
    ] {
        assert!(
            !claims(&key, mods, false),
            "Linux passes on {key:?} {mods:?}"
        );
    }
    // A focused button takes Space; the query types it.
    assert!(super::search_bar_claims(
        &named(NamedKey::Space),
        M::empty(),
        true,
        false,
        false
    ));
    assert!(!super::search_bar_claims(
        &named(NamedKey::Space),
        M::empty(),
        true,
        true,
        false
    ));
    // Caret keys belong to the query, not to a focused button.
    assert!(claims(&named(NamedKey::ArrowLeft), M::empty(), true));
    assert!(!super::search_bar_claims(
        &named(NamedKey::ArrowLeft),
        M::empty(),
        true,
        false,
        false
    ));
}

/// An Alt chord bound to a shortcut runs it rather than typing: Linux's
/// Alt+1 goes to tab 1. AltGr (Ctrl+Alt) keeps composing characters, and
/// an unbound Alt chord still types what it produced.
#[test]
fn a_bound_alt_chord_runs_its_shortcut_instead_of_typing() {
    use super::search_bar_text as text;
    use winit::keyboard::ModifiersState as M;
    assert_eq!(text(Some("1"), M::ALT, true, false), None);
    assert_eq!(text(Some("1"), M::ALT, false, false), Some("1"));
    // Windows AltGr composes; elsewhere Ctrl+Alt is a real chord.
    assert_eq!(text(Some("@"), M::ALT | M::CONTROL, true, true), Some("@"));
    assert_eq!(text(Some("t"), M::ALT | M::CONTROL, true, false), None);
    assert_eq!(text(Some("a"), M::SHIFT, true, false), Some("a"));
    assert_eq!(text(None, M::ALT, false, false), None);
}

/// Turning Wrap off while a step has already wrapped stops it at the edge
/// instead of landing on a match past it.
#[test]
fn turning_wrap_off_stops_a_step_that_already_wrapped() {
    use kettle_core::{SearchDirection, SearchPoint};
    let job = |direction, wrapped| crate::search_input::BackgroundSearch {
        token: kettle_core::SearchScanToken {
            query_revision: 1,
            output_generation: 1,
            layout: kettle_core::SearchLayout {
                columns: 80,
                screen_lines: 24,
                history_size: 10_000,
            },
        },
        direction,
        cursor: SearchPoint::new(0, 0),
        edge: SearchPoint::new(10, 79),
        wrap_anchor: SearchPoint::new(5, 0),
        wrapped,
        nearby: false,
        navigation: true,
        had_focus: true,
        output_drifted: false,
    };
    let span = kettle_core::SearchSpan {
        start: SearchPoint::new(1, 0),
        end: SearchPoint::new(1, 1),
    };
    for (direction, status) in [
        (SearchDirection::Forward, kettle_render::SearchStatus::End),
        (SearchDirection::Reverse, kettle_render::SearchStatus::Start),
    ] {
        let mut search = crate::search_input::SearchState {
            editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
            wrap: false,
            focused: Some(span),
            status: kettle_render::SearchStatus::Searching,
            background: Some(job(direction, true)),
            ..Default::default()
        };
        super::search_wrap_toggled(&mut search, std::time::Instant::now());
        assert!(search.background.is_none());
        assert_eq!(search.status, status);
    }
    // A step that never had a match reports no match, and one that output
    // drifted under stays limited, as they would at the edge.
    for (had_focus, output_drifted, status) in [
        (false, false, kettle_render::SearchStatus::NoMatch),
        (true, true, kettle_render::SearchStatus::Limited),
    ] {
        let mut search = crate::search_input::SearchState {
            editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
            wrap: false,
            status: kettle_render::SearchStatus::Searching,
            background: Some(crate::search_input::BackgroundSearch {
                had_focus,
                output_drifted,
                ..job(SearchDirection::Forward, true)
            }),
            ..Default::default()
        };
        super::search_wrap_toggled(&mut search, std::time::Instant::now());
        assert_eq!(
            search.status, status,
            "had_focus={had_focus} drifted={output_drifted}"
        );
    }
    // A step that has not wrapped yet keeps going.
    let mut search = crate::search_input::SearchState {
        editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
        wrap: false,
        focused: Some(span),
        status: kettle_render::SearchStatus::Searching,
        background: Some(job(SearchDirection::Forward, false)),
        ..Default::default()
    };
    super::search_wrap_toggled(&mut search, std::time::Instant::now());
    assert!(search.background.is_some());
}

/// Shortcuts that would type into the terminal do nothing while the bar
/// has the keyboard; Copy and Paste act on the query.
#[test]
fn shortcuts_that_type_into_the_terminal_do_nothing_while_searching() {
    use super::{SearchBarShortcut as K, search_bar_shortcut_kind as kind};
    use kettle_config::keybinds::Action;
    assert_eq!(kind(&Action::SendText("\x15".into())), K::Typing);
    assert_eq!(kind(&Action::SendNewline), K::Typing);
    assert_eq!(kind(&Action::InsertPaneNumber), K::Typing);
    assert_eq!(kind(&Action::PastePrimary), K::Typing);
    assert_eq!(kind(&Action::Copy), K::Copy);
    assert_eq!(kind(&Action::Paste), K::Paste);
    for action in [
        Action::NewTab,
        Action::CloseTab,
        Action::IncreaseFontSize,
        Action::ScrollPageUp,
        Action::FocusLeft,
        Action::ToggleZoom,
        Action::CommandPalette,
        Action::GotoTab(0),
    ] {
        assert_eq!(kind(&action), K::Run, "{action:?}");
    }
}

/// Both keyboard paths hand a key the bar did not use to the shortcuts.
#[test]
fn keys_the_search_bar_does_not_use_reach_the_shortcuts() {
    let src = super::production_source();
    let native = src
        .split("if ws.search.open {\n                    if event.repeat")
        .nth(1)
        .and_then(|rest| {
            rest.split("remember_closing_key(ws, key_modal, event.physical_key);")
                .next()
        })
        .expect("native search key branch");
    assert!(
        native.contains(
            "if !self.search_key(ws, &event.logical_key, Some(&event.physical_key), text)"
        ) && native.contains("self.search_bar_shortcut("),
        "a native key the bar does not use must reach the shortcuts"
    );
    let ctl = src
        .split("TextModal::Search => {")
        .nth(1)
        .and_then(|rest| rest.split("}\n            }").next())
        .expect("control-plane search key branch");
    assert!(
        ctl.contains("if !self.search_key(ws, key, None, text) {")
            && ctl.contains("self.search_bar_shortcut(ws, key, None, false, event_loop);"),
        "a dispatched key the bar does not use must reach the shortcuts"
    );
}

/// A focused toggle or Close acts once per press; Previous, Next and
/// the query keep repeating.
#[test]
fn held_space_or_enter_presses_a_search_button_once() {
    use kettle_render::SearchControl as C;
    use winit::keyboard::{Key, NamedKey};
    let space = Key::Named(NamedKey::Space);
    let enter = Key::Named(NamedKey::Enter);
    let typed_space = Key::Character(" ".into());
    for control in [C::Wrap, C::Case, C::Invert, C::Close] {
        for key in [&space, &enter, &typed_space] {
            assert!(
                super::search_button_ignores_repeat(control, key),
                "{control:?} {key:?}"
            );
        }
    }
    for control in [C::Editor, C::Previous, C::Next] {
        assert!(
            !super::search_button_ignores_repeat(control, &enter),
            "{control:?}"
        );
    }
    assert!(!super::search_button_ignores_repeat(
        C::Wrap,
        &Key::Named(NamedKey::Backspace)
    ));
}

/// Holding the key that closed the bar sends nothing to the terminal
/// until it is released; the next press is the terminal's.
#[test]
fn the_key_that_closed_search_does_not_repeat_into_the_terminal() {
    use winit::event::ElementState;
    use winit::keyboard::PhysicalKey;
    let space = PhysicalKey::Code(winit::keyboard::KeyCode::Space);
    let other = PhysicalKey::Code(winit::keyboard::KeyCode::KeyA);
    let mut closing = std::collections::HashSet::from([space]);
    let pressed = ElementState::Pressed;
    assert!(super::drop_closing_key_repeat(
        &mut closing,
        space,
        pressed,
        true
    ));
    assert!(!super::drop_closing_key_repeat(
        &mut closing,
        other,
        pressed,
        true
    ));
    assert!(closing.contains(&space));
    assert!(!super::drop_closing_key_repeat(
        &mut closing,
        space,
        ElementState::Released,
        false
    ));
    assert!(closing.is_empty());
    assert!(!super::drop_closing_key_repeat(
        &mut closing,
        space,
        pressed,
        true
    ));
}

/// Toggling Wrap with a match on screen keeps the match and clears a step
/// status that described the old setting.
#[test]
fn wrap_toggle_keeps_the_match_and_clears_the_old_step_status() {
    use kettle_render::SearchStatus as S;
    let span = kettle_core::SearchSpan {
        start: kettle_core::SearchPoint::new(3, 0),
        end: kettle_core::SearchPoint::new(3, 4),
    };
    for (before, after) in [
        (S::End, S::Match),
        (S::Start, S::Match),
        (S::Wrapped, S::Match),
        (S::Limited, S::Limited),
    ] {
        let mut search = crate::search_input::SearchState {
            editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
            focused: Some(span),
            status: before,
            ..Default::default()
        };
        super::search_wrap_toggled(&mut search, std::time::Instant::now());
        assert_eq!(search.focused, Some(span));
        assert_eq!(search.status, after, "{before:?}");
    }
    let mut search = crate::search_input::SearchState {
        editor: crate::search_input::SearchEditor::from_text("zz".into(), 64),
        status: S::NoMatch,
        ..Default::default()
    };
    super::search_wrap_toggled(&mut search, std::time::Instant::now());
    assert_eq!(search.status, S::Searching, "no match: search again");
}

/// A toggle restarts the search from the match on screen, or from the
/// viewport when that match has been scrolled away.
#[test]
fn a_toggle_restarts_search_from_what_is_on_screen() {
    use kettle_core::SearchPoint as P;
    let span = kettle_core::SearchSpan {
        start: P::new(-30, 2),
        end: P::new(-30, 6),
    };
    // 24 lines scrolled back 40: lines -40 ..= -17 are on screen.
    let viewport = Some((40, 24, 79));
    assert_eq!(
        super::search_anchor_for(Some(span), false, viewport),
        Some(P::new(-30, 2))
    );
    assert_eq!(
        super::search_anchor_for(Some(span), true, viewport),
        Some(P::new(-30, 6))
    );
    // Back at the bottom, the match is off screen.
    let bottom = Some((0, 24, 79));
    assert_eq!(
        super::search_anchor_for(Some(span), false, bottom),
        Some(P::new(0, 0))
    );
    assert_eq!(
        super::search_anchor_for(Some(span), true, bottom),
        Some(P::new(23, 79))
    );
    // With no pane to read, the match is still the best guess.
    assert_eq!(
        super::search_anchor_for(Some(span), false, None),
        Some(P::new(-30, 2))
    );
    assert_eq!(super::search_anchor_for(None, false, None), None);
}

#[test]
fn search_pointer_route_keeps_the_grid_live_under_the_bar() {
    use super::{SearchPointerRoute, search_pointer_route};

    for press in [None, Some(true), Some(false)] {
        for dragging in [false, true] {
            assert_eq!(
                search_pointer_route(false, press, dragging),
                SearchPointerRoute::Closed,
                "a closed bar routes nothing (press={press:?}, dragging={dragging})"
            );
        }
    }
    assert_eq!(
        search_pointer_route(true, Some(true), false),
        SearchPointerRoute::Bar,
        "a press inside the lane belongs to the bar's controls"
    );
    assert_eq!(
        search_pointer_route(true, Some(false), false),
        SearchPointerRoute::Grid,
        "a press above the lane is ordinary grid input"
    );
    assert_eq!(
        search_pointer_route(true, Some(false), true),
        SearchPointerRoute::Grid,
        "a fresh press decides by position even if a stale editor drag flag survived"
    );
    assert_eq!(
        search_pointer_route(true, None, true),
        SearchPointerRoute::Bar,
        "motion and release follow a live editor drag"
    );
    assert_eq!(
        search_pointer_route(true, None, false),
        SearchPointerRoute::Grid,
        "motion and release without an editor drag belong to the grid gesture"
    );
}

/// Source pins for the routing above: every pointer arm, native and
/// control-plane, must consult the same helper, the pointer gate must not
/// treat the bar as a pointer-owning modal, the cursor icon must follow
/// the pointer gate (I-beam over the grid while searching), and the
/// right-click menu must leave the bar in place.
#[test]
fn search_bar_pointer_routing_is_wired_on_both_input_paths() {
    let src = production_source();
    let native_press = src
        .split("WindowEvent::MouseInput {\n                state: ElementState::Pressed,")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::MouseInput {").next())
        .expect("native Pressed arm");
    let native_release = src
        .split("WindowEvent::MouseInput {\n                state: ElementState::Released,")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native Released arm");
    let native_move = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native CursorMoved arm");
    let ctl_press = src
        .split("fn ctl_mouse_press(")
        .nth(1)
        .and_then(|body| body.split("fn ctl_mouse_release").next())
        .expect("ctl_mouse_press body");
    let ctl_release = src
        .split("fn ctl_mouse_release(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("ctl_mouse_release body");
    let ctl_move = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("ctl_mouse_move body");

    for (label, body) in [("native press", native_press), ("ctl press", ctl_press)] {
        assert!(
            body.contains("Some(self.cursor_in_search_bar(ws)),")
                && body.contains("== SearchPointerRoute::Bar"),
            "{label} must route a search-time press by the bar's rectangle"
        );
        assert!(
            !body.contains("if ws.search.open {"),
            "{label} must not swallow every press while the bar is open"
        );
    }
    for (label, body) in [
        ("native move", native_move),
        ("ctl move", ctl_move),
        ("native release", native_release),
        ("ctl release", ctl_release),
    ] {
        assert!(
            body.contains(
                "search_pointer_route(ws.search.open, None, ws.search.pointer_captured())"
            ),
            "{label} must route search-time motion/release by the live bar gesture"
        );
        assert!(
            !body.contains("if ws.search.open {"),
            "{label} must not swallow every event while the bar is open"
        );
    }
    assert!(
        native_press.contains(
            "if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some())"
        ),
        "the native press gate must ignore the search bar"
    );
    let normalized = src.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized.contains(
            "let chrome = chrome_cursor_icon( self.cursor_in_chrome_band(ws), self.pointer_modal_open(ws), );"
        ) || normalized.contains(
            "let chrome = chrome_cursor_icon(self.cursor_in_chrome_band(ws), self.pointer_modal_open(ws));"
        ),
        "the cursor icon over the grid must not turn into the arrow while searching"
    );
    let open_menu = src
        .split("fn open_context_menu(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("open_context_menu body");
    assert!(
        open_menu.contains("self.close_modals_for_context_menu(ws);")
            && !open_menu.contains("self.close_all_modals(ws);"),
        "the right-click menu must leave an open search bar in place"
    );
    let note_focus = src
        .split("fn note_focus_change(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("note_focus_change body");
    assert!(
        note_focus.contains("self.retarget_search_to_focus(ws);"),
        "an open search bar must follow pane focus"
    );
    let begin_selection = src
        .split("fn begin_or_extend_mouse_selection(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("begin_or_extend_mouse_selection body");
    assert!(
        begin_selection.contains("ws.search.editor.clear_selection();"),
        "a grid press must drop the editor selection so Copy reaches the grid"
    );
    // Keyboard, file drops, and focus-follows-mouse keep the full gate:
    // search does own the keyboard.
    assert!(
        src.contains(
            "if self.any_modal_open(ws) {\n                    return;\n                }"
        ),
        "DroppedFile must still return while any modal, including search, is open"
    );
    assert!(
        native_move.contains("&& !self.any_modal_open(ws)"),
        "focus-follows-mouse must stay off while the bar is open, or the bar would retarget as the pointer drifts"
    );
}

/// An open bar follows focus: the query and toggles come along, the pane
/// id changes, and a non-empty query starts searching the new pane at once.
#[test]
fn search_follows_focus_with_its_query_and_toggles() {
    use super::{SearchCarryOver, fresh_search_state, search_retarget};

    assert_eq!(
        search_retarget(Some(1), Some(1)),
        None,
        "already on the focused pane"
    );
    assert_eq!(search_retarget(Some(1), None), None, "nothing focused");
    assert_eq!(search_retarget(None, Some(2)), Some(2));
    assert_eq!(search_retarget(Some(1), Some(2)), Some(2));

    let now = std::time::Instant::now();
    let carried = SearchCarryOver {
        query: "needle".into(),
        wrap: false,
        case_mode: kettle_config::SearchCaseSensitivity::Always,
        invert: true,
        focused_control: kettle_render::SearchControl::Next,
    };
    let moved = fresh_search_state(2, carried.clone(), 7, now);
    assert!(moved.open);
    assert_eq!(moved.target_pane, Some(2));
    assert_eq!(moved.query(), "needle");
    assert!(!moved.wrap);
    assert_eq!(
        moved.case_mode,
        kettle_config::SearchCaseSensitivity::Always
    );
    assert!(moved.invert);
    assert_eq!(moved.focused_control, kettle_render::SearchControl::Next);
    assert_eq!(moved.pre_open_display_offset, Some(7));
    assert_eq!(
        moved.status,
        kettle_render::SearchStatus::Searching,
        "a carried query scans the new pane immediately"
    );
    assert!(
        moved.anchor.is_none(),
        "the anchor is derived after the lane settles"
    );
    assert!(moved.focused.is_none() && moved.visible.is_empty());

    let fresh = fresh_search_state(
        3,
        SearchCarryOver {
            query: String::new(),
            ..carried
        },
        0,
        now,
    );
    assert_eq!(fresh.status, kettle_render::SearchStatus::Typing);
    assert_eq!(fresh.target_pane, Some(3));
}

/// Agent/control-plane mouse presses must stay in lock-step with the real
/// winit mouse path for app chrome. The live interaction smoke drives these
/// branches through `kettle ctl send_mouse`; this source guard prevents a
/// future refactor from dropping the modal gate or turning the new-tab
/// dropdown arrow back into a plain `+` click.
#[test]
fn ctl_mouse_press_handles_app_chrome_before_panes() {
    let src = production_source();
    let body = src
        .split("fn ctl_mouse_press(")
        .nth(1)
        .and_then(|s| s.split("fn ctl_mouse_release").next())
        .expect("ctl_mouse_press body present");
    let report = body
        .find("self.send_mouse(ws, bcode, true, false)")
        .expect("terminal mouse reporting");
    let menu = body
        .find("self.open_context_menu(ws, px, py, offer_copied)")
        .expect("local context menu");
    assert!(
        report < menu,
        "terminal reporting must precede the right-click menu"
    );
    assert!(
        body.contains("&& self.settings_mouse(ws, if bcode == 2 { -1 } else { 1 }, true)"),
        "ctl_mouse_press must route Settings modal clicks through settings_mouse"
    );
    assert!(
        body.contains(
            "if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some())"
        ),
        "ctl_mouse_press must swallow pane/tab clicks behind pointer-owning modals"
    );
    assert!(
        !body.contains("modal_swallows_pointer(self.any_modal_open(ws)"),
        "the search bar is not a pointer-owning modal; ctl_mouse_press must not gate on it"
    );
    assert!(
        body.contains("rect_contains(bar.new_tab_menu, px, py)")
            && body.contains("self.open_new_tab_menu(ws, ax, ay + ah);")
            && body.find("rect_contains(bar.new_tab_menu, px, py)")
                < body.find("rect_contains(bar.new_tab, px, py)"),
        "ctl_mouse_press must check the new-tab dropdown arrow before the + button"
    );
    assert!(
        body.contains("tab_bar_pointer_region_contains(&bar, px, py)"),
        "ctl_mouse_press must use the full vertical tab-bar band, not only its first row"
    );
    assert_eq!(
        src.matches("tab_bar_pointer_region_contains(&bar, px, py)")
            .count(),
        2,
        "both the control-plane and real-window mouse paths must use the full tab-bar band"
    );
}

/// Drift guard. Mouse drag-to-resize of split dividers is
/// wired across three event handlers (press starts the drag, move applies
/// the new ratio, up/focus-loss ends it) plus a hover resize-cursor. A
/// behavioral test needs a window + a real mouse drag (and the geometry math
/// is already unit-tested in `mux::node_tests`), so pin the wiring at the
/// source level.
///
/// Every needle below is matched against the production slice only, so a
/// signature change that leaves the guard behind fails it instead of
/// feeding it its own text.
#[test]
fn split_divider_drag_is_wired() {
    let src = production_source();
    // Press starts the drag from a seam hit-test.
    assert!(
        src.contains("let Some(drag) = self.split_drag_at(ws, area, px, py)"),
        "left-press must start a split-divider drag on a seam hit"
    );
    assert!(
        src.contains("ws.dragging_split = Some(drag);"),
        "the seam hit must actually latch the drag"
    );
    // Move applies the new ratio.
    assert!(
        src.contains("if ws.dragging_split.is_some() {")
            && src.contains("ws.mux.set_split_ratio(ws.mux.active, &path, ratio);"),
        "CursorMoved must apply the dragged split ratio"
    );
    // Up + focus-loss end the drag.
    assert!(
        src.contains("End any split-divider drag on left-button up."),
        "the split drag must be cleared on left-button up"
    );
    assert!(
        src.contains("A focus loss also ends any split-divider drag."),
        "the split drag must be cleared on focus loss"
    );
    // Hover shows a resize cursor.
    assert!(
        src.contains(".or_else(|| self.split_seam_hover_icon(ws))"),
        "hovering a divider must show the resize cursor"
    );
}

#[test]
fn pane_reaping_cancels_divider_drags() {
    let src = production_source();
    let mut count = 0;
    for after_reap in src.split("let reaped = ws.mux.reap();").skip(1) {
        let flat: String = after_reap.split_whitespace().collect();
        assert!(flat.starts_with("ifreaped.layout_changed{ws.dragging_split=None;}"));
        count += 1;
    }
    assert_eq!(count, 2);
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

/// The visual bell's time profile: instant on, quadratic ease-out, gone
/// at `BELL_FLASH_DURATION`. The renderer scales the configured lightness
/// step by this value, so its shape is the flash's perceived shape.
#[test]
fn bell_flash_ramp_is_instant_on_and_eases_out_within_the_duration() {
    use super::{BELL_FLASH_DURATION, bell_flash_ramp};
    use std::time::Duration;

    assert_eq!(BELL_FLASH_DURATION, Duration::from_millis(300));
    assert_eq!(
        bell_flash_ramp(Duration::ZERO),
        1.0,
        "the frame it rang is the peak"
    );
    assert!((bell_flash_ramp(Duration::from_millis(150)) - 0.25).abs() < 1e-6);
    assert!((bell_flash_ramp(Duration::from_millis(30)) - 0.81).abs() < 1e-6);
    assert_eq!(bell_flash_ramp(BELL_FLASH_DURATION), 0.0);
    assert_eq!(bell_flash_ramp(Duration::from_secs(5)), 0.0);
    let mut last = 1.0_f32;
    for ms in (0..=300).step_by(10) {
        let value = bell_flash_ramp(Duration::from_millis(ms));
        assert!(value <= last, "the ramp must never rise again ({ms} ms)");
        assert!((0.0..=1.0).contains(&value));
        last = value;
    }
    // Ease-out: the first half sheds most of the strength.
    assert!(bell_flash_ramp(Duration::from_millis(150)) < 0.5);
}

/// The bell flash is per pane: the drain stamps every ringing pane, the
/// frame builder turns each stamp into the pane's ramp, the idle loop
/// keeps the animation wake alive while any stamp is live and drops
/// expired stamps with one erasing repaint, and nothing keeps a
/// window-level bell timestamp any more.
#[test]
fn the_bell_flash_is_stamped_per_pane_and_pruned_on_expiry() {
    let src = production_source();
    let drain = src
        .split("if self.cfg.bell.visual() {")
        .nth(1)
        .and_then(|body| body.split("if self.cfg.bell.attention()").next())
        .expect("visual bell branch in drain_events");
    assert!(
        drain.contains("for id in &bell_panes {")
            && drain.contains("ws.bell_flashes.insert(*id, now);"),
        "every ringing pane must get its own flash stamp"
    );
    assert!(
        src.contains("bell_flash: *bell_flash,")
            && src.contains(".map_or(0.0, |rang| bell_flash_ramp(rang.elapsed())),"),
        "the frame builder must feed each pane its own ramp"
    );
    let idle = src
        .split("let bells_expired = ws")
        .nth(1)
        .and_then(|body| body.split("let bell_active =").next())
        .expect("bell expiry block in about_to_wait");
    assert!(
        idle.contains(".retain(|_, rang| rang.elapsed() < BELL_FLASH_DURATION);")
            && idle.contains("w.request_redraw();"),
        "expired stamps must be dropped and erased with one more repaint"
    );
    assert!(
        src.contains("let bell_active = !render_hidden && !ws.bell_flashes.is_empty();"),
        "the animation wake must key off live per-pane stamps"
    );
    assert!(
        !src.contains("last_bell"),
        "the window-level bell timestamp is gone; a flash belongs to the pane that rang"
    );
}

#[test]
fn the_pane_bell_indicator_is_wired_to_real_pane_state() {
    let src = production_source();
    let at = src.find("metas.push((").expect("frame builder");
    let push: String = src[at..].chars().take(900).collect();
    assert!(
        push.contains("p.bell,"),
        "the frame must carry each pane's OWN bell state; a literal here is \
             what made `icon_bell` undrawable"
    );
    // Bounded to the drain body, with the needle assembled at runtime, so
    // the guard cannot match its own text and keep passing after the
    // production call is deleted.
    let needle = ["ws.mux.clear_focused", "_pane_bell();"].concat();
    let drain = src
        .split("fn drain_events(&mut self, ws: &mut WindowState) {")
        .nth(1)
        .and_then(|b| b.split("\n    fn ").next())
        .expect("drain_events body");
    assert!(
        drain.contains(needle.as_str()),
        "the focused pane's bell must be cleared on the drain path, where \
             the current focus is always known — clearing it only on \
             focus-change events leaks the latch on every other route focus \
             can take (tab switch, reap promoting a sibling, ctl, restore)"
    );
}

/// An OSC 52 read answers with every write the program made before it,
/// including one latched after the drain took the pane's writes.
#[test]
fn an_osc52_read_applies_earlier_writes_first() {
    let src = production_source();
    let load = src
        .split("TermEvent::ClipboardLoad(target, fmt) => {")
        .nth(1)
        .and_then(|body| {
            body.split("TermEvent::TextAreaSizeRequest(fmt) => {")
                .next()
        })
        .expect("ClipboardLoad arm");
    let applied = load.find("self.store_osc52_writes(pane.term.take_clipboard_stores());");
    let read = load.find("get_osc52_clipboard(");
    assert!(applied.is_some() && applied < read, "{applied:?} {read:?}");
}

/// A key event queued behind a paste still being prepared keeps its press and
/// release on the same panes, as one sent at once does. A queued press once
/// recorded no recipients, so a release sent after the paste reached no pane,
/// and a queued release went to whatever had focus by then: either way a
/// Kitty-reporting program was left with a stuck key.
#[test]
fn queued_key_events_keep_press_and_release_on_the_same_panes() {
    let src = production_source();
    let body = src
        .split("fn defer_terminal_input(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("defer_terminal_input body");
    assert!(
        body.contains(".filter(|_| !prepare_input)")
            && body.contains("ws.terminal_key_recipients.remove(&event.physical_key)"),
        "a queued release takes the panes its press reached"
    );
    assert!(
        body.contains(".filter(|_| prepare_input)")
            && body.contains("result.is_some_and(|result| result.is_queued())")
            && body.contains(".entry(event.physical_key)"),
        "a press that was queued records the panes it will reach"
    );
}

#[test]
fn process_status_cannot_reap_a_pane_before_the_ordered_pty_exit() {
    let src = production_source();
    let drain = src
        .split("fn drain_events(&mut self, ws: &mut WindowState) {")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("drain_events body");
    let exit_latch = drain
        .split("if exited && !pane.closed {")
        .nth(1)
        .and_then(|body| body.split("\n            }\n").next())
        .expect("ordered exit latch");
    assert!(
        exit_latch.contains("apply_pane_exit_action(")
            && exit_latch.contains("self.cfg.exit_action"),
        "the reader's ordered exit must latch reaping and apply exit-action policy"
    );
    // The exit latch is read before the pane's other latches and queue, and
    // acted on after them: output the reader published before the exit is
    // never left behind it.
    let read = drain.find("let exited = pane.term.take_exited();");
    let taken = drain.find("pane.term.take_title()");
    let queue = drain.find("while let Ok(ev) = pane.rx.try_recv() {");
    let applied = drain.find("if exited && !pane.closed {");
    assert!(
        read.is_some() && read < taken && taken < queue && queue < applied,
        "exit read first, applied last: {read:?} {taken:?} {queue:?} {applied:?}"
    );

    let status_arm = drain
        .split("TermEvent::ChildExit(_) => {")
        .nth(1)
        .and_then(|body| body.split("\n                    _ => {}").next())
        .expect("advisory ChildExit arm");
    for forbidden in [
        "exit_observed",
        "pane.closed",
        "pane.held",
        "pending_restarts",
        "apply_pane_exit_action",
    ] {
        assert!(
            !status_arm.contains(forbidden),
            "ChildExit status must not perform lifecycle action {forbidden:?} \
                 before the final PTY drain"
        );
    }
    assert!(
        drain.contains("pane.term.direct_child_exit_at()")
            && drain.contains("pane.term.begin_pty_output_close()")
            && drain.contains("bounded_pty_exit_action(")
            && drain.matches("apply_pane_exit_action(").count() >= 2,
        "the ConPTY path must have a bounded post-child drain and fallback"
    );
}

#[test]
fn windows_child_exit_drain_has_two_distinct_bounds() {
    use super::BoundedPtyExitPhase::{Failed, InProgress, NotStarted};
    let bound = kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT;
    assert_eq!(
        super::bounded_pty_exit_action(bound - std::time::Duration::from_millis(1), NotStarted,),
        super::BoundedPtyExitAction::Wait
    );
    assert_eq!(
        super::bounded_pty_exit_action(bound, NotStarted),
        super::BoundedPtyExitAction::BeginClose
    );
    assert_eq!(
        super::bounded_pty_exit_action(bound * 2, NotStarted),
        super::BoundedPtyExitAction::BeginClose,
        "a delayed first wake must close before applying policy"
    );
    assert_eq!(
        super::bounded_pty_exit_action(
            bound * 2,
            InProgress(bound - std::time::Duration::from_millis(1)),
        ),
        super::BoundedPtyExitAction::Wait,
        "the second bound starts when close actually starts"
    );
    assert_eq!(
        super::bounded_pty_exit_action(bound, InProgress(bound)),
        super::BoundedPtyExitAction::ApplyPolicy,
        "a parked close cannot hold GUI policy forever"
    );
    assert_eq!(
        super::bounded_pty_exit_action(
            bound * 2,
            Failed {
                retry_in: std::time::Duration::from_millis(1),
            },
        ),
        super::BoundedPtyExitAction::Wait,
        "a failed worker start must not be recorded as an in-progress close"
    );
    assert_eq!(
        super::bounded_pty_exit_action(
            bound * 2,
            Failed {
                retry_in: std::time::Duration::ZERO,
            },
        ),
        super::BoundedPtyExitAction::BeginClose,
        "worker-start failure is retried instead of applying Hold to a live master"
    );
}

#[test]
fn lifecycle_wakes_are_drained_before_the_output_generation_gate() {
    let src = production_source();
    let inner = src
        .split("fn user_event_inner(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("user_event_inner body");
    let wake = inner
        .split("UserEvent::Wakeup => {")
        .nth(1)
        .and_then(|body| body.split("UserEvent::ReloadConfig").next())
        .expect("Wakeup arm");
    let lifecycle = wake
        .find("take_window_lifecycle_wakes(ws)")
        .expect("lifecycle edge consumption");
    let drain = wake[lifecycle..]
        .find("self.drain_events(ws);")
        .map(|offset| lifecycle + offset)
        .expect("semantic event drain");
    let output_gate = wake
        .find("output_wakeup_needs_paint(")
        .expect("output generation gate");
    assert!(
        lifecycle < drain && drain < output_gate,
        "quiet/hidden lifecycle events must be applied before a no-output wake can return"
    );
}

#[test]
fn held_child_status_poll_keeps_an_idle_deadline_until_reaped() {
    let src = production_source();
    let about = src
        .split("fn about_to_wait_inner(")
        .nth(1)
        .and_then(|body| body.split("\n    }\n}").next())
        .expect("about_to_wait_inner body");
    assert!(
        about.contains("let held_child_reap_pending = ws.mux.poll_held_child_statuses();")
            && about.contains("if held_child_reap_pending {")
            && about.contains("std::time::Duration::from_secs(1)"),
        "a held pane whose status lagged EOF needs a bounded idle wake until the zombie is collected"
    );
}

#[test]
fn animated_bg_redraw_is_edge_triggered() {
    let src = production_source();
    assert!(
        src.contains("let bg_frame_due = bg_frame.is_some() && bg_frame != ws.last_bg_frame;"),
        "the bg redraw must edge-trigger on a frame-index change"
    );
    // The redraw block must use the edge (bg_frame_due), not a level anim flag.
    assert!(
        src.contains("|| bg_frame_due\n"),
        "the redraw block must trigger on bg_frame_due, not level anim_active"
    );
    // The wake interval is the GIF's frame boundary, not a fixed 30 fps.
    assert!(
        src.contains("} else if let Some(bg_ms) = bg_anim_interval {"),
        "the wait must use the bg frame interval, not the fixed 33ms tick"
    );
}

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
            .split("\n    fn ")
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
        .and_then(|body| body.split("fn flush_recorder_output").next())
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

/// Source guards for event-loop states that require live winit gestures.
#[test]
fn event_state_leaks_are_gated() {
    let src = production_source();
    // 1. Dropped-file modal gate, at the top of the arm.
    let dropped = src
        .split("WindowEvent::DroppedFile(path) => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::Focused").next())
        .expect("DroppedFile event arm");
    let modal_gate = dropped
        .find("if self.any_modal_open(ws) {\n                    return;\n                }")
        .expect("DroppedFile must return while a modal is open");
    let first_path_operation = dropped
        .find("let paths = vec![path];")
        .expect("DroppedFile path delivery");
    assert!(
        modal_gate < first_path_operation,
        "the modal gate must precede all dropped-path work"
    );
    // 2. Focus loss disarms the latched drag and completion state.
    let focused_arm = src
        .split("WindowEvent::Focused(f) => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("Focused event arm");
    assert!(
        focused_arm.contains("self.finish_selection_gesture(ws);")
            && focused_arm.contains("ws.search.dragging_editor = false;")
            && focused_arm.contains("ws.mux.panes.values().chain(")
            && focused_arm.contains("self.windows\n                        .values()")
            && focused_arm.contains("pane.term.invalidate_completion();")
            && focused_arm.contains("ws.tab_drag_active = false;\n                    ws.tab_drag_press = None;\n                    ws.tab_pressed_idx = None;\n                    // A focus loss also ends any split-divider drag.\n                    ws.dragging_split = None;\n                    // Releases now land in another window: each program\n                    // with a held button's press gets its release here.\n                    self.release_held_buttons(ws, |_| true);"),
        "the Focused `!f` arm must disarm the latched drag and completion state"
    );
    // 3. Side-button dismisses a lone context menu instead of leaking SGR.
    assert!(
        src.contains(
            "if ws.context_menu.is_some() {\n                        ws.context_menu = None;"
        ),
        "a side-button press must dismiss a lone context menu, not forward SGR"
    );
}

/// Mouse motion reports only when it crosses into a new cell; a press or
/// release always reports.
#[test]
fn motion_coalesces_to_cell_crossings() {
    // Press/release always report, regardless of the last cell.
    assert!(App::motion_should_report(false, Some((4, 4)), (4, 4)));
    assert!(App::motion_should_report(false, None, (4, 4)));
    // Motion into a NEW cell reports; motion staying in the same cell does not.
    assert!(App::motion_should_report(true, Some((4, 4)), (4, 5)));
    assert!(App::motion_should_report(true, None, (4, 4)));
    assert!(!App::motion_should_report(true, Some((4, 4)), (4, 4)));
}

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
        .and_then(|body| body.split("\n    fn ").next())
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
        .and_then(|body| body.split("\n    fn ").next())
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
        let first_write = ["feed_input(", "feed_input_shared("]
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

/// Census: a tab that leaves its window takes the reported pane with it,
/// so every function that detaches a tab releases the report through the
/// detached tab before handing it on.
#[test]
fn every_tab_transfer_releases_the_focus_report() {
    let source = production_source();
    let mut transfers = 0;
    for chunk in source.split("\n    fn ").skip(1) {
        let name = chunk.split('(').next().unwrap_or_default().to_string();
        let body = chunk.split("\n    }\n").next().unwrap_or(chunk);
        if !body.contains(".mux.detach_tab(") {
            continue;
        }
        transfers += 1;
        let release = body.find("self.release_focus_report_to(");
        let handoff = ["WindowOpen::AdoptTab {", "self.dock_tab_into("]
            .iter()
            .filter_map(|needle| body.find(needle))
            .min()
            .expect("a detached tab is handed on");
        assert!(
            release.is_some_and(|release| release < handoff),
            "{name} hands a tab on without releasing its focus report"
        );
    }
    assert!(transfers >= 4, "census found only {transfers} transfers");
    // A target window docked into outside the dispatch funnel is
    // reconciled before it goes back into the window map.
    let docks = source
        .matches("self.dock_tab_into(&mut target, dt, paste, idx, td.seq);")
        .count();
    let reconciled = source
        .matches(
            "self.dock_tab_into(&mut target, dt, paste, idx, td.seq);\n            // The docked tab becomes the target's active tab; report it now,\n            // since an already focused target gets no new focus event.\n            self.sync_pane_focus_reports(&mut target);",
        )
        .count()
        + source
            .matches(
                "self.dock_tab_into(&mut target, dt, paste, idx, td.seq);\n                self.sync_pane_focus_reports(&mut target);",
            )
            .count();
    assert_eq!(docks, 2, "both cross-window docks are counted");
    assert_eq!(docks, reconciled, "every docked target is reconciled");
}

#[test]
fn docking_retargets_pane_owned_focus_state() {
    let src = production_source();
    let dock = src
        .split_once("fn dock_tab_into(")
        .unwrap()
        .1
        .split_once("\n    }")
        .unwrap()
        .0;
    let before = dock
        .find("let pre_focus = self.focus_key(target);")
        .unwrap();
    let attach = dock.find("target.mux.attach_tab(").unwrap();
    let notify = dock
        .find("self.note_focus_change(target, pre_focus);")
        .unwrap();
    assert!(before < attach && attach < notify);
    assert!(notify < dock.find("self.resize_all(target);").unwrap());
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

/// Census: a paste receipt follows its tab. Every function that detaches a
/// tab takes the tab's paste state before handing the tab on, and every
/// failed new-window handoff puts it back.
#[test]
fn every_tab_transfer_moves_its_paste_state() {
    let source = production_source();
    let mut transfers = 0;
    for chunk in source.split("\n    fn ").skip(1) {
        let name = chunk.split('(').next().unwrap_or_default().to_string();
        let body = chunk.split("\n    }\n").next().unwrap_or(chunk);
        if !body.contains(".mux.detach_tab(") {
            continue;
        }
        transfers += 1;
        // Each handoff needs its own take, after the previous handoff, so
        // one take cannot cover a second transfer in the same function.
        let mut handoffs: Vec<usize> = body
            .match_indices("WindowOpen::AdoptTab {")
            .filter(|(at, _)| !body[..*at].ends_with("Err("))
            .chain(body.match_indices("self.dock_tab_into("))
            .map(|(at, _)| at)
            .collect();
        handoffs.sort_unstable();
        let mut since = 0;
        for at in handoffs {
            assert!(
                body[since..at].contains(".take_moved_paste_state(&dt.tab,"),
                "{name} hands a tab on without its own paste state"
            );
            since = at;
        }
        assert_eq!(
            body.matches("Err(WindowOpen::AdoptTab { tab: dt, paste, .. })")
                .count(),
            body.matches(".restore_moved_paste_state(*paste);").count(),
            "{name} drops paste state on a failed handoff"
        );
        assert!(
            !body.contains("Err(WindowOpen::AdoptTab { tab: dt, .. })"),
            "{name} ignores paste state on a failed handoff"
        );
    }
    assert!(transfers >= 4, "census found only {transfers} transfers");
    // The receiving side adopts it, and a late video preview follows it.
    for needle in [
        "ws.mux.attach_tab(dt, None);\n                ws.adopt_moved_paste_state(*paste);",
        "let landed = target.mux.attach_tab(dt, Some(idx));\n        target.adopt_moved_paste_state(paste);",
        "video_preview_target_window(&self.windows, window_seq, pane_id, generation)",
    ] {
        assert!(source.contains(needle), "missing: {needle}");
    }
}

#[test]
fn mouse_and_focus_reports_use_chronological_user_input() {
    let source = production_source();
    let mouse = source
        .split("fn send_mouse_to(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("target-aware mouse sender");
    // Presses and drags take ordinary user input; a release takes the
    // user lane's room for releases. Both stay in order with typing.
    assert!(mouse.contains("pane.feed_input(&seq)"));
    assert!(mouse.contains("pane.feed_mouse_release(&seq)"));
    assert!(!mouse.contains("queue_protocol_reply"));

    let focus = source
        .split("fn sync_pane_focus_reports(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("focus report sync");
    assert!(focus.contains("let result = pane.feed_focus_report(focus);"));
    assert!(!focus.contains("queue_protocol_reply"));

    let mux = include_str!("../mux.rs").replace("\r\n", "\n");
    let focus_sender = mux
        .split("pub fn feed_focus_report(")
        .nth(1)
        .and_then(|body| body.split("\n    pub fn ").next())
        .expect("focus-report sender");
    assert!(focus_sender.contains("enqueue_user"));
    let release_lane = mux
        .split("fn enqueue_mouse_release(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("mouse-release lane");
    assert!(release_lane.contains("&self.user_tx,"));
    assert!(!focus_sender.contains("with_completion_input_admission"));

    let terminal_replies = source
        .split("TermEvent::PtyWrite(s) => {")
        .nth(1)
        .and_then(|body| body.split("TermEvent::").next())
        .expect("terminal reply arm");
    assert!(terminal_replies.contains("pane.queue_protocol_reply"));
}

/// Drift guard. `ScaledZoom` must baseline off the live
/// renderer size (`r.font_size()`), never `self.cfg.font_size` — the latter
/// is stale after any manual Increase/DecreaseFontSize (which only touch the
/// renderer), so scaling from it discards the user's manual zoom on exit.
/// A behavioral test needs a live renderer + mux; pin it at the source.
///
/// The scaling logic lives in the shared `toggle_zoom_with_scale` helper
/// (both `ToggleZoom` and `ScaledZoom` call it), so the guard checks the
/// helper body rather than the `Action::ScaledZoom` arm itself.
#[test]
fn scaled_zoom_baselines_off_live_font_size() {
    let src = production_source();
    let helper = src
        .split("fn toggle_zoom_with_scale(")
        .nth(1)
        .and_then(|s| s.split("\n    fn apply_window_resize").next())
        .expect("toggle_zoom_with_scale body present");
    // Strip line comments before the checks: the rationale prose in the
    // helper necessarily NAMES the `cfg.font_size` anti-pattern it warns
    // against, which would otherwise trip the negative assertion below.
    let code: String = helper
        .lines()
        .map(|line| line.split("//").next().unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        code.contains("let cur = r.font_size();") && code.contains("cur * factor"),
        "ScaledZoom must scale from the live renderer size"
    );
    assert!(
        !code.contains("cfg.font_size"),
        "ScaledZoom must not scale from the (stale) config font size"
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

/// Zoom-all changes the window-wide font size like Ctrl+Plus, so it must
/// also drop a stale scaled-zoom baseline. Otherwise leaving scaled zoom
/// restores the size from before the zoom-all.
#[test]
fn zoom_all_shares_the_font_size_arm() {
    let src = production_source();
    let arm = src
        .split_once("| Action::ZoomNormalAll => {")
        .expect("zoom-all joins the font-size arm")
        .1
        .split_once("Action::StartSearch =>")
        .expect("end of the font-size arm")
        .0;
    assert!(arm.contains("ws.scaled_zoom_prev_font_size = None;"));
    assert!(
        !src.contains("Action::ZoomInAll => {"),
        "no separate zoom-all arm"
    );
}

/// PTY rows, columns, and CSI 14 t pixels must be recomputed from the font
/// metrics selected by the zoom transition, not the metrics that preceded
/// it. A live renderer is too heavyweight for this unit suite, so pin the
/// mutation order at the shared helper boundary.
#[test]
fn scaled_zoom_resizes_after_changing_font_metrics() {
    let src = production_source();
    let helper = src
        .split("fn toggle_zoom_with_scale(")
        .nth(1)
        .and_then(|s| s.split("\n    fn apply_window_resize").next())
        .expect("toggle_zoom_with_scale body present");
    let resize = helper
        .rfind("self.resize_all(ws);")
        .expect("scaled zoom resizes PTYs");
    for change in ["r.set_font_size(new_size);", "r.set_font_size(prev);"] {
        assert!(
            helper.find(change).is_some_and(|offset| offset < resize),
            "{change} must precede the PTY geometry refresh"
        );
    }
}

/// Drift guard. `ToggleZoom` and `ScaledZoom` must route through the SAME
/// `toggle_zoom_with_scale` transition helper, which owns
/// `scaled_zoom_prev_font_size`. Otherwise a plain zoom-out after a
/// `ScaledZoom` zoom-in leaves the font enlarged and the saved baseline
/// stale, and a later `ScaledZoom` compounds from the wrong size. A
/// behavioral test needs a live renderer + mux; pin the shared call at the
/// source.
#[test]
fn toggle_zoom_and_scaled_zoom_share_the_transition_helper() {
    let src = production_source();
    let toggle_arm = src
        .split("Action::ToggleZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::EqualizeSplits").next())
        .expect("ToggleZoom arm present");
    assert!(
        toggle_arm.contains("self.toggle_zoom_with_scale(ws, None);"),
        "plain ToggleZoom must route through toggle_zoom_with_scale so \
             zoom-out restores/clears any pending scaled-zoom font baseline"
    );
    let scaled_arm = src
        .split("Action::ScaledZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::ToggleFullscreen").next())
        .expect("ScaledZoom arm present");
    assert!(
        scaled_arm.contains("self.toggle_zoom_with_scale(ws, Some(1.5));"),
        "ScaledZoom must route through the same shared helper as ToggleZoom"
    );
}

/// A plain zoom toggle on a single-pane tab does nothing rather than set a
/// hidden zoom flag, while leaving zoom always works and ScaledZoom keeps
/// its font step. A behavioral test needs a live renderer + mux; the live
/// `zoomed-layout-keys` smoke drives it end to end.
#[test]
fn plain_zoom_toggle_needs_a_pane_to_expand() {
    let src = production_source();
    let toggle_arm = src
        .split("Action::ToggleZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::EqualizeSplits").next())
        .expect("ToggleZoom arm present");
    assert!(
        toggle_arm.contains("if ws.mux.is_zoomed() || ws.mux.active_pane_count() > 1 {"),
        "ToggleZoom must leave a single-pane tab alone unless it is already zoomed"
    );
    let scaled_arm = src
        .split("Action::ScaledZoom => {")
        .nth(1)
        .and_then(|s| s.split("Action::ToggleFullscreen").next())
        .expect("ScaledZoom arm present");
    assert!(
        !scaled_arm.contains("active_pane_count"),
        "ScaledZoom keeps its font step on a single pane"
    );
}

/// Decreasing the font steps down to the renderer's own floor (5, see
/// `clamp_font_size`); a floor of 6 here once turned "decrease" at a
/// configured 5 into an increase.
#[test]
fn font_decrease_uses_the_renderer_floor() {
    let src = production_source();
    let arm = src
        .split("Action::DecreaseFontSize | Action::ZoomOutAll =>")
        .nth(1)
        .and_then(|s| s.split("_ => self.cfg.font_size").next())
        .expect("font step arm present");
    assert!(arm.contains("r.font_size() - 1.0"), "steps by one point");
    assert!(!arm.contains(".max("), "no floor above the renderer's own");

    let wheel = src
        .split("fn dispatch_wheel(")
        .nth(1)
        .and_then(|s| s.split("let new = if sign > 0 {").nth(1))
        .and_then(|s| s.split("r.set_font_size(new);").next())
        .expect("wheel font step present");
    assert!(wheel.contains("r.font_size() - 1.0"));
    assert!(
        !wheel.contains(".max("),
        "wheel must use the renderer floor"
    );
}

/// Drift guard. `search_key`'s catch-all must route typed text through the
/// shared modal rule and insert through its bounded editor.
///
/// The handler's explicit arms claim ⌘A/⌘C/⌘X/⌘V. A catch-all that filters
/// only control characters lets every other Command chord type its letter
/// into the query (`⌘Q` adds a `q`). Needs full App state; pin at the
/// source.
#[test]
fn search_key_filters_control_chars() {
    let src = production_source();
    let arm = src
        .split("fn search_key(")
        .nth(1)
        .and_then(|s| s.split("\n    fn ").next())
        .expect("search_key present");
    assert!(
        arm.contains("crate::modal_input::accept_text(text, ws.mods)"),
        "search_key must filter typed text through the shared modal rule, \
             or an unclaimed Command chord types its letter into the query"
    );
    assert!(
        arm.contains(".type_text(t, kettle_core::search::MAX_SEARCH_QUERY_BYTES)"),
        "search_key must type through the bounded editor"
    );
    assert!(
        !arm.contains("!t.chars().any(|c| c.is_control())"),
        "the inline control-character filter is now modal_input's job"
    );
    // The checks above only prove the catch-all is right. A *new*
    // `Key::Character` arm added above it with no guard would take
    // precedence and reintroduce the bug with every assertion still green,
    // so require every character arm in this handler to carry a condition.
    for (index, _) in arm.match_indices("Key::Character(") {
        let rest = &arm[index..];
        let head = rest.split("=>").next().unwrap_or_default();
        assert!(
            head.contains(" if "),
            "search_key has an unguarded `Key::Character` arm: `{}` — an \
                 arm without a modifier condition claims Command chords before \
                 the catch-all ever applies the shared rule",
            head.trim()
        );
    }
}

#[test]
fn non_text_modals_block_control_plane_typing_into_search() {
    use super::{ContextMenuState, TextModal, ViState, open_text_modal};

    let mut ws = WindowState::new(1, Mux::new());
    ws.search.open = true;
    assert_eq!(open_text_modal(&ws), Some(TextModal::Search));
    ws.vi_mode = Some(ViState {
        pane_id: 1,
        visual: false,
    });
    assert_eq!(
        open_text_modal(&ws),
        None,
        "vi owns the keyboard above search"
    );
    ws.context_menu = Some(ContextMenuState {
        anchor: (0.0, 0.0),
        anchor_scale: 1.0,
        items: Vec::new(),
        highlight: 0,
        drill_stack: Vec::new(),
        scroll_offset: 0,
        scroll_stack: Vec::new(),
        typeahead_buf: String::new(),
        typeahead_until: None,
    });
    assert_eq!(
        open_text_modal(&ws),
        None,
        "the menu owns the keyboard above vi"
    );
    ws.vi_mode = None;
    assert_eq!(
        open_text_modal(&ws),
        None,
        "the menu also blocks search alone"
    );
    ws.context_menu = None;
    assert_eq!(open_text_modal(&ws), Some(TextModal::Search));
}

/// Keep the shared owner resolver aligned with native keyboard dispatch.
#[test]
fn the_control_plane_modal_order_matches_the_real_key_handler() {
    let src = production_source();

    let resolver = src
        .split("fn top_modal(")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("top_modal present");
    let resolver_order: Vec<&str> = [
        ("ws.confirm_dialog.is_some()", "confirm"),
        ("ws.context_menu.is_some()", "context_menu"),
        ("ws.vi_mode.is_some()", "vi_mode"),
        ("ws.hint_state.is_some()", "hint"),
        ("ws.palette_input.is_some()", "palette"),
        ("ws.theme_picker_input.is_some()", "theme_picker"),
        // The compound condition, not just the text-edit half: dropping
        // the `settings_nav` guard would diverge from the real handler
        // while a substring probe stayed green.
        (
            "ws.settings_nav.is_some() && ws.settings_text_edit.is_some()",
            "settings_text",
        ),
        ("else if ws.settings_nav.is_some() {", "settings"),
        ("ws.layout_picker_input.is_some()", "layout_picker"),
        ("ws.ssh_input.is_some()", "ssh"),
        ("ws.editing_title.is_some()", "title_edit"),
        ("ws.search.open", "search"),
    ]
    .iter()
    .map(|(probe, name)| {
        assert!(resolver.contains(probe), "top_modal must test {probe}");
        (resolver.find(probe).expect("probe present"), *name)
    })
    .collect::<std::collections::BTreeMap<_, _>>()
    .into_values()
    .collect();

    // The same probes as they appear in the KeyboardInput arm, which runs
    // from the confirm dialog down to the terminal.
    let handler = src
        .split("let text = event.text.as_ref().map(|s| s.as_str());")
        .nth(1)
        .and_then(|rest| rest.split("fn ").next())
        .expect("KeyboardInput arm present");
    let handler_order: Vec<&str> = [
        ("if ws.confirm_dialog.is_some() {", "confirm"),
        ("if ws.context_menu.is_some() {", "context_menu"),
        ("if ws.vi_mode.is_some() {", "vi_mode"),
        ("if ws.hint_state.is_some() {", "hint"),
        ("if ws.palette_input.is_some() {", "palette"),
        ("if ws.theme_picker_input.is_some() {", "theme_picker"),
        (
            "if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {",
            "settings_text",
        ),
        ("if ws.settings_nav.is_some() {", "settings"),
        ("if ws.layout_picker_input.is_some() {", "layout_picker"),
        ("if ws.ssh_input.is_some() {", "ssh"),
        ("if ws.editing_title.is_some() {", "title_edit"),
        ("if ws.search.open {", "search"),
    ]
    .iter()
    .map(|(probe, name)| {
        assert!(handler.contains(probe), "the key handler must test {probe}");
        (handler.find(probe).expect("probe present"), *name)
    })
    .collect::<std::collections::BTreeMap<_, _>>()
    .into_values()
    .collect();

    assert_eq!(
        resolver_order, handler_order,
        "open_text_modal resolves modals in a different order than the real \
             key handler, so dispatch_ui_key would type into the wrong field"
    );
    // The Settings Theme row opens the theme picker over the panel, so the
    // picker must outrank every Settings branch or the panel eats its keys.
    let at = |name: &str| handler_order.iter().position(|n| *n == name).unwrap();
    assert!(
        at("theme_picker") < at("settings_text") && at("theme_picker") < at("settings"),
        "the theme picker must take keys before the Settings panel it covers"
    );

    // Matching order is not enough: dispatch_ui_key still has to send each
    // variant to *its own* handler. Swapping two arms in that match would
    // leave both orders identical above while typing palette keys into the
    // SSH launcher.
    let dispatch = src
        .split("fn ctl_dispatch_ui_key(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("ctl_dispatch_ui_key present");
    for (variant, handler) in [
        ("TextModal::Confirm", "self.confirm_dialog_key("),
        ("TextModal::Hint", "self.hint_key("),
        ("TextModal::Palette", "self.palette_key("),
        ("TextModal::ThemePicker", "self.theme_picker_key("),
        ("TextModal::SettingsText", "self.settings_text_key("),
        ("TextModal::Settings", "self.settings_key("),
        ("TextModal::LayoutPicker", "self.layout_picker_key("),
        ("TextModal::Ssh", "self.ssh_key("),
        ("TextModal::TitleEdit", "self.title_edit_key("),
        ("TextModal::Search", "self.search_key("),
    ] {
        // An arm runs to the next variant; the last one to the match's end.
        let arm = dispatch
            .split(&format!("{variant} =>"))
            .nth(1)
            .unwrap_or_else(|| panic!("dispatch_ui_key has no arm for {variant}"))
            .split("TextModal::")
            .next()
            .unwrap_or_default();
        assert!(
            arm.contains(handler),
            "dispatch_ui_key routes {variant} to `{arm}`, not {handler} — \
                 the control plane would type into a different field than the \
                 modal it reports"
        );
    }
}

/// The OS titlebar is a spoofing sink: the Cf bidi overrides are not
/// `char::is_control`, so they pass every control-character filter and can
/// reverse the rendered text in the titlebar and the Alt-Tab switcher.
///
/// Every route to that sink must run `sanitize_title`, including the
/// user-set window title. Its editor accepts paste, which puts a clipboard
/// payload one keystroke away, and terminal programs can write the
/// clipboard through OSC 52. Pin sanitization on the shared title path;
/// the edit buffer itself deliberately keeps what was typed.
#[test]
fn the_user_set_window_title_is_sanitized_before_it_reaches_the_window() {
    let src = production_source();
    let arm = src
        .split("TitleEditScope::Window => {")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("window title-edit arm present");
    assert!(arm.contains("self.sync_window_title(ws);"));
    assert!(!arm.contains("w.set_title("));
    let sync = src
        .split("fn sync_window_title(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .unwrap();
    assert!(sync.contains("let want = self.desired_window_title(ws);"));
    assert!(!sync.contains("window_title_override"));
    let desired = src
        .split("fn desired_window_title(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .unwrap();
    assert!(desired.contains("Some(forced) => sanitize_title(forced),"));
    let base = desired.find("ws.window_title_override").unwrap();
    let recording = desired.find("recording_window_title(").unwrap();
    let sanitize = desired.find("sanitize_native_window_title(").unwrap();
    assert!(base < recording && recording < sanitize);
    // And the sanitizer must still cover the bidi class, not just controls.
    assert_eq!(sanitize_title("safe\u{202e}evil"), "safe evil");
    assert_eq!(sanitize_title("ok\u{2069}"), "ok ");
    // A ZWJ emoji is not a bidi override and must survive a title.
    assert_eq!(
        sanitize_title("a\u{1f469}\u{200d}\u{1f680}"),
        "a\u{1f469}\u{200d}\u{1f680}"
    );
}

/// Drift guard for the whole family of append-only modal text fields.
///
/// Each of these handlers owns a plain `String`. `KeyEvent::text` is
/// **not** filtered by Command on macOS, so a raw append types `⌘V` as
/// `"v"`. Pasting into a tab rename then yields a tab named `v`, and in the
/// command palette it rewrites the query and re-ranks the list, so the next
/// Enter runs a command the user never chose. `String::pop` splits
/// grapheme clusters, so one Backspace on `👩‍🚀` leaves a dangling
/// zero-width joiner. A raw buffer also has no length cap.
///
/// `modal_input` owns modifier filtering, grapheme-correct deletion, the
/// byte cap, and paste in one place. This pins that every handler goes
/// through it, so a new modal cannot quietly reintroduce the raw-append
/// pattern. Needs full App state, so it pins at the source like its
/// neighbours.
#[test]
fn every_modal_text_field_routes_through_the_shared_input_rules() {
    let src = production_source();

    // Every handler that consumes typed text shares the modifier rule.
    // Search is included: it owns a real editor and does its own paste and
    // deletion, but its catch-all arm still has to reject Command chords,
    // because every chord it does not claim explicitly (⌘Q, ⌘W, …) used to
    // land in the query as a bare letter.
    let append_only = [
        "fn palette_key(",
        "fn ssh_key(",
        "fn layout_picker_key(",
        "fn settings_text_key(",
        "fn title_edit_key(",
    ];
    for name in append_only.iter().copied().chain(["fn search_key("]) {
        let body = src
            .split(name)
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("missing production body for {name}"));

        assert!(
            body.contains("crate::modal_input::accept_text(text, ws.mods)"),
            "{name} must filter text through modal_input::accept_text, \
                 or a Command chord types its letter into the field"
        );
        // ANY direct mutation of the buffer sidesteps the cap and the
        // filter. A `state.input.push_str(...)` next to the helper call
        // would drop the 4 KiB bound on that path.
        for banned in [
            "push_str(t)",
            ".buf.pop()",
            "input.pop()",
            "input.push(",
            "input.push_str(",
            "input.extend(",
            ".buf.push(",
            ".buf.push_str(",
            ".buf.extend(",
            "q.push(",
            "q.push_str(",
            "q.extend(",
        ] {
            assert!(
                !body.contains(banned),
                "{name} mutates its buffer directly with `{banned}`; every \
                     write must go through modal_input so the cap and the \
                     control-character filter apply"
            );
        }
        if !append_only.contains(&name) {
            continue;
        }
        assert!(
            body.contains("crate::modal_input::push_text("),
            "{name} must append through modal_input::push_text so the \
                 byte cap and control-character filter apply"
        );
        assert!(
            body.contains("crate::modal_input::backspace("),
            "{name} must delete through modal_input::backspace so a \
                 grapheme cluster leaves whole"
        );
        assert!(
            body.contains("crate::modal_input::is_paste_chord("),
            "{name} must honour the platform paste chord"
        );
    }
}

/// `parse_send_key` is the entire vocabulary an agent can press. Pin the
/// grammar: named keys (incl. the ones the keybind grammar lacks), chords
/// with every modifier alias, preserved character case, and loud failure
/// on typos.
#[test]
fn parse_send_key_grammar() {
    use super::parse_send_key;
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    let none = ModifiersState::empty();
    // Named keys the keybind grammar has no variant for.
    assert_eq!(
        parse_send_key("escape"),
        Some((none, Key::Named(NamedKey::Escape)))
    );
    assert_eq!(
        parse_send_key("backspace"),
        Some((none, Key::Named(NamedKey::Backspace)))
    );
    assert_eq!(
        parse_send_key("space"),
        Some((none, Key::Named(NamedKey::Space)))
    );
    // Chords, with aliases.
    assert_eq!(
        parse_send_key("ctrl+c"),
        Some((ModifiersState::CONTROL, Key::Character("c".into())))
    );
    assert_eq!(
        parse_send_key("shift+tab"),
        Some((ModifiersState::SHIFT, Key::Named(NamedKey::Tab)))
    );
    assert_eq!(
        parse_send_key("alt+enter"),
        Some((ModifiersState::ALT, Key::Named(NamedKey::Enter)))
    );
    assert_eq!(
        parse_send_key("control+shift+f5"),
        Some((
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            Key::Named(NamedKey::F5)
        ))
    );
    // Character case and its implied Shift are preserved — `G` (vim:
    // jump to end) is not an unmodified `g`.
    assert_eq!(
        parse_send_key("G"),
        Some((ModifiersState::SHIFT, Key::Character("G".into())))
    );
    assert_eq!(
        parse_send_key(":"),
        Some((none, Key::Character(":".into())))
    );
    // Shift+letter normalizes to the uppercase logical character while
    // retaining SHIFT. Legacy encoding still produces the uppercase byte,
    // while negotiated CSI-u preserves the actual chord. Super characters
    // remain valid tokens because the pane's live mode decides whether
    // Kitty CSI-u can encode them. The chord/CLI separator characters are
    // reachable via their names.
    assert_eq!(
        parse_send_key("shift+g"),
        Some((ModifiersState::SHIFT, Key::Character("G".into())))
    );
    assert_eq!(
        parse_send_key("super+x"),
        Some((ModifiersState::SUPER, Key::Character("x".into())))
    );
    assert_eq!(
        parse_send_key("plus"),
        Some((none, Key::Character("+".into())))
    );
    assert_eq!(
        parse_send_key("comma"),
        Some((none, Key::Character(",".into())))
    );
    assert_eq!(
        parse_send_key("ctrl+minus"),
        Some((ModifiersState::CONTROL, Key::Character("-".into())))
    );
    // Typos fail loudly instead of degrading.
    assert_eq!(parse_send_key("cttrl+c"), None, "typo'd modifier");
    assert_eq!(parse_send_key("ctrl+"), None, "missing key");
    assert_eq!(parse_send_key("f13"), None, "no such F-key");
    assert_eq!(parse_send_key("escape+x"), None, "named key as modifier");
}

#[test]
fn parse_ui_key_allows_app_shortcuts_and_rejects_inert_tokens() {
    use super::parse_ui_key;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    assert_eq!(
        parse_ui_key("cmd+a"),
        Some((ModifiersState::SUPER, Key::Character("a".into())))
    );
    assert_eq!(
        parse_ui_key("space"),
        Some((ModifiersState::empty(), Key::Character(" ".into())))
    );
    assert_eq!(
        parse_ui_key("shift+f3"),
        Some((ModifiersState::SHIFT, Key::Named(NamedKey::F3)))
    );
    assert_eq!(
        parse_ui_key("up"),
        Some((ModifiersState::empty(), Key::Named(NamedKey::ArrowUp)))
    );
    assert_eq!(
        parse_ui_key("down"),
        Some((ModifiersState::empty(), Key::Named(NamedKey::ArrowDown)))
    );
    assert_eq!(parse_ui_key("page_down"), None);
    assert_eq!(parse_ui_key("insert"), None);
}

#[test]
fn send_keys_caps_batch_tokens_and_encoded_bytes_before_growth() {
    let oversized = vec![serde_json::json!("a"); super::MAX_CTL_SEND_KEYS + 1];
    assert!(
        super::parse_ctl_send_key_batch(&oversized)
            .unwrap_err()
            .contains("must contain")
    );
    assert!(
        super::parse_ctl_send_key_batch(&[serde_json::json!(
            "x".repeat(super::MAX_CTL_SEND_KEY_TOKEN_BYTES + 1)
        )])
        .unwrap_err()
        .contains("key tokens")
    );

    let mut bytes = vec![b'x'; super::MAX_CTL_SEND_KEY_BYTES];
    assert!(super::extend_ctl_send_key_bytes(&mut bytes, b"y").is_err());
    assert_eq!(bytes.len(), super::MAX_CTL_SEND_KEY_BYTES);

    let src = production_source();
    let body = src
        .split("fn ctl_send_keys(")
        .nth(1)
        .and_then(|body| body.split("\n    /// Resolve the `pane` param").next())
        .expect("ctl_send_keys body");
    assert!(
        body.contains("p.feed_key_inputs(&encoded_keys)"),
        "send_keys must preserve per-key boundaries so every Tab advances one request"
    );
    assert!(
        !body.contains("p.feed_input(&bytes)"),
        "a flattened key batch hides Tab boundaries from completion sequencing"
    );
}

#[test]
fn ctl_send_keys_rejects_unencodable_super_tokens_before_writing() {
    let parsed = super::parse_ctl_send_key_batch(&[serde_json::json!("cmd+up")])
        .expect("cmd+up is a valid mode-dependent token");
    let (token, mods, key) = &parsed[0];
    assert_eq!(token, "cmd+up");
    assert_eq!(
        crate::input::encode_key_press(key, *mods, kettle_core::TermMode::empty()),
        None,
        "cmd+up must be unencodable before Kitty negotiation"
    );

    let src = production_source();
    let body = src
        .split("fn ctl_send_keys(")
        .nth(1)
        .and_then(|body| body.split("\n    /// Resolve the `pane` param").next())
        .expect("ctl_send_keys body");
    let encode = body
        .find("encode_key_press(key, *mods, mode)")
        .expect("send_keys encoder call");
    let reject = body
        .find("'{token}' has no legacy encoding")
        .expect("legacy Super rejection");
    let feed = body
        .find("p.feed_key_inputs(&encoded_keys)")
        .expect("pane input call");
    assert!(
        body.contains("for (token, mods, key) in &parsed")
            && body.contains("has not negotiated the Kitty keyboard protocol")
            && body.contains("negotiated Kitty keyboard mode")
            && body[reject..feed].contains("Response::err(req.id, ec::BAD_PARAMS, message)"),
        "ctl_send_keys must name the token and return BAD_PARAMS for every active mode"
    );
    assert!(
        encode < reject && reject < feed,
        "ctl_send_keys must reject an unencodable token before writing any parsed key"
    );
    // Ordering alone is not atomicity. A `p.feed_input(&b)` added inside
    // the encoding loop would sit before the rejection and satisfy every
    // check above, while `["a", "cmd+up"]` wrote `a` and then errored --
    // a partial batch, which is the exact failure this is here to prevent.
    // Pin that the whole body performs exactly ONE write, whatever it is
    // named.
    let writes = body.matches(".feed_").count();
    assert_eq!(
        writes, 1,
        "ctl_send_keys must write to the pane exactly once, after the whole \
             batch has been encoded; found {writes} write calls"
    );
    assert!(
        body[..reject].matches(".feed_").count() == 0,
        "no PTY write may precede the unencodable-token rejection"
    );
}

/// The encoded bytes must match what a human pressing the same keys
/// produces, through the same encoder and the same mode handling.
#[test]
fn send_keys_tokens_encode_like_gui_keystrokes() {
    use super::parse_send_key;
    use kettle_core::TermMode;
    let enc = |tok: &str, mode: TermMode| {
        let (mods, key) = parse_send_key(tok).expect(tok);
        crate::input::encode_key_press(&key, mods, mode)
    };
    let plain = TermMode::empty();
    assert_eq!(enc("escape", plain), Some(vec![0x1b]));
    assert_eq!(enc("ctrl+c", plain), Some(vec![0x03]));
    assert_eq!(enc("enter", plain), Some(vec![b'\r']));
    assert_eq!(enc("up", plain), Some(b"\x1b[A".to_vec()));
    // DECCKM application-cursor mode flips arrows to SS3 — the reason
    // send_keys reads the pane's LIVE mode (vim sets it).
    assert_eq!(enc("up", TermMode::APP_CURSOR), Some(b"\x1bOA".to_vec()));
    assert_eq!(enc("shift+tab", plain), Some(b"\x1b[Z".to_vec()));
    assert_eq!(enc("G", plain), Some(b"G".to_vec()));
    assert_eq!(enc("cmd+up", plain), None);
    assert_eq!(
        enc("cmd+up", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[1;9A".to_vec())
    );
    assert_eq!(
        enc("escape", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[27u".to_vec()),
        "send_keys must honor Kitty keyboard negotiation"
    );
    assert_eq!(
        enc("ctrl+c", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[99;5u".to_vec())
    );
    assert_eq!(
        enc("ctrl+shift+c", TermMode::DISAMBIGUATE_ESC_CODES),
        Some(b"\x1b[99;6u".to_vec()),
        "synthetic chords must retain the same modifiers as GUI input"
    );
    assert_eq!(
        enc(
            "shift+g",
            TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ALTERNATE_KEYS
        ),
        Some(b"\x1b[103:71;2u".to_vec())
    );
    assert_eq!(
        enc(
            "G",
            TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ALTERNATE_KEYS
        ),
        Some(b"\x1b[103:71;2u".to_vec()),
        "bare uppercase tokens must retain Kitty's Shift semantics"
    );
    assert_eq!(
        enc(
            "ctrl+space",
            TermMode::REPORT_ALL_KEYS_AS_ESC | TermMode::REPORT_ASSOCIATED_TEXT
        ),
        Some(b"\x1b[32;5u".to_vec()),
        "synthetic Control chords must not invent associated text"
    );
}

/// Every named key a config can bind arrives from the winit key that
/// produces it: winit reports the space bar as `NamedKey::Space`, so the
/// `ctrl+shift+space` vi-mode default was dead until it was mapped.
#[test]
fn every_configurable_named_key_matches_its_real_key() {
    use super::{resolve_keybind_action, to_kkey};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let mut pairs = vec![
        ("up", NamedKey::ArrowUp),
        ("down", NamedKey::ArrowDown),
        ("left", NamedKey::ArrowLeft),
        ("right", NamedKey::ArrowRight),
        ("pageup", NamedKey::PageUp),
        ("pagedown", NamedKey::PageDown),
        ("home", NamedKey::Home),
        ("end", NamedKey::End),
        ("enter", NamedKey::Enter),
        ("tab", NamedKey::Tab),
        ("backspace", NamedKey::Backspace),
        ("delete", NamedKey::Delete),
        ("space", NamedKey::Space),
    ];
    let fkeys = [
        NamedKey::F1,
        NamedKey::F2,
        NamedKey::F3,
        NamedKey::F4,
        NamedKey::F5,
        NamedKey::F6,
        NamedKey::F7,
        NamedKey::F8,
        NamedKey::F9,
        NamedKey::F10,
        NamedKey::F11,
        NamedKey::F12,
    ];
    let fnames: Vec<String> = (1..=12).map(|n| format!("f{n}")).collect();
    pairs.extend(fnames.iter().map(String::as_str).zip(fkeys));
    for (name, named) in pairs {
        let config = kettle_config::Config::parse_text(&format!(
            "keybind = ctrl+alt+{name}=toggle_vi_mode\n"
        ));
        let trigger = config
            .keybinds_declared
            .iter()
            .next()
            .copied()
            .unwrap_or_else(|| panic!("`{name}` does not parse"));
        assert_eq!(
            to_kkey(&Key::Named(named)),
            Some(trigger.key),
            "`{name}` parses to {:?} but {named:?} does not map to it",
            trigger.key
        );
    }

    let defaults = kettle_config::keybinds::defaults();
    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert_eq!(
        resolve_keybind_action(&defaults, Some(&Key::Named(NamedKey::Space)), None, mods),
        Some((
            Trigger::new(Mods::CTRL | Mods::SHIFT, KKey::Char(' ')),
            Action::ToggleViMode
        ))
    );
}

/// The chord that enters vi mode also leaves it, although vi mode takes
/// every key before the keybind table; other keys still drive it.
#[test]
fn the_vi_mode_chord_leaves_vi_mode() {
    use super::is_vi_mode_toggle_chord;
    use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

    let defaults = kettle_config::keybinds::defaults();
    let space = PhysicalKey::Code(KeyCode::Space);
    let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
    assert!(is_vi_mode_toggle_chord(
        &defaults,
        &Key::Named(NamedKey::Space),
        &space,
        cs
    ));
    assert!(!is_vi_mode_toggle_chord(
        &defaults,
        &Key::Named(NamedKey::Space),
        &space,
        ModifiersState::empty()
    ));
    assert!(!is_vi_mode_toggle_chord(
        &defaults,
        &Key::Character("j".into()),
        &PhysicalKey::Code(KeyCode::KeyJ),
        ModifiersState::empty()
    ));
    let src = production_source();
    let intercept = src
        .split("if ws.vi_mode.is_some() {\n                    if is_vi_mode_toggle_chord(")
        .nth(1)
        .expect("the keyboard vi-mode intercept must check the toggle chord");
    let exit = intercept
        .find("self.exit_vi_mode(ws)")
        .expect("the chord exits vi mode");
    assert!(
        intercept[..exit].contains("if !event.repeat {"),
        "a held chord must leave vi mode once, not flip it on every repeat"
    );
}

/// Auto-repeat of a held toggle chord runs the toggle once; repeat of a
/// motion or scroll chord keeps running it.
#[test]
fn toggles_run_once_per_press_while_held() {
    use super::toggles_once_per_press;
    use kettle_config::Action;
    for toggle in [
        Action::ToggleViMode,
        Action::ToggleZoom,
        Action::ScaledZoom,
        Action::ToggleFullscreen,
        Action::ToggleBroadcastAll,
        Action::TogglePaneReadOnly,
    ] {
        assert!(toggles_once_per_press(&toggle), "{toggle:?}");
    }
    for repeating in [
        Action::ScrollLineUp,
        Action::ScrollPageDown,
        Action::ResizeLeft,
        Action::IncreaseFontSize,
        Action::NextTab,
        Action::JumpPrevPrompt,
    ] {
        assert!(!toggles_once_per_press(&repeating), "{repeating:?}");
    }
    // Every toggle a config can bind is in the list.
    for name in kettle_config::keybinds::action_names() {
        if let Some(action) = Action::from_name(name)
            && format!("{action:?}").starts_with("Toggle")
        {
            assert!(toggles_once_per_press(&action), "{name} is a toggle");
        }
    }
    let src = production_source();
    assert!(
        src.contains("if !(event.repeat && toggles_once_per_press(&act)) {"),
        "the keyboard keybind path must skip toggle repeats"
    );
    assert!(
        src.contains("if !(repeat && toggles_once_per_press(&action)) {"),
        "the search bar's shortcut route must skip toggle repeats"
    );
    assert!(
        src.contains(
            "let inert = if settings_capture_active(ws) {\n                        event.repeat"
        ),
        "keybind capture must wait for a new press, not take the activating key's repeats"
    );
}

/// In a Kettle modal, keys that activate, toggle or pick act once per
/// press, keys that move keep repeating, and a first press always acts.
#[test]
fn modal_activation_keys_act_once_per_press() {
    use super::{ModalRepeat::*, modal_repeat_is_inert};
    use winit::keyboard::{Key, NamedKey};
    let enter = Key::Named(NamedKey::Enter);
    let space = Key::Named(NamedKey::Space);
    let down = Key::Named(NamedKey::ArrowDown);
    let right = Key::Named(NamedKey::ArrowRight);
    let chr = |c: &str| Key::Character(c.into());
    let menu = ContextMenu { vim_nav: true };
    let plain_menu = ContextMenu { vim_nav: false };
    let toggle_row = Settings { steps: false };
    let number_row = Settings { steps: true };
    for modal in [ConfirmDialog, menu, ViMode, Hint, Palette, toggle_row] {
        for key in [&enter, &space, &down, &chr("v"), &chr("l")] {
            assert!(
                !modal_repeat_is_inert(modal, false, key, false),
                "{modal:?} {key:?} press"
            );
        }
    }
    for (modal, key, ctrl, inert) in [
        (menu, &enter, false, true),
        (menu, &space, false, true),
        (menu, &chr("l"), false, true),
        (menu, &chr("n"), false, true),
        (menu, &down, false, false),
        (menu, &right, false, false),
        (menu, &chr("j"), false, false),
        (menu, &chr("K"), false, false),
        (menu, &chr("d"), true, false),
        (menu, &chr("U"), true, false),
        (menu, &chr("d"), false, true),
        // Without vim-menu-nav, `j` and `k` are mnemonics.
        (plain_menu, &chr("j"), false, true),
        (plain_menu, &chr("d"), true, true),
        (plain_menu, &down, false, false),
        (ConfirmDialog, &enter, false, true),
        (ConfirmDialog, &chr("y"), false, true),
        (ConfirmDialog, &right, false, false),
        (Palette, &enter, false, true),
        (Palette, &chr("a"), false, false),
        (Palette, &down, false, false),
        (toggle_row, &enter, false, true),
        (toggle_row, &space, false, true),
        (toggle_row, &right, false, true),
        (toggle_row, &chr("h"), false, true),
        (toggle_row, &down, false, false),
        (number_row, &right, false, false),
        (number_row, &chr("l"), false, false),
        (number_row, &enter, false, true),
        (ViMode, &chr("v"), false, true),
        (ViMode, &chr("j"), false, false),
        (Hint, &chr("a"), false, true),
    ] {
        assert_eq!(
            modal_repeat_is_inert(modal, true, key, ctrl),
            inert,
            "{modal:?} {key:?} ctrl={ctrl}"
        );
    }
}

/// Zooming a pane, splitting next to it or resizing the window reflows
/// the focused pane's text with no output. The detected links must follow
/// the reflowed text, or their underlines are painted over the wrong
/// cells (grep output whose path underlines went out of sync after a
/// zoom).
#[cfg(unix)]
#[test]
fn detected_links_follow_a_resize_that_reflows_the_text() {
    use kettle_config::Config;
    use kettle_core::{PtyGeometry, Waker};
    use std::sync::Arc;

    // Absolute paths, so detection needs no reported cwd.
    let line = "/srv/virtual-rack-api-tests/behave/features/contract/flight-data.contract.feature:1395:  \
                    When I send GET /flight-data/v1/time-of-departure request";
    let cfg = Config {
        shell: Some("/bin/sh".into()),
        ..Config::default()
    };
    let waker: Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        format!("printf '%s\\n' '{line}'; sleep 30"),
    ];
    if let Err(e) = mux.new_tab_with_geometry(
        &cfg,
        PtyGeometry::from_cell_size(160, 10, 8, 16),
        waker,
        &argv,
        None,
    ) {
        eprintln!("skipping: no PTY ({e})");
        return;
    }
    let mut ws = WindowState::new(1, mux);
    let pane = ws.mux.active_focus().expect("a focused pane");
    let spans = |links: &[kettle_core::Link]| -> Vec<(usize, usize, usize, String)> {
        links
            .iter()
            .map(|link| (link.row, link.start_col, link.end_col, link.uri.clone()))
            .collect()
    };
    let detected_now = |ws: &WindowState| {
        let term = ws.mux.panes[&pane].term.term.lock().expect("term lock");
        kettle_core::links::links_with_cwd(
            &term,
            ws.mux.panes[&pane].term.current_dir_or_native().as_deref(),
        )
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !ws.mux.panes[&pane]
        .term
        .screen_text(0)
        .is_some_and(|screen| screen.text.lines().next() == Some(line) && screen.cursor == (1, 0))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the shell never printed the complete fixture"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // Text is visible before its chunk's output generation is published.
    // Wait for parser completion before priming the cache, so a late
    // generation cannot hide a missing geometry key.
    while ws.mux.panes[&pane].term.pty_read_progress().pending_chunks != 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the parser never finished the fixture"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    super::scan_links(&mut ws);
    let wide = spans(&ws.links);
    assert_eq!(wide, spans(&detected_now(&ws)));

    // Narrower: the line wraps and every span after the first row moves.
    // No output follows.
    let output = ws.mux.panes[&pane].term.output_generation();
    ws.mux
        .panes
        .get_mut(&pane)
        .expect("pane")
        .term
        .resize(60, 10, 8, 16)
        .expect("resize");
    assert_eq!(ws.mux.panes[&pane].term.output_generation(), output);
    super::scan_links(&mut ws);
    let narrow = spans(&detected_now(&ws));
    assert_ne!(narrow, wide, "the reflow moved the links");
    assert_eq!(spans(&ws.links), narrow, "the scan followed the reflow");
}

#[test]
fn link_resize_fixture_waits_for_parser_completion_before_scanning() {
    // The fixture is a sibling test in this file.
    let code = kettle_test_support::code_only(include_str!("tests.rs"));
    let fixture = code
        .split_once("fn detected_links_follow_a_resize_that_reflows_the_text() {")
        .expect("resize fixture")
        .1
        .split_once("\nfn ")
        .expect("end of resize fixture")
        .0;
    let setup = fixture
        .split_once("super::scan_links(&mut ws);")
        .expect("cache priming")
        .0;
    let wait = setup
        .rsplit_once("while ")
        .expect("completion wait")
        .1
        .split_once('{')
        .expect("wait body")
        .0;
    assert!(wait.contains(".pty_read_progress()"));
    assert!(wait.contains(".pending_chunks != 0"));
}

#[test]
fn a_pending_wayland_drop_cannot_follow_changed_focus_or_broadcast() {
    let mut ws = WindowState::new(1, Mux::new());
    let target = super::WaylandDropTarget::capture(&ws);
    assert!(target == super::WaylandDropTarget::capture(&ws));
    ws.ime_focus_generation += 1;
    assert!(
        target != super::WaylandDropTarget::capture(&ws),
        "modal opened and closed"
    );
    ws.ime_focus_generation -= 1;
    ws.mux.broadcast = crate::mux::BroadcastScope::All;
    assert!(target != super::WaylandDropTarget::capture(&ws));
    ws.mux.broadcast = crate::mux::BroadcastScope::Off;
    ws.preview_focus = Some(crate::media::PreviewFocus { pane: 7, item: 8 });
    assert!(target != super::WaylandDropTarget::capture(&ws));
}

#[cfg(unix)]
#[test]
fn a_pending_wayland_drop_does_not_cross_another_paste() {
    let mut mux = Mux::new();
    mux.new_tab_with(
        &title_test_config(),
        80,
        24,
        8,
        16,
        Arc::new(|| {}),
        &["/bin/cat".to_owned()],
        None,
    )
    .expect("PTY fixture");
    let ws = WindowState::new(1, mux);
    let target = super::WaylandDropTarget::capture(&ws);
    let pane = &ws.mux.panes[&ws.mux.active_focus().unwrap()];
    assert_eq!(pane.feed_focus_report(true), PaneInputResult::Queued);
    assert!(target == super::WaylandDropTarget::capture(&ws));
    assert_eq!(pane.feed_input(b"new paste "), PaneInputResult::Queued);
    assert!(target != super::WaylandDropTarget::capture(&ws));
    ws.mux.kill_children();
}

/// The key whose press closed or replaced a modal is remembered, so its
/// repeats are dropped: whether nothing is open afterwards, or another
/// modal took over (the palette running "Open layout picker"). A press the
/// modal kept is not remembered.
#[test]
fn a_key_that_closes_or_replaces_a_modal_stops_repeating() {
    use super::{KeyModal, drop_closing_key_repeat, remember_closing_key, top_modal};
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, PhysicalKey};

    let enter = PhysicalKey::Code(KeyCode::Enter);
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((String::new(), 0));
    let owner = top_modal(&ws);
    assert_eq!(owner, Some(KeyModal::Palette));

    // An input-method composition is under way in the palette.
    ws.ime_preedit = Some(("ka".into(), None));
    ws.ime_preedit_owner = Some(crate::window_state::ImePreeditSession {
        owner: crate::window_state::ImePreeditOwner::Other,
        generation: ws.ime_focus_generation,
        ended: None,
    });
    let generation = ws.ime_focus_generation;

    // Typing keeps the palette open: nothing to drop, and the composition
    // goes on.
    remember_closing_key(&mut ws, owner, enter);
    assert!(ws.closing_keys.is_empty());
    assert_eq!(ws.ime_focus_generation, generation);
    assert!(ws.ime_preedit.is_some());

    // Enter runs "Open layout picker": the palette is replaced, and its
    // composition ends so a late Commit cannot type into the picker.
    ws.palette_input = None;
    ws.layout_picker_input = Some((String::new(), 0));
    remember_closing_key(&mut ws, owner, enter);
    assert!(ws.closing_keys.contains(&enter));
    assert_ne!(ws.ime_focus_generation, generation);
    assert!(ws.ime_preedit.is_none());
    assert!(ws.ime_preedit_owner.is_some(), "the owner marker stays");
    assert!(
        drop_closing_key_repeat(&mut ws.closing_keys, enter, ElementState::Pressed, true),
        "the repeat must not reach the layout picker"
    );
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        enter,
        ElementState::Released,
        false
    ));
    assert!(ws.closing_keys.is_empty(), "the release ends it");

    // Escape closes the last modal: nothing open afterwards.
    let escape = PhysicalKey::Code(KeyCode::Escape);
    let owner = top_modal(&ws);
    ws.layout_picker_input = None;
    remember_closing_key(&mut ws, owner, escape);
    assert!(ws.closing_keys.contains(&escape));
    assert!(drop_closing_key_repeat(
        &mut ws.closing_keys,
        escape,
        ElementState::Pressed,
        true
    ));
    // A fresh press of Escape is never swallowed, and ends its
    // suppression.
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        escape,
        ElementState::Pressed,
        false
    ));
    assert!(ws.closing_keys.is_empty());
    // The Escape that cancels a keybind capture ends the capture mode,
    // so its repeats cannot go on to close Settings.
    ws.settings_nav = Some(crate::settings::SettingsNav {
        category: 0,
        field: 0,
        capturing: true,
    });
    let owner = top_modal(&ws);
    assert_eq!(owner, Some(KeyModal::SettingsCapture));
    ws.settings_nav.as_mut().unwrap().capturing = false;
    remember_closing_key(&mut ws, owner, escape);
    assert!(ws.closing_keys.contains(&escape));
}

/// Each held key that closed a modal stops repeating until its own
/// release: Enter runs a palette command that opens Settings, then
/// Escape, pressed while Enter is still down, closes Settings. Recording
/// Escape must not end Enter's suppression.
#[test]
fn every_held_closing_key_stops_repeating() {
    use super::{KeyModal, drop_closing_key_repeat, remember_closing_key, top_modal};
    use winit::event::ElementState;
    use winit::keyboard::{KeyCode, PhysicalKey};

    let enter = PhysicalKey::Code(KeyCode::Enter);
    let escape = PhysicalKey::Code(KeyCode::Escape);
    let mut ws = WindowState::new(0, Mux::new());
    ws.palette_input = Some((String::new(), 0));
    let owner = top_modal(&ws);
    ws.palette_input = None;
    ws.settings_nav = Some(crate::settings::SettingsNav {
        category: 0,
        field: 0,
        capturing: false,
    });
    remember_closing_key(&mut ws, owner, enter);
    let owner = top_modal(&ws);
    assert_eq!(owner, Some(KeyModal::Settings));
    ws.settings_nav = None;
    remember_closing_key(&mut ws, owner, escape);

    for key in [enter, escape] {
        assert!(
            drop_closing_key_repeat(&mut ws.closing_keys, key, ElementState::Pressed, true),
            "{key:?} closed a modal; its repeat must not reach the terminal"
        );
    }
    // Releasing Escape leaves Enter suppressed until its own release.
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        escape,
        ElementState::Released,
        false
    ));
    assert!(drop_closing_key_repeat(
        &mut ws.closing_keys,
        enter,
        ElementState::Pressed,
        true
    ));
    assert!(!drop_closing_key_repeat(
        &mut ws.closing_keys,
        enter,
        ElementState::Released,
        false
    ));
    assert!(ws.closing_keys.is_empty());
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

#[test]
fn keybind_resolver_uses_logical_before_physical_fallback() {
    use super::resolve_keybind_action;
    use kettle_config::{Action, Bindings, Key as KKey, Mods, Trigger};
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let kmods = Mods::CTRL | Mods::SHIFT;
    let mut bindings = Bindings::new();
    bindings.insert(Trigger::new(kmods, KKey::Char('=')), Action::ResetFontSize);
    bindings.insert(
        Trigger::new(kmods, KKey::Char('+')),
        Action::IncreaseFontSize,
    );

    let matched = resolve_keybind_action(
        &bindings,
        Some(&Key::Character("=".into())),
        Some(&PhysicalKey::Code(KeyCode::Equal)),
        mods,
    );
    assert_eq!(
        matched,
        Some((Trigger::new(kmods, KKey::Char('=')), Action::ResetFontSize))
    );
}

#[test]
fn keybind_resolver_honors_shifted_digit_rebinds_and_unbinds() {
    use super::resolve_keybind_action;
    use kettle_config::{Action, Config};
    use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

    let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
    let physical = PhysicalKey::Code(KeyCode::Digit1);
    for spelling in ["1", "!"] {
        for (action, expected) in [("copy", Some(Action::Copy)), ("unbind", None)] {
            let cfg = Config::parse_text(&format!("keybind = ctrl+shift+{spelling} = {action}\n"));
            for logical in ["1", "!"] {
                let matched = resolve_keybind_action(
                    &cfg.keybinds,
                    Some(&Key::Character(logical.into())),
                    Some(&physical),
                    mods,
                );
                assert_eq!(
                    matched.map(|(_, action)| action),
                    expected,
                    "{spelling}={action}, logical {logical}"
                );
            }
        }
    }
}

#[test]
fn keybind_resolver_falls_back_to_physical_zoom_keys() {
    use super::resolve_keybind_action;
    use kettle_config::Action;
    use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};

    let defaults = kettle_config::keybinds::defaults();
    let ctrl = ModifiersState::CONTROL;
    let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;

    for (physical, mods, expected) in [
        (
            PhysicalKey::Code(KeyCode::Equal),
            ctrl_shift,
            Action::IncreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::NumpadAdd),
            ctrl,
            Action::IncreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::Minus),
            ctrl,
            Action::DecreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::NumpadSubtract),
            ctrl,
            Action::DecreaseFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::Digit0),
            ctrl,
            Action::ResetFontSize,
        ),
        (
            PhysicalKey::Code(KeyCode::Numpad0),
            ctrl,
            Action::ResetFontSize,
        ),
    ] {
        let matched = resolve_keybind_action(&defaults, None, Some(&physical), mods)
            .unwrap_or_else(|| panic!("{physical:?} should resolve"));
        assert_eq!(matched.1, expected);
    }
    assert_eq!(
        resolve_keybind_action(
            &defaults,
            None,
            Some(&PhysicalKey::Code(KeyCode::KeyA)),
            ctrl,
        ),
        None
    );
}

#[test]
fn adaptive_alt_focus_matches_only_the_default_directional_pairs() {
    use super::{adaptive_alt_focus_direction, adaptive_alt_focus_falls_through};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};

    for (key, action, direction) in [
        (KKey::Up, Action::FocusUp, (0, -1)),
        (KKey::Down, Action::FocusDown, (0, 1)),
        (KKey::Left, Action::FocusLeft, (-1, 0)),
        (KKey::Right, Action::FocusRight, (1, 0)),
    ] {
        assert_eq!(
            adaptive_alt_focus_direction(Trigger::new(Mods::ALT, key), &action, true),
            Some(direction)
        );
    }

    assert_eq!(
        adaptive_alt_focus_direction(Trigger::new(Mods::ALT, KKey::Up), &Action::FocusDown, true,),
        None,
        "a customized mismatched action must stay an application keybind"
    );
    assert_eq!(
        adaptive_alt_focus_direction(
            Trigger::new(Mods::CTRL | Mods::ALT, KKey::Up),
            &Action::FocusUp,
            true,
        ),
        None,
        "extra modifiers are not the adaptive default chord"
    );
    assert_eq!(
        adaptive_alt_focus_direction(Trigger::new(Mods::ALT, KKey::Up), &Action::FocusUp, false,),
        None,
        "macOS leaves Option+Arrow to the PTY and focuses with Cmd+Opt or Ctrl+Cmd"
    );

    let trigger = Trigger::new(Mods::ALT, KKey::Up);
    assert!(
        adaptive_alt_focus_falls_through(trigger, &Action::FocusUp, true, false),
        "no visible neighbour: the chord belongs to the program"
    );
    assert!(
        !adaptive_alt_focus_falls_through(trigger, &Action::FocusUp, true, true),
        "a visible neighbour keeps the chord as a focus move"
    );
    assert!(
        !adaptive_alt_focus_falls_through(trigger, &Action::FocusDown, true, false),
        "a customised mismatched pair never falls through, neighbour or not"
    );
    assert!(
        !adaptive_alt_focus_falls_through(trigger, &Action::FocusUp, false, false),
        "the adaptive rule is off on macOS even with nowhere to go"
    );
}

/// A zoomed multi-pane tab hides its siblings, so the mux reports no
/// visible neighbour and the adaptive chord must fall through to the
/// program. That is why `adaptive_alt_focus_falls_through` takes the mux's
/// visible-neighbour answer instead of a zoom flag. A zoom-flag guard
/// would keep the chord as a Kettle no-op and swallow Codex's
/// `Alt+Left`/`Alt+Right` word motion while `Ctrl+Shift+X` or the
/// `scaled_zoom` action is active.
#[test]
fn adaptive_alt_focus_falls_through_while_zoom_hides_siblings() {
    use super::adaptive_alt_focus_falls_through;
    use crate::mux::{Dir, Mux, Node, Tab};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};

    let area = (0.0, 0.0, 800.0, 600.0);
    let mut mux = Mux::new();
    mux.tabs.push(Tab {
        root: Node::Split {
            dir: Dir::Horizontal,
            ratio: 0.5,
            a: Box::new(Node::Leaf(1)),
            b: Box::new(Node::Leaf(2)),
        },
        focus: 2,
        title_override: None,
        zoomed: false,
        lanes: Default::default(),
        last_output_at: None,
        last_seen_at: None,
        bell: false,
    });
    mux.active = 0;
    let left = (
        Trigger::new(Mods::ALT, KKey::Left),
        Action::FocusLeft,
        (-1, 0),
    );
    let right = (
        Trigger::new(Mods::ALT, KKey::Right),
        Action::FocusRight,
        (1, 0),
    );

    let route = |mux: &Mux, (trigger, action, (dx, dy)): &(Trigger, Action, (i32, i32))| {
        adaptive_alt_focus_falls_through(
            *trigger,
            action,
            true,
            mux.pane_in_direction(area, *dx, *dy).is_some(),
        )
    };

    assert!(
        !route(&mux, &left),
        "unzoomed: the right pane has a left neighbour, so Alt+Left moves focus"
    );
    assert!(
        route(&mux, &right),
        "unzoomed: nothing lies right of the right pane, so Alt+Right falls through"
    );

    mux.toggle_zoom();
    assert!(mux.is_zoomed());
    assert!(
        route(&mux, &left),
        "zoomed: the hidden left sibling is not a visible neighbour"
    );
    assert!(route(&mux, &right));

    mux.toggle_zoom();
    assert!(!route(&mux, &left), "leaving zoom restores the focus move");
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

fn claims(owns: bool) -> kettle_core::KeyboardClaims {
    kettle_core::KeyboardClaims {
        keyboard_protocol: owns,
        ..kettle_core::KeyboardClaims::default()
    }
}

fn one_tab() -> super::KettleActionState {
    super::KettleActionState {
        tabs: 1,
        current_tab: 0,
        selection_exists: false,
        focus_target_exists: false,
        chord_is_distinct: false,
    }
}

/// A chord the config binds itself is Kettle's even when the line
/// restates the default pair, `keybind-yield = off` turns every rule off,
/// and binding one chord leaves the others' rules alone.
#[test]
fn a_chord_the_user_binds_is_never_yielded() {
    use super::{ProgramKeyRule::*, configured_program_key_rule};
    use kettle_config::{Action, Config, Key as KKey, Mods, Trigger};
    let shift_left = Trigger::new(Mods::SHIFT, KKey::Left);
    let shift_right = Trigger::new(Mods::SHIFT, KKey::Right);
    let rule = |cfg: &Config, trigger, action: &Action| {
        configured_program_key_rule(cfg, trigger, action, false)
    };

    let stock = Config::parse_text("");
    assert_eq!(
        rule(&stock, shift_left, &Action::ResizeLeft),
        Some(ProgramFirst)
    );

    let restated = Config::parse_text("keybind = shift+left=resize_left\n");
    assert_eq!(
        restated.keybinds.get(&shift_left),
        Some(&Action::ResizeLeft)
    );
    assert_eq!(rule(&restated, shift_left, &Action::ResizeLeft), None);
    assert_eq!(
        rule(&restated, shift_right, &Action::ResizeRight),
        Some(ProgramFirst),
        "binding one chord leaves the other defaults' rules alone"
    );

    let imported = Config::parse_text("resize_left = <Shift>Left\n");
    assert_eq!(
        imported.keybinds.get(&shift_left),
        Some(&Action::ResizeLeft)
    );
    assert_eq!(rule(&imported, shift_left, &Action::ResizeLeft), None);

    let unbound_then_default = Config::parse_text("keybind = shift+left=unbind\n");
    assert!(unbound_then_default.keybinds_declared.is_empty());

    let off = Config::parse_text("keybind-yield = off\n");
    assert_eq!(rule(&off, shift_left, &Action::ResizeLeft), None);
}

/// Only the shipped default (trigger, action) pairs yield: the same
/// chord bound to another action, or the action moved to another chord,
/// stays Kettle's, as `adaptive_alt_focus_direction` pins for Alt+Arrow.
#[test]
fn program_key_rules_match_only_exact_default_pairs() {
    use super::{ProgramKeyRule::*, program_key_rule};
    use kettle_config::{Action, Key as KKey, Mods, Trigger};
    let shift = Mods::SHIFT;
    let ctrl = Mods::CTRL;
    let cs = Mods::CTRL | Mods::SHIFT;
    for (mods, key, action, rule) in [
        (shift, KKey::Left, Action::ResizeLeft, ProgramFirst),
        (shift, KKey::Right, Action::ResizeRight, ProgramFirst),
        (shift, KKey::Up, Action::ResizeUp, ProgramFirst),
        (shift, KKey::Down, Action::ResizeDown, ProgramFirst),
        (ctrl, KKey::Up, Action::JumpPrevPrompt, WhenKettleIdle),
        (ctrl, KKey::Down, Action::JumpNextPrompt, WhenKettleIdle),
        (shift, KKey::PageUp, Action::ScrollPageUp, WhenKettleIdle),
        (
            shift,
            KKey::PageDown,
            Action::ScrollPageDown,
            WhenKettleIdle,
        ),
        (cs, KKey::Up, Action::ScrollLineUp, WhenKettleIdle),
        (cs, KKey::Down, Action::ScrollLineDown, WhenKettleIdle),
        (ctrl, KKey::Home, Action::ScrollToTop, WhenKettleIdle),
        (ctrl, KKey::End, Action::ScrollToBottom, WhenKettleIdle),
        (shift, KKey::Home, Action::SelectToTop, WhenKettleIdle),
        (shift, KKey::End, Action::SelectToBottom, WhenKettleIdle),
        (ctrl, KKey::PageDown, Action::NextTab, WhenKettleIdle),
        (ctrl, KKey::PageUp, Action::PrevTab, WhenKettleIdle),
        (cs, KKey::PageDown, Action::MoveTabRight, WhenKettleIdle),
        (cs, KKey::PageUp, Action::MoveTabLeft, WhenKettleIdle),
        (
            Mods::ALT,
            KKey::Char('3'),
            Action::GotoTab(2),
            WhenKettleIdle,
        ),
        (
            cs,
            KKey::Char('n'),
            Action::FocusNext,
            WhenKettleIdleAndDistinct,
        ),
        (
            cs,
            KKey::Char('p'),
            Action::FocusPrev,
            WhenKettleIdleAndDistinct,
        ),
    ] {
        for macos in [false, true] {
            assert_eq!(
                program_key_rule(Trigger::new(mods, key), &action, macos),
                Some(rule),
                "{mods:?}+{key:?} -> {action:?}"
            );
        }
    }
    // macOS's directional focus chords, and only there: Alt+Arrow
    // elsewhere has its own adaptive rule.
    for mods in [Mods::ALT | Mods::SUPER, Mods::CTRL | Mods::SUPER] {
        for (key, action) in [
            (KKey::Up, Action::FocusUp),
            (KKey::Down, Action::FocusDown),
            (KKey::Left, Action::FocusLeft),
            (KKey::Right, Action::FocusRight),
        ] {
            let trigger = Trigger::new(mods, key);
            assert_eq!(
                program_key_rule(trigger, &action, true),
                Some(WhenKettleIdleAndDistinct),
                "{mods:?}+{key:?}"
            );
            assert_eq!(program_key_rule(trigger, &action, false), None);
        }
    }
    for (mods, key, action) in [
        (Mods::ALT, KKey::Left, Action::FocusLeft),
        (Mods::CTRL | Mods::ALT, KKey::Left, Action::FocusLeft),
        (Mods::ALT | Mods::SUPER, KKey::Left, Action::FocusRight),
        (cs, KKey::Char('n'), Action::FocusPrev),
        (ctrl, KKey::Char('n'), Action::FocusNext),
        (cs, KKey::Char('x'), Action::ToggleZoom),
    ] {
        for macos in [false, true] {
            assert_eq!(
                program_key_rule(Trigger::new(mods, key), &action, macos),
                None,
                "{mods:?}+{key:?} -> {action:?}"
            );
        }
    }
    // Other actions on the same chords, and the same actions elsewhere.
    for (mods, key, action) in [
        (shift, KKey::Left, Action::FocusLeft),
        (cs, KKey::Left, Action::ResizeLeft),
        (Mods::ALT, KKey::Left, Action::ResizeLeft),
        (ctrl, KKey::Up, Action::ScrollLineUp),
        (ctrl, KKey::Char('c'), Action::Copy),
        (cs, KKey::Char('c'), Action::Copy),
        (Mods::SUPER, KKey::Char('c'), Action::Copy),
        (shift, KKey::Home, Action::ScrollToTop),
        (ctrl, KKey::Char('-'), Action::DecreaseFontSize),
        (cs, KKey::Char('e'), Action::SplitRight),
    ] {
        for macos in [false, true] {
            assert_eq!(
                program_key_rule(Trigger::new(mods, key), &action, macos),
                None
            );
        }
    }
    // Platform chords: Cmd on macOS, Alt digits elsewhere, digits that
    // match their tab only.
    let cmd = Mods::SUPER;
    assert_eq!(
        program_key_rule(Trigger::new(cmd, KKey::Up), &Action::JumpPrevPrompt, true),
        Some(WhenKettleIdle)
    );
    assert_eq!(
        program_key_rule(Trigger::new(cmd, KKey::Up), &Action::JumpPrevPrompt, false),
        None
    );
    assert_eq!(
        program_key_rule(
            Trigger::new(cmd, KKey::Char('3')),
            &Action::GotoTab(2),
            false
        ),
        None
    );
    assert_eq!(
        program_key_rule(
            Trigger::new(cmd, KKey::Char('3')),
            &Action::GotoTab(2),
            true
        ),
        Some(WhenKettleIdle)
    );
    assert_eq!(
        program_key_rule(
            Trigger::new(Mods::ALT, KKey::Char('3')),
            &Action::GotoTab(0),
            false
        ),
        None
    );
}

/// Every rule names a chord this platform actually ships, and every
/// shipped chord that should yield has its rule, so a change to either
/// side cannot leave a rule matching nothing or a default without one.
#[test]
fn every_program_key_rule_is_a_shipped_default() {
    let macos = cfg!(target_os = "macos");
    // Labels name Alt and Super the way the platform's keyboard does.
    let (alt, cmd) = if macos {
        ("Opt", "Cmd")
    } else {
        ("Alt", "Super")
    };
    let defaults = kettle_config::keybinds::defaults();
    let mut yielding: Vec<String> = defaults
        .iter()
        .filter(|(trigger, action)| super::program_key_rule(**trigger, action, macos).is_some())
        .map(|(trigger, _)| trigger.label())
        .collect();
    yielding.sort();
    let mut expected: Vec<String> = [
        "Shift+Left",
        "Shift+Right",
        "Shift+Up",
        "Shift+Down",
        "Ctrl+Up",
        "Ctrl+Down",
        "Ctrl+Home",
        "Ctrl+End",
        "Shift+Home",
        "Shift+End",
        "Shift+PageUp",
        "Shift+PageDown",
        "Ctrl+Shift+Up",
        "Ctrl+Shift+Down",
        "Ctrl+PageUp",
        "Ctrl+PageDown",
        "Ctrl+Shift+PageUp",
        "Ctrl+Shift+PageDown",
        "Ctrl+Shift+N",
        "Ctrl+Shift+P",
    ]
    .iter()
    .map(|chord| chord.to_string())
    .chain((1..=9).map(|n| format!("{alt}+{n}")))
    .chain(
        macos
            .then(|| {
                [format!("{cmd}+Up"), format!("{cmd}+Down")]
                    .into_iter()
                    .chain((1..=9).map(|n| format!("{cmd}+{n}")))
                    .chain(
                        [format!("{alt}+{cmd}"), format!("Ctrl+{cmd}")]
                            .into_iter()
                            .flat_map(|m| {
                                ["Up", "Down", "Left", "Right"]
                                    .into_iter()
                                    .map(move |k| format!("{m}+{k}"))
                            }),
                    )
            })
            .into_iter()
            .flatten(),
    )
    .collect();
    expected.sort();
    assert_eq!(yielding, expected);
}

/// The adaptive Alt+Arrow fall-through obeys K1's rule that a chord the
/// config binds itself is Kettle's: the method's `enabled` flag, which the
/// pure predicates below treat as off, includes the declared-trigger check.
#[test]
fn adaptive_focus_respects_a_chord_the_user_binds() {
    let src = &super::app_source();
    let method = src
        .split("fn adaptive_focus_chord_falls_through(")
        .nth(1)
        .and_then(|s| s.split("\n    fn chord_falls_through(").next())
        .expect("adaptive_focus_chord_falls_through body");
    assert!(
        method.contains("!self.cfg.keybinds_declared.contains(&trigger)"),
        "a declared trigger must disable the adaptive fall-through"
    );
    assert!(
        method.contains("adaptive_alt_focus_direction(trigger, action, enabled)"),
        "the declared check must gate the direction lookup"
    );
    use super::adaptive_alt_focus_falls_through;
    use kettle_config::{Action, Key as KKey, Mods, Trigger};
    let alt_left = Trigger::new(Mods::ALT, KKey::Left);
    assert!(!adaptive_alt_focus_falls_through(
        alt_left,
        &Action::FocusLeft,
        false,
        false
    ));
}

/// A pane-focus chord with nowhere to go (a zoomed or single-pane tab, or
/// no pane on that side) goes to a program that owns the keyboard, but
/// only while the view is at the bottom and the program's keyboard mode
/// tells the chord apart from another key.
#[test]
fn focus_chords_with_nowhere_to_go_yield_only_when_distinct() {
    use super::{KettleActionState, ProgramKeyRule::WhenKettleIdleAndDistinct, program_gets_chord};
    use kettle_config::Action;
    let owns = claims(true);
    let nowhere = KettleActionState {
        chord_is_distinct: true,
        ..one_tab()
    };
    for action in [
        Action::FocusNext,
        Action::FocusPrev,
        Action::FocusUp,
        Action::FocusDown,
        Action::FocusLeft,
        Action::FocusRight,
    ] {
        let gets = |c: &kettle_core::KeyboardClaims, k: &KettleActionState| {
            program_gets_chord(WhenKettleIdleAndDistinct, &action, c, k)
        };
        assert!(gets(&owns, &nowhere), "{action:?} with nowhere to go");
        // A visible pane to move to keeps the chord Kettle's.
        let somewhere = KettleActionState {
            focus_target_exists: true,
            ..nowhere
        };
        assert!(!gets(&owns, &somewhere), "{action:?} with a target");
        // A mode that would send it as another key, or as nothing.
        let indistinct = KettleActionState {
            chord_is_distinct: false,
            ..nowhere
        };
        assert!(!gets(&owns, &indistinct), "{action:?} indistinct");
        // No program, or a view scrolled into history.
        assert!(!gets(&claims(false), &nowhere), "{action:?} no program");
        let scrolled = kettle_core::KeyboardClaims {
            history_size: 50,
            display_offset: 10,
            ..owns
        };
        assert!(!gets(&scrolled, &nowhere), "{action:?} scrolled back");
    }
}

/// The encoder decides distinctness: plain legacy mode sends Ctrl+Shift+N
/// as Ctrl+N and a Command chord as nothing; the kitty protocol and
/// modifyOtherKeys level 2 send each as itself.
#[test]
fn a_chord_is_distinct_only_where_the_encoder_tells_it_apart() {
    use super::chord_reaches_program_distinctly as distinct;
    use kettle_config::{Key as KKey, Mods, Trigger};
    use kettle_core::TermMode;
    let ctrl_shift_n = Trigger::new(Mods::CTRL | Mods::SHIFT, KKey::Char('n'));
    let cmd_opt_left = Trigger::new(Mods::ALT | Mods::SUPER, KKey::Left);
    let ctrl_cmd_up = Trigger::new(Mods::CTRL | Mods::SUPER, KKey::Up);
    let legacy = TermMode::empty();
    let kitty = TermMode::DISAMBIGUATE_ESC_CODES;
    let mok2 = TermMode::MODIFY_OTHER_KEYS_2;
    assert!(!distinct(ctrl_shift_n, legacy), "legacy sends Ctrl+N");
    assert!(distinct(ctrl_shift_n, kitty));
    assert!(distinct(ctrl_shift_n, mok2));
    assert!(
        !distinct(ctrl_shift_n, TermMode::MODIFY_OTHER_KEYS_1),
        "level 1 keeps Ctrl+N"
    );
    // A real press carries its Control text, which without disambiguation
    // or report-all sends it as legacy Ctrl+N even under kitty flags.
    for flags in [
        TermMode::REPORT_EVENT_TYPES,
        TermMode::REPORT_ALTERNATE_KEYS,
    ] {
        assert!(!distinct(ctrl_shift_n, flags), "{flags:?} alone");
    }
    assert!(distinct(ctrl_shift_n, TermMode::REPORT_ALL_KEYS_AS_ESC));
    assert!(distinct(
        ctrl_shift_n,
        TermMode::DISAMBIGUATE_ESC_CODES | TermMode::REPORT_EVENT_TYPES
    ));
    // An arrow carries no text, so event types alone encode it as itself.
    assert!(distinct(cmd_opt_left, TermMode::REPORT_EVENT_TYPES));
    for chord in [cmd_opt_left, ctrl_cmd_up] {
        assert!(!distinct(chord, legacy), "legacy has no Command bytes");
        assert!(
            !distinct(chord, mok2),
            "modifyOtherKeys has no Command bytes"
        );
        assert!(distinct(chord, kitty), "kitty has a super bit");
    }
}

/// Shift+Arrow resize goes to a program that owns the keyboard, never to
/// a shell idle at its prompt or a pane with no program modes.
#[test]
fn program_first_chords_follow_keyboard_ownership() {
    use super::{ProgramKeyRule::ProgramFirst, program_gets_chord};
    use kettle_config::Action;
    let resize = Action::ResizeLeft;
    assert!(program_gets_chord(
        ProgramFirst,
        &resize,
        &claims(true),
        &one_tab()
    ));
    assert!(!program_gets_chord(
        ProgramFirst,
        &resize,
        &claims(false),
        &one_tab()
    ));
    let idle_prompt = kettle_core::KeyboardClaims {
        keyboard_protocol: true,
        at_prompt: true,
        ..kettle_core::KeyboardClaims::default()
    };
    assert!(!program_gets_chord(
        ProgramFirst,
        &resize,
        &idle_prompt,
        &one_tab()
    ));
}

/// The other chords go to the program only when Kettle's action would do
/// nothing, and never while the view is scrolled into history.
#[test]
fn idle_kettle_chords_yield_only_when_the_action_does_nothing() {
    use super::{KettleActionState, ProgramKeyRule::WhenKettleIdle, program_gets_chord};
    use kettle_config::Action;
    let owns = claims(true);
    let gets = |action: Action, c: &kettle_core::KeyboardClaims, k: &KettleActionState| {
        program_gets_chord(WhenKettleIdle, &action, c, k)
    };
    // Prompt jumps.
    assert!(gets(Action::JumpPrevPrompt, &owns, &one_tab()));
    let prompt_above = kettle_core::KeyboardClaims {
        prompt_above: true,
        ..owns
    };
    assert!(!gets(Action::JumpPrevPrompt, &prompt_above, &one_tab()));
    assert!(gets(Action::JumpNextPrompt, &prompt_above, &one_tab()));
    // Scrolling at the bottom: nothing to scroll on the alternate screen
    // or without history; with history, only the downward chords.
    let alt = kettle_core::KeyboardClaims {
        alt_screen: true,
        ..owns
    };
    let history = kettle_core::KeyboardClaims {
        history_size: 500,
        ..owns
    };
    for action in [
        Action::ScrollToTop,
        Action::ScrollPageUp,
        Action::ScrollLineUp,
    ] {
        assert!(gets(action.clone(), &alt, &one_tab()), "{action:?} alt");
        assert!(gets(action.clone(), &owns, &one_tab()), "{action:?} empty");
        assert!(!gets(action.clone(), &history, &one_tab()), "{action:?}");
    }
    for action in [
        Action::ScrollToBottom,
        Action::ScrollPageDown,
        Action::ScrollLineDown,
    ] {
        assert!(gets(action.clone(), &history, &one_tab()), "{action:?}");
    }
    // Scrolled into history: every chord stays Kettle's, even at the top
    // where scrolling up would do nothing, and even with nothing to jump
    // to; the key would otherwise snap the view to the bottom.
    let scrolled_to_top = kettle_core::KeyboardClaims {
        history_size: 500,
        display_offset: 500,
        ..owns
    };
    for action in [
        Action::ScrollToTop,
        Action::ScrollPageUp,
        Action::ScrollLineUp,
        Action::ScrollPageDown,
        Action::JumpPrevPrompt,
        Action::JumpNextPrompt,
        Action::SelectToTop,
        Action::NextTab,
        Action::GotoTab(0),
    ] {
        assert!(
            !gets(action.clone(), &scrolled_to_top, &one_tab()),
            "{action:?} while scrolled back"
        );
    }
    // Shift+Home/End: a selection to extend keeps them Kettle's on either
    // screen; with none they go to the program on either screen.
    let extending = KettleActionState {
        selection_exists: true,
        ..one_tab()
    };
    for screen in [&alt, &history] {
        assert!(gets(Action::SelectToTop, screen, &one_tab()));
        assert!(!gets(Action::SelectToTop, screen, &extending));
        assert!(!gets(Action::SelectToBottom, screen, &extending));
    }
    // Tabs.
    let two_tabs = KettleActionState {
        tabs: 2,
        ..one_tab()
    };
    assert!(gets(Action::NextTab, &owns, &one_tab()));
    assert!(!gets(Action::NextTab, &owns, &two_tabs));
    assert!(!gets(Action::MoveTabLeft, &owns, &two_tabs));
    assert!(
        gets(Action::GotoTab(0), &owns, &two_tabs),
        "already the current tab"
    );
    assert!(!gets(Action::GotoTab(1), &owns, &two_tabs));
    assert!(gets(Action::GotoTab(4), &owns, &two_tabs), "no such tab");
    // Chords without a rule never count as idle.
    assert!(!gets(Action::Copy, &owns, &one_tab()));
    // None of it without a program.
    assert!(!gets(Action::NextTab, &claims(false), &one_tab()));
}

/// Every theme change reaches the panes' colour scheme (DEC mode 2031)
/// through the redraw-time sync, which fires on a new background even when
/// the theme stays dark, and a new pane spawns with the window's scheme.
#[test]
fn theme_changes_reach_every_panes_color_scheme() {
    let src = production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_string()
    };
    assert!(
        body("redraw").contains(
            "self.maybe_sync_native_theme(ws);\n        self.maybe_sync_color_scheme(ws);"
        ),
        "the colour scheme must sync at every redraw, after the native theme"
    );
    let sync = body("maybe_sync_color_scheme");
    assert!(sync.contains("(self.cfg.theme.is_dark(), self.cfg.theme.background)"));
    assert!(sync.contains("std::iter::once(&mut *ws).chain(self.windows.values_mut())"));
    assert!(sync.contains("state.mux.set_color_scheme(Some(scheme.0));"));
    let mux = include_str!("../mux.rs");
    assert!(
        mux.contains("color_scheme_dark: self.color_scheme_dark,"),
        "a new pane must spawn with its window's scheme"
    );
}

/// Real keys and the `dispatch_keybind` diagnostic share one routing
/// decision; the search bar keeps only the adaptive focus rule, since the
/// program cannot receive a key while the bar has the keyboard.
#[test]
fn keyboard_and_control_routes_share_the_program_key_decision() {
    let src = production_source();
    let body = |name: &str| {
        src.split(&format!("fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .to_string()
    };
    assert!(
        body("chord_falls_through").contains("|| self.chord_goes_to_program(ws, trigger, action)"),
        "both routes must consult the program-key decision"
    );
    let decision = body("chord_goes_to_program");
    for guard in [
        "configured_program_key_rule(&self.cfg, trigger, action, cfg!(target_os = \"macos\"))",
        "ws.mux.is_broadcast_on()",
        "pane.read_only || pane.pty_input_failed()",
        "pane.term.keyboard_claims()",
        "program_gets_chord(rule, action, &claims, &kettle)",
    ] {
        assert!(
            decision.contains(guard),
            "chord_goes_to_program lost {guard}"
        );
    }
    assert_eq!(
        src.matches("self.chord_falls_through(ws, trigger, &")
            .count(),
        2
    );
    let search = src
        .split("fn search_bar_shortcut(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("search_bar_shortcut");
    assert!(search.contains("self.adaptive_focus_chord_falls_through(ws, trigger, &action)"));
    assert!(!search.contains("chord_falls_through(ws, trigger, &action) ||"));
    assert!(!search.contains("self.chord_falls_through("));
    assert_eq!(
        src.matches("self.adaptive_focus_chord_falls_through(ws, trigger, &action)")
            .count(),
        1,
        "only the search bar calls the adaptive rule alone"
    );
}

/// `vim-menu-nav` drift guards. (1) The vim layer must run BEFORE the
/// mnemonic/typeahead catch-all in `context_menu_key`; moved after it,
/// bare `j`/`k` would be eaten as typeahead input and the nav would
/// silently die. (2) The catch-all's mnemonic lookup must pass the
/// reservation set so a row can never claim a nav letter while the
/// setting is on. Both are ordering/wiring contracts a behavioral test
/// can't see without a live window; pin them at the source.
#[test]
fn vim_menu_nav_intercepts_before_mnemonic_catchall() {
    let src = production_source();
    let body = src
        .split("fn context_menu_key(")
        .nth(1)
        .and_then(|s| s.split("\n    fn ").next())
        .expect("context_menu_key present");
    let vim = body
        .find("self.context_menu_vim_key(ws, key, event_loop)")
        .expect("context_menu_key must consult the vim layer");
    let catchall = body
        .find("assign_mnemonics(&menu.items, reserved)")
        .expect("the mnemonic lookup must pass the reservation set");
    assert!(
        vim < catchall,
        "vim nav must intercept BEFORE the mnemonic/typeahead catch-all"
    );
}

/// Drift guard. The single "Window padding" Settings
/// control must persist BOTH `window-padding-x` and `-y` so the result is
/// symmetric — persisting only X leaves Y at its default (lopsided). A
/// behavioral test needs the live overlay + persist path; pin the
/// dual-write at the source.
///
/// The Y mirror write's `persist_pref` result must also be checked and
/// surfaced via `fire_notify` on failure, like the general Settings write
/// path just above it. Otherwise a failed mirror write after a successful
/// X write leaves lopsided padding after restart with no error shown.
#[test]
fn window_padding_setting_writes_both_axes() {
    let src = production_source();
    assert!(
        src.contains(
            "if key_str == \"window-padding-x\" && !self.persist_pref(\"window-padding-y\", &new_val) {"
        ),
        "the Window-padding control must mirror its value to window-padding-y AND \
             check the mirror write's result"
    );
}

/// Drift guard. The GPU device-picker branch writes THREE keys
/// (vendor/device/name) and must check every `persist_pref` result.
/// Setting `settings_restart_pending = true` when none of the writes landed
/// on disk tells the user a restart will apply a pin that silently reverts
/// to auto. A behavioral test needs a live Settings overlay + a failing
/// config write; pin the checked-result shape at the source.
#[test]
fn gpu_picker_checks_persist_results_before_flagging_restart() {
    let src = production_source();
    let arm = src
        .split("if key_str == \"gpu\" {")
        .nth(1)
        .and_then(|s| {
            s.split("\n        // Notify if the Settings change can't")
                .next()
        })
        .expect("gpu picker branch present");
    assert!(
        arm.contains("let a = self.persist_pref(\"gpu-vendor-id\", \"0\");")
            && arm.contains("a && b && c"),
        "the auto branch must fold all three persist_pref results together"
    );
    assert!(
        arm.contains("if saved {")
            && arm.contains("ws.settings_restart_pending = true;")
            && arm.contains("fire_notify("),
        "settings_restart_pending must only be set when the writes actually succeeded, \
             and a failure must be surfaced via fire_notify"
    );
}

#[test]
fn surface_restart_hint_requires_a_successful_config_write() {
    let src = production_source();
    let arm = src
        .split("let saved = self.persist_pref(key_str, &new_val);")
        .nth(1)
        .and_then(|rest| rest.split("self.reload_config(ws);").next())
        .expect("settings persistence arm");
    assert!(
        arm.contains("if saved")
            && arm.contains("\"background-opacity\" | \"background-type\" | \"window-blur\"",)
            && arm.contains("ws.settings_restart_pending = true;"),
        "a failed surface-setting write must not promise that a new window will apply it"
    );
}

#[test]
fn accessibility_key_tracks_each_visible_panes_scroll_offset() {
    let src = production_source();
    let key = src
        .split_once("fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64")
        .unwrap()
        .1
        .split("fn publish_accessibility_if_due")
        .next()
        .unwrap();
    let panes = key
        .split_once("for (pane_id, partition) in layout {")
        .unwrap()
        .1;
    let flat: String = panes.split_whitespace().collect();
    assert!(flat.contains(
        "pane.term.term.lock().ok().map(|term|term.grid().display_offset()).hash(&muthasher);"
    ));
}

#[test]
fn accessibility_hashes_the_visible_completion_projection() {
    let src = production_source();
    let tree = src
        .split("fn accessibility_tree(&self, ws: &WindowState, overlay: &Overlay)")
        .nth(1)
        .and_then(|rest| rest.split("fn accessibility_key").next())
        .expect("accessibility_tree body");
    assert!(
        tree.contains("row.set_position_in_set(candidate.position + 1)")
            && tree.contains("list.set_size_of_set(completion.total)")
            && tree.contains("completion_overlay_rect(completion, self.menu_cell(ws))"),
        "assistive technology must hear each visible row's absolute position and the full set size"
    );
    let receipt = tree
        .split("if let Some(receipt) = overlay.media_paste_receipt.as_ref()")
        .nth(1)
        .and_then(|rest| rest.split("let mut root = Node::new(Role::Window)").next())
        .expect("media receipt accessibility nodes");
    assert!(
        receipt.contains("let mut node = Node::new(Role::Button)")
            && receipt.contains("node.add_action(AccessibilityAction::Click)")
            && receipt.contains(
                "media_receipt_accessibility_label(&receipt.kind, receipt.remote, &receipt.tr)"
            )
            && !receipt.contains("Role::Group"),
        "receipt bodies must advertise their action and visible video details"
    );
    let body = src
        .split("fn accessibility_key(&self, ws: &WindowState, overlay: &Overlay) -> u64")
        .nth(1)
        .and_then(|rest| rest.split("fn publish_accessibility_if_due").next())
        .expect("accessibility_key body");
    assert!(
        body.contains("overlay.completion.as_ref()"),
        "accessibility invalidation must use the frame's focus/modal/viewport-gated projection"
    );
    assert!(
        body.contains("accessibility_completion_geometry_key(completion, self.menu_cell(ws))"),
        "accessibility invalidation must include the exact visible completion geometry"
    );
    assert!(
        !body.contains("self.overlay(") && !body.contains("pane.term.completion()"),
        "accessibility must neither rebuild the overlay nor read hidden raw completion state"
    );

    let draw = src
        .split("let overlay = self.overlay(ws, reuse_pane_snapshots);")
        .nth(1)
        .and_then(|rest| rest.split("let area = self.area(ws);").next())
        .expect("overlay publication block");
    assert!(
        draw.contains("self.publish_accessibility_if_due(ws, &overlay);")
            && !draw.contains("if !reuse_pane_snapshots"),
        "snapshot-only hover redraws must not strand a pending accessibility update"
    );
}

#[test]
fn video_receipt_accessibility_names_the_visible_details() {
    let one = kettle_render::MediaPasteReceiptKind::Video {
        extension: "MP4".into(),
        size: 1_500,
        count: 1,
        preview_pending: false,
    };
    let en = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    assert_eq!(
        super::media_receipt_accessibility_label(&one, false, &en),
        "Video path pasted, MP4, 1.5 KB"
    );
    assert_eq!(
        super::media_receipt_accessibility_label(&one, false, &es),
        "Ruta de video pegada, MP4, 1.5 KB"
    );

    let batch = kettle_render::MediaPasteReceiptKind::Video {
        extension: "WEBM".into(),
        size: 999,
        count: 3,
        preview_pending: true,
    };
    assert_eq!(
        super::media_receipt_accessibility_label(&batch, true, &en),
        "3 video paths pasted, first is WEBM, 999 B, remote pane, local path only"
    );
    assert_eq!(
        super::media_receipt_accessibility_label(&batch, true, &es),
        "3 rutas de video pegadas; el primer video es WEBM y ocupa 999 B, panel remoto, solo ruta local"
    );
    let image = kettle_render::MediaPasteReceiptKind::Image {
        original_width: 1920,
        original_height: 1080,
    };
    assert_eq!(
        super::media_receipt_accessibility_label(&image, false, &en),
        "Image path pasted, 1920 by 1080"
    );
    assert_eq!(
        super::media_receipt_accessibility_label(&image, false, &es),
        "Ruta de imagen pegada, 1920 por 1080"
    );
}

#[test]
fn media_receipt_accessibility_id_cannot_collide_with_completion_rows() {
    for receipt in [
        ACCESSIBILITY_MEDIA_RECEIPT_ID,
        ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID,
    ] {
        assert_ne!(
            receipt,
            accessibility_completion_row_id(0),
            "duplicate child ids make AccessKit panic when both overlays project"
        );
        assert_ne!(receipt, ACCESSIBILITY_COMPLETION_CONTAINER_ID);
    }
    assert_ne!(
        ACCESSIBILITY_MEDIA_RECEIPT_ID,
        ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID
    );
}

#[test]
fn completion_accessibility_geometry_key_tracks_layout_and_cell_metrics() {
    let completion = kettle_render::CompletionOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (20.0, 30.0, 800.0, 500.0),
        grid_rect: (28.0, 46.0, 784.0, 468.0),
        command_rows: (20, 21),
        anchor_col: None,
        kind: "Completions".to_string(),
        source: "fish".to_string(),
        selected: Some(0),
        total: 2,
        token: Some("al".to_string()),
        candidates: vec![
            kettle_render::CompletionOverlayRow {
                label: "alpha".to_string(),
                description: "first".to_string(),
                position: 0,
            },
            kettle_render::CompletionOverlayRow {
                label: "beta".to_string(),
                description: "second".to_string(),
                position: 1,
            },
        ],
    };
    let baseline = super::accessibility_completion_geometry_key(&completion, (9.0, 18.0));
    let rect = kettle_render::completion_overlay_rect(&completion, (9.0, 18.0))
        .expect("completion card geometry");
    assert!(super::completion_overlay_hit_test(
        Some(&completion),
        (9.0, 18.0),
        (rect.0 + rect.2 / 2.0, rect.1 + rect.3 / 2.0),
    ));
    assert!(!super::completion_overlay_hit_test(
        Some(&completion),
        (9.0, 18.0),
        (rect.0 - 1.0, rect.1),
    ));
    assert!(!super::completion_overlay_hit_test(
        None,
        (9.0, 18.0),
        (rect.0, rect.1),
    ));
    assert_ne!(
        baseline,
        super::accessibility_completion_geometry_key(&completion, (10.0, 20.0)),
        "font and cell-size changes must invalidate accessible row bounds"
    );

    let mut moved = completion.clone();
    moved.grid_rect.0 += 13.0;
    moved.grid_rect.1 += 7.0;
    assert_ne!(
        baseline,
        super::accessibility_completion_geometry_key(&moved, (9.0, 18.0)),
        "pane and grid movement must invalidate accessible row bounds"
    );
}

/// The card grew a header. It belongs to the list container, so no exposed
/// row may start at the top of the card or overlap the header band, and the
/// header press must still dismiss rather than reach terminal content.
#[test]
fn completion_rows_start_below_the_header_band() {
    let completion = kettle_render::CompletionOverlay {
        tr: kettle_i18n::Translator::default(),
        pane_rect: (20.0, 30.0, 800.0, 500.0),
        grid_rect: (28.0, 46.0, 784.0, 468.0),
        command_rows: (20, 21),
        anchor_col: None,
        kind: "Completions".to_string(),
        source: "fish".to_string(),
        selected: Some(1),
        total: 2,
        token: None,
        candidates: vec![
            kettle_render::CompletionOverlayRow {
                label: "alpha".to_string(),
                description: "first".to_string(),
                position: 0,
            },
            kettle_render::CompletionOverlayRow {
                label: "beta".to_string(),
                description: "second".to_string(),
                position: 1,
            },
        ],
    };
    let cell = (9.0, 18.0);
    let (x, y, width, height) =
        kettle_render::completion_overlay_rect(&completion, cell).expect("card geometry");
    let rows = kettle_render::completion_overlay_row_rects(&completion, cell);
    assert_eq!(rows.len(), 2);
    let first_row_top = rows[0].1.1;
    assert!(
        first_row_top > y,
        "the header band must sit above every exposed candidate row"
    );
    for (_, row) in &rows {
        assert!(row.1 >= first_row_top && row.1 + row.3 <= y + height);
        assert_eq!(row.0, x, "rows span the card so hit targets match paint");
        assert_eq!(row.2, width);
    }
    assert!(
        super::completion_overlay_hit_test(Some(&completion), cell, (x + width / 2.0, y + 1.0),),
        "a press on the header dismisses instead of reaching hidden content"
    );
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
fn resized_ignores_degenerate_size() {
    let src = production_source();
    let arm = src
        .split("WindowEvent::Resized(size) => {")
        .nth(1)
        .expect("Resized arm present");
    let head = &arm[..arm.len().min(900)];
    assert!(
        head.contains("size.width == 0 || size.height == 0") && head.contains("return"),
        "the Resized handler must early-return on a 0-dimension size"
    );
}

/// The `▾` menu is shells → separator → Windows
/// Terminal's bottom section (Settings / Command palette / About).
#[test]
fn new_tab_menu_items_has_wt_bottom_rows() {
    let shells = vec![
        ("PowerShell".to_string(), vec!["pwsh.exe".to_string()]),
        ("Git Bash".to_string(), vec!["bash.exe".to_string()]),
    ];
    let items = App::new_tab_menu_items(
        &shells,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(items.len(), 6, "2 shells + separator + 3 bottom rows");
    assert!(matches!(
        &items[0],
        ContextMenuItem::NewTabShell { label, .. } if label == "PowerShell"
    ));
    assert!(matches!(items[2], ContextMenuItem::Separator));
    assert!(matches!(
        items[3],
        ContextMenuItem::Item {
            label: "Settings…",
            action: Action::OpenSettings,
            enabled: true
        }
    ));
    assert!(matches!(
        items[4],
        ContextMenuItem::Item {
            label: "Command palette",
            action: Action::CommandPalette,
            enabled: true
        }
    ));
    assert!(matches!(
        items[5],
        ContextMenuItem::Item {
            label: "About kettle",
            action: Action::About,
            enabled: true
        }
    ));
    // Every dispatchable row gets a distinct mnemonic; the two-pass
    // assignment must keep working with the new bottom rows.
    let mn = assign_mnemonics(&items, &[]);
    let mut letters: Vec<char> = mn.iter().flatten().map(|(_, c)| *c).collect();
    assert_eq!(letters.len(), 5, "all 5 dispatchable rows get mnemonics");
    letters.sort_unstable();
    letters.dedup();
    assert_eq!(letters.len(), 5, "mnemonics are distinct");
}

/// An `Info` row (About panel) is inert — not
/// dispatchable, no click mapping, survives filter_disabled.
#[test]
fn info_rows_are_inert_but_visible() {
    let info = ContextMenuItem::Info {
        label: "kettle 9.9.9".to_string(),
    };
    assert!(!super::item_is_dispatchable(&info));
    assert!(super::item_to_click(&info, 0).is_none());
    let kept = filter_disabled(vec![
        ContextMenuItem::Info {
            label: "x".to_string(),
        },
        ContextMenuItem::Separator,
        item("Copy version info", true),
    ]);
    assert_eq!(kept.len(), 3, "Info + separator + item all survive");
}

/// PERF (key-repeat stutter fix) drift guard: output inside the typing
/// window bypasses the coalescer (paints immediately); output after it
/// re-enters the frame-budget defer. Without the bypass, holding a key
/// stutters because echo paints ride the WaitUntil deadline (~16ms Windows
/// timer granularity) instead of the keystroke cadence.
#[test]
fn typed_echo_bypasses_the_output_coalescer() {
    use std::time::{Duration, Instant};
    let base = Instant::now();
    let now = base + Duration::from_millis(500);
    // Echo within the window → immediate (bypass).
    assert!(super::typed_recently(
        now,
        Some(base + Duration::from_millis(470)),
        super::TYPING_ECHO_WINDOW
    ));
    // Stale keystroke → back to the coalescer.
    assert!(!super::typed_recently(
        now,
        Some(base),
        super::TYPING_ECHO_WINDOW
    ));
    // Never typed → coalescer.
    assert!(!super::typed_recently(now, None, super::TYPING_ECHO_WINDOW));
    // The window must bridge OS key-repeat intervals (~33ms default) with
    // slack for ConPTY echo latency.
    assert!(super::TYPING_ECHO_WINDOW >= Duration::from_millis(100));
    // The Wakeup arm must consult it (source-level pin: the bypass calls
    // request_redraw and clears any pending coalesce).
    let src = production_source();
    assert!(
        src.contains(
            "if typed_recently(now, ws.last_typed, TYPING_ECHO_WINDOW) {\n                    ws.output_pacer.reset();"
        ),
        "the Wakeup arm must paint echo immediately"
    );
}

/// Peacock drift guard: the live-dedupe pool walk. Same project →
/// same starting slot; a collision with a live window advances to the
/// next free hue; a fully-claimed pool accepts the seed slot.
#[test]
fn accent_slot_walk_dedupes_live_windows() {
    let pool: Vec<kettle_config::Rgb> =
        (0u8..4).map(|i| kettle_config::Rgb::new(i, i, i)).collect();
    // Empty in-use → the seed's own slot.
    assert_eq!(super::pick_accent_slot(&pool, 1, &[]), 1);
    assert_eq!(
        super::pick_accent_slot(&pool, 5, &[]),
        1,
        "seed wraps modulo"
    );
    // Seed slot taken → next free.
    assert_eq!(super::pick_accent_slot(&pool, 1, &[pool[1]]), 2);
    // Walk wraps past the end.
    assert_eq!(super::pick_accent_slot(&pool, 3, &[pool[3], pool[0]]), 1);
    // Fully claimed → fall back to the seed slot (accept the collision).
    assert_eq!(
        super::pick_accent_slot(&pool, 2, &[pool[0], pool[1], pool[2], pool[3]]),
        2
    );
    // Degenerate empty pool.
    assert_eq!(super::pick_accent_slot(&[], 7, &[]), 0);
    // Presence wire format round-trips through Rgb::parse.
    let c = kettle_config::Rgb::new(0xcb, 0xa6, 0xf7);
    assert_eq!(
        kettle_config::Rgb::parse(&crate::window_accent::rgb_hex(c)),
        Some(c)
    );
}

/// Regression guard. `resumed_inner` must take ONLY the consumed-once CLI
/// fields (`command`, `cwd`) from `self.startup`. A wholesale
/// `mem::take(&mut self.startup)` silently defaults every later read: the
/// `--tab-handoff` / `--layout` / `--restore` startup gates (which then
/// load nothing, so a 2-tab `--layout` opens 1 tab), the save-session
/// layout/restore gating, and reload_config's launch-override
/// re-application.
#[test]
fn startup_is_not_taken_wholesale() {
    let src = production_source();
    let taken = concat!("std::mem::take", "(&mut self.startup)");
    assert!(
        !src.contains(taken),
        "self.startup must never be wholesale-taken; take the \
             consumed-once fields individually"
    );
    assert!(
        src.contains("let cmd_override = self.startup.command.take();")
            && src.contains("let cwd_override = self.startup.cwd.take();"),
        "the -e/-d overrides are the only consumed-once startup fields"
    );
}

/// Drift guard for `theme-mode = auto` / `system` / `follow-system`.
/// OS appearance following must apply both the initial window theme and
/// live `WindowEvent::ThemeChanged` updates, while `theme-schedule` remains
/// the explicit owner when configured.
#[test]
fn system_theme_following_is_wired() {
    let src = production_source();
    assert!(
        src.contains(concat!(
            "WindowEvent::ThemeChanged",
            "(theme) => {\n                self.apply_os_theme_preference(ws, theme);"
        )),
        "winit ThemeChanged events must feed theme-mode=auto"
    );
    assert!(
        src.contains(concat!("ws.window.as_ref().and_then", "(|w| w.theme())")),
        "startup must apply the platform's current window theme when winit reports one"
    );
    assert!(
        src.contains(concat!(
            "self.apply_initial_os_theme_preference",
            "(ws, theme);"
        )),
        "startup OS theme application must use the pre-session-save path"
    );
    let initial_body = src
        .split(concat!("fn apply_initial_os_theme_preference", "("))
        .nth(1)
        .and_then(|s| s.split(concat!("fn apply_os_theme_preference", "(")).next())
        .expect("initial OS theme helper exists");
    assert!(
        !initial_body.contains(concat!("save_", "session")),
        "initial OS theme application must not save an empty startup session"
    );
    assert!(
        src.contains(concat!(
            "if self.cfg.theme_schedule.is_some",
            "() {\n            return None;\n        }"
        )),
        "theme-schedule must not fight OS appearance following"
    );
}

#[test]
fn runtime_theme_changes_synchronize_lua_and_redraw_every_window() {
    let src = production_source();
    let state_body = src
        .split(concat!("fn set_runtime_theme_state", "("))
        .nth(1)
        .and_then(|body| body.split(concat!("fn set_runtime_theme_name", "(")).next())
        .expect("runtime theme state helper exists");
    assert!(
        state_body.contains(concat!("self.cfg.", "theme_name = name;"))
            && state_body.contains(concat!("self.cfg.", "theme = theme;"))
            && state_body.contains(concat!("self.", "sync_lua_active_theme();"))
            && state_body.contains(concat!("self.", "request_all_window_redraws(ws);")),
        "palette, Lua introspection, and redraw invalidation must be one transition"
    );

    let theme_assignment = concat!("self.cfg.", "theme_name = name;");
    assert_eq!(
        src.matches(theme_assignment).count(),
        1,
        "runtime code must not bypass the synchronized theme transition"
    );

    let redraw_body = src
        .split(concat!("fn request_all_window_redraws", "("))
        .nth(1)
        .and_then(|body| body.split(concat!("fn sync_lua_active_theme", "(")).next())
        .expect("all-window redraw helper exists");
    assert!(
        redraw_body.contains(concat!("for (&seq, window) in &self.", "windows"))
            && redraw_body.contains("seq != ws.seq")
            && redraw_body.contains(concat!("native.", "request_redraw();")),
        "a global theme change must invalidate every mapped sibling window"
    );
}

/// Multi-window drift guard. A window close must NOT exit the event loop
/// directly. It requests close for its exact window id, and the single
/// funnel (`finish_window_dispatch`) drops the window, exiting only when
/// the windows map is empty. The only legitimate direct exits are the
/// funnel itself (close + quit arms) and `resumed_inner`'s window-1
/// startup failures (window create / renderer init / `-e` spawn / shell
/// spawn), where no other window can exist yet. A new `event_loop.exit()`
/// anywhere else makes closing one window kill them all.
#[test]
fn event_loop_exit_sites_are_allowlisted() {
    let src = production_source();
    // concat! so this test's own literals don't self-match the scan.
    let exit_needle = concat!("event_loop", ".exit();");
    let n_exits = src.matches(exit_needle).count();
    assert_eq!(
        n_exits, 7,
        "expected exactly 7 event_loop.exit() sites (2 in \
             finish_window_dispatch + 5 resumed_inner startup failures, one of \
             them the first pane failing to start before the window); a new \
             one must route through request_window_close instead"
    );
    // The close paths all flag instead of exiting: keybind CloseTab /
    // ClosePane / CloseWindow, the three confirm-dialog arms, the OS
    // close button, the tab-bar ✕ on the last tab, and the reap path.
    let flag_needle = concat!("self.request_window_close", "(ws.seq);");
    let n_flags = src.matches(flag_needle).count();
    assert!(
        n_flags >= 9,
        "expected the 9 window-close paths to request their own window close \
             (found {n_flags})"
    );
    assert!(
        src.contains("pending_window_closes: std::collections::BTreeSet<u64>")
            && src.contains("if take_window_close_request(&mut self.pending_window_closes, seq)"),
        "close requests must be keyed and consumed by the dispatched window id"
    );
}

#[test]
fn one_windows_close_request_cannot_be_consumed_by_a_sibling() {
    use super::take_window_close_request;

    let mut pending = std::collections::BTreeSet::from([20]);
    assert!(!take_window_close_request(&mut pending, 10));
    assert!(
        pending.contains(&20),
        "window 10 must leave window 20 armed"
    );
    assert!(take_window_close_request(&mut pending, 20));
    assert!(pending.is_empty());
}

/// A side-button press reaches the terminal only past every surface that
/// owns the pointer, and its release only when the press did, once.
#[test]
fn side_button_forward_is_modal_gated_and_releases_pair_with_presses() {
    // Collapse all whitespace before scanning. rustfmt wraps the call
    // across lines, and the match must also survive a CRLF working tree
    // (which `.gitattributes eol=lf` normally prevents).
    let src = production_source()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        src.contains(
            "if !self.press_preview_lane(ws, px, py, sgr) \
                 && !modal_swallows_pointer( self.pointer_modal_open(ws), ws.context_menu.is_some(), ) \
                 { self.report_mouse(ws, sgr, true, false); }"
        ),
        "a side-button press is forwarded past the lanes and modals"
    );
    assert!(
        src.contains(
            "if let Some(sgr) = extra_mouse_sgr(button) { ws.held_buttons.end_own(sgr); self.send_mouse(ws, sgr, false, false); return; }"
        ),
        "its release goes only where a press went: report_mouse declines one no program had"
    );
    assert!(
        src.contains(
            "self.release_held_buttons(ws, |held| held.button == sgr); ws.held_buttons.pressed(sgr, None, (0, 0)); if self.confirm_mouse_press(ws, event_loop, sgr) { return; } // A lone context menu"
        ),
        "a side press first releases one whose release went elsewhere, and is Kettle's until a program gets it"
    );
    // A press the search bar keeps from the terminal is not one it saw.
    let send = production_source();
    let send_to = send
        .split("fn send_mouse_to(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("send_mouse_to");
    assert!(send_to.contains(
        "ws.held_buttons.any(),\n        ) {\n            return MouseReport::Swallowed;"
    ));
    assert_eq!(send_to.matches("MouseReport::Written").count(), 1);
    assert!(super::MouseReport::Swallowed.consumed() && !super::MouseReport::Declined.consumed());
}

/// A press a program got keeps its pane: that button's drag and its one
/// release go there wherever focus has gone, Shift or not, and one whose
/// pane left the screen gets only the release, at the cell it last saw.
/// A press no program got leaves no button held; a release takes room
/// the program's input keeps for releases, so a full queue cannot refuse
/// it. The newest press owns the drag, and a gesture of Kettle's own ends
/// with its release whoever handles it. Kettle ending a program's gesture
/// early (focus loss, search, the menu, a tab leaving, the button pressed
/// again) releases it in place.
#[test]
fn a_held_buttons_reports_stay_with_the_pane_its_press_went_to() {
    let source = production_source();
    let body = |name: &str| {
        source
            .split(&format!("    fn {name}("))
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name}"))
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    let report = body("report_mouse");
    let release = report
        .find("(false, _) => match ws.held_buttons.take(btn) { Some(held) if held.pane.is_some() => Some(held),")
        .expect("a release takes its press's entry, and only a program's is reported");
    let drag = report
        .find("(true, true) => ws.held_buttons.dragging(),")
        .expect("a drag follows the newest held button");
    let routed = report
        .find("return self.send_mouse_to(ws, pane, at, button, pressed, motion);")
        .expect("held reports go to their pane");
    let focus = report.find("ws.mux.active_focus()").expect("focus");
    assert!(release < routed && drag < routed && routed < focus);
    assert!(report.contains(
        "(MouseReport::Written, Some((pane, row, col))) => (Some(pane), (row, col)), _ => (None, (0, 0)), }; if let Some(stale) = ws.held_buttons.pressed(btn, pane, cell) { self.release_held(ws, stale); }"
    ));
    let send = body("send_mouse_to");
    assert!(send.contains("if ws.mods.shift_key() && !held {"));
    assert!(send.contains(
        "if matches!(at, MouseAt::Held { rect: None, .. }) && pressed { return MouseReport::Swallowed; }"
    ));
    assert!(send.contains("MouseAt::Held { rect: None, cell } => cell,"));
    assert!(send.contains(
        "let result = if pressed || motion { pane.feed_input(&seq) } else { pane.feed_mouse_release(&seq) };"
    ));
    assert!(send.contains(
        "if !result.is_queued() { // The program never got it: a press it missed leaves no button // held, so no release follows. self.report_input_result(result); return MouseReport::Swallowed; }"
    ));
    assert!(send.contains("ws.held_buttons.told(pane_id, (row, col));"));
    // Kettle releases what it ends early.
    for ends in ["open_search", "show_context_menu"] {
        assert!(
            body(ends).contains("self.release_held_buttons(ws, |_| true);"),
            "{ends}"
        );
    }
    assert!(
        source.split_whitespace().collect::<Vec<_>>().join(" ").contains(
            "Action::TogglePaneReadOnly => { // A pane turned read-only gets no more input, so a held // button's release goes to it first. if let Some(pane) = ws.mux.active_focus() && ws.mux.focused().is_some_and(|focused| !focused.read_only) { self.release_held_buttons(ws, |held| held.pane == Some(pane)); }"
        ),
        "a pane turning read-only gets its held releases first"
    );
    let flat_source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    for reset in [
        "self.release_held_buttons(ws, |held| held.pane == Some(pane_id)); apply_terminal_control(&ws.mux, &[pane_id], b\"\\x1bc\");",
        "self.release_held_buttons(ws, |held| held.pane == Some(pane_id)); // One request, so a full queue cannot accept RIS but reject // the paired history clear. apply_terminal_control(&ws.mux, &[pane_id], b\"\\x1bc\\x1b[3J\");",
    ] {
        assert!(
            flat_source.contains(reset),
            "a reset releases held buttons first: {reset}"
        );
    }
    let flat = source.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        flat.matches("self.release_held_buttons_in_tab(").count(),
        flat.matches(".mux.detach_tab(").count(),
        "every tab that leaves a window releases its held buttons first"
    );
    for needle in [
        // Each press first releases one whose release went elsewhere, and
        // is Kettle's own gesture until a program receives it.
        "self.release_held_buttons(ws, |held| held.button == bcode); ws.held_buttons.pressed(bcode, None, (0, 0)); if self.confirm_mouse_press(ws, event_loop, bcode) { return; } // Context menu:",
        "if bcode == 0 { ws.card_press = None; } if ws.card_focus.take().is_some() { ws.accessibility_pending = true; } self.release_held_buttons(ws, |held| held.button == bcode); ws.held_buttons.pressed(bcode, None, (0, 0)); if self.confirm_mouse_press(ws, event_loop, bcode) { return true; } let bar",
        // Each release ends a gesture of Kettle's own first.
        "ws.held_buttons.end_own(bcode); // A release ends an editor drag inside the search bar;",
        "if let Some(sgr) = extra_mouse_sgr(button) { ws.held_buttons.end_own(sgr); self.send_mouse(ws, sgr, false, false); return; }",
        "// As natively: a gesture of Kettle's own ends with its release. ws.held_buttons.end_own(bcode);",
    ] {
        assert!(flat.contains(needle), "{needle}");
    }
}

fn item(label: &'static str, enabled: bool) -> ContextMenuItem {
    // Any concrete Action works — the filter only looks at the
    // `enabled` flag + variant kind, not the action payload.
    ContextMenuItem::Item {
        label,
        action: Action::Paste,
        enabled,
    }
}

/// Drift guard. Disabled `Item`s are removed entirely
/// (Terminator-style "only show what you can click") and
/// orphaned/leading/trailing separators collapse so the menu
/// never has visual gaps that lead nowhere.
#[test]
fn disabled_items_are_hidden_and_separators_collapse() {
    // Mixed: Copy disabled, Paste enabled, separator, Split disabled,
    // separator, NewTab enabled, separator (trailing).
    let menu = vec![
        item("Copy", false),
        item("Paste", true),
        ContextMenuItem::Separator,
        item("Split Right", false),
        ContextMenuItem::Separator,
        item("New Tab", true),
        ContextMenuItem::Separator,
    ];
    let got = filter_disabled(menu);
    // Expected: Paste, separator, New Tab.
    assert_eq!(got.len(), 3);
    assert!(matches!(&got[0], ContextMenuItem::Item { label, .. } if *label == "Paste"));
    assert!(matches!(&got[1], ContextMenuItem::Separator));
    assert!(matches!(&got[2], ContextMenuItem::Item { label, .. } if *label == "New Tab"));
}

/// Runs of separators collapse to a single one; a
/// leading separator (everything-disabled above it) gets dropped.
#[test]
fn consecutive_separators_collapse_and_leading_is_dropped() {
    let menu = vec![
        ContextMenuItem::Separator,
        ContextMenuItem::Separator,
        item("OK", true),
        ContextMenuItem::Separator,
        ContextMenuItem::Separator,
        ContextMenuItem::Separator,
        item("Cancel", true),
    ];
    let got = filter_disabled(menu);
    assert_eq!(got.len(), 3);
    assert!(matches!(&got[0], ContextMenuItem::Item { label, .. } if *label == "OK"));
    assert!(matches!(&got[1], ContextMenuItem::Separator));
    assert!(matches!(&got[2], ContextMenuItem::Item { label, .. } if *label == "Cancel"));
}

/// Filter is identity (modulo trailing separator) when
/// nothing is disabled.
#[test]
fn filter_disabled_is_near_identity_when_all_enabled() {
    let menu = vec![item("A", true), ContextMenuItem::Separator, item("B", true)];
    let got = filter_disabled(menu);
    assert_eq!(got.len(), 3);
}

/// Empty menu stays empty (defensive).
#[test]
fn filter_disabled_handles_empty() {
    let got = filter_disabled(vec![]);
    assert!(got.is_empty());
}

/// All-disabled menu collapses to empty (no rows, no
/// orphan separators).
#[test]
fn filter_disabled_collapses_all_disabled_to_empty() {
    let menu = vec![
        item("X", false),
        ContextMenuItem::Separator,
        item("Y", false),
    ];
    let got = filter_disabled(menu);
    assert!(got.is_empty());
}

/// Drift guard. The Preferences ▸ submenu builder
/// must surface every runtime-mutable toggle the spec promises:
///   - 3 scrollbar radio rows
///   - 3 boolean toggles (cursor blink, copy on select, mouse-hide)
///   - 3 close-confirmation radio rows
///   - 4 bell radio rows
///   - 2 font-size +/− rows
///   - 1 Advanced… (EditConfig) escape hatch
///
/// Total: 16 actionable rows + 5 separators = 21 items. The test pins the
/// 16 actions and their `from_name` spellings; it does not count rows.
#[test]
fn preferences_submenu_contains_all_user_facing_toggles() {
    // append_preferences_submenu_items needs an App, so pin the submenu's
    // Action variants instead to keep the keybinds-side palette wiring in
    // sync.
    let expected_actions: &[Action] = &[
        Action::SetScrollbarAlways,
        Action::SetScrollbarAuto,
        Action::SetScrollbarNever,
        Action::ToggleCursorBlink,
        Action::ToggleCopyOnSelect,
        Action::ToggleMouseHide,
        Action::SetAskBeforeClosingAlways,
        Action::SetAskBeforeClosingMultiple,
        Action::SetAskBeforeClosingNever,
        Action::SetBellOff,
        Action::SetBellVisual,
        Action::SetBellAttention,
        Action::SetBellBoth,
        Action::IncreaseFontSize,
        Action::DecreaseFontSize,
        Action::EditConfig,
    ];
    // Each action parses from its name (the `Action::from_name`
    // surface) so the keybind grammar accepts them. Catches the
    // case where someone adds a variant but forgets the
    // from_name arm.
    for a in expected_actions {
        // We need a known name string for each. Use the canonical
        // forms documented in palette.rs / keybinds.rs.
        let name = match a {
            Action::SetScrollbarAlways => "set_scrollbar_always",
            Action::SetScrollbarAuto => "set_scrollbar_auto",
            Action::SetScrollbarNever => "set_scrollbar_never",
            Action::ToggleCursorBlink => "toggle_cursor_blink",
            Action::ToggleCopyOnSelect => "toggle_copy_on_select",
            Action::ToggleMouseHide => "toggle_mouse_hide",
            Action::SetAskBeforeClosingAlways => "set_ask_before_closing_always",
            Action::SetAskBeforeClosingMultiple => "set_ask_before_closing_multiple",
            Action::SetAskBeforeClosingNever => "set_ask_before_closing_never",
            Action::SetBellOff => "set_bell_off",
            Action::SetBellVisual => "set_bell_visual",
            Action::SetBellAttention => "set_bell_attention",
            Action::SetBellBoth => "set_bell_both",
            Action::IncreaseFontSize => "increase_font_size",
            Action::DecreaseFontSize => "decrease_font_size",
            Action::EditConfig => "edit_config",
            _ => unreachable!(),
        };
        let parsed = Action::from_name(name)
            .unwrap_or_else(|| panic!("Action::from_name({name:?}) returned None"));
        assert_eq!(
            std::mem::discriminant(&parsed),
            std::mem::discriminant(a),
            "Action::from_name({name:?}) returned the wrong variant"
        );
    }
}

/// Drift guard. `assign_mnemonics` gives each row its first A-Z char; on a
/// collision the later row falls through to its next free A-Z char.
/// Pinning the contract: Copy=C, Close Pane=l (C taken),
/// Cancel=a (C taken).
#[test]
fn mnemonics_assign_unique_chars_with_fallback() {
    let menu = vec![
        item("Copy", true),       // 'C'
        item("Close Pane", true), // 'C' taken → 'l'
        item("Cancel", true),     // 'C' + 'l' taken → 'a'
        ContextMenuItem::Separator,
        item("Theme", true), // 'T'
        item("Tab", true),   // 'T' taken → 'a' taken → 'b'
        item("12345", true), // no A-Z → None
    ];
    let mn = assign_mnemonics(&menu, &[]);
    assert_eq!(mn[0], Some((0, 'c')));
    // "Close Pane": C taken, so next alphabetic is 'l' at byte 1.
    assert_eq!(mn[1], Some((1, 'l')));
    // "Cancel": C + l taken, so next is 'a' at byte 1.
    assert_eq!(mn[2], Some((1, 'a')));
    // Separator: no label, no mnemonic.
    assert_eq!(mn[3], None);
    // "Theme": T at byte 0.
    assert_eq!(mn[4], Some((0, 't')));
    // "Tab": T taken, a taken, so 'b' at byte 2.
    assert_eq!(mn[5], Some((2, 'b')));
    // "12345": no A-Z, None.
    assert_eq!(mn[6], None);
}

/// The URL-aware leading rows claim their mnemonics AFTER the stable core
/// rows. Without the two-round pass, "Open Link" / "Copy Link Address"
/// leading the menu would steal 'o'/'c', silently remapping muscle-memory
/// mnemonics whenever the right-click lands on a link ('p' would fire Copy
/// instead of Paste).
#[test]
fn mnemonics_url_rows_claim_letters_last() {
    let url = |label: &'static str, how: super::UrlHow| ContextMenuItem::UrlItem {
        label,
        url: "https://example.com".into(),
        how,
        pane: None,
    };
    let menu = vec![
        url("Open Link", super::UrlHow::Open),
        url("Copy Link Address", super::UrlHow::Copy),
        ContextMenuItem::Separator,
        item("Copy", true),
        item("Paste", true),
    ];
    let mn = assign_mnemonics(&menu, &[]);
    // Core rows keep their stable letters…
    assert_eq!(mn[3], Some((0, 'c'))); // Copy = c (not stolen)
    assert_eq!(mn[4], Some((0, 'p'))); // Paste = p (not stolen)
    // …and the URL rows pick from what's left.
    assert_eq!(mn[0], Some((0, 'o'))); // Open Link = o (free)
    // "Copy Link Address": c, o, p taken → 'y' (byte 3).
    assert_eq!(mn[1], Some((3, 'y')));
    assert_eq!(mn[2], None); // separator
}

/// `vim-menu-nav` drift guard: while the setting is on, the vim navigation
/// letters must never be assigned as mnemonics. The nav layer intercepts
/// them first, so a row keyed on one would silently lose its hotkey. Rows
/// fall through to their next free letter instead.
#[test]
fn vim_nav_letters_are_excluded_from_mnemonics() {
    let menu = vec![
        item("Jump", true),      // 'j' reserved → 'u'
        item("Kill Pane", true), // 'k' reserved → 'i'
        item("Hold", true),      // 'h' reserved → 'o'
        item("List", true),      // 'l' reserved → 'i' taken → 's'
        item("Go", true),        // 'g' reserved → 'o' taken → None
    ];
    let mn = assign_mnemonics(&menu, super::VIM_NAV_RESERVED);
    for (i, slot) in mn.iter().enumerate() {
        if let Some((_, c)) = slot {
            assert!(
                !super::VIM_NAV_RESERVED.contains(c),
                "row {i} was assigned reserved nav letter {c:?}"
            );
        }
    }
    assert_eq!(mn[0], Some((1, 'u'))); // Jump → 'u'
    assert_eq!(mn[1], Some((1, 'i'))); // Kill Pane → 'i'
    assert_eq!(mn[2], Some((1, 'o'))); // Hold → 'o'
    assert_eq!(mn[3], Some((2, 's'))); // List → 's'
    // "Go": both letters reserved/taken → no mnemonic at all.
    assert_eq!(mn[4], None);
    // And with the reservation off, first letters win as before.
    let plain = assign_mnemonics(&menu, &[]);
    assert_eq!(plain[0], Some((0, 'j')));
    assert_eq!(plain[1], Some((0, 'k')));
}

/// `vim-menu-nav`: `Ctrl+d`/`Ctrl+u` clamp at the list ends (no wrap, per
/// vim half-page semantics) and snap off separators in the direction of
/// travel.
#[test]
fn half_page_menu_target_clamps_and_snaps() {
    let menu = vec![
        item("A", true),            // 0
        item("B", true),            // 1
        ContextMenuItem::Separator, // 2
        item("C", true),            // 3
        item("D", true),            // 4
    ];
    // Down by 2 from row 0 lands on the separator → snaps DOWN to 3.
    assert_eq!(super::half_page_menu_target(&menu, 0, 2, 1), 3);
    // Down by 10 from row 0 clamps to the last row.
    assert_eq!(super::half_page_menu_target(&menu, 0, 10, 1), 4);
    // Up by 2 from row 4 lands on the separator → snaps UP to 1.
    assert_eq!(super::half_page_menu_target(&menu, 4, 2, -1), 1);
    // Up by 10 clamps to the first row.
    assert_eq!(super::half_page_menu_target(&menu, 4, 10, -1), 0);
    // Empty list: target stays put.
    assert_eq!(super::half_page_menu_target(&[], 0, 3, 1), 0);
}

/// Drift guard. Typeahead prefix-match is case-
/// insensitive and stops at the first dispatchable hit.
#[test]
fn typeahead_th_highlights_theme_first() {
    let menu = vec![
        item("Copy", true),
        item("Theme", true),
        item("Toggle Broadcast", true),
        item("Profile", true),
    ];
    // Single-char "t" hits Theme (first label starting with t).
    assert_eq!(typeahead_match(&menu, "t"), Some(1));
    // "th" still Theme.
    assert_eq!(typeahead_match(&menu, "th"), Some(1));
    // "to" → Toggle Broadcast.
    assert_eq!(typeahead_match(&menu, "to"), Some(2));
    // Uppercase = lowercase (case-insensitive).
    assert_eq!(typeahead_match(&menu, "TH"), Some(1));
    // No match.
    assert_eq!(typeahead_match(&menu, "xyz"), None);
    // Empty buffer.
    assert_eq!(typeahead_match(&menu, ""), None);
}

/// Drift guard. Disabled rows aren't typeahead targets
/// (would be confusing to highlight a row that can't dispatch).
#[test]
fn typeahead_skips_disabled_rows() {
    let menu = vec![item("Theme", false), item("Theme Plus", true)];
    // First Theme is disabled; second matches.
    assert_eq!(typeahead_match(&menu, "th"), Some(1));
}

/// Drift guard. `count_rows_fitting` walks rows from
/// `start` forward and sums heights until the next row would
/// exceed `panel_h`. Pinning the arithmetic so scroll math
/// can't silently drift if the row-height constants change.
#[test]
fn count_rows_fitting_respects_panel_height_and_separator_height() {
    let menu = vec![
        item("A", true),            // row_h=24
        item("B", true),            // row_h=24
        ContextMenuItem::Separator, // sep_h=8
        item("C", true),            // row_h=24
        item("D", true),            // row_h=24
        item("E", true),            // row_h=24
    ];
    let row_h = 24.0;
    let sep_h = 8.0;
    // panel_h = 0 -> nothing fits.
    assert_eq!(count_rows_fitting(&menu, 0, 0.0, row_h, sep_h), 0);
    // panel_h = 24 -> A only (B would push us to 48 > 24).
    assert_eq!(count_rows_fitting(&menu, 0, 24.0, row_h, sep_h), 1);
    // panel_h = 48 -> A + B.
    assert_eq!(count_rows_fitting(&menu, 0, 48.0, row_h, sep_h), 2);
    // panel_h = 56 -> A + B + separator (24 + 24 + 8 = 56).
    assert_eq!(count_rows_fitting(&menu, 0, 56.0, row_h, sep_h), 3);
    // panel_h = 1000 -> all 6 rows.
    assert_eq!(count_rows_fitting(&menu, 0, 1000.0, row_h, sep_h), 6);
    // Start past the separator: skip the first 3, fit C+D in 48.
    assert_eq!(count_rows_fitting(&menu, 3, 48.0, row_h, sep_h), 2);
    // Start at last row, plenty of height — just 1 fits.
    assert_eq!(count_rows_fitting(&menu, 5, 100.0, row_h, sep_h), 1);
    // Start past the end -> 0.
    assert_eq!(count_rows_fitting(&menu, 99, 1000.0, row_h, sep_h), 0);
    // The wheel clamp computes the earliest suffix once from the end rather
    // than rescanning every candidate offset.
    assert_eq!(context_menu_max_scroll_offset(&menu, 48.0, row_h, sep_h), 4);
    assert_eq!(context_menu_max_scroll_offset(&menu, 80.0, row_h, sep_h), 2);
    assert_eq!(
        context_menu_max_scroll_offset(&menu, 1000.0, row_h, sep_h),
        0
    );
    assert_eq!(
        context_menu_max_scroll_offset(&menu, 0.0, row_h, sep_h),
        menu.len() - 1
    );
    assert_eq!(context_menu_max_scroll_offset(&[], 48.0, row_h, sep_h), 0);
}

#[test]
fn context_menu_shrink_keeps_bottom_highlight_visible() {
    let menu: Vec<ContextMenuItem> = [
        "Row 0", "Row 1", "Row 2", "Row 3", "Row 4", "Row 5", "Row 6", "Row 7", "Row 8", "Row 9",
        "Row 10", "Row 11",
    ]
    .into_iter()
    .map(|label| item(label, true))
    .collect();
    let offset = context_menu_scroll_for_highlight(&menu, 0, 11, 72.0, 24.0, 8.0);
    assert_eq!(offset, 9);
    let visible = count_rows_fitting(&menu, offset, 72.0, 24.0, 8.0);
    assert!(
        (offset..offset + visible).contains(&11),
        "the row Enter will dispatch must remain in the visible slice"
    );
    assert_eq!(
        context_menu_scroll_for_highlight(&menu, usize::MAX, 0, 72.0, 24.0, 8.0),
        0,
        "a stale overscrolled offset is normalized when the top row is highlighted"
    );
}

/// Drift guard. With a 512-entry submenu and a real
/// surface-bound panel height of ~580px (surface_h=660 - 80px
/// chrome breathing room), `count_rows_fitting` reports a tiny
/// fraction of the total — the rest scrolls into view. The
/// "menu doesn't grow off-screen" invariant.
#[test]
fn theme_submenu_with_512_entries_clamps_panel_to_surface_height() {
    let big: Vec<ContextMenuItem> = (0..512)
        .map(|i| ContextMenuItem::Item {
            label: Box::leak(format!("Theme {i}").into_boxed_str()),
            action: Action::Paste,
            enabled: true,
        })
        .collect();
    let row_h = 24.0;
    let sep_h = 8.0;
    // 580px / 24px ≈ 24 rows visible.
    let visible = count_rows_fitting(&big, 0, 580.0, row_h, sep_h);
    assert!(
        (20..30).contains(&visible),
        "expected ~24 visible rows at 580px panel; got {visible}"
    );
    // All 512 rows shouldn't possibly fit in 580px.
    assert!(visible < big.len(), "panel must clamp; visible={visible}");
    let max_offset = context_menu_max_scroll_offset(&big, 580.0, row_h, sep_h);
    assert_eq!(
        max_offset,
        big.len() - visible,
        "the 512-row wheel clamp should be one reverse pass with no blank suffix"
    );
}

/// Drift guard. Hover-to-highlight walks `find_menu_row_y`
/// on every `CursorMoved`; pin the contract so a render-layout
/// change can't silently drop separator handling, off-by-one the
/// last row, or break the out-of-bounds contract.
#[test]
fn hover_updates_menu_highlight_skipping_separators() {
    // Menu layout: 4 rows with a separator between row 1 (Item)
    // and row 3 (Item):
    //   row 0 (Item)      [anchor + 0   .. anchor + row_h]
    //   row 1 (Item)      [anchor + 24  .. anchor + 48]
    //   row 2 (Separator) [anchor + 48  .. anchor + 56]
    //   row 3 (Item)      [anchor + 56  .. anchor + 80]
    let anchor_y = 100.0;
    let row_h = 24.0;
    let sep_h = 8.0;
    let kinds = [false, false, true, false];
    let panel_h = 80.0;
    // Cursor squarely inside row 0.
    assert_eq!(
        find_menu_row_y(anchor_y + 10.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(0)
    );
    // Cursor on the edge between row 0 and row 1: lands on row 1
    // (half-open interval [y, y+h)).
    assert_eq!(
        find_menu_row_y(anchor_y + row_h, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(1)
    );
    // Cursor inside row 1.
    assert_eq!(
        find_menu_row_y(anchor_y + 30.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(1)
    );
    // Cursor inside the separator — None (don't highlight a
    // visual-only gap).
    assert_eq!(
        find_menu_row_y(anchor_y + 50.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        None
    );
    // Cursor inside row 3 (after the separator).
    assert_eq!(
        find_menu_row_y(anchor_y + 60.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        Some(3)
    );
    // A clamped panel that leaves only 14 px after the separator does not
    // render the 24 px fourth row. That blank strip must remain inert.
    assert_eq!(
        find_menu_row_y(anchor_y + 60.0, anchor_y, 70.0, row_h, sep_h, kinds,),
        None
    );
    // Cursor above the panel.
    assert_eq!(
        find_menu_row_y(anchor_y - 1.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        None
    );
    // Cursor below the last row.
    assert_eq!(
        find_menu_row_y(anchor_y + 1000.0, anchor_y, panel_h, row_h, sep_h, kinds,),
        None
    );
    // Empty menu: always None.
    assert_eq!(
        find_menu_row_y(anchor_y + 10.0, anchor_y, panel_h, row_h, sep_h, []),
        None
    );
}

#[test]
fn clicks_share_the_fully_visible_hover_row_contract() {
    let src = production_source();
    let start = src
        .find("fn context_menu_click_action(")
        .expect("context-menu click resolver");
    let end = src[start..]
        .find("\n    /// `(items, anchor, panel_w, panel_h)`")
        .map(|offset| start + offset)
        .expect("end of context-menu click resolver");
    let resolver = &src[start..end];
    assert!(
        resolver.contains("let idx = self.menu_row_at_cursor(ws)?;"),
        "clicks must use the clipped-row helper exercised above"
    );
    assert!(
        !resolver.contains("for (idx, item)"),
        "a second row walk can drift from hover/render clipping"
    );
}

/// Drift guard. `rank_layouts` filters layout names by
/// every lower-cased query token; empty query returns identity.
#[test]
fn rank_layouts_filters_by_tokens_case_insensitive() {
    let layouts: Vec<String> = ["dev", "work", "dev-rust", "Notes", "ssh prod"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    // Empty query — identity, every layout shows.
    assert_eq!(rank_layouts("", &layouts), vec![0, 1, 2, 3, 4]);
    // Whitespace-only query — still identity (trim).
    assert_eq!(rank_layouts("   ", &layouts), vec![0, 1, 2, 3, 4]);
    // Single token, case-insensitive.
    assert_eq!(rank_layouts("dev", &layouts), vec![0, 2]);
    assert_eq!(rank_layouts("DEV", &layouts), vec![0, 2]);
    // Multi-token AND (every token must match).
    assert_eq!(rank_layouts("dev rust", &layouts), vec![2]);
    assert_eq!(rank_layouts("ssh prod", &layouts), vec![4]);
    // Lower-cased target matches mixed-case layouts.
    assert_eq!(rank_layouts("notes", &layouts), vec![3]);
    // No matches → empty (not a panic).
    assert_eq!(rank_layouts("xyz", &layouts), Vec::<usize>::new());
    // Empty layouts → empty (defensive).
    assert_eq!(rank_layouts("dev", &[]), Vec::<usize>::new());
}

#[test]
fn picker_candidates_are_vertical_rows_and_selection_scrolls_into_view() {
    let list = command_picker_list(
        "",
        20,
        &kettle_config::keybinds::defaults(),
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
        None,
    );
    let menu = picker_context_menu(list, (320.0, 160.0), (8.0, 16.0))
        .expect("a 320x160 surface fits a picker row above its input lane");

    assert!(
        menu.rows.len() > 20,
        "the projection must retain one row per ranked command instead of flattening matches"
    );
    assert_eq!(menu.highlight, 20);
    assert!(menu.scroll_offset > 0);
    let visible_rows =
        (menu.panel_h_clamped / (16.0 + kettle_render::menu::ROW_PAD)).floor() as usize;
    assert!(
        (menu.scroll_offset..menu.scroll_offset + visible_rows).contains(&menu.highlight),
        "the command Enter will run must remain inside the visible row window"
    );
    assert!(
        menu.rows.iter().any(|row| !row.hint.is_empty()),
        "command rows must carry live keybind hints in the menu hint column"
    );
    assert!(
        menu.anchor.1 + menu.panel_h_clamped <= 160.0 - (16.0 + 10.0),
        "the vertical list must stay above the bottom input lane"
    );
}

#[test]
fn layout_and_ssh_pickers_keep_ranked_candidates_on_separate_rows() {
    let layouts = ["dev", "dev-rust", "notes"]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let layout = layout_picker_list(
        "dev",
        1,
        &layouts,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(
        layout
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["dev", "dev-rust"]
    );
    assert_eq!(layout.selected, 1);

    let hosts = [
        ("staging-prod".to_string(), "dev@staging".to_string()),
        ("gpu-prod".to_string(), "dev@gpu".to_string()),
        ("gpu".to_string(), "dev@gpu-short".to_string()),
    ];
    assert_eq!(rank_ssh_hosts("gp", &hosts), vec![2, 1, 0]);
    let ssh = ssh_picker_list(
        "gp",
        1,
        &hosts,
        &kettle_i18n::Translator::new(kettle_i18n::Language::En),
    );
    assert_eq!(
        ssh.rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["gpu", "gpu-prod", "staging-prod"]
    );
    assert_eq!(ssh.selected, 1);
    assert_eq!(
        selected_ssh_target("gp", 1, &hosts).as_deref(),
        Some("dev@gpu")
    );
    assert_eq!(
        selected_ssh_target("someone@example.test", 0, &hosts).as_deref(),
        Some("someone@example.test"),
        "an unmatched query must retain direct user-at-host launches"
    );
    assert_eq!(selected_ssh_target("", 0, &[]), None);
    assert_eq!(
        selected_ssh_target("gp", usize::MAX, &hosts).as_deref(),
        Some("dev@staging"),
        "dispatch must clamp to the same last visible row after a host-list reload"
    );
}

#[test]
fn picker_overlay_projection_installs_the_vertical_context_menu() {
    let menu = picker_overlay_context_menu(
        [
            Some(command_picker_list(
                "",
                3,
                &kettle_config::keybinds::defaults(),
                &kettle_i18n::Translator::new(kettle_i18n::Language::En),
                None,
            )),
            None,
            None,
            None,
        ],
        (480.0, 240.0),
        (8.0, 16.0),
    )
    .expect("an active command picker must install the shared vertical panel");

    assert!(menu.rows.len() > 3);
    assert_eq!(menu.highlight, 3);
    assert!(menu.rows.iter().all(|row| !row.separator));

    // The theme picker's rows use the same panel.
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let themes = super::theme_picker_list("", 2, "TokyoNight Night", &tr);
    let menu = picker_overlay_context_menu(
        [None, Some(themes), None, None],
        (480.0, 240.0),
        (8.0, 16.0),
    )
    .expect("an active theme picker must install the shared vertical panel");
    assert_eq!(menu.rows.len(), kettle_config::Theme::list().len());
    assert_eq!(menu.highlight, 2);
}

#[test]
fn picker_scroll_offset_handles_tiny_and_invalid_geometry() {
    assert_eq!(picker_scroll_offset(12, 11, 72.0, 24.0), 9);
    assert_eq!(picker_scroll_offset(12, 0, 72.0, 24.0), 0);
    assert_eq!(picker_scroll_offset(0, 0, 72.0, 24.0), 0);
    assert_eq!(picker_scroll_offset(12, 11, f32::NAN, 24.0), 0);
    assert_eq!(picker_scroll_offset(12, 11, 72.0, 0.0), 0);
}

#[test]
fn cap_title_for_status_bar_truncates_at_char_budget_with_ellipsis() {
    // Drift guard. The status-bar strip is 1 cell tall;
    // a long title would wrap past the visible region without
    // this cap. Pins the contract:
    //   - under-budget: returned as-is, no `…`.
    //   - over-budget: truncated to `max` chars + `…`.
    //   - UTF-8 multibyte: char-count not byte-count (so a
    //     "🦀🦀🦀…" title isn't cut mid-character).
    use super::cap_title_for_status_bar;
    assert_eq!(cap_title_for_status_bar("short", 60), "short");
    assert_eq!(cap_title_for_status_bar("", 60), "");
    let exactly_60 = "x".repeat(60);
    assert_eq!(cap_title_for_status_bar(&exactly_60, 60), exactly_60);
    let long = "x".repeat(80);
    let capped = cap_title_for_status_bar(&long, 60);
    assert_eq!(capped.chars().count(), 61); // 60 + the `…`
    assert!(capped.ends_with('…'));
    // UTF-8 multibyte at the boundary — must split on a char
    // boundary, not a byte boundary.
    let crab_run = "🦀".repeat(80);
    let capped_crab = cap_title_for_status_bar(&crab_run, 60);
    assert_eq!(capped_crab.chars().count(), 61);
    // Every char before the `…` should still be a full crab.
    assert!(capped_crab.chars().take(60).all(|c| c == '🦀'));
}

/// The conhost startup-title detector that lets a stock Windows shell fall
/// back to a cwd label instead of showing its own exe path.
#[test]
fn conhost_startup_title_detected_real_titles_pass() {
    use super::is_conhost_startup_title;
    let pwsh = ["pwsh.exe".to_string()];
    // The injected full exe path for a WindowsApps-packaged pwsh.
    assert!(is_conhost_startup_title(
        "C:\\Program Files\\WindowsApps\\Microsoft.PowerShell_7.6.2.0_x64__8wekyb3d8bbwe\\pwsh.exe",
        &pwsh
    ));
    assert!(is_conhost_startup_title(
        "C:\\Windows\\System32\\cmd.exe",
        &["cmd.exe".to_string()]
    ));
    // Bare-name argv0 equality (no path in the title).
    assert!(is_conhost_startup_title("pwsh", &["pwsh".to_string()]));
    // Default shell (empty argv, argv0 unknown): any absolute .exe qualifies.
    assert!(is_conhost_startup_title(
        "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
        &[]
    ));
    // A REAL program-set title is NEVER suppressed.
    assert!(!is_conhost_startup_title("PS C:\\repos\\kettle>", &pwsh));
    assert!(!is_conhost_startup_title("vim README.md", &pwsh));
    // A different exe path the program legitimately set (leaf != argv0 leaf).
    assert!(!is_conhost_startup_title("C:\\tools\\fzf.exe", &pwsh));
    // Empty title, and a default-shell non-path title.
    assert!(!is_conhost_startup_title("", &pwsh));
    assert!(!is_conhost_startup_title("my session", &[]));
}

/// The per-pane
/// titlebar badges, composed by `compose_pane_title`.
#[test]
fn pane_title_badges_compose_from_state() {
    use super::{compose_pane_title, pane_title_parts};
    // No badges: title unchanged regardless of the configured badge text.
    assert_eq!(compose_pane_title("[agent] ", false, false, "bash"), "bash");
    // Agent attached: badge prefixed.
    assert_eq!(
        compose_pane_title("[agent] ", true, false, "bash"),
        "[agent] bash"
    );
    // Empty badge disables the agent prefix even when attached.
    assert_eq!(compose_pane_title("", true, false, "bash"), "bash");
    // Custom badge glyph.
    assert_eq!(compose_pane_title("🤖 ", true, false, "vim"), "🤖 vim");
    // Read-only: `[RO] ` prefixed.
    assert_eq!(
        compose_pane_title("[agent] ", false, true, "bash"),
        "[RO] bash"
    );
    // Both: read-only leads, then the agent badge.
    assert_eq!(
        compose_pane_title("[agent] ", true, true, "bash"),
        "[RO] [agent] bash"
    );
    // Read-only with an empty agent badge still shows `[RO]`.
    assert_eq!(compose_pane_title("", true, true, "bash"), "[RO] bash");

    let parts = pane_title_parts(
        "[agent] ",
        true,
        true,
        "..ine-server-go",
        false,
        Some("/home/u/Repos/SPI-1/flight-event-line-server-go"),
        Some("/home/u"),
    );
    assert_eq!(parts.prefix, "[RO] [agent] ");
    assert_eq!(parts.title, "flight-event-line-server-go");
    assert_eq!(
        parts.path.as_deref(),
        Some("~/Repos/SPI-1/flight-event-line-server-go")
    );
}

#[test]
fn malformed_config_notification_is_edge_triggered() {
    use super::should_notify_malformed;
    let none: Vec<String> = vec![];
    let a = vec!["cursor-style = beem".to_string()];
    let b = vec!["cursor-style = beem".to_string(), "bell = loud".to_string()];
    // Nothing malformed → never notify.
    assert!(!should_notify_malformed(&none, &none));
    assert!(!should_notify_malformed(&none, &a));
    // First appearance of a problem → notify.
    assert!(should_notify_malformed(&a, &none));
    // Same problem set on the next reload (e.g. a settings-persistence
    // write) → stay quiet.
    assert!(!should_notify_malformed(&a, &a));
    // A newly-introduced/changed problem → notify again.
    assert!(should_notify_malformed(&b, &a));
    // Fixing everything → no notification (and resets the latch).
    assert!(!should_notify_malformed(&none, &b));
}

/// A non-positive `scroll-multiplier` must never scroll BACKWARDS. The
/// config clamps to `[0.1, 50.0]` so this is unreachable in practice, but
/// the accumulator floors it at 0 for lines only. Detents still count, so
/// tab cycling and font zoom (which are deliberately multiplier-independent)
/// keep working.
#[test]
fn wheel_accum_ignores_negative_multiplier_for_lines_only() {
    use crate::input::WheelAccum;
    use winit::event::MouseScrollDelta;

    let steps = WheelAccum::default().feed(&MouseScrollDelta::LineDelta(0.0, 1.0), -5.0);
    assert_eq!(steps.lines, 0, "a bad multiplier must not invert scrolling");
    assert_eq!(
        steps.notches, 1,
        "detent-driven actions are multiplier-independent by design"
    );
}

/// Drift guard: pin the `copy_clipboard_decision`
/// policy. The four cases enumerate every (selection ×
/// smart_copy) combination so a future refactor that inverts
/// the smart_copy semantics or drops the empty-clobber case
/// fires this test before the regression ships.
#[test]
fn copy_clipboard_decision_smart_vs_clobber() {
    use super::copy_clipboard_decision;
    // Selection present + smart_copy = true: copy the selection.
    assert_eq!(
        copy_clipboard_decision(Some("hello".into()), true)
            .unwrap()
            .as_deref(),
        Some("hello")
    );
    // Selection present + smart_copy = false: still copy
    // (smart_copy only affects the no-selection branch).
    assert_eq!(
        copy_clipboard_decision(Some("hello".into()), false)
            .unwrap()
            .as_deref(),
        Some("hello")
    );
    // No selection + smart_copy = true (kettle default + Terminator
    // default): preserve existing clipboard — return None so the
    // caller skips the set_text call.
    assert_eq!(copy_clipboard_decision(None, true).unwrap(), None);
    // No selection + smart_copy = false: clobber clipboard with
    // empty string. Terminator's deliberate-UX-choice mode.
    assert_eq!(
        copy_clipboard_decision(None, false).unwrap().as_deref(),
        Some("")
    );
}

#[test]
fn putty_paste_source_honors_clipboard_toggle() {
    use super::{PasteSource, putty_paste_source};
    assert_eq!(putty_paste_source(false), PasteSource::Primary);
    assert_eq!(putty_paste_source(true), PasteSource::Clipboard);
}

#[test]
fn mouse_paste_routes_primary_and_putty_source() {
    let src = production_source();
    assert!(
        src.contains("if bcode == 1 && !self.cfg.disable_mouse_paste {\n                    self.paste_primary(ws);"),
        "middle-click paste must use paste_primary so X11 PRIMARY works"
    );
    assert!(
        src.contains("match putty_paste_source(self.cfg.putty_paste_style_source_clipboard)")
            && src.contains("PasteSource::Clipboard => self.paste_clipboard(ws)")
            && src.contains("PasteSource::Primary => self.paste_primary(ws)"),
        "PuTTY right-click paste must honor putty_paste_style_source_clipboard"
    );
}

/// Terminator's right-click menu, row for row, plus the rows Ghostty adds.
///
/// Set Window Title, Split Auto, and Zoom/Restore must be in the menu, not
/// only on keybinds, because Terminator presents them there and users reach
/// for the mouse to find them.
///
/// Building a real menu needs a live App, so the row list is pinned at the
/// source. Terminator's own set is `terminal_popup_menu.py`.
#[test]
fn the_context_menu_carries_every_terminator_row() {
    let src = production_source();
    let body = src
        .split("fn context_menu_items(")
        .nth(1)
        .and_then(|b| b.split("\n    /// ").next())
        .expect("context_menu_items body");
    use kettle_i18n::{Language, Text as T, Translator};
    let en = Translator::new(Language::En);
    for (key, row, action) in [
        (T::MenuCopy, "Copy", "Action::Copy"),
        (T::MenuPaste, "Paste", "Action::Paste"),
        (
            T::MenuSetWindowTitle,
            "Set Window Title…",
            "Action::EditWindowTitle",
        ),
        (T::MenuSplitAuto, "Split Auto", "Action::SplitAuto"),
        (T::MenuSplitRight, "Split Right", "Action::SplitRight"),
        (T::MenuSplitDown, "Split Down", "Action::SplitDown"),
        (T::MenuClosePane, "Close Pane", "Action::ClosePane"),
        (T::MenuNewTab, "New Tab", "Action::NewTab"),
        (T::MenuReadOnly, "Read only", "Action::TogglePaneReadOnly"),
        (T::MenuSetGroup, "Set Group…", "Action::CreateGroup"),
        // Rows Ghostty adds to Terminator's set.
        (T::MenuSplitLeft, "Split Left", "Action::SplitLeft"),
        (T::MenuSplitUp, "Split Up", "Action::SplitUp"),
        (T::MenuCloseTab, "Close Tab", "Action::CloseTab"),
        (T::MenuNewWindow, "New Window", "Action::NewWindow"),
        (T::MenuCloseWindow, "Close Window", "Action::CloseWindow"),
        (T::MenuSetTabTitle, "Set Tab Title…", "Action::EditTabTitle"),
        (
            T::MenuSetPaneTitle,
            "Set Pane Title…",
            "Action::EditPaneTitle",
        ),
        (T::MenuResetTerminal, "Reset Terminal", "Action::Reset"),
    ] {
        // The row reads as Terminator's in English.
        assert_eq!(en.text(key), row);
        // Checked as a PAIR, not as two independent tokens: "row present"
        // and "action present" both still hold if two rows swap actions,
        // which is exactly the bug that sends a click somewhere else.
        // Matching per menu ENTRY also stops a key that appears in a
        // nearby comment from standing in for the row itself.
        let needle = format!("T::{key:?})");
        let entry = body
            .split("ContextMenuItem::")
            .find(|chunk| {
                chunk
                    .split("label:")
                    .nth(1)
                    .and_then(|rest| rest.split("action:").next())
                    .is_some_and(|label| label.contains(&needle))
            })
            .unwrap_or_else(|| panic!("the context menu is missing Terminator's {row:?} row"));
        assert!(
            entry.contains(action),
            "{row:?} must dispatch {action} — the row and its action must \
                 stay together, found entry: {entry:?}"
        );
    }
    // Zoom swaps label with the state rather than sitting on one word,
    // matching Terminator's Zoom/Restore pair.
    assert_eq!(en.text(T::MenuZoom), "Zoom");
    assert_eq!(en.text(T::MenuRestore), "Restore");
    assert!(
        body.contains("if zoomed { T::MenuRestore } else { T::MenuZoom }")
            && body.contains("Action::ToggleZoom"),
        "the zoom row must show Restore while zoomed and Zoom otherwise"
    );
    // Splitting a zoomed pane has nothing to put the new pane beside, and
    // `filter_disabled` drops the rows rather than greying them out.
    assert_eq!(
        body.matches("enabled: !zoomed,").count(),
        5,
        "all five split rows must be gated on the zoom state"
    );
}

/// AppKit reads `ApplePersistence` when the application launches, and
/// building the event loop launches it. Registered any later, the
/// persistent-UI flush keeps running while idle.
#[test]
fn app_state_restoration_is_off_before_the_event_loop_exists() {
    let src = production_source();
    let run = src
        .split("pub fn run_with(mut startup: crate::Options) -> Result<()> {")
        .nth(1)
        .expect("run_with");
    let disable = run
        .find("disable_app_state_restoration();")
        .expect("run_with must turn off AppKit's persistent UI");
    let build = run
        .find("EventLoop::<UserEvent>::with_user_event().build()")
        .expect("run_with builds the event loop");
    assert!(
        disable < build,
        "ApplePersistence must be registered before the event loop exists"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn app_state_restoration_registers_apple_persistence_off() {
    use objc2_foundation::{NSRegistrationDomain, NSString, NSUserDefaults};

    super::disable_app_state_restoration();
    // SAFETY: plain Foundation reads on this thread's defaults object.
    let registered = unsafe {
        NSUserDefaults::standardUserDefaults().volatileDomainForName(NSRegistrationDomain)
    };
    let value = registered
        .get(&*NSString::from_str("ApplePersistence"))
        .expect("ApplePersistence is registered");
    // SAFETY: the registered value is the NSNumber this module created,
    // and `boolValue` takes no arguments.
    let on: bool = unsafe { objc2::msg_send![value, boolValue] };
    assert!(!on, "persistent UI must be off");
}

#[test]
fn context_menu_middle_click_dismisses_before_paste_or_tab_close() {
    let src = production_source();
    assert!(
        src.matches("if ws.context_menu.is_some() && bcode != 2")
            .count()
            >= 2,
        "GUI and ctl mouse paths must dismiss an open context menu on left/middle click; \
             only right-click may fall through to relocate it"
    );
    let gui_menu_gate = src
        .find("if ws.context_menu.is_some() && bcode != 2")
        .expect("GUI context-menu dismiss gate present");
    let middle_paste = src
        .find("if bcode == 1 && !self.cfg.disable_mouse_paste")
        .expect("middle paste path present");
    assert!(
        gui_menu_gate < middle_paste,
        "open context menu must preempt middle-click paste behind it"
    );
}

/// Drift guard: pin the `should_zoom_font` policy. The
/// in-wheel-handler call relies on this returning Some only when
/// Ctrl is held AND the user hasn't opted out via
/// `disable-mousewheel-zoom`. If a future refactor accidentally
/// inverts the disable check or drops the Ctrl gate, the test
/// fires before the regression ships.
#[test]
fn should_zoom_font_gates_on_ctrl_and_disable_flag() {
    use super::should_zoom_font;
    // Ctrl + scroll up + not disabled → +1 (grow font).
    assert_eq!(should_zoom_font(true, 3, false), Some(1));
    // Ctrl + scroll down + not disabled → -1 (shrink font).
    assert_eq!(should_zoom_font(true, -3, false), Some(-1));
    // Ctrl + scroll up + DISABLED → None (opt-out honored).
    assert_eq!(should_zoom_font(true, 3, true), None);
    // No Ctrl → None even when not disabled (regular scrollback
    // scroll path takes over).
    assert_eq!(should_zoom_font(false, 3, false), None);
    // No Ctrl AND disabled → still None.
    assert_eq!(should_zoom_font(false, 3, true), None);
    // Zero lines → None (a stale wheel event with no actual
    // motion shouldn't trigger a font change).
    assert_eq!(should_zoom_font(true, 0, false), None);
}

#[test]
fn selection_drag_requires_real_dpi_scaled_travel() {
    use super::selection_drag_motion_started;
    use winit::dpi::PhysicalPosition;

    let origin = PhysicalPosition::new(100.0, 200.0);
    assert!(!selection_drag_motion_started(None, origin, 1.0));
    assert!(!selection_drag_motion_started(Some(origin), origin, 1.0));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.99, 200.0),
        1.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        1.0,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(103.99, 200.0),
        2.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(104.0, 200.0),
        2.0,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(100.99, 200.0),
        0.5,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.0, 200.0),
        0.5,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.0, 201.0),
        1.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(101.5, 201.5),
        1.0,
    ));
    // Invalid scales use the conservative 1x fallback rather than making
    // every movement a drag or permanently disabling drag detection.
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        0.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        f64::NAN,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        -1.0,
    ));
    assert!(selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(102.0, 200.0),
        f64::INFINITY,
    ));
    assert!(!selection_drag_motion_started(
        Some(origin),
        PhysicalPosition::new(f64::NAN, 200.0),
        1.0,
    ));
    assert!(!selection_drag_motion_started(
        Some(PhysicalPosition::new(f64::INFINITY, 200.0)),
        origin,
        1.0,
    ));
}

#[test]
fn grid_selection_gestures_clear_the_search_editor_selection() {
    use super::{Mux, WindowState, arm_selection_gesture};
    use crate::search_input::SearchEditor;

    let mut ws = WindowState::new(1, Mux::new());
    ws.search.open = true;
    ws.search.editor = SearchEditor::from_text("query".into(), 100);
    for button in [0, 2] {
        ws.search.editor.select_all();
        assert_eq!(ws.search.editor.selected_text(), Some("query"));
        arm_selection_gesture(&mut ws, 42, button);
        assert_eq!(ws.search.editor.selected_text(), None);
        assert_eq!(ws.search.query(), "query");
    }
}

#[test]
fn selection_drag_state_measures_from_press_and_stays_latched() {
    use super::{
        Mux, WindowState, arm_selection_gesture, clear_selection_gesture,
        note_selection_drag_motion, selection_release_matches,
    };
    use winit::dpi::PhysicalPosition;

    let mut ws = WindowState::new(1, Mux::new());
    let origin = PhysicalPosition::new(100.0, 200.0);
    ws.cursor = origin;
    arm_selection_gesture(&mut ws, 42, 2);
    assert_eq!(ws.selecting_pane, Some(42));
    assert_eq!(ws.selection_button, Some(2));
    assert!(selection_release_matches(ws.selection_button, 2));
    assert!(!selection_release_matches(ws.selection_button, 0));

    note_selection_drag_motion(&mut ws);
    assert!(!ws.selection_dragged, "a duplicate native move stays inert");

    ws.cursor = PhysicalPosition::new(101.99, 200.0);
    note_selection_drag_motion(&mut ws);
    assert!(
        !ws.selection_dragged,
        "travel is measured from the press using the no-window 1x fallback"
    );

    ws.cursor = PhysicalPosition::new(102.0, 200.0);
    note_selection_drag_motion(&mut ws);
    assert!(ws.selection_dragged, "the exact threshold starts the drag");

    ws.cursor = origin;
    note_selection_drag_motion(&mut ws);
    assert!(
        ws.selection_dragged,
        "once real travel starts a drag, returning to the origin cannot disarm it"
    );

    clear_selection_gesture(&mut ws);
    assert!(!ws.selecting);
    assert!(!ws.selection_dragged);
    assert_eq!(ws.selection_drag_origin, None);
    assert_eq!(ws.selection_button, None);
    assert!(!selection_release_matches(ws.selection_button, 2));
}

#[test]
fn selection_copy_requires_the_gesture_owner() {
    use super::selection_copy_owner;

    assert_eq!(selection_copy_owner(true, Some(7)), Some(7));
    assert_eq!(selection_copy_owner(true, None), None);
    assert_eq!(selection_copy_owner(false, Some(7)), None);
}

#[test]
fn native_and_control_selection_motion_share_the_drag_threshold() {
    let src = production_source();
    let control = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("ctl mouse move body");
    assert!(control.contains("note_selection_drag_motion(ws);"));

    let native = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native cursor-moved arm");
    assert!(native.contains("note_selection_drag_motion(ws);"));
    let cursor_at = native
        .find("ws.cursor = position;")
        .expect("native motion stores the delivered position");
    let motion_at = native
        .find("note_selection_drag_motion(ws);")
        .expect("native motion applies the drag threshold");
    assert!(
        cursor_at < motion_at,
        "native motion must store the new cursor before measuring from the press origin"
    );
    assert!(
        !src.contains("ws.selection_dragged = true;"),
        "a raw move event must not bypass the travel threshold"
    );

    let cursor_left = src
        .split("WindowEvent::CursorLeft { .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native cursor-left arm");
    let cursor_left = cursor_left.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        cursor_left.contains("selection_leave_edge(")
            && cursor_left.contains("ws.selection_dragged"),
        "leaving the client area must not bypass measured drag travel"
    );

    let clear = src
        .split("fn clear_selection_gesture(")
        .nth(1)
        .and_then(|body| body.split("\n}\n").next())
        .expect("selection gesture reset");
    assert!(clear.contains("ws.selection_drag_origin = None;"));
    assert!(clear.contains("ws.selection_button = None;"));
    assert_eq!(
        src.matches("arm_selection_gesture(ws, pane_id,").count(),
        2,
        "fresh and extended selections must share origin and button ownership"
    );

    let close_modals = src
        .split("fn close_all_modals(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("shared modal transition");
    assert!(
        close_modals.contains("self.finish_selection_gesture(ws);"),
        "every modal transition must commit copy-on-select before ending pointer ownership"
    );
    assert!(
        close_modals.contains("ws.theme_picker_input = None;"),
        "opening another modal must close the theme picker, which reverts its preview"
    );

    let finish_selection = src
        .split("fn finish_selection_gesture(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("shared selection completion transition");
    let copy_at = finish_selection
        .find("self.copy_selection(ws);")
        .expect("copy-on-select is committed");
    let owner_at = finish_selection
        .find("selection_copy_owner(ws.selecting, ws.selecting_pane).is_some()")
        .expect("copy-on-select requires an explicit gesture owner");
    let clear_at = finish_selection
        .find("clear_selection_gesture(ws);")
        .expect("selection gesture is cleared");
    assert!(
        owner_at < copy_at && copy_at < clear_at,
        "copying requires selecting_pane and must precede the clear"
    );

    let install_confirm = src
        .split("fn install_confirm_dialog(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("shared confirmation transition");
    let finish_at = install_confirm
        .find("self.finish_selection_gesture(ws);")
        .expect("confirmation completes terminal selection ownership");
    let install_at = install_confirm
        .find("ws.confirm_dialog = Some(dialog);")
        .expect("confirmation stores the modal");
    assert!(
        finish_at < install_at,
        "selection ownership must end before a confirmation becomes visible"
    );
    let normalized_src = src.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        normalized_src
            .matches("self.install_confirm_dialog(")
            .count(),
        2,
        "every confirmation path (closing and keybind conflicts) must use the shared transition"
    );
    assert_eq!(
        normalized_src.matches("ws.confirm_dialog = Some(").count(),
        1,
        "confirmations must not bypass the shared pointer-ownership transition"
    );

    let show_context_menu = src
        .split("fn show_context_menu(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("shared context-menu transition");
    assert!(
        show_context_menu.contains("self.finish_selection_gesture(ws);"),
        "a context menu must commit copy-on-select before taking the pointer"
    );

    let update_selection = src
        .split("fn update_selection(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("selection update body");
    assert_eq!(
        update_selection
            .matches("self.finish_selection_gesture(ws);")
            .count(),
        3,
        "every selection-owner invalidation must commit copy-on-select before clearing"
    );
    assert!(
        !update_selection.contains("clear_selection_gesture(ws);"),
        "selection invalidation must not bypass copy-on-select"
    );

    let redraw = src
        .split("fn redraw(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("redraw body");
    assert!(
        redraw.contains("self.finish_selection_gesture(ws);")
            && !redraw.contains("clear_selection_gesture(ws);"),
        "frame-time pane invalidation must preserve copy-on-select"
    );

    let control_move = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("control mouse move body");
    assert!(
        control_move.contains("ws.selection_autoscroll_edge = 0;"),
        "a control move inside the client must clear a stale native edge latch"
    );

    let control_release = src
        .split("fn ctl_mouse_release(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("control mouse release body");
    let native_release = src
        .split("state: ElementState::Released,")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native mouse release arm");
    for (name, body) in [("control", control_release), ("native", native_release)] {
        assert!(
            body.contains("selection_release_matches(ws.selection_button, bcode)")
                && body.contains("self.finish_selection_gesture(ws);"),
            "{name} release must match the owning button and commit copy-on-select before clearing it"
        );
    }
}

#[test]
fn selection_autoscroll_rate_scales_with_overshoot() {
    use super::selection_autoscroll_lines;
    let dragged = |y| selection_autoscroll_lines(y, 100.0, 200.0, 1.0, true);
    // Outside the six-point edge zones → no scroll.
    assert_eq!(dragged(150.0), 0);
    assert_eq!(dragged(107.0), 0);
    assert_eq!(dragged(193.0), 0);
    assert_eq!(dragged(100.0), 1);
    assert_eq!(dragged(200.0), -1);
    // Just past the top → 1 line/frame up into history (positive).
    assert_eq!(dragged(95.0), 1);
    // Moderate overshoot (10..40 px) → 2 lines/frame.
    assert_eq!(dragged(80.0), 2);
    // Big overshoot (≥40 px) → 3 lines/frame.
    assert_eq!(dragged(50.0), 3);
    // Past the bottom → negative (toward the present).
    assert_eq!(dragged(205.0), -1);
    assert_eq!(dragged(220.0), -2);
    assert_eq!(dragged(280.0), -3);
    // Holding on the first or last few pixels inside a full-height pane
    // must keep the selection moving; there may be no client area beyond
    // the bottom edge for the pointer to enter.
    assert_eq!(dragged(102.0), 1);
    assert_eq!(dragged(198.0), -1);
    // Every path is drag-only, including a coordinate just outside the
    // pane. A press on the boundary plus sub-threshold outward jitter must
    // not bypass the measured-travel gate.
    assert_eq!(
        selection_autoscroll_lines(100.0, 100.0, 200.0, 1.0, false),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(95.0, 100.0, 200.0, 1.0, false),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(205.0, 100.0, 200.0, 1.0, false),
        0
    );
    // The zone is specified in logical points and capped to a quarter of a
    // tiny pane so its top and bottom regions cannot overlap.
    assert_eq!(
        selection_autoscroll_lines(111.0, 100.0, 200.0, 2.0, true),
        1
    );
    assert_eq!(
        selection_autoscroll_lines(113.0, 100.0, 200.0, 2.0, true),
        0
    );
    assert_eq!(selection_autoscroll_lines(1.0, 0.0, 8.0, 1.0, true), 1);
    assert_eq!(selection_autoscroll_lines(4.0, 0.0, 8.0, 1.0, true), 0);
    assert_eq!(selection_autoscroll_lines(7.0, 0.0, 8.0, 1.0, true), -1);
    for invalid_scale in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(
            selection_autoscroll_lines(105.0, 100.0, 200.0, invalid_scale, true),
            1,
            "invalid display scales use the 1x edge zone"
        );
        assert_eq!(
            selection_autoscroll_lines(195.0, 100.0, 200.0, invalid_scale, true),
            -1,
            "invalid display scales use the 1x edge zone"
        );
    }
    // A transient empty layout must not invent an edge direction.
    assert_eq!(
        selection_autoscroll_lines(100.0, 100.0, 100.0, 1.0, true),
        0
    );
    assert_eq!(
        selection_autoscroll_lines(100.0, 200.0, 100.0, 1.0, true),
        0
    );
}

#[test]
fn selection_leave_latches_only_the_nearest_vertical_edge() {
    use super::selection_leave_edge;

    // Leaving the window is not itself proof of a drag. A press on the
    // last pixel followed by sub-threshold outward jitter must stay inert.
    assert_eq!(selection_leave_edge(false, 400.0, 1.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(false, 400.0, 599.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 400.0, 1.0, 800.0, 600.0), 1);
    assert_eq!(selection_leave_edge(true, 400.0, 599.0, 800.0, 600.0), -1);
    assert_eq!(selection_leave_edge(true, 1.0, 300.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 799.0, 300.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 1.0, 1.0, 800.0, 600.0), 1);
    assert_eq!(selection_leave_edge(true, 799.0, 599.0, 800.0, 600.0), -1);
    assert_eq!(selection_leave_edge(true, f32::NAN, 1.0, 800.0, 600.0), 0);
    assert_eq!(selection_leave_edge(true, 1.0, 1.0, 0.0, 600.0), 0);
}

#[test]
fn wheel_target_follows_hover_without_changing_focus_and_falls_back() {
    use super::wheel_target_at;

    let layout = [
        (10, (0.0, 0.0, 400.0, 600.0)),
        (20, (400.0, 0.0, 400.0, 600.0)),
    ];
    assert_eq!(
        wheel_target_at(&layout, Some(10), 650.0, 300.0),
        Some(layout[1]),
        "the non-focused pane under the pointer must own the wheel"
    );
    assert_eq!(
        wheel_target_at(&layout, Some(10), 900.0, 300.0),
        Some(layout[0]),
        "outside pane geometry must preserve focused-pane behavior"
    );
    assert_eq!(wheel_target_at(&layout, None, 900.0, 300.0), None);
}

#[test]
fn pane_titlebar_hit_geometry() {
    use super::pane_titlebar_hit;
    // 800x600 surface with 2 panes side-by-side, 24px titlebar.
    // Left pane:  (0, 0, 400, 600)
    // Right pane: (400, 0, 400, 600)
    let rects = vec![
        (1u64, (0.0_f32, 0.0_f32, 400.0_f32, 600.0_f32)),
        (2u64, (400.0_f32, 0.0_f32, 400.0_f32, 600.0_f32)),
    ];
    // Top titlebar: y-band [1, 25).
    // Click inside left pane's titlebar:
    assert_eq!(pane_titlebar_hit(50.0, 12.0, &rects, false, 24.0), Some(1));
    // Click inside right pane's titlebar:
    assert_eq!(pane_titlebar_hit(500.0, 12.0, &rects, false, 24.0), Some(2));
    // Click below the titlebar (in cell content) → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 100.0, &rects, false, 24.0), None);
    // Click ABOVE the bar (y < 1) → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 0.5, &rects, false, 24.0), None);
    // Bottom titlebar: y-band [576, 600).
    assert_eq!(pane_titlebar_hit(50.0, 580.0, &rects, true, 24.0), Some(1));
    assert_eq!(pane_titlebar_hit(500.0, 580.0, &rects, true, 24.0), Some(2));
    // Click at top-of-pane with title_at_bottom=true → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 12.0, &rects, true, 24.0), None);
    // Click in cell content with title_at_bottom=true → no hit.
    assert_eq!(pane_titlebar_hit(50.0, 300.0, &rects, true, 24.0), None);
}

#[test]
fn gpu_init_watchdog_fires_only_on_a_real_hang() {
    use super::gpu_init_timed_out;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    // Drift guard. `done` never set within the window → reports a
    // hang (true), so the real watchdog would log + exit.
    let stuck = AtomicBool::new(false);
    assert!(gpu_init_timed_out(
        &stuck,
        Duration::from_millis(60),
        Duration::from_millis(10)
    ));
    // `done` already set (init finished) → stands down immediately (false),
    // even with a long nominal timeout, so a fast init/failure is never
    // killed.
    let finished = AtomicBool::new(true);
    assert!(!gpu_init_timed_out(
        &finished,
        Duration::from_secs(30),
        Duration::from_millis(10)
    ));
}

#[test]
fn viewport_point_to_grid_applies_display_offset() {
    use super::viewport_point_to_grid;
    use kettle_core::{Column, Line, Point};
    // At the bottom (display_offset 0): viewport == grid-absolute.
    let p = viewport_point_to_grid(Point::new(Line(5), Column(3)), 0);
    assert_eq!(p.line, Line(5));
    assert_eq!(p.column, Column(3));
    // Scrolled back by 3: absolute = viewport − offset. Without the
    // subtraction, a scrolled selection reads the wrong row.
    assert_eq!(
        viewport_point_to_grid(Point::new(Line(5), Column(0)), 3).line,
        Line(2)
    );
    // Scrolled far enough that the visible row maps into history (negative
    // absolute line — alacritty's grid indexes history with negative lines).
    assert_eq!(
        viewport_point_to_grid(Point::new(Line(2), Column(0)), 10).line,
        Line(-8)
    );
}

#[test]
fn output_paint_coalesces_within_frame_budget() {
    use super::{OUTPUT_FRAME_BUDGET, should_defer_output_paint};
    use std::time::{Duration, Instant};
    let now = Instant::now();
    // No prior paint → paint immediately (never defer the first frame).
    assert!(!should_defer_output_paint(now, None, OUTPUT_FRAME_BUDGET));
    // Painted just now → defer so a same-burst wakeup coalesces.
    assert!(should_defer_output_paint(
        now,
        Some(now),
        OUTPUT_FRAME_BUDGET
    ));
    // A frame's worth later → the budget elapsed, paint immediately.
    let later = now + OUTPUT_FRAME_BUDGET + Duration::from_millis(5);
    assert!(!should_defer_output_paint(
        later,
        Some(now),
        OUTPUT_FRAME_BUDGET
    ));
}

/// The output-paint budget must GROW under a sustained flood so fewer
/// per-frame snapshots take the `Term` lock the PTY reader needs. A brief
/// burst must stay at the responsive monitor-rate budget so keystroke echo
/// and short bursts don't get throttled.
#[test]
fn effective_output_budget_grows_under_sustained_flood() {
    use super::{
        DEEP_FLOOD_MAX_BUDGET, MID_FLOOD_MAX_BUDGET, OUTPUT_FRAME_BUDGET, effective_output_budget,
        output_frame_budget_for_refresh,
    };
    use std::time::Duration;
    // A brief burst stays at the window's responsive monitor rate.
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 0),
        OUTPUT_FRAME_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 3),
        OUTPUT_FRAME_BUDGET
    );
    // A 60 Hz window retains the established ~30 fps, then ~20 fps
    // sustained-flood backoff.
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 4),
        MID_FLOOD_MAX_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 15),
        MID_FLOOD_MAX_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 16),
        DEEP_FLOOD_MAX_BUDGET
    );
    assert_eq!(
        effective_output_budget(OUTPUT_FRAME_BUDGET, 10_000),
        DEEP_FLOOD_MAX_BUDGET
    );
    // A 165 Hz window's budget sits one small dispatch lead under its
    // 6.06 ms period. Under sustained output it stays bounded at 60/30 fps
    // instead of spending unbounded CPU repainting unreadable flood frames
    // at 82/55 fps.
    let high_refresh = output_frame_budget_for_refresh(Some(165_000));
    assert_eq!(high_refresh, Duration::from_nanos(5_810_607));
    assert_eq!(
        effective_output_budget(high_refresh, 4),
        Duration::from_nanos(16_666_667)
    );
    assert_eq!(
        effective_output_budget(high_refresh, 16),
        Duration::from_nanos(33_333_334)
    );
    // Monotonic non-decreasing: deeper flood never paints MORE often.
    let mut prev = Duration::ZERO;
    for n in 0..40u32 {
        let b = effective_output_budget(high_refresh, n);
        assert!(b >= prev, "budget must not shrink as flood deepens");
        prev = b;
    }
    // The throttle is bounded (never starves the settled frame indefinitely).
    assert!(effective_output_budget(OUTPUT_FRAME_BUDGET, u32::MAX) <= Duration::from_millis(50));
}

#[test]
fn output_frame_budget_tracks_monitor_refresh_with_safe_bounds() {
    use super::{OUTPUT_FRAME_BUDGET, output_frame_budget_for_refresh};
    use std::time::Duration;

    assert_eq!(output_frame_budget_for_refresh(None), OUTPUT_FRAME_BUDGET);
    assert_eq!(
        output_frame_budget_for_refresh(Some(0)),
        OUTPUT_FRAME_BUDGET
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(60_000)),
        Duration::from_nanos(16_416_667)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(120_000)),
        Duration::from_nanos(8_083_334)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(165_000)),
        Duration::from_nanos(5_810_607)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(240_000)),
        Duration::from_millis(4)
    );
    // Driver outliers cannot create a busy loop or a sluggish >33 ms
    // output deadline.
    assert_eq!(
        output_frame_budget_for_refresh(Some(500_000)),
        Duration::from_millis(4)
    );
    assert_eq!(
        output_frame_budget_for_refresh(Some(20_000)),
        Duration::from_nanos(33_333_334)
    );
}

#[test]
fn monitor_refresh_probe_is_rate_limited_with_a_trailing_deadline() {
    use super::{MONITOR_REFRESH_PROBE_INTERVAL, monitor_refresh_probe_wait};
    use std::time::{Duration, Instant};

    let start = Instant::now();
    assert_eq!(monitor_refresh_probe_wait(None, start, false), None);
    assert_eq!(
        monitor_refresh_probe_wait(Some(start), start + Duration::from_millis(1), false),
        Some(MONITOR_REFRESH_PROBE_INTERVAL - Duration::from_millis(1))
    );
    assert_eq!(
        monitor_refresh_probe_wait(Some(start), start + MONITOR_REFRESH_PROBE_INTERVAL, false),
        None,
        "the trailing probe becomes due even if a move storm stopped"
    );
    assert_eq!(
        monitor_refresh_probe_wait(Some(start), start, true),
        None,
        "creation and DPI events force an immediate refresh probe"
    );
}

/// Behavioral pin for the deferred -> queued -> presenting transaction.
#[test]
fn output_coalescer_retains_flood_signal_without_busy_waiting() {
    let mut pacer = crate::window_state::OutputPaintPacer::default();
    pacer.defer();
    assert!(pacer.is_deferred());
    assert!(!pacer.redraw_queued());
    assert!(pacer.queue_deferred_redraw());
    assert!(pacer.redraw_queued());
    assert!(
        !pacer.queue_deferred_redraw(),
        "one due deadline queues at most one redraw"
    );
    assert!(
        pacer.begin_frame(),
        "queued output retains the flood signal"
    );
    pacer.presentation_failed();
    assert!(pacer.is_deferred());
    assert!(!pacer.redraw_queued());
    assert!(pacer.queue_deferred_redraw());
    assert!(pacer.begin_frame());
    pacer.presented();
    assert!(!pacer.is_deferred());

    // Keep a small source pin only for sub-millisecond deadline precision;
    // lifecycle behavior above is exercised through the real state machine.
    let src = production_source();
    let start = src
        .find("fn about_to_wait_inner(")
        .expect("about_to_wait_inner present");
    let rest = &src[start..];
    // Bound the scan to the about_to_wait_inner body (it is the last method
    // in the impl, so the next column-0 `}` closes the impl).
    let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("remaining.max(std::time::Duration::from_nanos(1))")
            && !body.contains("remaining.as_millis()"),
        "fractional monitor deadlines must reach ControlFlow without millisecond truncation"
    );
}

#[test]
fn output_frame_transition_follows_every_renderability_guard() {
    let source = production_source();
    let redraw_start = source.find("fn redraw(").expect("redraw present");
    let redraw_tail = &source[redraw_start..];
    let redraw_end = redraw_tail
        .find("\n    /// Compose the status-bar")
        .expect("redraw end");
    let redraw = &redraw_tail[..redraw_end];
    let renderability_guard = redraw
        .find("if !ws.dpi_resize.render_allowed()")
        .expect("DPI/recovery/visibility guard present");
    let screenshot_policy = redraw
        .find("let screenshot_may_render_offscreen")
        .expect("offscreen screenshot policy present");
    let renderer_guard = redraw
        .find("if ws.renderer.is_none()")
        .expect("renderer-availability guard present");
    let frame_transition = redraw
        .find("let was_coalescing_paint = ws.output_pacer.begin_frame();")
        .expect("output frame transition present");
    let combined_guard = &redraw[screenshot_policy..renderer_guard];

    assert!(
        combined_guard.contains("ws.frame_recovery.has_pending()")
            && combined_guard.contains("ws.frame_recovery.renderer_rebuild_pending()")
            && combined_guard.contains("!screenshot_may_render_offscreen")
            && combined_guard.contains("window_is_render_hidden(ws)")
            && renderability_guard < frame_transition
            && renderer_guard < frame_transition,
        "only an explicit offscreen capture may bypass transient surface recovery; renderer \
             rebuilds and ordinary hidden output must stay blocked before Presenting"
    );
}

/// Idle blink must wake at the configured half-period deadline, not at a
/// fixed sub-interval. A 120 ms poll would request four mostly-no-op
/// redraws before each default 530 ms blink toggle.
#[test]
fn cursor_blink_waits_until_the_actual_deadline() {
    let src = production_source();
    let start = src
        .find("fn about_to_wait_inner(")
        .expect("about_to_wait_inner present");
    let rest = &src[start..];
    let end = rest.find("\n}\n").map(|i| i + 2).unwrap_or(rest.len());
    let body = &rest[..end];
    assert!(
        body.contains("next_cursor_blink_phase(")
            && body.contains("let blink_due = next_blink.is_some()"),
        "about_to_wait must request a blink redraw only when the half-period elapsed"
    );
    let blink = body
        .find("let next_blink =")
        .expect("blink phase scheduling must exist");
    let blink_body = &body[blink..];
    assert!(
        blink_body.find("ws.last_blink = now") < blink_body.find("w.request_redraw()"),
        "advance the blink deadline before requesting a compositor frame so repeated \
             about_to_wait callbacks cannot enqueue the same blink"
    );
    assert!(
        body.contains("blink_interval.saturating_sub(blink_elapsed)")
            && !body.contains("Some(120)"),
        "blink wait time must be the remaining configured interval, not a fixed poll"
    );
}

/// A titlebar the layout carves off the top moves row zero down by its
/// height for the pointer and the grid alike; one at the bottom leaves row
/// zero at the padded pane origin. Both follow from the pane's partition,
/// whose terminal the pointer maps into and the PTY is sized from.
#[test]
fn titlebar_inset_realigns_hit_test_and_grid() {
    use super::{grid_dims_px, px_to_cell};
    use crate::pane_partition::{Metrics, partition_leaf};
    let rect = (10.0, 20.0, 800.0, 600.0);
    let (cw, ch, pad) = (8.0_f32, 16.0_f32, 4.0_f32);
    let tb = ch + 6.0; // the titlebar height the window's style uses
    let terminal = |titlebar: f32, at_bottom: bool| {
        let metrics = Metrics::new((cw, ch), (pad, pad), titlebar, at_bottom).unwrap();
        partition_leaf(rect, metrics, None).terminal
    };
    let bare = terminal(0.0, false);
    let top = terminal(tb, false);
    let bottom = terminal(tb, true);

    // No titlebar (single-pane tab): content origin = ry + pad = 24.
    let (_, line0, _) = px_to_cell(100.0, 24.0, bare, (cw, ch), (pad, pad));
    assert_eq!(line0, 0, "py at content origin → row 0 (no titlebar)");

    // With a titlebar: content origin shifts down by `tb` to 24 + 22 = 46.
    let (_, line_origin, _) = px_to_cell(100.0, 24.0 + tb, top, (cw, ch), (pad, pad));
    assert_eq!(
        line_origin, 0,
        "py at the titlebar'd content origin → row 0"
    );
    // One cell below the titlebar'd origin → row 1. Ignoring the titlebar
    // inset would give row 2, an off-by-one.
    let (_, line1, _) = px_to_cell(100.0, 24.0 + tb + ch, top, (cw, ch), (pad, pad));
    assert_eq!(line1, 1);
    // A click up in the titlebar band clamps to row 0 (never negative).
    let (_, clamped, _) = px_to_cell(100.0, 24.0, top, (cw, ch), (pad, pad));
    assert_eq!(clamped, 0);

    // Moving the title to the bottom moves row zero back to the ordinary
    // padded pane origin. Pointer projection must follow the rendered grid,
    // not retain the old top-title inset.
    let (_, bottom_line0, _) = px_to_cell(100.0, 24.0, bottom, (cw, ch), (pad, pad));
    assert_eq!(bottom_line0, 0);
    let (_, bottom_line1, _) = px_to_cell(100.0, 24.0 + ch, bottom, (cw, ch), (pad, pad));
    assert_eq!(bottom_line1, 1);

    // The titlebar steals height, so fewer rows fit in its terminal.
    let (cols_no, rows_no) = grid_dims_px((bare.2, bare.3), (cw, ch), (pad, pad));
    let (cols_tb, rows_tb) = grid_dims_px((top.2, top.3), (cw, ch), (pad, pad));
    assert_eq!(cols_no, cols_tb, "titlebar doesn't change column count");
    assert!(
        rows_tb < rows_no,
        "titlebar must reduce reported rows ({rows_tb} < {rows_no})"
    );
}

#[test]
fn fractional_cell_geometry_rounds_after_the_full_grid() {
    use super::{pty_pixel_extent, pty_pixel_size};

    assert_eq!(pty_pixel_extent(9.6, 100), 960);
    assert_eq!(pty_pixel_extent(19.2, 40), 768);
    assert_eq!(pty_pixel_size((9.6, 19.2), (100, 40)), (960, 768));

    // Both cell metrics round to the same integer cell width, but their
    // complete PTY geometry remains distinguishable. This is the
    // pixel-only resize a fractional monitor transition must not miss.
    assert_eq!(9.1_f32.round() as u16, 9.4_f32.round() as u16);
    assert_eq!(pty_pixel_extent(9.1, 100), 910);
    assert_eq!(pty_pixel_extent(9.4, 100), 940);

    assert_eq!(pty_pixel_extent(f32::NAN, 80), 80);
    assert_eq!(pty_pixel_extent(f32::INFINITY, 80), 80);
    assert_eq!(pty_pixel_extent(1000.0, usize::MAX), u16::MAX);
}

#[test]
fn px_to_cell_reports_sub_cell_side() {
    use super::px_to_cell;
    use kettle_core::Side;
    let rect = (10.0, 20.0, 800.0, 600.0);
    let (cw, ch, pad) = (8.0_f32, 16.0_f32, 4.0_f32);
    // Content origin x = rx + pad = 14.0; each cell is `cw` wide.
    let ox = 10.0 + pad; // 14.0
    let y = 30.0; // any in-grid row

    // Left half of cell 0 → Left.
    let (c0, _, s0) = px_to_cell(ox + 1.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(c0, 0);
    assert_eq!(s0, Side::Left, "left half of the cell → Left");

    // Right half of cell 0 → Right.
    let (_, _, s1) = px_to_cell(ox + 6.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(s1, Side::Right, "right half of the cell → Right");

    // The exact midpoint counts as the right half (matches alacritty's
    // `< half ⇒ Left` boundary — anything ≥ half is Right).
    let (_, _, s2) = px_to_cell(ox + cw / 2.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(s2, Side::Right, "exact midpoint → Right");

    // The side is a within-cell property: the same sub-cell offset in a later
    // column yields the same side (no drift across the row).
    let (c3, _, s3) = px_to_cell(ox + cw * 5.0 + 1.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(c3, 5);
    assert_eq!(s3, Side::Left, "side is independent of the column index");

    // A pointer in the LEFT PADDING (left of column 0) clamps to col 0 AND
    // Side::Left, so a drag starting there still INCLUDES the first cell.
    // Deriving the side from the raw negative offset via `rem_euclid` would
    // wrap it into the right half and drop column 0.
    let (c4, _, s4) = px_to_cell(ox - 3.0, y, rect, (cw, ch), (pad, pad));
    assert_eq!(c4, 0, "left of origin clamps to column 0");
    assert_eq!(
        s4,
        Side::Left,
        "left of origin → Side::Left (first cell kept)"
    );
}

#[test]
fn keybind_chord_is_safe_rejects_modless_text_keys() {
    use super::keybind_chord_is_safe;
    use kettle_config::{Key as KKey, Mods};
    // Modifier-less text/essential keys are REFUSED (the soft-brick).
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Char('a')));
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Enter));
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Tab));
    assert!(!keybind_chord_is_safe(Mods::empty(), KKey::Up));
    // A modifier-less F-key is fine (produces no text — F1=help etc.).
    assert!(keybind_chord_is_safe(Mods::empty(), KKey::F(1)));
    // Any modifier makes the chord safe.
    assert!(keybind_chord_is_safe(Mods::CTRL, KKey::Char('a')));
    assert!(keybind_chord_is_safe(Mods::ALT, KKey::Enter));
    assert!(keybind_chord_is_safe(Mods::SHIFT, KKey::Char('z')));
}

/// Drift guard. Rebinding a Settings keybind row to a chord already bound
/// to a DIFFERENT action must not silently steal it. Like the
/// modifier-safety check before it, which refuses to apply silently (it
/// notifies and stays in capture mode), the conflict check must ask first
/// instead of falling straight into `keybinds.insert`. A behavioral test
/// needs a live App + WindowState + Settings overlay, so this pins the
/// shape at the source level.
#[test]
fn keybind_rebind_conflict_is_gated_behind_confirmation() {
    let src = production_source();
    let capture_arm = src
        .split("if !keybind_chord_is_safe(mods, kk) {")
        .nth(1)
        .and_then(|s| s.split("\n        match key {").next())
        .expect("chord-capture body present");
    assert!(
        capture_arm.contains("self.cfg.keybinds.get(&trig).filter(|v| **v != act).cloned()"),
        "a conflicting existing binding must be looked up before applying the rebind"
    );
    let capture_arm_normalized = capture_arm.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        capture_arm_normalized.contains("self.install_confirm_dialog(")
            && capture_arm.contains("ConfirmAction::RebindKeybind {"),
        "a conflicting rebind must route through a confirm dialog, not apply immediately"
    );
    assert!(
        capture_arm.contains("self.apply_keybind_rebind(trig, act, action);"),
        "a non-conflicting rebind must still apply immediately via the shared helper"
    );
    // The confirm dialog's dispatch must finish the SAME way the
    // direct path does — no separate, divergent apply logic.
    let dispatch = src
        .split("fn dispatch_confirm_action_arms(")
        .nth(1)
        .and_then(|s| s.split("\n    fn ").next())
        .expect("dispatch_confirm_action_arms body present");
    assert!(
        dispatch.contains("ConfirmAction::RebindKeybind {")
            && dispatch.contains("self.apply_keybind_rebind(trig, act, action_name);"),
        "confirming a rebind must apply via apply_keybind_rebind, matching the direct path"
    );
}

#[test]
fn keybind_rebind_persistence_reports_missing_path_and_writes_valid_path() {
    use super::persist_keybind_rebind;

    let stale = vec!["Ctrl+Shift+X".to_string()];
    assert!(
        !persist_keybind_rebind(None, &stale, "Ctrl+Shift+Y", "copy"),
        "a missing config path must be reported as not saved"
    );

    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    assert!(persist_keybind_rebind(
        Some(&path),
        &stale,
        "Ctrl+Shift+Y",
        "copy"
    ));
    let saved = std::fs::read_to_string(path).expect("saved keybind config");
    assert!(saved.contains("Ctrl+Shift+X=unbind"));
    assert!(saved.contains("Ctrl+Shift+Y=copy"));
}

/// A rebind saves its unbinds and its new binding in one edit. At the size
/// cap the whole edit is refused, so the old chord stays bound; saving the
/// unbind alone left the action with no chord at all after a reload.
#[test]
fn keybind_rebind_at_the_size_cap_leaves_the_config_unchanged() {
    let dir = crate::test_tempdir();
    let path = dir.path().join("config");
    let mut original = String::from("keybind = Ctrl+Alt+R=new_tab\n#");
    original.push_str(&"x".repeat(1024 * 1024 - original.len() - 1));
    original.push('\n');
    std::fs::write(&path, &original).unwrap();

    let stale = vec!["Ctrl+Alt+R".to_string()];
    assert!(!super::persist_keybind_rebind(
        Some(&path),
        &stale,
        "Ctrl+Alt+Y",
        "new_tab",
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn cwd_is_local_rejects_unc_and_traversal() {
    use super::cwd_is_local;
    // Drift guard. Ordinary absolute cwds (the OSC 7 path form,
    // which uses forward slashes even on Windows: /C:/Users/x) are fine.
    assert!(cwd_is_local("/home/user"));
    assert!(cwd_is_local("/C:/Users/me/Repos"));
    // UNC / authority forms and traversal are refused (the SMB/NTLM vector).
    assert!(!cwd_is_local("//evil.host/share"));
    assert!(!cwd_is_local("\\\\evil\\share"));
    assert!(!cwd_is_local("/x/../../etc/passwd"));
    assert!(!cwd_is_local(""));
}

#[test]
fn extra_mouse_sgr_maps_side_buttons() {
    use super::extra_mouse_sgr;
    use winit::event::MouseButton;
    // Drift guard. Back = XBUTTON1 = SGR 128, Forward =
    // XBUTTON2 = SGR 129; the primary three + anything else are handled
    // locally (or dropped), so they return None here.
    assert_eq!(extra_mouse_sgr(MouseButton::Back), Some(128));
    assert_eq!(extra_mouse_sgr(MouseButton::Forward), Some(129));
    assert_eq!(extra_mouse_sgr(MouseButton::Left), None);
    assert_eq!(extra_mouse_sgr(MouseButton::Middle), None);
    assert_eq!(extra_mouse_sgr(MouseButton::Right), None);
    assert_eq!(extra_mouse_sgr(MouseButton::Other(5)), None);
}

/// `send_mouse`'s `back` and `forward` report what the physical buttons
/// do, so a mouse-tracking program cannot tell them apart; xterm's button
/// numbers 8 and 9 would read as a modified left or middle press.
#[test]
fn ctl_side_buttons_report_like_the_physical_ones() {
    use super::{ctl_mouse_button, extra_mouse_sgr};
    use winit::event::MouseButton;
    let button = |name: &str| ctl_mouse_button(&serde_json::json!({ "button": name }));
    assert_eq!(button("back").ok(), extra_mouse_sgr(MouseButton::Back));
    assert_eq!(
        button("forward").ok(),
        extra_mouse_sgr(MouseButton::Forward)
    );
    assert_eq!(button("left"), Ok(0));
    let sgr = |code| crate::input::mouse_encode(true, code, true, false, 0, 0, Default::default());
    assert_eq!(sgr(button("back").unwrap()), b"\x1b[<128;1;1M");
}

#[test]
fn cursor_in_tab_bar_band_geometry() {
    use super::{TabBarPos, cursor_in_tab_bar_band};
    // 600 px window, 24 px tab bar. Top: [0, 24).
    assert!(cursor_in_tab_bar_band(0.0, 24.0, 600.0, TabBarPos::Top));
    assert!(cursor_in_tab_bar_band(23.0, 24.0, 600.0, TabBarPos::Top));
    assert!(!cursor_in_tab_bar_band(24.0, 24.0, 600.0, TabBarPos::Top));
    assert!(!cursor_in_tab_bar_band(300.0, 24.0, 600.0, TabBarPos::Top));
    // Bottom: [576, 600].
    assert!(cursor_in_tab_bar_band(
        576.0,
        24.0,
        600.0,
        TabBarPos::Bottom
    ));
    assert!(cursor_in_tab_bar_band(
        600.0,
        24.0,
        600.0,
        TabBarPos::Bottom
    ));
    assert!(!cursor_in_tab_bar_band(
        575.0,
        24.0,
        600.0,
        TabBarPos::Bottom
    ));
    assert!(!cursor_in_tab_bar_band(0.0, 24.0, 600.0, TabBarPos::Bottom));
    // Hidden tab bar (`tab-bar = off` or single-tab `auto`) → always
    // out of band so the wheel falls through to scrollback.
    assert!(!cursor_in_tab_bar_band(0.0, 0.0, 600.0, TabBarPos::Top));
    assert!(!cursor_in_tab_bar_band(0.0, 0.0, 600.0, TabBarPos::Bottom));
}

#[test]
fn cursor_in_status_bar_band_geometry() {
    // Drift guard for the status-bar cursor icon, same shape as the
    // `cursor_in_tab_bar_band_geometry` drift guard above. Pins:
    //   - Off mode → always false (status bar invisible).
    //   - Top mode → [0, bar_h).
    //   - Bottom mode → [surface - bar_h, surface].
    //   - bar_h == 0 → false regardless of mode, checked before the mode
    //     (`status_bar_h()` returns 0 for Off).
    use super::cursor_in_status_bar_band;
    use kettle_config::StatusBarMode;
    // Off → always false.
    assert!(!cursor_in_status_bar_band(
        0.0,
        22.0,
        600.0,
        StatusBarMode::Off
    ));
    assert!(!cursor_in_status_bar_band(
        300.0,
        22.0,
        600.0,
        StatusBarMode::Off
    ));
    // Top: [0, 22).
    assert!(cursor_in_status_bar_band(
        0.0,
        22.0,
        600.0,
        StatusBarMode::Top
    ));
    assert!(cursor_in_status_bar_band(
        21.0,
        22.0,
        600.0,
        StatusBarMode::Top
    ));
    assert!(!cursor_in_status_bar_band(
        22.0,
        22.0,
        600.0,
        StatusBarMode::Top
    ));
    assert!(!cursor_in_status_bar_band(
        300.0,
        22.0,
        600.0,
        StatusBarMode::Top
    ));
    // Bottom: [578, 600].
    assert!(cursor_in_status_bar_band(
        578.0,
        22.0,
        600.0,
        StatusBarMode::Bottom
    ));
    assert!(cursor_in_status_bar_band(
        600.0,
        22.0,
        600.0,
        StatusBarMode::Bottom
    ));
    assert!(!cursor_in_status_bar_band(
        577.0,
        22.0,
        600.0,
        StatusBarMode::Bottom
    ));
    assert!(!cursor_in_status_bar_band(
        0.0,
        22.0,
        600.0,
        StatusBarMode::Bottom
    ));
    // bar_h == 0 → out of band regardless of mode.
    assert!(!cursor_in_status_bar_band(
        0.0,
        0.0,
        600.0,
        StatusBarMode::Top
    ));
    assert!(!cursor_in_status_bar_band(
        0.0,
        0.0,
        600.0,
        StatusBarMode::Bottom
    ));
}

#[test]
fn chrome_cursor_icon_overrides_only_for_chrome() {
    use super::chrome_cursor_icon;
    use winit::window::CursorIcon;
    // Content area, no modals → caller picks (None means "use content
    // logic" — Text by default, Pointer over a Ctrl-held link).
    assert_eq!(chrome_cursor_icon(false, false), None);
    // Tab bar → Default arrow (clickable tabs are not selectable text).
    assert_eq!(chrome_cursor_icon(true, false), Some(CursorIcon::Default));
    // Modal up (search / palette / hints / SSH launcher) → Default arrow
    // regardless of where the pointer is, so the I-beam doesn't bleed
    // through onto the overlay.
    assert_eq!(chrome_cursor_icon(false, true), Some(CursorIcon::Default));
    // Both at once (modal opened while pointer happened to be over the
    // tab bar) → still Default.
    assert_eq!(chrome_cursor_icon(true, true), Some(CursorIcon::Default));
}

/// With `close-button-on-tab = false`, the tab's close rect must be
/// unhittable, not just unpainted. A full-size hidden rect lets a click on
/// the tab's trailing square close it, and every pane in it, with no
/// visible button.
///
/// The empty rect keeps all three consumers (real press, ctl press, hover
/// cursor) safe without each having to consult the config.
#[test]
fn a_hidden_tab_close_button_is_not_clickable() {
    use super::{hovered_close_button, tab_close_rect};
    use kettle_render::TabSeg;

    let visible = tab_close_rect(76.0, 0.0, 24.0, true);
    let hidden = tab_close_rect(76.0, 0.0, 24.0, false);
    assert_eq!(visible, (76.0, 0.0, 24.0, 24.0), "shown: full square");
    assert_eq!(hidden.2, 0.0, "hidden: zero width");
    assert_eq!(hidden.3, 0.0, "hidden: zero height");

    // Probe the dead centre of where the hidden button would be.
    let seg = |close| TabSeg {
        idx: 0,
        rect: (0.0, 0.0, 100.0, 24.0),
        title_rect: (8.0, 0.0, 68.0, 24.0),
        close,
        title: String::new(),
        path: None,
        active: true,
        activity: kettle_render::TabActivity::Normal,
    };
    assert_eq!(
        hovered_close_button(&[seg(visible)], 88.0, 12.0),
        Some(0),
        "a visible close button must still be hittable"
    );
    assert_eq!(
        hovered_close_button(&[seg(hidden)], 88.0, 12.0),
        None,
        "a hidden close button must not be hittable"
    );
}

/// A hand-set pane name must survive the shell's next prompt. bash and zsh
/// emit OSC 0/2 on EVERY prompt, so without the Manual origin, naming a pane
/// `db-prod` would last under a second.
#[test]
fn a_hand_set_pane_title_outranks_the_shells_osc_title() {
    use super::osc_title_may_replace;
    use crate::mux::PaneTitleOrigin;

    assert!(
        !osc_title_may_replace(PaneTitleOrigin::Manual),
        "a title the user typed must not be replaced by the shell"
    );
    // Every other origin is shell- or launch-derived and must keep
    // tracking OSC, or titles would freeze at the first thing set.
    for origin in [
        PaneTitleOrigin::Placeholder,
        PaneTitleOrigin::ExplicitLaunch,
        PaneTitleOrigin::Osc,
        PaneTitleOrigin::Remote,
    ] {
        assert!(
            osc_title_may_replace(origin),
            "{origin:?} must still follow the shell's title"
        );
    }
}

/// The gate has to cover BOTH doors. Shells also emit a title RESET at
/// prompts, so gating only the OSC set would still lose the hand-set name,
/// just via the other event.
#[test]
fn a_hand_set_pane_title_survives_a_title_reset_too() {
    use super::apply_shell_title;
    use crate::mux::PaneTitleOrigin;

    let mut title = "db-prod".to_string();
    let mut placeholder = false;
    let mut origin = PaneTitleOrigin::Manual;
    let mut before_remote = None;

    for door in [Some("bash"), None] {
        apply_shell_title(
            &mut title,
            &mut placeholder,
            &mut origin,
            &mut before_remote,
            door,
        );
        assert_eq!(title, "db-prod", "the shell must not take a hand-set name");
        assert_eq!(origin, PaneTitleOrigin::Manual);
        assert!(!placeholder);
    }
}

/// Exiting ssh must put the pane's own name back. While connected, the
/// remote shell's title shows, since it is more useful than kettle's
/// synthetic label. Applying it must keep the saved pre-remote title, or
/// the disconnect restore has nothing to restore. It must also keep the
/// `Remote` origin, or the restore does not run and a pane named `db-prod`
/// comes back from ssh still calling itself the remote host.
#[test]
fn a_remote_shells_title_shows_without_losing_the_name_to_come_back_to() {
    use super::{apply_remote_title_transition, apply_shell_title};
    use crate::mux::PaneTitleOrigin;

    let mut title = "db-prod".to_string();
    let mut placeholder = false;
    let mut origin = PaneTitleOrigin::Manual;
    let mut before_remote = None;

    // Enter a remote context: the manual name is stashed.
    let ctx = kettle_remote::RemoteContext::Ssh {
        host: "buildbox".into(),
        user: Some("kevim".into()),
        options: kettle_remote::SshOptions::default(),
    };
    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some(&ctx),
    );
    assert_eq!(origin, PaneTitleOrigin::Remote);
    assert_eq!(
        before_remote,
        Some(("db-prod".to_string(), PaneTitleOrigin::Manual))
    );

    // The remote shell sets its own title. It shows...
    apply_shell_title(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        Some("kevim@buildbox:~/work"),
    );
    assert_eq!(title, "kevim@buildbox:~/work", "the remote title shows");
    // ...without disturbing either thing the restore depends on.
    assert_eq!(
        origin,
        PaneTitleOrigin::Remote,
        "the pane is still in a remote context"
    );
    assert_eq!(
        before_remote,
        Some(("db-prod".to_string(), PaneTitleOrigin::Manual)),
        "the name to come back to must survive the remote shell's title"
    );

    // Disconnect: the hand-set name comes back.
    apply_remote_title_transition(
        &mut title,
        &mut placeholder,
        &mut origin,
        &mut before_remote,
        None,
    );
    assert_eq!(title, "db-prod");
    assert_eq!(origin, PaneTitleOrigin::Manual);
    assert_eq!(before_remote, None);
}

#[test]
fn hovered_close_button_finds_only_the_close_rect_hits() {
    use super::hovered_close_button;
    use kettle_render::TabSeg;
    // Two tab segments side-by-side, each 100×24 with a trailing
    // 24-px close-button zone at x=76..100 (segment 0) and
    // x=176..200 (segment 1) — mirrors the tab_bar()
    // builder's `(x + seg_w - height)` formula.
    let segs = vec![
        TabSeg {
            idx: 0,
            rect: (0.0, 0.0, 100.0, 24.0),
            title_rect: (8.0, 0.0, 68.0, 24.0),
            close: (76.0, 0.0, 24.0, 24.0),
            title: "one".into(),
            path: None,
            active: true,
            activity: kettle_render::TabActivity::Normal,
        },
        TabSeg {
            idx: 1,
            rect: (100.0, 0.0, 100.0, 24.0),
            title_rect: (108.0, 0.0, 68.0, 24.0),
            close: (176.0, 0.0, 24.0, 24.0),
            title: "two".into(),
            path: None,
            active: false,
            activity: kettle_render::TabActivity::Normal,
        },
    ];
    // Cursor over title area of segment 0 → no close hit.
    assert_eq!(hovered_close_button(&segs, 20.0, 12.0), None);
    // Cursor over segment 0's close button → hit on idx 0.
    assert_eq!(hovered_close_button(&segs, 88.0, 12.0), Some(0));
    // Cursor over segment 1's close button → hit on idx 1.
    assert_eq!(hovered_close_button(&segs, 188.0, 12.0), Some(1));
    // Cursor outside the bar entirely → no hit.
    assert_eq!(hovered_close_button(&segs, 88.0, 100.0), None);
    // Edge: cursor exactly at the bar bottom (24.0) is outside
    // because `py < ry + rh` is strict.
    assert_eq!(hovered_close_button(&segs, 88.0, 24.0), None);
    // Empty bar (single-pane, tabs hidden) → no hit ever.
    assert_eq!(hovered_close_button(&[], 50.0, 10.0), None);
}

#[test]
fn tab_drag_target_index_uses_rendered_segment_rects() {
    use super::tab_drag_target_index;
    let seg = |idx, x, w| {
        let rect = (x, 0.0, w, 24.0);
        let close = (x + w - 24.0, 0.0, 24.0, 24.0);
        kettle_render::TabSeg {
            idx,
            rect,
            title_rect: super::tab_title_rect(rect, close, true),
            close,
            title: format!("tab-{idx}"),
            path: None,
            active: idx == 0,
            activity: kettle_render::TabActivity::Normal,
        }
    };
    // 3 equal tabs, 300-px strip → 100 px per segment. Cursor at
    // 50 → tab 0; 150 → tab 1; 250 → tab 2.
    let equal = [
        seg(0, 0.0, 100.0),
        seg(1, 100.0, 100.0),
        seg(2, 200.0, 100.0),
    ];
    assert_eq!(tab_drag_target_index(50.0, 12.0, &equal), 0);
    assert_eq!(tab_drag_target_index(150.0, 12.0, &equal), 1);
    assert_eq!(tab_drag_target_index(250.0, 12.0, &equal), 2);
    // Right at the boundary: 100 → tab 1 (floor); 200 → tab 2.
    assert_eq!(tab_drag_target_index(100.0, 12.0, &equal), 1);
    assert_eq!(tab_drag_target_index(200.0, 12.0, &equal), 2);
    // Natural-width tabs: targeting follows the rendered rects,
    // not a hidden strip/n assumption.
    let natural = [seg(0, 0.0, 72.0), seg(1, 72.0, 144.0), seg(2, 216.0, 88.0)];
    assert_eq!(tab_drag_target_index(71.0, 12.0, &natural), 0);
    assert_eq!(tab_drag_target_index(72.0, 12.0, &natural), 1);
    assert_eq!(tab_drag_target_index(215.0, 12.0, &natural), 1);
    assert_eq!(tab_drag_target_index(216.0, 12.0, &natural), 2);
    // Negative cursor (past the left edge) → clamps to 0.
    assert_eq!(tab_drag_target_index(-50.0, 12.0, &natural), 0);
    // Past the right edge → clamps to last segment, not n.
    assert_eq!(tab_drag_target_index(900.0, 12.0, &natural), 2);
    assert_eq!(tab_drag_target_index(f32::MAX, 12.0, &natural), 2);
    // Empty bar → 0 (defensive no-op).
    assert_eq!(tab_drag_target_index(50.0, 12.0, &[]), 0);
}

/// `tab-position = left` / `right` stacks the segments down a shared
/// column, so every one of them contains any x inside the strip. Targeting
/// by the cursor's x would always match the first segment, and dragging a
/// tab anywhere below the top of the strip would move tab 0 instead.
#[test]
fn a_vertical_tab_strip_reorders_by_the_axis_it_is_actually_stacked_on() {
    use super::tab_drag_target_index;
    let seg = |idx, y, h| {
        let rect = (0.0, y, 160.0, h);
        let close = (136.0, y, 24.0, 24.0);
        kettle_render::TabSeg {
            idx,
            rect,
            title_rect: super::tab_title_rect(rect, close, true),
            close,
            title: format!("tab-{idx}"),
            path: None,
            active: idx == 0,
            activity: kettle_render::TabActivity::Normal,
        }
    };
    let stacked = [seg(0, 0.0, 28.0), seg(1, 28.0, 28.0), seg(2, 56.0, 28.0)];

    // Precondition: every segment spans the same x, which is exactly why
    // an x-axis test could not tell them apart.
    let x_mid = 80.0;
    assert!(
        stacked
            .iter()
            .all(|s| x_mid >= s.rect.0 && x_mid < s.rect.0 + s.rect.2),
        "fixture must share one column, or the old bug can't be reproduced"
    );

    assert_eq!(tab_drag_target_index(x_mid, 14.0, &stacked), 0);
    assert_eq!(tab_drag_target_index(x_mid, 42.0, &stacked), 1);
    assert_eq!(tab_drag_target_index(x_mid, 70.0, &stacked), 2);
    // Boundaries follow the rendered rects, same rule as horizontal.
    assert_eq!(tab_drag_target_index(x_mid, 28.0, &stacked), 1);
    assert_eq!(tab_drag_target_index(x_mid, 56.0, &stacked), 2);
    // Past either end clamps to the nearest segment rather than running off.
    assert_eq!(tab_drag_target_index(x_mid, -40.0, &stacked), 0);
    assert_eq!(tab_drag_target_index(x_mid, 9_000.0, &stacked), 2);
    // The x coordinate is now irrelevant on a vertical strip — including
    // one far outside it, which a torn-drag produces.
    assert_eq!(tab_drag_target_index(-999.0, 42.0, &stacked), 1);
}

/// Side tab bars reorder by drag like top and bottom ones. Both pointer paths
/// once skipped the reorder for `tab-position = left` / `right`, a leftover
/// from before `tab_drag_target_index` read the strip's axis off its segments.
#[test]
fn every_tab_bar_position_reorders_by_drag() {
    let src = production_source();
    let native_move = src
        .split("WindowEvent::CursorMoved { position, .. } => {")
        .nth(1)
        .and_then(|body| body.split("WindowEvent::").next())
        .expect("native CursorMoved arm");
    let ctl_move = src
        .split("fn ctl_mouse_move(")
        .nth(1)
        .and_then(|body| body.split("\n    fn ").next())
        .expect("ctl_mouse_move body");
    for (label, body) in [("native move", native_move), ("ctl move", ctl_move)] {
        let reorder = body
            .find("self.reorder_active_tab_for_cursor(ws)")
            .unwrap_or_else(|| panic!("{label} reorders through the shared helper"));
        let gate = &body[body[..reorder].rfind("if ").expect("a reorder gate")..reorder];
        assert!(
            gate.contains("ws.tab_drag_active") && gate.contains("ws.tab_drag_press.is_none()"),
            "{label} reorders only a promoted tab drag"
        );
        assert!(
            !gate.contains("is_vertical"),
            "{label} reorders on every tab bar position"
        );
    }
}

#[test]
fn tab_title_rect_excludes_visible_close_button() {
    use super::tab_title_rect;

    let segment = (100.0, 0.0, 240.0, 24.0);
    let close = (316.0, 0.0, 24.0, 24.0);
    assert_eq!(
        tab_title_rect(segment, close, true),
        (108.0, 0.0, 208.0, 24.0)
    );
    assert_eq!(
        tab_title_rect(segment, close, false),
        (108.0, 0.0, 224.0, 24.0)
    );
}

#[test]
fn tab_reorder_drag_threshold_filters_click_jitter() {
    use super::tab_reorder_drag_threshold_px;
    // Default-height tab bars get a 12px promotion distance. That leaves
    // normal mouse-down jitter visually in the pressed-tab state instead of
    // flashing the drag ghost beside the selected tab.
    assert!((tab_reorder_drag_threshold_px(24.0) - 12.0).abs() < f32::EPSILON);
    // Tiny and large bars are bounded so the gesture remains usable.
    assert_eq!(tab_reorder_drag_threshold_px(8.0), 8.0);
    assert_eq!(tab_reorder_drag_threshold_px(60.0), 16.0);
}

/// The tear decision is uniform hysteresis in every direction away from the
/// tab band, for every `tab-bar-pos`.
#[test]
fn tear_threshold_crossed_per_orientation() {
    use super::{dist_to_rect, tear_threshold_crossed};
    // Top band (0,0,800,24), threshold 36 (= 1.5 × 24).
    let top = (0.0, 0.0, 800.0, 24.0);
    // Inside the band, anywhere along the strip: reorder, never tear.
    assert!(!tear_threshold_crossed(5.0, 12.0, top, 36.0));
    assert!(!tear_threshold_crossed(790.0, 12.0, top, 36.0));
    // Below the band but within the slop: still no tear.
    assert!(!tear_threshold_crossed(400.0, 59.0, top, 36.0));
    // Past the slop downward (into the terminal content): tear.
    assert!(tear_threshold_crossed(400.0, 60.0, top, 36.0));
    // Above the window (band edge == window edge): same hysteresis.
    assert!(!tear_threshold_crossed(400.0, -35.0, top, 36.0));
    assert!(tear_threshold_crossed(400.0, -36.0, top, 36.0));
    // Past the strip's right end horizontally: distance is measured
    // from the band edge, so the slop applies there too.
    assert!(!tear_threshold_crossed(820.0, 12.0, top, 36.0));
    assert!(tear_threshold_crossed(836.0, 12.0, top, 36.0));
    // Diagonal: Euclidean, not Manhattan — 30² + 30² > 36².
    assert!(tear_threshold_crossed(830.0, 54.0, top, 36.0));
    // Bottom band on a 600-tall surface.
    let bottom = (0.0, 576.0, 800.0, 24.0);
    assert!(!tear_threshold_crossed(400.0, 580.0, bottom, 36.0));
    assert!(tear_threshold_crossed(400.0, 540.0, bottom, 36.0));
    // Left vertical band (width = tab_bar_width 200): tearing happens
    // rightward past the slop.
    let left = (0.0, 0.0, 200.0, 600.0);
    assert!(!tear_threshold_crossed(100.0, 300.0, left, 36.0));
    assert!(!tear_threshold_crossed(235.0, 300.0, left, 36.0));
    assert!(tear_threshold_crossed(236.0, 300.0, left, 36.0));
    // Right vertical band: leftward.
    let right = (600.0, 0.0, 200.0, 600.0);
    assert!(tear_threshold_crossed(560.0, 300.0, right, 36.0));
    // A zero threshold can never tear (defensive: hidden bar).
    assert!(!tear_threshold_crossed(400.0, 500.0, top, 0.0));
    // dist_to_rect itself: inside = 0, outside = Euclidean.
    assert_eq!(dist_to_rect(10.0, 10.0, top), 0.0);
    assert_eq!(dist_to_rect(400.0, 124.0, top), 100.0);
    assert_eq!(dist_to_rect(803.0, 28.0, top), 5.0); // 3-4-5 corner
}

/// The ghost's tear-lift is a clamped linear ramp over the SAME
/// distance/threshold pair the tear decision uses, so 1.0 lands exactly
/// where `tear_threshold_crossed` fires.
#[test]
fn tear_lift_ratio_scales_with_distance() {
    use super::{tear_lift_ratio, tear_threshold_crossed};
    let top = (0.0, 0.0, 800.0, 24.0);
    // Inside the band: no lift, anywhere along the strip.
    assert_eq!(tear_lift_ratio(5.0, 12.0, top, 36.0), 0.0);
    assert_eq!(tear_lift_ratio(790.0, 12.0, top, 36.0), 0.0);
    // Halfway to the threshold below the band: half lift.
    assert_eq!(tear_lift_ratio(400.0, 42.0, top, 36.0), 0.5);
    // Exactly at the threshold: full lift — and the tear fires here.
    assert_eq!(tear_lift_ratio(400.0, 60.0, top, 36.0), 1.0);
    assert!(tear_threshold_crossed(400.0, 60.0, top, 36.0));
    // Far past the threshold (Wayland's at-release hover): clamped.
    assert_eq!(tear_lift_ratio(400.0, 500.0, top, 36.0), 1.0);
    // Above the window: same uniform hysteresis as the tear decision.
    assert_eq!(tear_lift_ratio(400.0, -18.0, top, 36.0), 0.5);
    // A zero threshold can never tear, so it never lifts either.
    assert_eq!(tear_lift_ratio(400.0, 500.0, top, 0.0), 0.0);
}

#[test]
fn window_choice_keeps_the_original_tab_and_is_keyboard_reachable() {
    let panes: Arc<[u64]> = vec![41, 42].into();
    let row = super::ContextMenuItem::WindowChoice {
        label: "2: project".into(),
        window: 2,
        panes: Arc::clone(&panes),
    };
    assert!(super::item_is_dispatchable(&row));
    let Some(super::ContextMenuClick::MoveTabToWindow {
        window,
        panes: captured,
    }) = super::item_to_click(&row, 0)
    else {
        panic!("window choices need the same mouse and keyboard dispatch");
    };
    assert_eq!(window, 2);
    assert_eq!(&*captured, &[41, 42]);
    assert!(Arc::ptr_eq(&panes, &captured));
}

/// A bottom bar sits above the search bar and a bottom status bar; the
/// docking band must be where the bar is painted, not at the window edge.
#[test]
fn bottom_dock_band_follows_search_and_bottom_status_chrome() {
    use super::{TabBarPos, dist_to_rect, tab_band_rect, tear_lift_ratio};
    for search_height in [0.0, 26.0, 78.0] {
        for bottom_status_height in [0.0, 22.0] {
            let band = tab_band_rect(
                (800, 600),
                TabBarPos::Bottom,
                24.0,
                180.0,
                search_height,
                bottom_status_height,
            );
            let visible_center = 600.0 - search_height - bottom_status_height - 12.0;
            assert_eq!(dist_to_rect(400.0, visible_center, band), 0.0);
            assert_eq!(tear_lift_ratio(400.0, visible_center, band, 36.0), 0.0);
            if search_height + bottom_status_height >= 24.0 {
                assert!(dist_to_rect(400.0, 588.0, band) > 0.0);
            }
        }
    }
}

/// Side strips stop above the search bar; the top band is unaffected.
#[test]
fn side_dock_bands_exclude_search_without_moving_the_top_band() {
    use super::{TabBarPos, dist_to_rect, tab_band_rect};
    for pos in [TabBarPos::Left, TabBarPos::Right] {
        let band = tab_band_rect((800, 600), pos, 24.0, 180.0, 78.0, 22.0);
        let x = if matches!(pos, TabBarPos::Left) {
            90.0
        } else {
            710.0
        };
        assert_eq!(dist_to_rect(x, 510.0, band), 0.0);
        assert!(dist_to_rect(x, 560.0, band) > 0.0);
        assert_eq!(band.3, 522.0);
    }
    assert_eq!(
        tab_band_rect((800, 600), TabBarPos::Top, 24.0, 180.0, 78.0, 22.0),
        (0.0, 0.0, 800.0, 24.0),
    );
}

/// Painting and docking share one band function.
#[test]
fn painted_and_docking_tab_bands_share_one_function() {
    let src = production_source();
    assert_eq!(src.matches("self.tab_band(ws, height)").count(), 2);
}

#[test]
fn rescue_tick_finishes_released_tab_drags_before_following() {
    let src = production_source();
    let tick = src
        .split_once("fn torn_drag_pointer_tick(")
        .unwrap()
        .1
        .split_once("let cursor = cursor_screen_pos()?;")
        .unwrap()
        .1;
    let before_follow = tick.split_once("if td.native {").unwrap().0;
    assert!(before_follow.contains("if !primary_button_physically_held() {"));
    assert!(before_follow.contains("self.revalidate_dock_latch(&ws)"));
    assert!(before_follow.contains("self.finalize_torn_drag(&mut ws, commit);"));
}

/// Docking inserts BETWEEN segments, so n tabs have n+1 slots, decided by
/// segment midpoints.
#[test]
fn dock_insertion_index_slots_between_segments() {
    use super::dock_insertion_index;
    // Three segments at mids 50 / 150 / 250.
    let mids = [50.0, 150.0, 250.0];
    assert_eq!(dock_insertion_index(&mids, 0.0), 0); // before first
    assert_eq!(dock_insertion_index(&mids, 49.0), 0);
    assert_eq!(dock_insertion_index(&mids, 51.0), 1); // past mid 0
    assert_eq!(dock_insertion_index(&mids, 149.0), 1);
    assert_eq!(dock_insertion_index(&mids, 200.0), 2);
    assert_eq!(dock_insertion_index(&mids, 251.0), 3); // append
    assert_eq!(dock_insertion_index(&mids, f32::MAX), 3);
    // No segments (hidden-bar fallback handles this elsewhere): 0.
    assert_eq!(dock_insertion_index(&[], 100.0), 0);
}

#[test]
fn dock_insertion_index_preserves_visible_tab_indices() {
    use crate::app::dock_insertion_index;
    let mut mids = vec![-100_000.0; 20];
    mids[..5].copy_from_slice(&[50.0, 150.0, 250.0, 350.0, 450.0]);
    assert_eq!(dock_insertion_index(&mids, 475.0), 5);
    assert_eq!(dock_insertion_index(&mids, 425.0), 4);
    mids.fill(-100_000.0);
    mids[8..11].copy_from_slice(&[50.0, 150.0, 250.0]);
    assert_eq!(dock_insertion_index(&mids, 0.0), 8);
    assert_eq!(dock_insertion_index(&mids, 251.0), 11);
}

/// Drift guard: the tear-at-threshold call sites and the native drag
/// handoff stay wired the way the platform analysis verified them. The
/// needles are source-level because the flow spans winit's event dispatch
/// and can't run headless.
#[test]
fn tear_off_flow_stays_wired() {
    let src = production_source();
    // 1. The CursorMoved FSM block tears at the threshold (Chromium
    //    model), not at release.
    assert!(
        src.contains("if self.maybe_tear_off(ws, event_loop) {"),
        "CursorMoved must drive the threshold tear"
    );
    // 2. The torn window inherits the source size and is positioned by
    //    the grab offset (open_window's size override).
    assert!(
        src.contains(
            "WindowOpen::AdoptTab {\n                    tab: dt,\n                    source_color: self.tear_off_source_color(ws),\n                    paste: Box::new(paste),\n                },\n                pos,\n                Some(size),"
        ),
        "the tear must open the torn window at the grab position and source size"
    );
    // 2b. A lone-tab drag follows the pointer on the same motion that
    //     started it, so one fast gesture can latch a dock target.
    assert!(
        src.contains(
            "                caption: false,\n                saw_move: false,\n                last_signal: std::time::Instant::now(),\n                hwnd: window_hwnd(&src),\n                signal_cursor: None,\n            });\n            self.follow_torn_drag(ws);\n            return true;"
        ),
        "the lone-tab drag must follow the pointer immediately"
    );
    // 2c. A new window applies a due scheduled theme before claiming its
    //     accent, so the source exclusion uses the pool it will paint.
    assert!(
        src.contains(
            "        self.poll_theme_schedule(&mut ws);\n        if self.cfg.accent_auto && self.cfg.accent_color.is_none() {\n            self.assign_window_accent(&mut ws, source_color);"
        ),
        "the theme schedule must run before the accent claim"
    );
    // 3. The native handoff happens right after the insert.
    assert!(
        src.contains(".map(|w| match w.drag_window() {"),
        "the torn window must be handed to the OS move loop"
    );
    // 4. The at-release tear is Wayland-only now.
    assert!(
        src.contains(
            "if self.cfg.detachable_tabs\n                        && dropped_outside\n                        && wayland\n                        && ws.mux.tabs.len() > 1\n                    {"
        ),
        "the Released-arm tear must be gated to Wayland and detachable-tabs"
    );
    // 5. The drop commits the latched dock from the left-release, GATED on
    //    the primary button being physically up. Windows' synthesized
    //    release also fires for an Esc-CANCELLED modal loop, with the
    //    button still held, and committing there would perform the exact
    //    merge the user is cancelling.
    assert!(
        src.contains("let commit = !primary_button_physically_held();"),
        "the release-drop must distinguish Esc-cancel via physical button state"
    );
    // 6. The X11/macOS pointer-event commits revalidate the latch
    //    against the torn window's FINAL position (a WM-cancelled move
    //    snapped it back to its origin, off the band).
    assert!(
        src.contains("let commit = self.revalidate_dock_latch(ws);"),
        "heuristic commits must revalidate the latch"
    );
    // 7. Manual-follow listens only to the capture holder (stale
    //    tracking must not hijack every window's cursor stream).
    assert!(
        src.contains("if td.native || td.carrier != ws.seq {"),
        "manual-follow must be carrier-gated"
    );
    // 8. A window that dies mid-drag takes its tracking with it.
    assert!(
        src.contains(".is_some_and(|t| t.seq == seq || t.carrier == seq)"),
        "finish_window_dispatch must abandon tracking for a dying torn/carrier window"
    );
    // 9. The dock hit-test runs from the LIVE cursor when a query source
    //    exists (GetCursorPos / X11 QueryPointer). The frame+grab
    //    approximation alone misses the band by the WM's anchor drift
    //    (measured 55-86px under Mutter).
    assert!(
        src.contains("let cursor = real.unwrap_or(approx);"),
        "the Moved-driven hit-test must prefer the live cursor"
    );
    // 10. The frozen-drag rescue tick is wired into about_to_wait. Without
    //     it, a silent native handoff (or a pointer past the carrier's
    //     bounds) leaves the torn window frozen mid-air with no path to
    //     carry or dock it.
    assert!(
        src.contains("let torn_tick_wait = self.torn_drag_pointer_tick(event_loop);"),
        "about_to_wait must run the frozen-drag rescue tick"
    );
    // 11. The heuristic pointer-event commits distinguish an Esc-cancel
    //     from a real drop by PHYSICAL button state (the X11 QueryPointer
    //     mask / Windows GetAsyncKeyState). Position heuristics cannot,
    //     because Esc moves the frame, never the pointer, and the WM's
    //     restore `Moved` re-syncs any frame-anchor estimate before the
    //     commit event arrives.
    assert!(
        src.contains("if primary_button_physically_held() {\n            return false;\n        }"),
        "revalidate_dock_latch must key Esc-cancel detection off physical button state"
    );
}

/// Drift guard: the Terminator-parity detachable-tabs setting is
/// a real runtime switch, not just a parsed compatibility key.
#[test]
fn detachable_tabs_config_gates_all_detach_paths() {
    let src = production_source();
    assert!(
        src.contains("if !self.cfg.detachable_tabs {\n                    log::info!(\"move_tab_to_new_window ignored because detachable-tabs = false\");"),
        "keyboard/palette move_tab_to_new_window must honor detachable-tabs = false"
    );
    let tear_off = src
        .split_once("fn maybe_tear_off(")
        .and_then(|(_, rest)| rest.split_once(") -> bool {"))
        .expect("maybe_tear_off")
        .1;
    assert!(
        tear_off
            .trim_start()
            .starts_with("if !self.cfg.detachable_tabs {"),
        "mouse threshold tear-off must honor detachable-tabs = false"
    );
    assert!(
        src.contains(
            "if self.cfg.detachable_tabs {\n                                    ws.detach_drag ="
        ),
        "tab press must not arm the cross-window detach FSM when disabled"
    );
    assert!(
        src.contains(
            "if self.cfg.detachable_tabs\n                        && dropped_outside\n                        && wayland\n                        && ws.mux.tabs.len() > 1\n                    {"
        ),
        "Wayland release-only tear-off must honor detachable-tabs = false"
    );
}

/// Drift guard: the drag strip and `tab_bar()`'s segment layout
/// share this width, so a tab can be dragged all the way to the last slot.
#[test]
fn tab_segment_strip_width_excludes_both_buttons() {
    use super::tab_segment_strip_width;
    // plus_w = arrow_w = height = 24, surface 800 → strip excludes BOTH
    // buttons (the `▾ +` pair), 800 - 48 = 752, not 800 - 24 = 776.
    assert_eq!(tab_segment_strip_width(800.0, 24.0, 24.0), 752.0);
    // No dropdown (vertical bar): arrow_w = 0 → only `+` excluded.
    assert_eq!(tab_segment_strip_width(800.0, 24.0, 0.0), 776.0);
    // Degenerate narrow bar floors at plus_w so `+` always has room.
    assert_eq!(tab_segment_strip_width(20.0, 24.0, 24.0), 24.0);
}

#[test]
fn next_context_menu_highlight_skips_separators_and_disabled() {
    use super::{ContextMenuItem, next_context_menu_highlight};
    use kettle_config::Action;
    // Layout: [Copy(disabled), Paste, Separator, SplitRight,
    //          ClosePane(disabled), Separator, NewTab]
    // Indices:  0               1      2          3
    //           4                       5          6
    // Enabled-only walk should land 1 → 3 → 6 → 1 (wrap).
    let items = vec![
        ContextMenuItem::Item {
            label: "Copy",
            action: Action::Copy,
            enabled: false,
        },
        ContextMenuItem::Item {
            label: "Paste",
            action: Action::Paste,
            enabled: true,
        },
        ContextMenuItem::Separator,
        ContextMenuItem::Item {
            label: "Split Right",
            action: Action::SplitRight,
            enabled: true,
        },
        ContextMenuItem::Item {
            label: "Close Pane",
            action: Action::ClosePane,
            enabled: false,
        },
        ContextMenuItem::Separator,
        ContextMenuItem::Item {
            label: "New Tab",
            action: Action::NewTab,
            enabled: true,
        },
    ];
    // ↓ from Paste (1) skips Separator(2) → SplitRight(3).
    assert_eq!(next_context_menu_highlight(&items, 1, 1), 3);
    // ↓ from SplitRight(3) skips disabled ClosePane(4) + Separator
    // (5) → NewTab(6).
    assert_eq!(next_context_menu_highlight(&items, 3, 1), 6);
    // ↓ from NewTab(6) wraps past disabled Copy(0) → Paste(1).
    assert_eq!(next_context_menu_highlight(&items, 6, 1), 1);
    // ↑ from Paste(1) wraps past disabled Copy(0) → NewTab(6).
    assert_eq!(next_context_menu_highlight(&items, 1, -1), 6);
    // No enabled items at all — return `current` unchanged so the
    // caller doesn't crash. (Defensive — caller shouldn't open an
    // empty menu, but the guard exists.)
    let all_disabled = vec![ContextMenuItem::Separator];
    assert_eq!(next_context_menu_highlight(&all_disabled, 0, 1), 0);
}

/// A card menu's rows are ordinary rows: dispatchable, clicked with what
/// the menu captured, reachable by mnemonic and by typing, and measured
/// by their label.
#[test]
fn a_card_menu_row_is_an_ordinary_menu_row() {
    use super::{
        ContextMenuClick, ContextMenuItem, MediaMenuAction, MediaTarget, assign_mnemonics,
        context_menu_item_columns, item_is_dispatchable, item_to_click, typeahead_match,
    };
    let target = MediaTarget {
        pane: 3,
        nonce: kettle_core::InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap(),
        item: 9,
        generation: 2,
    };
    let rows = [
        ContextMenuItem::Media {
            label: "Open",
            target,
            action: MediaMenuAction::View,
        },
        ContextMenuItem::Media {
            label: "Open in Preview",
            target,
            action: MediaMenuAction::OpenOutside(crate::media::Outside::Image(
                crate::media::Viewer::Preview,
            )),
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        assert!(item_is_dispatchable(row));
        let Some(ContextMenuClick::Media {
            target: clicked,
            action,
        }) = item_to_click(row, index)
        else {
            panic!("row {index} is not a media click");
        };
        assert_eq!(clicked, target);
        assert_eq!(
            action,
            [
                MediaMenuAction::View,
                MediaMenuAction::OpenOutside(crate::media::Outside::Image(
                    crate::media::Viewer::Preview,
                ))
            ][index]
        );
    }
    assert!(context_menu_item_columns(&rows[1], "") >= "Open in Preview".len());
    let mnemonics = assign_mnemonics(&rows, &[]);
    assert!(mnemonics.iter().all(Option::is_some), "{mnemonics:?}");
    assert_ne!(mnemonics[0].map(|m| m.1), mnemonics[1].map(|m| m.1));
    assert_eq!(typeahead_match(&rows, "open in"), Some(1));
}

/// The shared row→click mapper must recognise EVERY dispatchable row type,
/// so the keyboard Enter / Space + mnemonic paths reach the same rows the
/// mouse hit-test does. A row type it misses is a keyboard dead-end.
#[test]
fn item_to_click_maps_every_dispatchable_row_type() {
    use super::{ContextMenuClick, ContextMenuItem, UrlHow, item_to_click};
    use kettle_config::Action;

    // Enabled Item / DynamicItem → Action.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: true
            },
            0
        ),
        Some(ContextMenuClick::Action(_))
    ));
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::DynamicItem {
                label: "Bold".into(),
                action: Action::Copy,
                enabled: true
            },
            0
        ),
        Some(ContextMenuClick::Action(_))
    ));
    // Submenu → DrillIntoSubmenu(idx) — carries the row index so the
    // dispatcher knows which level to descend into.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::Submenu {
                label: "Theme".into(),
                items: vec![]
            },
            7
        ),
        Some(ContextMenuClick::DrillIntoSubmenu(7))
    ));
    // Lua item → LuaMenuItem.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::LuaItem {
                label: "Plugin".into(),
                lua_idx: 3
            },
            0
        ),
        Some(ContextMenuClick::LuaMenuItem(3))
    ));
    // Config command → ConfigCommand.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::ConfigItem {
                label: "Clear".into(),
                command: "clear".into(),
                new_pane: false,
            },
            0
        ),
        Some(ContextMenuClick::ConfigCommand(_))
    ));
    // Profile choices → SetProfile.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::ProfileChoice {
                label: "work".into(),
                profile: "work".into()
            },
            0
        ),
        Some(ContextMenuClick::SetProfile(_))
    ));
    // New-tab ▾ shell choice → NewTabWithArgv.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::NewTabShell {
                label: "bash".into(),
                argv: vec!["bash".into()]
            },
            0
        ),
        Some(ContextMenuClick::NewTabWithArgv(_))
    ));
    // URL-aware rows → Url { how } carrying the captured
    // address, for both the Open and Copy flavors.
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::UrlItem {
                label: "Open Link",
                url: "https://example.com".into(),
                how: UrlHow::Open,
                pane: Some(7),
            },
            0
        ),
        // The pane the link came from rides along to the click.
        Some(ContextMenuClick::Url {
            how: UrlHow::Open,
            pane: Some(7),
            ..
        })
    ));
    assert!(matches!(
        item_to_click(
            &ContextMenuItem::UrlItem {
                label: "Copy Link Address",
                url: "https://example.com".into(),
                how: UrlHow::Copy,
                pane: Some(7),
            },
            0
        ),
        Some(ContextMenuClick::Url {
            how: UrlHow::Copy,
            ..
        })
    ));
    // Non-dispatchable rows → None (disabled item + separator).
    assert!(
        item_to_click(
            &ContextMenuItem::Item {
                label: "Copy",
                action: Action::Copy,
                enabled: false
            },
            0
        )
        .is_none()
    );
    assert!(item_to_click(&ContextMenuItem::Separator, 0).is_none());
}

/// Profile choice leaves must be keyboard-navigable, otherwise drilling
/// into a Profile ▸ submenu leaves ↑/↓ unable to land on any row.
#[test]
fn profile_choices_are_keyboard_navigable() {
    use super::{ContextMenuItem, item_is_dispatchable};
    assert!(item_is_dispatchable(&ContextMenuItem::ProfileChoice {
        label: "work".into(),
        profile: "work".into()
    }));
}

#[test]
fn submenu_transitions_reclamp_the_new_panel() {
    let src = super::production_source();
    let call = "self.reclamp_context_menu(ws, (width as f32, height as f32));";
    let drill = src
        .split("ContextMenuClick::DrillIntoSubmenu(idx) => {")
        .nth(1)
        .unwrap()
        .split("ContextMenuClick::Action(action) =>")
        .next()
        .unwrap();
    assert!(drill.contains(call));
    let back = src
        .split("fn context_menu_back(")
        .nth(1)
        .unwrap()
        .split("fn context_menu_activate(")
        .next()
        .unwrap();
    assert!(back.find(call).unwrap() < back.find("return;").unwrap());
}

#[test]
fn clamp_context_menu_anchor_keeps_panel_on_screen() {
    use super::clamp_context_menu_anchor;
    // 800x600 surface, 200x300 panel — anchors inside the safe
    // region pass through unchanged.
    assert_eq!(
        clamp_context_menu_anchor((100.0, 100.0), (200.0, 300.0), (800.0, 600.0)),
        (100.0, 100.0)
    );
    // Right-click near the bottom-right corner gets clamped so the
    // panel still fits with the 4-px margin (max_x = 800 - 200 - 4
    // = 596, max_y = 600 - 300 - 4 = 296).
    assert_eq!(
        clamp_context_menu_anchor((780.0, 580.0), (200.0, 300.0), (800.0, 600.0)),
        (596.0, 296.0)
    );
    // Right-click in the top-left corner clamps up against the
    // 4-px screen-edge margin so the panel doesn't glue to the
    // bezel.
    assert_eq!(
        clamp_context_menu_anchor((0.0, 0.0), (200.0, 300.0), (800.0, 600.0)),
        (4.0, 4.0)
    );
    // Pathological: panel larger than the surface — anchor clamps
    // to the 4-px margin without producing a NaN / negative.
    let (x, y) = clamp_context_menu_anchor((100.0, 100.0), (2000.0, 2000.0), (800.0, 600.0));
    assert!(x >= 4.0 && y >= 4.0);

    // An anchor that was valid on the old surface must be re-clamped when
    // a DPI transition or user resize shrinks the window underneath an
    // already-open menu.
    assert_eq!(
        clamp_context_menu_anchor((596.0, 296.0), (200.0, 300.0), (420.0, 320.0)),
        (216.0, 16.0)
    );
}

#[test]
fn context_menu_anchor_scales_once_across_dpi_then_reclamps() {
    use super::{clamp_context_menu_anchor, scale_context_menu_anchor};

    let scaled = scale_context_menu_anchor((100.0, 50.0), 1.0, 1.5);
    assert_eq!(scaled, (150.0, 75.0));
    assert_eq!(
        clamp_context_menu_anchor(scaled, (200.0, 100.0), (320.0, 180.0)),
        (116.0, 75.0),
        "the logical anchor should scale before the new physical surface clamps it"
    );
    assert_eq!(
        scale_context_menu_anchor(scaled, 1.5, 1.5),
        scaled,
        "recording the new scale must make later same-DPI resizes idempotent"
    );
    assert_eq!(
        scale_context_menu_anchor((12.0, 34.0), 0.0, 2.0),
        (12.0, 34.0),
        "invalid prior scale must fail closed"
    );
    assert_eq!(
        scale_context_menu_anchor((12.0, 34.0), 1.0, f64::NAN),
        (12.0, 34.0),
        "invalid current scale must fail closed"
    );
}

#[test]
fn context_menu_panel_clamp_makes_oversized_panel_fit_with_margin() {
    use super::{clamp_context_menu_anchor, clamp_context_menu_panel};

    let surface = (800.0, 600.0);
    assert_eq!(
        clamp_context_menu_panel((200.0, 300.0), surface),
        (200.0, 300.0),
        "an already-fitting panel must keep its natural size"
    );
    let panel = clamp_context_menu_panel((2000.0, 2000.0), surface);
    assert_eq!(panel, (792.0, 592.0));
    let anchor = clamp_context_menu_anchor((780.0, 580.0), panel, surface);
    assert_eq!(anchor, (4.0, 4.0));
    assert!(anchor.0 + panel.0 <= surface.0 - 4.0);
    assert!(anchor.1 + panel.1 <= surface.1 - 4.0);
}

#[test]
fn context_menu_requires_one_real_row_and_content_column() {
    let cell = (9.6, 18.25);
    let threshold = (
        8.0 + kettle_render::menu::H_PAD + cell.0,
        8.0 + cell.1 + kettle_render::menu::ROW_PAD,
    );
    assert!(context_menu_surface_can_fit_row(threshold, cell));
    assert!(!context_menu_surface_can_fit_row(
        (threshold.0 - 0.01, threshold.1),
        cell
    ));
    assert!(!context_menu_surface_can_fit_row(
        (threshold.0, threshold.1 - 0.01),
        cell
    ));
    assert!(!context_menu_surface_can_fit_row(
        (f32::NAN, threshold.1),
        cell
    ));
}

#[test]
fn oversized_menu_hint_is_removed_before_label_ellipsis() {
    use unicode_width::UnicodeWidthStr as _;

    let mut row = ContextMenuRow {
        label: "Visible label".to_string(),
        separator: false,
        enabled: true,
        hint: "Ctrl+".repeat(1000),
    };
    fit_context_menu_row(&mut row, 5);
    assert!(row.hint.is_empty());
    assert!(!row.label.is_empty());
    assert!(row.label.width() <= 5);
}

#[test]
fn context_menu_width_covers_every_rendered_row_shape() {
    use unicode_width::UnicodeWidthStr as _;

    let submenu = ContextMenuItem::Submenu {
        label: "主题".into(),
        items: vec![],
    };
    assert_eq!(
        context_menu_item_columns(&submenu, "ignored"),
        "主题 ▸".width()
    );

    let profile = ContextMenuItem::ProfileChoice {
        label: "👩‍💻 work".into(),
        profile: "work".into(),
    };
    assert_eq!(
        context_menu_item_columns(&profile, "ignored"),
        "👩‍💻 work".width()
    );

    let hinted = ContextMenuItem::Item {
        label: "Copy",
        action: Action::Copy,
        enabled: true,
    };
    assert_eq!(
        context_menu_item_columns(&hinted, "Ctrl+界"),
        "Copy".width() + "Ctrl+界".width() + 2
    );
}

#[test]
fn context_menu_label_ellipsis_is_bounded_and_utf8_safe() {
    use super::ellipsize_menu_label;
    use unicode_width::UnicodeWidthStr as _;

    assert_eq!(ellipsize_menu_label("Short", 10), "Short");
    assert_eq!(ellipsize_menu_label("abcdef", 5), "abcd…");
    assert_eq!(ellipsize_menu_label("éclair", 4), "écl…");
    assert_eq!(ellipsize_menu_label("👩‍💻abc", 3), "👩‍💻…");
    assert_eq!(ellipsize_menu_label("anything", 1), "…");
    assert_eq!(ellipsize_menu_label("anything", 0), "");
    for budget in 0..=8 {
        assert!(ellipsize_menu_label("multi-byte 中文 label", budget).width() <= budget);
    }
}

#[test]
fn tab_close_hover_icon_overrides_chrome_default() {
    use super::{chrome_cursor_icon, tab_close_hover_icon};
    use winit::window::CursorIcon;
    // Not over a close button → no override; chrome decision wins.
    assert_eq!(tab_close_hover_icon(false), None);
    let chrome = chrome_cursor_icon(true, false);
    // Hover a close ✕ → Pointer (the browser-tab convention).
    assert_eq!(tab_close_hover_icon(true), Some(CursorIcon::Pointer));
    // Compose: close-hover wins over the chrome Default — once
    // the cursor lands on a clickable affordance we want the
    // hand, not the arrow.
    assert_eq!(
        tab_close_hover_icon(true).or(chrome),
        Some(CursorIcon::Pointer)
    );
}

/// The tab-drag cursor tracks the detach FSM (Grab while armed, Grabbing
/// while dragging) and outranks every other icon rule so it can't flicker
/// mid-drag.
#[test]
fn tab_drag_cursor_icon_by_drag_state() {
    use super::tab_drag_cursor_icon;
    use crate::detach::DragState;
    use winit::window::CursorIcon;
    assert_eq!(tab_drag_cursor_icon(&DragState::Idle), None);
    assert_eq!(
        tab_drag_cursor_icon(&DragState::ArmedInside { tab_idx: 0 }),
        Some(CursorIcon::Grab)
    );
    assert_eq!(
        tab_drag_cursor_icon(&DragState::DraggingInside { tab_idx: 1 }),
        Some(CursorIcon::Grabbing)
    );
    assert_eq!(
        tab_drag_cursor_icon(&DragState::DraggingOutside { tab_idx: 2 }),
        Some(CursorIcon::Grabbing)
    );
    // Compose: a live drag outranks the close-✕ Pointer (the pointer
    // can transiently cross another tab's close zone mid-drag).
    assert_eq!(
        tab_drag_cursor_icon(&DragState::DraggingInside { tab_idx: 0 })
            .or(super::tab_close_hover_icon(true)),
        Some(CursorIcon::Grabbing)
    );
    // Idle defers — the close-hover rule gets its turn.
    assert_eq!(
        tab_drag_cursor_icon(&DragState::Idle).or(super::tab_close_hover_icon(true)),
        Some(CursorIcon::Pointer)
    );
}

#[test]
fn osc52_copy_scrubs_complete_cluster_before_its_byte_limit() {
    let marker: String = std::iter::once('\u{10eeee}')
        .chain(std::iter::repeat_n('\u{0305}', 8))
        .collect();
    assert_eq!(
        super::prepare_osc52_copy(format!("AAAAAA{marker}Z"), 16).unwrap(),
        "AAAAAA Z"
    );
    assert_eq!(super::prepare_osc52_copy("é界tail".into(), 4).unwrap(), "é");
    assert_eq!(
        super::prepare_osc52_copy(format!("A{marker}B"), 0).unwrap(),
        ""
    );
}

#[test]
fn osc52_da1_copy_capability_combines_policy_and_clipboard_availability() {
    use kettle_config::Osc52;

    let available = super::osc52_copy_is_available;
    assert!(!available(Osc52::Off, true));
    assert!(!available(Osc52::Paste, true));
    assert!(!available(Osc52::Copy, false));
    assert!(!available(Osc52::Both, false));
    assert!(available(Osc52::Copy, true));
    assert!(available(Osc52::Both, true));
}

#[test]
fn clamp_osc52_bounds_and_keeps_char_boundary() {
    use super::clamp_osc52;
    assert_eq!(clamp_osc52("hello", 1024), "hello"); // under cap → as-is
    assert_eq!(clamp_osc52("abcdef", 3), "abc"); // truncated
    // Never splits a multibyte char: "é" is 2 bytes; cap 1 → empty.
    assert_eq!(clamp_osc52("é", 1), "");
    assert_eq!(clamp_osc52("aé", 2), "a");
    assert_eq!(clamp_osc52("", 0), "");
    // Result always within the byte cap.
    let big = "x".repeat(5000);
    assert!(clamp_osc52(&big, 1000).len() <= 1000);
}

#[test]
fn window_title_formats_and_falls_back() {
    use super::{window_title, window_title_with_home};
    let dflt = "{title} — kettle";
    assert_eq!(
        window_title(dflt, "vim README.md", "", 1),
        "vim README.md — kettle"
    );
    assert_eq!(window_title(dflt, "  spaced  ", "", 1), "spaced — kettle");
    // Empty / placeholder titles with no cwd → just the app name (the
    // template never produces a stub like " — kettle").
    assert_eq!(window_title(dflt, "", "", 1), "kettle");
    assert_eq!(window_title(dflt, "   ", "", 1), "kettle");
    assert_eq!(window_title(dflt, "kettle", "", 1), "kettle");
    // Empty / placeholder title *with* cwd → cwd basename fills in
    // (the cwd-basename tab-title fallback, same shape for the window title).
    assert_eq!(
        window_title(dflt, "", "/home/k/Repos/kettle", 1),
        "kettle — kettle",
        "cwd basename `kettle` fills the {{title}} slot pre-OSC 2"
    );
    assert_eq!(
        window_title(dflt, "kettle", "/home/k/Documents", 1),
        "Documents — kettle"
    );
    assert_eq!(
        window_title_with_home(
            dflt,
            "..PI-1/platform",
            "/home/k/Repos/SPI-1/platform",
            Some("/home/k"),
            1
        ),
        "~/Repos/SPI-1/platform — kettle",
        "truncated cwd-rendered shell titles recover the full cwd for the OS title"
    );
    // Custom templates can use {tab} and {cwd}.
    let t = "[{tab}] {title} ({cwd})";
    assert_eq!(
        window_title(t, "vim", "/home/k/Repos/kettle", 2),
        "[2] vim (/home/k/Repos/kettle)"
    );
    assert_eq!(
        window_title_with_home(
            t,
            "...PI-1/platform",
            "/home/k/Repos/SPI-1/platform",
            Some("/home/k"),
            2
        ),
        "[2] ~/Repos/SPI-1/platform (~/Repos/SPI-1/platform)"
    );
}

#[test]
fn record_dir_tilde_is_expanded_to_home() {
    use super::expand_user_path_with;
    use std::path::{Path, PathBuf};
    let home = Some("/home/k".to_string());
    // `~` and `~/...` expand; a bare-relative or absolute path is untouched.
    assert_eq!(
        expand_user_path_with(Path::new("~/.cache/kettle/records"), home.clone()),
        PathBuf::from("/home/k/.cache/kettle/records")
    );
    assert_eq!(
        expand_user_path_with(Path::new("~"), home.clone()),
        PathBuf::from("/home/k")
    );
    assert_eq!(
        expand_user_path_with(Path::new("/abs/records"), home.clone()),
        PathBuf::from("/abs/records")
    );
    // `~user` (no slash) is NOT expanded — only `~`/`~/`.
    assert_eq!(
        expand_user_path_with(Path::new("~bob/x"), home),
        PathBuf::from("~bob/x")
    );
    // Without a home dir, expansion no-ops and the caller sees the literal
    // path. No stray `~` dir under CWD is created here.
    assert_eq!(
        expand_user_path_with(Path::new("~/x"), None),
        PathBuf::from("~/x")
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

#[test]
fn get_state_reports_desired_window_title() {
    let src = production_source();
    let body = src
        .split("fn ctl_get_state")
        .nth(1)
        .and_then(|s| s.split("fn ctl_list_tabs").next())
        .expect("ctl_get_state body present");
    assert!(
        body.contains("\"window_title\": self.desired_window_title(ws),"),
        "get_state must expose the same native title string that sync_window_title sets"
    );
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
fn selection_kind_maps_clicks_and_alt() {
    // Double/triple click → word/line regardless of Alt.
    assert!(matches!(selection_kind(2, false), SelectionType::Semantic));
    assert!(matches!(selection_kind(2, true), SelectionType::Semantic));
    assert!(matches!(selection_kind(3, false), SelectionType::Lines));
    // Single click: plain drag, or Alt → rectangular block.
    assert!(matches!(selection_kind(1, false), SelectionType::Simple));
    assert!(matches!(selection_kind(1, true), SelectionType::Block));
    // A 0 click-count (no cell) still behaves like a single click.
    assert!(matches!(selection_kind(0, false), SelectionType::Simple));
    assert!(matches!(selection_kind(0, true), SelectionType::Block));
}

#[test]
fn trigger_command_slots_are_bounded_and_released() {
    use super::TriggerCommandSlot;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    let active = AtomicUsize::new(0);
    let mut slots: Vec<_> = (0..32)
        .map(|_| TriggerCommandSlot::acquire(&active).unwrap())
        .collect();
    assert!(TriggerCommandSlot::acquire(&active).is_none());
    drop(slots.pop());
    assert!(TriggerCommandSlot::acquire(&active).is_some());
    drop(slots);
    assert_eq!(active.load(Relaxed), 0);
}

#[cfg(unix)]
#[test]
fn trigger_command_reaps_exited_child() {
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pid");
    super::spawn_trigger_command(&[
        "/bin/sh".into(),
        "-c".into(),
        "printf '%s' \"$$\" > \"$1\"".into(),
        "kettle-trigger-test".into(),
        path.to_str().unwrap().into(),
    ]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let pid: libc::pid_t = loop {
        if let Some(pid) = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.parse().ok())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "trigger did not publish its pid");
        std::thread::sleep(Duration::from_millis(10));
    };
    let reaped = loop {
        if unsafe { libc::kill(pid, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !reaped {
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    }
    assert!(reaped, "the completed trigger child was left unreaped");
}

#[test]
fn match_triggers_finds_pattern_anywhere_in_text() {
    // The matching engine fires on the first regex hit, returns its
    // action, and no-ops when nothing matches. Patterns are unanchored.
    use super::{compile_triggers, match_triggers};
    use kettle_config::{OutputTrigger, TriggerAction};
    let cfg = vec![
        OutputTrigger {
            pattern: r"error.*panic".into(),
            action: TriggerAction::Urgency,
        },
        OutputTrigger {
            pattern: r"(BUILD SUCCESSFUL|FAILED)".into(),
            action: TriggerAction::Urgency,
        },
    ];
    let compiled = compile_triggers(&cfg);
    assert_eq!(compiled.len(), 2);

    // Bare match anywhere in the snapshot.
    assert!(match_triggers("thread 'main' panicked: error panic", &compiled).is_some());
    // Alternation pattern still matches both branches.
    assert!(match_triggers("BUILD SUCCESSFUL in 1m23s", &compiled).is_some());
    assert!(match_triggers("BUILD FAILED with 3 errors", &compiled).is_some());
    // Non-matching text: no fire.
    assert!(match_triggers("just normal output, nothing here", &compiled).is_none());
    assert!(match_triggers("", &compiled).is_none());

    // Empty trigger set never fires.
    assert!(match_triggers("error.*panic any text", &[]).is_none());

    // Invalid regex is dropped at compile time (warn-logged); the
    // remaining valid ones still work.
    let cfg_mixed = vec![
        OutputTrigger {
            pattern: r"valid_pattern".into(),
            action: TriggerAction::Urgency,
        },
        OutputTrigger {
            pattern: r"unclosed[".into(), // invalid regex
            action: TriggerAction::Urgency,
        },
    ];
    let compiled_mixed = compile_triggers(&cfg_mixed);
    assert_eq!(
        compiled_mixed.len(),
        1,
        "invalid regex should be dropped at compile time"
    );
    assert!(match_triggers("here is the valid_pattern token", &compiled_mixed).is_some());
}

/// Viewport snapshots join rows with '\n', so `^` and `$` must anchor to a
/// row, not to the whole snapshot.
#[test]
fn trigger_anchors_match_each_row() {
    use super::{compile_triggers, match_triggers};
    use kettle_config::{OutputTrigger, TriggerAction};
    let compiled = compile_triggers(&[OutputTrigger {
        pattern: r"^Build failed: (.+)$".into(),
        action: TriggerAction::Urgency,
    }]);
    let snapshot = "$ make\nBuild failed: missing header\n$ \n";
    assert!(match_triggers(snapshot, &compiled).is_some());
    assert!(match_triggers("$ make\nok: Build failed: no\n", &compiled).is_none());
}

/// Terminator `run_cmd_on_match.py` parity: a trigger's capture groups
/// substitute into the spawned argv. `{0}` is the whole match and `{1}`…
/// are numbered groups. An out-of-range reference stays LITERAL so the
/// typo stays visible. Substitution can change a VALUE but never add
/// arguments.
#[test]
fn trigger_capture_groups_substitute_into_argv() {
    use super::{compile_triggers, match_triggers};
    use kettle_config::{OutputTrigger, TriggerAction};
    let cfg = vec![OutputTrigger {
        pattern: r"ERROR in ([\w./-]+) line (\d+)".into(),
        action: TriggerAction::RunCommand(vec![
            "notify-send".into(),
            "build error".into(),
            "{1}:{2}".into(),
            "match={0}".into(),
            "{9}".into(), // out of range → stays literal
        ]),
    }];
    let compiled = compile_triggers(&cfg);
    let action =
        match_triggers("xx ERROR in src/main.rs line 42 yy", &compiled).expect("trigger fires");
    let TriggerAction::RunCommand(argv) = action else {
        panic!("expected RunCommand");
    };
    assert_eq!(
        argv,
        vec![
            "notify-send".to_string(),
            "build error".to_string(),
            "src/main.rs:42".to_string(),
            "match=ERROR in src/main.rs line 42".to_string(),
            "{9}".to_string(),
        ]
    );
    // Argv arity is unchanged — a match can never ADD arguments.
    assert_eq!(argv.len(), 5);
    // A capture whose MATCHED TEXT contains a placeholder must not expand
    // again (single template pass, no re-scan of substituted output).
    let nested = vec![OutputTrigger {
        pattern: r"E:(\S+):(\S+)".into(),
        action: TriggerAction::RunCommand(vec!["log".into(), "{1}".into()]),
    }];
    let action =
        match_triggers("E:{2}:secret", &compile_triggers(&nested)).expect("nested trigger fires");
    let TriggerAction::RunCommand(argv) = action else {
        panic!("expected RunCommand");
    };
    assert_eq!(
        argv[1], "{2}",
        "substituted text must be emitted verbatim, never re-expanded"
    );
    // Urgency triggers are untouched by capture plumbing.
    let urgency = vec![OutputTrigger {
        pattern: r"(panic)".into(),
        action: TriggerAction::Urgency,
    }];
    assert!(matches!(
        match_triggers("a panic b", &compile_triggers(&urgency)),
        Some(TriggerAction::Urgency)
    ));
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

/// Regression coverage for confirmation-dialog keyboard navigation.
#[test]
fn confirm_dialog_keypress_walks_state_machine() {
    use super::{ConfirmKey, ConfirmKeyResult, confirm_dialog_keypress};
    // Escape always cancels.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Escape),
        ConfirmKeyResult::Cancel
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Escape),
        ConfirmKeyResult::Cancel
    );
    // Enter activates the FOCUSED button (buttons are
    // `[Cancel, Confirm]`). Focused on Cancel (idx 0) → Cancel; focused on
    // Confirm (idx 1, the last) → Confirm. This must match the highlighted
    // button so Enter on the safe default doesn't fire the destructive action.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Enter),
        ConfirmKeyResult::Cancel,
        "Enter on the highlighted Cancel button must cancel, not confirm"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Enter),
        ConfirmKeyResult::Confirm,
        "Enter on the Confirm button confirms"
    );
    // Tab cycles forward with wrap.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Tab),
        ConfirmKeyResult::Move(1)
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Tab),
        ConfirmKeyResult::Move(0),
        "Tab wraps at end"
    );
    // Shift+Tab cycles backward with wrap.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::ShiftTab),
        ConfirmKeyResult::Move(1),
        "Shift+Tab wraps at start"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::ShiftTab),
        ConfirmKeyResult::Move(0)
    );
    // Left at idx 0 is a no-op (Ignore — caller suppresses).
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Left),
        ConfirmKeyResult::Ignore
    );
    // Left at idx 1 moves to 0.
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Left),
        ConfirmKeyResult::Move(0)
    );
    // Right at last idx is a no-op.
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Right),
        ConfirmKeyResult::Ignore
    );
    // Right at idx 0 with 2 buttons moves to 1.
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Right),
        ConfirmKeyResult::Move(1)
    );
    // Defensive: 0 buttons → any key cancels.
    assert_eq!(
        confirm_dialog_keypress(0, 0, ConfirmKey::Enter),
        ConfirmKeyResult::Cancel
    );
    // Single-button dialog: Tab cycles to itself (no-op move).
    assert_eq!(
        confirm_dialog_keypress(0, 1, ConfirmKey::Tab),
        ConfirmKeyResult::Move(0)
    );
}

/// With `vim-menu-nav` on, `y` confirms and `n` cancels regardless of
/// focus. They answer the QUESTION, unlike Enter, which fires the focused
/// button.
#[test]
fn confirm_dialog_y_and_n_answer_directly() {
    use super::{ConfirmKey, ConfirmKeyResult, confirm_dialog_keypress};
    assert_eq!(
        confirm_dialog_keypress(0, 2, ConfirmKey::Yes),
        ConfirmKeyResult::Confirm,
        "y must confirm even with Cancel focused"
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::Yes),
        ConfirmKeyResult::Confirm
    );
    assert_eq!(
        confirm_dialog_keypress(1, 2, ConfirmKey::No),
        ConfirmKeyResult::Cancel,
        "n must cancel even with Confirm focused"
    );
    // Degenerate zero-button dialog still cancels safely.
    assert_eq!(
        confirm_dialog_keypress(0, 0, ConfirmKey::Yes),
        ConfirmKeyResult::Cancel
    );
}

/// The title-edit overlay must be wide enough to show what you are typing.
///
/// Under a vertical tab bar, the first tab SEGMENT's rect is only
/// `tab-bar-width` wide (default 180px). The renderer fits
/// `"  ✎ {label} {input}_   (Enter apply · Esc cancel)"` into
/// `overlay_label_cols(width, cw)`; at 180px and an 8px cell that is 21
/// columns, and the trailing hint alone is 30. An overlay placed in that
/// rect shows zero input columns, no caret, and no horizontal scroll, yet
/// Enter still commits the invisible buffer. For `EditPaneGroup` that
/// buffer is the broadcast-group name, which decides which panes receive
/// later keystrokes.
#[test]
fn title_edit_overlay_is_wide_enough_to_show_the_input() {
    use super::{ContentBands, content_rect_for_with_strip, title_edit_rect_for};
    use kettle_config::{StatusBarMode, TabBarPos};

    let sw = 1512.0f32;
    let sh = 945.0f32;
    let row_h = 24.0f32;
    let strip_w = 180.0f32;
    let content = content_rect_for_with_strip(
        (sw as u32, sh as u32),
        ContentBands {
            tab_bar_h: row_h,
            vertical_title_edit_h: row_h,
            ..ContentBands::default()
        },
        TabBarPos::Left,
        StatusBarMode::Off,
        strip_w,
    );
    // A vertical strip: tall bar, tab-bar-width wide. The overlay occupies
    // the horizontal band immediately above terminal content.
    let vertical = title_edit_rect_for(true, 0.0, sh, content.1, sw, sh, row_h);
    assert_eq!(
        vertical.2, sw,
        "vertical tab bar: overlay must span the window, not the 180px strip"
    );
    assert_eq!(
        vertical.3, row_h,
        "vertical tab bar: overlay is one row, not the full strip height"
    );

    let intersects = vertical.0 < content.0 + content.2
        && content.0 < vertical.0 + vertical.2
        && vertical.1 < content.1 + content.3
        && content.1 < vertical.1 + vertical.3;
    assert!(
        !intersects,
        "the full-width title input must occupy reserved chrome, not cover terminal content: \
             overlay={vertical:?}, content={content:?}"
    );

    // A top status bar keeps its own band. The title input starts after it,
    // and terminal content starts after both pieces of chrome.
    let status_h = 22.0;
    let content_below_status = content_rect_for_with_strip(
        (sw as u32, sh as u32),
        ContentBands {
            tab_bar_h: row_h,
            status_bar_h: status_h,
            vertical_title_edit_h: row_h,
        },
        TabBarPos::Right,
        StatusBarMode::Top,
        strip_w,
    );
    let below_status = title_edit_rect_for(true, 0.0, sh, content_below_status.1, sw, sh, row_h);
    assert_eq!(below_status.1, status_h);
    assert_eq!(content_below_status.1, status_h + row_h);
    assert!(below_status.1 + below_status.3 <= content_below_status.1);

    // Horizontal is unchanged: the overlay sits on the bar itself.
    let horizontal = title_edit_rect_for(false, 12.0, 28.0, 0.0, sw, sh, row_h);
    assert_eq!(horizontal, (0.0, 12.0, sw, 28.0));

    // No tab bar at all: a single row at the top.
    let none = title_edit_rect_for(false, 0.0, 0.0, 0.0, sw, sh, row_h);
    assert_eq!(none, (0.0, 0.0, sw, row_h));
}

#[test]
fn confirm_dialog_button_hit_tracks_right_aligned_button_row() {
    use super::{ConfirmButton, confirm_dialog_button_hit};

    let buttons = vec![
        ConfirmButton::Cancel,
        ConfirmButton::Confirm {
            label: "Close".into(),
            destructive: true,
        },
    ];
    let sw = 800.0;
    let sh = 600.0;
    let cw = 8.0;
    let ch = 16.0;
    let y = sh - (ch + 10.0) + 4.0;

    // `[  Cancel]  [  Close]` consumes 10 + 2 + 9 = 21 cells, right-aligned
    // inside the PAINTED width. An 800px window at cw=8 is 100 cells, but
    // the renderer fits bottom-bar overlays to `overlay_label_cols` =
    // floor(w/cw) - 1 = 99, so the row occupies columns 78..99:
    // Cancel 78..88, gap 88..90, Close 90..99. Hit-testing against the full
    // window width would shift the live region one column right, past the
    // last painted glyph.
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (78.0 * cw + 1.0, y), (sw, sh), (cw, ch), &EN_TEXT),
        Some(0),
        "clicking the visible Cancel button should cancel"
    );
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (90.0 * cw + 1.0, y), (sw, sh), (cw, ch), &EN_TEXT),
        Some(1),
        "clicking the visible Close button should confirm"
    );
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (88.0 * cw + 1.0, y), (sw, sh), (cw, ch), &EN_TEXT),
        None,
        "the gap between buttons is not a button"
    );
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (99.0 * cw + 1.0, y), (sw, sh), (cw, ch), &EN_TEXT),
        None,
        "the column past the painted bar is not a button"
    );
    assert_eq!(
        confirm_dialog_button_hit(
            &buttons,
            (91.0 * cw + 1.0, sh - ch - 20.0),
            (sw, sh),
            (cw, ch),
            &EN_TEXT
        ),
        None,
        "clicks above the confirm bar are still generic modal clicks"
    );

    // The painted budget is 21 columns when the window provides 22. The
    // complete row is visible and live at that exact boundary; one column
    // narrower paints no buttons and therefore must expose no target.
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (1.0, y), (22.0 * cw, sh), (cw, ch), &EN_TEXT),
        Some(0)
    );
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (1.0, y), (21.0 * cw, sh), (cw, ch), &EN_TEXT),
        None
    );
}

#[test]
fn confirm_dialog_button_hit_uses_display_columns() {
    use super::{ConfirmButton, confirm_dialog_button_cells, confirm_dialog_button_hit};

    let buttons = vec![
        ConfirmButton::Cancel,
        ConfirmButton::Confirm {
            label: "界".into(),
            destructive: true,
        },
    ];
    let sw = 800.0;
    let sh = 600.0;
    let cw = 8.0;
    let ch = 16.0;
    let y = sh - (ch + 10.0) + 4.0;

    assert_eq!(confirm_dialog_button_cells(&buttons[1], &EN_TEXT), 6);
    assert_eq!(
        confirm_dialog_button_hit(&buttons, (81.0 * cw + 1.0, y), (sw, sh), (cw, ch), &EN_TEXT),
        Some(0),
        "the hit row must start where the wide-glyph button row is painted"
    );
}

/// Hit testing measures the buttons in the UI's language, as the renderer
/// paints them. A 24-column window fits `[  Cancel]  [  Close]` (21 cells)
/// but not `[  Cancelar]  [  Cerrar]` (24 cells), so in Spanish the row is
/// hidden and must expose no target: an invisible Close would confirm.
#[test]
fn confirm_dialog_button_hit_measures_translated_labels() {
    use super::{ConfirmButton, confirm_dialog_button_hit};
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    let buttons = |close: &str| {
        vec![
            ConfirmButton::Cancel,
            ConfirmButton::Confirm {
                label: close.to_string(),
                destructive: true,
            },
        ]
    };
    let (sh, cw, ch) = (600.0, 8.0, 16.0);
    let y = sh - (ch + 10.0) + 4.0;
    let sw = 24.0 * cw;
    assert_eq!(
        confirm_dialog_button_hit(
            &buttons("Close"),
            (20.0 * cw, y),
            (sw, sh),
            (cw, ch),
            &EN_TEXT
        ),
        Some(1)
    );
    assert_eq!(
        confirm_dialog_button_hit(&buttons("Cerrar"), (20.0 * cw, y), (sw, sh), (cw, ch), &es),
        None
    );
}

/// Regression coverage for content geometry at every tab-bar position.
/// Walks the 4 (tab_pos)×3 (status_pos) cases.
#[test]
fn content_rect_for_carves_out_tab_and_status_bands() {
    use super::content_rect_for;
    use kettle_config::{StatusBarMode, TabBarPos};
    // Top + status-off: content starts at y=tab_h, full width.
    let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Off);
    assert_eq!(r, (0.0, 24.0, 800.0, 576.0));
    // Bottom + status-off: content at y=0, height shrinks by tab_h.
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Bottom,
        StatusBarMode::Off,
    );
    assert_eq!(r, (0.0, 0.0, 800.0, 576.0));
    // Top + status-top: both claim from top.
    let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Top);
    assert_eq!(r, (0.0, 40.0, 800.0, 560.0));
    // Top + status-bottom: status claims from bottom.
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Top,
        StatusBarMode::Bottom,
    );
    assert_eq!(r, (0.0, 24.0, 800.0, 560.0));
    // Bottom + status-bottom: both claim from bottom.
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Bottom,
        StatusBarMode::Bottom,
    );
    assert_eq!(r, (0.0, 0.0, 800.0, 560.0));
    // Left vertical strip — content carves out
    // `VERTICAL_TAB_STRIP_W` (180 px) from the LEFT side.
    let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
    assert_eq!(r, (180.0, 0.0, 620.0, 600.0));
    // Right: 180 px carved from the RIGHT side.
    let r = content_rect_for((800, 600), 24.0, 0.0, TabBarPos::Right, StatusBarMode::Off);
    assert_eq!(r, (0.0, 0.0, 620.0, 600.0));
    // Vertical + status-bar: status still claims y-band
    // (status is always horizontal in v1); strip claims x-band.
    let r = content_rect_for((800, 600), 24.0, 16.0, TabBarPos::Left, StatusBarMode::Top);
    assert_eq!(r, (180.0, 16.0, 620.0, 584.0));
    let r = content_rect_for(
        (800, 600),
        24.0,
        16.0,
        TabBarPos::Right,
        StatusBarMode::Bottom,
    );
    assert_eq!(r, (0.0, 0.0, 620.0, 584.0));
    // Defensive: content_h + content_w clamped to >= 1.0 so a
    // tiny window doesn't degenerate to a zero/negative rect.
    let r = content_rect_for((100, 30), 24.0, 16.0, TabBarPos::Top, StatusBarMode::Bottom);
    assert!(r.3 >= 1.0);
    let r = content_rect_for((100, 600), 0.0, 0.0, TabBarPos::Left, StatusBarMode::Off);
    assert!(
        r.2 >= 1.0,
        "narrow window with vertical strip clamps content_w"
    );
}

/// #576: no update state may resize a pane. The chip lives in the tab bar's
/// own band, the bar's height ignores it, and nothing that applies or
/// dismisses a notice queues a resize.
#[test]
fn no_update_state_changes_content_rect() {
    let src = production_source();
    let body = |name: &str| {
        src.split(name)
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .unwrap_or_else(|| panic!("{name} exists"))
            .to_string()
    };
    for name in [
        "fn apply_update_facts(",
        "fn dismiss_update_notice(",
        "fn open_update_release(",
        "fn request_update_facts(",
    ] {
        let body = body(name);
        for call in ["pending_resize", ".resize(", "queue_window_resizes"] {
            assert!(!body.contains(call), "{name} must not resize ({call})");
        }
    }
    assert!(!body("fn tab_bar_h(").contains("update"));
    assert!(!src.contains("update_banner_h"));
}

/// Drift guard. `session_screenshot_path` is the
/// pure helper behind `Action::TakeScreenshot`. Mirrors the
/// `session_log_path` shape.
#[test]
fn session_screenshot_path_under_cache_kettle_shots() {
    use super::session_screenshot_path;
    let cache = std::path::Path::new("/home/u/.cache");
    let p = session_screenshot_path(1_716_422_400, 1234, Some(cache));
    assert_eq!(
        p,
        std::path::PathBuf::from("/home/u/.cache/kettle/shots/kettle-1716422400-1234.png")
    );
    // Relative fallback when no cache dir resolves.
    let p = session_screenshot_path(1_716_422_400, 1234, None);
    assert_eq!(
        p,
        std::path::PathBuf::from("kettle-shots/kettle-1716422400-1234.png")
    );
    // .png extension is fixed (vs the `session_log_path` .log shape).
    assert_eq!(p.extension().and_then(|s| s.to_str()), Some("png"));
}

/// `validate_screenshot_path` must refuse an empty path and any destination
/// that already exists in any form, so the ctl `screenshot` method / the
/// `kettle_screenshot` MCP tool can never be pointed at an existing
/// sensitive file to silently overwrite it.
#[test]
fn validate_screenshot_path_rejects_empty_and_existing_destinations() {
    use super::validate_screenshot_path;

    assert!(validate_screenshot_path("").is_err());
    assert!(validate_screenshot_path("   ").is_err());

    let dir = tempfile::tempdir().expect("tempdir");
    // A brand-new path inside a real (existing) directory is fine.
    let fresh = dir.path().join("new-shot.png");
    assert_eq!(
        validate_screenshot_path(fresh.to_str().unwrap()).unwrap(),
        fresh
    );

    // An EXISTING regular file must be refused — this is the
    // "~/.bashrc" / "~/.ssh/authorized_keys" overwrite scenario.
    let existing_file = dir.path().join("already-here.png");
    std::fs::write(&existing_file, b"not a screenshot").expect("write fixture file");
    assert!(validate_screenshot_path(existing_file.to_str().unwrap()).is_err());

    // An EXISTING directory must also be refused.
    assert!(validate_screenshot_path(dir.path().to_str().unwrap()).is_err());
}

/// A pre-planted symlink must be refused even when its target doesn't
/// exist (a dangling symlink). `Path::exists` follows symlinks and would
/// report `false` for a dangling one, wrongly treating the destination as
/// free; `symlink_metadata` must be used instead so the attacker can't race
/// a "the target doesn't exist yet" window. Unix-only: creating a symlink on
/// Windows needs Developer Mode / admin, which isn't guaranteed in CI.
#[cfg(unix)]
#[test]
fn validate_screenshot_path_rejects_dangling_symlink() {
    use super::validate_screenshot_path;

    let dir = tempfile::tempdir().expect("tempdir");
    let link = dir.path().join("evil-link.png");
    let target = dir.path().join("does-not-exist-yet");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");
    assert!(!target.exists(), "sanity: the symlink target is dangling");
    assert!(
        validate_screenshot_path(link.to_str().unwrap()).is_err(),
        "a dangling symlink must still be refused, not silently followed/created through"
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

/// The join neither crate can check on its own.
///
/// `cache_dir_from_env` decides where kettle puts its `kettle/` directory;
/// `kettle_state::is_kettle_owned_dir_name` decides whether that directory
/// may be repaired to `0700`. kettle-state builds its base list from the
/// environment and cannot depend on kettle-ui to ask where the cache went,
/// so a resolver base missing from that list is invisible to every test in
/// either crate. Such a gap can leave `~/.cache/kettle` at `0775` with
/// `0600` content underneath it.
///
/// Each resolver branch runs in a re-executed child. Environment mutation in
/// this test process would race every other test's `getenv`, and checking
/// only the ambient branch would skip under `env -i` and never exercise XDG
/// on normal CI.
#[test]
fn every_resolved_cache_directory_is_recognized_as_kettles_own() {
    use super::cache_dir_from_env;

    const CHILD: &str = "KETTLE_CACHE_RESOLVER_BRANCH";
    if std::env::var_os(CHILD).is_none() {
        let cases: &[(&str, &[(&str, &str)])] = if cfg!(windows) {
            &[
                ("xdg", &[("XDG_CACHE_HOME", r"C:\cache-xdg")]),
                (
                    "local-app-data",
                    &[("LOCALAPPDATA", r"C:\Users\test\AppData\Local")],
                ),
            ]
        } else {
            &[
                (
                    "xdg",
                    &[("XDG_CACHE_HOME", "/tmp/cache-xdg"), ("HOME", "/tmp/home")],
                ),
                ("home", &[("HOME", "/tmp/home")]),
            ]
        };
        for (case, variables) in cases {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([
                    "--exact",
                    "app::tests::every_resolved_cache_directory_is_recognized_as_kettles_own",
                    "--nocapture",
                ])
                .env(CHILD, case);
            for key in ["XDG_CACHE_HOME", "HOME", "LOCALAPPDATA", "USERPROFILE"] {
                child.env_remove(key);
            }
            for (key, value) in *variables {
                child.env(key, value);
            }
            let status = child.status().expect("re-exec cache resolver test");
            assert!(status.success(), "cache resolver child {case} failed");
        }
        return;
    }

    let cache = cache_dir_from_env(|k| std::env::var(k).ok())
        .expect("the controlled branch must resolve a cache directory");
    assert!(
        kettle_state::is_kettle_owned_dir_name(&cache.join("kettle")),
        "the resolver puts kettle's cache in {}, which the private-path base \
             list does not recognize — kettle cannot repair its own directory \
             there, and every private path under it stays refused",
        cache.join("kettle").display()
    );
}

/// Drift guard. `cache_dir_from_env` probes the XDG /
/// HOME / Windows-ish envs in order. Empty values are treated as
/// unset so a stripped CI container with `XDG_CACHE_HOME=""`
/// falls through to the next probe instead of returning `""`.
#[test]
fn cache_dir_from_env_probes_in_order() {
    use super::cache_dir_from_env;
    // XDG wins when set.
    let f = |k: &str| match k {
        "XDG_CACHE_HOME" => Some("/x/cache".to_string()),
        "HOME" => Some("/h".to_string()),
        _ => None,
    };
    assert_eq!(
        cache_dir_from_env(f).as_deref(),
        Some(std::path::Path::new("/x/cache"))
    );
    // The non-XDG fallback is per-OS, mirroring the
    // config-dir resolver. On Unix, empty XDG falls through to HOME/.cache.
    #[cfg(not(windows))]
    {
        let f = |k: &str| match k {
            "XDG_CACHE_HOME" => Some(String::new()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new("/h/.cache"))
        );
    }
    // On Windows the cache dir is %LOCALAPPDATA%, and a stray HOME is IGNORED
    // (the config-dir split-brain class — a shell launch must not disagree
    // with a Start-menu launch).
    #[cfg(windows)]
    {
        // HOME set but no LOCALAPPDATA → None (HOME must not be used).
        let f = |k: &str| match k {
            "XDG_CACHE_HOME" => Some(String::new()),
            "HOME" => Some("/h".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f),
            None,
            "Windows must not fall back to HOME/.cache"
        );
        // HOME AND LOCALAPPDATA set → LOCALAPPDATA wins, HOME ignored.
        let f = |k: &str| match k {
            "HOME" => Some(r"C:\Users\u".to_string()),
            "LOCALAPPDATA" => Some(r"C:\Users\u\AppData\Local".to_string()),
            _ => None,
        };
        assert_eq!(
            cache_dir_from_env(f).as_deref(),
            Some(std::path::Path::new(r"C:\Users\u\AppData\Local"))
        );
    }
    // None of the env vars set → None (every OS).
    assert!(cache_dir_from_env(|_| None).is_none());
}

/// Drift guard for `tab_strip_layout`: tabs ALWAYS fill the bar (even
/// division, no max cap), then floor at min-width + overflow-scroll, and
/// the active tab stays visible.
#[test]
fn tab_strip_layout_fills_and_overflows() {
    use super::tab_strip_layout;
    // (No trailing button reserve here so this drift guard keeps exercising
    // the fill/overflow behavior directly; the full-width-aligned path with
    // a real reserve is covered by the alignment test below.)
    // Few tabs on a wide strip FILL the bar — 2 tabs each take half (no max
    // cap), left-packed, no arrows.
    let l = tab_strip_layout(2, 0, 1920.0, 0.0, 24.0, 120.0, true);
    assert_eq!(l.width, 960.0);
    assert_eq!(l.xs, vec![Some(0.0), Some(960.0)]);
    assert!(l.arrow_left.is_none() && l.arrow_right.is_none());
    // Even division also fills for more tabs while above the min floor.
    assert_eq!(
        tab_strip_layout(4, 0, 800.0, 0.0, 24.0, 120.0, true).width,
        200.0
    );
    // Many tabs: overflow → min width, arrows, only whole tabs shown.
    let l = tab_strip_layout(20, 0, 800.0, 0.0, 24.0, 120.0, true);
    assert_eq!(l.width, 120.0);
    assert!(l.arrow_left.is_some() && l.arrow_right.is_some());
    let visible = l.xs.iter().filter(|x| x.is_some()).count();
    assert!(
        (1..20).contains(&visible),
        "some tabs scroll off: {visible}"
    );
    assert_eq!(l.xs[0], Some(24.0), "active tab 0 visible at the left");
    // Active near the end is pinned into view; early tabs scroll off.
    let l = tab_strip_layout(20, 19, 800.0, 0.0, 24.0, 120.0, true);
    assert!(l.xs[19].is_some(), "active tab 19 must be visible");
    assert!(l.xs[0].is_none(), "early tabs scrolled off");
    // Scrolling disabled: no arrows; width may fall below min (just shrinks).
    let l = tab_strip_layout(20, 0, 800.0, 0.0, 24.0, 120.0, false);
    assert!(l.arrow_left.is_none() && l.arrow_right.is_none() && l.width < 120.0);
    // Degenerate (no tabs): panic-safe, one slot.
    assert_eq!(
        tab_strip_layout(0, 0, 100.0, 0.0, 24.0, 120.0, true)
            .xs
            .len(),
        1
    );
}

/// A side tab bar shows the active tab and `+` however many tabs there are.
/// It once stacked every tab from the top and clamped `+` to the bottom row,
/// so past a screenful the active tab was drawn below the window and `+`
/// covered a tab.
#[test]
fn side_tab_rows_keep_the_active_tab_and_plus_in_view() {
    use super::{SideTabRows, side_tab_rows};
    let fits = |n| SideTabRows {
        first: 0,
        shown: n,
        top: 0,
        plus: n,
        arrows: false,
    };
    // Every tab and `+` fit: tabs from the top, `+` right below the last.
    assert_eq!(side_tab_rows(3, 2, 13), fits(3));
    assert_eq!(side_tab_rows(12, 11, 13), fits(12));
    // One more and the strip scrolls: `⌃`, ten tabs, `⌄`, then `+` on the
    // last row, with the window ending at the active tab past page one.
    let scrolled = |first| SideTabRows {
        first,
        shown: 10,
        top: 1,
        plus: 12,
        arrows: true,
    };
    assert_eq!(side_tab_rows(13, 0, 13), scrolled(0));
    assert_eq!(side_tab_rows(13, 9, 13), scrolled(0));
    assert_eq!(side_tab_rows(13, 10, 13), scrolled(1));
    assert_eq!(side_tab_rows(13, 12, 13), scrolled(3));
    // Too short for the arrows: the active tab and `+`.
    assert_eq!(
        side_tab_rows(5, 4, 2),
        SideTabRows {
            first: 4,
            shown: 1,
            top: 0,
            plus: 1,
            arrows: false,
        }
    );
    for n in 1..48 {
        for rows in 0..20 {
            for active in 0..n {
                let l = side_tab_rows(n, active, rows);
                assert!(
                    (l.first..l.first + l.shown).contains(&active) && l.first + l.shown <= n,
                    "{n} tabs, {rows} rows, active {active}: {l:?}"
                );
                if rows < 2 {
                    // One row or less: the active tab alone, `+` left out.
                    assert!(
                        l.shown == 1 && l.plus >= rows,
                        "{n} tabs, {rows} rows: {l:?}"
                    );
                    continue;
                }
                assert!(l.plus < rows, "{n} tabs, {rows} rows: + below the strip");
                let arrow_rows = usize::from(l.arrows);
                assert_eq!(l.top, arrow_rows);
                assert!(
                    l.top + l.shown + arrow_rows <= l.plus,
                    "{n} tabs, {rows} rows: + or ⌄ covers a tab: {l:?}"
                );
            }
        }
    }
}

/// The side bar lays its rows out through `side_tab_rows` and parks the tabs
/// it does not show, rather than stacking every tab from the top.
#[test]
fn the_side_tab_bar_places_rows_through_side_tab_rows() {
    let src = production_source();
    let body = src
        .split("fn tab_bar_vertical(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("tab_bar_vertical body");
    assert!(body.contains("let layout = side_tab_rows(labels.len(), active, rows);"));
    assert!(body.contains("row_y(layout.top + i - layout.first)"));
    assert!(!body.contains("i as f32 * height"));
    // A title editor takes the top row of the window, strip included, so the
    // rows start below it rather than under it.
    assert!(body.contains("let lead = self.vertical_title_edit_h(ws);"));
    assert!(body.contains("let row_y = |row: usize| lead + row as f32 * height;"));
    assert!(body.contains("let strip_rows = ((sh - lead) / height).floor()"));
    // `+` and the update chip are left out of a strip with no row for them.
    assert!(body.contains("let plus_y = if layout.plus < rows {"));
    assert!(body.contains(".filter(|_| strip_rows >= 2)"));
    // A narrowed horizontal tab has no close button reaching past it.
    let horizontal = src
        .split("fn tab_bar(&self, ws: &WindowState) -> TabBar {")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("tab_bar body");
    assert!(horizontal.contains("let closable = self.cfg.close_button_on_tab && seg_w >= height;"));
}

fn parked_strip(vertical: bool) -> Vec<kettle_render::TabSeg> {
    // Ten tabs, 2..=4 on screen and the rest parked, as an overflowing bar
    // lays them out.
    (0..10)
        .map(|idx| {
            let at = if (2..=4).contains(&idx) {
                (idx - 2) as f32 * 30.0 + 24.0
            } else {
                super::PARKED_TAB
            };
            let rect = if vertical {
                (0.0, at, 160.0, 30.0)
            } else {
                (at, 0.0, 30.0, 24.0)
            };
            kettle_render::TabSeg {
                idx,
                rect,
                title_rect: rect,
                close: (0.0, 0.0, 0.0, 0.0),
                title: format!("tab-{idx}"),
                path: None,
                active: idx == 2,
                activity: kettle_render::TabActivity::Normal,
            }
        })
        .collect()
}

/// A drag past the last tab on screen stops there, and a drop lands right
/// after it, on either bar axis. Both once reached the hidden last tab: a
/// drag onto `⌄` or `›` moved the tab to the very end, and a drop appended
/// after every hidden tab.
#[test]
fn dragging_and_docking_stop_at_the_tabs_on_screen() {
    use super::{dock_insertion_tab, tab_drag_target_index};
    for vertical in [true, false] {
        let segs = parked_strip(vertical);
        let at = |main: f32| if vertical { (80.0, main) } else { (main, 12.0) };
        let (x, y) = at(24.0 + 3.0 * 30.0 + 10.0);
        assert_eq!(tab_drag_target_index(x, y, &segs), 4, "past the last shown");
        let (x, y) = at(5.0);
        assert_eq!(
            tab_drag_target_index(x, y, &segs),
            2,
            "before the first shown"
        );
        let (x, y) = at(24.0 + 30.0 + 5.0);
        assert_eq!(tab_drag_target_index(x, y, &segs), 3);

        assert_eq!(
            dock_insertion_tab(&segs, vertical, 24.0 + 2.0 * 30.0 + 20.0),
            Some(5)
        );
        assert_eq!(dock_insertion_tab(&segs, vertical, 24.0 + 5.0), Some(2));
        assert_eq!(
            dock_insertion_tab(&segs, vertical, 24.0 + 30.0 + 20.0),
            Some(4)
        );
    }
    assert_eq!(dock_insertion_tab(&[], true, 10.0), None);
}

/// A strip with less than `tab-min-width` between its scroll arrows still
/// keeps the active tab between them. It was once drawn at full width over
/// `›`, `▾` and `+` in a window about 120 px wide.
#[test]
fn an_overflowing_tab_strip_keeps_its_tabs_between_the_arrows() {
    use super::tab_strip_layout;
    for strip in [12.0_f32, 30.0, 71.5, 100.0, 167.0, 400.0, 1000.0] {
        for n in [2_usize, 5, 40] {
            for active in [0, n / 2, n - 1] {
                let l = tab_strip_layout(n, active, strip, 48.5, 24.25, 120.0, true);
                let Some(right) = l.arrow_right else {
                    continue;
                };
                let arrow_w = strip - right;
                assert!(l.xs[active].is_some(), "{strip} px, {n} tabs: active shown");
                for (i, x) in l.xs.iter().enumerate() {
                    let Some(x) = *x else { continue };
                    let w = if i + 1 == n { l.last_width } else { l.width };
                    assert!(
                        x >= arrow_w - 0.01 && x + w <= right + 0.01,
                        "{strip} px, {n} tabs, active {active}: tab {i} at {x}..{} leaves {arrow_w}..{right}",
                        x + w
                    );
                }
            }
        }
    }
}

/// The 2-tab boundary must line up with a centered vertical pane split so
/// the split divider visually continues the tab boundary. With the trailing
/// `▾`/`+` reservation, tabs divide the FULL bar width and only the last tab
/// is shortened.
#[test]
fn tab_strip_layout_aligns_boundary_with_split_seam() {
    use super::{tab_segment_strip_width, tab_strip_layout};
    let w = 1000.0_f32; // window / content width
    let h = 24.0_f32; // tab-bar height == each button's side
    let reserve = 2.0 * h; // `▾` + `+`
    let strip = tab_segment_strip_width(w, h, h); // button-trimmed strip
    let l = tab_strip_layout(2, 0, strip, reserve, h, 120.0, true);
    // tab1/tab2 boundary at the window centre …
    assert_eq!(l.xs, vec![Some(0.0), Some(w / 2.0)]);
    // … which is exactly where a 50/50 vertical split's divider sits
    // (`mux::Node::layout`: `x + round(content_w * ratio)`, content_w == w).
    assert_eq!(l.xs[1], Some((w * 0.5).round()));
    // tab 1 is a full half; tab 2 yields the button reservation.
    assert_eq!(l.width, w / 2.0);
    assert_eq!(l.last_width, w / 2.0 - reserve);
    // The last tab's right edge stops exactly at the button strip.
    assert_eq!(l.xs[1].unwrap() + l.last_width, strip);
    // A window too narrow for the shortened last tab to clear min_w falls
    // back to plain strip packing (no zero/negative last tab).
    let narrow = tab_strip_layout(2, 0, 200.0, reserve, h, 120.0, true);
    assert!(narrow.last_width > 0.0 && (narrow.last_width - narrow.width).abs() < 0.01);
}

/// Link/path overlay underlines must follow focused output immediately.
/// `less -R` redraws the viewport with output bytes while
/// `display_offset` stays fixed, so an output-only debounce leaves stale
/// link rects visibly attached to the previous frame.
#[test]
fn focused_link_scans_are_not_output_debounced() {
    let app_src = production_source();
    let window_src = kettle_test_support::production_source(include_str!("../window_state.rs"));
    let removed_const = format!("LINKS_SCAN_{}", "DEBOUNCE");
    let removed_field = format!("last_{}_scan", "links");
    assert!(
        !app_src.contains(&removed_const),
        "focused output link scans must not be debounce-delayed"
    );
    assert!(
        !window_src.contains(&removed_field),
        "WindowState must not keep a timestamp for output-only link-scan debounce"
    );
    assert!(
        app_src.contains("output_generation"),
        "link scan key must still include focused output_generation"
    );
}

/// Drift guard. `split_new_tab_button` splits the trailing
/// button into the `▾` arrow (left) and `+` (right) hit-rects; they must
/// abut with no gap/overlap, and a degenerate `arrow_w` can't yield a
/// negative `+` width.
#[test]
fn split_new_tab_button_places_arrow_left_of_plus() {
    use super::{
        NewTabHoverTarget, new_tab_hover_target, split_new_tab_button,
        tab_bar_pointer_region_contains,
    };
    // button at x=200, 52 wide (two 26-px halves), arrow 26 wide.
    let (arrow, plus) = split_new_tab_button((200.0, 0.0, 52.0, 26.0), 26.0);
    assert_eq!(arrow, (200.0, 0.0, 26.0, 26.0)); // arrow on the LEFT
    assert_eq!(plus, (226.0, 0.0, 26.0, 26.0)); // plus on the RIGHT
    // They abut exactly (no gap, no overlap).
    assert_eq!(arrow.0 + arrow.2, plus.0);
    // Degenerate: arrow wider than the button → clamped, plus width >= 0.
    let (a2, p2) = split_new_tab_button((0.0, 0.0, 20.0, 10.0), 999.0);
    assert_eq!(a2.2, 20.0);
    assert_eq!(p2.2, 0.0);
    assert!(p2.2 >= 0.0);

    let mut bar = kettle_render::TabBar::hidden();
    bar.new_tab_menu = arrow;
    bar.new_tab = plus;
    assert_eq!(
        new_tab_hover_target(&bar, 213.0, 13.0),
        Some(NewTabHoverTarget::Menu)
    );
    assert_eq!(
        new_tab_hover_target(&bar, 239.0, 13.0),
        Some(NewTabHoverTarget::Plus)
    );
    assert_eq!(new_tab_hover_target(&bar, 199.9, 13.0), None);
    assert_eq!(new_tab_hover_target(&bar, 252.1, 13.0), None);

    // Horizontal bars occupy one row; vertical bars keep the same row
    // height but expose a full-height interaction band. The latter is the
    // geometry that makes lower tab rows and their trailing `+` clickable.
    bar.height = 26.0;
    bar.band = (0.0, 0.0, 252.0, 26.0);
    assert!(tab_bar_pointer_region_contains(&bar, 239.0, 13.0));
    assert!(!tab_bar_pointer_region_contains(&bar, 239.0, 27.0));
    bar.band = (200.0, 0.0, 52.0, 400.0);
    bar.new_tab = (200.0, 78.0, 52.0, 26.0);
    assert!(tab_bar_pointer_region_contains(&bar, 226.0, 91.0));
    assert_eq!(
        new_tab_hover_target(&bar, 226.0, 91.0),
        Some(NewTabHoverTarget::Plus)
    );
    // Adding a vertical tab moves the button down one row. Re-running the
    // same hit-test against the new layout must clear the stale hover even
    // though the physical pointer has not moved.
    bar.new_tab = (200.0, 104.0, 52.0, 26.0);
    assert_eq!(new_tab_hover_target(&bar, 226.0, 91.0), None);
    assert!(!tab_bar_pointer_region_contains(&bar, 199.9, 91.0));
}

/// Every user-visible tab-add path funnels through this helper, so keep the
/// post-layout hover refresh beside the Lua/recording event rather than
/// relying on each input route to remember it independently.
#[test]
fn tab_add_event_recomputes_chrome_hover_after_layout_changes() {
    let src = production_source();
    let body = src
        .split("fn fire_tab_add_event(")
        .nth(1)
        .and_then(|rest| rest.split("\n    fn ").next())
        .expect("fire_tab_add_event body");
    assert!(
        body.contains("self.sync_cursor_icon(ws);"),
        "adding a tab must recompute hover after a vertical new-tab button moves"
    );
}

/// Drift guard. `pick_next_profile` is the pure
/// helper behind `Action::NextProfile`/`PrevProfile`. Cycles
/// the sorted profile list with wrap-around; starts at index 0
/// when current isn't a known profile (e.g. --config FILE or
/// default config).
#[test]
fn pick_next_profile_wraps_and_starts_at_index_0() {
    use super::pick_next_profile;
    let names: Vec<String> = vec!["dark".into(), "dev".into(), "light".into()];
    // Forward through the list, wrapping at the end.
    assert_eq!(pick_next_profile(Some("dark"), &names, true), "dev");
    assert_eq!(pick_next_profile(Some("dev"), &names, true), "light");
    assert_eq!(pick_next_profile(Some("light"), &names, true), "dark");
    // Backward, wrapping at the start.
    assert_eq!(pick_next_profile(Some("dark"), &names, false), "light");
    assert_eq!(pick_next_profile(Some("dev"), &names, false), "dark");
    assert_eq!(pick_next_profile(Some("light"), &names, false), "dev");
    // Current is None or an unknown name: start the cycle at idx 0.
    // Forward → idx 1 (the second name); backward → last name.
    assert_eq!(pick_next_profile(None, &names, true), "dev");
    assert_eq!(pick_next_profile(None, &names, false), "light");
    assert_eq!(pick_next_profile(Some("missing"), &names, true), "dev");
    // Single profile: always returns itself (n=1, both arms ⇒ idx 0).
    let single = vec!["only".to_string()];
    assert_eq!(pick_next_profile(Some("only"), &single, true), "only");
    assert_eq!(pick_next_profile(Some("only"), &single, false), "only");
}

/// #251: on macOS the titlebar theme must not be seeded at window
/// creation. winit turns a creation-time hint into an `NSWindow`
/// appearance override before AppKit has built the titlebar's container
/// view, and the native material view kettle adds to the frame view right
/// afterwards then locks in the container-less layout — AppKit centres the
/// title over the traffic lights, and nothing recovers it for the life of
/// the window. The appearance goes on once the window is the key window
/// instead. Layout itself is only checkable live, on a real desktop; this
/// pins the policy and the wiring that carry it.
#[test]
fn macos_applies_the_titlebar_theme_after_the_window_is_up() {
    use super::{WindowTheme, creation_theme_hint, native_theme_sync_is_due};
    for hint in [None, Some(WindowTheme::Light), Some(WindowTheme::Dark)] {
        assert_eq!(
            creation_theme_hint(hint, false),
            hint,
            "every other platform still seeds the caption at creation"
        );
        assert_eq!(
            creation_theme_hint(hint, true),
            None,
            "macOS must reach the window with no appearance override"
        );
    }
    // The deferral is macOS-only.
    for already_synced in [false, true] {
        assert!(native_theme_sync_is_due(false, already_synced, false));
        assert!(native_theme_sync_is_due(true, already_synced, false));
    }
    // On macOS only the first appearance waits for the key window; once a
    // window has one, later changes must not be held back or a background
    // window strands on a stale titlebar until someone clicks it.
    assert!(!native_theme_sync_is_due(false, false, true));
    assert!(native_theme_sync_is_due(true, false, true));
    assert!(native_theme_sync_is_due(false, true, true));
    assert!(native_theme_sync_is_due(true, true, true));

    // Policy alone does not fix anything if the call sites drop it.
    let src = production_source();
    assert!(
        src.contains(".with_theme(creation_theme_hint("),
        "window creation must route its theme hint through creation_theme_hint"
    );
    assert!(
        src.contains("state.window_shown && w.has_focus()"),
        "the first apply must wait for a shown, key window"
    );
    assert!(
        src.contains("state.native_theme_synced.is_some(),"),
        "later changes must not be gated on focus, or a background window \
             keeps a stale titlebar"
    );
    assert!(
        src.contains("state.native_theme_synced = Some(hint)"),
        "the applied hint must be tracked per window, not once per process"
    );
}

/// `native_theme_hint` is the pure policy behind the native titlebar theme
/// (window creation + `maybe_sync_native_theme`). Pin the decision table:
/// plain Auto defers to the OS (`None`, keeping winit's `ThemeChanged`
/// flowing for the palette auto-switcher); every other mode, and Auto with
/// a schedule, forces the titlebar to the active theme's darkness.
#[test]
fn native_theme_hint_decision_table() {
    use super::{WindowTheme, native_theme_hint};
    use kettle_config::ThemeMode;
    // Plain Auto: follow the OS regardless of the active palette.
    assert_eq!(native_theme_hint(ThemeMode::Auto, false, true), None);
    assert_eq!(native_theme_hint(ThemeMode::Auto, false, false), None);
    // Auto + schedule: the schedule drives the palette, so the titlebar
    // matches the palette, not the OS.
    assert_eq!(
        native_theme_hint(ThemeMode::Auto, true, true),
        Some(WindowTheme::Dark)
    );
    // Explicit / Light / Dark: titlebar matches the active theme.
    for mode in [ThemeMode::Explicit, ThemeMode::Light, ThemeMode::Dark] {
        assert_eq!(
            native_theme_hint(mode, false, true),
            Some(WindowTheme::Dark)
        );
        assert_eq!(
            native_theme_hint(mode, false, false),
            Some(WindowTheme::Light)
        );
    }
}

#[test]
fn live_window_icon_uses_the_active_palette_variant() {
    let src = kettle_test_support::production_source(&super::app_source());
    assert!(
        src.contains(".with_window_icon(load_window_icon(self.cfg.theme.is_dark()))"),
        "window creation must start with the active theme's icon"
    );
    assert!(
        src.contains("w.set_window_icon(icon.clone());"),
        "theme changes must update Windows and X11 live icons"
    );
    for asset in ["kettle-256.png", "kettle-light-256.png"] {
        assert!(
            src.contains(&format!("packaging/linux/{asset}")),
            "the runtime icon loader must embed {asset}"
        );
    }
}

/// winit's macOS backend drops a window icon, so macOS never decodes one
/// on the way to the first window.
#[cfg(target_os = "macos")]
#[test]
fn macos_skips_the_window_icon_decode() {
    use super::load_window_icon;
    assert!(load_window_icon(true).is_none(), "dark icon decoded");
    assert!(load_window_icon(false).is_none(), "light icon decoded");
}

/// Drift guard: the winit dependency must enable Wayland CSD via
/// `wayland-csd-adwaita-notitle`, the sctk-adwaita variant with NO text
/// renderer, and never the `ab_glyph`/`crossfont` variants, which would bring
/// back the unmaintained `ttf-parser` (RUSTSEC-2026-0192). `deny.toml` bans it
/// from the graph; this pins the manifest side so `cargo test` catches a
/// feature edit before CI.
/// Dropping the feature entirely would regress GNOME Wayland to
/// smithay-client-toolkit's FallbackFrame (flat gray bar, filled-square
/// close button, no Adwaita styling).
#[test]
fn winit_wayland_csd_stays_notitle() {
    let manifest = include_str!("../../Cargo.toml");
    let winit_features = manifest
        .split("winit = {")
        .nth(1)
        .and_then(|s| s.split(']').next())
        .expect("kettle-ui Cargo.toml declares winit with a feature list");
    assert!(
        winit_features.contains("\"wayland-csd-adwaita-notitle\""),
        "winit must keep the wayland-csd-adwaita-notitle feature — \
             without it GNOME Wayland falls back to the ugly FallbackFrame"
    );
    assert!(
        !winit_features.contains("\"wayland-csd-adwaita\"\n")
            && !winit_features.contains("\"wayland-csd-adwaita\",")
            && !winit_features.contains("crossfont"),
        "the ab_glyph/crossfont CSD variants reintroduce ttf-parser or a \
             FreeType build dep — keep the notitle variant (RUSTSEC-2026-0192)"
    );
}

#[test]
fn custom_url_handler_failure_falls_back_to_system_open() {
    let src = production_source();
    let open_url = src
        .split("fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {")
        .nth(1)
        .and_then(|s| s.split("fn paste_clipboard").next())
        .expect("open_url helper present");
    assert!(open_url.contains("spawn_custom_url_handler(&cmd, uri)"));
    let checked = src
        .split("fn open_checked_file(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    assert!(checked.contains("spawn_custom_url_handler(cmd, &url)"));
    let helper = src
        .split("fn spawn_custom_url_handler(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    let worker = helper.find(".spawn(move ||").unwrap();
    let child = helper.find("Command::new(&cmd)").unwrap();
    let wait = helper.find("child.wait()").unwrap();
    assert!(worker < child && child < wait);
    assert!(
        open_url.contains("Ok(_) => return,"),
        "a successful custom-url-handler spawn should own the URL"
    );
    assert!(
        open_url.contains("falling through to system")
            && open_url.contains("open::that_detached(uri)"),
        "a failed custom-url-handler spawn must fall back to system open"
    );
    let failed_spawn_tail = open_url
        .split("Err(e) => log::warn!")
        .nth(1)
        .expect("custom handler error arm present");
    assert!(
        !failed_spawn_tail
            .split("open::that_detached(uri)")
            .next()
            .unwrap_or("")
            .contains("return;"),
        "custom-url-handler failure must not return before system open"
    );
}

#[test]
fn run_command_output_budget_includes_json_escaping_and_metadata() {
    use kettle_ctl::protocol::{MAX_RESPONSE_LINE_BYTES, Response, to_json_vec_bounded};
    for timed_out in [false, true] {
        for unit in ["\"", "\\", "\0", "\n", "e\u{301}"] {
            let original = unit.repeat(450_000);
            let mut result = serde_json::json!({
                "pane": u64::MAX,
                "exit_code": if timed_out { None } else { Some(17) },
                "timed_out": timed_out,
                "output": original,
                "output_truncated": false,
            });
            if timed_out {
                result["hint"] = serde_json::json!(
                    "no OSC 133 command-end seen; enable shell integration for exit codes"
                );
            } else {
                result["duration_ms"] = serde_json::json!(u64::MAX);
            }
            let response = super::cap_ctl_command_response(Response::ok(u64::MAX, result));
            let bytes = to_json_vec_bounded(&response, MAX_RESPONSE_LINE_BYTES).unwrap();
            let decoded: Response = serde_json::from_slice(&bytes).unwrap();
            assert!(decoded.ok);
            assert_eq!(decoded.result["timed_out"], timed_out);
            assert_eq!(
                decoded.result["exit_code"],
                if timed_out {
                    serde_json::Value::Null
                } else {
                    serde_json::json!(17)
                }
            );
            assert_eq!(decoded.result["output_truncated"], true);
            let output = decoded.result["output"].as_str().unwrap();
            assert!(!output.is_empty());
            assert!(original.starts_with(output));
            if !timed_out {
                assert_eq!(decoded.result["duration_ms"], u64::MAX);
            }
        }
    }
}

#[test]
fn pending_run_orphan_check_searches_all_windows() {
    let src = production_source();
    let check = src
        .split("fn check_pending_run_deadlines(&mut self, ws: &mut WindowState) {")
        .nth(1)
        .and_then(|s| s.split("fn ctl_capture_output_since").next())
        .expect("check_pending_run_deadlines present");
    assert!(
        check.contains("Self::ctl_pane_ref(ws, &self.windows, *pane).is_none()"),
        "pending run orphan detection must search the checked-out window \
             and the other windows, not only the current ws.mux"
    );
    assert!(
        !check.contains("!ws.mux.panes.contains_key(pane)"),
        "current-window-only orphan checks kill pending runs in other windows"
    );
}

#[test]
fn run_command_ignores_queued_completion_from_before_submission() {
    use super::{PendingRun, take_pending_run};
    let (reply, replies) = crossbeam_channel::bounded(1);
    let run = PendingRun {
        conn_id: 1,
        req_id: 2,
        start_line: 0,
        command_generation: 7,
        deadline: std::time::Instant::now(),
        reply,
    };
    let mut runs = std::collections::HashMap::from([(3, run)]);
    let mut event = kettle_core::CommandFinished {
        generation: 7,
        duration: std::time::Duration::from_secs(5),
        exit_code: Some(9),
    };
    assert!(take_pending_run(&mut runs, 3, &event).is_none());
    assert!(runs.contains_key(&3));
    assert!(replies.try_recv().is_err());
    event.generation = 8;
    let run = take_pending_run(&mut runs, 3, &event).unwrap();
    assert_eq!(run.req_id, 2);
    assert!(runs.is_empty());
}

#[test]
fn run_command_line_cap_reports_a_dropped_head() {
    use super::{MAX_CTL_SCROLLBACK_LINES, ctl_capture_bounds};

    let history_size = MAX_CTL_SCROLLBACK_LINES + 5_000;
    let (start, end, head_dropped) = ctl_capture_bounds(
        0,
        history_size,
        0,
        history_size as u64,
        MAX_CTL_SCROLLBACK_LINES + 24,
    );
    assert_eq!(start, 0);
    assert!(end > start);
    assert!(
        head_dropped,
        "a command that outruns the retained-line capture must mark its output incomplete"
    );

    let (start, end, head_dropped) = ctl_capture_bounds(0, 100, 50, 100, MAX_CTL_SCROLLBACK_LINES);
    assert!(end > start);
    assert!(!head_dropped, "a fully retained capture is complete");
}

#[test]
fn run_command_capture_survives_full_scrollback() {
    use super::ctl_capture_bounds;

    // Two retained history rows and two screen rows; the command began at
    // document row 3, then printed through row 5 while history stayed full.
    let lines = ["command", "first", "second"];
    let (start, end, truncated) = ctl_capture_bounds(2, 2, 3, 5, lines.len());
    assert_eq!(lines[start..end].join("\n"), "command\nfirst\nsecond");
    assert!(!truncated);

    // More output evicts the command's first rows.
    let lines = ["second", "third", "fourth", "prompt"];
    let (start, end, truncated) = ctl_capture_bounds(5, 2, 3, 8, lines.len());
    assert_eq!(
        lines[start..end].join("\n"),
        "second\nthird\nfourth\nprompt"
    );
    assert!(truncated);
}

/// Drift guard. `pick_light_dark_target` is the
/// pure helper behind `Action::ToggleLightDark` — the policy
/// must round-trip current ↔ {light, dark} cleanly, default
/// to `dark` on a third-party current theme, and silently
/// no-op when neither config key is set.
#[test]
fn pick_light_dark_target_round_trips() {
    use super::pick_light_dark_target;
    // Round-trip: current==dark → switch to light.
    assert_eq!(
        pick_light_dark_target("TokyoNight Night", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Day"),
    );
    // Round-trip: current==light → switch to dark.
    assert_eq!(
        pick_light_dark_target("TokyoNight Day", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Night"),
    );
    // Case-insensitive match on `current`.
    assert_eq!(
        pick_light_dark_target("tokyonight night", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Day"),
    );
    // Current is a third-party theme: default to dark.
    assert_eq!(
        pick_light_dark_target("Catppuccin Mocha", "TokyoNight Day", "TokyoNight Night").as_deref(),
        Some("TokyoNight Night"),
    );
    // Only light set: one-way switch to light.
    assert_eq!(
        pick_light_dark_target("Catppuccin Mocha", "TokyoNight Day", "").as_deref(),
        Some("TokyoNight Day"),
    );
    // Only dark set: one-way switch to dark.
    assert_eq!(
        pick_light_dark_target("Catppuccin Latte", "", "TokyoNight Night").as_deref(),
        Some("TokyoNight Night"),
    );
    // Neither set: no-op.
    assert_eq!(pick_light_dark_target("anything", "", ""), None);
}

/// #576: the chip shows its full label while every tab keeps its minimum
/// width, the short one while one tab still does, and nothing otherwise.
#[test]
fn update_chip_fit_full_short_none() {
    use super::update_chip_fit;
    let (full, short) = ("Update v5.2.0", "⬆ v5.2.0");
    // 13 + 2 cells of 10 px, plus a 3 px inset on each side.
    let full_slot = 156.0;
    let short_slot = 106.0;
    assert_eq!(
        update_chip_fit(1000.0, 48.0, 4, 120.0, 10.0, full, short),
        Some((full_slot, false))
    );
    assert_eq!(
        update_chip_fit(1000.0, 48.0, 12, 120.0, 10.0, full, short),
        Some((short_slot, true))
    );
    assert_eq!(
        update_chip_fit(250.0, 48.0, 1, 120.0, 10.0, full, short),
        None
    );
}

#[test]
fn update_chip_hit_and_menu_anchor_follow_the_bar() {
    use super::{update_chip_hit_rect, update_chip_menu_anchor};
    use kettle_config::TabBarPos;
    let chip = (700.0, 3.0, 150.0, 18.0);
    assert_eq!(update_chip_hit_rect(chip), (697.0, 0.0, 156.0, 24.0));
    assert_eq!(update_chip_menu_anchor(chip, TabBarPos::Top), (700.0, 21.0));
    assert_eq!(
        update_chip_menu_anchor(chip, TabBarPos::Bottom),
        (700.0, 3.0)
    );
    assert_eq!(update_chip_menu_anchor(chip, TabBarPos::Left), (850.0, 3.0));
}

/// A press on the chip must reach it before the tab bar's button gate, in
/// the window and in `kettle ctl send_mouse`, or a right click would open
/// the terminal's menu and a left click could start a tab drag.
#[test]
fn both_press_paths_route_the_chip_before_the_button_gate() {
    let src = production_source();
    let gate = "if tab_bar_pointer_region_contains(&bar, px, py) && (bcode == 0 || bcode == 1) {";
    let mut checked = 0;
    for (at, _) in src.match_indices(gate) {
        let before = &src[..at];
        let start = before
            .rfind("let bar = self.tab_bar(ws);")
            .expect("the bar is built first");
        assert!(
            before[start..].contains("self.press_update_chip(ws, &bar, px, py, bcode)"),
            "the chip is routed before this gate"
        );
        checked += 1;
    }
    assert_eq!(checked, 2, "the window and ctl press paths");
}

/// The chip is a button under the window root with a Click action; it never
/// takes the screen reader's focus from the terminal.
#[test]
fn chip_node_never_takes_focus_and_click_opens_menu() {
    let src = production_source();
    let node = src
        .split("// The update chip: a button under the window root")
        .nth(1)
        .and_then(|rest| {
            rest.split("nodes.push((ACCESSIBILITY_UPDATE_CHIP_ID, node));")
                .next()
        })
        .expect("the chip's accessibility node");
    assert!(node.contains("node.add_action(AccessibilityAction::Click);"));
    assert!(!node.contains("focus =") && !node.contains("set_focus"));
    let action = src
        .split("if request.target_node == ACCESSIBILITY_UPDATE_CHIP_ID {")
        .nth(1)
        .and_then(|rest| rest.split("return;").next())
        .expect("the chip's action");
    assert!(action.contains("self.open_update_menu(ws, anchor);"));
}

#[test]
fn ctl_reports_update_notice_not_modal() {
    let src = production_source();
    assert!(!src.contains("\"update_available\""));
    assert!(src.contains("fields.insert(\"update_notice\".into(), update_notice.into());"));
    assert!(src.contains("tab_bar.insert(\"update_chip\".into(), update_chip.into());"));
}

/// While an update notice is active the palette lists its release notes and
/// Dismiss, findable in English from any language; otherwise neither.
#[test]
fn update_rows_present_iff_notice_active() {
    let es = kettle_i18n::Translator::new(kettle_i18n::Language::Es);
    let notice = crate::update_notice::Notice {
        kind: crate::update_notice::NoticeKind::Available,
        tag: "v5.2.0".into(),
    };
    let actions = |notice| {
        super::palette_entries(notice, &es)
            .into_iter()
            .map(|(_, action)| action)
            .collect::<Vec<_>>()
    };
    assert!(!actions(None).contains(&Action::OpenUpdate));
    let with = actions(Some(&notice));
    assert!(with.contains(&Action::OpenUpdate) && with.contains(&Action::DismissUpdate));
    let entries = super::palette_entries(Some(&notice), &es);
    let ranked = kettle_config::palette::rank_entries("release notes", &entries, &es);
    assert_eq!(entries[ranked[0]].1, Action::OpenUpdate);
}

#[test]
fn remote_reconnect_menu_uses_new_pane_action() {
    use super::ContextMenuClick;
    let contexts = [
        kettle_remote::RemoteContext::Ssh {
            host: "box".into(),
            user: Some("alice".into()),
            options: kettle_remote::SshOptions::default(),
        },
        kettle_remote::RemoteContext::Container {
            runtime: kettle_remote::ContainerRuntime::Docker,
            container: "web".into(),
            options: kettle_remote::ContainerOptions::default(),
        },
    ];
    for ctx in contexts {
        let item = super::remote_reconnect_item(&ctx, &EN_TEXT).unwrap();
        assert!(super::item_is_dispatchable(&item));
        let Some(ContextMenuClick::ReconnectRemote(command)) = super::item_to_click(&item, 0)
        else {
            panic!("reconnect must open a new pane");
        };
        assert_eq!(Some(command), kettle_remote::clone_session_command(&ctx));
    }
}

#[cfg(unix)]
#[test]
fn remote_reconnect_opens_local_shell_without_typing_into_source() {
    use super::{BroadcastScope, Config, Mux, PtyGeometry};
    let cfg = Config {
        shell: Some("/bin/sh".into()),
        ..Config::default()
    };
    let geometry = PtyGeometry::from_cell_size(80, 24, 8, 16);
    let waker: kettle_core::Waker = Arc::new(|| {});
    let mut mux = Mux::new();
    mux.new_tab_with_geometry(&cfg, geometry, waker.clone(), &["/bin/cat".into()], None)
        .expect("PTY fixture");
    let source = mux.active_focus().unwrap();
    let generation = mux.panes[&source].input_generation();
    mux.panes.get_mut(&source).unwrap().read_only = true;
    mux.broadcast = BroadcastScope::All;
    assert_eq!(
        super::reconnect_remote_session(&mut mux, &cfg, geometry, waker, ":".into(),).unwrap(),
        super::PaneInputResult::Queued
    );
    let target = mux.active_focus().unwrap();
    assert_ne!(target, source);
    assert_eq!(mux.panes.len(), 2);
    assert_eq!(mux.panes[&target].argv, vec!["/bin/sh".to_string()]);
    assert_eq!(mux.panes[&source].input_generation(), generation);
    assert_eq!(mux.panes[&target].input_generation(), 1);
    mux.kill_children();
}

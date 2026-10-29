const EN_TEXT: kettle_i18n::Translator = kettle_i18n::Translator::new(kettle_i18n::Language::En);

fn key_release_test_paste(pane: u64, content: super::QueuedPasteContent) -> super::QueuedPaste {
    super::QueuedPaste {
        window: 1,
        target: Some(pane),
        receipt_pane: Some(pane),
        broadcast: BroadcastScope::Off,
        recipients: Vec::new(),
        content,
        key_presses: Vec::new(),
        key_releases: Vec::new(),
    }
}

fn replay_key_release_test_queue(
    ws: &mut WindowState,
    queue: &mut std::collections::VecDeque<super::QueuedPaste>,
) -> Vec<u8> {
    let mut delivered = Vec::new();
    while let Some(paste) = super::paste::pop_ready_paste(queue) {
        if let super::QueuedPasteContent::Input {
            targets,
            prepare_input,
        } = paste.content
        {
            assert_eq!(
                App::deliver_deferred_input(ws, &targets, &paste.key_presses, prepare_input, false),
                PaneInputResult::Queued
            );
            delivered.extend(targets.into_iter().flat_map(|(_, bytes)| bytes));
        }
        assert_eq!(
            App::release_keys_to(&ws.mux, &paste.key_releases),
            PaneInputResult::Queued
        );
        for (_, _, release) in paste.key_releases {
            if release.press_accepted.load(Ordering::Relaxed) {
                delivered.extend_from_slice(&release.bytes);
            }
        }
    }
    delivered
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

/// Wait up to `timeout` for `needle` on `pane`'s screen. On failure, return
/// the last screen so the assertion shows what the program printed instead.
#[cfg(unix)]
fn pane_screen_shows(
    mux: &Mux,
    pane: u64,
    needle: &str,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = String::new();
    loop {
        if let Some(screen) = mux.panes[&pane].term.screen_text(0) {
            if screen.text.contains(needle) {
                return Ok(());
            }
            last = screen.text;
        }
        if std::time::Instant::now() >= deadline {
            return Err(last);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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

pub(super) fn test_lane_panel() -> kettle_render::MediaLanePanel {
    kettle_render::MediaLanePanel {
        focused_control: None,
        pin_label: None,
        play: None,
        play_feedback: None,
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

fn item(label: &'static str, enabled: bool) -> ContextMenuItem {
    // Any concrete Action works — the filter only looks at the
    // `enabled` flag + variant kind, not the action payload.
    ContextMenuItem::Item {
        label,
        action: Action::Paste,
        enabled,
    }
}

fn confirmation_test_overlay() -> kettle_render::ConfirmDialogOverlay {
    kettle_render::ConfirmDialogOverlay {
        heading: "Close this window?".into(),
        body: "Both panes will close. Commands may be interrupted and unsaved work may be lost."
            .into(),
        buttons: vec![
            kettle_render::ConfirmDialogButton {
                label: "Cancel".into(),
                destructive: false,
            },
            kettle_render::ConfirmDialogButton {
                label: "Close window".into(),
                destructive: true,
            },
        ],
        focus_idx: 0,
        hover_idx: None,
        pressed_idx: None,
        scroll: 0,
    }
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

fn finding_body<'a>(source: &'a str, name: &str) -> &'a str {
    source
        .split(&format!("fn {name}("))
        .nth(1)
        .unwrap()
        .split("\n    }")
        .next()
        .unwrap()
}

fn review_file_facts_fixture(
    identity: Option<kettle_media::PathIdentity>,
) -> (
    crate::media::MediaService,
    WindowState,
    u64,
    crate::media::FactsQuery,
    std::sync::mpsc::Receiver<()>,
) {
    let (mux, pane, _) = title_test_split_mux();
    let mut ws = WindowState::new(1, mux);
    let item = crate::media::ShelfItem::checking_file(
        73,
        None,
        "unknown.bin".into(),
        crate::media::Provenance::User,
        crate::media::FileWhy::Unsupported,
        crate::media::ItemSource::sample(b"unknown"),
    );
    ws.mux
        .panes
        .get_mut(&pane)
        .unwrap()
        .media_shelf
        .publish(item, None, &[]);
    let query = crate::media::FactsQuery {
        window: 1,
        pane,
        item: 73,
        generation: 0,
        control: Default::default(),
        why: crate::media::FileWhy::Unsupported,
        source: kettle_media::Source::Bytes(Vec::new()),
        reply: None,
    };
    let (wake, woke) = std::sync::mpsc::channel();
    let lane = crate::media::FactsLane::start(
        move |_| identity,
        Arc::new(move || {
            let _ = wake.send(());
        }),
    )
    .unwrap();
    let mut media = crate::media::MediaService::default();
    media.set_facts_lane(lane);
    (media, ws, pane, query, woke)
}

fn review_file_identity() -> kettle_media::PathIdentity {
    kettle_media::PathIdentity {
        dev: 1,
        ino: 2,
        size: 12345,
        mtime_seconds: 3,
        mtime_nanos: 4,
    }
}

include!("accessibility_tests.rs");
include!("actions_tests.rs");
include!("automation_tests.rs");
include!("chrome_tests.rs");
include!("clipboard_tests.rs");
include!("config_reload_tests.rs");
include!("ctl_tests.rs");
include!("cursor_tests.rs");
include!("focus_tests.rs");
include!("gpu_recovery_tests.rs");
include!("ime_tests.rs");
include!("keyboard_tests.rs");
include!("keyboard_chords_tests.rs");
include!("layout_tests.rs");
include!("links_tests.rs");
include!("media_tests.rs");
include!("menus_tests.rs");
include!("modals_tests.rs");
include!("notifications_tests.rs");
include!("output_tests.rs");
include!("paste_tests.rs");
include!("pickers_tests.rs");
include!("pointer_tests.rs");
include!("preview_tests.rs");
include!("recording_tests.rs");
include!("redraw_tests.rs");
include!("remote_tests.rs");
include!("scheduling_tests.rs");
include!("screenshots_tests.rs");
include!("search_tests.rs");
include!("sessions_tests.rs");
include!("startup_tests.rs");
include!("title_tests.rs");
include!("update_toast_tests.rs");
include!("windowing_tests.rs");

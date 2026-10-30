//! winit application: window lifecycle, input routing, the tiled multiplexer,
//! the search overlay, clipboard, and live config reload.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use crate::cursor_blink::{cursor_blink_timed_out, next_cursor_blink_phase};
use accesskit::{
    Action as AccessibilityAction, ActionData, ActionHandler, ActionRequest, ActivationHandler,
    DeactivationHandler, Node, NodeId, Role, TextPosition, TextSelection, TreeId, TreeInfo,
    TreeUpdate,
};
use anyhow::Result;
#[cfg(any(target_os = "macos", test))]
use kettle_config::MacosOptionAsAlt;
use kettle_config::{
    Action, Bindings, Config, ConfigTrust, Key as KKey, Mods, Osc52, StatusBarMode, TabBarMode,
    TabBarPos, Trigger,
};
use kettle_core::{ClipboardType, Dimensions, PtyGeometry, Scroll, TermEvent};
use kettle_render::{
    ContextMenu, ContextMenuRow, CursorPatchFailure, CursorPatchOutcome, FrameOutcome,
    HighlightRect, HintLabel, ImePreedit, Overlay, PaneSnapshot, PaneView, Renderer,
    TabActivity as RenderTabActivity, TabBar, TabSeg,
};
use unicode_width::UnicodeWidthStr as _;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{
    CursorIcon, Fullscreen, Theme as WindowTheme, UserAttentionType, Window, WindowId,
};

use crate::gpu_diagnostics::{IncidentLog, RecoveryAction, RecoveryState};
use crate::input;
#[cfg(windows)]
use crate::mux::PtyOutputClosePhase;
use crate::mux::{
    BroadcastScope, Dir, Mux, Pane, PaneInputDelivery, PaneInputResult, PaneTitleOrigin, Rect,
};
use crate::pane_partition::{LayoutStyle, PanePartition};
use crate::window_state::{
    DpiResizeAction, DpiResizeEvent, FrameRecoveryAction, FrameRecoveryPoll, WindowState,
    track_consumed_key_release, track_terminal_key_press,
};

/// This file and the production modules in `app/`, as one text, for the
/// guards that read the App's source. A method moved to a module is
/// `pub(super)` only so app.rs can still call it; the guards read it as the
/// private method it is.
#[cfg(test)]
pub(crate) fn app_source() -> String {
    [
        include_str!("app.rs"),
        include_str!("app/accessibility.rs"),
        include_str!("app/actions.rs"),
        include_str!("app/automation.rs"),
        include_str!("app/chrome.rs"),
        include_str!("app/clipboard.rs"),
        include_str!("app/completion.rs"),
        include_str!("app/config_reload.rs"),
        include_str!("app/ctl.rs"),
        include_str!("app/ctl_snapshot.rs"),
        include_str!("app/cursor.rs"),
        include_str!("app/focus.rs"),
        include_str!("app/gpu_recovery.rs"),
        include_str!("app/hints.rs"),
        include_str!("app/ime.rs"),
        include_str!("app/keyboard.rs"),
        include_str!("app/layout.rs"),
        include_str!("app/links.rs"),
        include_str!("app/media.rs"),
        include_str!("app/menus.rs"),
        include_str!("app/modals.rs"),
        include_str!("app/notifications.rs"),
        include_str!("app/output.rs"),
        include_str!("app/paste.rs"),
        include_str!("app/pickers.rs"),
        include_str!("app/pointer.rs"),
        include_str!("app/preview.rs"),
        include_str!("app/preview_model.rs"),
        include_str!("app/recording.rs"),
        include_str!("app/redraw.rs"),
        include_str!("app/remote.rs"),
        include_str!("app/scheduling.rs"),
        include_str!("app/screenshots.rs"),
        include_str!("app/search.rs"),
        include_str!("app/sessions.rs"),
        include_str!("app/settings_text.rs"),
        include_str!("app/startup.rs"),
        include_str!("app/system_opener.rs"),
        include_str!("app/title.rs"),
        include_str!("app/update_toast.rs"),
        include_str!("app/window_events.rs"),
        include_str!("app/windowing.rs"),
    ]
    .concat()
    .replace("pub(super) ", "")
}

/// The production source of the App, excluding test-only items.
#[cfg(test)]
fn production_source() -> String {
    let production = kettle_test_support::production_source(&app_source());
    assert!(
        !production.contains("fn production_source()"),
        "the production slice retained its own helper"
    );
    assert!(
        !production.contains("#[test]"),
        "the production slice retained a test function"
    );
    assert!(
        !production.contains("#[cfg(test)]"),
        "the production slice retained a test-only item"
    );
    production
}

mod actions;
mod clipboard;
mod completion;
mod ctl_snapshot;
mod cursor;
mod focus;
mod hints;
mod ime;
mod keyboard;
mod layout;
mod links;
mod notifications;
mod output;
mod pickers;
mod preview_model;
mod recording;
mod redraw;
mod remote;
mod scheduling;
mod screenshots;
mod settings_text;
mod startup;
mod title;
mod window_events;

use accessibility::*;
pub(crate) use actions::*;
use automation::*;
use chrome::*;
use clipboard::*;
use completion::*;
use config_reload::*;
use ctl::*;
use ctl_snapshot::*;
use cursor::*;
pub(crate) use focus::*;
use gpu_recovery::*;
pub(crate) use hints::*;
use ime::*;
pub(crate) use keyboard::*;
pub(crate) use layout::*;
pub(crate) use links::*;
use media::*;
pub(crate) use menus::*;
pub(crate) use modals::*;
pub(crate) use notifications::*;
use output::*;
use paste::*;
pub(crate) use pickers::*;
pub(crate) use pointer::*;
use preview_model::*;
use recording::*;
use redraw::*;
use remote::*;
use scheduling::*;
use screenshots::*;
use search::*;
use sessions::*;
use settings_text::*;
pub(crate) use startup::*;
pub(crate) use title::*;
use windowing::*;

mod accessibility;
mod automation;
mod chrome;
mod config_reload;
mod ctl;
mod gpu_recovery;
mod media;
mod menus;
mod modals;
mod paste;
mod pointer;
mod preview;
mod search;
mod sessions;
mod system_opener;
mod update_toast;
mod windowing;

#[cfg(test)]
mod macos_chrome_policy_tests;

#[cfg(test)]
mod ctl_target_param_tests;

#[derive(Debug, Clone)]
pub enum UserEvent {
    Wakeup,
    ShellsDetected,
    /// The paste encoder finished a clipboard image's PNG.
    PastedImageEncoded,
    #[cfg(target_os = "linux")]
    Wayland,
    #[cfg(target_os = "linux")]
    BlurDiscovered,
    /// The coalescing process-tree worker published a new remote/cwd snapshot.
    /// Kept distinct from PTY output wakeups so an idle window consumes the
    /// result even when no output generation advanced.
    RemoteScanReady,
    ReloadConfig,
    VideoPreviewReady {
        window_seq: u64,
        pane_id: u64,
        generation: u64,
        candidate: Option<crate::video_preview::VideoPasteCandidate>,
        preview: Option<kettle_core::ImageData>,
    },
    NativeFullscreenExited {
        window_seq: u64,
        generation: u64,
    },
    AccessibilityActivated {
        window_seq: u64,
    },
    /// An assistive-technology action addressed to a specific OS window.
    /// AccessKit can invoke this callback off the winit dispatch path, so it is
    /// re-serialized through the event-loop proxy before touching UI state.
    AccessibilityAction {
        window_seq: u64,
        request: ActionRequest,
    },
    /// A compatible bare Kettle launch asked this primary process to open a
    /// fresh OS window. The bounded inbox carries the request and completion.
    Activation,
    /// Remote control: the remote-command file changed and
    /// the watcher needs the main thread to read + process new lines.
    /// One event per change (notify coalesces consecutive writes), so
    /// the main thread can batch-read all pending lines at once.
    RemoteCommand,
    /// The control server enqueued a message
    /// (new connection / request / disconnect). The main thread drains the
    /// server channel and dispatches each request against `ws.mux`.
    Ctl,
    /// The media lane finished a `show` render; publish it and start the
    /// next.
    MediaRendered,
    /// A lane's video playback has something new: a frame, or a change of
    /// state. Coalesced by the session until the lane looks.
    LanePlayback,
    /// The background update check announced a newer release, by tag (e.g.
    /// `v5.2.0`). Update facts drive foreground delivery.
    UpdateAvailable {
        tag: String,
        /// Legacy per-tag delivery marker.
        notify: bool,
    },
    /// Fresh update facts, read off the UI thread.
    UpdateFacts(Box<crate::update_notice::UpdateFacts>),
    UpdateCheckFinished,
    UpdateToastClaimed {
        window: u64,
        reply: crate::update_toast::ClaimReply,
    },
    /// An automatic update completed. Existing windows keep running the
    /// current image; the next kettle launch uses the installed release.
    UpdateInstalled {
        tag: String,
        staged: bool,
        first_time: bool,
        notify: bool,
    },
    /// Automatic installs of `tag` failed often enough to say so. The raw
    /// errors are in the log only.
    UpdateFailed {
        tag: String,
        notify: bool,
    },
    /// A newer version than this process runs is installed, or committed and
    /// waiting for its own version to start. Only a restart picks it up.
    UpdateRestartPending {
        installed: String,
    },
    /// The recurring in-session update timer fired; re-run the due check with
    /// the current config so a long-lived window keeps itself current.
    UpdateCheckTick,
    /// A row in the macOS Dock context menu was chosen. AppKit delivers that on
    /// the main thread but outside winit's dispatch, with no `ActiveEventLoop`
    /// and no `&mut App`, so it is re-serialized through the event-loop proxy
    /// exactly as `AccessibilityAction` is.
    #[cfg(target_os = "macos")]
    DockCommand(crate::macos_dock::DockCommand),
    #[cfg(target_os = "macos")]
    DockReopen,
    #[cfg(target_os = "macos")]
    DockReopenReady(std::path::PathBuf),
    #[cfg(target_os = "macos")]
    DockReopenFinished(bool),
}

fn rect_contains(rect: kettle_render::Rect4, x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

#[cfg(test)]
#[path = "app/restore_content_tests.rs"]
mod restore_content_tests;

#[cfg(test)]
use crate::window_accent::pick_accent_slot;

pub struct App {
    #[cfg(target_os = "linux")]
    wayland: Option<crate::wayland::Bridge>,
    #[cfg(target_os = "linux")]
    wayland_tab_drag: Option<WaylandTabDrag>,
    #[cfg(target_os = "linux")]
    wayland_file_drops: std::collections::BTreeMap<u64, (u64, WaylandDropTarget)>,
    cfg: Config,
    /// The file this launch's session was read from, if any.
    ///
    /// Held so a restore that fails can move that exact file aside. It is not
    /// recoverable later: `session_write_target` decides where a *save* goes
    /// from state this never sees, and a one-shot tab handoff is deleted as it
    /// is read.
    restore_source: Option<std::path::PathBuf>,
    restore_identity: Option<crate::session::SnapshotIdentity>,
    /// An unread restore snapshot must survive automatic saves for this launch.
    session_load_failed: bool,
    session_writer: crate::session::SessionWriter,
    /// All per-window state, keyed by the
    /// window's stable sequence number (`WindowState::seq`, 1-based, never
    /// reused). BTreeMap so iteration order is deterministic (window 1, 2,
    /// ...). The `ApplicationHandler` entry points remove the addressed entry,
    /// run the inner handler with disjoint `&mut self` (globals) +
    /// `&mut WindowState` borrows, then reinsert — see window_state.rs for the
    /// dispatch contract.
    windows: std::collections::BTreeMap<u64, WindowState>,
    accent_registry: crate::window_accent::AccentRegistry,
    /// Seq of the window that has (or most recently had) OS focus. Routes
    /// window-less events (UserEvent wakeups, remote/ctl/Lua commands).
    focused_seq: u64,
    /// Next `WindowState::seq` to assign (consumed by `open_window`).
    next_window_seq: u64,
    /// The shared GPU context, cached when window 1's renderer comes up
    /// in `resumed`; `open_window` reuses it for the synchronous (no
    /// adapter/device request) renderer init of windows 2..N.
    gpu: Option<kettle_render::GpuContext>,
    /// The system fonts, loading on their own thread since `run_with`
    /// began. Window 1 takes them before its first spawn; later windows load their own.
    font_preload: Option<crate::font_preload::FontPreload>,
    /// Decided before the first spawn consumes command/cwd.
    launch_override: bool,
    pre_launch_eligible: bool,
    /// Detected GPUs as `(token, label)` pairs for the Settings →
    /// Graphics device picker. Enumerated ONCE when the settings overlay first
    /// opens (a wgpu instance + adapter walk is ~tens of ms — too heavy per
    /// frame) and cached for the session; `categories()` reads it. Empty until
    /// the overlay is first opened (the picker then shows just "Automatic").
    gpu_choices: Vec<(String, String)>,
    /// The UI's language, fixed for the process. Kettle-owned text goes
    /// through it; terminal content, user names and config values do not.
    ui_text: kettle_i18n::Translator,
    /// Pure settle/backoff state after a GPU device loss.
    gpu_recovery: RecoveryState,
    /// Fault-only local record for the current device-loss incident.
    gpu_incident: Option<IncidentLog>,
    /// Prevent repeated filesystem attempts if diagnostics cannot be created.
    gpu_incident_started_for_loss: bool,
    /// True once recovery had to fall back to software rendering.
    gpu_software_fallback: bool,
    /// Last palette variant installed as the live Windows/X11 window icon.
    /// macOS and Wayland ignore this winit hook and use their bundle/desktop
    /// assets, but keeping one lazy theme-sync path makes native platforms that
    /// do expose it change immediately with the rest of the chrome.
    window_icon_dark_synced: Option<bool>,
    /// Window ids awaiting removal after their checked-out dispatch finishes.
    /// This must be keyed by window: a process-global boolean can be set while
    /// ctl/Lua dispatches into one mapped window and then consumed by another
    /// window's epilogue, closing a sibling the user never asked to close.
    pending_window_closes: std::collections::BTreeSet<u64>,
    /// Quit drops every window and exits, regardless of how many are open.
    quit_requested: bool,
    /// Session recorder (asciicast trace). `Some` when recording is active for
    /// this window — requested via `--record PATH` / `KETTLE_RECORD` or the
    /// persistent `record = on` config key. `None` (the default) records
    /// nothing at zero cost.
    recorder: Option<crate::dev_record::Recorder>,
    /// A requested recorder that failed before construction still needs a
    /// persistent visible error state; there is no `Recorder` to carry it.
    recording_start_failed: bool,
    /// Desktop notifications are edge-triggered so a stopped recorder does
    /// not emit one notification per redraw or stale-flush timer tick.
    recording_error_reported: bool,
    /// Throttles visible PTY input rejection feedback. A child which has
    /// stopped reading can reject a burst of key-repeat events; the user needs
    /// one explicit notice, not a desktop-toast storm.
    last_input_rejection_notice: Option<(PaneInputResult, std::time::Instant)>,
    system_motion: std::sync::OnceLock<crate::motion::SystemMotion>,
    #[cfg(target_os = "linux")]
    agent_launches: kettle_ctl::agent_launch::LaunchScopes,
    proxy: EventLoopProxy<UserEvent>,
    pending_shell_action: PendingShellAction,
    /// Private, bounded bare-launch activation inbox. The IPC thread waits for
    /// this event-loop thread to confirm window creation before telling the
    /// secondary launcher it may exit.
    activation: Option<crate::activation_server::ActivationInbox>,
    /// Privacy-safe event-loop phase tracker. A separate watchdog records a
    /// bounded incident when a callback stops making progress; it never
    /// captures terminal contents, command lines, or paths.
    runtime_tracker: crate::runtime_diagnostics::RuntimeTracker,
    /// Wakeup-dedup latch shared by every pane's `Waker`.
    /// Under output flood the PTY readers fire once per 64KiB read — dozens
    /// of queued `UserEvent::Wakeup`s per paint window, each one fanning out
    /// over every window just to discover the generations already matched.
    /// The waker enqueues only when it flips this false→true; the Wakeup arm
    /// re-opens the latch BEFORE reading generations, so output that lands
    /// after the clear enqueues a fresh event (no lost wakeups).
    wake_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Exactly one config watcher notification may be queued or debouncing.
    /// The main thread clears this immediately before it rereads the file.
    config_reload_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Short, non-blocking settle window for atomic-save event bursts.
    config_reload_deadline: Option<std::time::Instant>,
    /// Remote-file counterpart to `config_reload_pending`. Commands accumulate
    /// in the bounded file, so one drain notification is sufficient.
    remote_command_pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Claimed remote-file operations awaiting ordered dispatch. The receiver
    /// never claims another bounded batch while this FIFO has a retained suffix.
    pending_remote_commands: VecDeque<PendingRemoteCommand>,
    /// A watcher event (or startup lock timeout) promised that the spool still
    /// needs a claim. This remains true while an older in-memory suffix is
    /// backpressured, so consuming the one watcher event cannot strand a later
    /// batch in the file.
    remote_spool_claim_pending: bool,
    /// Deadline/backoff for a busy spool lock or backpressured target pane.
    remote_command_retry: AutomationRetry,
    /// The in-process control server, present when the control policy allows
    /// anything (`agent-server`, `agent-display` or their launch flags).
    /// `None` keeps the zero-cost default path. Started in `resumed`, or when
    /// display is turned on later; dropped on exit (which unregisters the
    /// discovery entry).
    ctl: Option<crate::ctl_server::CtlServer>,
    /// The control policy in force, resolved at launch from config and the
    /// `--agent-server`/`--agent-display` flags. `agent-server` applies only
    /// at launch; display can only be turned on while Kettle runs (see
    /// `reconcile_agent_display`), so this never narrows.
    ctl_policy: kettle_ctl::CtlPolicy,
    /// Whether the control server may start yet, and whether it has tried.
    ctl_start: CtlStart,
    /// Pending `run_command` correlations keyed by
    /// pane id. A request writes `cmd\n`, records the start line + deadline
    /// here, and the next OSC-133 `CommandFinished` for that pane resolves it.
    pending_runs: std::collections::HashMap<u64, PendingRun>,
    /// `show` pushes: their render queue and the thread that renders them.
    media: crate::media::MediaService,
    /// The one-time tip that a click opens an inline card.
    cards_tip: crate::media::CardsTip,
    clipboard: Option<arboard::Clipboard>,
    /// Temporary PNGs materialized from clipboard bitmaps. Owner-only, bounded,
    /// and removed on exit — see [`crate::paste_image`].
    pasted_images: crate::paste_image::PastedImages,
    /// Encodes clipboard images off the window thread; started on first use.
    png_encoder: Option<crate::paste_image::PngEncoder>,
    png_jobs: u64,
    /// Pastes waiting, in order, for the images ahead of them to encode.
    queued_pastes: std::collections::VecDeque<QueuedPaste>,
    /// The PNG copies handed to the image viewer, made off the window thread.
    /// Owner-only, bounded, oldest dropped first, and removed on exit.
    viewer_copies: std::sync::Arc<std::sync::Mutex<crate::paste_image::PastedImages>>,
    /// The private copies of videos handed to the video player, made off
    /// the window thread. Owner-only, bounded, oldest dropped first, and
    /// removed on exit.
    video_copies: std::sync::Arc<crate::media::PrivateCopies>,
    /// The private copies of PDFs handed to the document viewer, kept as
    /// the video copies are.
    document_copies: std::sync::Arc<crate::media::PrivateCopies>,
    /// Previews the user asked for that are on their pane's shelf, waiting
    /// for that pane's window to open them: window, pane and item.
    previews_ready: Vec<(u64, u64, u64)>,
    /// Whether a control client's request is being handled: what it
    /// drives is never the user's own gesture.
    ctl_driving: bool,
    /// Whether a Lua script's action is being run: such an action is no
    /// gesture of the user's, and starts no preview that reads a file.
    lua_driving: bool,
    /// The last ticket a lane took for sharper pixels.
    crop_tickets: u64,
    /// The last gallery page request a lane made, counting from 0, so two
    /// requests for one page are told apart.
    page_tickets: u64,
    /// The last ticket a lane took for a video's silent preview.
    preview_tickets: u64,
    /// The thread that copies a lane's image or source, from the first copy.
    preview_copy: Option<crate::media::CopyService>,
    /// Bounded native-poster jobs. Paths cross only the private child-worker
    /// protocol; decoded pixels return through the event loop.
    video_previewer: crate::video_preview::VideoPreviewer,
    next_video_preview_generation: u64,
    /// Triggers: compiled regex set built from `cfg.triggers` at App
    /// construction (and after live reload). Invalid patterns are logged via
    /// `log::warn!` and dropped.
    compiled_triggers: Vec<(regex::Regex, kettle_config::TriggerAction)>,
    /// Last trigger fire, or `None` before the first fire / after a
    /// config reload. Dedupes a fast-arriving match flood without fabricating
    /// an `Instant` before the OS monotonic clock's origin.
    last_trigger_fire: Option<std::time::Instant>,
    /// Coalescing worker for periodic remote/cwd detection. Process enumeration
    /// and procfs reads never block the window event loop.
    remote_scan_worker: Option<kettle_remote::RemoteScanWorker>,
    /// Last snapshot values actually applied to panes. This is the redraw and
    /// stale-result baseline; worker results may overwrite each other before
    /// the event loop consumes them.
    remote_applied: std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    remote_live_panes_scratch: std::collections::HashSet<(u64, u32)>,
    /// Reused target collection for the cross-window remote poll.
    remote_targets_scratch: Vec<kettle_remote::RemoteProbeTarget>,
    /// Throttle the remote-detect poll to ~5 Hz. `None` makes the
    /// first poll immediately eligible, including during the first minute
    /// after Windows boots.
    last_remote_poll: Option<std::time::Instant>,
    /// When a poll the throttle skipped must run. `about_to_wait` wakes for it.
    remote_poll_due: Option<std::time::Instant>,
    /// The most-recent auto-theme "schedule decision" (true=dark)
    /// we've applied, so a boundary-crossing fires the swap exactly once.
    last_schedule_decision: Option<bool>,
    os_theme_dark: Option<bool>,
    /// Resolved non-default config file (explicit path or named profile).
    config_path: Option<std::path::PathBuf>,
    /// Provenance paired with `config_path`; profile switching resets this to
    /// directory verification even when the process started with `--config`.
    config_trust: ConfigTrust,
    /// The malformed-value diagnostics surfaced on the previous live reload,
    /// so the reload notification is edge-triggered — kettle's own
    /// settings-persistence writes reload the config and must not re-fire an
    /// identical warning every time. See [`should_notify_malformed`].
    config_malformed_last: Vec<String>,
    /// First-tab CLI overrides (`-e cmd`, `-d dir`); consumed once.
    startup: crate::Options,
    /// May this session write back to the `--layout NAME` file?
    ///
    /// `false` when a launch override (`-e` / `-d`) meant the named layout was
    /// never loaded. Otherwise `kettle --layout dev -d ~/work` would open a
    /// plain window and overwrite `dev.json` with that single pane on the first
    /// save. The write is gated the same way as the load.
    ///
    /// Recorded here rather than recomputed at save time because the startup
    /// command/cwd fields are consumed once and are gone by then.
    named_layout_writable: bool,
    _watcher: Option<notify::RecommendedWatcher>,
    config_watcher_path: Option<std::path::PathBuf>,
    /// Drop guard for the remote-control watcher.
    _remote_watcher: Option<notify::RecommendedWatcher>,
    /// Ordered, bounded Lua side effects. Each send remains a distinct shared
    /// message so retry cannot merge calls into an oversize allocation, while
    /// actions/notifications/theme changes retain script order.
    pending_lua_commands: PendingLuaCommands,
    /// Retry state for a child that temporarily stops accepting Lua-generated
    /// input. Capacity release has no event-loop wake of its own.
    lua_command_retry: AutomationRetry,
    /// The live LuaEngine persisted across the App's lifetime so
    /// `kettle.on(event, callback)` registrations stay in scope. All event
    /// hooks share `drain_lua_hook_commands` for the LuaCommand dispatch.
    lua_engine: Option<crate::LuaEngine>,
    /// Set after LuaEvent::Startup fired once (Wayland can re-emit
    /// `resumed`).
    lua_startup_fired: bool,
    /// What is true about updates for this process; see `update_notice`.
    update_facts: crate::update_notice::UpdateFacts,
    update_local_dismissals: crate::update_notice::LocalDismissals,
    /// This process dismissed its restart notice, until it exits.
    update_restart_dismissed: bool,
    update_toast: Option<crate::update_toast::Toast>,
    update_toast_receipts: Option<crate::update_toast::ReceiptWriter>,
    update_toast_claiming: Option<(crate::update_notice::Notice, u64)>,
    update_toast_handled: Option<crate::update_notice::Notice>,
    update_toast_unavailable: Option<crate::update_notice::Notice>,
    update_toast_retry: Option<std::time::Instant>,
    update_toast_focus: Option<NodeId>,
    /// When the update facts were last asked for, to throttle focus refreshes.
    update_facts_requested: Option<std::time::Instant>,
    update_facts_refresh: crate::update_check::FactsRefresh,
    /// The installed version last logged as waiting for a restart.
    logged_restart_pending: Option<String>,
    /// Primaries of other builds still running, found at election.
    other_builds: kettle_ctl::activation::OtherBuilds,
    /// This executable's canonical path, found at startup. On Linux an
    /// update replaces it by rename, after which `current_exe()` reads
    /// `<path> (deleted)` and no longer resolves; agent files keep naming
    /// the installed one.
    executable: Option<std::path::PathBuf>,
    /// The full version string the About panel shows —
    /// the bin crate passes its `KETTLE_VERSION` (version + git hash, exactly
    /// what `--version` prints); falls back to the bare crate version.
    version_line: String,
    /// `Some` while a torn-off window is riding an OS-native move loop
    /// (`drag_window()`) or the manual-follow fallback. Drives the re-dock:
    /// hit-testing sibling tab bands on `Moved`, the insertion preview, and
    /// the drop-merge. App-level (not per-window) because exactly one tear-off
    /// drag can be in flight per pointer.
    torn_drag: Option<TornDrag>,
    /// When the session was last considered for writing. Drives the sweep
    /// described on [`SESSION_SWEEP`]; `None` until the first one runs.
    last_session_sweep: Option<std::time::Instant>,
}

#[cfg(test)]
mod completion_surface_tests;

/// Whether an optional monotonic-clock throttle is eligible at `now`.
///
/// `None` represents "never run" and is immediately eligible. Saturating
/// subtraction also keeps tests and unusual clock implementations safe if a
/// caller supplies a timestamp ordered before `last`.
fn throttle_elapsed(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    interval: std::time::Duration,
) -> bool {
    last.is_none_or(|last| now.saturating_duration_since(last) >= interval)
}

impl ApplicationHandler<UserEvent> for App {
    // C1-DISPATCH-BEGIN: take-out/put-back window dispatch.
    //
    // Every per-window field lives on `WindowState` (window_state.rs). Each
    // handler removes the addressed window from the map, runs the inner
    // handler with disjoint `&mut self` (globals) + `&mut WindowState`
    // borrows, then hands the entry to `finish_window_dispatch` — which
    // reinserts it, or drops it when the inner handler flagged the
    // window closed, exiting the loop once no windows remain. A panic
    // mid-handler drops the entry, which is fine: kettle aborts on panic,
    // nothing observes the missing window.
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let runtime_tracker = self.runtime_tracker.clone();
        let _phase = runtime_tracker.enter("resumed");
        // winit's English application menu is up by now; Kettle's, in the
        // UI's language, takes its place once.
        crate::macos_menu::install(&self.ui_text);
        let seq = self.focused_seq;
        let Some(mut ws) = self.windows.remove(&seq) else {
            return;
        };
        self.resumed_inner(&mut ws, event_loop);
        self.sync_output_wake_gate(&ws);
        self.finish_window_dispatch(event_loop, seq, ws);
        self.drain_activation_requests(event_loop);
        #[cfg(target_os = "linux")]
        {
            if self.wayland.is_none() {
                let proxy = self.proxy.clone();
                self.wayland = crate::wayland::Bridge::new(
                    event_loop.owned_display_handle(),
                    Arc::new(move || {
                        let _ = proxy.send_event(UserEvent::Wayland);
                    }),
                );
            }
            self.poll_wayland(event_loop);
        }
        runtime_tracker.set_window_count(self.windows.len());
    }

    fn user_event(&mut self, el: &ActiveEventLoop, ev: UserEvent) {
        let runtime_tracker = self.runtime_tracker.clone();
        let _phase = runtime_tracker.enter("user_event");
        match ev {
            UserEvent::ShellsDetected => {
                let Some(shells) = kettle_core::term::cached_shells() else {
                    return;
                };
                let pending = self.pending_shell_action.complete(&shells);
                let seqs: Vec<_> = self.windows.keys().copied().collect();
                for seq in seqs {
                    let Some(mut ws) = self.windows.remove(&seq) else {
                        continue;
                    };
                    if let Some(anchor) = ws
                        .context_menu
                        .as_ref()
                        .filter(|menu| Self::shell_menu_is_loading(&menu.items, &self.ui_text))
                        .map(|menu| menu.anchor)
                    {
                        let items = Self::new_tab_menu_items(&shells, &self.ui_text);
                        self.show_context_menu(&mut ws, items, anchor.0, anchor.1);
                        if let Some(window) = &ws.window {
                            window.request_redraw();
                        }
                    }
                    if let Some((_, argv)) = pending.as_ref().filter(|(window, _)| *window == seq) {
                        self.open_tab_with_argv(&mut ws, argv);
                    }
                    self.finish_window_dispatch(el, seq, ws);
                }
            }
            #[cfg(target_os = "linux")]
            UserEvent::Wayland => self.poll_wayland(el),
            #[cfg(target_os = "linux")]
            UserEvent::BlurDiscovered => {
                for window in self.windows.values() {
                    if let (Some(material), Some(handle)) =
                        (&window.native_material, &window.window)
                    {
                        material.sync(handle, &self.cfg);
                        handle.request_redraw();
                    }
                }
            }
            UserEvent::Activation => self.drain_activation_requests(el),
            #[cfg(target_os = "macos")]
            UserEvent::DockReopen => {
                self.focus_or_open_window(el, None);
                crate::macos_reopen::request(
                    self.startup.update_install.clone(),
                    self.startup
                        .version
                        .as_deref()
                        .unwrap_or(env!("CARGO_PKG_VERSION")),
                    self.proxy.clone(),
                );
            }
            #[cfg(target_os = "macos")]
            UserEvent::DockReopenReady(bundle) => {
                crate::macos_reopen::launch(&bundle, self.proxy.clone());
            }
            #[cfg(target_os = "macos")]
            UserEvent::DockReopenFinished(opened) => {
                crate::macos_reopen::finish();
                if !opened {
                    self.focus_or_open_window(el, None);
                }
            }
            UserEvent::PastedImageEncoded => self.drain_queued_pastes(el),
            // A watcher event starts a short non-blocking settle window. The
            // pending latch stays closed until about_to_wait clears it just
            // before the single process-level read.
            UserEvent::ReloadConfig => {
                self.config_reload_deadline
                    .get_or_insert_with(|| std::time::Instant::now() + CONFIG_RELOAD_DEBOUNCE);
            }
            UserEvent::LanePlayback => {
                // Each window whose lanes play takes what its sessions have.
                let seqs: Vec<u64> = self
                    .windows
                    .iter()
                    .filter(|(_, window)| !window.lane_playbacks.is_empty())
                    .map(|(&seq, _)| seq)
                    .collect();
                for seq in seqs {
                    let Some(mut ws) = self.windows.remove(&seq) else {
                        continue;
                    };
                    self.poll_lane_playbacks(&mut ws);
                    self.finish_window_dispatch(el, seq, ws);
                }
            }
            UserEvent::MediaRendered => {
                let seq = if self.windows.contains_key(&self.focused_seq) {
                    self.focused_seq
                } else if let Some(seq) = self.windows.keys().next().copied() {
                    seq
                } else {
                    return;
                };
                let Some(mut ws) = self.windows.remove(&seq) else {
                    return;
                };
                self.media_tick(&mut ws);
                self.finish_window_dispatch(el, seq, ws);
            }
            UserEvent::RemoteScanReady => {
                let seq = if self.windows.contains_key(&self.focused_seq) {
                    self.focused_seq
                } else if let Some(seq) = self.windows.keys().next().copied() {
                    seq
                } else {
                    return;
                };
                let Some(mut ws) = self.windows.remove(&seq) else {
                    return;
                };
                self.apply_remote_scan_snapshot(&mut ws);
                self.finish_window_dispatch(el, seq, ws);
            }
            // PTY wakeups carry no pane id. Fan out, with each window gated on
            // its panes' output generations.
            UserEvent::Wakeup => {
                // Reopen the wakeup latch BEFORE any generation reads below. A
                // reader that bumps its generation after this store enqueues a
                // fresh Wakeup, so the new output is painted either by this
                // pass (we see the bump) or by the next event (we already woke
                // for it). A swap with AcqRel, not a plain Release store, gives
                // the acquire edge that pairs with the reader's swap.
                self.wake_pending
                    .swap(false, std::sync::atomic::Ordering::AcqRel);
                let seqs: Vec<u64> = self.windows.keys().copied().collect();
                for seq in seqs {
                    let Some(mut ws) = self.windows.remove(&seq) else {
                        continue;
                    };
                    self.user_event_inner(&mut ws, el, ev.clone());
                    self.finish_window_dispatch(el, seq, ws);
                }
            }
            UserEvent::NativeFullscreenExited {
                window_seq,
                generation,
            } => {
                let Some(mut ws) = self.windows.remove(&window_seq) else {
                    return;
                };
                if ws.native_fullscreen.as_ref().is_some_and(|native| {
                    let fullscreen = ws
                        .window
                        .as_ref()
                        .is_some_and(|window| window.fullscreen().is_some());
                    native.finish(generation, fullscreen)
                        || (ws.dpi_resize_surface_suspended && !native.pending())
                }) && let Some(window) = &ws.window
                {
                    let size = window.inner_size();
                    self.window_event_inner(&mut ws, el, WindowEvent::Resized(size));
                }
                self.finish_window_dispatch(el, window_seq, ws);
            }
            UserEvent::AccessibilityActivated { window_seq } => {
                if let Some(ws) = self.windows.get_mut(&window_seq) {
                    ws.accessibility_key = None;
                    ws.accessibility_updated_at = None;
                    ws.accessibility_pending = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                }
            }
            UserEvent::AccessibilityAction {
                window_seq,
                request,
            } => {
                let Some(mut ws) = self.windows.remove(&window_seq) else {
                    return;
                };
                self.user_event_inner(
                    &mut ws,
                    el,
                    UserEvent::AccessibilityAction {
                        window_seq,
                        request,
                    },
                );
                self.finish_window_dispatch(el, window_seq, ws);
            }
            UserEvent::VideoPreviewReady {
                window_seq,
                pane_id,
                generation,
                candidate,
                preview,
            } => {
                // The pane's tab may have moved to another window since the
                // preview was requested.
                let Some(window_seq) =
                    video_preview_target_window(&self.windows, window_seq, pane_id, generation)
                else {
                    return;
                };
                let Some(mut ws) = self.windows.remove(&window_seq) else {
                    return;
                };
                self.user_event_inner(
                    &mut ws,
                    el,
                    UserEvent::VideoPreviewReady {
                        window_seq,
                        pane_id,
                        generation,
                        candidate,
                        preview,
                    },
                );
                self.finish_window_dispatch(el, window_seq, ws);
            }
            UserEvent::UpdateToastClaimed { window, reply } => {
                let Some(mut ws) = self.windows.remove(&window) else {
                    self.abandon_update_toast_claim(window, reply);
                    return;
                };
                self.receive_update_toast(&mut ws, window, reply);
                self.finish_window_dispatch(el, window, ws);
            }
            UserEvent::RemoteCommand => {
                // Re-arm before reading: a writer racing the drain can queue a
                // new notification, while commands already present are batched.
                self.remote_command_pending
                    .swap(false, std::sync::atomic::Ordering::AcqRel);
                let seq = self.focused_seq;
                let Some(mut ws) = self.windows.remove(&seq) else {
                    return;
                };
                self.user_event_inner(&mut ws, el, UserEvent::RemoteCommand);
                self.finish_window_dispatch(el, seq, ws);
            }
            // A Dock row acts on the focused window: NewWindow opens a
            // sibling of it and NewTab adds to it. Explicit rather than left to
            // the wildcard below so the no-window case is deliberate — kettle
            // exits once the last window closes, so an empty map means the
            // process is already on its way out and the row has nothing to do.
            #[cfg(target_os = "macos")]
            UserEvent::DockCommand(command) => {
                let seq = self.focused_seq;
                let Some(mut ws) = self.windows.remove(&seq) else {
                    log::debug!("dock menu: {command:?} arrived with no window to act on");
                    return;
                };
                self.user_event_inner(&mut ws, el, UserEvent::DockCommand(command));
                self.finish_window_dispatch(el, seq, ws);
            }
            // Ctl / remote / update-banner events act on the focused window.
            _ => {
                let seq = self.focused_seq;
                let Some(mut ws) = self.windows.remove(&seq) else {
                    return;
                };
                self.user_event_inner(&mut ws, el, ev);
                self.finish_window_dispatch(el, seq, ws);
            }
        }
        runtime_tracker.set_window_count(self.windows.len());
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        #[cfg(target_os = "linux")]
        self.poll_wayland(event_loop);
        let phase_name = if matches!(&event, WindowEvent::RedrawRequested) {
            "redraw"
        } else {
            "window_event"
        };
        let runtime_tracker = self.runtime_tracker.clone();
        let _phase = runtime_tracker.enter(phase_name);
        // An unknown WindowId (a late event for an already-closed window)
        // is dropped rather than misrouted to the focused window.
        let Some(seq) = self.seq_of_window(id) else {
            return;
        };
        let Some(mut ws) = self.windows.remove(&seq) else {
            return;
        };
        self.window_event_inner(&mut ws, event_loop, event);
        self.sync_output_wake_gate(&ws);
        // Single chokepoint for the live theme preview. After every event,
        // make `cfg.theme` reflect the theme picker's selection, or revert to
        // the baseline once the picker closes without committing.
        self.sync_theme_preview(&mut ws);
        self.finish_window_dispatch(event_loop, seq, ws);
        runtime_tracker.set_window_count(self.windows.len());
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "linux")]
        self.poll_wayland(event_loop);
        let runtime_tracker = self.runtime_tracker.clone();
        let _phase = runtime_tracker.enter("about_to_wait");
        // Opt-in cursor_exit_v1: a key that could have ended the layer blink
        // but asked for no frame is recorded before the loop sleeps.
        for ws in self.windows.values_mut() {
            if ws.cursor_exit_log.enabled {
                ws.cursor_exit_log.flush_unrequested();
            }
        }
        // Tear-off UX failsafe: torn-drag tracking that lost its
        // drop signal (an X11 release the WM swallowed, then no further
        // input) is abandoned after 120s of silence. The window is long
        // because a LIVE-but-stationary drag is indistinguishable from
        // stale tracking on X11 (about_to_wait keeps running there while
        // the WM moves the window, and a motionless hover generates no
        // Moved events) — 120s only fires on a hover no real gesture
        // holds, and the cost of waiting is bounded (the next press or
        // release also cleans stale tracking). Windows can't misfire
        // either way: the modal move loop doesn't run about_to_wait.
        if self
            .torn_drag
            .as_ref()
            .is_some_and(|t| t.last_signal.elapsed().as_secs() >= 120)
        {
            self.abandon_torn_drag(None);
        }
        // X11 tear-off frozen-drag rescue: reposition/dock from the live
        // pointer when the WM or the carrier's event stream stops delivering.
        // Returns a wait budget that keeps it ticking.
        let torn_tick_wait = self.torn_drag_pointer_tick(event_loop);
        let now = std::time::Instant::now();
        if crate::owned_dir::take_retry(now) {
            self.reconcile_claude_plugin();
            self.reconcile_codex_shell();
        }
        if self
            .config_reload_deadline
            .is_some_and(|deadline| deadline <= now)
        {
            self.config_reload_deadline = None;
            // Clear before the read so a genuine edit racing the reload queues
            // a follow-up; Access(Open) itself is filtered by event kind.
            self.config_reload_pending
                .swap(false, std::sync::atomic::Ordering::AcqRel);
            self.reload_config_windows();
        }
        // Every window ticks (reap, blink, coalesced-paint deadlines);
        // the per-window wait requests merge to the EARLIEST deadline so one
        // window's animation can't starve another's coalesced output flush.
        let seqs: Vec<u64> = self.windows.keys().copied().collect();
        let mut earliest_deadline: Option<std::time::Instant> = None;
        // The poll covers every window, so one checkout is enough.
        let mut remote_poll_due = self.remote_poll_due.is_some_and(|due| due <= now);
        for seq in seqs {
            let Some(mut ws) = self.windows.remove(&seq) else {
                continue;
            };
            if std::mem::take(&mut remote_poll_due) {
                self.poll_remote_contexts(&mut ws);
            }
            for pane in ws.mux.panes.values_mut() {
                if let Some(wait) = pane.term.poll_startup_geometry() {
                    let deadline = std::time::Instant::now() + wait;
                    earliest_deadline =
                        Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
                }
            }
            let wait = self.about_to_wait_inner(&mut ws, event_loop);
            self.finish_window_dispatch(event_loop, seq, ws);
            if let Some(deadline) = wait {
                earliest_deadline =
                    Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
            }
        }
        if let Some(deadline) = self.config_reload_deadline {
            let deadline =
                deadline.max(std::time::Instant::now() + std::time::Duration::from_millis(1));
            earliest_deadline =
                Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
        }
        if let Some(deadline) = crate::owned_dir::retry_deadline() {
            earliest_deadline =
                Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
        }
        if let Some(due) = self.remote_poll_due {
            let deadline = due.max(std::time::Instant::now() + std::time::Duration::from_millis(1));
            earliest_deadline =
                Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
        }
        if let Some(ms) = torn_tick_wait {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
            earliest_deadline =
                Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
        }
        #[cfg(target_os = "linux")]
        if let Some(deadline) = self
            .wayland
            .as_ref()
            .and_then(crate::wayland::Bridge::deadline)
        {
            earliest_deadline =
                Some(earliest_deadline.map_or(deadline, |current| current.min(deadline)));
        }
        if self.windows.is_empty() {
            return;
        }
        match earliest_deadline {
            Some(deadline) => event_loop.set_control_flow(ControlFlow::WaitUntil(deadline)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
        runtime_tracker.set_window_count(self.windows.len());
    }
    // C1-DISPATCH-END
}

/// Drift guard. The confirm dialog ("Close pane?", "Quit?") is a
/// modal: opening another overlay over it must clear it (`close_all_modals`),
/// and it must count as a modal for mouse/scroll/cursor gating
/// (`any_modal_open`). A full behavioral test would need a constructed `App`
/// (window + renderer); pin the invariant at the source level instead — the
/// same approach as `kettle-core`'s teardown guard.
#[cfg(test)]
mod modal_discipline_guard;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod keyboard_selection_tests;

#[cfg(test)]
mod notification_text_tests;

#[cfg(test)]
mod menu_text_tests;

#[cfg(test)]
mod picker_text_tests;

#[cfg(test)]
mod theme_picker_tests;

#[cfg(test)]
mod ui_text_drift_tests;

#[cfg(test)]
mod hint_action_tests;

#[cfg(test)]
mod program_link_tests;

#[cfg(test)]
mod settings_footer_text_tests;

#[cfg(test)]
mod lane_control_tests;

#[cfg(test)]
#[path = "app/modal_editor_tests.rs"]
mod modal_editor_tests;
